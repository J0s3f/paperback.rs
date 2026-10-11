// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

//! Parity blocks: an erasure code over all data blocks of a file, in the manner of PAR2.
//!
//! With `k` parity blocks, any `k` data blocks of a stripe that are missing can be rebuilt,
//! whichever they are and on whichever page they were. The code is a Cauchy matrix over
//! GF(2^16) (polynomial `x^16 + x^12 + x^3 + x + 1`, as in PAR2): every square part of it
//! can be inverted, so any parity blocks will do for any missing data blocks.
//!
//! A 90-byte block is 45 symbols of 16 bits (little endian). Large files are cut into
//! interleaved stripes of at most [`STRIPE_BLOCKS`] data blocks, which bounds the work: data
//! block `i` and parity block `k` belong to stripe `i % stripes` and `k % stripes`. Every stripe
//! still spans all pages, so the loss of a whole page is shared evenly by the stripes.
//!
//! Parity block `k` is the block at row `r = k / stripes` of its stripe; data block `i` is
//! column `j = i / stripes`. The coefficient of column `j` in row `r` is `1 / (x ^ y)` with
//! `x = 0xFFFF - r` and `y = j`.

use std::collections::HashMap;
use std::sync::OnceLock;

use rayon::prelude::*;

use crate::block::{ADDRESS_MASK, DATA_LEN};

/// Most data blocks in a stripe. Up to this many data blocks a file is one stripe, and any lost
/// data blocks up to the number of parity blocks read are rebuilt. The rows and the columns of
/// a stripe must be distinct elements of the field, so a stripe of this length leaves room for
/// three times as many parity blocks, and a shorter one for more (see [`Code::max_parity`]);
/// longer stripes would allow fewer, and cost more work per parity block and per lost block.
pub(crate) const STRIPE_BLOCKS: usize = 16384;
/// Elements of GF(2^16).
const FIELD_SIZE: usize = 1 << 16;
/// Order of the multiplicative group.
const ORDER: usize = FIELD_SIZE - 1;
const POLYNOMIAL: u32 = 0x1_100B;
/// Stands for the logarithm of zero, which has none.
const LOG_OF_ZERO: u16 = u16::MAX;
const SYMBOLS: usize = DATA_LEN / 2;
/// Parity blocks a worker computes at a time.
const ROWS_PER_JOB: usize = 32;

/// Parity block addresses are those that are not a multiple of 90: 89 between two data blocks.
const PER_GAP: usize = DATA_LEN - 1;

struct Field {
    log: Vec<u16>,
    /// Twice the period, so that the sum of two logarithms needs no reduction.
    exp: Vec<u16>,
}

fn field() -> &'static Field {
    static FIELD: OnceLock<Field> = OnceLock::new();
    FIELD.get_or_init(|| {
        let mut log = vec![LOG_OF_ZERO; FIELD_SIZE];
        let mut exp = vec![0u16; 2 * ORDER];
        let mut value: u32 = 1;
        for power in 0..ORDER {
            exp[power] = value as u16;
            exp[power + ORDER] = value as u16;
            log[value as usize] = power as u16;
            value <<= 1;
            if value & (FIELD_SIZE as u32) != 0 {
                value ^= POLYNOMIAL;
            }
        }
        Field { log, exp }
    })
}

impl Field {
    #[cfg(test)]
    fn mul(&self, a: u16, b: u16) -> u16 {
        if a == 0 || b == 0 {
            0
        } else {
            self.exp[usize::from(self.log[usize::from(a)]) + usize::from(self.log[usize::from(b)])]
        }
    }

    /// The inverse of a non-zero element.
    #[cfg(test)]
    fn inv(&self, a: u16) -> u16 {
        self.exp[(ORDER - usize::from(self.log[usize::from(a)])) % ORDER]
    }

    /// Logarithm of the coefficient of column `column` in row `row`.
    fn coefficient_log(&self, row: usize, column: usize) -> usize {
        let sum = (ORDER - row) ^ column;
        (ORDER - usize::from(self.log[sum])) % ORDER
    }

