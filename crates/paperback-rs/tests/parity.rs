// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

//! Parity blocks rebuild lost blocks wherever they were lost, filled pages use the whole paper,
//! and both go with or without PaperBack's recovery blocks.

// Tests may unwrap: a failure should stop the test with the error.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use paperback_rs::block::Redundancy;
use paperback_rs::codec::Compression;
use paperback_rs::decode::{DecodeOptions, RestoredFile, decode};
use paperback_rs::encode::{EncodeOptions, encode};
use paperback_rs::layout::PageSetup;
use paperback_rs::raster::Raster;

fn options(redundancy: Option<u8>, parity_percent: u32, fill_pages: bool) -> EncodeOptions {
    EncodeOptions {
        setup: PageSetup {
            printer_dpi: 300,
            dot_dpi: 100,
            ..PageSetup::default()
        },
        compression: Compression::None,
        redundancy: redundancy.and_then(Redundancy::new),
        parity_percent,
        fill_pages,
        ..EncodeOptions::default()
    }
}

fn sample(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 131 + i / 3) as u8).collect()
}

fn rasters(data: &[u8], options: &EncodeOptions) -> Vec<Raster> {
    encode(data, options)
        .unwrap()
        .into_iter()
        .map(|page| page.raster)
        .collect()
}

fn restore(pages: &[Raster]) -> paperback_rs::Result<RestoredFile> {
    decode(pages, &DecodeOptions::default(), |_| {})
}

/// Bytes a page holds with PaperBack's default layout, found by encoding ever larger files.
fn page_capacity() -> usize {
    let plain = options(Some(5), 0, false);
    (1..=100)
        .map(|n| n * 1000)
        .find(|&len| rasters(&sample(len), &plain).len() > 1)
        .map(|len| len - 1000)
        .unwrap()
}

#[test]
fn a_lost_page_is_rebuilt_from_the_parity_on_the_others() {
    let capacity = page_capacity();
    let data = sample(capacity * 3 / 2);
    let pages = rasters(&data, &options(None, 60, false));
    assert!(pages.len() >= 3, "{} pages", pages.len());
    for lost in 0..pages.len() {
        let rest: Vec<Raster> = pages
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != lost)
            .map(|(_, page)| page.clone())
            .collect();
        let restored = restore(&rest).unwrap();
        assert_eq!(restored.data, data, "page {} lost", lost + 1);
        assert!(restored.report.parity_recovered_blocks > 0);
        assert_eq!(restored.report.misplaced_blocks, 0);
    }
}

#[test]
fn without_enough_parity_a_lost_page_is_reported() {
    let capacity = page_capacity();
    let data = sample(capacity * 3 / 2);
    let pages = rasters(&data, &options(None, 5, false));
    assert!(matches!(
        restore(&pages[1..]),
        Err(paperback_rs::Error::Incomplete { .. })
    ));
}

#[test]
fn a_wiped_band_of_a_page_is_rebuilt() {
    let data = sample(4000);
    let page = rasters(&data, &options(Some(5), 30, true)).remove(0);
    // A band across the upper part of the grid, where the strings are: several blocks of
    // every group are lost, more than PaperBack's recovery blocks can rebuild.
    let band = Raster::from_pixels(page.width(), page.height(), {
        let mut pixels = page.pixels().to_vec();
        let (top, bottom) = (page.height() / 10, page.height() / 5);
        pixels[top * page.width()..bottom * page.width()].fill(255);
        pixels
    })
    .unwrap();
    let restored = restore(&[band]).unwrap();
    assert_eq!(restored.data, data);
    assert!(restored.report.parity_recovered_blocks > 0);
}

#[test]
fn filled_pages_use_the_whole_paper_and_need_no_more_pages() {
    let capacity = page_capacity();
    let data = sample(2 * capacity + 300);
    let full_height = rasters(&data, &options(Some(5), 0, false))[0].height();
    let plain = rasters(&data, &options(Some(10), 10, false));
    let filled = rasters(&data, &options(Some(10), 10, true));
    assert_eq!(filled.len(), plain.len());
    assert!(plain.iter().any(|page| page.height() < full_height));
    assert!(filled.iter().all(|page| page.height() == full_height));
    let restored = restore(&filled).unwrap();
    assert_eq!(restored.data, data);
    assert_eq!(restored.report.misplaced_blocks, 0);
}

#[test]
fn pages_read_back_in_every_mix_of_redundancy_and_parity() {
    let data = sample(5000);
    for (redundancy, parity, fill) in [
        (Some(5), 0, false),
        (Some(5), 0, true),
        (None, 0, false),
        (None, 20, false),
        (None, 20, true),
        (Some(10), 10, false),
        (Some(2), 100, true),
    ] {
        let pages = rasters(&data, &options(redundancy, parity, fill));
        let restored = restore(&pages).unwrap();
        assert_eq!(restored.data, data, "{redundancy:?} {parity} {fill}");
        assert_eq!(restored.report.misplaced_blocks, 0);
    }
}

#[test]
fn filled_pages_read_upside_down_have_no_misplaced_blocks() {
    let data = sample(1500);
    let pages = rasters(&data, &options(None, 20, true));
    assert_eq!(pages.len(), 1);
    let restored = restore(&[pages[0].flipped_vertically()]).unwrap();
    assert_eq!(restored.data, data);
    assert_eq!(restored.report.misplaced_blocks, 0);
}

#[test]
fn parity_works_without_the_records_too() {
    let capacity = page_capacity();
    let data = sample(capacity * 3 / 2);
    let bare = EncodeOptions {
        extensions: false,
        ..options(None, 60, false)
    };
    let pages = rasters(&data, &bare);
    let restored = restore(&pages[1..]).unwrap();
    assert_eq!(restored.data, data);
}

#[test]
fn the_pages_can_insist_on_their_records() {
    let strict = EncodeOptions {
        extensions: false,
        require_extensions: true,
        ..options(None, 20, false)
    };
    assert!(encode(b"x", &strict).is_err());
}

#[test]
fn legacy_pages_carry_nothing_the_originals_do_not_know() {
    let data = sample(5000);
    let legacy = EncodeOptions {
        setup: options(None, 0, false).setup,
        compression: Compression::None,
        ..EncodeOptions::legacy()
    };
    let restored = restore(&rasters(&data, &legacy)).unwrap();
    assert_eq!(restored.data, data);
    assert_eq!(restored.report.parity_blocks, 0);
    assert_eq!(restored.report.sheets, Vec::new());
    assert_eq!(
        restored.report.integrity,
        paperback_rs::Integrity::Unchecked
    );
}

#[test]
fn modern_pages_carry_the_hash_and_no_recovery_blocks() {
    let data = sample(5000);
    let modern = EncodeOptions {
        setup: options(None, 0, false).setup,
        compression: Compression::None,
        ..EncodeOptions::modern()
    };
    let restored = restore(&rasters(&data, &modern)).unwrap();
    assert_eq!(restored.data, data);
    assert!(restored.report.parity_blocks > 0);
    assert_eq!(restored.report.integrity, paperback_rs::Integrity::Verified);
}
