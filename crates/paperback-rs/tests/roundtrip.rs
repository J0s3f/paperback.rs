// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

//! Files survive encoding to page pictures and decoding again.

// Tests may unwrap: a failure should stop the test with the error.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use paperback_rs::codec::Compression;
use paperback_rs::decode::{DecodeOptions, decode};
use paperback_rs::encode::{EncodeOptions, encode};
use paperback_rs::layout::PageSetup;

fn small_setup() -> PageSetup {
    PageSetup {
        printer_dpi: 300,
        dot_dpi: 100,
        ..PageSetup::default()
    }
}

fn sample(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 31 + i / 7) as u8).collect()
}

fn round_trip(data: &[u8], options: &EncodeOptions, password: Option<&str>) -> Vec<u8> {
    let pages = encode(data, options).expect("encode");
    let rasters: Vec<_> = pages.into_iter().map(|p| p.raster).collect();
    let decode_options = DecodeOptions {
        password: password.map(str::to_owned),
        ..DecodeOptions::default()
    };
    decode(&rasters, &decode_options, |_| {})
        .expect("decode")
        .data
}

#[test]
fn small_text_round_trips() {
    let options = EncodeOptions {
        setup: small_setup(),
        name: "hello.txt".into(),
        ..EncodeOptions::default()
    };
    let data = b"Hello, paper world!".to_vec();
    assert_eq!(round_trip(&data, &options, None), data);
}

#[test]
fn uncompressed_binary_round_trips() {
    let options = EncodeOptions {
        setup: small_setup(),
        compression: Compression::None,
        ..EncodeOptions::default()
    };
    let data = sample(5000);
    assert_eq!(round_trip(&data, &options, None), data);
}

#[test]
fn multi_page_file_round_trips_in_any_page_order() {
    let options = EncodeOptions {
        setup: small_setup(),
        compression: Compression::None,
        ..EncodeOptions::default()
    };
    let data = sample(60_000);
    let mut pages: Vec<_> = encode(&data, &options)
        .unwrap()
        .into_iter()
        .map(|p| p.raster)
        .collect();
    assert!(pages.len() > 1, "expected several pages");
    pages.reverse();
    let restored = decode(&pages, &DecodeOptions::default(), |_| {}).unwrap();
    assert_eq!(restored.data, data);
}

#[test]
fn encrypted_file_needs_the_right_password() {
    let options = EncodeOptions {
        setup: small_setup(),
        password: Some("s3cret".into()),
        ..EncodeOptions::default()
    };
    let data = sample(3000);
    assert_eq!(round_trip(&data, &options, Some("s3cret")), data);

    let pages: Vec<_> = encode(&data, &options)
        .unwrap()
        .into_iter()
        .map(|p| p.raster)
        .collect();
    let wrong = DecodeOptions {
        password: Some("other".into()),
        ..DecodeOptions::default()
    };
    assert!(matches!(
        decode(&pages, &wrong, |_| {}),
        Err(paperback_rs::Error::WrongPassword)
    ));
    assert!(matches!(
        decode(&pages, &DecodeOptions::default(), |_| {}),
        Err(paperback_rs::Error::PasswordRequired)
    ));
}
