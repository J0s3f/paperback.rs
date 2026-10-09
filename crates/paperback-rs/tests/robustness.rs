// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

//! Pages that are rotated, mirrored, scaled or tilted must still be read.

// Tests may unwrap: a failure should stop the test with the error.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use paperback_rs::decode::{DecodeOptions, decode};
use paperback_rs::encode::{EncodeOptions, encode};
use paperback_rs::layout::PageSetup;
use paperback_rs::raster::{Raster, WHITE};

const PRINTER_DPI: usize = 300;

fn setup() -> PageSetup {
    PageSetup {
        printer_dpi: PRINTER_DPI,
        dot_dpi: 100,
        ..PageSetup::default()
    }
}

fn sample(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 131 + i / 3) as u8).collect()
}

fn one_page(data: &[u8]) -> (EncodeOptions, Raster) {
    let options = EncodeOptions {
        setup: setup(),
        compression: paperback_rs::codec::Compression::None,
        ..EncodeOptions::default()
    };
    let mut pages = encode(data, &options).unwrap();
    assert_eq!(pages.len(), 1);
    (options, pages.remove(0).raster)
}

fn restore(raster: Raster) -> Result<Vec<u8>, paperback_rs::Error> {
    decode(&[raster], &DecodeOptions::default(), |_| {}).map(|file| file.data)
}

fn scaled_up(raster: &Raster, factor: usize) -> Raster {
    let (w, h) = (raster.width() * factor, raster.height() * factor);
    let mut pixels = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            pixels.push(raster.row(y / factor)[x / factor]);
        }
    }
    Raster::from_pixels(w, h, pixels).unwrap()
}

fn rotated_quarter_turn(raster: &Raster) -> Raster {
    let (w, h) = (raster.width(), raster.height());
    let mut pixels = Vec::with_capacity(w * h);
    for y in 0..w {
        for x in 0..h {
            pixels.push(raster.row(h - 1 - x)[y]);
        }
    }
    Raster::from_pixels(h, w, pixels).unwrap()
}

fn mirrored(raster: &Raster) -> Raster {
    let rows: Vec<u8> = (0..raster.height())
        .flat_map(|y| raster.row(y).iter().rev().copied())
        .collect();
    Raster::from_pixels(raster.width(), raster.height(), rows).unwrap()
}

#[test]
fn upside_down_and_mirrored_pages_are_read() {
    let data = sample(2500);
    let (_, raster) = one_page(&data);
    let flipped = raster.flipped_vertically();
    assert_eq!(restore(flipped.clone()).unwrap(), data);
    assert_eq!(restore(mirrored(&raster)).unwrap(), data);
    assert_eq!(restore(mirrored(&flipped)).unwrap(), data);
}

#[test]
fn quarter_turned_pages_are_read() {
    let data = sample(2500);
    let (_, raster) = one_page(&data);
    let once = rotated_quarter_turn(&raster);
    assert_eq!(restore(once.clone()).unwrap(), data);
    assert_eq!(restore(rotated_quarter_turn(&once)).unwrap(), data);
}

#[test]
fn a_page_scanned_at_twice_the_resolution_is_read() {
    let data = sample(2500);
    let (_, raster) = one_page(&data);
    assert_eq!(restore(scaled_up(&raster, 2)).unwrap(), data);
}

/// The grid search alone copes with about five degrees; the page is
/// straightened first when it does not read as it is.
#[test]
fn tilted_pages_are_read() {
    let data = sample(30_000);
    let (_, raster) = one_page(&data);
    let big = scaled_up(&raster, 2);
    for degrees in [0.25, 1.5, 3.0, 6.0, 10.0, 20.0, 30.0, 45.0, -12.0, -37.0] {
        assert_eq!(
            restore(big.rotated(degrees, WHITE).unwrap()).unwrap(),
            data,
            "tilt of {degrees} degrees"
        );
    }
}

#[test]
fn quality_pictures_show_damage_and_cover_the_page() {
    let data = sample(2500);
    let (_, raster) = one_page(&data);
    let (width, height) = (raster.width(), raster.height());
    let mut pixels = raster.into_pixels();
    // Blacken a patch in the middle of the grid, more than error correction can repair.
    for y in height / 2 - 20..height / 2 + 20 {
        pixels[y * width + width / 2 - 20..y * width + width / 2 + 20].fill(0);
    }
    let damaged = Raster::from_pixels(width, height, pixels).unwrap();
    let options = DecodeOptions {
        quality: true,
        ..DecodeOptions::default()
    };
    let mut quality = None;
    let _ = decode(&[damaged], &options, |outcome| quality = outcome.quality);
    let quality = quality.expect("the page was read");
    assert!(quality.unreadable_blocks() >= 1);
    for picture in [quality.map(), quality.overlay()] {
        assert_eq!((picture.width(), picture.height()), (width, height));
        let mostly_green = picture
            .rgb()
            .chunks(3)
            .filter(|p| p[1] > p[0] && p[1] > p[2])
            .count();
        assert!(mostly_green > width * height / 4, "green blocks expected");
        assert!(
            picture
                .rgb()
                .chunks(3)
                .any(|p| u16::from(p[0]) > u16::from(p[1]) + 40),
            "a red block expected"
        );
    }
}

