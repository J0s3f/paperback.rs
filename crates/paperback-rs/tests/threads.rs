// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

//! What is read from a page must not depend on how many threads read it: blocks that do not read at
//! once are tried in several ways side by side, and the result has to be the one that trying them
//! one after the other gives.

// Tests may unwrap: a failure should stop the test with the error.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use paperback_rs::Error;
use paperback_rs::decode::{DecodeOptions, PageStatistics, decode};
use paperback_rs::imageio::read_pages;

/// What a decode of one picture gave: the file or how much of it, and everything that was counted.
#[derive(Debug, PartialEq)]
struct Reading {
    restored: Result<Vec<u8>, usize>,
    statistics: Option<PageStatistics>,
}

fn read_with(threads: usize, name: &str) -> Option<Reading> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/synthetic")
        .join(format!("{name}.png"));
    // The packaged crate leaves the pictures out; the test is skipped then.
    let bytes = std::fs::read(path).ok()?;
    let pages = read_pages(&bytes).unwrap();
    let mut statistics = None;
    let options = DecodeOptions {
        threads,
        ..DecodeOptions::default()
    };
    let result = decode(&pages, &options, |outcome| {
        statistics = outcome.result.ok();
    });
    let restored = match result {
        Ok(file) => Ok(file.data),
        Err(Error::Incomplete { recovered, .. }) => Err(recovered),
        Err(other) => panic!("unexpected: {other}"),
    };
    Some(Reading {
        restored,
        statistics,
    })
}

/// Reads the picture with each of the thread counts and requires what one thread gives.
fn same_with_threads(name: &str, thread_counts: &[usize]) {
    let Some(one) = read_with(1, name) else {
        eprintln!("{name}: no picture; skipped");
        return;
    };
    // A reading that found nothing would agree with any other that finds nothing.
    assert!(one.statistics.is_some_and(|s| s.good_blocks > 0), "{name}");
    for &threads in thread_counts {
        assert_eq!(
            read_with(threads, name).unwrap(),
            one,
            "{name} with {threads} threads"
        );
    }
}

#[test]
fn the_same_blocks_are_read_with_any_number_of_threads() {
    for name in ["bent12", "slant15"] {
        same_with_threads(name, &[3, 0]);
    }
}

/// The picture on which reading ahead once gave one block less, and only with 16 threads: more
/// threads than most runners have read further ahead.
#[test]
fn the_same_blocks_are_read_with_sixteen_threads() {
    same_with_threads("bent20", &[16]);
}
