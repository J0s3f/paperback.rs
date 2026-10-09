// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider
// Derived from PaperBack 1.10, Copyright (c) 2007 Oleh Yuschuk; see NOTICE.md.

//! Finds the dot grid of a scanned page: position, step, tilt and brightness.

use super::bitmap::Bitmap;
use super::peaks::{GridFit, find_peaks};
use crate::block::BLOCK_DOTS;
use crate::error::{Error, Result};

const PROFILE_POINTS: usize = 1024;
const MAX_SAMPLE_LINES: usize = 256;
/// Bitmaps this small cannot hold a block.
const MIN_BITMAP_SIDE: usize = 3 * BLOCK_DOTS;
/// Tilt search range: about +/-5 degrees expressed in 1/`PROFILE_POINTS` units.
const MAX_TILT: i32 = (PROFILE_POINTS as i32 / 20) * 2;
const TILT_STEP: usize = 2;
const MIN_BLOCK_STEP: f64 = BLOCK_DOTS as f64;
const MAX_STEP_RATIO: f64 = 2.5;
const MIN_STEP_RATIO: f64 = 0.4;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Intensity {
    pub(crate) min: i32,
    pub(crate) max: i32,
    pub(crate) sharpness: f64,
}

/// Where the block grid lies in the bitmap.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Grid {
    pub(crate) x_peak: f64,
    pub(crate) x_step: f64,
    pub(crate) x_angle: f64,
    pub(crate) y_peak: f64,
    pub(crate) y_step: f64,
    pub(crate) y_angle: f64,
}

#[derive(Clone, Copy, Debug)]
struct Extent {
    min: usize,
    max: usize,
}

#[derive(Clone, Copy, Debug)]
struct SearchWindow {
    x0: usize,
    x1: usize,
    y0: usize,
    y1: usize,
}

impl SearchWindow {
    fn width(&self) -> usize {
        self.x1 - self.x0
    }

    fn height(&self) -> usize {
        self.y1 - self.y0
    }
}

pub(crate) fn locate(bitmap: &Bitmap) -> Result<(Grid, Intensity)> {
    let (x_extent, y_extent) = rough_extent(bitmap)?;
    let window = search_window(bitmap, x_extent, y_extent);
    let intensity = measure_intensity(bitmap, &window)?;
    let (x_peak, x_step, x_angle) = fit_vertical_lines(bitmap, &window)?;
    let (y_peak, y_step, y_angle) = fit_horizontal_lines(bitmap, &window, x_step)?;
    Ok((
        Grid {
            x_peak,
            x_step,
            x_angle,
            y_peak,
            y_step,
            y_angle,
        },
        intensity,
    ))
}

