// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

//! How well each block of a page could be read, as a picture.
//!
//! Blocks that read without any correction are green, blocks that needed more
//! and more error correction shade through yellow to orange, and blocks that
//! could not be read are red. The original PaperBack shows the same idea as its
//! "quality map".

use crate::decode::grid::Grid;
use crate::decode::mesh::Quad;
use crate::raster::Raster;

/// Corrections a block can survive; one more and it is unreadable.
const MAX_CORRECTIONS: usize = 16;
const GREEN_HUE: f64 = 120.0;
const ORANGE_HUE: f64 = 25.0;
const RED_HUE: f64 = 0.0;
const SATURATION: f64 = 0.85;
const VALUE: f64 = 0.95;
const PAPER: [u8; 3] = [255, 255, 255];
/// Share of the colour in an overlay; the rest is the scan, so the dots stay visible.
/// Distance in pixels between the points that colour a block.
const QUAD_PAINT_STEP: f64 = 0.5;
/// Corners that must have been found in the picture for an unreadable block to be drawn.
const MIN_CORNERS_TO_PAINT: usize = 3;
/// All four corners of a block.
const FULL_QUAD: usize = 4;
const OVERLAY_STRENGTH: f64 = 0.45;

/// An 8-bit colour picture, rows stored top to bottom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColorImage {
    width: usize,
    height: usize,
    rgb: Vec<u8>,
}

impl ColorImage {
    /// Width in pixels.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Height in pixels.
    pub fn height(&self) -> usize {
        self.height
    }

    /// Red, green and blue of every pixel, row by row from the top.
    pub fn rgb(&self) -> &[u8] {
        &self.rgb
    }
}

/// What reading one block gave.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BlockQuality {
    /// There is no block here (outside the page or no grid found).
    Absent,
    /// The block was found but could not be repaired.
    Unreadable,
    /// The block was read after correcting this many bytes.
    Readable(usize),
}

impl BlockQuality {
    /// Colour of the block, or `None` where there is nothing to show.
    fn color(self) -> Option<[u8; 3]> {
        match self {
            Self::Absent => None,
            Self::Unreadable => Some(hsv(RED_HUE)),
            Self::Readable(corrected) => {
                let share = corrected.min(MAX_CORRECTIONS) as f64 / MAX_CORRECTIONS as f64;
                Some(hsv(GREEN_HUE + (ORANGE_HUE - GREEN_HUE) * share))
            }
        }
    }
}

/// Block by block quality of one page, with the picture it was read from.
#[derive(Clone, Debug)]
pub struct PageQuality {
    page: Raster,
    grid: Grid,
    columns: usize,
    rows: usize,
    cells: Vec<BlockQuality>,
    /// Where each block really sat compared with the grid, in pixels.
    shifts: Vec<(f64, f64)>,
    /// The four corners of every block (row by row from the top), when they were followed
    /// across the page; they draw bent paper better than the straight grid and the shifts.
    quads: Option<Vec<Quad>>,
}

impl PageQuality {
    pub(crate) fn new(
        page: Raster,
        grid: Grid,
        columns: usize,
        rows: usize,
        mut cells: Vec<BlockQuality>,
        shifts: Vec<(f64, f64)>,
        quads: Option<Vec<Quad>>,
    ) -> Self {
        debug_assert_eq!(cells.len(), columns * rows);
        debug_assert_eq!(shifts.len(), cells.len());
        debug_assert!(quads.as_ref().is_none_or(|q| q.len() == cells.len()));
        mark_gaps_as_unreadable(&mut cells, columns);
        if let Some(quads) = &quads {
            mark_followed_blocks_as_unreadable(&mut cells, quads);
        }
        Self {
            page,
            grid,
            columns,
            rows,
            cells,
            shifts,
            quads,
        }
    }

    /// Blocks that were read.
    pub(crate) fn readable_blocks(&self) -> usize {
        self.cells
            .iter()
            .filter(|cell| matches!(cell, BlockQuality::Readable(_)))
            .count()
    }

    /// Blocks that were found but could not be read.
    pub fn unreadable_blocks(&self) -> usize {
        self.cells
            .iter()
            .filter(|cell| **cell == BlockQuality::Unreadable)
            .count()
    }

