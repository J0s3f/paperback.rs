// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider
// Derived from PaperBack 1.10, Copyright (c) 2007 Oleh Yuschuk; see NOTICE.md.

//! Shortened Reed-Solomon (255,223) code over GF(256), derived from the
//! software by Phil Karn, KA9Q (2002), as adapted by PaperBack.
//!
//! The field polynomial is 0x187, the first consecutive root is alpha^112 and
//! the primitive element is alpha^11. These parameters are fixed by the
//! on-paper format and differ from the common CCSDS and QR variants, so the
//! code is ported rather than taken from a general purpose crate.

#![allow(clippy::needless_range_loop)]

use std::sync::OnceLock;

pub(crate) const PARITY_LEN: usize = 32;
pub(crate) const CODEWORD_LEN: usize = 255;
pub(crate) const MESSAGE_LEN: usize = CODEWORD_LEN - PARITY_LEN;

const FIELD_POLYNOMIAL: u16 = 0x187;
const FIELD_ORDER: usize = 255;
const FIRST_ROOT: usize = 112;
const ROOT_STEP: usize = 11;
const ZERO_LOG: u8 = 255;

/// Generator polynomial coefficients in index (logarithm) form.
const GENERATOR: [u8; PARITY_LEN + 1] = [
    0, 249, 59, 66, 4, 43, 126, 251, 97, 30, 3, 213, 50, 66, 170, 5, 24, 5, 170, 66, 50, 213, 3,
    30, 97, 251, 126, 43, 4, 66, 59, 249, 0,
];

struct Field {
    alpha: [u8; FIELD_ORDER + 1],
    index: [u8; FIELD_ORDER + 1],
    /// `syndrome_step[i][s]` is `s` times the `i`-th root of the code: one step of the evaluation
    /// of the received word at that root, without logarithms.
    syndrome_step: [[u8; 256]; PARITY_LEN],
    /// `product[a][b]` is `a * b`.
    product: Vec<[u8; 256]>,
    /// `locator_power[j][p]` is `alpha^(j * (p + 1))`: the `j`-th term of the error locator
    /// polynomial at the `p`-th place of the search for its roots.
    locator_power: Vec<[u8; FIELD_ORDER]>,
}

fn field() -> &'static Field {
    static FIELD: OnceLock<Field> = OnceLock::new();
    FIELD.get_or_init(|| {
        let mut alpha = [0u8; FIELD_ORDER + 1];
        let mut index = [ZERO_LOG; FIELD_ORDER + 1];
        let mut element: u16 = 1;
        for (power, slot) in alpha.iter_mut().enumerate().take(FIELD_ORDER) {
            *slot = element as u8;
            index[element as usize] = power as u8;
            element <<= 1;
            if element & 0x100 != 0 {
                element ^= FIELD_POLYNOMIAL;
            }
        }
        let mut syndrome_step = [[0u8; 256]; PARITY_LEN];
        for (i, row) in syndrome_step.iter_mut().enumerate() {
            let root = (FIRST_ROOT + i) * ROOT_STEP;
            for (value, product) in row.iter_mut().enumerate().skip(1) {
                *product = alpha[(index[value] as usize + root) % FIELD_ORDER];
            }
        }
        let product = (0..256usize)
            .map(|a| {
                std::array::from_fn(|b| {
                    if a == 0 || b == 0 {
                        0
                    } else {
                        alpha[(index[a] as usize + index[b] as usize) % FIELD_ORDER]
                    }
                })
            })
            .collect();
        let locator_power = (0..=PARITY_LEN)
            .map(|j| std::array::from_fn(|p| alpha[(j * (p + 1)) % FIELD_ORDER]))
            .collect();
        Field {
            alpha,
            index,
            syndrome_step,
            product,
            locator_power,
        }
    })
}

