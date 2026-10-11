// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider
// Derived from PaperBack 1.10, Copyright (c) 2007 Oleh Yuschuk; see NOTICE.md.

//! Which block sits in which cell of a page, and how a file is spread over its pages.
//!
//! Every string of cells (the data strings plus, with PaperBack's redundancy, one recovery
//! string) starts with a label block. Neighbouring strings are shifted against each other so
//! blocks of one redundancy group never share a column. Behind the strings come the parity
//! blocks of the page (see [`crate::parity`]), with a repeated label in every
//! [`LABEL_EVERY`]th cell. The encoder fills the plan with blocks; a decoder that knows the
//! layout of a page can compare it with what it read.

use crate::block::{DATA_LEN, Redundancy, recovery_address};

/// Cells at the start of a page that are left to PBX1 records, and the same number at its end:
/// one copy of each record far from the other. A page without records has none.
pub(crate) const RECORD_CELLS_PER_END: usize = 3;
/// All cells a page gives up for records.
pub(crate) const RECORD_CELLS: usize = 2 * RECORD_CELLS_PER_END;
/// Data strings of a page without PaperBack's recovery blocks; with the label at the start of
/// each, a page carries as many labels as with the default group size of the originals.
pub(crate) const PLAIN_DATA_STRINGS: usize = 6;
/// Of the cells behind the strings every this many-th repeats the label, so the page can still
/// be told what it is when some of the label blocks do not read.
pub(crate) const LABEL_EVERY: usize = 8;
/// Pages carry at least this many block rows so the orientation stays detectable.
pub(crate) const MIN_ROWS: usize = 3;

/// Whether a page of `cells` cells holds data when it gives up `reserved` of them: the label of
/// every string, one group, and the cell that stays spare as in the originals. [`Demand::paginate`]
/// lays out pages under the same rule.
pub(crate) fn has_room(cells: usize, strings: Strings, reserved: usize) -> bool {
    cells > reserved + 2 * strings.count()
}

/// How the strings of a page are made up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Strings {
    /// Data strings: data blocks per group.
    pub(crate) data: usize,
    /// PaperBack's redundancy, which adds a string of recovery blocks.
    pub(crate) recovery: Option<Redundancy>,
}

impl Strings {
    pub(crate) fn new(recovery: Option<Redundancy>) -> Self {
        Self {
            data: recovery.map_or(PLAIN_DATA_STRINGS, Redundancy::group_size),
            recovery,
        }
    }

    /// All strings of a page, each starting with a label.
    pub(crate) fn count(self) -> usize {
        self.data + usize::from(self.recovery.is_some())
    }

    /// Bytes of the file in one group.
    pub(crate) fn group_bytes(self) -> usize {
        self.data * DATA_LEN
    }

    /// Cells the strings of a page with `groups` groups take.
    pub(crate) fn cells(self, groups: usize) -> usize {
        self.count() * (groups + 1)
    }
}

/// What the cell of a page holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Slot {
    /// A copy of the page label; every string starts with one.
    Label,
    /// The data block at this offset.
    Data(u32),
    /// The recovery block of the group starting at this offset.
    Recovery(u32),
    /// The parity block with this index.
    Parity(usize),
    /// A cell no block needs. The originals fill it with another label.
    Spare,
}

impl Slot {
    /// The address of the block in the cell, if it is a data, recovery or parity block.
    pub(crate) fn address(self, strings: Strings) -> Option<u32> {
        match self {
            Self::Data(offset) => Some(offset),
            Self::Recovery(group_start) => strings
                .recovery
                .map(|redundancy| recovery_address(group_start, redundancy)),
            Self::Parity(index) => Some(crate::parity::address(index)),
            Self::Label | Self::Spare => None,
        }
    }
}

