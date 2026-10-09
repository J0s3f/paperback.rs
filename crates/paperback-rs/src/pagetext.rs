// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

//! Title above and hint below the dot grid, drawn with a small bitmap font.
//!
//! The original prints the same two lines on paper. Here they also say how to
//! restore the data with this tool.

use font8x8::LATIN_FONTS;
use font8x8::UnicodeFonts;
use font8x8::legacy::BASIC_LEGACY;

use crate::block::FileTime;
use crate::raster::Raster;

const GLYPH_SIZE: usize = 8;
const TEXT_LEVEL: u8 = 128;
const UNKNOWN_GLYPH: char = '?';
/// Scanning at three times the dot density resolves the dots reliably.
const SCAN_OVERSAMPLING: usize = 3;
pub(crate) const PROJECT_URL: &str = "paperback.j0s.at";

const SECONDS_PER_DAY: u64 = 86_400;

/// Sizes of the text lines for a printer resolution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TextStyle {
    header_scale: usize,
    footer_scale: usize,
    dpi: usize,
}

impl TextStyle {
    pub(crate) fn for_dpi(dpi: usize) -> Self {
        Self {
            header_scale: fitting_scale(dpi / 6),
            footer_scale: fitting_scale(dpi / 10),
            dpi,
        }
    }

    /// Height of the band above the grid, pixels.
    pub(crate) fn top_band(&self) -> usize {
        self.header_scale * GLYPH_SIZE + self.dpi / 16
    }

    /// Height of the band below the grid, pixels.
    pub(crate) fn bottom_band(&self) -> usize {
        self.footer_scale * GLYPH_SIZE + self.dpi / 24
    }

    pub(crate) fn draw_header(&self, raster: &mut Raster, text: &str) {
        draw_centered(raster, self.dpi / 32, text, self.header_scale);
    }

    pub(crate) fn draw_footer(&self, raster: &mut Raster, top: usize, text: &str) {
        draw_centered(raster, top + self.dpi / 48, text, self.footer_scale);
    }
}

/// Largest integer enlargement of the 8-pixel font that fits the target height.
fn fitting_scale(target_height: usize) -> usize {
    (target_height / GLYPH_SIZE).max(1)
}

pub(crate) fn header_line(
    name: &str,
    modified: FileTime,
    original_size: u32,
    page: usize,
    pages: usize,
) -> String {
    let name = if name.is_empty() {
        "paperback.rs"
    } else {
        name
    };
    format!(
        "{name} [{}, {original_size} bytes] - page {page} of {pages}",
        format_timestamp(modified)
    )
}

pub(crate) fn footer_line(printer_dpi: usize, dot_pitch: usize) -> String {
    format!(
        "Recommended scanner resolution {} dpi, grayscale - restore with: paperback-rs decode - {PROJECT_URL}",
        printer_dpi * SCAN_OVERSAMPLING / dot_pitch
    )
}

/// `YYYY-MM-DD HH:MM` in UTC; empty when the time is unknown.
pub(crate) fn format_timestamp(time: FileTime) -> String {
    let Some(seconds) = time.to_unix_seconds() else {
        return String::new();
    };
    let (year, month, day) = civil_date(seconds / SECONDS_PER_DAY);
    let of_day = seconds % SECONDS_PER_DAY;
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        of_day / 3600,
        of_day % 3600 / 60
    )
}

/// Year, month and day for a number of days since 1970-01-01 (proleptic Gregorian).
fn civil_date(days_since_epoch: u64) -> (i64, u32, u32) {
    let z = days_since_epoch as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn glyph(c: char) -> [u8; GLYPH_SIZE] {
    match c {
        c if c.is_ascii() => BASIC_LEGACY[c as usize],
        c => LATIN_FONTS
            .get(c)
            .unwrap_or(BASIC_LEGACY[UNKNOWN_GLYPH as usize]),
    }
}

/// Draws `text` centred, as large as possible up to `scale` while still fitting.
fn draw_centered(raster: &mut Raster, top: usize, text: &str, scale: usize) {
    let chars: Vec<char> = text.chars().collect();
    let advance_for = |scale: usize| GLYPH_SIZE * scale;
    let scale = (1..=scale)
        .rev()
        .find(|&s| chars.len() * advance_for(s) <= raster.width())
        .unwrap_or(1);
    let text_width = chars.len() * advance_for(scale);
    let left = raster.width().saturating_sub(text_width) / 2;
    for (index, &c) in chars.iter().enumerate() {
        let x0 = left + index * advance_for(scale);
        for (row, bits) in glyph(c).iter().enumerate() {
            for column in 0..GLYPH_SIZE {
                if bits >> column & 1 == 1 {
                    raster.fill_rect(
                        x0 + column * scale,
                        top + row * scale,
                        scale,
                        scale,
                        TEXT_LEVEL,
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raster::WHITE;

    #[test]
    fn timestamps_are_formatted_in_utc() {
        let time = FileTime::from_unix_seconds(1_700_000_000);
        assert_eq!(format_timestamp(time), "2023-11-14 22:13");
        assert_eq!(
            format_timestamp(FileTime::from_unix_seconds(0)),
            "1970-01-01 00:00"
        );
        assert_eq!(
            format_timestamp(FileTime::from_unix_seconds(951_782_400)),
            "2000-02-29 00:00"
        );
    }

    #[test]
    fn unknown_times_give_no_text() {
        assert_eq!(format_timestamp(FileTime::default()), "");
    }

    #[test]
    fn header_names_file_size_and_page() {
        let line = header_line("a.txt", FileTime::from_unix_seconds(0), 12, 2, 5);
        assert_eq!(line, "a.txt [1970-01-01 00:00, 12 bytes] - page 2 of 5");
    }

    #[test]
    fn footer_recommends_triple_the_dot_density() {
        assert!(footer_line(600, 3).starts_with("Recommended scanner resolution 600 dpi"));
    }

    #[test]
    fn bands_grow_with_the_printer_resolution() {
        let low = TextStyle::for_dpi(300);
        let high = TextStyle::for_dpi(600);
        assert!(high.top_band() > low.top_band());
        assert!(high.bottom_band() > low.bottom_band());
    }

    #[test]
    fn long_text_shrinks_to_fit_the_width() {
        let style = TextStyle::for_dpi(600);
        let mut raster = Raster::filled(800, style.top_band(), WHITE);
        style.draw_header(&mut raster, &"x".repeat(80));
        assert!((0..raster.height()).all(|y| raster.row(y)[0] == WHITE));
        assert!(raster.pixels().contains(&TEXT_LEVEL));
    }

    #[test]
    fn characters_without_a_glyph_become_question_marks() {
        assert_eq!(glyph('\u{4e2d}'), glyph('?'));
    }
}