    /// Adds `exp(factor_log) * symbols` to `target`; `logs` are the logarithms of the symbols.
    fn add_scaled(&self, target: &mut [u16], factor_log: usize, logs: &[u16]) {
        for (t, &l) in target.iter_mut().zip(logs) {
            if l != LOG_OF_ZERO {
                *t ^= self.exp[factor_log + usize::from(l)];
            }
        }
    }

    fn logs_of(&self, symbols: &[u16]) -> Vec<u16> {
        symbols.iter().map(|&s| self.log[usize::from(s)]).collect()
    }
}

/// The address on paper of parity block `index`, which PaperBack takes for no block at all.
pub(crate) fn address(index: usize) -> u32 {
    (index / PER_GAP * DATA_LEN + index % PER_GAP + 1) as u32
}

/// The parity block an address names, if it is one.
pub(crate) fn index_of(address: u32) -> Option<usize> {
    let address = address as usize;
    let within = address % DATA_LEN;
    (within != 0).then(|| address / DATA_LEN * PER_GAP + within - 1)
}

fn symbols(block: &[u8]) -> [u16; SYMBOLS] {
    std::array::from_fn(|t| u16::from_le_bytes([block[2 * t], block[2 * t + 1]]))
}

fn to_bytes(symbols: &[u16; SYMBOLS]) -> [u8; DATA_LEN] {
    let mut bytes = [0u8; DATA_LEN];
    for (pair, symbol) in bytes.as_chunks_mut::<2>().0.iter_mut().zip(symbols) {
        pair.copy_from_slice(&symbol.to_le_bytes());
    }
    bytes
}

/// The code for a file of a given number of data blocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Code {
    blocks: usize,
    stripes: usize,
}

impl Code {
    pub(crate) fn new(blocks: usize) -> Self {
        Self {
            blocks,
            stripes: blocks.div_ceil(STRIPE_BLOCKS).max(1),
        }
    }

    /// Stripes of the file.
    pub(crate) fn stripes(&self) -> usize {
        self.stripes
    }

    /// The stripe of parity block `index`, if it can belong to this file.
    pub(crate) fn stripe_of_parity(&self, index: usize) -> Option<usize> {
        (index / self.stripes < self.rows_per_stripe()).then_some(index % self.stripes)
    }

    /// Data blocks in stripe `stripe`.
    fn stripe_len(&self, stripe: usize) -> usize {
        (self.blocks + self.stripes - 1 - stripe) / self.stripes
    }

    /// Rows a stripe can have: their `x` must differ from every column's `y`.
    fn rows_per_stripe(&self) -> usize {
        FIELD_SIZE - self.stripe_len(0)
    }

    /// The most parity blocks the file can have.
    pub(crate) fn max_parity(&self) -> usize {
        let by_address = index_of(ADDRESS_MASK - ADDRESS_MASK % DATA_LEN as u32 - 1).unwrap_or(0);
        (self.rows_per_stripe() * self.stripes).min(by_address)
    }

    /// Logarithms of the symbols of every data block of `data`, which is padded with zeros.
    fn data_logs(&self, data: &[u8], stripe: usize) -> Vec<u16> {
        let field = field();
        let mut logs = Vec::with_capacity(self.stripe_len(stripe) * SYMBOLS);
        for column in 0..self.stripe_len(stripe) {
            let start = (column * self.stripes + stripe) * DATA_LEN;
            let mut block = [0u8; DATA_LEN];
            if start < data.len() {
                let end = (start + DATA_LEN).min(data.len());
                block[..end - start].copy_from_slice(&data[start..end]);
            }
            logs.extend(symbols(&block).iter().map(|&s| field.log[usize::from(s)]));
        }
        logs
    }