/// Computes the parity of the first `MESSAGE_LEN - pad` bytes of `data`.
pub(crate) fn encode(data: &[u8], pad: usize) -> [u8; PARITY_LEN] {
    let Field { alpha, index, .. } = field();
    let mut parity = [0u8; PARITY_LEN];
    for &byte in &data[..MESSAGE_LEN - pad] {
        let feedback = index[(byte ^ parity[0]) as usize] as usize;
        if feedback != ZERO_LOG as usize {
            for j in 1..PARITY_LEN {
                parity[j] ^= alpha[(feedback + GENERATOR[PARITY_LEN - j] as usize) % FIELD_ORDER];
            }
        }
        parity.copy_within(1.., 0);
        parity[PARITY_LEN - 1] = if feedback == ZERO_LOG as usize {
            0
        } else {
            alpha[(feedback + GENERATOR[0] as usize) % FIELD_ORDER]
        };
    }
    parity
}

/// Corrects `data` (message followed by parity, `CODEWORD_LEN - pad` bytes) in
/// place. Returns the number of corrected bytes, or `None` if the block is
/// beyond repair.
#[cfg(test)]
pub(crate) fn decode(data: &mut [u8], pad: usize) -> Option<usize> {
    decode_with_erasures(data, pad, &[])
}

/// Like [`decode`], but gives up as soon as it is clear that more than `max_corrections` bytes
/// are wrong. Most blocks that are tried are not blocks at all (the wrong turn of the page, the
/// wrong threshold), and for those this saves searching for the error positions.
pub(crate) fn decode_up_to(data: &mut [u8], pad: usize, max_corrections: usize) -> Option<usize> {
    decode_limited(data, pad, &[], max_corrections)
}

/// Like [`decode`], for bytes at known positions that are probably damaged (their dots
/// were hard to read). Such an erasure costs one parity byte instead of two, so with `e`
/// erasures up to `(32 - e) / 2` further errors can be corrected. `erasures` are indices
/// into `data`; the contents at those positions do not matter.
#[allow(
    clippy::too_many_lines,
    reason = "a close port of the original routine; splitting it would hide the correspondence"
)]
pub(crate) fn decode_with_erasures(
    data: &mut [u8],
    pad: usize,
    erasures: &[usize],
) -> Option<usize> {
    decode_limited(data, pad, erasures, PARITY_LEN)
}

