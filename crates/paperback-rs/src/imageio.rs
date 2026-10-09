// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

//! Reading and writing page images: PNG, JPEG, BMP and PDF.

use std::io::Cursor;

use image::{DynamicImage, ImageReader};
use miniz_oxide::deflate::compress_to_vec_zlib;
use pdf_writer::{Content, Filter, Finish, Name, Pdf, Rect, Ref};

use crate::encode::Page;
use crate::error::{Error, Result};
use crate::quality::ColorImage;
use crate::raster::Raster;

const PDF_POINTS_PER_INCH: f32 = 72.0;
/// A scan of a large sheet at 1200 dpi is about 14000 pixels tall; this leaves room.
const MAX_SIDE: u32 = 1 << 17;
const MAX_DECODE_BYTES: u64 = 2 << 30;
const METERS_PER_INCH_X10000: u32 = 254;
const PDF_COMPRESSION_LEVEL: u8 = 6;
const BMP_FILE_HEADER_LEN: u32 = 14;
const BMP_INFO_HEADER_LEN: u32 = 40;
const BMP_PALETTE_LEN: u32 = 256 * 4;

/// Output formats for rendered pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputFormat {
    /// PNG pictures, one per page.
    Png,
    /// 8-bit BMP, the only image format the original PaperBack opens.
    Bmp,
    /// One PDF holding all pages.
    Pdf,
}

impl OutputFormat {
    /// The format a file name suggests.
    pub fn from_extension(path: &str) -> Option<Self> {
        let extension = path.rsplit_once('.')?.1.to_ascii_lowercase();
        match extension.as_str() {
            "png" => Some(Self::Png),
            "bmp" => Some(Self::Bmp),
            "pdf" => Some(Self::Pdf),
            _ => None,
        }
    }
}

/// Reads every page contained in the file: one for an image, one per page for a PDF.
pub fn read_pages(bytes: &[u8]) -> Result<Vec<Raster>> {
    if bytes.starts_with(b"%PDF") {
        crate::pdfimage::read_pdf(bytes)
    } else {
        Ok(vec![read_image(bytes)?])
    }
}

pub(crate) fn read_image(bytes: &[u8]) -> Result<Raster> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| Error::Image(e.to_string()))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    let image = reader.decode().map_err(|e| Error::Image(e.to_string()))?;
    if !crate::raster::size_is_allowed(image.width() as usize, image.height() as usize) {
        return Err(Error::Image("the picture is too large".into()));
    }
    to_gray(&image)
}

/// Grayscale as the original computes it: the plain mean of red, green and blue.
fn to_gray(image: &DynamicImage) -> Result<Raster> {
    let (width, height) = (image.width() as usize, image.height() as usize);
    let pixels = match image.color() {
        image::ColorType::L8
        | image::ColorType::L16
        | image::ColorType::La8
        | image::ColorType::La16 => image.to_luma8().into_raw(),
        _ => image
            .to_rgb8()
            .pixels()
            .map(|p| ((u32::from(p[0]) + u32::from(p[1]) + u32::from(p[2])) / 3) as u8)
            .collect(),
    };
    Raster::from_pixels(width, height, pixels)
        .ok_or_else(|| Error::Image("the picture data does not match its size".into()))
}

/// PNG with the page resolution stored.
pub fn write_png(page: &Page) -> Result<Vec<u8>> {
    let raster = &page.raster;
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, raster.width() as u32, raster.height() as u32);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    let per_meter = pixels_per_meter(page.dpi);
    encoder.set_pixel_dims(Some(png::PixelDimensions {
        xppu: per_meter,
        yppu: per_meter,
        unit: png::Unit::Meter,
    }));
    let png_error = |e: png::EncodingError| Error::Image(e.to_string());
    let mut writer = encoder.write_header().map_err(png_error)?;
    writer
        .write_image_data(raster.pixels())
        .map_err(png_error)?;
    writer.finish().map_err(png_error)?;
    Ok(out)
}

/// PNG of a colour picture such as a quality map.
pub fn write_color_png(image: &ColorImage) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, image.width() as u32, image.height() as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let png_error = |e: png::EncodingError| Error::Image(e.to_string());
    let mut writer = encoder.write_header().map_err(png_error)?;
    writer.write_image_data(image.rgb()).map_err(png_error)?;
    writer.finish().map_err(png_error)?;
    Ok(out)
}

fn pixels_per_meter(dpi: u32) -> u32 {
    dpi * 10_000 / METERS_PER_INCH_X10000
}

