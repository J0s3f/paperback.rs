// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

//! A dot detector fitted to the page it is used on.
//!
//! Printers make dots bigger or smaller than meant and blur them into their neighbours, and
//! cameras add their own blur, so what a dot looks like depends on the dots around it and on
//! the page. Every block that is read comes with the truth: after error correction we know
//! which dots were black. This module learns from those blocks how the gray values around a
//! dot predict it, and gives the blocks that could not be read a second look with that
//! knowledge. (Equalizers of this kind are standard in page-oriented optical storage, where
//! neighbouring pixels interfere in the same way.)

use crate::block::BLOCK_DOTS;

type Dots = [[u8; BLOCK_DOTS]; BLOCK_DOTS];

/// Side of the square of dots around a dot that is looked at.
const WINDOW: usize = 5;
const REACH: isize = (WINDOW / 2) as isize;
/// The window, standardised, and a constant.
const FEATURES: usize = WINDOW * WINDOW + 1;
/// Blocks needed before the first fit; fewer give a model that is worse than a threshold.
const MIN_BLOCKS: usize = 3;
/// After this many blocks nothing more is learned: more blocks no longer change the model,
/// and each one costs a pass over its dots.
const MAX_BLOCKS: usize = 200;
/// Regularisation, as a share of the number of samples.
const RIDGE: f64 = 1e-4;

/// How a dot looks, learned from blocks whose dots are known.
#[derive(Clone, Debug)]
pub(crate) struct DotDetector {
    normal: Vec<[f64; FEATURES]>,
    target: [f64; FEATURES],
    blocks: usize,
    next_fit: usize,
    weights: Option<[f64; FEATURES]>,
}

impl DotDetector {
    pub(crate) fn new() -> Self {
        Self {
            normal: vec![[0.0; FEATURES]; FEATURES],
            target: [0.0; FEATURES],
            blocks: 0,
            next_fit: MIN_BLOCKS,
            weights: None,
        }
    }

    /// Whether enough blocks were seen to judge dots.
    pub(crate) fn is_ready(&self) -> bool {
        self.weights.is_some()
    }

    /// Learns from a block read as `gray` whose dots were `black` (indexed like `gray`).
    pub(crate) fn learn(&mut self, gray: &Dots, black: &[[bool; BLOCK_DOTS]; BLOCK_DOTS]) {
        if self.blocks >= MAX_BLOCKS {
            return;
        }
        let standard = standardised(gray);
        for j in 0..BLOCK_DOTS {
            for i in 0..BLOCK_DOTS {
                let features = features_at(&standard, j, i);
                let wanted = if black[j][i] { 1.0 } else { -1.0 };
                for (row, &a) in self.normal.iter_mut().zip(&features) {
                    for (cell, &b) in row.iter_mut().zip(&features) {
                        *cell += a * b;
                    }
                }
                for (sum, &feature) in self.target.iter_mut().zip(&features) {
                    *sum += feature * wanted;
                }
            }
        }
        self.blocks += 1;
        if self.blocks >= self.next_fit {
            self.fit();
            // Refit when the amount of data has doubled.
            self.next_fit = self.blocks * 2;
        }
    }

    /// For every dot a number that is positive where the dot is believed black, and the larger
    /// in size the more sure; all zero until the detector is ready.
    pub(crate) fn scores(&self, gray: &Dots) -> [[f64; BLOCK_DOTS]; BLOCK_DOTS] {
        let mut scores = [[0.0; BLOCK_DOTS]; BLOCK_DOTS];
        let Some(weights) = &self.weights else {
            return scores;
        };
        let standard = standardised(gray);
        for (j, row) in scores.iter_mut().enumerate() {
            for (i, score) in row.iter_mut().enumerate() {
                let features = features_at(&standard, j, i);
                *score = features.iter().zip(weights).map(|(f, w)| f * w).sum();
            }
        }
        scores
    }

    fn fit(&mut self) {
        let samples = (self.blocks * BLOCK_DOTS * BLOCK_DOTS) as f64;
        let mut matrix: Vec<[f64; FEATURES + 1]> = self
            .normal
            .iter()
            .zip(&self.target)
            .enumerate()
            .map(|(at, (row, &target))| {
                let mut extended = [0.0; FEATURES + 1];
                extended[..FEATURES].copy_from_slice(row);
                extended[at] += RIDGE * samples;
                extended[FEATURES] = target;
                extended
            })
            .collect();
        self.weights = solve(&mut matrix);
    }
}

/// The gray values as distances from the block's mean in units of its spread, so that a
/// block photographed in shadow looks like one in the light.
fn standardised(gray: &Dots) -> [[f64; BLOCK_DOTS]; BLOCK_DOTS] {
    let count = (BLOCK_DOTS * BLOCK_DOTS) as f64;
    let mean = gray.iter().flatten().map(|&v| f64::from(v)).sum::<f64>() / count;
    let variance = gray
        .iter()
        .flatten()
        .map(|&v| (f64::from(v) - mean).powi(2))
        .sum::<f64>()
        / count;
    let spread = variance.sqrt().max(1.0);
    let mut standard = [[0.0; BLOCK_DOTS]; BLOCK_DOTS];
    for (row, source) in standard.iter_mut().zip(gray) {
        for (value, &v) in row.iter_mut().zip(source) {
            *value = (f64::from(v) - mean) / spread;
        }
    }
    standard
}