#[allow(
    clippy::too_many_lines,
    reason = "a close port of the original routine; splitting it would hide the correspondence"
)]
#[cfg_attr(feature = "profile", inline(never))]
fn decode_limited(
    data: &mut [u8],
    pad: usize,
    erasures: &[usize],
    max_corrections: usize,
) -> Option<usize> {
    if erasures.len() > PARITY_LEN || erasures.iter().any(|&at| at >= CODEWORD_LEN - pad) {
        return None;
    }
    let Field {
        alpha,
        index,
        syndrome_step,
        product,
        locator_power,
    } = field();
    let log = |value: u8| index[value as usize];
    let exp = |power: usize| alpha[power % FIELD_ORDER];

    let mut syndrome = [0u8; PARITY_LEN];
    syndrome.fill(data[0]);
    for &byte in &data[1..CODEWORD_LEN - pad] {
        for (s, step) in syndrome.iter_mut().zip(syndrome_step) {
            *s = byte ^ step[*s as usize];
        }
    }
    let mut syndrome_nonzero = 0u8;
    for s in &mut syndrome {
        syndrome_nonzero |= *s;
        *s = log(*s);
    }
    if syndrome_nonzero == 0 {
        return Some(0);
    }

    let mut lambda = [0u8; PARITY_LEN + 1];
    lambda[0] = 1;
    // The erasure locator polynomial is the starting point of the search for the others.
    if let Some((&first, others)) = erasures.split_first() {
        lambda[1] = exp(ROOT_STEP * (CODEWORD_LEN - 1 - (first + pad)));
        for (done, &at) in others.iter().enumerate() {
            let u = ROOT_STEP * (CODEWORD_LEN - 1 - (at + pad));
            for j in (1..=done + 2).rev() {
                if lambda[j - 1] != 0 {
                    lambda[j] ^= exp(u + log(lambda[j - 1]) as usize);
                }
            }
        }
    }
    let mut b = [0u8; PARITY_LEN + 1];
    for i in 0..=PARITY_LEN {
        b[i] = log(lambda[i]);
    }
    let mut t = [0u8; PARITY_LEN + 1];
    let mut el = erasures.len();
    for r in erasures.len() + 1..=PARITY_LEN {
        let mut discrepancy = 0u8;
        // Beyond the highest non-zero coefficient there is nothing to add.
        let reach = lambda
            .iter()
            .rposition(|&l| l != 0)
            .map_or(0, |top| top + 1);
        for i in 0..r.min(reach) {
            if lambda[i] != 0 && syndrome[r - i - 1] != ZERO_LOG {
                discrepancy ^= exp(log(lambda[i]) as usize + syndrome[r - i - 1] as usize);
            }
        }
        let discrepancy = log(discrepancy);
        if discrepancy == ZERO_LOG {
            b.copy_within(0..PARITY_LEN, 1);
            b[0] = ZERO_LOG;
        } else {
            t[0] = lambda[0];
            for i in 0..PARITY_LEN {
                t[i + 1] = if b[i] == ZERO_LOG {
                    lambda[i + 1]
                } else {
                    lambda[i + 1] ^ exp(discrepancy as usize + b[i] as usize)
                };
            }
            if 2 * el < r + erasures.len() {
                el = r + erasures.len() - el;
                for i in 0..=PARITY_LEN {
                    b[i] = if lambda[i] == 0 {
                        ZERO_LOG
                    } else {
                        ((log(lambda[i]) as usize + FIELD_ORDER - discrepancy as usize)
                            % FIELD_ORDER) as u8
                    };
                }
            } else {
                b.copy_within(0..PARITY_LEN, 1);
                b[0] = ZERO_LOG;
            }
            lambda = t;
        }
    }

    let linear_lambda = lambda;
    let mut degree = 0usize;
    for (i, l) in lambda.iter_mut().enumerate() {
        *l = log(*l);
        if *l != ZERO_LOG {
            degree = i;
        }
    }
    // More errors than the caller can use: no need to look for where they are.
    if degree > max_corrections {
        return None;
    }

    // The error locator polynomial at every place of the search at once: its terms are added
    // up one coefficient after the other, each a table row times a fixed row of powers.
    let mut values = [1u8; FIELD_ORDER];
    for (j, &coefficient) in linear_lambda.iter().enumerate().take(degree + 1).skip(1) {
        if coefficient == 0 {
            continue;
        }
        let times = &product[coefficient as usize];
        for (value, &power) in values.iter_mut().zip(&locator_power[j]) {
            *value ^= times[power as usize];
        }
    }
    let mut roots = [0usize; PARITY_LEN];
    let mut locations = [0usize; PARITY_LEN];
    let mut count = 0usize;
    let mut k = 115usize;
    for (i, &value) in (1..=FIELD_ORDER).zip(&values) {
        if value == 0 {
            roots[count] = i;
            locations[count] = k;
            count += 1;
            if count == degree {
                break;
            }
        }
        k = (k + 116) % FIELD_ORDER;
    }
    if degree != count {
        return None;
    }

    let omega_degree = degree.saturating_sub(1);
    let mut omega = [0u8; PARITY_LEN + 1];
    for i in 0..=omega_degree {
        let mut sum = 0u8;
        for j in (0..=i).rev() {
            if syndrome[i - j] != ZERO_LOG && lambda[j] != ZERO_LOG {
                sum ^= exp(syndrome[i - j] as usize + lambda[j] as usize);
            }
        }
        omega[i] = log(sum);
    }
    for j in (0..count).rev() {
        let mut numerator1 = 0u8;
        for i in (0..=omega_degree).rev() {
            if omega[i] != ZERO_LOG {
                numerator1 ^= exp(omega[i] as usize + i * roots[j]);
            }
        }
        let numerator2 = exp(roots[j] * 111 + FIELD_ORDER);
        let mut denominator = 0u8;
        let mut i = (degree.min(31)) & !1usize;
        loop {
            if lambda[i + 1] != ZERO_LOG {
                denominator ^= exp(lambda[i + 1] as usize + i * roots[j]);
            }
            if i < 2 {
                break;
            }
            i -= 2;
        }
        if numerator1 != 0 && locations[j] >= pad {
            let shift = log(numerator1) as usize + log(numerator2) as usize + FIELD_ORDER
                - log(denominator) as usize;
            data[locations[j] - pad] ^= exp(shift);
        }
    }
    Some(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAD: usize = 127;
    const BLOCK: usize = CODEWORD_LEN - PAD;

    fn sample_codeword() -> [u8; BLOCK] {
        let mut block = [0u8; BLOCK];
        for (i, byte) in block.iter_mut().take(MESSAGE_LEN - PAD).enumerate() {
            *byte = (i * 7 + 3) as u8;
        }
        let parity = encode(&block, PAD);
        block[MESSAGE_LEN - PAD..].copy_from_slice(&parity);
        block
    }

    #[test]
    fn field_tables_match_the_reference_values() {
        let Field { alpha, index, .. } = field();
        assert_eq!(
            &alpha[..10],
            &[0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x87, 0x89]
        );
        assert_eq!(alpha[254], 0xc3);
        assert_eq!(&index[..8], &[255, 0, 1, 99, 2, 198, 100, 106]);
    }

    #[test]
    fn corrects_thirty_two_erased_bytes_at_known_positions() {
        let original = sample_codeword();
        let mut damaged = original;
        let erased: Vec<usize> = (0..32).map(|i| i * 4 + 1).collect();
        for &at in &erased {
            damaged[at] ^= 0xA5;
        }
        assert!(decode_with_erasures(&mut damaged, PAD, &erased).is_some());
        assert_eq!(damaged, original);
    }

    #[test]
    fn corrects_erasures_together_with_unknown_errors() {
        // 20 erasures use 20 parity bytes, the other 12 cover 6 unknown errors.
        let original = sample_codeword();
        let mut damaged = original;
        let erased: Vec<usize> = (0..20).map(|i| i * 6).collect();
        for &at in &erased {
            damaged[at] ^= 0x3C;
        }
        for at in [3, 11, 29, 47, 71, 101] {
            damaged[at] ^= 0x81;
        }
        assert!(decode_with_erasures(&mut damaged, PAD, &erased).is_some());
        assert_eq!(damaged, original);
    }

    #[test]
    fn erasures_that_were_not_damaged_are_harmless() {
        let original = sample_codeword();
        let mut block = original;
        block[40] ^= 0x11;
        assert!(decode_with_erasures(&mut block, PAD, &[5, 40, 90]).is_some());
        assert_eq!(block, original);
    }

    #[test]
    fn too_many_unknown_errors_are_still_refused() {
        let mut damaged = sample_codeword();
        for i in 0..20 {
            damaged[i * 5] ^= 0x77;
        }
        assert_eq!(decode_with_erasures(&mut damaged, PAD, &[1, 2]), None);
    }

    #[test]
    fn clean_codeword_needs_no_correction() {
        let mut block = sample_codeword();
        assert_eq!(decode(&mut block, PAD), Some(0));
    }

    #[test]
    fn corrects_up_to_sixteen_damaged_bytes() {
        let original = sample_codeword();
        let mut damaged = original;
        for i in 0..16 {
            damaged[i * 8] ^= 0x5A + i as u8;
        }
        assert_eq!(decode(&mut damaged, PAD), Some(16));
        assert_eq!(damaged, original);
    }

    #[test]
    fn gives_up_beyond_sixteen_damaged_bytes() {
        let mut damaged = sample_codeword();
        for i in 0..40 {
            damaged[i * 3] ^= 0x33 + i as u8;
        }
        let verdict = decode(&mut damaged, PAD);
        assert!(verdict.is_none_or(|n| n <= 16));
    }
}
