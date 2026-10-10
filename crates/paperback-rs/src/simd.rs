// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

//! Multiplying whole rows of bytes in GF(256) with the field polynomial of the on-paper format
//! (0x187) on the processor's vector units, chosen when the program runs.
//!
//! The only operation is `dst ^= c * src` for a constant `c` and equal-length rows, which is
//! what the error correction spends most of its time on. Five implementations exist and give the
//! same result byte for byte:
//!
//! * [`Level::Scalar`]: two table lookups per byte; the reference, and the only one on
//!   processors that have none of the others;
//! * [`Level::Neon`]: the same lookups as `vqtbl1q_u8` table lookups, 16 bytes at a time, on
//!   64-bit ARM, where NEON is part of every processor;
//! * [`Level::Ssse3`]: the same lookups as `pshufb` shuffles, 16 bytes at a time;
//! * [`Level::Avx2`]: 32 bytes at a time;
//! * [`Level::Gfni`]: one `gf2p8affineqb` per 32 bytes, which multiplies by a constant of any
//!   field polynomial when given the 8 x 8 bit matrix of the constant.
//!
//! The level is the best the processor offers when the program starts. The environment variable
//! `PAPERBACK_SIMD` (`scalar`, `ssse3`, `avx2` or `gfni`) asks for a lower one, which is how
//! the levels are compared on one machine; a level the processor lacks is never used.
//!
//! This module is the only place of the program with `unsafe` code: the vector intrinsics that
//! load and store through pointers can be called only so, and a function that needs a processor
//! feature can be called only after the feature was seen. Each use says why it is sound.

#![allow(
    unsafe_code,
    reason = "vector intrinsics; each use is explained and the module is the only one with unsafe"
)]

use std::sync::OnceLock;

/// What the processor can do for the multiplications. On one processor the levels it offers are
/// ordered from least to most; the vector levels of ARM and of x86-64 never meet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Plain code.
    Scalar,
    /// 128-bit table lookups (64-bit ARM: NEON).
    Neon,
    /// 128-bit shuffles (x86-64: SSSE3).
    Ssse3,
    /// 256-bit shuffles (x86-64: AVX2).
    Avx2,
    /// Galois field instructions (x86-64: GFNI with AVX2).
    Gfni,
}

impl Level {
    const ALL: [Self; 5] = [
        Self::Scalar,
        Self::Neon,
        Self::Ssse3,
        Self::Avx2,
        Self::Gfni,
    ];

    /// The name the environment variable and the diagnostics use.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Scalar => "scalar",
            Self::Neon => "neon",
            Self::Ssse3 => "ssse3",
            Self::Avx2 => "avx2",
            Self::Gfni => "gfni",
        }
    }

    /// Whether this processor can run the level.
    #[must_use]
    pub fn is_available(self) -> bool {
        match self {
            Self::Scalar => true,
            // NEON is part of every 64-bit ARM processor.
            Self::Neon => cfg!(target_arch = "aarch64"),
            #[cfg(target_arch = "x86_64")]
            Self::Ssse3 => is_x86_feature_detected!("ssse3"),
            #[cfg(target_arch = "x86_64")]
            Self::Avx2 => is_x86_feature_detected!("avx2"),
            #[cfg(target_arch = "x86_64")]
            Self::Gfni => is_x86_feature_detected!("gfni") && is_x86_feature_detected!("avx2"),
            #[cfg(not(target_arch = "x86_64"))]
            Self::Ssse3 | Self::Avx2 | Self::Gfni => false,
        }
    }

    /// The best level this processor offers.
    #[must_use]
    pub fn best_available() -> Self {
        Self::ALL
            .into_iter()
            .rev()
            .find(|level| level.is_available())
            .unwrap_or(Self::Scalar)
    }

    /// Every level this processor can run, lowest first.
    #[must_use]
    pub fn all_available() -> Vec<Self> {
        Self::ALL
            .into_iter()
            .filter(|level| level.is_available())
            .collect()
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|level| level.name().eq_ignore_ascii_case(name.trim()))
    }
}

