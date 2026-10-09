// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

//! Which block sits in which cell of a sheet, across several photographs or scans of it.
//!
//! Every picture of a sheet shows the same grid of cells, but each picture numbers its cells
//! from its own corner and may be turned or mirrored against another. The address of a block
//! read in a cell gives the link: a few blocks read in a new picture say which of the eight
//! turns and mirrors, and which shift, takes the cells of an earlier picture to those of this
//! one. After that, every cell whose block an earlier picture gave need not be read at all.

use std::collections::HashMap;

use crate::block::SuperBlock;

/// Where a block belongs in the file: recovery block or data block, and its address.
pub(crate) type BlockKey = (bool, u32);

/// A cell of the grid of a picture: column and row.
pub(crate) type Cell = (usize, usize);

/// Cells read for a first look at a picture, to find how it relates to the earlier ones.
pub(crate) const SAMPLES_PER_SIDE: usize = 4;
/// Agreeing blocks needed to trust the relation between two pictures.
const MIN_VOTES: usize = 3;

/// What a picture read: its grid and the key of the block read in each cell.
#[derive(Clone, Debug, Default)]
pub(crate) struct LatticeRead {
    pub(crate) columns: usize,
    pub(crate) rows: usize,
    pub(crate) keys: Vec<Option<BlockKey>>,
    /// The sheet this picture was found to show, and how its cells relate to the sheet's.
    pub(crate) sheet: Option<(usize, Relation)>,
}

/// How the grid of a sheet is laid onto the grid of a picture: one of the eight turns and
/// mirrors of the square, then a shift.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Relation {
    turn: usize,
    shift: (isize, isize),
    /// Size of the grid of the sheet, which the turns use.
    sheet_size: (usize, usize),
}

impl Relation {
    /// The cell of the picture that shows `cell` of the sheet.
    fn to_picture(self, cell: Cell) -> (isize, isize) {
        let (columns, rows) = (self.sheet_size.0 as isize, self.sheet_size.1 as isize);
        let (c, r) = (cell.0 as isize, cell.1 as isize);
        let turned = match self.turn {
            0 => (c, r),
            1 => (columns - 1 - c, r),
            2 => (c, rows - 1 - r),
            3 => (columns - 1 - c, rows - 1 - r),
            4 => (r, c),
            5 => (rows - 1 - r, c),
            6 => (r, columns - 1 - c),
            _ => (rows - 1 - r, columns - 1 - c),
        };
        (turned.0 + self.shift.0, turned.1 + self.shift.1)
    }

    /// The cell of the sheet that a cell of the picture shows, if it lies in the sheet's grid.
    fn to_sheet(self, cell: Cell) -> Option<Cell> {
        let (columns, rows) = (self.sheet_size.0 as isize, self.sheet_size.1 as isize);
        let (x, y) = (
            cell.0 as isize - self.shift.0,
            cell.1 as isize - self.shift.1,
        );
        let (c, r) = match self.turn {
            0 => (x, y),
            1 => (columns - 1 - x, y),
            2 => (x, rows - 1 - y),
            3 => (columns - 1 - x, rows - 1 - y),
            4 => (y, x),
            5 => (y, rows - 1 - x),
            6 => (columns - 1 - y, x),
            _ => (columns - 1 - y, rows - 1 - x),
        };
        ((0..columns).contains(&c) && (0..rows).contains(&r)).then_some((c as usize, r as usize))
    }
}

/// The blocks known of one sheet, by the cell of the sheet's own grid.
#[derive(Clone, Debug)]
struct SheetMap {
    columns: usize,
    rows: usize,
    keys: Vec<Option<BlockKey>>,
    cell_of: HashMap<BlockKey, Cell>,
    /// The label the sheet's pictures carried.
    label: SuperBlock,
}

impl SheetMap {
    fn new(columns: usize, rows: usize, label: SuperBlock) -> Self {
        Self {
            columns,
            rows,
            keys: vec![None; columns * rows],
            cell_of: HashMap::new(),
            label,
        }
    }

    fn set(&mut self, cell: Cell, key: BlockKey) {
        self.keys[cell.1 * self.columns + cell.0] = Some(key);
        self.cell_of.insert(key, cell);
    }
}

/// The sheets seen so far, kept apart.
#[derive(Clone, Debug, Default)]
pub(crate) struct Sheets {
    maps: Vec<SheetMap>,
}

impl Sheets {
    pub(crate) fn is_empty(&self) -> bool {
        self.maps.is_empty()
    }

