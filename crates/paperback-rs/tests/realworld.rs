// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

//! Real-world decode tests: print the PDFs in `fixtures/realworld/sheets`, scan or photograph them
//! and put the pictures into `fixtures/realworld/scans` as `<case>_<page>.<png|jpg|bmp|pdf>`.
//! Cases without scans are skipped, so the tests pass before any scan exists.
//! The sheets themselves must always decode to the data, which checks them for the next print.

// Tests may unwrap: a failure should stop the test with the error.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use paperback_rs::decode::{DecodeOptions, decode};
use paperback_rs::imageio::read_pages;

const PASSWORD: &str = "paperback-rs-test";
const IMAGE_EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "bmp", "pdf"];

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/realworld")
}

fn cases() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(fixtures().join("data"))
        .unwrap()
        .filter_map(|entry| entry.ok()?.path().file_stem()?.to_str().map(str::to_owned))
        .collect();
    names.sort();
    names
}

fn options_for(case: &str) -> DecodeOptions {
    DecodeOptions {
        password: case.contains("encrypted").then(|| PASSWORD.to_owned()),
        ..DecodeOptions::default()
    }
}

fn restore(case: &str, files: &[PathBuf]) -> Vec<u8> {
    let pages: Vec<_> = files
        .iter()
        .flat_map(|file| read_pages(&std::fs::read(file).unwrap()).unwrap())
        .collect();
    decode(&pages, &options_for(case), |_| {})
        .unwrap_or_else(|error| panic!("{case}: {error}"))
        .data
}

fn scans_of(case: &str) -> Vec<PathBuf> {
    let prefix = format!("{case}_");
    let mut files: Vec<PathBuf> = std::fs::read_dir(fixtures().join("scans"))
        .unwrap()
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            name.starts_with(&prefix)
                && IMAGE_EXTENSIONS.contains(&extension.to_lowercase().as_str())
        })
        .collect();
    files.sort();
    files
}

#[test]
fn every_sheet_decodes_to_its_data() {
    for case in cases() {
        let sheet = fixtures().join("sheets").join(format!("{case}.pdf"));
        let data = std::fs::read(fixtures().join("data").join(format!("{case}.bin"))).unwrap();
        assert_eq!(restore(&case, &[sheet]), data, "{case}");
    }
}

#[test]
fn printed_and_scanned_sheets_decode_to_their_data() {
    let mut checked = 0;
    for case in cases() {
        let files = scans_of(&case);
        if files.is_empty() {
            eprintln!("no scans for {case}; skipped");
            continue;
        }
        let data = std::fs::read(fixtures().join("data").join(format!("{case}.bin"))).unwrap();
        assert_eq!(restore(&case, &files), data, "{case} from {files:?}");
        checked += 1;
    }
    eprintln!("{checked} case(s) checked against scans");
}
