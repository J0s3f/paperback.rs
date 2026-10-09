// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

//! Pages bent or photographed at an angle on purpose (see `fixtures/synthetic/README.md`).
//! Each must be restored completely, or, for the hardest, to a minimum number of blocks.

// Tests may unwrap: a failure should stop the test with the error.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use paperback_rs::Error;
use paperback_rs::decode::{DecodeOptions, decode};
use paperback_rs::imageio::read_pages;

/// The data of every page: the same bytes the pages were made from.
fn data() -> Vec<u8> {
    (0..9_000usize).map(|i| (i * 131 + i / 3) as u8).collect()
}

/// The restored data of a fixture, or `None` if the page is not there: the packaged crate leaves
/// the pictures out (crates.io limits the size), and the tests skip them then.
fn decode_fixture(name: &str) -> Option<Result<Vec<u8>, Error>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/synthetic")
        .join(format!("{name}.png"));
    let Ok(bytes) = std::fs::read(path) else {
        eprintln!("{name}: no picture; skipped");
        return None;
    };
    let pages = read_pages(&bytes).unwrap();
    Some(decode(&pages, &DecodeOptions::default(), |_| {}).map(|file| file.data))
}

/// Data blocks of a page that must be recovered at least, out of 101 (the label is not counted).
fn assert_restored_completely(name: &str) {
    match decode_fixture(name) {
        Some(Ok(restored)) => assert_eq!(restored, data(), "{name} restored wrongly"),
        Some(Err(error)) => panic!("{name} is not restored completely: {error}"),
        None => {}
    }
}

#[test]
fn smoothly_bent_pages_are_restored() {
    for name in ["bent8", "bent12", "bent16"] {
        assert_restored_completely(name);
    }
}

#[test]
fn pages_photographed_at_an_angle_are_restored() {
    for name in ["slant10", "slant15", "slant25"] {
        assert_restored_completely(name);
    }
}

#[test]
fn the_hardest_bent_page_gives_most_of_its_blocks() {
    // 92 of 101 blocks at the time of writing; the rest is lost to bending inside blocks.
    const AT_LEAST: usize = 85;
    match decode_fixture("bent20") {
        Some(Ok(restored)) => assert_eq!(restored, data()),
        Some(Err(Error::Incomplete { recovered, .. })) => {
            assert!(recovered >= AT_LEAST, "only {recovered} blocks");
        }
        Some(Err(error)) => panic!("unexpected: {error}"),
        None => {}
    }
}