/// The level in use: the best available, or the one asked for in `PAPERBACK_SIMD` if the
/// processor has it.
#[must_use]
pub fn level() -> Level {
    static LEVEL: OnceLock<Level> = OnceLock::new();
    #[cfg(test)]
    if let Some(forced) = tests::FORCED.with(std::cell::Cell::get) {
        return forced;
    }
    *LEVEL.get_or_init(|| {
        let best = Level::best_available();
        std::env::var("PAPERBACK_SIMD")
            .ok()
            .and_then(|name| Level::from_name(&name))
            .filter(|asked| asked.is_available())
            .unwrap_or(best)
    })
}

/// The field polynomial of the format, without the leading bit.
const REDUCTION: u16 = 0x87;

/// `a * b` in GF(256) by shifting and adding; the definition the tables are built from.
pub(crate) fn mul(a: u8, b: u8) -> u8 {
    let mut product = 0u16;
    let mut shifted = u16::from(a);
    for bit in 0..8 {
        if b >> bit & 1 == 1 {
            product ^= shifted;
        }
        shifted <<= 1;
        if shifted & 0x100 != 0 {
            shifted ^= 0x100 | REDUCTION;
        }
    }
    product as u8
}

/// What every implementation needs to multiply by one constant.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Scale {
    /// `lo[n]` is the constant times `n`, for the low four bits of a byte.
    lo: [u8; 16],
    /// `hi[n]` is the constant times `n << 4`, for the high four bits.
    hi: [u8; 16],
    /// The constant times every byte.
    full: [u8; 256],
    /// The 8 x 8 bit matrix of multiplying by the constant, as `gf2p8affineqb` wants it: byte
    /// `7 - i` of the 64 bits is the row that makes bit `i` of the product, and bit `j` of a
    /// row selects bit `j` of the byte multiplied.
    #[cfg_attr(
        not(target_arch = "x86_64"),
        allow(dead_code, reason = "only the GFNI kernel reads it")
    )]
    matrix: u64,
}

impl Scale {
    fn new(constant: u8) -> Self {
        let lo = std::array::from_fn(|n| mul(constant, n as u8));
        let hi = std::array::from_fn(|n| mul(constant, (n as u8) << 4));
        let mut matrix = 0u64;
        for bit in 0..8 {
            let mut row = 0u8;
            for source in 0..8 {
                row |= (mul(constant, 1 << source) >> bit & 1) << source;
            }
            matrix |= u64::from(row) << (8 * (7 - bit));
        }
        let full = std::array::from_fn(|byte| mul(constant, byte as u8));
        Self {
            lo,
            hi,
            full,
            matrix,
        }
    }

    /// The tables of every constant, made once.
    pub(crate) fn of(constant: u8) -> &'static Self {
        static SCALES: OnceLock<Vec<Scale>> = OnceLock::new();
        &SCALES.get_or_init(|| (0..=u8::MAX).map(Scale::new).collect())[usize::from(constant)]
    }

    fn times(&self, byte: u8) -> u8 {
        self.full[usize::from(byte)]
    }
}

/// A vector implementation: multiplies the whole vectors of the rows and returns how many bytes
/// it did.
type Kernel = fn(&mut [u8], &Scale, &[u8]) -> usize;

/// The implementation of `level`. A vector implementation is handed out only if the processor
/// has what it needs, which is what makes the wrappers in [`x86`] sound.
fn kernel_for(level: Level) -> Kernel {
    match level {
        #[cfg(target_arch = "x86_64")]
        Level::Ssse3 if is_x86_feature_detected!("ssse3") => x86::ssse3,
        #[cfg(target_arch = "x86_64")]
        Level::Avx2 if is_x86_feature_detected!("avx2") => x86::avx2,
        #[cfg(target_arch = "x86_64")]
        Level::Gfni if is_x86_feature_detected!("gfni") && is_x86_feature_detected!("avx2") => {
            x86::gfni
        }
        #[cfg(target_arch = "aarch64")]
        Level::Neon => arm::neon,
        _ => |_, _, _| 0,
    }
}

