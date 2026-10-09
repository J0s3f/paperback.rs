// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

//! Likely reasons why a scan reads badly: too few pixels per dot, or a scanner that
//! "enhanced" the picture. Only a diagnosis for people; the reader never uses it.

use crate::raster::Raster;

/// Below this many pixels per dot the dots blur into each other; the usual advice is three.
const MIN_PIXELS_PER_DOT: f64 = 2.5;
/// Share of the pixels that must be pure black or white for the page to count as binarised
/// ("text" or "line art" mode of a scanner).
const BINARISED_SHARE: f64 = 0.97;
/// Gray values this close to black or white count as pure ones.
const PURE_MARGIN: u8 = 8;
/// Share of black pixels above which the contrast was stretched (auto-levelling); the dots of
/// an untouched scan stay gray.
const CLIPPED_BLACK_SHARE: f64 = 0.12;
const BLACK_LIMIT: u8 = 3;
/// How far the brightest pixels may exceed the blank paper at the page edge before the
/// picture counts as sharpened (sharpening draws bright halos around dark dots).
const HALO_OVERSHOOT: i32 = 10;
/// Width of the page edge, as a share of the picture, taken as blank paper.
const EDGE_SHARE: f64 = 0.02;
/// Percentile of the brightest pixels compared with the paper.
const BRIGHT_PERCENTILE: f64 = 0.995;

/// What looks wrong with a scan. All fields are false or zero for a clean scan.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScanHints {
    /// Pixels per dot of the scan as it was given, times 100; 0 if not measured.
    pub pixels_per_dot_percent: u32,
    /// The picture has only black and white pixels, so dot sizes carry no gray information.
    pub binarised: bool,
    /// Many pixels are pure black: the scanner stretched the contrast.
    pub contrast_stretched: bool,
    /// Bright halos around dark dots: the scanner sharpened the picture.
    pub sharpened: bool,
}

impl ScanHints {
    /// Whether the scan has fewer pixels per dot than reading needs.
    pub fn low_resolution(&self) -> bool {
        self.pixels_per_dot_percent > 0
            && f64::from(self.pixels_per_dot_percent) < MIN_PIXELS_PER_DOT * 100.0
    }

    /// Adds what another page showed: the lowest resolution and every problem found.
    pub fn merge(&mut self, other: &ScanHints) {
        self.pixels_per_dot_percent =
            match (self.pixels_per_dot_percent, other.pixels_per_dot_percent) {
                (0, value) | (value, 0) => value,
                (a, b) => a.min(b),
            };
        self.binarised |= other.binarised;
        self.contrast_stretched |= other.contrast_stretched;
        self.sharpened |= other.sharpened;
    }

    /// Whether any hint applies.
    pub fn any(&self) -> bool {
        self.low_resolution() || self.binarised || self.contrast_stretched || self.sharpened
    }

    /// Short machine-readable names of the hints that apply.
    pub fn names(&self) -> Vec<&'static str> {
        [
            (self.low_resolution(), "low_resolution"),
            (self.binarised, "binarised"),
            (self.contrast_stretched, "contrast_stretched"),
            (self.sharpened, "sharpened"),
        ]
        .into_iter()
        .filter_map(|(applies, name)| applies.then_some(name))
        .collect()
    }

    /// The hints as advice for a person, one sentence each.
    pub fn advice(&self) -> Vec<String> {
        let mut advice = Vec::new();
        if self.low_resolution() {
            advice.push(format!(
                "only {:.1} pixels per dot; scan at three or more (see the page's hint line)",
                f64::from(self.pixels_per_dot_percent) / 100.0
            ));
        }
        if self.binarised {
            advice.push("the scan is pure black and white; scan in grayscale".to_string());
        }
        if self.contrast_stretched {
            advice.push(
                "the contrast looks stretched; turn off auto-levelling or brightness correction"
                    .to_string(),
            );
        }
        if self.sharpened {
            advice.push(
                "bright halos around the dots; turn off the scanner's or camera's sharpening"
                    .to_string(),
            );
        }
        advice
    }
}

