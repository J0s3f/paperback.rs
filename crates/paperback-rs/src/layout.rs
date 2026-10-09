// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider
// Derived from PaperBack 1.10, Copyright (c) 2007 Oleh Yuschuk; see NOTICE.md.

//! Geometry of a page: how many blocks fit and where every dot goes.

use crate::block::{BLOCK_DOTS, DATA_LEN, Redundancy};
use crate::error::{Error, Result};
use crate::pagetext::TextStyle;

/// Blocks are separated by three dot pitches (two before, one after the grid line).
pub(crate) const CELL_DOTS: usize = BLOCK_DOTS + 3;
/// Dots between the grid line and the first dot of a block.
const BLOCK_LEAD_DOTS: usize = 2;
const MIN_DOT_PITCH: usize = 2;
const MIN_ROWS: usize = 3;
/// Width of the quiet margin around the grid in image output.
const PLAIN_BORDER: usize = 25;
const BORDER_DOTS: usize = 16;
const THOUSANDTHS_PER_INCH: usize = 1000;
const MAX_DPI: usize = 9600;
const MAX_DOT_PERCENT: usize = 200;
const MAX_MARGIN_MILS: usize = 20_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Sheet sizes.
#[non_exhaustive]
pub enum Paper {
    /// 210 x 297 mm.
    A4,
    /// 8.5 x 11 inches.
    Letter,
}

impl Paper {
    /// Width and height in thousandths of an inch.
    fn size_in_mils(self) -> (usize, usize) {
        match self {
            Self::A4 => (8270, 11690),
            Self::Letter => (8500, 11000),
        }
    }
}

/// Page margins in thousandths of an inch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Margins {
    /// Left margin.
    pub left: usize,
    /// Right margin.
    pub right: usize,
    /// Top margin.
    pub top: usize,
    /// Bottom margin.
    pub bottom: usize,
}