    /// Parity blocks `0..count` of `data`, which holds the data blocks one after the other
    /// (the last one may be short).
    pub(crate) fn encode(&self, data: &[u8], count: usize) -> Vec<[u8; DATA_LEN]> {
        let field = field();
        let mut parity = vec![[0u8; DATA_LEN]; count];
        // One stripe after the other, so that only the logarithms of one are held; its rows are
        // worked on side by side.
        for stripe in 0..self.stripes.min(count) {
            let logs = self.data_logs(data, stripe);
            let rows = (count + self.stripes - 1 - stripe) / self.stripes;
            let done: Vec<[u8; DATA_LEN]> = (0..rows)
                .into_par_iter()
                .with_min_len(ROWS_PER_JOB)
                .map(|row| {
                    let mut parity = [0u16; SYMBOLS];
                    for (column, column_logs) in logs.as_chunks::<SYMBOLS>().0.iter().enumerate() {
                        field.add_scaled(
                            &mut parity,
                            field.coefficient_log(row, column),
                            column_logs,
                        );
                    }
                    to_bytes(&parity)
                })
                .collect();
            for (row, block) in done.into_iter().enumerate() {
                parity[row * self.stripes + stripe] = block;
            }
        }
        parity
    }

    /// Rebuilds missing data blocks of `data` (`blocks` blocks of 90 bytes; `present` tells
    /// which are known) from the parity blocks read, by index. A stripe is rebuilt when it has
    /// at least as many parity blocks as missing data blocks, and not if one parity block more
    /// was read and does not agree with the blocks rebuilt; the stripes are worked on side by
    /// side in `pool`, or in rayon's global pool. Returns how many blocks were rebuilt.
    pub(crate) fn recover(
        &self,
        data: &mut [u8],
        present: &mut [bool],
        parity: &HashMap<usize, [u8; DATA_LEN]>,
        pool: Option<&rayon::ThreadPool>,
    ) -> usize {
        let mut rows_of: Vec<Vec<usize>> = vec![Vec::new(); self.stripes];
        for &index in parity.keys() {
            let row = index / self.stripes;
            if row < self.rows_per_stripe() {
                rows_of[index % self.stripes].push(row);
            }
        }
        let work: Vec<(usize, Vec<usize>, Vec<usize>)> = rows_of
            .into_iter()
            .enumerate()
            .filter_map(|(stripe, mut rows)| {
                let missing: Vec<usize> = (0..self.stripe_len(stripe))
                    .filter(|&column| !present[column * self.stripes + stripe])
                    .collect();
                if missing.is_empty() || rows.len() < missing.len() {
                    return None;
                }
                // The lowest rows rebuild, the next one, if read, checks.
                rows.sort_unstable();
                rows.truncate(missing.len() + 1);
                Some((stripe, missing, rows))
            })
            .collect();
        if work.is_empty() {
            return 0;
        }
        let known: &[u8] = data;
        let solve_all = || -> Vec<Option<Vec<[u8; DATA_LEN]>>> {
            work.par_iter()
                .map(|(stripe, missing, rows)| {
                    self.solve(known, present, parity, *stripe, missing, rows)
                })
                .collect()
        };
        let solved = match pool {
            Some(pool) => pool.install(solve_all),
            None => solve_all(),
        };
        let mut rebuilt = 0;
        for ((stripe, missing, _), blocks) in work.iter().zip(solved) {
            let Some(blocks) = blocks else {
                continue;
            };
            for (column, block) in missing.iter().zip(blocks) {
                let index = column * self.stripes + stripe;
                data[index * DATA_LEN..(index + 1) * DATA_LEN].copy_from_slice(&block);
                present[index] = true;
            }
            rebuilt += missing.len();
        }
        rebuilt
    }