    /// The map on its own: white paper with every block in its colour. It has
    /// the size of the page as it was read, which is the scan itself unless the
    /// page had to be straightened first.
    pub fn map(&self) -> ColorImage {
        self.paint(|_, color| color, |_| PAPER)
    }

    /// The scan with the colours laid over it. Like [`PageQuality::map`] it shows the
    /// page as it was read.
    pub fn overlay(&self) -> ColorImage {
        self.paint(
            |scan, color| {
                let scan = f64::from(scan);
                std::array::from_fn(|i| {
                    (f64::from(color[i]) * OVERLAY_STRENGTH + scan * (1.0 - OVERLAY_STRENGTH))
                        .round() as u8
                })
            },
            |scan| [scan; 3],
        )
    }

    /// Paints every pixel from the scan's gray level: `inside` for pixels in a
    /// block (given the block colour), `outside` for the rest.
    fn paint(
        &self,
        inside: impl Fn(u8, [u8; 3]) -> [u8; 3],
        outside: impl Fn(u8) -> [u8; 3],
    ) -> ColorImage {
        let (width, height) = (self.page.width(), self.page.height());
        let mut rgb = Vec::with_capacity(width * height * 3);
        if let Some(quads) = &self.quads {
            for &gray in self.page.pixels() {
                rgb.extend_from_slice(&outside(gray));
            }
            self.paint_quads(quads, &inside, &mut rgb);
            return ColorImage { width, height, rgb };
        }
        for y in 0..height {
            for (x, &gray) in self.page.row(y).iter().enumerate() {
                let pixel = match self.cell_at(x, y).and_then(BlockQuality::color) {
                    Some(color) => inside(gray, color),
                    None => outside(gray),
                };
                rgb.extend_from_slice(&pixel);
            }
        }
        ColorImage { width, height, rgb }
    }

    /// Colours every block by walking over its quadrilateral in steps of half a pixel, which
    /// leaves no gaps; a pixel on the edge of two blocks takes the colour of the later one.
    fn paint_quads(
        &self,
        quads: &[Quad],
        inside: &impl Fn(u8, [u8; 3]) -> [u8; 3],
        rgb: &mut [u8],
    ) {
        let (width, height) = (self.page.width(), self.page.height());
        for (quad, cell) in quads.iter().zip(&self.cells) {
            let Some(color) = cell.color() else {
                continue;
            };
            // A block whose lines were not found is outside the page, not a damaged block.
            if quad.corners_found < MIN_CORNERS_TO_PAINT
                && !matches!(cell, BlockQuality::Readable(_))
            {
                continue;
            }
            let longest = [
                (quad.lower_left, quad.lower_right),
                (quad.upper_left, quad.upper_right),
                (quad.lower_left, quad.upper_left),
                (quad.lower_right, quad.upper_right),
            ]
            .iter()
            .map(|(a, b)| (a.0 - b.0).hypot(a.1 - b.1))
            .fold(0.0, f64::max);
            let steps = (longest / QUAD_PAINT_STEP).ceil().max(1.0) as usize;
            for v in 0..=steps {
                for u in 0..=steps {
                    let (x, y) = quad.at(u as f64 / steps as f64, v as f64 / steps as f64);
                    // The decoder's bitmap has its first row at the bottom of the page.
                    let (px, py) = (x.round(), (height - 1) as f64 - y.round());
                    if px < 0.0 || py < 0.0 || px >= width as f64 || py >= height as f64 {
                        continue;
                    }
                    let (px, py) = (px as usize, py as usize);
                    let pixel = inside(self.page.row(py)[px], color);
                    rgb[(py * width + px) * 3..][..3].copy_from_slice(&pixel);
                }
            }
        }
    }

    /// The block covering a pixel, following the tilt of the grid and the
    /// measured shift of each block (paper that is bent moves its blocks).
    fn cell_at(&self, x: usize, y: usize) -> Option<BlockQuality> {
        // The decoder works on a bitmap whose first row is the bottom of the page.
        let memory_y = (self.page.height() - 1 - y) as f64;
        let memory_x = x as f64;
        let (column, row) = self.cell_index(memory_x, memory_y, (0.0, 0.0))?;
        let covers = |column: usize, row: usize| {
            let shift = self.shifts[row * self.columns + column];
            self.cell_index(memory_x, memory_y, shift) == Some((column, row))
        };
        let (column, row) = if covers(column, row) {
            (column, row)
        } else {
            self.neighbours(column, row).find(|&(c, r)| covers(c, r))?
        };
        Some(self.cells[row * self.columns + column])
    }

