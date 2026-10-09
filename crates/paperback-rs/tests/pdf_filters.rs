// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

//! Page images inside PDFs, stored with every filter a scanner or a print-to-PDF
//! tool is likely to use, must decode like the plain image.

// Tests may unwrap: a failure should stop the test with the error.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use fax::{Color, VecWriter, encoder::Encoder};
use miniz_oxide::deflate::compress_to_vec_zlib;
use paperback_rs::codec::Compression;
use paperback_rs::decode::{DecodeOptions, decode};
use paperback_rs::encode::{EncodeOptions, encode};
use paperback_rs::imageio::read_pages;
use paperback_rs::layout::PageSetup;
use paperback_rs::raster::Raster;
use pdf_writer::{Content, Filter, Finish, Name, Pdf, Ref};

const DATA_LEN: usize = 2500;
/// Pixels darker than this are ink when a page is reduced to black and white.
const INK_THRESHOLD: u8 = 160;

fn sample() -> Vec<u8> {
    (0..DATA_LEN).map(|i| (i * 131 + i / 3) as u8).collect()
}

fn page() -> Raster {
    let options = EncodeOptions {
        setup: PageSetup {
            printer_dpi: 300,
            dot_dpi: 100,
            ..PageSetup::default()
        },
        compression: Compression::None,
        ..EncodeOptions::default()
    };
    encode(&sample(), &options).unwrap().remove(0).raster
}

fn assert_decodes(pdf: &[u8]) {
    let pages = read_pages(pdf).expect("PDF is readable");
    assert_eq!(pages.len(), 1);
    let restored = decode(&pages, &DecodeOptions::default(), |_| {}).expect("page decodes");
    assert_eq!(restored.data, sample());
}

/// What the test wants to put into the image dictionary.
struct ImageParts<'a> {
    raster: &'a Raster,
    filter: Option<Filter>,
    data: Vec<u8>,
    setup: fn(&mut pdf_writer::writers::ImageXObject),
}

fn pdf_with_image(parts: &ImageParts<'_>) -> Vec<u8> {
    let (catalog, tree, page_id, content_id, image_id) = (
        Ref::new(1),
        Ref::new(2),
        Ref::new(3),
        Ref::new(4),
        Ref::new(5),
    );
    let (width, height) = (parts.raster.width(), parts.raster.height());
    let mut pdf = Pdf::new();
    pdf.catalog(catalog).pages(tree);
    pdf.pages(tree).kids([page_id]).count(1);
    let mut page = pdf.page(page_id);
    page.media_box(pdf_writer::Rect::new(0.0, 0.0, width as f32, height as f32));
    page.parent(tree);
    page.contents(content_id);
    page.resources().x_objects().pair(Name(b"Im0"), image_id);
    page.finish();
    let mut content = Content::new();
    content.save_state();
    content.transform([width as f32, 0.0, 0.0, height as f32, 0.0, 0.0]);
    content.x_object(Name(b"Im0"));
    content.restore_state();
    pdf.stream(content_id, &content.finish());
    let mut image = pdf.image_xobject(image_id, &parts.data);
    if let Some(filter) = parts.filter {
        image.filter(filter);
    }
    image.width(width as i32).height(height as i32);
    (parts.setup)(&mut image);
    image.finish();
    pdf.finish()
}

fn gray(image: &mut pdf_writer::writers::ImageXObject) {
    image.color_space().device_gray();
    image.bits_per_component(8);
}

#[test]
fn flate_gray_samples() {
    let raster = page();
    let data = compress_to_vec_zlib(raster.pixels(), 6);
    assert_decodes(&pdf_with_image(&ImageParts {
        raster: &raster,
        filter: Some(Filter::FlateDecode),
        data,
        setup: gray,
    }));
}

#[test]
fn flate_rgb_samples() {
    let raster = page();
    let rgb: Vec<u8> = raster.pixels().iter().flat_map(|&v| [v, v, v]).collect();
    assert_decodes(&pdf_with_image(&ImageParts {
        raster: &raster,
        filter: Some(Filter::FlateDecode),
        data: compress_to_vec_zlib(&rgb, 6),
        setup: |image| {
            image.color_space().device_rgb();
            image.bits_per_component(8);
        },
    }));
}

#[test]
fn indexed_samples() {
    let raster = page();
    assert_decodes(&pdf_with_image(&ImageParts {
        raster: &raster,
        filter: Some(Filter::FlateDecode),
        data: compress_to_vec_zlib(raster.pixels(), 6),
        setup: |image| {
            let palette: Vec<u8> = (0..=255u8).flat_map(|v| [v, v, v]).collect();
            image
                .color_space()
                .indexed(Name(b"DeviceRGB"), 255, &palette);
            image.bits_per_component(8);
        },
    }));
}

#[test]
fn jpeg_images() {
    let raster = page();
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 100)
        .encode(
            raster.pixels(),
            raster.width() as u32,
            raster.height() as u32,
            image::ExtendedColorType::L8,
        )
        .unwrap();
    assert_decodes(&pdf_with_image(&ImageParts {
        raster: &raster,
        filter: Some(Filter::DctDecode),
        data: jpeg,
        setup: gray,
    }));
}

