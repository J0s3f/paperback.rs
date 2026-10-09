// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

//! Garbage, truncated and absurd input must produce errors, never panics or
//! huge allocations: scans and PDFs come from outside.

// Tests may unwrap: a failure should stop the test with the error.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use paperback_rs::decode::{DecodeOptions, decode};
use paperback_rs::encode::{EncodeOptions, encode};
use paperback_rs::imageio::{read_pages, write_bmp, write_pdf, write_png};
use paperback_rs::layout::{Margins, PageSetup};
use paperback_rs::raster::Raster;
use pdf_writer::{Filter, Finish, Name, Pdf, Rect, Ref};

/// Small deterministic generator, so failures are reproducible.
struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next() as u8).collect()
    }
}

fn small_page() -> paperback_rs::encode::Page {
    let options = EncodeOptions {
        setup: PageSetup {
            printer_dpi: 300,
            dot_dpi: 100,
            ..PageSetup::default()
        },
        ..EncodeOptions::default()
    };
    encode(b"hostile input test", &options).unwrap().remove(0)
}

#[test]
fn random_bytes_are_not_images() {
    let mut random = XorShift(0x9E37_79B9_7F4A_7C15);
    for len in [0, 1, 7, 100, 4096] {
        let bytes = random.bytes(len);
        assert!(read_pages(&bytes).is_err(), "{len} random bytes");
        let mut pdf = b"%PDF-1.7\n".to_vec();
        pdf.extend(random.bytes(len));
        assert!(
            read_pages(&pdf).is_err(),
            "{len} random bytes after a PDF header"
        );
    }
}

#[test]
fn truncated_files_do_not_panic() {
    let page = small_page();
    for file in [
        write_png(&page).unwrap(),
        write_bmp(&page),
        write_pdf(std::slice::from_ref(&page)),
    ] {
        for cut in [1, 8, 33, 100, file.len() / 2, file.len() - 1] {
            let _ = read_pages(&file[..cut.min(file.len())]);
        }
    }
}

#[test]
fn flipped_bytes_do_not_panic() {
    let page = small_page();
    let original = write_pdf(std::slice::from_ref(&page));
    let mut random = XorShift(42);
    for _ in 0..40 {
        let mut damaged = original.clone();
        for _ in 0..8 {
            let at = random.next() as usize % damaged.len();
            damaged[at] ^= 1 << (random.next() % 8);
        }
        if let Ok(pages) = read_pages(&damaged) {
            let _ = decode(&pages, &DecodeOptions::default(), |_| {});
        }
    }
}

#[test]
fn noise_pictures_are_not_pages() {
    let mut random = XorShift(7);
    for (width, height) in [(1, 1), (3, 200), (100, 100), (400, 300), (700, 90)] {
        let noise = Raster::from_pixels(width, height, random.bytes(width * height)).unwrap();
        let outcome = decode(&[noise], &DecodeOptions::default(), |_| {});
        assert!(outcome.is_err(), "{width}x{height}");
    }
}

#[test]
fn flat_pictures_are_not_pages() {
    for level in [0, 128, 255] {
        let flat = Raster::filled(500, 400, level);
        assert!(decode(&[flat], &DecodeOptions::default(), |_| {}).is_err());
    }
}

fn pdf_with_dictionary(
    setup: impl FnOnce(&mut pdf_writer::writers::ImageXObject),
    data: &[u8],
) -> Vec<u8> {
    let (catalog, tree, page_id, content_id, image_id) = (
        Ref::new(1),
        Ref::new(2),
        Ref::new(3),
        Ref::new(4),
        Ref::new(5),
    );
    let mut pdf = Pdf::new();
    pdf.catalog(catalog).pages(tree);
    pdf.pages(tree).kids([page_id]).count(1);
    let mut page = pdf.page(page_id);
    page.media_box(Rect::new(0.0, 0.0, 100.0, 100.0));
    page.parent(tree);
    page.contents(content_id);
    page.resources().x_objects().pair(Name(b"Im0"), image_id);
    page.finish();
    pdf.stream(content_id, b"q 100 0 0 100 0 0 cm /Im0 Do Q");
    let mut image = pdf.image_xobject(image_id, data);
    setup(&mut image);
    image.finish();
    pdf.finish()
}

#[test]
fn absurd_pdf_image_sizes_are_refused() {
    for (width, height) in [(1_000_000, 1_000_000), (i32::MAX, 2), (0, 5), (-4, 5)] {
        let pdf = pdf_with_dictionary(
            |image| {
                image.width(width).height(height);
                image.color_space().device_gray();
                image.bits_per_component(8);
            },
            &[0; 16],
        );
        assert!(read_pages(&pdf).is_err(), "{width}x{height}");
    }
}

#[test]
fn unusual_bit_depths_are_refused() {
    for bits in [0, 3, 12, 32, -1] {
        let pdf = pdf_with_dictionary(
            |image| {
                image.width(8).height(8);
                image.color_space().device_gray();
                image.bits_per_component(bits);
            },
            &[0; 64],
        );
        assert!(read_pages(&pdf).is_err(), "{bits} bits");
    }
}

#[test]
fn absurd_fax_widths_are_refused() {
    let pdf = pdf_with_dictionary(
        |image| {
            image.filter(Filter::CcittFaxDecode);
            image.width(8).height(8);
            image.color_space().device_gray();
            image.bits_per_component(1);
            image.decode_parms().k(-1).columns(i32::MAX);
        },
        &[0xFF; 32],
    );
    assert!(read_pages(&pdf).is_err());
}

#[test]
fn damaged_fax_jbig2_and_jpeg_2000_data_gives_errors() {
    let mut random = XorShift(99);
    for filter in [
        Filter::CcittFaxDecode,
        Filter::Jbig2Decode,
        Filter::JpxDecode,
        Filter::DctDecode,
    ] {
        let garbage = random.bytes(300);
        let pdf = pdf_with_dictionary(
            |image| {
                image.filter(filter);
                image.width(64).height(64);
                image.color_space().device_gray();
                image.bits_per_component(8);
            },
            &garbage,
        );
        let _ = read_pages(&pdf);
    }
}

#[test]
fn absurd_settings_are_refused_not_allocated() {
    let huge_resolution = PageSetup {
        printer_dpi: 1_000_000,
        ..PageSetup::default()
    };
    let huge_margins = PageSetup {
        margins: Margins {
            left: usize::MAX / 2,
            ..Margins::default()
        },
        ..PageSetup::default()
    };
    let beyond_picture_limit = PageSetup {
        printer_dpi: 9600,
        dot_dpi: 3200,
        ..PageSetup::default()
    };
    for setup in [huge_resolution, huge_margins, beyond_picture_limit] {
        let options = EncodeOptions {
            setup,
            ..EncodeOptions::default()
        };
        assert!(encode(b"x", &options).is_err());
    }
}
