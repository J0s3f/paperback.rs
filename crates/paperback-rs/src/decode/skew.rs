// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

//! Estimates how far a page is turned from the upright position.
//!
//! The page carries long, straight grid lines. Projecting the dark pixels onto
//! the vertical axis gives sharp peaks exactly when the lines are horizontal,
//! so the turn with the sharpest projection is the one that straightens the page.

use crate::raster::Raster;

/// Largest turn considered; the grid looks the same after a quarter turn, and
/// the block reader recognises quarter turns by itself.
const MAX_TURN_DEGREES: f64 = 45.0;
const COARSE_STEP_DEGREES: f64 = 1.0;
const FINE_STEP_DEGREES: f64 = 0.1;
/// Side of the central square that is examined.
const WINDOW: usize = 1600;
/// Sampling stride inside the window.
const STRIDE: usize = 2;
/// Turns below this are not worth a resampling pass.
pub(crate) const NEGLIGIBLE_DEGREES: f64 = 0.15;

/// The angle in degrees to pass to `Raster::rotated` so that the grid lines
/// become horizontal and vertical.
pub(crate) fn estimate(raster: &Raster) -> f64 {
    let samples = sample_dark_points(raster);
    if samples.is_empty() {
        return 0.0;
    }
    let coarse = best_angle(
        &samples,
        steps(-MAX_TURN_DEGREES, MAX_TURN_DEGREES, COARSE_STEP_DEGREES),
    );
    best_angle(
        &samples,
        steps(
            coarse - COARSE_STEP_DEGREES,
            coarse + COARSE_STEP_DEGREES,
            FINE_STEP_DEGREES,
        ),
    )
}

/// Darkness of the sampled pixels relative to the window centre, as (x, y, darkness).
fn sample_dark_points(raster: &Raster) -> Vec<(f32, f32, f32)> {
    let side_x = WINDOW.min(raster.width());
    let side_y = WINDOW.min(raster.height());
    let left = (raster.width() - side_x) / 2;
    let top = (raster.height() - side_y) / 2;
    let mean = mean_level(raster, left, top, side_x, side_y);
    // A disc, not a square: a square window always projects sharpest when upright.
    let radius = side_x.min(side_y) as f32 / 2.0;
    let mut samples = Vec::with_capacity(side_x * side_y / (STRIDE * STRIDE));
    for y in (top..top + side_y).step_by(STRIDE) {
        let row = raster.row(y);
        for x in (left..left + side_x).step_by(STRIDE) {
            let darkness = mean - f32::from(row[x]);
            let cx = x as f32 - (left + side_x / 2) as f32;
            let cy = y as f32 - (top + side_y / 2) as f32;
            if darkness > 0.0 && cx * cx + cy * cy <= radius * radius {
                samples.push((cx, cy, darkness));
            }
        }
    }
    samples
}

fn mean_level(raster: &Raster, left: usize, top: usize, width: usize, height: usize) -> f32 {
    let mut sum = 0u64;
    let mut count = 0u64;
    for y in (top..top + height).step_by(STRIDE) {
        for &value in raster.row(y)[left..left + width].iter().step_by(STRIDE) {
            sum += u64::from(value);
            count += 1;
        }
    }
    if count == 0 {
        0.0
    } else {
        sum as f32 / count as f32
    }
}

fn steps(from: f64, to: f64, step: f64) -> impl Iterator<Item = f64> {
    let count = ((to - from) / step).round() as usize;
    (0..=count).map(move |i| from + step * i as f64)
}

fn best_angle(samples: &[(f32, f32, f32)], candidates: impl Iterator<Item = f64>) -> f64 {
    let mut best = (f64::MIN, 0.0);
    for angle in candidates {
        let score = projection_sharpness(samples, angle);
        if score > best.0 {
            best = (score, angle);
        }
    }
    best.1
}

/// Sum of squared row sums after turning by `degrees` (same convention as
/// `Raster::rotated`: a pixel at offset (x, y) lands on row `y cos - x sin`).
fn projection_sharpness(samples: &[(f32, f32, f32)], degrees: f64) -> f64 {
    let (sin, cos) = degrees.to_radians().sin_cos();
    let (sin, cos) = (sin as f32, cos as f32);
    let extent = (WINDOW as f32 * 1.5) as isize;
    let mut bins = vec![0f32; (2 * extent + 1) as usize];
    // Bins as wide as the sampling stride; narrower bins would only count the lattice parity.
    let bin_width = STRIDE as f32;
    for &(x, y, darkness) in samples {
        let row = ((y * cos - x * sin) / bin_width).floor() as isize + extent;
        if let Some(bin) = bins.get_mut(row as usize) {
            *bin += darkness;
        }
    }
    bins.iter().map(|&v| f64::from(v) * f64::from(v)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn striped(width: usize, height: usize) -> Raster {
        let mut raster = Raster::filled(width, height, 255);
        for y in (0..height).step_by(40) {
            raster.fill_rect(0, y, width, 3, 0);
        }
        for x in (0..width).step_by(40) {
            raster.fill_rect(x, 0, 3, height, 0);
        }
        raster
    }

    #[test]
    fn upright_grid_needs_no_turn() {
        assert!(estimate(&striped(800, 800)).abs() < NEGLIGIBLE_DEGREES);
    }

    #[test]
    fn estimated_turn_straightens_a_tilted_grid() {
        let grid = striped(1000, 1000);
        for tilt in [-30.0, -7.0, 4.0, 12.0, 38.0] {
            let tilted = grid.rotated(tilt, 255).unwrap();
            let correction = estimate(&tilted);
            assert!(
                (correction + tilt).abs() < 0.5,
                "tilt {tilt}: correction {correction}"
            );
        }
    }

    #[test]
    fn a_rendered_page_is_found_tilted_too() {
        use crate::encode::{EncodeOptions, encode};
        use crate::layout::PageSetup;
        let options = EncodeOptions {
            setup: PageSetup {
                printer_dpi: 300,
                dot_dpi: 100,
                ..PageSetup::default()
            },
            compression: crate::codec::Compression::None,
            ..EncodeOptions::default()
        };
        let page = encode(&vec![7u8; 30_000], &options)
            .unwrap()
            .remove(0)
            .raster;
        for tilt in [-37.0, -12.0, 0.0, 5.0, 10.0, 20.0, 30.0] {
            let tilted = page.rotated(tilt, 255).unwrap();
            let correction = estimate(&tilted);
            assert!(
                (correction + tilt).abs() < 0.5,
                "tilt {tilt}: correction {correction}"
            );
        }
    }
}