/// The plan of a page of `columns` by `rows` cells that holds `groups` groups of blocks,
/// the first one starting at byte `first_offset` of the file. Cells run left to right and
/// wrap to the next row. The blocks start `head` cells in, which are left spare; moving
/// them all by the same number of cells keeps the blocks of a group in different columns.
/// `behind` goes into the cells behind the strings, up to the `head` cells at the end of the page.
pub(crate) fn plan_page(
    first_offset: u32,
    groups: usize,
    strings: Strings,
    (columns, rows): (usize, usize),
    head: usize,
    behind: &[Slot],
) -> Vec<Slot> {
    let count = strings.count();
    let string_len = groups + 1;
    let wraps = string_len >= columns;
    let rotation = |string: usize| {
        let start = string * string_len;
        (columns / count * string + columns - start % columns) % columns
    };
    let cell = |string: usize, position: usize| {
        let start = string * string_len;
        if wraps {
            start + (position + rotation(string)) % string_len
        } else {
            start + position
        }
    };

    let mut slots = vec![Slot::Spare; columns * rows];
    for string in 0..count {
        let start = string * string_len;
        let at = if wraps {
            start + rotation(string)
        } else {
            start
        };
        slots[head + at] = Slot::Label;
    }
    let mut offset = first_offset;
    for index in 0..groups {
        let group_start = offset;
        for string in 0..strings.data {
            slots[head + cell(string, index + 1)] = Slot::Data(offset);
            offset += DATA_LEN as u32;
        }
        if strings.recovery.is_some() {
            slots[head + cell(strings.data, index + 1)] = Slot::Recovery(group_start);
        }
    }
    let free = head + strings.cells(groups)..slots.len().saturating_sub(head);
    for (cell, slot) in free.zip(behind) {
        slots[cell] = *slot;
    }
    slots
}

/// What goes behind the strings of a page that carries `count` parity blocks from index
/// `first` on: the parity blocks in order, and a label in every [`LABEL_EVERY`]th cell.
pub(crate) fn behind_strings(first: usize, count: usize) -> Vec<Slot> {
    let mut slots = Vec::with_capacity(count + count / (LABEL_EVERY - 1));
    let mut placed = 0;
    while placed < count {
        if slots.len() % LABEL_EVERY == LABEL_EVERY - 1 {
            slots.push(Slot::Label);
        } else {
            slots.push(Slot::Parity(first + placed));
            placed += 1;
        }
    }
    slots
}

/// Parity blocks that `free` cells behind the strings hold.
fn parity_room(free: usize) -> usize {
    free - free / LABEL_EVERY
}

/// What one page holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PageShare {
    /// Groups of the file on the page.
    pub(crate) groups: usize,
    /// Index of the first parity block on the page.
    pub(crate) parity_first: usize,
    /// Parity blocks on the page.
    pub(crate) parity_count: usize,
    /// Block rows of the page.
    pub(crate) rows: usize,
}

/// How a file is spread over its pages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pagination {
    /// Groups on every page but the last, which the label's page size gives in bytes.
    pub(crate) groups_per_page: usize,
    pub(crate) pages: Vec<PageShare>,
    /// Parity blocks of the file.
    pub(crate) parity: usize,
}

/// What a file needs of its pages.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Demand {
    /// Data blocks of the file.
    pub(crate) blocks: usize,
    pub(crate) strings: Strings,
    /// Columns and rows of a full page.
    pub(crate) size: (usize, usize),
    /// Cells of a page kept for records.
    pub(crate) reserved: usize,
    /// Parity blocks asked for.
    pub(crate) parity: usize,
    /// The most parity blocks the file can have.
    pub(crate) max_parity: usize,
    /// Whether every page uses all its rows, with as many parity blocks as fit.
    pub(crate) fill: bool,
}

impl Demand {
    /// The fewest pages that hold the file and the parity asked for. Without parity and filling,
    /// the pages are filled one after the other as PaperBack does it. Otherwise the groups are
    /// spread evenly over the pages and so are the parity blocks, each page holding a run of
    /// them. `None` if the file does not fit.
    pub(crate) fn paginate(&self) -> Option<Pagination> {
        let (columns, full_rows) = self.size;
        let cells = columns * full_rows;
        let count = self.strings.count();
        let groups = self.blocks.div_ceil(self.strings.data).max(1);
        // One cell stays spare, as in the originals.
        let most_groups = (cells.checked_sub(self.reserved + 1)? / count).checked_sub(1)?;
        if most_groups == 0 || self.parity > self.max_parity {
            return None;
        }
        let fewest_pages = groups.div_ceil(most_groups);
        if self.parity == 0 && !self.fill {
            let spare = if self.reserved > 0 { self.reserved } else { 1 };
            let pages = (0..fewest_pages)
                .map(|page| {
                    let on_page = (groups - page * most_groups).min(most_groups);
                    PageShare {
                        groups: on_page,
                        parity_first: 0,
                        parity_count: 0,
                        rows: rows_for(self.strings.cells(on_page) + spare, columns, full_rows),
                    }
                })
                .collect();
            return Some(Pagination {
                groups_per_page: most_groups,
                pages,
                parity: 0,
            });
        }
        let room = |on_page: usize| {
            parity_room(cells.saturating_sub(self.reserved + self.strings.cells(on_page)))
        };
        let (pages, per_page, rooms) = (fewest_pages..=groups).find_map(|pages| {
            let per_page = groups.div_ceil(pages);
            let last = groups
                .checked_sub((pages - 1) * per_page)
                .filter(|&g| g > 0)?;
            let rooms: Vec<usize> = (0..pages)
                .map(|page| room(if page + 1 == pages { last } else { per_page }))
                .collect();
            (rooms.iter().sum::<usize>() >= self.parity).then_some((pages, per_page, rooms))
        })?;
        let total = if self.fill {
            rooms.iter().sum::<usize>().min(self.max_parity)
        } else {
            self.parity
        };
        let counts = share(total, &rooms);
        let mut first = 0;
        let pages = (0..pages)
            .map(|page| {
                let on_page = (groups - page * per_page).min(per_page);
                let used = self.reserved
                    + self.strings.cells(on_page)
                    + behind_strings(0, counts[page]).len();
                let share = PageShare {
                    groups: on_page,
                    parity_first: first,
                    parity_count: counts[page],
                    rows: if self.fill {
                        full_rows
                    } else {
                        rows_for(used, columns, full_rows)
                    },
                };
                first += counts[page];
                share
            })
            .collect();
        Some(Pagination {
            groups_per_page: per_page,
            pages,
            parity: total,
        })
    }
}

