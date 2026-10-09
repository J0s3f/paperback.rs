// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

//! Which block sits in which cell of a sheet, across several photographs or scans of it.
//!
//! Every picture of a sheet shows the same grid of cells, but each picture numbers its cells
//! from its own corner and may be turned or mirrored against another. The address of a block
//! read in a cell gives the link: a few blocks read in a new picture say which of the eight
//! turns and mirrors, and which shift, takes the cells of an earlier picture to those of this
//! one. After that, every cell whose block an earlier picture gave need not be read at all.

use std::collections::{HashMap, HashSet};

use crate::block::SuperBlock;
use crate::pbx::SheetId;
use crate::plan::RECORD_CELLS_PER_END;

/// Where a block belongs in the file: recovery block or data block, and its address.
pub(crate) type BlockKey = (bool, u32);

/// A cell of the grid of a picture: column and row.
pub(crate) type Cell = (usize, usize);

/// Cells read for a first look at a picture, to find how it relates to the earlier ones.
pub(crate) const SAMPLES_PER_SIDE: usize = 4;
/// Agreeing blocks needed to trust the relation between two pictures.
const MIN_VOTES: usize = 3;
/// The same when both pictures say they show the same sheet.
const MIN_VOTES_OF_KNOWN_SHEET: usize = 2;

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
    /// The identifier the sheet's pictures carried, if they showed one.
    id: Option<SheetId>,
}