/// Rough limits of the area with quickly changing intensity, i.e. the raster.
/// Constant black or white borders are ignored by looking at 2-pixel changes.
fn rough_extent(bitmap: &Bitmap) -> Result<(Extent, Extent)> {
    let (sx, sy) = (bitmap.width(), bitmap.height());
    if sx <= MIN_BITMAP_SIDE || sy <= MIN_BITMAP_SIDE {
        return Err(Error::Decode("bitmap is too small to process".into()));
    }
    let step_x = sx / MAX_SAMPLE_LINES + 1;
    let nx = ((sx - 2) / step_x).min(MAX_SAMPLE_LINES);
    let step_y = sy / MAX_SAMPLE_LINES + 1;
    let ny = ((sy - 2) / step_y).min(MAX_SAMPLE_LINES);

    let mut activity_x = vec![0i64; nx];
    let mut activity_y = vec![0i64; ny];
    for j in 0..ny {
        for i in 0..nx {
            let (x, y) = (i * step_x, j * step_y);
            let samples = [
                bitmap.at(x, y),
                bitmap.at(x + 2, y),
                bitmap.at(x + 1, y + 1),
                bitmap.at(x, y + 2),
                bitmap.at(x + 2, y + 2),
            ];
            let (lowest, highest) = samples
                .iter()
                .fold((u8::MAX, u8::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
            let change = i64::from(highest) - i64::from(lowest);
            activity_x[i] += change;
            activity_y[j] += change;
        }
    }
    Ok((
        Extent {
            min: first_active(&activity_x) * step_x,
            max: last_active(&activity_x) * step_x,
        },
        Extent {
            min: first_active(&activity_y) * step_y,
            max: last_active(&activity_y) * step_y,
        },
    ))
}

fn half_of_peak(activity: &[i64]) -> i64 {
    activity.iter().copied().max().unwrap_or(0) / 2
}

fn first_active(activity: &[i64]) -> usize {
    let limit = half_of_peak(activity);
    (0..activity.len().saturating_sub(1))
        .find(|&i| activity[i] >= limit)
        .unwrap_or(activity.len().saturating_sub(1))
}

fn last_active(activity: &[i64]) -> usize {
    let limit = half_of_peak(activity);
    (1..activity.len())
        .rev()
        .find(|&i| activity[i] >= limit)
        .unwrap_or(0)
}

/// Window around the centre of the raster used for all profile measurements.
fn search_window(bitmap: &Bitmap, x: Extent, y: Extent) -> SearchWindow {
    let center_x = x.min.midpoint(x.max);
    let center_y = y.min.midpoint(y.max);
    let x0 = center_x.saturating_sub(PROFILE_POINTS / 2);
    let y0 = center_y.saturating_sub(PROFILE_POINTS / 2);
    SearchWindow {
        x0,
        x1: (x0 + PROFILE_POINTS).min(bitmap.width()),
        y0,
        y1: (y0 + PROFILE_POINTS).min(bitmap.height()),
    }
}

/// Brightness range (ignoring the 3% extremes) and a sharpness estimate.
fn measure_intensity(bitmap: &Bitmap, w: &SearchWindow) -> Result<Intensity> {
    let mut levels = [0usize; 256];
    let mut differences = [0usize; 256];
    let mut count = 0usize;
    for j in 0..w.height().saturating_sub(1) {
        for i in 0..w.width().saturating_sub(1) {
            let (x, y) = (w.x0 + i, w.y0 + j);
            let here = bitmap.at(x, y);
            levels[here as usize] += 1;
            count += 1;
            differences[here.abs_diff(bitmap.at(x + 1, y)) as usize] += 1;
            differences[here.abs_diff(bitmap.at(x, y + 1)) as usize] += 1;
        }
    }
    if count == 0 {
        return Err(Error::Decode("no image".into()));
    }
    let tail = count / 33;
    let mut accumulated = 0;
    let mut min = 0;
    while min < 255 {
        accumulated += levels[min];
        if accumulated >= tail {
            break;
        }
        min += 1;
    }
    accumulated = 0;
    let mut max = 255;
    while max > 0 {
        accumulated += levels[max];
        if accumulated >= tail {
            break;
        }
        max -= 1;
    }
    if max <= min {
        return Err(Error::Decode("no image".into()));
    }
    let contrast_tail = count / 10;
    accumulated = 0;
    let mut contrast = 255;
    while contrast > 1 {
        accumulated += differences[contrast];
        if accumulated >= contrast_tail {
            break;
        }
        contrast -= 1;
    }
    Ok(Intensity {
        min: min as i32,
        max: max as i32,
        sharpness: (max - min) as f64 / (2.0 * contrast as f64) - 1.0,
    })
}

fn tilt_candidates() -> impl Iterator<Item = i32> {
    (-MAX_TILT..=MAX_TILT).step_by(TILT_STEP)
}

/// Slight preference for no tilt, which matters on small synthetic bitmaps.
fn tilt_preference(tilt: i32) -> f64 {
    1.0 / (f64::from(tilt.abs()) + 10.0)
}

/// Finds step, phase and tilt of the vertical grid lines.
fn fit_vertical_lines(bitmap: &Bitmap, w: &SearchWindow) -> Result<(f64, f64, f64)> {
    let line_step = (w.height() / MAX_SAMPLE_LINES).max(1);
    let mut best = Selection::default();
    let mut last = GridFit {
        weight: 0.0,
        peak: 0.0,
        step: 0.0,
    };
    for tilt in tilt_candidates() {
        let mut sum = vec![0i64; w.width()];
        let mut seen = vec![0i64; w.width()];
        for j in (0..w.height()).step_by(line_step) {
            let y = w.y0 + j;
            let shift = (y as i32 * tilt / PROFILE_POINTS as i32) as isize;
            for i in 0..w.width() {
                let x = (w.x0 + i) as isize + shift;
                if x < 0 {
                    continue;
                }
                if x as usize >= bitmap.width() {
                    break;
                }
                sum[i] += i64::from(bitmap.at(x as usize, y));
                seen[i] += 1;
            }
        }
        let profile = normalised(&sum, &seen);
        let fit = find_peaks(&profile);
        if let Some(fit) = fit {
            last = fit;
        }
        let weight = fit.map_or(0.0, |fit| fit.weight) + tilt_preference(tilt);
        best.offer(
            weight,
            last.peak + w.x0 as f64,
            f64::from(tilt) / PROFILE_POINTS as f64,
            last.step,
        );
    }
    if best.weight == 0.0 || best.step < MIN_BLOCK_STEP {
        return Err(Error::Decode("no grid".into()));
    }
    Ok((best.peak, best.step, best.angle))
}

/// Finds step, phase and tilt of the horizontal grid lines.
fn fit_horizontal_lines(bitmap: &Bitmap, w: &SearchWindow, x_step: f64) -> Result<(f64, f64, f64)> {
    let line_step = (w.width() / MAX_SAMPLE_LINES).max(1);
    let mut best = Selection::default();
    let mut last = GridFit {
        weight: 0.0,
        peak: 0.0,
        step: 0.0,
    };
    for tilt in tilt_candidates() {
        let mut sum = vec![0i64; w.height()];
        let mut seen = vec![0i64; w.height()];
        for i in (0..w.width()).step_by(line_step) {
            let x = w.x0 + i;
            let shift = (x as i32 * tilt / PROFILE_POINTS as i32) as isize;
            for j in 0..w.height() {
                let y = (w.y0 + j) as isize + shift;
                if y < 0 {
                    continue;
                }
                if y as usize >= bitmap.height() {
                    break;
                }
                sum[j] += i64::from(bitmap.at(x, y as usize));
                seen[j] += 1;
            }
        }
        let profile = normalised(&sum, &seen);
        let fit = find_peaks(&profile);
        if let Some(fit) = fit {
            last = fit;
        }
        let weight = fit.map_or(0.0, |fit| fit.weight) + tilt_preference(tilt);
        best.offer(
            weight,
            last.peak + w.y0 as f64,
            f64::from(tilt) / PROFILE_POINTS as f64,
            last.step,
        );
    }
    if best.weight == 0.0
        || best.step < MIN_BLOCK_STEP
        || best.step < x_step * MIN_STEP_RATIO
        || best.step > x_step * MAX_STEP_RATIO
    {
        return Err(Error::Decode("no grid".into()));
    }
    Ok((best.peak, best.step, best.angle))
}

fn normalised(sum: &[i64], seen: &[i64]) -> Vec<i32> {
    sum.iter()
        .zip(seen)
        .map(|(&s, &n)| if n > 0 { (s / n) as i32 } else { 0 })
        .collect()
}

#[derive(Default)]
struct Selection {
    weight: f64,
    peak: f64,
    angle: f64,
    step: f64,
}

impl Selection {
    fn offer(&mut self, weight: f64, peak: f64, angle: f64, step: f64) {
        if weight > self.weight {
            *self = Selection {
                weight,
                peak,
                angle,
                step,
            };
        }
    }
}
