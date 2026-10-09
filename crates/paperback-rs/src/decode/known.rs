// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

//! Which parts of the picture were read already.
//!
//! A page is read again after it was turned, or reduced, when the first reading left blocks
//! missing. The blocks that were read must not be read twice, and what was read must not be
//! lost. Blocks have no address before they are read, but they have a place: a block read in
//! the first picture sat at a point of it, and a cell of the turned picture can be mapped
//! back to that picture to see whether a block was read there.

use std::collections::HashMap;

use crate::raster::Turn;

pub(crate) type Point = (f64, f64);

/// How a picture that is read relates to the one the caller gave.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Placement {
    turn: Option<Turn>,
}

impl Placement {
    /// The picture is the original turned as described.
    pub(crate) fn turned(turn: Turn) -> Self {
        Self { turn: Some(turn) }
    }

    /// Where a point of a picture that was reduced by `reduced_by` from the picture read lies in
    /// the original, in pixels of the original.
    pub(crate) fn to_original(self, point: Point, reduced_by: usize) -> Point {
        let factor = reduced_by.max(1) as f64;
        // A reduced pixel covers `factor` pixels; its middle is half a pixel less than half of it.
        let middle = (factor - 1.0) / 2.0;
        let in_read_picture = (point.0 * factor + middle, point.1 * factor + middle);
        match self.turn {
            Some(turn) => turn.source_of(in_read_picture.0, in_read_picture.1),
            None => in_read_picture,
        }
    }
}

/// Places in the original picture where a block was read.
#[derive(Clone, Debug, Default)]
pub(crate) struct KnownBlocks {
    /// Side of the squares that sort the places, a bit more than a block.
    side: f64,
    squares: HashMap<(i64, i64), Vec<Point>>,
}

impl KnownBlocks {
    /// An empty set for blocks about `block_size` pixels apart.
    pub(crate) fn new(block_size: f64) -> Self {
        Self {
            side: block_size.max(1.0),
            squares: HashMap::new(),
        }
    }

    pub(crate) fn add(&mut self, point: Point) {
        let key = self.square(point);
        self.squares.entry(key).or_default().push(point);
    }

    /// Whether a block was read within `radius` of the point.
    pub(crate) fn is_near(&self, point: Point, radius: f64) -> bool {
        if self.squares.is_empty() {
            return false;
        }
        let (column, row) = self.square(point);
        (row - 1..=row + 1).any(|r| {
            (column - 1..=column + 1).any(|c| {
                self.squares.get(&(c, r)).is_some_and(|points| {
                    points
                        .iter()
                        .any(|p| (p.0 - point.0).hypot(p.1 - point.1) <= radius)
                })
            })
        })
    }

    fn square(&self, point: Point) -> (i64, i64) {
        (
            (point.0 / self.side.max(1.0)).floor() as i64,
            (point.1 / self.side.max(1.0)).floor() as i64,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raster::Raster;

    #[test]
    fn a_place_is_known_only_near_where_a_block_was_read() {
        let mut known = KnownBlocks::new(100.0);
        known.add((250.0, 130.0));
        assert!(known.is_near((260.0, 140.0), 40.0));
        assert!(!known.is_near((330.0, 130.0), 40.0));
        // Across the border of the squares that sort the places.
        assert!(known.is_near((299.0, 199.0), 90.0));
        // And a set with nothing in it knows no place.
        assert!(!KnownBlocks::default().is_near((0.0, 0.0), 1e9));
    }

    #[test]
    fn a_reduced_picture_maps_back_to_the_pixels_it_was_made_of() {
        let placement = Placement::default();
        // Reduced pixel 10 covers original pixels 40 to 43, of which 41.5 is the middle.
        let (x, y) = placement.to_original((10.0, 3.0), 4);
        assert!((x - 41.5).abs() < 1e-9 && (y - 13.5).abs() < 1e-9);
    }

    #[test]
    fn a_point_of_a_turned_picture_maps_back_to_the_original() {
        let original = Raster::filled(300, 200, 255);
        let (turn, width, height) = Turn::new(original.width(), original.height(), 30.0);
        let placement = Placement::turned(turn);
        // The middle of the turned picture is the middle of the original.
        let (x, y) =
            placement.to_original((width as f64 / 2.0 - 0.5, height as f64 / 2.0 - 0.5), 1);
        assert!(
            (x - 149.5).abs() < 0.01 && (y - 99.5).abs() < 0.01,
            "{x} {y}"
        );
    }
}