impl SheetMap {
    fn new(columns: usize, rows: usize, label: SuperBlock, id: Option<SheetId>) -> Self {
        Self {
            columns,
            rows,
            keys: vec![None; columns * rows],
            cell_of: HashMap::new(),
            label,
            id,
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
    /// enough. `id` is the identifier the picture shows, if it showed one: a sheet with another
    /// identifier is never taken, and one with the same identifier needs fewer blocks to agree.
    pub(crate) fn recognise(
        &self,
        samples: &[(Cell, BlockKey)],
        id: Option<SheetId>,
    ) -> Option<(usize, Relation)> {
        let mut best: Option<(usize, Relation, usize, usize)> = None;
        for (index, map) in self.maps.iter().enumerate() {
            let min_votes = match (id, map.id) {
                (Some(shown), Some(known)) if shown != known => continue,
                (Some(_), Some(_)) => MIN_VOTES_OF_KNOWN_SHEET,
                _ => MIN_VOTES,
            };
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
            if count >= min_votes
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
    pub(crate) fn record(&mut self, read: &LatticeRead, label: &SuperBlock, id: Option<SheetId>) {
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
            self.maps[index].id = self.maps[index].id.or(id);
            return;
        }
        let mut map = SheetMap::new(read.columns, read.rows, label.clone(), id);
        for (at, key) in read.keys.iter().enumerate() {
            if let Some(key) = key {
                map.set((at % read.columns, at / read.columns), *key);
            }
        }
        self.maps.push(map);
    }
}

/// How the blocks read in a picture agree with the layout the sheet announces.
#[derive(Debug, Default)]
pub(crate) struct PlanCheck {
    /// Cells of the picture (by index) holding a block that the layout puts elsewhere.
    pub(crate) misplaced: Vec<usize>,
    /// Blocks found in the cell the layout gives them.
    pub(crate) confirmed: HashSet<BlockKey>,
}

/// Compares what a picture read with the layout of the sheet: `plan` holds the block each cell
/// of the sheet (`columns` by `rows`) must carry. The picture may be turned, mirrored or shifted
/// against the plan; that is found from the blocks themselves. `None` if the picture does not
/// agree with the plan well enough to say which cell is which.
pub(crate) fn check_against_plan(
    read: &LatticeRead,
    plan: &[Option<BlockKey>],
    (columns, rows): (usize, usize),
    label: &SuperBlock,
) -> Option<PlanCheck> {
    let mut map = SheetMap::new(columns, rows, label.clone(), None);
    for (at, key) in plan.iter().enumerate() {
        if let Some(key) = key {
            map.set((at % columns, at / columns), *key);
        }
    }
    let sheets = Sheets { maps: vec![map] };
    let cell_of = |at: usize| (at % read.columns, at / read.columns);
    let samples: Vec<(Cell, BlockKey)> = read
        .keys
        .iter()
        .enumerate()
        .filter_map(|(at, key)| key.map(|key| (cell_of(at), key)))
        .collect();
    let (_, relation) = sheets.recognise(&samples, None)?;
    let mut check = PlanCheck::default();
    for (at, key) in read.keys.iter().enumerate() {
        let Some(key) = *key else {
            continue;
        };
        let expected = relation
            .to_sheet(cell_of(at))
            .and_then(|(c, r)| plan[r * columns + c]);
        if expected == Some(key) {
            check.confirmed.insert(key);
        } else {
            check.misplaced.push(at);
        }
    }
    Some(check)
}

/// Cells in the corners of a grid, where a page keeps its records whatever the turn of the
/// picture: a run of cells along one edge, starting in a corner.
pub(crate) fn corner_cells(columns: usize, rows: usize) -> Vec<Cell> {
    let side = RECORD_CELLS_PER_END;
    let mut cells = Vec::new();
    for row in (0..side).chain(rows.saturating_sub(side)..rows) {
        for column in (0..side).chain(columns.saturating_sub(side)..columns) {
            if row < rows && column < columns && !cells.contains(&(column, row)) {
                cells.push((column, row));
            }
        }
    }
    cells
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
            sheets.record(&first, &a_label(), None);
            let other = other_picture(&first, turn, (2, 1), |_| true);
            let found = sheets
                .recognise(&samples_of(&other), None)
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

    fn plan_of(sheet: &LatticeRead) -> Vec<Option<BlockKey>> {
        sheet.keys.clone()
    }

    #[test]
    fn blocks_in_their_cells_of_the_plan_are_confirmed_whatever_the_turn() {
        let first = first_picture();
        let plan = plan_of(&first);
        for turn in 0..8 {
            let other = other_picture(&first, turn, (1, 2), |_| true);
            let check = check_against_plan(&other, &plan, (first.columns, first.rows), &a_label())
                .unwrap_or_else(|| panic!("turn {turn} not matched"));
            assert!(check.misplaced.is_empty(), "turn {turn}");
            assert_eq!(check.confirmed.len(), first.columns * first.rows);
        }
    }

    #[test]
    fn a_block_in_a_cell_the_plan_gives_to_another_is_misplaced() {
        let first = first_picture();
        let plan = plan_of(&first);
        let mut other = other_picture(&first, 3, (2, 1), |_| true);
        let cells: Vec<usize> = (0..other.keys.len())
            .filter(|&at| other.keys[at].is_some())
            .collect();
        // Two blocks swap places, and one cell holds a block the plan does not know.
        other.keys.swap(cells[4], cells[9]);
        other.keys[cells[12]] = Some((false, 999_990));
        let check =
            check_against_plan(&other, &plan, (first.columns, first.rows), &a_label()).unwrap();
        let mut misplaced = check.misplaced.clone();
        misplaced.sort_unstable();
        assert_eq!(misplaced, vec![cells[4], cells[9], cells[12]]);
    }

    #[test]
    fn a_picture_that_does_not_fit_the_plan_is_not_judged() {
        let first = first_picture();
        let mut plan = plan_of(&first);
        plan.reverse();
        plan.rotate_left(7);
        let mut other = other_picture(&first, 0, (0, 0), |_| true);
        // Half of the blocks are from elsewhere.
        for (at, key) in other.keys.iter_mut().enumerate() {
            if at % 2 == 0 && key.is_some() {
                *key = Some((true, 70_000 + at as u32));
            }
        }
        assert!(
            check_against_plan(&other, &plan, (first.columns, first.rows), &a_label()).is_none()
        );
    }

    fn few_blocks_of(read: &LatticeRead, keep: usize) -> LatticeRead {
        let mut few = read.clone();
        let kept: Vec<usize> = (0..few.keys.len())
            .filter(|&at| few.keys[at].is_some())
            .collect();
        for &at in kept.iter().skip(keep) {
            few.keys[at] = None;
        }
        few
    }

    #[test]
    fn a_picture_that_shows_the_identifier_of_a_sheet_needs_fewer_blocks_to_match() {
        let id = SheetId([5; 16]);
        let mut sheets = Sheets::default();
        let first = first_picture();
        sheets.record(&first, &a_label(), Some(id));
        let few = few_blocks_of(&other_picture(&first, 3, (1, 1), |_| true), 2);
        assert!(sheets.recognise(&samples_of(&few), None).is_none());
        assert_eq!(
            sheets.recognise(&samples_of(&few), Some(id)).map(|f| f.0),
            Some(0)
        );
    }

    #[test]
    fn a_picture_with_another_identifier_is_never_taken_for_the_sheet() {
        let mut sheets = Sheets::default();
        let first = first_picture();
        sheets.record(&first, &a_label(), Some(SheetId([5; 16])));
        let other = other_picture(&first, 0, (1, 1), |_| true);
        assert!(sheets.recognise(&samples_of(&other), None).is_some());
        assert!(
            sheets
                .recognise(&samples_of(&other), Some(SheetId([6; 16])))
                .is_none()
        );
    }

    #[test]
    fn a_sheet_that_never_showed_an_identifier_matches_by_blocks_alone() {
        let mut sheets = Sheets::default();
        let first = first_picture();
        sheets.record(&first, &a_label(), None);
        let other = other_picture(&first, 0, (1, 1), |_| true);
        assert!(
            sheets
                .recognise(&samples_of(&other), Some(SheetId([6; 16])))
                .is_some()
        );
    }

    #[test]
    fn the_corners_of_a_grid_are_listed_once_each() {
        let cells = corner_cells(10, 8);
        assert_eq!(cells.len(), 4 * 9);
        assert!(cells.contains(&(0, 0)) && cells.contains(&(9, 7)) && cells.contains(&(2, 7)));
        // A grid not larger than a corner has each cell once.
        assert_eq!(corner_cells(4, 3).len(), 12);
        assert_eq!(corner_cells(2, 2).len(), 4);
    }

    #[test]
    fn a_picture_with_too_few_known_blocks_is_not_taken_for_a_sheet() {
        let mut sheets = Sheets::default();
        let first = first_picture();
        sheets.record(&first, &a_label(), None);
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
        assert!(sheets.recognise(&samples_of(&few), None).is_none());
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
        sheets.record(&first, &a_label(), None);
        let full = first_picture();
        let other = other_picture(&full, 5, (1, 2), |_| true);
        let found = sheets
            .recognise(
                &samples_of(&other_picture(&full, 5, (1, 2), |c| {
                    c.0 % 2 == 0 || c.1 % 2 == 0
                })),
                None,
            )
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
        sheets.record(&first, &a_label(), None);
        let full = first_picture();
        let mut other = other_picture(&full, 3, (0, 0), |_| true);
        other.sheet = sheets.recognise(&samples_of(&other), None);
        sheets.record(&other, &a_label(), None);
        let found = sheets
            .recognise(
                &samples_of(&other_picture(&full, 6, (1, 0), |_| true)),
                None,
            )
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