    /// The missing columns of a stripe from the parity rows `rows`, as many as are missing and
    /// perhaps one more, which then checks the blocks rebuilt: `None` if they do not give it,
    /// which a parity block that is not what its address says (forged, or of another file)
    /// brings about.
    ///
    /// The rows and the missing columns form a Cauchy matrix `1 / (x_i + y_j)`, whose inverse has
    /// a closed form: with `A(t)` the product of `t + x_i` over the rows and `B(t)` that of
    /// `t + y_j` over the missing columns, column `j` is the sum over the rows of
    /// `B(x_i) A(y_j) / ((x_i + y_j) A'(x_i) B'(y_j))` times what row `i` holds less the part
    /// of the data blocks that are known. That is work in the square of the missing blocks, not
    /// in its cube as an elimination would be, and needs no matrix in memory.
    fn solve(
        &self,
        data: &[u8],
        present: &[bool],
        parity: &HashMap<usize, [u8; DATA_LEN]>,
        stripe: usize,
        missing: &[usize],
        rows: &[usize],
    ) -> Option<Vec<[u8; DATA_LEN]>> {
        let field = field();
        let logs = self.data_logs(data, stripe);
        let n = missing.len();
        // What each parity row holds less the part of the data blocks that are known.
        let sums: Vec<[u16; SYMBOLS]> = rows
            .par_iter()
            .map(|&row| {
                let mut sum = parity
                    .get(&(row * self.stripes + stripe))
                    .map_or([0; SYMBOLS], |block| symbols(block));
                for (column, column_logs) in logs.as_chunks::<SYMBOLS>().0.iter().enumerate() {
                    if present[column * self.stripes + stripe] {
                        field.add_scaled(&mut sum, field.coefficient_log(row, column), column_logs);
                    }
                }
                sum
            })
            .collect();
        // The elements of the rows and of the missing columns; they are distinct, since a row is
        // below `rows_per_stripe`, and so none of the differences below is zero.
        let xs: Vec<usize> = rows[..n].iter().map(|&row| ORDER - row).collect();
        let ys = missing;
        let log = |value: usize| u64::from(field.log[value]);
        let inverse = |log: u64| ORDER as u64 - log % ORDER as u64;
        // log(B(x_i) / A'(x_i)) and log(A(y_j) / B'(y_j)).
        let row_factors: Vec<u64> = xs
            .par_iter()
            .enumerate()
            .map(|(i, &x)| {
                let b: u64 = ys.iter().map(|&y| log(x ^ y)).sum();
                let a: u64 = xs
                    .iter()
                    .enumerate()
                    .filter(|&(k, _)| k != i)
                    .map(|(_, &other)| log(x ^ other))
                    .sum();
                (b + inverse(a)) % ORDER as u64
            })
            .collect();
        let column_factors: Vec<u64> = ys
            .par_iter()
            .enumerate()
            .map(|(j, &y)| {
                let a: u64 = xs.iter().map(|&x| log(x ^ y)).sum();
                let b: u64 = ys
                    .iter()
                    .enumerate()
                    .filter(|&(k, _)| k != j)
                    .map(|(_, &other)| log(y ^ other))
                    .sum();
                (a + inverse(b)) % ORDER as u64
            })
            .collect();
        let sum_logs: Vec<Vec<u16>> = sums[..n].iter().map(|sum| field.logs_of(sum)).collect();
        let blocks: Vec<[u16; SYMBOLS]> = ys
            .par_iter()
            .zip(&column_factors)
            .map(|(&y, &column_factor)| {
                let mut block = [0u16; SYMBOLS];
                for ((&x, &row_factor), logs) in xs.iter().zip(&row_factors).zip(&sum_logs) {
                    let factor = (row_factor + column_factor + inverse(log(x ^ y))) % ORDER as u64;
                    field.add_scaled(&mut block, factor as usize, logs);
                }
                block
            })
            .collect();
        // The spare row, if there is one, must come out at zero with the blocks rebuilt.
        if let (Some(&row), Some(sum)) = (rows.get(n), sums.get(n)) {
            let mut check = *sum;
            for (&column, block) in missing.iter().zip(&blocks) {
                let block_logs = field.logs_of(block);
                field.add_scaled(&mut check, field.coefficient_log(row, column), &block_logs);
            }
            if check != [0; SYMBOLS] {
                return None;
            }
        }
        Some(blocks.iter().map(to_bytes).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(blocks: usize) -> Vec<u8> {
        let mut state = 0x2545_F491_u32;
        (0..blocks * DATA_LEN - 17)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as u8
            })
            .collect()
    }

    fn padded(data: &[u8], blocks: usize) -> Vec<u8> {
        let mut padded = data.to_vec();
        padded.resize(blocks * DATA_LEN, 0);
        padded
    }