/// The window around dot (`j`, `i`); outside the block the edge value is repeated.
fn features_at(standard: &[[f64; BLOCK_DOTS]; BLOCK_DOTS], j: usize, i: usize) -> [f64; FEATURES] {
    let mut features = [0.0; FEATURES];
    let last = BLOCK_DOTS as isize - 1;
    let mut at = 0;
    for dj in -REACH..=REACH {
        for di in -REACH..=REACH {
            let (y, x) = (
                (j as isize + dj).clamp(0, last) as usize,
                (i as isize + di).clamp(0, last) as usize,
            );
            features[at] = standard[y][x];
            at += 1;
        }
    }
    features[at] = 1.0;
    features
}

/// Solves the linear system in the augmented matrix by Gauss elimination; `None` if it is
/// singular.
fn solve(matrix: &mut [[f64; FEATURES + 1]]) -> Option<[f64; FEATURES]> {
    for column in 0..FEATURES {
        let pivot = (column..FEATURES)
            .max_by(|&a, &b| matrix[a][column].abs().total_cmp(&matrix[b][column].abs()))?;
        if matrix[pivot][column].abs() < 1e-12 {
            return None;
        }
        matrix.swap(column, pivot);
        for row in column + 1..FEATURES {
            let factor = matrix[row][column] / matrix[column][column];
            for k in column..=FEATURES {
                matrix[row][k] -= factor * matrix[column][k];
            }
        }
    }
    let mut solution = [0.0; FEATURES];
    for row in (0..FEATURES).rev() {
        let known: f64 = (row + 1..FEATURES)
            .map(|k| matrix[row][k] * solution[k])
            .sum();
        solution[row] = (matrix[row][FEATURES] - known) / matrix[row][row];
    }
    Some(solution)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A block of random dots as a blurred, noisy picture: each dot is spread over its
    /// neighbours, which a threshold cannot undo.
    fn blurred_block(seed: u32) -> (Dots, [[bool; BLOCK_DOTS]; BLOCK_DOTS]) {
        let mut state = seed;
        let mut next = move || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            f64::from(state >> 8) / f64::from(1u32 << 24)
        };
        let mut black = [[false; BLOCK_DOTS]; BLOCK_DOTS];
        for row in &mut black {
            for dot in row.iter_mut() {
                *dot = next() < 0.5;
            }
        }
        let mut gray = [[0u8; BLOCK_DOTS]; BLOCK_DOTS];
        for j in 0..BLOCK_DOTS {
            for i in 0..BLOCK_DOTS {
                let mut sum = 0.0;
                let mut weight = 0.0;
                for (dj, di, w) in [
                    (0isize, 0isize, 1.0),
                    (-1, 0, 0.45),
                    (1, 0, 0.45),
                    (0, -1, 0.75),
                    (0, 1, 0.75),
                ] {
                    let y = (j as isize + dj).clamp(0, 31) as usize;
                    let x = (i as isize + di).clamp(0, 31) as usize;
                    sum += w * if black[y][x] { 70.0 } else { 220.0 };
                    weight += w;
                }
                gray[j][i] = (sum / weight + (next() - 0.5) * 30.0).clamp(0.0, 255.0) as u8;
            }
        }
        (gray, black)
    }

    fn errors(
        detector: &DotDetector,
        gray: &Dots,
        black: &[[bool; BLOCK_DOTS]; BLOCK_DOTS],
    ) -> usize {
        let scores = detector.scores(gray);
        (0..BLOCK_DOTS)
            .flat_map(|j| (0..BLOCK_DOTS).map(move |i| (j, i)))
            .filter(|&(j, i)| (scores[j][i] > 0.0) != black[j][i])
            .count()
    }

    fn threshold_errors(gray: &Dots, black: &[[bool; BLOCK_DOTS]; BLOCK_DOTS]) -> usize {
        let mean = gray.iter().flatten().map(|&v| f64::from(v)).sum::<f64>() / 1024.0;
        (0..BLOCK_DOTS)
            .flat_map(|j| (0..BLOCK_DOTS).map(move |i| (j, i)))
            .filter(|&(j, i)| (f64::from(gray[j][i]) < mean) != black[j][i])
            .count()
    }

    #[test]
    fn it_knows_nothing_before_it_has_seen_blocks() {
        let detector = DotDetector::new();
        assert!(!detector.is_ready());
        assert!(detector.scores(&[[100; BLOCK_DOTS]; BLOCK_DOTS])[3][3].abs() < f64::EPSILON);
    }

    #[test]
    fn after_a_few_blocks_it_beats_a_plain_threshold_on_blurred_dots() {
        let mut detector = DotDetector::new();
        for seed in 1..=MIN_BLOCKS as u32 + 1 {
            let (gray, black) = blurred_block(seed);
            detector.learn(&gray, &black);
        }
        assert!(detector.is_ready());
        let (mut learned, mut plain) = (0, 0);
        for seed in 100..110 {
            let (gray, black) = blurred_block(seed);
            learned += errors(&detector, &gray, &black);
            plain += threshold_errors(&gray, &black);
        }
        assert!(
            learned * 2 < plain,
            "learned {learned} wrong dots against {plain} with a threshold"
        );
    }

    #[test]
    fn a_dark_block_is_judged_like_a_bright_one() {
        let mut detector = DotDetector::new();
        for seed in 1..=4 {
            let (gray, black) = blurred_block(seed);
            detector.learn(&gray, &black);
        }
        let (gray, black) = blurred_block(50);
        let shaded = gray.map(|row| row.map(|v| v / 2 + 10));
        let (bright, dark) = (
            errors(&detector, &gray, &black),
            errors(&detector, &shaded, &black),
        );
        assert!(dark <= bright + 20, "{dark} against {bright}");
    }
}