/// The implementation in use, looked up once: after that a call is one indirect jump, with no
/// check of the processor's features.
fn kernel() -> Kernel {
    static KERNEL: OnceLock<Kernel> = OnceLock::new();
    #[cfg(test)]
    if tests::FORCED.with(std::cell::Cell::get).is_some() {
        return kernel_for(level());
    }
    *KERNEL.get_or_init(|| kernel_for(level()))
}

/// `dst ^= constant * src`, byte by byte, with the best implementation of this processor.
/// `dst` and `src` must have the same length.
pub(crate) fn xor_scaled(dst: &mut [u8], constant: u8, src: &[u8]) {
    xor_scaled_by(kernel(), dst, constant, src);
}

/// Like [`xor_scaled`] with the implementation of `level`, which must be one the processor has.
#[cfg(test)]
pub(crate) fn xor_scaled_with(level: Level, dst: &mut [u8], constant: u8, src: &[u8]) {
    xor_scaled_by(kernel_for(level), dst, constant, src);
}

fn xor_scaled_by(kernel: Kernel, dst: &mut [u8], constant: u8, src: &[u8]) {
    assert_eq!(dst.len(), src.len(), "rows of different lengths");
    match constant {
        0 => return,
        1 => {
            dst.iter_mut().zip(src).for_each(|(d, s)| *d ^= s);
            return;
        }
        _ => {}
    }
    let scale = Scale::of(constant);
    let done = kernel(dst, scale, src);
    // What the vector code left over, and everything for processors without it.
    for (d, &s) in dst[done..].iter_mut().zip(&src[done..]) {
        *d ^= scale.times(s);
    }
}

#[cfg(target_arch = "aarch64")]
mod arm {
    use std::arch::aarch64::{
        vandq_u8, vdupq_n_u8, veorq_u8, vld1q_u8, vqtbl1q_u8, vshrq_n_u8, vst1q_u8,
    };

    use super::Scale;

