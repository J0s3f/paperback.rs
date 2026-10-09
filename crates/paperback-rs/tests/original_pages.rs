// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

//! Pages written by the original programs (PaperBack 1.00 and 1.10), stored in
//! `tests/fixtures/original`, must decode here. The fixtures were made with
//! `tools/interop/make-fixtures.ps1` from neutral input, so these tests need no
//! original executable and run anywhere.

// Tests may unwrap: a failure should stop the test with the error.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use paperback_rs::decode::{DecodeOptions, RestoredFile, decode};
use paperback_rs::imageio::read_pages;
use paperback_rs::{Error, Result};

const SAMPLE_NAME: &str = "sample.txt";
const PATTERN_NAME: &str = "pattern.bin";
/// 2024-01-02 03:04:05 UTC, the modification time given to the input files.
const FIXED_TIME: u64 = 1_704_164_645;
const PASSWORD: &str = "correct horse";
const UMLAUT_PASSWORD: &str = "pässwört";

/// Same bytes as `make-fixtures.ps1` writes to sample.txt.
fn sample_text() -> Vec<u8> {
    "Lorem ipsum paper test line\r\n".repeat(400).into_bytes()
}

/// Same bytes as `make-fixtures.ps1` writes to pattern.bin.
fn pattern_bytes() -> Vec<u8> {
    (0..120_000usize).map(|i| (i * 131 + i / 3) as u8).collect()
}

fn fixture_pages(version: &str, files: &[&str]) -> Vec<paperback_rs::raster::Raster> {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/original")
        .join(version);
    files
        .iter()
        .flat_map(|file| {
            let bytes = std::fs::read(directory.join(file)).expect("fixture exists");
            read_pages(&bytes).expect("fixture is an image")
        })
        .collect()
}

fn restore(version: &str, files: &[&str], password: Option<&str>) -> Result<RestoredFile> {
    let options = DecodeOptions {
        password: password.map(str::to_owned),
        ..DecodeOptions::default()
    };
    decode(&fixture_pages(version, files), &options, |_| {})
}

fn assert_restored(file: &RestoredFile, data: &[u8], name: &str) {
    assert_eq!(file.data, data);
    assert_eq!(file.name, name);
    assert_eq!(
        file.modified
            .and_then(paperback_rs::block::FileTime::to_unix_seconds),
        Some(FIXED_TIME)
    );
}

const VERSIONS: [&str; 2] = ["v100", "v110"];

#[test]
fn compressed_text_written_by_the_original_is_restored() {
    for version in VERSIONS {
        let file = restore(version, &["text-1.bmp"], None).unwrap();
        assert_restored(&file, &sample_text(), SAMPLE_NAME);
        assert_eq!(file.report.bad_blocks, 0, "{version}");
    }
}

#[test]
fn a_two_page_file_written_by_the_original_is_restored_in_any_page_order() {
    for version in VERSIONS {
        for order in [
            ["pattern-1.png", "pattern-2.png"],
            ["pattern-2.png", "pattern-1.png"],
        ] {
            let file = restore(version, &order, None).unwrap();
            assert_restored(&file, &pattern_bytes(), PATTERN_NAME);
            assert_eq!(file.report.pages_read, 2);
        }
    }
}

#[test]
fn a_missing_page_is_reported_by_number() {
    let error = restore("v110", &["pattern-2.png"], None).unwrap_err();
    match error {
        Error::Incomplete { pages, .. } => assert_eq!(pages, "1"),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn encrypted_pages_of_both_original_versions_open_with_the_password() {
    for version in VERSIONS {
        let file = restore(version, &["text-crypt-1.png"], Some(PASSWORD)).unwrap();
        assert_restored(&file, &sample_text(), SAMPLE_NAME);
    }
}

#[test]
fn passwords_with_umlauts_match_the_originals_ansi_bytes() {
    for version in VERSIONS {
        let file = restore(version, &["text-umlaut-1.png"], Some(UMLAUT_PASSWORD)).unwrap();
        assert_restored(&file, &sample_text(), SAMPLE_NAME);
    }
}

#[test]
fn encrypted_pages_reject_a_missing_or_wrong_password() {
    for version in VERSIONS {
        assert!(matches!(
            restore(version, &["text-crypt-1.png"], None),
            Err(Error::PasswordRequired)
        ));
        assert!(matches!(
            restore(version, &["text-crypt-1.png"], Some("battery staple")),
            Err(Error::WrongPassword)
        ));
    }
}