/// 8-bit BMP with a gray palette, the format the original program reads.
pub fn write_bmp(page: &Page) -> Vec<u8> {
    let raster = &page.raster;
    let (width, height) = (raster.width() as u32, raster.height() as u32);
    let data_offset = BMP_FILE_HEADER_LEN + BMP_INFO_HEADER_LEN + BMP_PALETTE_LEN;
    let row_len = width.div_ceil(4) * 4;
    let image_len = row_len * height;
    let per_meter = pixels_per_meter(page.dpi);

    let mut out = Vec::with_capacity((data_offset + image_len) as usize);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(data_offset + image_len).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&data_offset.to_le_bytes());
    out.extend_from_slice(&BMP_INFO_HEADER_LEN.to_le_bytes());
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&8u16.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&image_len.to_le_bytes());
    out.extend_from_slice(&per_meter.to_le_bytes());
    out.extend_from_slice(&per_meter.to_le_bytes());
    out.extend_from_slice(&256u32.to_le_bytes());
    out.extend_from_slice(&256u32.to_le_bytes());
    for level in 0..=255u8 {
        out.extend_from_slice(&[level, level, level, 0]);
    }
    for y in (0..raster.height()).rev() {
        out.extend_from_slice(raster.row(y));
        out.resize(out.len() + (row_len as usize - raster.width()), 0);
    }
    out
}

/// A PDF with one full-page image per page, sized from the page resolution.
pub fn write_pdf(pages: &[Page]) -> Vec<u8> {
    let mut pdf = Pdf::new();
    let catalog_id = Ref::new(1);
    let tree_id = Ref::new(2);
    let first_free = 3;
    let page_refs: Vec<Ref> = (0..pages.len())
        .map(|i| Ref::new(first_free + 3 * i as i32))
        .collect();

    pdf.catalog(catalog_id).pages(tree_id);
    pdf.pages(tree_id)
        .kids(page_refs.iter().copied())
        .count(pages.len() as i32);

    for (page, &page_id) in pages.iter().zip(&page_refs) {
        let content_id = Ref::new(page_id.get() + 1);
        let image_id = Ref::new(page_id.get() + 2);
        let width_pt = page.raster.width() as f32 * PDF_POINTS_PER_INCH / page.dpi as f32;
        let height_pt = page.raster.height() as f32 * PDF_POINTS_PER_INCH / page.dpi as f32;
        let image_name = Name(b"Im0");

        let mut pdf_page = pdf.page(page_id);
        pdf_page.media_box(Rect::new(0.0, 0.0, width_pt, height_pt));
        pdf_page.parent(tree_id);
        pdf_page.contents(content_id);
        pdf_page.resources().x_objects().pair(image_name, image_id);
        pdf_page.finish();

        let mut content = Content::new();
        content.save_state();
        content.transform([width_pt, 0.0, 0.0, height_pt, 0.0, 0.0]);
        content.x_object(image_name);
        content.restore_state();
        pdf.stream(content_id, &content.finish());

        let packed = compress_to_vec_zlib(page.raster.pixels(), PDF_COMPRESSION_LEVEL);
        let mut image = pdf.image_xobject(image_id, &packed);
        image.filter(Filter::FlateDecode);
        image.width(page.raster.width() as i32);
        image.height(page.raster.height() as i32);
        image.color_space().device_gray();
        image.bits_per_component(8);
        image.finish();
    }
    pdf.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient_page() -> Page {
        let pixels: Vec<u8> = (0..60 * 40).map(|i| (i % 251) as u8).collect();
        Page {
            raster: Raster::from_pixels(60, 40, pixels).unwrap(),
            dpi: 300,
        }
    }

    #[test]
    fn png_round_trips() {
        let page = gradient_page();
        let pages = read_pages(&write_png(&page).unwrap()).unwrap();
        assert_eq!(pages, vec![page.raster]);
    }

    #[test]
    fn bmp_round_trips_with_row_padding() {
        let pixels: Vec<u8> = (0..30 * 20).map(|i| (i % 253) as u8).collect();
        let page = Page {
            raster: Raster::from_pixels(30, 20, pixels).unwrap(),
            dpi: 300,
        };
        let pages = read_pages(&write_bmp(&page)).unwrap();
        assert_eq!(pages, vec![page.raster]);
    }

    #[test]
    fn multi_page_pdf_round_trips() {
        let first = gradient_page();
        let mut second = gradient_page();
        second.raster = second.raster.flipped_vertically();
        let pdf = write_pdf(&[first.clone(), second.clone()]);
        let pages = read_pages(&pdf).unwrap();
        assert_eq!(pages, vec![first.raster, second.raster]);
    }

    #[test]
    fn extension_selects_the_format() {
        assert_eq!(
            OutputFormat::from_extension("a/b.PDF"),
            Some(OutputFormat::Pdf)
        );
        assert_eq!(OutputFormat::from_extension("x.txt"), None);
    }

    #[test]
    fn garbage_is_not_an_image() {
        assert!(matches!(
            read_pages(b"definitely not an image"),
            Err(Error::Image(_))
        ));
    }
}