/// Moves every pixel by a smooth, position-dependent amount, like paper that is bent or
/// photographed from the side. The picture is resampled bilinearly, as a real scan is.
fn bent(raster: &Raster, amplitude: f64) -> Raster {
    let (w, h) = (raster.width(), raster.height());
    let at = |x: usize, y: usize| f64::from(raster.row(y)[x]);
    let mut pixels = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let (u, v) = (x as f64 / w as f64, y as f64 / h as f64);
            let tau = std::f64::consts::TAU;
            let dx = amplitude * (tau * (1.4 * v + 0.2)).sin() * (0.3 + u);
            let dy = amplitude * (tau * (1.1 * u + 0.4)).sin() * (0.3 + v);
            let (sx, sy) = (x as f64 + dx, y as f64 + dy);
            let value = if sx < 0.0 || sy < 0.0 || sx >= (w - 1) as f64 || sy >= (h - 1) as f64 {
                f64::from(WHITE)
            } else {
                let (x0, y0) = (sx as usize, sy as usize);
                let (fx, fy) = (sx - x0 as f64, sy - y0 as f64);
                let top = at(x0, y0) * (1.0 - fx) + at(x0 + 1, y0) * fx;
                let bottom = at(x0, y0 + 1) * (1.0 - fx) + at(x0 + 1, y0 + 1) * fx;
                top * (1.0 - fy) + bottom * fy
            };
            pixels.push(value.round() as u8);
        }
    }
    Raster::from_pixels(w, h, pixels).unwrap()
}

/// The page as a camera sees it from below: the upper edge is farther away, so it looks
/// shorter and the lines near it are closer together. `farness` is how much farther the
/// upper edge is than the lower one (0 is a straight-on view).
fn foreshortened(raster: &Raster, farness: f64) -> Raster {
    let (w, h) = (raster.width(), raster.height());
    let at = |x: usize, y: usize| f64::from(raster.row(y)[x]);
    let mut pixels = Vec::with_capacity(w * h);
    for y in 0..h {
        let v = y as f64 / h as f64;
        // The page row that lands on this picture row (the inverse of a projective mapping).
        let page_v = v * (1.0 + farness) / (1.0 + farness * v);
        let width_scale = 1.0 + farness * (1.0 - page_v);
        for x in 0..w {
            let u = x as f64 / w as f64 - 0.5;
            let (sx, sy) = ((u * width_scale + 0.5) * w as f64, page_v * h as f64);
            let value = if sx < 0.0 || sy < 0.0 || sx >= (w - 1) as f64 || sy >= (h - 1) as f64 {
                f64::from(WHITE)
            } else {
                let (x0, y0) = (sx as usize, sy as usize);
                let (fx, fy) = (sx - x0 as f64, sy - y0 as f64);
                let top = at(x0, y0) * (1.0 - fx) + at(x0 + 1, y0) * fx;
                let bottom = at(x0, y0 + 1) * (1.0 - fx) + at(x0 + 1, y0 + 1) * fx;
                top * (1.0 - fy) + bottom * fy
            };
            pixels.push(value.round() as u8);
        }
    }
    Raster::from_pixels(w, h, pixels).unwrap()
}

#[test]
fn pages_bent_by_a_tenth_of_a_block_are_read() {
    let data = sample(9_000);
    let (_, page) = one_page(&data);
    // A block is about 105 pixels wide at this resolution; the bend moves dots by up to 8 of them.
    let curved = bent(&page, 8.0);
    assert_eq!(restore(curved).unwrap(), data);
}

#[test]
fn pages_photographed_at_an_angle_are_read() {
    let data = sample(9_000);
    let (_, page) = one_page(&data);
    let slanted = foreshortened(&page, 0.10);
    assert_eq!(restore(slanted).unwrap(), data);
}

#[test]
fn pictures_of_one_sheet_complete_each_other_and_reading_stops_when_the_file_is_complete() {
    let data = sample(9_000);
    let (_, page) = one_page(&data);
    let (width, height) = (page.width(), page.height());
    let third = width / 3;
    // The first picture lacks the right third of the sheet, the second the left third (and with
    // it the labels there) and is turned a quarter, so its cells are numbered another way.
    let mut first = page.clone();
    first.fill_rect(width - third, 0, third, height, WHITE);
    let mut second = page.clone();
    second.fill_rect(0, 0, third, height, WHITE);
    let second = rotated_quarter_turn(&second);

    let mut alone = None;
    let _ = decode(
        std::slice::from_ref(&second),
        &DecodeOptions::default(),
        |outcome| alone = outcome.result.ok().map(|stats| stats.good_blocks),
    );

    let mut outcomes = Vec::new();
    let restored = decode(
        &[first, second, page.clone()],
        &DecodeOptions::default(),
        |outcome| outcomes.push(outcome),
    )
    .unwrap();
    assert_eq!(restored.data, data);

    // The second picture was read only where the first had no block: fewer than alone.
    let in_second = outcomes[1].result.as_ref().unwrap().good_blocks;
    assert!(
        in_second < alone.unwrap(),
        "{in_second} blocks read after the first picture, {alone:?} alone"
    );
    // The third picture would restore the file by itself and was not read: the file was complete.
    assert!(outcomes[2].result.is_err());
}
