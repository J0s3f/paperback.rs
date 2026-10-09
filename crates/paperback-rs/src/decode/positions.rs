// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

//! Checks the blocks read from a page against the layout the page announces.
//!
//! A page written by paperback.rs carries a record with its number of columns and rows and its
//! group size. With those and the label, the cell of every block can be worked out the way the
//! encoder placed it. A block read in another cell, however it came to pass the block checksum
//! and error correction, is not trusted.

use std::collections::HashSet;

use super::assemble::ScannedPage;
use super::sheet::{BlockKey, check_against_plan};
use crate::block::{DATA_LEN, Redundancy, SuperBlock};
use crate::pbx::{self, Record, SheetRecord};
use crate::plan::{RECORD_CELLS, RECORD_CELLS_PER_END, Slot, plan_page};

/// Cells a page may have at most; the numbers on the page are not trusted further.
const MAX_CELLS: usize = 1 << 20;

/// Takes the records out of the page's blocks, notes which sheet the page is, and drops the
/// blocks that were read in the wrong cell.
pub(crate) fn examine(page: &mut ScannedPage) {
    page.records = pbx::with_missing_rebuilt(take_records(page));
    let Some(sheet) = page.records.iter().find_map(|record| match record {
        Record::Sheet(sheet) if sheet.page == page.label.page => Some(*sheet),
        _ => None,
    }) else {
        return;
    };
    page.statistics.sheet_id = Some(sheet.id);
    let Some(plan) = expected_keys(&page.label, &sheet) else {
        return;
    };
    let size = (usize::from(sheet.columns), usize::from(sheet.rows));
    let Some(check) = check_against_plan(&page.lattice, &plan, size, &page.label) else {
        return;
    };
    let wrong: HashSet<BlockKey> = check
        .misplaced
        .iter()
        .filter_map(|&at| page.lattice.keys[at])
        .filter(|key| !check.confirmed.contains(key))
        .collect();
    for &at in &check.misplaced {
        page.lattice.keys[at] = None;
    }
    let before = page.blocks.len();
    page.blocks
        .retain(|block| !wrong.contains(&block.identity()));
    let dropped = before - page.blocks.len();
    page.statistics.misplaced_blocks = dropped;
    page.statistics.good_blocks = page.statistics.good_blocks.saturating_sub(dropped);
}

/// The records among the blocks of a page, which are removed from them. A record is a block
/// behind the end of the data, where the originals take no block.
fn take_records(page: &mut ScannedPage) -> Vec<Record> {
    let data_size = page.label.data_size;
    let mut records = Vec::new();
    page.blocks.retain(|block| {
        let record = block.record(data_size);
        records.extend(record);
        record.is_none()
    });
    records
}

/// The block that each cell of the page holds, from the layout the sheet record announces;
/// `None` if the record does not describe a page of this file.
fn expected_keys(label: &SuperBlock, sheet: &SheetRecord) -> Option<Vec<Option<BlockKey>>> {
    let redundancy = Redundancy::new(sheet.group_size)?;
    let group = redundancy.group_size();
    let (columns, rows) = (usize::from(sheet.columns), usize::from(sheet.rows));
    let page_size = label.page_size as usize;
    let first_offset = usize::from(label.page)
        .checked_sub(1)?
        .checked_mul(page_size)?;
    let on_page = (label.data_size as usize)
        .checked_sub(first_offset)?
        .min(page_size);
    let groups = on_page.div_ceil(DATA_LEN).div_ceil(group);
    let cells = columns.checked_mul(rows)?;
    let needed = (groups + 1) * (group + 1) + RECORD_CELLS;
    if columns == 0 || groups == 0 || needed > cells || cells > MAX_CELLS {
        return None;
    }
    let slots = plan_page(
        first_offset as u32,
        groups,
        redundancy,
        (columns, rows),
        RECORD_CELLS_PER_END,
    );
    Some(
        slots
            .into_iter()
            .map(|slot| match slot {
                Slot::Data(offset) => Some((false, offset)),
                Slot::Recovery(group_start) => Some((true, group_start)),
                Slot::Label | Slot::Spare => None,
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{FileTime, Mode, NAME_LEN};
    use crate::pbx::{SHEET_ID_LEN, SheetId};

    fn label(data_size: u32, page_size: u32, page: u16) -> SuperBlock {
        SuperBlock {
            data_size,
            page_size,
            original_size: data_size,
            mode: Mode(0),
            attributes: 0,
            page,
            modified: FileTime::default(),
            file_crc: 0,
            name: [0; NAME_LEN],
        }
    }

    fn sheet(page: u16, columns: u16, rows: u16, group_size: u8) -> SheetRecord {
        SheetRecord {
            id: SheetId([1; SHEET_ID_LEN]),
            page,
            page_count: 2,
            columns,
            rows,
            group_size,
        }
    }

    #[test]
    fn the_plan_of_a_page_names_every_block_of_it() {
        // 4 groups of 5 blocks and their recovery blocks on a page of 12 x 6 cells.
        let page_size = 4 * 5 * DATA_LEN as u32;
        let plan = expected_keys(&label(2 * page_size, page_size, 2), &sheet(2, 12, 6, 5)).unwrap();
        assert_eq!(plan.len(), 72);
        assert_eq!(plan.iter().flatten().filter(|key| key.0).count(), 4);
        assert_eq!(plan.iter().flatten().filter(|key| !key.0).count(), 20);
        // The second page starts where the first ended.
        let first = plan
            .iter()
            .flatten()
            .filter(|key| !key.0)
            .map(|key| key.1)
            .min();
        assert_eq!(first, Some(page_size));
    }

    #[test]
    fn a_record_that_does_not_fit_the_page_gives_no_plan() {
        let page_one = label(1800, 1800, 1);
        assert!(expected_keys(&page_one, &sheet(1, 12, 6, 0)).is_none());
        assert!(expected_keys(&page_one, &sheet(1, 0, 6, 5)).is_none());
        assert!(expected_keys(&page_one, &sheet(1, 3, 3, 5)).is_none());
        assert!(expected_keys(&page_one, &sheet(1, u16::MAX, u16::MAX, 5)).is_none());
        // Page 3 of a file of one page.
        assert!(expected_keys(&label(1800, 1800, 3), &sheet(3, 12, 6, 5)).is_none());
    }
}