    /// Which sheet the blocks read at `samples` (cells of a picture with `columns` by `rows`
    /// cells) belong to, and how the picture relates to it; `None` if no sheet agrees well
    /// enough.
    pub(crate) fn recognise(&self, samples: &[(Cell, BlockKey)]) -> Option<(usize, Relation)> {
        let mut best: Option<(usize, Relation, usize, usize)> = None;
        for (index, map) in self.maps.iter().enumerate() {
            let mut votes: HashMap<(usize, (isize, isize)), usize> = HashMap::new();
            let mut matched = 0;
            for &(cell, key) in samples {
                let Some(&at) = map.cell_of.get(&key) else {
                    continue;
                };
                matched += 1;
                for turn in 0..8 {
                    let relation = Relation {
                        turn,
                        shift: (0, 0),
                        sheet_size: (map.columns, map.rows),
                    };
                    let turned = relation.to_picture(at);
                    let shift = (cell.0 as isize - turned.0, cell.1 as isize - turned.1);
                    *votes.entry((turn, shift)).or_default() += 1;
                }
            }
            let Some((&(turn, shift), &count)) = votes.iter().max_by_key(|(_, count)| **count)
            else {
                continue;
            };
            // Most of the blocks that were found in the sheet must agree.
            if count >= MIN_VOTES
                && count * 4 >= matched * 3
                && best.is_none_or(|(_, _, c, _)| count > c)
            {
                let relation = Relation {
                    turn,
                    shift,
                    sheet_size: (map.columns, map.rows),
                };
                best = Some((index, relation, count, matched));
            }
        }
        best.map(|(index, relation, _, _)| (index, relation))
    }

    /// The label of a sheet, for a picture of it that shows no label block.
    pub(crate) fn label_of(&self, sheet: usize) -> SuperBlock {
        self.maps[sheet].label.clone()
    }

    /// The cells of a picture with `columns` by `rows` cells whose blocks the sheet already has.
    pub(crate) fn known_cells(
        &self,
        (sheet, relation): (usize, Relation),
        columns: usize,
        rows: usize,
    ) -> Vec<bool> {
        let mut known = vec![false; columns * rows];
        let map = &self.maps[sheet];
        for row in 0..map.rows {
            for column in 0..map.columns {
                if map.keys[row * map.columns + column].is_none() {
                    continue;
                }
                let (c, r) = relation.to_picture((column, row));
                if (0..columns as isize).contains(&c) && (0..rows as isize).contains(&r) {
                    known[r as usize * columns + c as usize] = true;
                }
            }
        }
        known
    }

    /// Adds what a picture read: to the sheet it was found to show, or as a sheet of its own.
    pub(crate) fn record(&mut self, read: &LatticeRead, label: &SuperBlock) {
        if read.columns == 0 || read.rows == 0 {
            return;
        }
        if let Some((index, relation)) = read.sheet {
            for (at, key) in read.keys.iter().enumerate() {
                let Some(key) = key else {
                    continue;
                };
                let cell = (at % read.columns, at / read.columns);
                if let Some(in_sheet) = relation.to_sheet(cell) {
                    self.maps[index].set(in_sheet, *key);
                }
            }
            return;
        }
        let mut map = SheetMap::new(read.columns, read.rows, label.clone());
        for (at, key) in read.keys.iter().enumerate() {
            if let Some(key) = key {
                map.set((at % read.columns, at / read.columns), *key);
            }
        }
        self.maps.push(map);
    }
}

