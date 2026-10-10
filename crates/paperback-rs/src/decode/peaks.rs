// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider
// Derived from PaperBack 1.10, Copyright (c) 2007 Oleh Yuschuk; see NOTICE.md.

//! Locates the periodic dark lines of the grid in an intensity profile.

const MAX_PROFILE: usize = 1024;
const MAX_PEAKS: usize = 32;
const MIN_PROFILE: usize = 16;
/// Distances below this many pixels are too short to be grid steps.
const MIN_STEP: usize = 16;

/// Phase and step of a grid, with a quality estimate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GridFit {
    pub(crate) weight: f64,
    pub(crate) peak: f64,
    pub(crate) step: f64,
}

/// Given an intensity profile, finds the dark peaks and fits an arithmetic
/// sequence through them. Returns `None` when no grid is recognisable.
#[cfg_attr(feature = "profile", inline(never))]
pub(crate) fn find_peaks(profile: &[i32]) -> Option<GridFit> {
    let n = profile.len().min(MAX_PROFILE);
    if n < MIN_PROFILE {
        return None;
    }
    let h = &profile[..n];
    let depth = depth_below_envelope(h);
    let top = depth.iter().copied().max().unwrap_or(0);
    let limit = (top * 3 / 4).max(1);

    let peaks = collect_peaks(&depth, limit)?;
    let distance = most_common_distance(&peaks, n)?;
    fit_sequence(&peaks, distance)
}

struct Peak {
    position: f64,
    height: i32,
}

/// Removes slow gradients by shadowing the profile over about 32 pixels and
/// returns how far each point lies below the shadow.
fn depth_below_envelope(h: &[i32]) -> Vec<i32> {
    let n = h.len();
    let (min, max) = h
        .iter()
        .fold((i32::MAX, i32::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    let decay = (max - min + 16) / 32;
    let mut level = h[0];
    let mut envelope = vec![0i32; n];
    for i in 0..n {
        level = (level - decay).max(h[i]);
        envelope[i] = level;
    }
    let mut depth = vec![0i32; n];
    for i in (0..n).rev() {
        level = (level - decay).max(envelope[i]);
        depth[i] = level - h[i];
    }
    depth
}

fn collect_peaks(depth: &[i32], limit: i32) -> Option<Vec<Peak>> {
    let n = depth.len();
    let mut peaks: Vec<Peak> = Vec::new();
    let mut i = 0;
    while i < n && depth[i] > limit {
        i += 1;
    }
    while i < n && peaks.len() < MAX_PEAKS {
        while i < n && depth[i] <= limit {
            i += 1;
        }
        let (mut area, mut moment, mut height) = (0.0f64, 0.0f64, 0i32);
        while i < n && depth[i] > limit {
            let excess = f64::from(depth[i] - limit);
            area += excess;
            moment += excess * i as f64;
            height = height.max(depth[i]);
            i += 1;
        }
        if i >= n {
            break;
        }
        if let Some(previous) = peaks.last() {
            if height * 8 < previous.height {
                continue;
            }
            if height > previous.height * 8 {
                peaks.pop();
            }
        }
        peaks.push(Peak {
            position: moment / area,
            height,
        });
    }
    (peaks.len() >= 2).then_some(peaks)
}

/// Finds the spacing shared by most pairs of peaks, tolerating about 3%
/// dispersion.
fn most_common_distance(peaks: &[Peak], n: usize) -> Option<usize> {
    let mut histogram = vec![0usize; n];
    for (i, a) in peaks.iter().enumerate() {
        for b in &peaks[i + 1..] {
            histogram[(b.position - a.position) as usize] += 1;
        }
    }
    let (mut best_distance, mut best_count) = (0, 0);
    for i in MIN_STEP..n {
        if histogram[i] == 0 {
            continue;
        }
        let count: usize = (i..=(i + i / 33 + 1))
            .take_while(|&j| j < n)
            .map(|j| histogram[j])
            .sum();
        if count > best_count {
            best_distance = i;
            best_count = count;
        }
    }
    (best_distance != 0).then_some(best_distance)
}

/// Least-squares line through the peaks, numbering them as they are linked.
fn fit_sequence(peaks: &[Peak], distance: usize) -> Option<GridFit> {
    let (mut sn, mut sx, mut sy, mut sxx, mut sxy) = (0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64);
    let mut height_sum = 0.0f64;
    for (i, a) in peaks.iter().enumerate() {
        for b in &peaks[i + 1..] {
            let gap = (b.position - a.position) as usize;
            if gap < distance || gap > distance + distance / 33 {
                continue;
            }
            let k = if sn == 0.0 {
                0.0
            } else {
                let denominator = sx * sx - sn * sxx;
                let origin = (sx * sxy - sxx * sy) / denominator;
                let step = (sx * sy - sn * sxy) / denominator;
                ((a.position - origin + step / 2.0) / step).trunc()
            };
            sn += 2.0;
            sx += k * 2.0 + 1.0;
            sy += a.position + b.position;
            sxx += k * k + (k + 1.0) * (k + 1.0);
            sxy += a.position * k + b.position * (k + 1.0);
            height_sum += f64::from(a.height) + f64::from(b.height);
        }
    }
    if sn == 0.0 {
        return None;
    }
    let denominator = sx * sx - sn * sxx;
    let fit = GridFit {
        weight: height_sum / sn,
        peak: (sx * sxy - sxx * sy) / denominator,
        step: (sx * sy - sn * sxy) / denominator,
    };
    // A degenerate fit (all peaks equal) divides by zero; callers loop on the values.
    (fit.weight.is_finite() && fit.peak.is_finite() && fit.step.is_finite()).then_some(fit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comb(len: usize, period: usize, phase: usize) -> Vec<i32> {
        (0..len)
            .map(|i| {
                if (i + period - phase % period) % period < 3 {
                    40
                } else {
                    200
                }
            })
            .collect()
    }

    #[test]
    fn finds_period_and_phase_of_a_regular_comb() {
        let fit = find_peaks(&comb(600, 70, 10)).unwrap();
        assert!((fit.step - 70.0).abs() < 0.5, "step {}", fit.step);
        let phase = fit.peak.rem_euclid(70.0);
        assert!((phase - 11.0).abs() < 1.5, "peak {}", fit.peak);
    }

    #[test]
    fn flat_profiles_have_no_grid() {
        assert!(find_peaks(&[128; 300]).is_none());
    }

    #[test]
    fn short_profiles_have_no_grid() {
        assert!(find_peaks(&[1, 2, 3]).is_none());
    }
}
