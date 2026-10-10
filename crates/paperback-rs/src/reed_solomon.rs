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

use crate::simd;

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
    /// `generator_multiples[f]` is `f` times the low 32 coefficients of the generator
    /// polynomial, the 32 bytes as four little-endian words (byte `k` is the coefficient of
    /// `x^k`): what dividing a word by the generator subtracts when `f` falls off the top.
    generator_multiples: Vec<[u64; 4]>,
    /// `evaluation[k][n]` holds, byte `i`, `n * root_i^k` for the 16 values `n` of a nibble, and
    /// `evaluation[k][16 + n]` the same for `n << 4`: with them the remainder of the division
    /// by the generator is turned into the 32 syndromes by table rows alone.
    evaluation: Vec<[[u64; 4]; 32]>,
    /// `locator_power[j][p]` is `alpha^(j * (p + 1))`: the `j`-th term of the error locator
    /// polynomial at the `p`-th place of the search for its roots. The rows have 256 places, the
    /// last one unused, so that they are whole vectors.
    locator_power: Vec<[u8; 256]>,
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
        let times = |a: u8, b: u8| {
            if a == 0 || b == 0 {
                0
            } else {
                alpha[(index[a as usize] as usize + index[b as usize] as usize) % FIELD_ORDER]
            }
        };
        let roots: Vec<u8> = (0..PARITY_LEN)
            .map(|i| alpha[((FIRST_ROOT + i) * ROOT_STEP) % FIELD_ORDER])
            .collect();
        // The generator polynomial: the product of (x + root) over the roots.
        let mut generator = vec![1u8];
        for &root in &roots {
            let mut next = vec![0u8; generator.len() + 1];
            for (k, &coefficient) in generator.iter().enumerate() {
                next[k + 1] ^= coefficient;
                next[k] ^= times(root, coefficient);
            }
            generator = next;
        }
        let pack = |bytes: [u8; PARITY_LEN]| -> [u64; 4] {
            std::array::from_fn(|word| {
                u64::from_le_bytes(std::array::from_fn(|byte| bytes[word * 8 + byte]))
            })
        };
        let generator_multiples = (0..=u8::MAX)
            .map(|f| pack(std::array::from_fn(|k| times(f, generator[k]))))
            .collect();
        let evaluation = (0..PARITY_LEN)
            .map(|k| {
                std::array::from_fn(|entry| {
                    let n = if entry < 16 { entry } else { (entry - 16) << 4 } as u8;
                    pack(std::array::from_fn(|i| {
                        let power = (0..k).fold(1u8, |p, _| times(p, roots[i]));
                        times(n, power)
                    }))
                })
            })
            .collect();
        let locator_power = (0..=PARITY_LEN)
            .map(|j| {
                std::array::from_fn(|p| {
                    if p < FIELD_ORDER {
                        alpha[(j * (p + 1)) % FIELD_ORDER]
                    } else {
                        0
                    }
                })
            })
            .collect();
        Field {
            alpha,
            index,
            generator_multiples,
            evaluation,
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
        generator_multiples,
        evaluation,
        locator_power,
    } = field();
    let log = |value: u8| index[value as usize];
    let exp = |power: usize| alpha[power % FIELD_ORDER];

    // The word divided by the generator polynomial, 32 bytes held in four words: the shift
    // register of the encoder, run over the received word. Its value at a root is the syndrome of
    // that root, and it is zero exactly when all syndromes are.
    let mut remainder = [0u64; 4];
    for &byte in &data[..CODEWORD_LEN - pad] {
        let fed_back = &generator_multiples[(remainder[3] >> 56) as usize];
        remainder = [
            ((remainder[0] << 8) | u64::from(byte)) ^ fed_back[0],
            ((remainder[1] << 8) | (remainder[0] >> 56)) ^ fed_back[1],
            ((remainder[2] << 8) | (remainder[1] >> 56)) ^ fed_back[2],
            ((remainder[3] << 8) | (remainder[2] >> 56)) ^ fed_back[3],
        ];
    }
    if remainder == [0; 4] {
        return Some(0);
    }
    let mut syndromes = [0u64; 4];
    for (k, row) in evaluation.iter().enumerate() {
        let coefficient = (remainder[k / 8] >> (8 * (k % 8))) as u8;
        for (sum, (low, high)) in syndromes.iter_mut().zip(
            row[usize::from(coefficient & 0x0F)]
                .iter()
                .zip(&row[16 + usize::from(coefficient >> 4)]),
        ) {
            *sum ^= low ^ high;
        }
    }
    let mut syndrome = [0u8; PARITY_LEN];
    for (i, s) in syndrome.iter_mut().enumerate() {
        *s = log((syndromes[i / 8] >> (8 * (i % 8))) as u8);
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
    // Berlekamp-Massey. The polynomials are kept as they are, not as logarithms, so that its
    // update is one row times a constant, which the vector code does a whole row at a time.
    let mut b = lambda;
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
        if discrepancy == 0 {
            b.copy_within(0..PARITY_LEN, 1);
            b[0] = 0;
        } else {
            let mut next = lambda;
            simd::xor_scaled(&mut next[1..], discrepancy, &b[..PARITY_LEN]);
            if 2 * el < r + erasures.len() {
                el = r + erasures.len() - el;
                let inverse = exp(FIELD_ORDER - log(discrepancy) as usize);
                b = [0; PARITY_LEN + 1];
                simd::xor_scaled(&mut b, inverse, &lambda);
            } else {
                b.copy_within(0..PARITY_LEN, 1);
                b[0] = 0;
            }
            lambda = next;
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
    // up one coefficient after the other, each a fixed row of powers times the coefficient.
    let mut values = [1u8; 256];
    for (j, &coefficient) in linear_lambda.iter().enumerate().take(degree + 1).skip(1) {
        simd::xor_scaled(&mut values, coefficient, &locator_power[j]);
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

    type Decoder = dyn FnMut(&mut [u8; BLOCK]) -> Option<usize>;

    /// Time per call of the decoder on words that are not codewords, which is what most calls
    /// are: the page is turned the wrong way or the threshold is wrong. Run with
    /// `cargo test --release -- --ignored --nocapture decoder_speed`.
    #[test]
    #[ignore = "a speed measurement, not a test"]
    fn decoder_speed() {
        for level in simd::Level::all_available() {
            println!("level {}", level.name());
            simd::tests::with_level(level, measure_decoder);
        }
        // The way the program runs: the level found once, the kernel looked up once.
        println!("in use: {}", simd::level().name());
        measure_decoder();
    }

    fn measure_decoder() {
        use std::time::Instant;
        const CALLS: usize = 200_000;
        let mut state = 0x1234_5678_u32;
        let mut random_word = || -> [u8; BLOCK] {
            std::array::from_fn(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 24) as u8
            })
        };
        let words: Vec<[u8; BLOCK]> = (0..64).map(|_| random_word()).collect();
        let erased: Vec<usize> = (0..24).map(|i| i * 5).collect();
        let time = |what: &str, mut call: Box<Decoder>| {
            let start = Instant::now();
            let mut accepted = 0;
            for n in 0..CALLS {
                let mut word = words[n % words.len()];
                accepted += usize::from(call(&mut word).is_some());
            }
            let per_call = start.elapsed().as_secs_f64() * 1e9 / CALLS as f64;
            println!("{what:<34} {per_call:>8.0} ns per call, {accepted} accepted");
        };
        // A codeword has no syndromes to speak of: the cost of the syndromes alone.
        let message: [u8; BLOCK - PARITY_LEN] = std::array::from_fn(|i| (i * 7 + 3) as u8);
        let mut valid = [0u8; BLOCK];
        valid[..message.len()].copy_from_slice(&message);
        let parity = encode(&valid, PAD);
        valid[BLOCK - PARITY_LEN..].copy_from_slice(&parity);
        let start = Instant::now();
        for _ in 0..CALLS {
            let mut word = valid;
            assert_eq!(decode(&mut word, PAD), Some(0));
        }
        println!(
            "{:<34} {:>8.0} ns per call",
            "a codeword (syndromes only)",
            start.elapsed().as_secs_f64() * 1e9 / CALLS as f64
        );
        time("no erasures, any errors", Box::new(|w| decode(w, PAD)));
        time(
            "no erasures, at most 16",
            Box::new(|w| decode_up_to(w, PAD, 16)),
        );
        let e16 = erased[..16].to_vec();
        time(
            "16 erasures",
            Box::new(move |w| decode_with_erasures(w, PAD, &e16)),
        );
        time(
            "24 erasures",
            Box::new(move |w| decode_with_erasures(w, PAD, &erased)),
        );
    }

    /// The decoder as it was before it was made faster: logarithms throughout, one place of the
    /// root search at a time. Everything faster is compared with it.
    #[allow(
        clippy::too_many_lines,
        reason = "a copy of the original routine, kept as it was"
    )]
    fn reference_decode(data: &mut [u8], pad: usize, erasures: &[usize]) -> Option<usize> {
        if erasures.len() > PARITY_LEN || erasures.iter().any(|&at| at >= CODEWORD_LEN - pad) {
            return None;
        }
        let Field { alpha, index, .. } = field();
        let log = |value: u8| index[value as usize];
        let exp = |power: usize| alpha[power % FIELD_ORDER];

        let mut syndrome = [0u8; PARITY_LEN];
        syndrome.fill(data[0]);
        for &byte in &data[1..CODEWORD_LEN - pad] {
            for (i, s) in syndrome.iter_mut().enumerate() {
                *s = if *s == 0 {
                    byte
                } else {
                    byte ^ exp(log(*s) as usize + (FIRST_ROOT + i) * ROOT_STEP)
                };
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
            for i in 0..r {
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

        let mut degree = 0usize;
        for (i, l) in lambda.iter_mut().enumerate() {
            *l = log(*l);
            if *l != ZERO_LOG {
                degree = i;
            }
        }

        let mut reg = [0u8; PARITY_LEN + 1];
        reg[1..].copy_from_slice(&lambda[1..]);
        let mut roots = [0usize; PARITY_LEN];
        let mut locations = [0usize; PARITY_LEN];
        let mut count = 0usize;
        let mut k = 115usize;
        for i in 1..=FIELD_ORDER {
            let mut q = 1u8;
            for j in (1..=degree).rev() {
                if reg[j] != ZERO_LOG {
                    reg[j] = ((reg[j] as usize + j) % FIELD_ORDER) as u8;
                    q ^= alpha[reg[j] as usize];
                }
            }
            if q == 0 {
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

    fn xorshift(state: &mut u32) -> u32 {
        *state ^= *state << 13;
        *state ^= *state >> 17;
        *state ^= *state << 5;
        *state
    }

    /// A codeword with some bytes changed and some positions named as erasures.
    fn damaged_word(state: &mut u32) -> ([u8; BLOCK], Vec<usize>) {
        let mut word = [0u8; BLOCK];
        for byte in &mut word[..BLOCK - PARITY_LEN] {
            *byte = xorshift(state) as u8;
        }
        let parity = encode(&word, PAD);
        word[BLOCK - PARITY_LEN..].copy_from_slice(&parity);
        let errors = xorshift(state) as usize % 26;
        for _ in 0..errors {
            let at = xorshift(state) as usize % BLOCK;
            word[at] ^= (xorshift(state) as u8) | 1;
        }
        let erasures = xorshift(state) as usize % 3 * 8;
        let mut named = Vec::new();
        while named.len() < erasures {
            let at = xorshift(state) as usize % BLOCK;
            if !named.contains(&at) {
                named.push(at);
            }
        }
        (word, named)
    }

    /// Compares the decoder with the original routine on damaged words, at the level in use.
    fn agrees_with_the_original_routine(words: usize) {
        let mut state = 0x9E37_79B9;
        for _ in 0..words {
            let (word, erasures) = damaged_word(&mut state);
            let (mut new, mut old) = (word, word);
            let found = decode_with_erasures(&mut new, PAD, &erasures);
            let expected = reference_decode(&mut old, PAD, &erasures);
            assert_eq!((found, new), (expected, old), "erasures {erasures:?}");
            // With a limit it does the same, or gives up where the result would be refused.
            for limit in [0, 8, 16] {
                let (mut limited, mut original) = (word, word);
                let got = decode_limited(&mut limited, PAD, &erasures, limit);
                match reference_decode(&mut original, PAD, &erasures) {
                    Some(count) if count <= limit => {
                        assert_eq!((got, limited), (Some(count), original));
                    }
                    _ => assert_eq!(got, None, "limit {limit}, erasures {erasures:?}"),
                }
            }
        }
    }

    #[test]
    fn the_faster_decoder_does_what_the_original_routine_does() {
        for level in simd::Level::all_available() {
            simd::tests::with_level(level, || agrees_with_the_original_routine(1500));
        }
    }
}
