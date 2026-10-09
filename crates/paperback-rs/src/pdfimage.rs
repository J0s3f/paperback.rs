// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

//! Reading the page images out of a PDF, whatever filter they were stored with.
//!
//! Scanners and print-to-PDF tools wrap one raster per page. The filters seen
//! in practice are Flate and LZW (raw samples), DCT (JPEG), CCITT (fax),
//! JBIG2 and JPX (JPEG 2000). Vector content is not rendered.

use lopdf::xobject::PdfImage;
use lopdf::{Dictionary, Document, Object, Stream};

use crate::error::{Error, Result};
use crate::raster::Raster;

const GENERIC_FILTERS: [&str; 5] = [
    "FlateDecode",
    "LZWDecode",
    "ASCII85Decode",
    "ASCIIHexDecode",
    "RunLengthDecode",
];
const DEFAULT_CCITT_COLUMNS: u32 = 1728;
const MAX_CCITT_COLUMNS: u32 = 1 << 17;
const WHITE_SAMPLE: u8 = 255;
const BLACK_SAMPLE: u8 = 0;

pub(crate) fn read_pdf(bytes: &[u8]) -> Result<Vec<Raster>> {
    let document = Document::load_mem(bytes).map_err(pdf_error)?;
    let mut rasters = Vec::new();
    for (number, page_id) in document.get_pages() {
        let images = document.get_page_images(page_id).map_err(pdf_error)?;
        let largest = images
            .iter()
            .max_by_key(|image| image.width * image.height)
            .ok_or_else(|| Error::Pdf(format!("page {number} contains no image")))?;
        let raster = decode_image(&document, largest)
            .map_err(|e| Error::Pdf(format!("page {number}: {e}")))?;
        rasters.push(raster);
    }
    Ok(rasters)
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "used directly as a map_err function"
)]
fn pdf_error(error: lopdf::Error) -> Error {
    Error::Pdf(error.to_string())
}

/// What the image's stream dictionary says about its samples.
struct ImageSpec {
    width: usize,
    height: usize,
    bits: usize,
    color: ColorModel,
    /// `/Decode [1 0]`: samples count from white to black instead.
    inverted: bool,
}

enum ColorModel {
    Gray,
    Rgb,
    Cmyk,
    Indexed {
        base_components: usize,
        palette: Vec<u8>,
    },
}

impl ColorModel {
    fn components(&self) -> usize {
        match self {
            Self::Gray | Self::Indexed { .. } => 1,
            Self::Rgb => 3,
            Self::Cmyk => 4,
        }
    }
}

fn decode_image(document: &Document, image: &PdfImage<'_>) -> Result<Raster> {
    let Object::Stream(stream) = document.get_object(image.id).map_err(pdf_error)? else {
        return Err(Error::Pdf("the image is not a stream".into()));
    };
    let filters = filter_names(document, &stream.dict);
    let spec = image_spec(document, &stream.dict)?;

    match filters
        .iter()
        .position(|f| !GENERIC_FILTERS.contains(&f.as_str()))
    {
        None => {
            let samples = stream.decompressed_content().map_err(pdf_error)?;
            raster_from_samples(&samples, &spec)
        }
        Some(index) => {
            let data = unwrap_leading_filters(stream, index)?;
            let params = decode_params(document, &stream.dict, index);
            match filters[index].as_str() {
                "DCTDecode" => crate::imageio::read_image(&data),
                "CCITTFaxDecode" => ccitt(&data, &params, &spec),
                "JBIG2Decode" => jbig2(document, &data, &params, &spec),
                "JPXDecode" => jpx(&data),
                other => Err(Error::Pdf(format!(
                    "images using {other} are not supported"
                ))),
            }
        }
    }
}

/// Applies the generic filters in front of the image filter at `index`.
fn unwrap_leading_filters(stream: &Stream, index: usize) -> Result<Vec<u8>> {
    if index == 0 {
        return Ok(stream.content.clone());
    }
    let mut dict = stream.dict.clone();
    let leading: Vec<Object> = filter_objects(&stream.dict)
        .into_iter()
        .take(index)
        .collect();
    dict.set("Filter", Object::Array(leading));
    Stream::new(dict, stream.content.clone())
        .decompressed_content()
        .map_err(pdf_error)
}

fn filter_objects(dict: &Dictionary) -> Vec<Object> {
    match dict.get(b"Filter") {
        Ok(Object::Name(name)) => vec![Object::Name(name.clone())],
        Ok(Object::Array(items)) => items.clone(),
        _ => Vec::new(),
    }
}

fn filter_names(document: &Document, dict: &Dictionary) -> Vec<String> {
    filter_objects(dict)
        .iter()
        .filter_map(|object| document.dereference(object).ok())
        .filter_map(|(_, object)| object.as_name().ok())
        .map(|name| String::from_utf8_lossy(name).into_owned())
        .collect()
}