    /// Column and row (from the top) of the cell a point falls in when the grid
    /// is moved by `shift`.
    fn cell_index(&self, x: f64, y: f64, shift: (f64, f64)) -> Option<(usize, usize)> {
        let g = &self.grid;
        let (x, y) = (x - shift.0, y - shift.1);
        let column = (x - y * g.x_angle - g.x_peak) / g.x_step;
        let from_bottom = (y - x * g.y_angle - g.y_peak) / g.y_step;
        if column < 0.0 || from_bottom < 0.0 {
            return None;
        }
        let (column, from_bottom) = (column as usize, from_bottom as usize);
        if column >= self.columns || from_bottom >= self.rows {
            return None;
        }
        Some((column, self.rows - 1 - from_bottom))
    }

    fn neighbours(&self, column: usize, row: usize) -> impl Iterator<Item = (usize, usize)> {
        let (columns, rows) = (self.columns, self.rows);
        (row.saturating_sub(1)..=(row + 1).min(rows - 1)).flat_map(move |r| {
            (column.saturating_sub(1)..=(column + 1).min(columns - 1)).map(move |c| (c, r))
        })
    }
}

/// A block that is missing between blocks that were found is a block too damaged to
/// even show its grid lines, so it is shown as unreadable. Only what lies inside the
/// bounding box of the found blocks is treated so; the rest is outside the page.
fn mark_gaps_as_unreadable(cells: &mut [BlockQuality], columns: usize) {
    let found = |(_, cell): &(usize, &BlockQuality)| **cell != BlockQuality::Absent;
    let positions: Vec<(usize, usize)> = cells
        .iter()
        .enumerate()
        .filter(found)
        .map(|(at, _)| (at % columns, at / columns))
        .collect();
    let bounds = |pick: fn(&(usize, usize)) -> usize| {
        let values = positions.iter().map(pick);
        (values.clone().min(), values.max())
    };
    let ((Some(left), Some(right)), (Some(top), Some(bottom))) = (bounds(|p| p.0), bounds(|p| p.1))
    else {
        return;
    };
    for (at, cell) in cells.iter_mut().enumerate() {
        let (column, row) = (at % columns, at / columns);
        let inside = (left..=right).contains(&column) && (top..=bottom).contains(&row);
        if inside && *cell == BlockQuality::Absent {
            *cell = BlockQuality::Unreadable;
        }
    }
}

/// A block that was not read but whose four corners were found along the grid lines is
/// there, so it is shown as unreadable, also beyond the blocks that were read.
fn mark_followed_blocks_as_unreadable(cells: &mut [BlockQuality], quads: &[Quad]) {
    for (cell, quad) in cells.iter_mut().zip(quads) {
        if *cell == BlockQuality::Absent && quad.corners_found == FULL_QUAD {
            *cell = BlockQuality::Unreadable;
        }
    }
}

