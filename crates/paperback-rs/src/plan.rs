// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider
// Derived from PaperBack 1.10, Copyright (c) 2007 Oleh Yuschuk; see NOTICE.md.

//! Which block sits in which cell of a page.
//!
//! Every string of cells (`group_size` data strings plus one recovery string) starts with a
//! label block. Neighbouring strings are shifted against each other so blocks of one
//! redundancy group never share a column. The encoder fills the plan with blocks; a decoder
//! that knows the layout of a page can compare it with what it read.

use crate::block::{Redundancy, recovery_address};

/// Cells at the start of a page that are left to PBX1 records, and the same number at its end:
/// one copy of each record far from the other. A page without records has none.
pub(crate) const RECORD_CELLS_PER_END: usize = 3;
/// All cells a page gives up for records.
pub(crate) const RECORD_CELLS: usize = 2 * RECORD_CELLS_PER_END;

/// What the cell of a page holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Slot {
    /// A copy of the page label; every string starts with one.
    Label,
    /// The data block at this offset.
    Data(u32),
    /// The recovery block of the group starting at this offset.
    Recovery(u32),
    /// A cell no block needs. The originals fill it with another label.
    Spare,
}

impl Slot {
    /// The address of the block in the cell, if it is a data or recovery block.
    pub(crate) fn address(self, redundancy: Redundancy) -> Option<u32> {
        match self {
            Self::Data(offset) => Some(offset),
            Self::Recovery(group_start) => Some(recovery_address(group_start, redundancy)),
            Self::Label | Self::Spare => None,
        }
    }
}

/// The plan of a page of `columns` by `rows` cells that holds `groups` groups of blocks,
/// the first one starting at byte `first_offset` of the file. Cells run left to right and
/// wrap to the next row. The blocks start `head` cells in, which are left spare; moving
/// them all by the same number of cells keeps the blocks of a group in different columns.
pub(crate) fn plan_page(
    first_offset: u32,
    groups: usize,
    redundancy: Redundancy,
    (columns, rows): (usize, usize),
    head: usize,
) -> Vec<Slot> {
    let group = redundancy.group_size();
    let string_len = groups + 1;
    let wraps = string_len >= columns;
    let rotation = |string: usize| {
        let start = string * string_len;
        (columns / (group + 1) * string + columns - start % columns) % columns
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
    for string in 0..=group {
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
        for string in 0..group {
            slots[head + cell(string, index + 1)] = Slot::Data(offset);
            offset += crate::block::DATA_LEN as u32;
        }
        slots[head + cell(group, index + 1)] = Slot::Recovery(group_start);
    }
    slots
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_block_has_a_cell_and_the_cells_left_over_are_spare() {
        let redundancy = Redundancy::default();
        let (columns, groups) = (12usize, 9usize);
        let rows = ((groups + 1) * 6 + 1).div_ceil(columns);
        let slots = plan_page(0, groups, redundancy, (columns, rows), 0);
        assert_eq!(slots.len(), columns * rows);
        let count = |wanted: fn(&Slot) -> bool| slots.iter().filter(|s| wanted(s)).count();
        assert_eq!(count(|s| matches!(s, Slot::Label)), 6);
        assert_eq!(count(|s| matches!(s, Slot::Data(_))), groups * 5);
        assert_eq!(count(|s| matches!(s, Slot::Recovery(_))), groups);
    }

    #[test]
    fn blocks_of_a_group_never_share_a_column() {
        let redundancy = Redundancy::default();
        let (columns, groups) = (12usize, 9usize);
        let slots = plan_page(0, groups, redundancy, (columns, 6), 0);
        for group in 0..groups {
            let start = (group * 5 * crate::block::DATA_LEN) as u32;
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
        let redundancy = Redundancy::default();
        let plain = plan_page(0, 9, redundancy, (12, 7), 0);
        let moved = plan_page(0, 9, redundancy, (12, 7), RECORD_CELLS_PER_END);
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
    fn a_recovery_cell_names_the_address_with_the_group_size() {
        let redundancy = Redundancy::default();
        assert_eq!(
            Slot::Recovery(450).address(redundancy),
            Some(recovery_address(450, redundancy))
        );
        assert_eq!(Slot::Data(90).address(redundancy), Some(90));
        assert_eq!(Slot::Label.address(redundancy), None);
    }
}