/// The `/DecodeParms` entry that belongs to the filter at `index`.
fn decode_params(document: &Document, dict: &Dictionary, index: usize) -> Dictionary {
    let found = match dict.get(b"DecodeParms").or_else(|_| dict.get(b"DP")) {
        Ok(Object::Array(items)) => items.get(index).cloned(),
        Ok(other) => (index == 0 || filter_objects(dict).len() == 1).then(|| other.clone()),
        Err(_) => None,
    };
    found
        .and_then(|object| document.dereference(&object).ok().map(|(_, o)| o.clone()))
        .and_then(|object| object.as_dict().ok().cloned())
        .unwrap_or_default()
}

fn int(dict: &Dictionary, key: &[u8]) -> Option<i64> {
    dict.get(key).ok().and_then(|o| o.as_i64().ok())
}

fn boolean(dict: &Dictionary, key: &[u8]) -> Option<bool> {
    dict.get(key).ok().and_then(|o| o.as_bool().ok())
}

fn image_spec(document: &Document, dict: &Dictionary) -> Result<ImageSpec> {
    let dimension = |key: &[u8]| {
        int(dict, key)
            .and_then(|v| usize::try_from(v).ok())
            .filter(|&v| v > 0)
            .ok_or_else(|| Error::Pdf("image without dimensions".into()))
    };
    let is_mask = boolean(dict, b"ImageMask").unwrap_or(false);
    let bits = if is_mask {
        1
    } else {
        usize::try_from(int(dict, b"BitsPerComponent").unwrap_or(8)).unwrap_or(0)
    };
    let color = if is_mask {
        ColorModel::Gray
    } else {
        dict.get(b"ColorSpace")
            .ok()
            .map_or(ColorModel::Gray, |space| color_model(document, space))
    };
    let inverted = dict
        .get(b"Decode")
        .ok()
        .and_then(|o| o.as_array().ok())
        .is_some_and(|d| d.first().and_then(|v| v.as_float().ok()) == Some(1.0));
    let (width, height) = (dimension(b"Width")?, dimension(b"Height")?);
    if !crate::raster::size_is_allowed(width, height) {
        return Err(Error::Pdf(format!(
            "the image size {width}x{height} is not accepted"
        )));
    }
    if ![1, 2, 4, 8, 16].contains(&bits) {
        return Err(Error::Pdf(format!(
            "{bits} bits per sample are not supported"
        )));
    }
    Ok(ImageSpec {
        width,
        height,
        bits,
        color,
        inverted,
    })
}

fn color_model(document: &Document, space: &Object) -> ColorModel {
    let Ok((_, space)) = document.dereference(space) else {
        return ColorModel::Gray;
    };
    match space {
        Object::Name(name) => named_color_model(name),
        Object::Array(items) => match items.first().and_then(|o| o.as_name().ok()) {
            Some(b"ICCBased") => icc_color_model(document, items),
            Some(b"Indexed" | b"I") => indexed_color_model(document, items),
            Some(name) => named_color_model(name),
            None => ColorModel::Gray,
        },
        _ => ColorModel::Gray,
    }
}

fn named_color_model(name: &[u8]) -> ColorModel {
    match name {
        b"DeviceRGB" | b"RGB" | b"CalRGB" | b"Lab" => ColorModel::Rgb,
        b"DeviceCMYK" | b"CMYK" => ColorModel::Cmyk,
        _ => ColorModel::Gray,
    }
}

fn icc_color_model(document: &Document, items: &[Object]) -> ColorModel {
    let components = items
        .get(1)
        .and_then(|o| document.dereference(o).ok())
        .and_then(|(_, o)| o.as_stream().ok())
        .and_then(|s| int(&s.dict, b"N"));
    match components {
        Some(3) => ColorModel::Rgb,
        Some(4) => ColorModel::Cmyk,
        _ => ColorModel::Gray,
    }
}

fn indexed_color_model(document: &Document, items: &[Object]) -> ColorModel {
    let base = items
        .get(1)
        .map_or(ColorModel::Gray, |o| color_model(document, o));
    let palette = items
        .get(3)
        .and_then(|o| document.dereference(o).ok())
        .and_then(|(_, o)| match o {
            Object::String(bytes, _) => Some(bytes.clone()),
            Object::Stream(stream) => stream.decompressed_content().ok(),
            _ => None,
        })
        .unwrap_or_default();
    ColorModel::Indexed {
        base_components: base.components(),
        palette,
    }
}