    /// Takes the blocks at `lost` and the parity blocks at `parity_lost` away and rebuilds.
    fn lose_and_rebuild(
        blocks: usize,
        count: usize,
        lost: &[usize],
        parity_lost: &[usize],
    ) -> bool {
        let data = sample(blocks);
        let code = Code::new(blocks);
        let parity: HashMap<usize, [u8; DATA_LEN]> = code
            .encode(&data, count)
            .into_iter()
            .enumerate()
            .filter(|(index, _)| !parity_lost.contains(index))
            .collect();
        let original = padded(&data, blocks);
        let mut damaged = original.clone();
        let mut present = vec![true; blocks];
        for &i in lost {
            present[i] = false;
            damaged[i * DATA_LEN..(i + 1) * DATA_LEN].fill(0xEE);
        }
        let rebuilt = code.recover(&mut damaged, &mut present, &parity, None);
        rebuilt == lost.len() && damaged == original && present.iter().all(|&p| p)
    }

    #[test]
    fn the_field_has_inverses() {
        let field = field();
        for a in [1u16, 2, 3, 0x1234, 0xFFFF, 0x8000] {
            assert_eq!(field.mul(a, field.inv(a)), 1);
        }
        assert_eq!(field.mul(0, 7), 0);
    }

    #[test]
    fn addresses_lie_between_data_blocks_and_name_their_index() {
        for index in [0, 1, 88, 89, 90, 1_000_000] {
            let address = address(index);
            assert_ne!(address as usize % DATA_LEN, 0);
            assert_eq!(index_of(address), Some(index));
        }
        assert_eq!(index_of(900), None);
        assert_eq!(address(0), 1);
        assert_eq!(address(89), 91);
    }

    #[test]
    fn any_lost_blocks_up_to_the_parity_count_are_rebuilt() {
        assert!(lose_and_rebuild(40, 6, &[0, 7, 8, 9, 30, 39], &[]));
        assert!(lose_and_rebuild(40, 6, &[3, 4], &[0, 2, 5]));
    }

    #[test]
    fn a_parity_block_that_does_not_fit_the_data_is_caught_by_one_more() {
        // A block that passes its checksum but is not the parity block its address names
        // (forged, or of another file) would rebuild wrong blocks. One parity block more than
        // there are missing blocks shows it, and then nothing is rebuilt.
        let blocks = 40;
        let data = sample(blocks);
        let code = Code::new(blocks);
        let original = padded(&data, blocks);
        let mut parity: HashMap<usize, [u8; DATA_LEN]> =
            code.encode(&data, 6).into_iter().enumerate().collect();
        parity.get_mut(&0).unwrap()[10] ^= 1;
        let rebuild = |parity: &HashMap<usize, [u8; DATA_LEN]>| {
            let mut damaged = original.clone();
            let mut present = vec![true; blocks];
            for i in [3, 4] {
                present[i] = false;
                damaged[i * DATA_LEN..(i + 1) * DATA_LEN].fill(0);
            }
            let rebuilt = code.recover(&mut damaged, &mut present, parity, None);
            (rebuilt, present[3], damaged == original)
        };
        assert_eq!(rebuild(&parity), (0, false, false));
        // Without the forged block the others rebuild the file.
        let mut genuine = parity.clone();
        genuine.remove(&0);
        assert_eq!(rebuild(&genuine), (2, true, true));
        // With no parity block to spare there is nothing to check against.
        parity.retain(|&index, _| index < 2);
        assert_eq!(rebuild(&parity), (2, true, false));
    }

    #[test]
    fn the_closed_form_rebuilds_as_many_blocks_as_there_are_parity_blocks() {
        // Every data block lost, and every parity block there: a matrix of the full size.
        let blocks = 300;
        let lost: Vec<usize> = (0..blocks).collect();
        assert!(lose_and_rebuild(blocks, blocks, &lost, &[]));
        // Rows and columns far apart in the field.
        let parity_lost: Vec<usize> = (0..1000).filter(|i| i % 7 != 0).collect();
        assert!(lose_and_rebuild(blocks, 1000, &lost[..143], &parity_lost));
    }

    #[test]
    fn more_lost_blocks_than_parity_blocks_are_not_rebuilt() {
        assert!(!lose_and_rebuild(40, 3, &[1, 2, 3, 4], &[]));
    }