/// Colour for a hue in degrees at the fixed saturation and value.
fn hsv(hue: f64) -> [u8; 3] {
    let chroma = VALUE * SATURATION;
    let sector = hue / 60.0;
    let second = chroma * (1.0 - (sector % 2.0 - 1.0).abs());
    let channels = match sector as u32 {
        0 => [chroma, second, 0.0],
        1 => [second, chroma, 0.0],
        _ => [0.0, chroma, second],
    };
    let floor = VALUE - chroma;
    channels.map(|channel| ((channel + floor) * 255.0).round() as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> Grid {
        Grid {
            x_peak: 0.0,
            x_step: 10.0,
            x_angle: 0.0,
            y_peak: 0.0,
            y_step: 10.0,
            y_angle: 0.0,
        }
    }

    fn quality(cells: Vec<BlockQuality>) -> PageQuality {
        PageQuality::new(
            Raster::filled(20, 10, 100),
            grid(),
            2,
            1,
            cells,
            vec![(0.0, 0.0); 2],
            None,
        )
    }

    fn pixel(image: &ColorImage, x: usize, y: usize) -> [u8; 3] {
        let at = (y * image.width() + x) * 3;
        [image.rgb()[at], image.rgb()[at + 1], image.rgb()[at + 2]]
    }

    #[test]
    fn colours_run_from_green_over_orange_to_red() {
        let green = BlockQuality::Readable(0).color().unwrap();
        let worn = BlockQuality::Readable(MAX_CORRECTIONS).color().unwrap();
        let red = BlockQuality::Unreadable.color().unwrap();
        assert!(green[1] > green[0]);
        assert!(worn[0] > worn[2] && worn[1] > 0);
        assert!(red[0] > 200 && red[1] < 100 && red[2] < 100);
        assert!(BlockQuality::Absent.color().is_none());
    }

    #[test]
    fn the_map_shows_each_block_in_its_colour_on_white() {
        let q = quality(vec![BlockQuality::Readable(0), BlockQuality::Unreadable]);
        let map = q.map();
        assert_eq!(
            pixel(&map, 3, 5),
            BlockQuality::Readable(0).color().unwrap()
        );
        assert_eq!(
            pixel(&map, 15, 5),
            BlockQuality::Unreadable.color().unwrap()
        );
    }

    #[test]
    fn cells_without_a_block_stay_white_in_the_map_and_gray_in_the_overlay() {
        let q = quality(vec![BlockQuality::Absent, BlockQuality::Absent]);
        assert_eq!(pixel(&q.map(), 3, 5), PAPER);
        assert_eq!(pixel(&q.overlay(), 3, 5), [100, 100, 100]);
    }

    #[test]
    fn the_overlay_keeps_the_scan_visible() {
        let q = quality(vec![BlockQuality::Readable(0), BlockQuality::Unreadable]);
        let overlay = q.overlay();
        let green = BlockQuality::Readable(0).color().unwrap();
        assert_ne!(pixel(&overlay, 3, 5), green);
        assert!(pixel(&overlay, 3, 5)[1] > pixel(&overlay, 3, 5)[0]);
    }

    #[test]
    fn gaps_between_found_blocks_count_as_unreadable_but_the_margin_does_not() {
        let q = PageQuality::new(
            Raster::filled(40, 10, 100),
            grid(),
            4,
            1,
            vec![
                BlockQuality::Absent,
                BlockQuality::Readable(0),
                BlockQuality::Absent,
                BlockQuality::Readable(0),
            ],
            vec![(0.0, 0.0); 4],
            None,
        );
        assert_eq!(q.cells[0], BlockQuality::Absent);
        assert_eq!(q.cells[2], BlockQuality::Unreadable);
    }

    #[test]
    fn blocks_with_corners_are_painted_inside_their_quadrilateral_only() {
        // One block as a parallelogram leaning to the right; the picture is 40 x 20 pixels.
        let quad = Quad::new((4.0, 2.0), (14.0, 2.0), (10.0, 16.0), (20.0, 16.0), 4);
        let q = PageQuality::new(
            Raster::filled(40, 20, 100),
            grid(),
            1,
            1,
            vec![BlockQuality::Readable(0)],
            vec![(0.0, 0.0)],
            Some(vec![quad]),
        );
        let map = q.map();
        let green = BlockQuality::Readable(0).color().unwrap();
        // The bitmap's first row is the bottom: bitmap y 3 is picture row 16, y 16 is row 3.
        assert_eq!(pixel(&map, 9, 16), green);
        assert_eq!(
            pixel(&map, 5, 16),
            green,
            "inside at the bottom, where the block starts further left"
        );
        assert_eq!(pixel(&map, 12, 3), green);
        assert_eq!(
            pixel(&map, 5, 3),
            PAPER,
            "outside at the top, where the block starts further right"
        );
        assert_eq!(pixel(&map, 30, 10), PAPER);
        // Left of the leaning edge at half height there is paper, not colour.
        assert_eq!(pixel(&map, 5, 10), PAPER);
    }

    #[test]
    fn counts_unreadable_blocks() {
        let q = quality(vec![BlockQuality::Readable(3), BlockQuality::Unreadable]);
        assert_eq!((q.readable_blocks(), q.unreadable_blocks()), (1, 1));
    }
}