/// Gray from raw sample data: unpacks the bit depth, applies the palette and
/// averages colour components, as the original does for scans.
fn raster_from_samples(data: &[u8], spec: &ImageSpec) -> Result<Raster> {
    let components = spec.color.components();
    let row_bits = spec.width * components * spec.bits;
    let row_len = row_bits.div_ceil(8);
    if data.len() < row_len * spec.height {
        return Err(Error::Pdf("the image data is shorter than its size".into()));
    }
    let mut pixels = Vec::with_capacity(spec.width * spec.height);
    for y in 0..spec.height {
        let row = &data[y * row_len..(y + 1) * row_len];
        for x in 0..spec.width {
            let values: Vec<u32> = (0..components)
                .map(|c| sample_at(row, (x * components + c) * spec.bits, spec.bits))
                .collect();
            pixels.push(gray_of(&values, spec));
        }
    }
    Raster::from_pixels(spec.width, spec.height, pixels)
        .ok_or_else(|| Error::Pdf("inconsistent image size".into()))
}

/// The sample starting at bit `at`, scaled to 0..=255.
fn sample_at(row: &[u8], at: usize, bits: usize) -> u32 {
    let max = (1u32 << bits.min(16)) - 1;
    let raw = match bits {
        16 => u32::from(row[at / 8]) << 8 | u32::from(row[at / 8 + 1]),
        8 => u32::from(row[at / 8]),
        _ => u32::from(row[at / 8] >> (8 - bits - at % 8) & ((1u8 << bits) - 1)),
    };
    if bits == 8 { raw } else { raw * 255 / max }
}

fn gray_of(values: &[u32], spec: &ImageSpec) -> u8 {
    let gray = match &spec.color {
        ColorModel::Gray => values[0],
        ColorModel::Rgb => values.iter().sum::<u32>() / 3,
        ColorModel::Cmyk => {
            let black = values[3];
            values[..3]
                .iter()
                .map(|&ink| 255 - (ink + black).min(255))
                .sum::<u32>()
                / 3
        }
        ColorModel::Indexed {
            base_components,
            palette,
        } => {
            let index = indexed_value(values[0], spec.bits);
            let at = index * base_components;
            let entry = palette.get(at..at + base_components).unwrap_or(&[]);
            match entry.len() {
                0 => WHITE_SAMPLE.into(),
                1 => entry[0].into(),
                3 => entry.iter().map(|&v| u32::from(v)).sum::<u32>() / 3,
                _ => entry.iter().map(|&v| u32::from(v)).sum::<u32>() / entry.len() as u32,
            }
        }
    };
    let gray = gray.min(255) as u8;
    if spec.inverted { 255 - gray } else { gray }
}

/// Palette indices were scaled to 0..=255 when unpacked; recover the index.
fn indexed_value(scaled: u32, bits: usize) -> usize {
    if bits >= 8 {
        scaled as usize
    } else {
        let max = (1u32 << bits) - 1;
        (scaled * max / 255) as usize
    }
}

/// Gray level of ink when the stream's samples are read the way a viewer does:
/// a 1 sample is white unless /Decode [1 0] says otherwise.
fn ink_level(one_is_ink: bool, spec: &ImageSpec) -> u8 {
    let level = if one_is_ink {
        WHITE_SAMPLE
    } else {
        BLACK_SAMPLE
    };
    if spec.inverted {
        WHITE_SAMPLE - level
    } else {
        level
    }
}

/// Bilevel pictures to gray: ink gets ink, the paper the opposite level.
fn bilevel_raster(width: usize, height: usize, lines: &[Vec<bool>], ink: u8) -> Result<Raster> {
    let mut pixels = Vec::with_capacity(width * height);
    for y in 0..height {
        let line = lines.get(y);
        for x in 0..width {
            let is_ink = line.and_then(|l| l.get(x)).copied().unwrap_or(false);
            pixels.push(if is_ink { ink } else { WHITE_SAMPLE - ink });
        }
    }
    Raster::from_pixels(width, height, pixels)
        .ok_or_else(|| Error::Pdf("inconsistent image size".into()))
}

fn ccitt(data: &[u8], params: &Dictionary, spec: &ImageSpec) -> Result<Raster> {
    let k = int(params, b"K").unwrap_or(0);
    let columns = int(params, b"Columns")
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(DEFAULT_CCITT_COLUMNS);
    if columns == 0 || columns > MAX_CCITT_COLUMNS {
        return Err(Error::Pdf(format!(
            "{columns} fax columns are not accepted"
        )));
    }
    let rows = int(params, b"Rows")
        .and_then(|v| u32::try_from(v).ok())
        .filter(|&v| v > 0)
        .unwrap_or(spec.height as u32);
    let black_is_one = boolean(params, b"BlackIs1").unwrap_or(false);

    let mut lines: Vec<Vec<bool>> = Vec::new();
    let mut collect = |transitions: &[u32]| {
        if lines.len() >= spec.height {
            return;
        }
        lines.push(
            fax::decoder::pels(transitions, columns)
                .map(|pel| pel == fax::Color::Black)
                .collect(),
        );
    };
    let decoded = match k {
        k if k < 0 => {
            fax::decoder::decode_g4(data.iter().copied(), columns, Some(rows), &mut collect)
        }
        0 => fax::decoder::decode_g3(data.iter().copied(), &mut collect),
        _ => {
            return Err(Error::Pdf(
                "mixed one- and two-dimensional fax coding is not supported".into(),
            ));
        }
    };
    if decoded.is_none() && lines.is_empty() {
        return Err(Error::Pdf("the fax data cannot be decoded".into()));
    }
    let ink = ink_level(black_is_one, spec);
    bilevel_raster(columns as usize, lines.len(), &lines, ink)
        .map(|raster| crop_or_pad(raster, spec.width, spec.height))
}