    #[test]
    fn a_large_file_is_cut_into_stripes_that_share_a_lost_run() {
        let blocks = 2 * STRIPE_BLOCKS + 100;
        let code = Code::new(blocks);
        assert_eq!(code.stripes, 3);
        // A run of 300 lost blocks costs every stripe 100; 330 parity blocks give each 110.
        let lost: Vec<usize> = (1000..1300).collect();
        assert!(lose_and_rebuild(blocks, 330, &lost, &[]));
    }

    #[test]
    fn each_stripe_is_rebuilt_from_its_own_share_of_the_parity_blocks() {
        // As the README says: 16385 blocks are two stripes; 1 % parity are 164 blocks, 82 for
        // stripe 0 (the even blocks) and 82 for stripe 1. 83 even blocks lost are too many for
        // stripe 0, although all 164 parity blocks are there; 83 blocks in a row cost each
        // stripe 41 or 42.
        let blocks = STRIPE_BLOCKS + 1;
        let parity = blocks.div_ceil(100);
        assert_eq!((Code::new(blocks).stripes, parity), (2, 164));
        let even: Vec<usize> = (0..83).map(|i| 2 * i).collect();
        assert!(!lose_and_rebuild(blocks, parity, &even, &[]));
        let run: Vec<usize> = (100..183).collect();
        assert!(lose_and_rebuild(blocks, parity, &run, &[]));
        // 82 even blocks are just what stripe 0 can take.
        assert!(lose_and_rebuild(blocks, parity, &even[..82], &[]));
    }

    #[test]
    fn the_most_parity_blocks_fit_the_field() {
        // A full stripe leaves room for three times its length, a shorter file for more.
        let code = Code::new(STRIPE_BLOCKS);
        assert_eq!(code.max_parity(), FIELD_SIZE - STRIPE_BLOCKS);
        assert_eq!(code.max_parity(), 3 * STRIPE_BLOCKS);
        assert_eq!(Code::new(1000).max_parity(), FIELD_SIZE - 1000);
        // Several stripes: at least three parity blocks per data block still.
        for blocks in [STRIPE_BLOCKS + 1, 5 * STRIPE_BLOCKS - 7] {
            assert!(Code::new(blocks).max_parity() >= 3 * blocks);
        }
    }
}

#[cfg(test)]
mod speed {
    use super::*;

    /// Times encoding and rebuilding for about 18 pages of data blocks.
    /// `cargo test --release -p paperback-rs --lib parity_speed -- --ignored --nocapture`
    #[test]
    #[ignore = "timing, not a check"]
    fn parity_speed() {
        let blocks = 33_000;
        let data: Vec<u8> = (0..blocks * DATA_LEN)
            .map(|i| (i * 7 + i / 13) as u8)
            .collect();
        let code = Code::new(blocks);
        let start = std::time::Instant::now();
        let parity = code.encode(&data, 6_600);
        eprintln!("encode 6600 parity blocks: {:?}", start.elapsed());
        let parity: HashMap<usize, [u8; DATA_LEN]> = parity.into_iter().enumerate().collect();
        for lost in [100, 1_900, 5_000] {
            let mut damaged = data.clone();
            let mut present = vec![true; blocks];
            present[10_000..10_000 + lost].fill(false);
            let start = std::time::Instant::now();
            assert_eq!(
                code.recover(&mut damaged, &mut present, &parity, None),
                lost
            );
            eprintln!("rebuild {lost} blocks: {:?}", start.elapsed());
            assert_eq!(damaged, data);
        }
        // One full stripe, 10000 of its blocks lost.
        let blocks = STRIPE_BLOCKS;
        let data = &data[..blocks * DATA_LEN];
        let code = Code::new(blocks);
        let parity: HashMap<usize, [u8; DATA_LEN]> =
            code.encode(data, 10_000).into_iter().enumerate().collect();
        let mut damaged = data.to_vec();
        let mut present = vec![true; blocks];
        present[..10_000].fill(false);
        let start = std::time::Instant::now();
        assert_eq!(
            code.recover(&mut damaged, &mut present, &parity, None),
            10_000
        );
        eprintln!("rebuild 10000 blocks of one stripe: {:?}", start.elapsed());
        assert_eq!(damaged, data);
    }
}