fn rows_for(cells: usize, columns: usize, full_rows: usize) -> usize {
    cells.div_ceil(columns).max(MIN_ROWS).min(full_rows)
}

/// `total` shared as evenly as the `rooms` allow; what a page cannot take goes to the others.
fn share(total: usize, rooms: &[usize]) -> Vec<usize> {
    let pages = rooms.len();
    let mut counts: Vec<usize> = rooms
        .iter()
        .enumerate()
        .map(|(page, &room)| (total / pages + usize::from(page < total % pages)).min(room))
        .collect();
    let mut left = total - counts.iter().sum::<usize>();
    for (count, &room) in counts.iter_mut().zip(rooms) {
        let more = left.min(room - *count);
        *count += more;
        left -= more;
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy() -> Strings {
        Strings::new(Some(Redundancy::default()))
    }

    #[test]
    fn every_block_has_a_cell_and_the_cells_left_over_are_spare() {
        let (columns, groups) = (12usize, 9usize);
        let rows = ((groups + 1) * 6 + 1).div_ceil(columns);
        let slots = plan_page(0, groups, legacy(), (columns, rows), 0, &[]);
        assert_eq!(slots.len(), columns * rows);
        let count = |wanted: fn(&Slot) -> bool| slots.iter().filter(|s| wanted(s)).count();
        assert_eq!(count(|s| matches!(s, Slot::Label)), 6);
        assert_eq!(count(|s| matches!(s, Slot::Data(_))), groups * 5);
        assert_eq!(count(|s| matches!(s, Slot::Recovery(_))), groups);
    }

    #[test]
    fn blocks_of_a_group_never_share_a_column() {
        let (columns, groups) = (12usize, 9usize);
        let slots = plan_page(0, groups, legacy(), (columns, 6), 0, &[]);
        for group in 0..groups {
            let start = (group * 5 * DATA_LEN) as u32;
            let mut seen = std::collections::HashSet::new();
            for (cell, slot) in slots.iter().enumerate() {
                let in_group = match *slot {
                    Slot::Data(offset) => offset >= start && offset < start + 5 * 90,
                    Slot::Recovery(group_start) => group_start == start,
                    _ => false,
                };
                if in_group {
                    assert!(seen.insert(cell % columns), "group {group} shares a column");
                }
            }
        }
    }

    #[test]
    fn leaving_cells_at_the_start_moves_every_block_by_as_many() {
        let plain = plan_page(0, 9, legacy(), (12, 7), 0, &[]);
        let moved = plan_page(0, 9, legacy(), (12, 7), RECORD_CELLS_PER_END, &[]);
        assert!(
            moved[..RECORD_CELLS_PER_END]
                .iter()
                .all(|s| *s == Slot::Spare)
        );
        assert_eq!(
            &plain[..plain.len() - RECORD_CELLS_PER_END],
            &moved[RECORD_CELLS_PER_END..]
        );
    }

    #[test]
    fn a_page_without_recovery_blocks_has_only_data_strings() {
        let strings = Strings::new(None);
        let slots = plan_page(0, 4, strings, (12, 6), 0, &[]);
        let count = |wanted: fn(&Slot) -> bool| slots.iter().filter(|s| wanted(s)).count();
        assert_eq!(count(|s| matches!(s, Slot::Label)), PLAIN_DATA_STRINGS);
        assert_eq!(
            count(|s| matches!(s, Slot::Data(_))),
            4 * PLAIN_DATA_STRINGS
        );
        assert_eq!(count(|s| matches!(s, Slot::Recovery(_))), 0);
    }

    #[test]
    fn a_cell_names_the_address_of_its_block() {
        assert_eq!(
            Slot::Recovery(450).address(legacy()),
            Some(recovery_address(450, Redundancy::default()))
        );
        assert_eq!(Slot::Data(90).address(legacy()), Some(90));
        assert_eq!(Slot::Parity(3).address(legacy()), Some(4));
        assert_eq!(Slot::Label.address(legacy()), None);
    }

    #[test]
    fn parity_blocks_go_behind_the_strings_with_a_label_every_eighth_cell() {
        let behind = behind_strings(10, 9);
        assert_eq!(behind.len(), 10);
        assert_eq!(behind[0], Slot::Parity(10));
        assert_eq!(behind[7], Slot::Label);
        assert_eq!(behind[9], Slot::Parity(18));
        let slots = plan_page(0, 2, legacy(), (12, 10), 3, &behind);
        assert_eq!(slots[3 + 3 * 6], Slot::Parity(10));
        assert_eq!(slots[3 + 3 * 6 + 10], Slot::Spare);
    }

    fn demand(blocks: usize, parity: usize, fill: bool) -> Demand {
        Demand {
            blocks,
            strings: legacy(),
            size: (12, 10),
            reserved: RECORD_CELLS,
            parity,
            max_parity: 1_000_000,
            fill,
        }
    }

    #[test]
    fn without_parity_pages_fill_up_one_after_the_other() {
        // 120 cells: 6 records, 1 spare, 6 strings of 18 cells hold 17 groups.
        let plan = demand(20 * 5, 0, false).paginate().unwrap();
        assert_eq!(plan.groups_per_page, 17);
        assert_eq!(plan.pages.len(), 2);
        assert_eq!(plan.pages[1].groups, 3);
        assert!(plan.pages[1].rows < 10);
    }

    #[test]
    fn with_parity_the_groups_and_the_parity_are_spread_evenly() {
        let plan = demand(20 * 5, 30, false).paginate().unwrap();
        assert_eq!(plan.pages.len(), 2);
        assert_eq!(plan.groups_per_page, 10);
        assert_eq!(plan.pages[0].parity_count, 15);
        assert_eq!(plan.pages[1].parity_first, 15);
        assert_eq!(plan.parity, 30);
    }

    #[test]
    fn parity_that_does_not_fit_takes_another_page() {
        // One page of 17 groups leaves 6 free cells; two pages leave plenty.
        let plan = demand(17 * 5, 30, false).paginate().unwrap();
        assert_eq!(plan.pages.len(), 2);
    }

    #[test]
    fn filled_pages_use_every_free_cell_for_parity() {
        let plan = demand(23 * 5, 0, true).paginate().unwrap();
        assert_eq!(plan.pages.len(), 2);
        assert_eq!(plan.groups_per_page, 12);
        let free = |groups: usize| {
            let free = 120 - RECORD_CELLS - 6 * (groups + 1);
            free - free / LABEL_EVERY
        };
        assert_eq!(plan.pages[0].parity_count, free(12));
        assert_eq!(plan.pages[1].parity_count, free(11));
        assert!(plan.pages.iter().all(|page| page.rows == 10));
    }

    #[test]
    fn a_page_has_room_exactly_when_it_can_be_laid_out() {
        // Once a page of 2 * 3 + 6 cells passed for one with room for the records: the cell that
        // stays spare was left out, and the file failed to encode instead of going without them.
        for strings in [
            legacy(),
            Strings::new(Redundancy::new(2)),
            Strings::new(None),
        ] {
            for cells in 0..40 {
                for reserved in [0, RECORD_CELLS] {
                    let demand = Demand {
                        blocks: 1,
                        strings,
                        size: (cells, 1),
                        reserved,
                        parity: 0,
                        max_parity: 0,
                        fill: false,
                    };
                    assert_eq!(
                        has_room(cells, strings, reserved),
                        demand.paginate().is_some(),
                        "{cells} cells, {reserved} reserved, {} strings",
                        strings.count()
                    );
                }
            }
        }
    }

    #[test]
    fn shares_follow_the_room_of_each_page() {
        assert_eq!(share(10, &[3, 10, 10]), vec![3, 4, 3]);
        assert_eq!(share(7, &[2, 2, 10]), vec![2, 2, 3]);
    }
}