/// Gives the raster the size the dictionary announces.
fn crop_or_pad(raster: Raster, width: usize, height: usize) -> Raster {
    if raster.width() == width && raster.height() == height {
        return raster;
    }
    let mut out = Raster::filled(width, height, WHITE_SAMPLE);
    out.paste(&raster, 0, 0);
    out
}

struct BilevelCollector {
    width: usize,
    max_lines: usize,
    lines: Vec<Vec<bool>>,
    current: Vec<bool>,
}

impl hayro_jbig2::Decoder for BilevelCollector {
    fn push_pixel(&mut self, black: bool) {
        if self.current.len() < self.width && self.lines.len() < self.max_lines {
            self.current.push(black);
        }
    }

    fn push_pixel_chunk(&mut self, black: bool, chunk_count: u32) {
        let room = self.width.saturating_sub(self.current.len());
        let count = (chunk_count as usize).saturating_mul(8).min(room);
        if self.lines.len() < self.max_lines {
            self.current.extend(std::iter::repeat_n(black, count));
        }
    }

    fn next_line(&mut self) {
        if self.lines.len() < self.max_lines {
            self.lines.push(std::mem::take(&mut self.current));
        }
        self.current.clear();
    }
}

fn jbig2(
    document: &Document,
    data: &[u8],
    params: &Dictionary,
    spec: &ImageSpec,
) -> Result<Raster> {
    let globals = params
        .get(b"JBIG2Globals")
        .ok()
        .and_then(|o| document.dereference(o).ok())
        .and_then(|(_, o)| o.as_stream().ok())
        .and_then(|s| s.decompressed_content().ok());
    let image = hayro_jbig2::Image::new_embedded(data, globals.as_deref())
        .map_err(|e| Error::Pdf(format!("JBIG2: {e}")))?;
    if !crate::raster::size_is_allowed(image.width() as usize, image.height() as usize) {
        return Err(Error::Pdf("JBIG2: the image size is not accepted".into()));
    }
    let mut collector = BilevelCollector {
        width: image.width() as usize,
        max_lines: image.height() as usize,
        lines: Vec::new(),
        current: Vec::new(),
    };
    image
        .decode(&mut collector)
        .map_err(|e| Error::Pdf(format!("JBIG2: {e}")))?;
    // JBIG2 stores ink as 1; the PDF filter hands it on inverted, so ink reads as 0.
    let ink = ink_level(false, spec);
    let raster = bilevel_raster(
        image.width() as usize,
        collector.lines.len(),
        &collector.lines,
        ink,
    )?;
    Ok(crop_or_pad(raster, spec.width, spec.height))
}

fn jpx(data: &[u8]) -> Result<Raster> {
    let settings = hayro_jpeg2000::DecodeSettings::default();
    let image = hayro_jpeg2000::Image::new(data, &settings)
        .map_err(|e| Error::Pdf(format!("JPEG 2000: {e}")))?;
    let (width, height) = (image.width() as usize, image.height() as usize);
    if !crate::raster::size_is_allowed(width, height) {
        return Err(Error::Pdf(
            "JPEG 2000: the image size is not accepted".into(),
        ));
    }
    let alpha = usize::from(image.has_alpha());
    let channels = usize::from(image.color_space().num_channels()) + alpha;
    let mut context = hayro_jpeg2000::DecoderContext::default();
    let decoded = image
        .decode(&mut context)
        .map_err(|e| Error::Pdf(format!("JPEG 2000: {e}")))?;
    let samples = decoded.data_u8();
    let color_channels = channels - alpha;
    if color_channels == 0 {
        return Err(Error::Pdf(
            "JPEG 2000: the image has no colour channel".into(),
        ));
    }
    let pixels = samples
        .chunks_exact(channels)
        .map(|pixel| {
            let sum: u32 = pixel[..color_channels].iter().map(|&v| u32::from(v)).sum();
            (sum / color_channels as u32) as u8
        })
        .collect();
    Raster::from_pixels(width, height, pixels)
        .ok_or_else(|| Error::Pdf("JPEG 2000: inconsistent image size".into()))
}