    /// 16 bytes at a time through two table lookups per vector; returns the bytes done.
    pub(super) fn neon(dst: &mut [u8], scale: &Scale, src: &[u8]) -> usize {
        let chunks = src.len() / 16;
        // SAFETY: NEON is part of every 64-bit ARM processor. The tables are 16 bytes, and every
        // access is inside the first `chunks * 16` bytes of `src` and `dst`, which the caller
        // gave the same length.
        unsafe {
            let lo = vld1q_u8(scale.lo.as_ptr());
            let hi = vld1q_u8(scale.hi.as_ptr());
            let low_nibbles = vdupq_n_u8(0x0F);
            for chunk in 0..chunks {
                let at = chunk * 16;
                let bytes = vld1q_u8(src.as_ptr().add(at));
                let low = vandq_u8(bytes, low_nibbles);
                let high = vshrq_n_u8::<4>(bytes);
                let product = veorq_u8(vqtbl1q_u8(lo, low), vqtbl1q_u8(hi, high));
                let target = dst.as_mut_ptr().add(at);
                vst1q_u8(target, veorq_u8(vld1q_u8(target), product));
            }
        }
        chunks * 16
    }
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    #[allow(
        clippy::wildcard_imports,
        reason = "the intrinsics are many and are named alike"
    )]
    use std::arch::x86_64::*;

    use super::Scale;

    // The three wrappers below are what `kernel_for` hands out, and only after it has seen the
    // feature each one needs; they are not called from anywhere else.

    pub(super) fn ssse3(dst: &mut [u8], scale: &Scale, src: &[u8]) -> usize {
        // SAFETY: `kernel_for` returns this function only on a processor with SSSE3.
        unsafe { xor_scaled_ssse3(dst, scale, src) }
    }

    pub(super) fn avx2(dst: &mut [u8], scale: &Scale, src: &[u8]) -> usize {
        // SAFETY: `kernel_for` returns this function only on a processor with AVX2.
        unsafe { xor_scaled_avx2(dst, scale, src) }
    }

    pub(super) fn gfni(dst: &mut [u8], scale: &Scale, src: &[u8]) -> usize {
        // SAFETY: `kernel_for` returns this function only on a processor with GFNI and AVX2.
        unsafe { xor_scaled_gfni(dst, scale, src) }
    }

    /// The vectors of 16 bytes that fit in the rows; returns the bytes done. The caller has seen
    /// SSSE3 and checked that the rows are of equal length.
    #[target_feature(enable = "ssse3")]
    unsafe fn xor_scaled_ssse3(dst: &mut [u8], scale: &Scale, src: &[u8]) -> usize {
        let chunks = src.len() / 16;
        // SAFETY: the tables are 16 bytes; every access below is inside the first
        // `chunks * 16` bytes of `src` and `dst`, which have the same length; unaligned loads
        // and stores are used.
        unsafe {
            let lo = _mm_loadu_si128(scale.lo.as_ptr().cast());
            let hi = _mm_loadu_si128(scale.hi.as_ptr().cast());
            let low_nibbles = _mm_set1_epi8(0x0F);
            for chunk in 0..chunks {
                let at = chunk * 16;
                let bytes = _mm_loadu_si128(src.as_ptr().add(at).cast());
                let low = _mm_and_si128(bytes, low_nibbles);
                let high = _mm_and_si128(_mm_srli_epi16(bytes, 4), low_nibbles);
                let product = _mm_xor_si128(_mm_shuffle_epi8(lo, low), _mm_shuffle_epi8(hi, high));
                let target = dst.as_mut_ptr().add(at).cast();
                _mm_storeu_si128(target, _mm_xor_si128(_mm_loadu_si128(target), product));
            }
        }
        chunks * 16
    }

    /// As [`xor_scaled_ssse3`], 32 bytes at a time. The caller has seen AVX2.
    #[target_feature(enable = "avx2")]
    unsafe fn xor_scaled_avx2(dst: &mut [u8], scale: &Scale, src: &[u8]) -> usize {
        let chunks = src.len() / 32;
        // SAFETY: as in `xor_scaled_ssse3`, with 32-byte vectors; the 16-byte tables are
        // loaded into both halves.
        unsafe {
            let lo = _mm256_broadcastsi128_si256(_mm_loadu_si128(scale.lo.as_ptr().cast()));
            let hi = _mm256_broadcastsi128_si256(_mm_loadu_si128(scale.hi.as_ptr().cast()));
            let low_nibbles = _mm256_set1_epi8(0x0F);
            for chunk in 0..chunks {
                let at = chunk * 32;
                let bytes = _mm256_loadu_si256(src.as_ptr().add(at).cast());
                let low = _mm256_and_si256(bytes, low_nibbles);
                let high = _mm256_and_si256(_mm256_srli_epi16(bytes, 4), low_nibbles);
                let product =
                    _mm256_xor_si256(_mm256_shuffle_epi8(lo, low), _mm256_shuffle_epi8(hi, high));
                let target = dst.as_mut_ptr().add(at).cast();
                _mm256_storeu_si256(
                    target,
                    _mm256_xor_si256(_mm256_loadu_si256(target), product),
                );
            }
        }
        chunks * 32
    }

    /// As [`xor_scaled_avx2`], with one affine transformation instead of two shuffles and the
    /// nibble handling. The caller has seen GFNI and AVX2.
    #[target_feature(enable = "gfni,avx2")]
    unsafe fn xor_scaled_gfni(dst: &mut [u8], scale: &Scale, src: &[u8]) -> usize {
        let chunks = src.len() / 32;
        // SAFETY: as in `xor_scaled_avx2`.
        unsafe {
            let matrix = _mm256_set1_epi64x(scale.matrix as i64);
            for chunk in 0..chunks {
                let at = chunk * 32;
                let bytes = _mm256_loadu_si256(src.as_ptr().add(at).cast());
                let product = _mm256_gf2p8affine_epi64_epi8::<0>(bytes, matrix);
                let target = dst.as_mut_ptr().add(at).cast();
                _mm256_storeu_si256(
                    target,
                    _mm256_xor_si256(_mm256_loadu_si256(target), product),
                );
            }
        }
        chunks * 32
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    thread_local! {
        /// The level a test asks for on its thread.
        pub(super) static FORCED: std::cell::Cell<Option<Level>> = const { std::cell::Cell::new(None) };
    }

    /// Runs `body` with every multiplication on this thread done at `level`.
    pub(crate) fn with_level<R>(level: Level, body: impl FnOnce() -> R) -> R {
        let before = FORCED.with(|forced| forced.replace(Some(level)));
        let result = body();
        FORCED.with(|forced| forced.set(before));
        result
    }

    /// Parity of the AND of the matrix row of each result bit with the byte: what the
    /// instruction `gf2p8affineqb` with no constant does to one byte.
    fn affine_model(matrix: u64, byte: u8) -> u8 {
        (0..8).fold(0, |result, bit| {
            let row = (matrix >> (8 * (7 - bit))) as u8;
            result | (((row & byte).count_ones() & 1) as u8) << bit
        })
    }

    #[test]
    fn multiplication_follows_the_field_of_the_format() {
        // x^7 * x = x^8 = x^7 + x^2 + x + 1 under 0x187.
        assert_eq!(mul(0x80, 2), 0x87);
        assert_eq!(mul(1, 0xB7), 0xB7);
        assert_eq!(mul(0, 0xB7), 0);
        for a in 0..=u8::MAX {
            for b in [0, 1, 2, 3, 0x57, 0x80, 0xFF] {
                assert_eq!(mul(a, b), mul(b, a));
            }
        }
        // The element 2 has the full order 255.
        let (mut power, mut order) = (2u8, 1);
        while power != 1 {
            power = mul(power, 2);
            order += 1;
        }
        assert_eq!(order, 255);
    }

    #[test]
    fn the_bit_matrix_of_a_constant_multiplies_by_it() {
        for constant in 0..=u8::MAX {
            let scale = Scale::of(constant);
            for byte in 0..=u8::MAX {
                assert_eq!(affine_model(scale.matrix, byte), mul(constant, byte));
                assert_eq!(scale.times(byte), mul(constant, byte));
            }
        }
    }

    /// Rows of every length around the sizes of the vectors, over bytes that are not aligned.
    fn rows(len: usize, seed: u32) -> Vec<u8> {
        let mut state = seed;
        (0..len)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn every_level_gives_the_scalar_result() {
        // Shown with `--nocapture`: which levels this processor could test.
        eprintln!(
            "levels tried: {}",
            Level::all_available()
                .iter()
                .map(|level| level.name())
                .collect::<Vec<_>>()
                .join(", ")
        );
        for level in Level::all_available() {
            for len in [0, 1, 15, 16, 17, 31, 32, 33, 63, 64, 65, 255, 256] {
                let src = rows(len + 1, 7);
                for constant in 0..=u8::MAX {
                    let mut expected = rows(len + 1, 99);
                    let mut got = expected.clone();
                    for (e, &s) in expected.iter_mut().zip(&src) {
                        *e ^= mul(constant, s);
                    }
                    // Starting one byte in, so that the rows are not aligned.
                    xor_scaled_with(level, &mut got[1..], constant, &src[1..]);
                    assert_eq!(got[0], rows(len + 1, 99)[0]);
                    assert_eq!(
                        &got[1..],
                        &expected[1..],
                        "{level:?} length {len} by {constant}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_level_asked_for_never_exceeds_the_processor() {
        assert!(level().is_available());
        assert_eq!(Level::from_name(" AVX2 "), Some(Level::Avx2));
        assert_eq!(Level::from_name("nonsense"), None);
        assert_eq!(Level::all_available().first(), Some(&Level::Scalar));
    }
}
