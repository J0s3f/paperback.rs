// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

//! What paperback.rs adds to its pages beyond PaperBack 1.10: a hash of the file and an
//! identifier and layout for every sheet. The originals ignore both; `tools/interop` checks that
//! against the real programs.

// Tests may unwrap: a failure should stop the test with the error.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use paperback_rs::Integrity;
use paperback_rs::codec::Compression;
use paperback_rs::decode::{DecodeOptions, decode};
use paperback_rs::encode::{EncodeOptions, encode};
use paperback_rs::layout::PageSetup;
use paperback_rs::raster::Raster;

fn setup() -> PageSetup {
    PageSetup {
        printer_dpi: 300,
        dot_dpi: 100,
        ..PageSetup::default()
    }
}

fn options(extensions: bool) -> EncodeOptions {
    EncodeOptions {
        setup: setup(),
        compression: Compression::None,
        extensions,
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

fn quarter_turned(raster: &Raster) -> Raster {
    let (w, h) = (raster.width(), raster.height());
    let mut pixels = Vec::with_capacity(w * h);
    for x in 0..w {
        for y in (0..h).rev() {
            pixels.push(raster.row(y)[x]);
        }
    }
    Raster::from_pixels(h, w, pixels).unwrap()
}

#[test]
fn a_restored_file_is_checked_against_the_hash_on_the_pages() {
    let data = sample(3000);
    let pages = rasters(&data, &options(true));
    let restored = decode(&pages, &DecodeOptions::default(), |_| {}).unwrap();
    assert_eq!(restored.data, data);
    assert_eq!(restored.report.integrity, Integrity::Verified);
    assert_eq!(restored.report.misplaced_blocks, 0);
}

#[test]
fn pages_without_extensions_are_read_as_before() {
    let data = sample(3000);
    let pages = rasters(&data, &options(false));
    let restored = decode(&pages, &DecodeOptions::default(), |_| {}).unwrap();
    assert_eq!(restored.data, data);
    assert_eq!(restored.report.integrity, Integrity::Unchecked);
    assert_eq!(restored.report.sheets, []);
}

#[test]
fn every_sheet_of_a_file_has_an_identifier_of_its_own() {
    let options = options(true);
    let data = sample(130_000);
    let pages = rasters(&data, &options);
    assert!(pages.len() >= 3, "{} pages", pages.len());
    let restored = decode(&pages, &DecodeOptions::default(), |_| {}).unwrap();
    assert_eq!(restored.data, data);
    assert_eq!(restored.report.integrity, Integrity::Verified);
    let mut ids = restored.report.sheets.clone();
    ids.sort_by_key(|id| id.0);
    ids.dedup();
    assert_eq!(ids.len(), pages.len());
    // The same page printed again is the same sheet.
    let again = rasters(&data, &options);
    let mut seen = Vec::new();
    decode(&again, &DecodeOptions::default(), |outcome| {
        seen.push(outcome.result.unwrap().sheet_id.unwrap());
    })
    .unwrap();
    assert_eq!(seen, restored.report.sheets);
}

#[test]
fn the_layout_on_a_page_holds_when_the_picture_is_turned() {
    let data = sample(3000);
    let pages: Vec<Raster> = rasters(&data, &options(true))
        .iter()
        .map(quarter_turned)
        .collect();
    let restored = decode(&pages, &DecodeOptions::default(), |_| {}).unwrap();
    assert_eq!(restored.data, data);
    assert_eq!(restored.report.integrity, Integrity::Verified);
    assert_eq!(restored.report.misplaced_blocks, 0);
}

#[test]
fn a_page_with_extensions_costs_no_data_blocks() {
    let with = rasters(&sample(3000), &options(true));
    let without = rasters(&sample(3000), &options(false));
    assert_eq!(with.len(), without.len());
}

#[test]
fn a_hash_can_be_required_and_pages_without_one_are_then_refused() {
    let data = sample(3000);
    let strict = DecodeOptions {
        require_hash: true,
        ..DecodeOptions::default()
    };
    let with = rasters(&data, &options(true));
    assert_eq!(decode(&with, &strict, |_| {}).unwrap().data, data);
    // Pages whose hash was never written look like pages whose hash was taken off.
    let without = rasters(&data, &options(false));
    assert!(matches!(
        decode(&without, &strict, |_| {}),
        Err(paperback_rs::Error::NotVerified)
    ));
    assert_eq!(
        decode(&without, &DecodeOptions::default(), |_| {})
            .unwrap()
            .data,
        data
    );
}

#[test]
fn an_encrypted_file_is_checked_with_the_password() {
    let data = sample(3000);
    let encrypting = EncodeOptions {
        password: Some("hunter2".into()),
        ..options(true)
    };
    let pages = rasters(&data, &encrypting);
    let with_password = DecodeOptions {
        password: Some("hunter2".into()),
        ..DecodeOptions::default()
    };
    let restored = decode(&pages, &with_password, |_| {}).unwrap();
    assert_eq!(restored.data, data);
    assert_eq!(restored.report.integrity, Integrity::Verified);
    let wrong = DecodeOptions {
        password: Some("hunter3".into()),
        ..DecodeOptions::default()
    };
    assert!(decode(&pages, &wrong, |_| {}).is_err());
}
