// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

//! Pages of every profile with one column of cells wiped white on the first page (see
//! `fixtures/damaged/README.md`). The file must be restored, and the first page must say that
//! exactly the wiped cells did not read, and that no byte needed correcting.

// Tests may unwrap: a failure should stop the test with the error.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use paperback_rs::decode::{DecodeOptions, PageStatistics, decode};
use paperback_rs::imageio::read_pages;

/// The data of every case: the same bytes the pages were made from.
fn data() -> Vec<u8> {
    (0..90_000usize).map(|i| (i * 131 + i / 3) as u8).collect()
}

/// What the first page of a case says about itself: its statistics and the cells its quality
/// shows as unreadable. `None` when the pictures are not there: the packaged crate leaves them
/// out (crates.io limits the size), and the tests skip them then.
fn first_page(case: &str, pages: usize) -> Option<(PageStatistics, usize)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/damaged");
    let mut rasters = Vec::new();
    for page in 1..=pages {
        let Ok(bytes) = std::fs::read(dir.join(format!("{case}-{page}.png"))) else {
            eprintln!("{case}: no picture; skipped");
            return None;
        };
        rasters.extend(read_pages(&bytes).unwrap());
    }
    let options = DecodeOptions {
        quality: true,
        ..DecodeOptions::default()
    };
    let mut first = None;
    let file = decode(&rasters, &options, |page| {
        if page.index == 0 {
            let statistics = page.result.clone().unwrap();
            first = Some((statistics, page.quality.unwrap().unreadable_blocks()));
        }
    })
    .unwrap();
    assert_eq!(file.data, data(), "{case} restored wrongly");
    first
}

/// The wiped column holds one cell of every row: 44 rows on a full page.
const FULL_PAGE_ROWS: usize = 44;

#[test]
fn a_wiped_column_is_rebuilt_and_shown_as_it_is_in_every_profile() {
    for (case, pages, rows) in [
        ("legacy", 1, FULL_PAGE_ROWS),
        ("balanced", 1, FULL_PAGE_ROWS),
        // Two pages that share out the file evenly, each shorter than a full one.
        ("parity", 2, 27),
        ("modern", 2, FULL_PAGE_ROWS),
    ] {
        let Some((statistics, unreadable)) = first_page(case, pages) else {
            continue;
        };
        assert_eq!(
            statistics.bad_blocks, rows,
            "{case}: blocks counted unreadable"
        );
        assert_eq!(unreadable, rows, "{case}: cells shown unreadable");
        assert_eq!(
            statistics.restored_bytes, 0,
            "{case}: the other cells are as printed"
        );
    }
}

#[test]
fn a_column_wiped_with_its_grid_lines_is_shown_unreadable() {
    let Some((statistics, unreadable)) = first_page("legacy-lines", 1) else {
        return;
    };
    assert_eq!(statistics.bad_blocks, FULL_PAGE_ROWS);
    assert_eq!(unreadable, FULL_PAGE_ROWS);
}