/// Looks at a picture of a page whose dots are `pixels_per_dot` pixels apart.
pub(crate) fn assess(raster: &Raster, pixels_per_dot: f64) -> ScanHints {
    let pixels = raster.pixels();
    if pixels.is_empty() {
        return ScanHints::default();
    }
    let total = pixels.len() as f64;
    let share =
        |keep: &dyn Fn(u8) -> bool| pixels.iter().filter(|&&p| keep(p)).count() as f64 / total;
    let binarised = share(&|p| p <= PURE_MARGIN || p >= u8::MAX - PURE_MARGIN) >= BINARISED_SHARE;
    ScanHints {
        pixels_per_dot_percent: (pixels_per_dot * 100.0).round() as u32,
        binarised,
        contrast_stretched: !binarised && share(&|p| p <= BLACK_LIMIT) >= CLIPPED_BLACK_SHARE,
        sharpened: !binarised && has_halos(raster),
    }
}

fn has_halos(raster: &Raster) -> bool {
    let (width, height) = (raster.width(), raster.height());
    let edge = ((width.min(height) as f64) * EDGE_SHARE) as usize;
    if edge == 0 || width <= 2 * edge || height <= 2 * edge {
        return false;
    }
    let mut paper: Vec<u8> = Vec::new();
    for y in 0..height {
        let row = raster.row(y);
        if y < edge || y >= height - edge {
            paper.extend_from_slice(row);
        } else {
            paper.extend_from_slice(&row[..edge]);
            paper.extend_from_slice(&row[width - edge..]);
        }
    }
    paper.sort_unstable();
    let blank = i32::from(paper[paper.len() / 2]);
    let mut all = raster.pixels().to_vec();
    let at = ((all.len() - 1) as f64 * BRIGHT_PERCENTILE) as usize;
    let brightest = i32::from(*all.select_nth_unstable(at).1);
    brightest - blank > HALO_OVERSHOOT
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raster_of(width: usize, height: usize, pixel: impl Fn(usize, usize) -> u8) -> Raster {
        let pixels = (0..height)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .map(|(x, y)| pixel(x, y))
            .collect();
        Raster::from_pixels(width, height, pixels).unwrap()
    }

    /// Blank paper at 235 with dots of 90 to 140 in the middle: an untouched scan.
    fn plain_scan() -> Raster {
        raster_of(200, 200, |x, y| {
            if (40..160).contains(&x) && (40..160).contains(&y) {
                90 + ((x * 7 + y * 13) % 50) as u8
            } else {
                235
            }
        })
    }

    #[test]
    fn an_untouched_scan_has_no_hints() {
        let hints = assess(&plain_scan(), 3.5);
        assert!(!hints.any(), "{hints:?}");
        assert_eq!(hints.advice(), Vec::<String>::new());
    }

    #[test]
    fn few_pixels_per_dot_are_reported() {
        let hints = assess(&plain_scan(), 1.8);
        assert!(hints.low_resolution());
        assert_eq!(hints.names(), ["low_resolution"]);
        assert!(hints.advice()[0].contains("1.8"));
    }

    #[test]
    fn a_black_and_white_picture_is_binarised() {
        let page = raster_of(200, 200, |x, y| if (x + y) % 3 == 0 { 0 } else { 255 });
        let hints = assess(&page, 3.0);
        assert!(hints.binarised);
        assert!(!hints.contrast_stretched && !hints.sharpened);
    }

    #[test]
    fn many_pure_black_pixels_mean_stretched_contrast() {
        let page = raster_of(200, 200, |x, y| {
            if (40..160).contains(&x) && (40..160).contains(&y) {
                if (x + y) % 2 == 0 { 0 } else { 120 }
            } else {
                235
            }
        });
        assert!(assess(&page, 3.0).contrast_stretched);
    }

    #[test]
    fn bright_halos_above_the_paper_mean_sharpening() {
        let page = raster_of(200, 200, |x, y| {
            if (40..160).contains(&x) && (40..160).contains(&y) {
                if (x + y) % 4 == 0 { 90 } else { 250 }
            } else {
                235
            }
        });
        assert!(assess(&page, 3.0).sharpened);
    }
}