#[test]
fn jpeg_2000_images() {
    let jpx = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/pdf/page.jp2"
    ))
    .unwrap();
    let probe = hayro_jpeg2000_size(&jpx);
    let raster = Raster::filled(probe.0, probe.1, 255);
    let pdf = pdf_with_image(&ImageParts {
        raster: &raster,
        filter: Some(Filter::JpxDecode),
        data: jpx,
        setup: |_| {},
    });
    let expected = b"PaperBack JPEG 2000 fixture line\n".repeat(40);
    let pages = read_pages(&pdf).unwrap();
    let restored = decode(&pages, &DecodeOptions::default(), |_| {}).unwrap();
    assert_eq!(restored.data, expected);
}

fn hayro_jpeg2000_size(data: &[u8]) -> (usize, usize) {
    let image =
        hayro_jpeg2000::Image::new(data, &hayro_jpeg2000::DecodeSettings::default()).unwrap();
    (image.width() as usize, image.height() as usize)
}

/// Black and white version of the page as fax lines: true is ink.
fn ink_lines(raster: &Raster) -> Vec<Vec<bool>> {
    (0..raster.height())
        .map(|y| raster.row(y).iter().map(|&v| v < INK_THRESHOLD).collect())
        .collect()
}

fn fax_group4(raster: &Raster) -> Vec<u8> {
    let mut encoder = Encoder::new(VecWriter::new());
    for line in ink_lines(raster) {
        let pels = line
            .into_iter()
            .map(|ink| if ink { Color::Black } else { Color::White });
        encoder.encode_line(pels, raster.width() as u32).unwrap();
    }
    encoder.finish().unwrap().finish()
}

#[test]
fn ccitt_group_4_with_black_as_zero() {
    let raster = page();
    assert_decodes(&pdf_with_image(&ImageParts {
        raster: &raster,
        filter: Some(Filter::CcittFaxDecode),
        data: fax_group4(&raster),
        setup: |image| {
            image.color_space().device_gray();
            image.bits_per_component(1);
            let mut parms = image.decode_parms();
            parms.k(-1).columns(2048).rows(469);
        },
    }));
}

#[test]
fn ccitt_group_4_with_black_as_one_and_inverted_decode() {
    let raster = page();
    assert_decodes(&pdf_with_image(&ImageParts {
        raster: &raster,
        filter: Some(Filter::CcittFaxDecode),
        data: fax_group4(&raster),
        setup: |image| {
            image.color_space().device_gray();
            image.bits_per_component(1);
            image.decode([1.0, 0.0]);
            let mut parms = image.decode_parms();
            parms.k(-1).columns(2048).black_is_1(true);
        },
    }));
}

/// A JBIG2 stream in the embedded organisation: page information followed by one
/// MMR coded generic region (MMR is the same coding as fax group 4).
fn jbig2_embedded(raster: &Raster) -> Vec<u8> {
    let (width, height) = (raster.width() as u32, raster.height() as u32);
    let mmr = fax_group4(raster);

    let mut page_info = Vec::new();
    page_info.extend_from_slice(&width.to_be_bytes());
    page_info.extend_from_slice(&height.to_be_bytes());
    page_info.extend_from_slice(&[0; 8]); // resolution unknown
    page_info.push(0); // page flags
    page_info.extend_from_slice(&[0, 0]); // no striping

    let mut region = Vec::new();
    region.extend_from_slice(&width.to_be_bytes());
    region.extend_from_slice(&height.to_be_bytes());
    region.extend_from_slice(&[0; 8]); // x and y location
    region.push(0); // combination operator OR
    region.push(1); // generic region flags: MMR
    region.extend_from_slice(&mmr);

    let mut stream = Vec::new();
    for (number, kind, data) in [(0u32, 48u8, &page_info), (1, 38, &region)] {
        stream.extend_from_slice(&number.to_be_bytes());
        stream.push(kind);
        stream.push(0); // no referred-to segments
        stream.push(1); // page association
        stream.extend_from_slice(&(data.len() as u32).to_be_bytes());
        stream.extend_from_slice(data);
    }
    stream
}

#[test]
fn jbig2_images() {
    let raster = page();
    assert_decodes(&pdf_with_image(&ImageParts {
        raster: &raster,
        filter: Some(Filter::Jbig2Decode),
        data: jbig2_embedded(&raster),
        setup: |image| {
            image.color_space().device_gray();
            image.bits_per_component(1);
        },
    }));
}

#[test]
fn unsupported_fax_coding_is_reported() {
    let raster = page();
    let pdf = pdf_with_image(&ImageParts {
        raster: &raster,
        filter: Some(Filter::CcittFaxDecode),
        data: fax_group4(&raster),
        setup: |image| {
            image.color_space().device_gray();
            image.bits_per_component(1);
            image.decode_parms().k(2).columns(2048);
        },
    });
    let error = read_pages(&pdf).unwrap_err().to_string();
    assert!(error.contains("not supported"), "{error}");
}