/// Cells spread over the middle of a grid, for a first look at a picture.
pub(crate) fn sample_cells(columns: usize, rows: usize) -> Vec<Cell> {
    let n = SAMPLES_PER_SIDE;
    (1..=n)
        .flat_map(|j| (1..=n).map(move |i| (columns * i / (n + 1), rows * j / (n + 1))))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_label() -> SuperBlock {
        use crate::block::{FileTime, Mode, NAME_LEN};
        SuperBlock {
            data_size: 900,
            page_size: 900,
            original_size: 900,
            mode: Mode(0),
            attributes: 0x20,
            page: 1,
            modified: FileTime::from_unix_seconds(1_600_000_000),
            file_crc: 0,
            name: [0; NAME_LEN],
        }
    }

    /// A sheet of 6 x 5 cells whose block in each cell has a key of its own.
    fn first_picture() -> LatticeRead {
        let (columns, rows) = (6, 5);
        let keys = (0..columns * rows)
            .map(|at| Some((at % 7 == 0, 90 * at as u32)))
            .collect();
        LatticeRead {
            columns,
            rows,
            keys,
            sheet: None,
        }
    }

    /// The same sheet seen turned by `turn` and shifted, with only some of the blocks read.
    fn other_picture(
        first: &LatticeRead,
        turn: usize,
        shift: (isize, isize),
        readable: impl Fn(Cell) -> bool,
    ) -> LatticeRead {
        let relation = Relation {
            turn,
            shift,
            sheet_size: (first.columns, first.rows),
        };
        // The picture's grid is large enough for any turn and shift used here.
        let (columns, rows) = (9, 9);
        let mut keys = vec![None; columns * rows];
        for row in 0..first.rows {
            for column in 0..first.columns {
                let (c, r) = relation.to_picture((column, row));
                let cell = (c as usize, r as usize);
                if readable(cell) {
                    keys[cell.1 * columns + cell.0] = first.keys[row * first.columns + column];
                }
            }
        }
        LatticeRead {
            columns,
            rows,
            keys,
            sheet: None,
        }
    }

    fn samples_of(read: &LatticeRead) -> Vec<(Cell, BlockKey)> {
        read.keys
            .iter()
            .enumerate()
            .filter_map(|(at, key)| key.map(|key| ((at % read.columns, at / read.columns), key)))
            .collect()
    }

    #[test]
    fn every_turn_and_mirror_of_a_picture_is_recognised() {
        for turn in 0..8 {
            let mut sheets = Sheets::default();
            let first = first_picture();
            sheets.record(&first, &a_label());
            let other = other_picture(&first, turn, (2, 1), |_| true);
            let found = sheets
                .recognise(&samples_of(&other))
                .unwrap_or_else(|| panic!("turn {turn} not recognised"));
            assert_eq!(found.0, 0);
            // The cells of the picture map back to the cells of the sheet.
            for (cell, key) in samples_of(&other) {
                let in_sheet = found.1.to_sheet(cell).unwrap();
                assert_eq!(
                    first.keys[in_sheet.1 * first.columns + in_sheet.0],
                    Some(key)
                );
            }
        }
    }

    #[test]
    fn a_picture_with_too_few_known_blocks_is_not_taken_for_a_sheet() {
        let mut sheets = Sheets::default();
        let first = first_picture();
        sheets.record(&first, &a_label());
        let mut few = other_picture(&first, 0, (1, 1), |_| true);
        let kept: Vec<usize> = few
            .keys
            .iter()
            .enumerate()
            .filter(|(_, k)| k.is_some())
            .map(|(i, _)| i)
            .collect();
        for &at in kept.iter().skip(2) {
            few.keys[at] = None;
        }
        assert!(sheets.recognise(&samples_of(&few)).is_none());
    }

    #[test]
    fn cells_known_from_earlier_pictures_are_marked_in_a_new_one() {
        let mut sheets = Sheets::default();
        let mut first = first_picture();
        // The first picture read only the cells in even columns.
        for (at, key) in first.keys.iter_mut().enumerate() {
            if (at % first.columns) % 2 == 1 {
                *key = None;
            }
        }
        sheets.record(&first, &a_label());
        let full = first_picture();
        let other = other_picture(&full, 5, (1, 2), |_| true);
        let found = sheets
            .recognise(&samples_of(&other_picture(&full, 5, (1, 2), |c| {
                c.0 % 2 == 0 || c.1 % 2 == 0
            })))
            .unwrap();
        let known = sheets.known_cells(found, other.columns, other.rows);
        // Cells that show an even column of the sheet are known, the others are not.
        for row in 0..full.rows {
            for column in 0..full.columns {
                let (c, r) = found.1.to_picture((column, row));
                let expected = column % 2 == 0;
                assert_eq!(
                    known[r as usize * other.columns + c as usize],
                    expected,
                    "({column},{row})"
                );
            }
        }
    }

    #[test]
    fn what_a_later_picture_reads_is_added_to_the_sheet() {
        let mut sheets = Sheets::default();
        let mut first = first_picture();
        first.keys[0] = None;
        sheets.record(&first, &a_label());
        let full = first_picture();
        let mut other = other_picture(&full, 3, (0, 0), |_| true);
        other.sheet = sheets.recognise(&samples_of(&other));
        sheets.record(&other, &a_label());
        let found = sheets
            .recognise(&samples_of(&other_picture(&full, 6, (1, 0), |_| true)))
            .unwrap();
        let known = sheets.known_cells(found, 9, 9);
        assert_eq!(
            known.iter().filter(|&&k| k).count(),
            30,
            "every cell of the sheet is known now"
        );
    }

    #[test]
    fn samples_are_spread_over_the_grid() {
        let cells = sample_cells(20, 20);
        assert_eq!(cells.len(), SAMPLES_PER_SIDE * SAMPLES_PER_SIDE);
        assert!(
            cells
                .iter()
                .all(|&(c, r)| c > 0 && c < 20 && r > 0 && r < 20)
        );
    }
}