impl Default for Margins {
    fn default() -> Self {
        Self {
            left: 1000,
            right: 400,
            top: 400,
            bottom: 500,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Sheet, margins and print resolution of the pages.
pub struct PageSetup {
    /// Sheet the pages are laid out for.
    pub paper: Paper,
    /// Unprinted border of the sheet.
    pub margins: Margins,
    /// Resolution of the printer the page is meant for, dots per inch.
    pub printer_dpi: usize,
    /// Density of the data dots, dots per inch.
    pub dot_dpi: usize,
    /// Size of a dot as a percentage of the dot pitch.
    pub dot_percent: usize,
    /// Draw a grid frame with alignment raster around the page.
    pub frame: bool,
    /// Print a title above and a scanning hint below the grid.
    pub text: bool,
}

impl Default for PageSetup {
    fn default() -> Self {
        Self {
            paper: Paper::A4,
            margins: Margins::default(),
            printer_dpi: 600,
            dot_dpi: 200,
            dot_percent: 70,
            frame: false,
            text: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PageLayout {
    /// Distance between dots, pixels.
    pub(crate) dx: usize,
    pub(crate) dy: usize,
    /// Size of a dot, pixels.
    pub(crate) px: usize,
    pub(crate) py: usize,
    /// Quiet area around the grid, pixels.
    pub(crate) border: usize,
    /// Grid size in blocks.
    pub(crate) nx: usize,
    pub(crate) ny: usize,
    /// Title and hint lines, when printed.
    pub(crate) text: Option<TextStyle>,
}

impl PageSetup {
    /// Rejects values that are meaningless or would need absurd amounts of memory.
    fn validate(&self) -> Result<()> {
        let in_range = |value: usize, max: usize| (1..=max).contains(&value);
        if !in_range(self.printer_dpi, MAX_DPI) || !in_range(self.dot_dpi, MAX_DPI) {
            return Err(Error::InvalidSetting(format!(
                "resolutions must be between 1 and {MAX_DPI} dpi"
            )));
        }
        if !in_range(self.dot_percent, MAX_DOT_PERCENT) {
            return Err(Error::InvalidSetting(format!(
                "the dot size must be between 1 and {MAX_DOT_PERCENT} percent"
            )));
        }
        let m = self.margins;
        if [m.left, m.right, m.top, m.bottom]
            .iter()
            .any(|&v| v > MAX_MARGIN_MILS)
        {
            return Err(Error::InvalidSetting(
                "margins are larger than 20 inches".into(),
            ));
        }
        Ok(())
    }
}

impl PageLayout {
    pub(crate) fn compute(setup: &PageSetup, redundancy: Redundancy) -> Result<Self> {
        setup.validate()?;
        let (paper_width, paper_height) = setup.paper.size_in_mils();
        let to_pixels = |mils: usize| mils * setup.printer_dpi / THOUSANDTHS_PER_INCH;
        let margins = setup.margins;
        let width = to_pixels(paper_width)
            .checked_sub(to_pixels(margins.left) + to_pixels(margins.right))
            .ok_or(Error::PageTooSmall)?;
        let text = setup.text.then(|| TextStyle::for_dpi(setup.printer_dpi));
        let text_height = text.map_or(0, |t| t.top_band() + t.bottom_band());
        let height = to_pixels(paper_height)
            .checked_sub(to_pixels(margins.top) + to_pixels(margins.bottom) + text_height)
            .ok_or(Error::PageTooSmall)?;

        let dx = (setup.printer_dpi / setup.dot_dpi).max(MIN_DOT_PITCH);
        let dy = dx;
        let px = (dx * setup.dot_percent / 100).max(1);
        let py = px;
        let border = if setup.frame {
            dx * BORDER_DOTS
        } else {
            PLAIN_BORDER
        };

        let nx = width.saturating_sub(px + 2 * border) / (CELL_DOTS * dx);
        let ny = height.saturating_sub(py + 2 * border) / (CELL_DOTS * dy);
        let group = redundancy.group_size();
        if nx < group + 1 || ny < MIN_ROWS || nx * ny < 2 * group + 2 {
            return Err(Error::PageTooSmall);
        }
        let layout = Self {
            dx,
            dy,
            px,
            py,
            border,
            nx,
            ny,
            text,
        };
        if !crate::raster::size_is_allowed(layout.image_width(), layout.page_height(layout.ny)) {
            return Err(Error::InvalidSetting(
                "the page would be larger than the supported picture size; lower the resolution"
                    .into(),
            ));
        }
        Ok(layout)
    }

    /// Bytes of payload on a full page: per string of `group` data blocks one
    /// recovery block and, per page, one superblock for every string.
    pub(crate) fn page_capacity(&self, redundancy: Redundancy) -> usize {
        let group = redundancy.group_size();
        ((self.nx * self.ny - group - 2) / (group + 1)) * group * DATA_LEN
    }

    pub(crate) fn image_width(&self) -> usize {
        (self.nx * CELL_DOTS * self.dx + self.px + 2 * self.border + 3) & !3
    }

    /// Height of the whole page image: title band, grid and hint band.
    pub(crate) fn page_height(&self, rows: usize) -> usize {
        let bands = self.text.map_or(0, |t| t.top_band() + t.bottom_band());
        self.image_height(rows) + bands
    }

    /// Height of the dot grid alone.
    pub(crate) fn image_height(&self, rows: usize) -> usize {
        rows * CELL_DOTS * self.dy + self.py + 2 * self.border
    }

    /// Top-left pixel of the block in the cell with the given index; cells
    /// run left to right and wrap to the next row.
    pub(crate) fn cell_origin(&self, cell: usize) -> (usize, usize) {
        let (column, row) = (cell % self.nx, cell / self.nx);
        (
            column * self.grid_step_x() + BLOCK_LEAD_DOTS * self.dx + self.border,
            row * self.grid_step_y() + BLOCK_LEAD_DOTS * self.dy + self.border,
        )
    }

    /// Pixel offset of grid line `index` (0..=n) before the border.
    pub(crate) fn grid_step_x(&self) -> usize {
        CELL_DOTS * self.dx
    }

    pub(crate) fn grid_step_y(&self) -> usize {
        CELL_DOTS * self.dy
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_a4_page_at_600_dpi_holds_a_sensible_amount() {
        let layout = PageLayout::compute(&PageSetup::default(), Redundancy::default()).unwrap();
        assert_eq!((layout.dx, layout.px), (3, 2));
        let capacity = layout.page_capacity(Redundancy::default());
        assert!(
            (100_000..400_000).contains(&capacity),
            "capacity {capacity}"
        );
    }

    #[test]
    fn image_width_is_a_multiple_of_four() {
        let layout = PageLayout::compute(&PageSetup::default(), Redundancy::default()).unwrap();
        assert_eq!(layout.image_width() % 4, 0);
    }

    #[test]
    fn tiny_papers_are_rejected() {
        let setup = PageSetup {
            printer_dpi: 60,
            ..PageSetup::default()
        };
        assert!(matches!(
            PageLayout::compute(&setup, Redundancy::default()),
            Err(Error::PageTooSmall)
        ));
    }
}
