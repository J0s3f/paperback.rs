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
        Field { alpha, index }
    })
}

/// Computes the parity of the first `MESSAGE_LEN - pad` bytes of `data`.
pub(crate) fn encode(data: &[u8], pad: usize) -> [u8; PARITY_LEN] {
    let Field { alpha, index } = field();
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
#[allow(
    clippy::too_many_lines,
    reason = "a close port of the original routine; splitting it would hide the correspondence"
)]
pub(crate) fn decode(data: &mut [u8], pad: usize) -> Option<usize> {
    let Field { alpha, index } = field();
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
    let mut b = [0u8; PARITY_LEN + 1];
    for i in 0..=PARITY_LEN {
        b[i] = log(lambda[i]);
    }
    let mut t = [0u8; PARITY_LEN + 1];
    let mut el = 0usize;
    for r in 1..=PARITY_LEN {
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
            if 2 * el < r {
                el = r - el;
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
        let Field { alpha, index } = field();
        assert_eq!(
            &alpha[..10],
            &[0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x87, 0x89]
        );
        assert_eq!(alpha[254], 0xc3);
        assert_eq!(&index[..8], &[255, 0, 1, 99, 2, 198, 100, 106]);
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
