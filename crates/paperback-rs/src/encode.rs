// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider
// Derived from PaperBack 1.10, Copyright (c) 2007 Oleh Yuschuk; see NOTICE.md.

//! Turns a file into printable pages.

use crate::block::{
    BLOCK_DOTS, DATA_LEN, FileTime, MAX_FILE_SIZE, Mode, NAME_LEN, NAME_TEXT_LEN, RawBlock,
    Redundancy, SuperBlock, recovery_address, row_mask,
};
use crate::codec::{self, Compression, MAX_PASSWORD_LEN, SALT_AND_IV_LEN};
use crate::crc::crc16;
use crate::error::{Error, Result};
use crate::layout::{CELL_DOTS, PageLayout, PageSetup};
use crate::pagetext::{self, TextStyle};
use crate::raster::{Raster, WHITE};

/// Gray level of data dots; deliberately not black so the grid lines stand out.
const DOT_LEVEL: u8 = 64;
const LINE_LEVEL: u8 = 0;
/// Pages carry at least this many block rows so the orientation stays detectable.
const MIN_ROWS: usize = 3;
const ALIGNMENT_ROW_PATTERN_ODD: u32 = 0xAAAA_AAAA;
const ALIGNMENT_ROW_PATTERN_EVEN: u32 = 0x5555_5555;
/// Windows "archive" attribute, the usual state of a plain file.
pub const DEFAULT_ATTRIBUTES: u8 = 0x20;

#[derive(Clone, Debug)]
/// Settings for [`encode`].
pub struct EncodeOptions {
    /// Page geometry and print settings.
    pub setup: PageSetup,
    /// One recovery block per this many data blocks.
    pub redundancy: Redundancy,
    /// Compression applied before printing.
    pub compression: Compression,
    /// Empty or absent means no encryption.
    pub password: Option<String>,
    /// Stored in the page labels; only the first 31 bytes are kept.
    pub name: String,
    /// Stored on the pages and restored with the file.
    pub modified: FileTime,
    /// Windows file attributes stored on the pages.
    pub attributes: u8,
}

impl Default for EncodeOptions {
    fn default() -> Self {
        Self {
            setup: PageSetup::default(),
            redundancy: Redundancy::default(),
            compression: Compression::Maximal,
            password: None,
            name: String::new(),
            modified: FileTime::default(),
            attributes: DEFAULT_ATTRIBUTES,
        }
    }
}

#[derive(Clone, Debug)]
/// One rendered page.
pub struct Page {
    /// The page picture.
    pub raster: Raster,
    /// Resolution the page is meant to be printed at.
    pub dpi: u32,
}

/// Gray values from this level up are paper in [`Page::black_and_white`]. The page text and the
/// dots are drawn at 0, 64 and 128, all of which print as ink.
const PAPER_FROM: u8 = 192;

impl Page {
    /// The page with ink black and paper white and nothing in between, for output that
    /// is printed: a black dot prints crisper than a gray one, which a printer would halftone.
    #[must_use]
    pub fn black_and_white(&self) -> Page {
        Page {
            raster: self.raster.black_and_white(PAPER_FROM),
            dpi: self.dpi,
        }
    }
}

/// Renders `data` as pages ready to print.
pub fn encode(data: &[u8], options: &EncodeOptions) -> Result<Vec<Page>> {
    let salt_and_iv = match options.password.as_deref() {
        Some(password) if !password.is_empty() => Some(codec::random_salt_and_iv()?),
        _ => None,
    };
    encode_with_salt(data, options, salt_and_iv)
}

/// Like [`encode`] but with a caller-chosen salt and IV, for reproducible output.
pub fn encode_with_salt(
    data: &[u8],
    options: &EncodeOptions,
    salt_and_iv: Option<[u8; SALT_AND_IV_LEN]>,
) -> Result<Vec<Page>> {
    if data.is_empty() {
        return Err(Error::EmptyInput);
    }
    if data.len() > MAX_FILE_SIZE as usize {
        return Err(Error::InputTooLarge(data.len()));
    }
    let payload = Payload::prepare(data, options, salt_and_iv)?;
    let layout = PageLayout::compute(&options.setup, options.redundancy)?;
    let label = payload.label(options, layout.page_capacity(options.redundancy));
    let page_count = payload
        .bytes
        .len()
        .div_ceil(layout.page_capacity(options.redundancy));
    if page_count > usize::from(u16::MAX) {
        return Err(Error::InvalidSetting(format!(
            "the data needs {page_count} pages; at most {} are possible",
            u16::MAX
        )));
    }
    let renderer = PageRenderer {
        layout,
        frame: options.setup.frame,
        redundancy: options.redundancy,
        page_count,
        dot_pitch: layout.dx,
        printer_dpi: options.setup.printer_dpi,
    };
    Ok((0..page_count)
        .map(|index| Page {
            raster: renderer.render(&payload.bytes, index, &label),
            dpi: options.setup.printer_dpi as u32,
        })
        .collect())
}

/// The bytes to print and what a decoder needs to know about them.
struct Payload {
    bytes: Vec<u8>,
    original_size: usize,
    mode: Mode,
    file_crc: u16,
    salt_and_iv: Option<[u8; SALT_AND_IV_LEN]>,
}

impl Payload {
    fn prepare(
        data: &[u8],
        options: &EncodeOptions,
        salt_and_iv: Option<[u8; SALT_AND_IV_LEN]>,
    ) -> Result<Self> {
        let packed = codec::compress(data, options.compression)?;
        let mut mode = Mode::default();
        let mut bytes = match packed {
            Some(packed) => {
                mode.0 |= Mode::COMPRESSED;
                packed
            }
            None => data.to_vec(),
        };
        bytes.resize(codec::align_up(bytes.len()), 0);
        let file_crc = crc16(&bytes);

        let password = options.password.as_deref().filter(|p| !p.is_empty());
        let salt_and_iv = match (password, salt_and_iv) {
            (Some(password), Some(salt_and_iv)) => {
                if codec::password_bytes(password).len() > MAX_PASSWORD_LEN {
                    return Err(Error::InvalidSetting(format!(
                        "password is longer than {MAX_PASSWORD_LEN} bytes"
                    )));
                }
                codec::encrypt(&mut bytes, password, &salt_and_iv)?;
                mode.0 |= Mode::ENCRYPTED;
                Some(salt_and_iv)
            }
            _ => None,
        };
        Ok(Self {
            bytes,
            original_size: data.len(),
            mode,
            file_crc,
            salt_and_iv,
        })
    }

    fn label(&self, options: &EncodeOptions, page_size: usize) -> SuperBlock {
        let mut name = [0u8; NAME_LEN];
        let text = truncate_at_char_boundary(&options.name, NAME_TEXT_LEN - 1);
        name[..text.len()].copy_from_slice(text.as_bytes());
        if let Some(salt_and_iv) = self.salt_and_iv {
            name[NAME_TEXT_LEN..].copy_from_slice(&salt_and_iv);
        }
        SuperBlock {
            data_size: self.bytes.len() as u32,
            page_size: page_size as u32,
            original_size: self.original_size as u32,
            mode: self.mode,
            attributes: options.attributes,
            page: 0,
            modified: options.modified,
            file_crc: self.file_crc,
            name,
        }
    }
}

fn truncate_at_char_boundary(text: &str, max_bytes: usize) -> &str {
    let mut end = text.len().min(max_bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

struct PageRenderer {
    layout: PageLayout,
    frame: bool,
    redundancy: Redundancy,
    page_count: usize,
    dot_pitch: usize,
    printer_dpi: usize,
}

impl PageRenderer {
    fn render(&self, payload: &[u8], page_index: usize, label: &SuperBlock) -> Raster {
        let capacity = self.layout.page_capacity(self.redundancy);
        let offset = page_index * capacity;
        let on_page = (payload.len() - offset).min(capacity);
        let group = self.redundancy.group_size();
        let groups = on_page.div_ceil(DATA_LEN).div_ceil(group);
        let rows = self.rows_needed(groups);

        let mut label = label.clone();
        label.page = (page_index + 1) as u16;
        let blocks = place_blocks(
            &payload[offset..],
            offset as u32,
            groups,
            self.redundancy,
            self.layout.nx,
            rows,
            &label.to_raw(),
        );

        let mut canvas = Canvas::new(&self.layout, rows);
        canvas.draw_grid_lines(self.frame);
        if self.frame {
            canvas.draw_frame_raster();
        }
        for (cell, mut block) in blocks {
            block.seal();
            canvas.draw_block(cell, &block);
        }
        let grid = canvas.into_raster();
        match self.layout.text {
            Some(style) => self.with_text(style, &grid, &label, page_index),
            None => grid,
        }
    }

    /// Puts the grid between the title and the hint line.
    fn with_text(
        &self,
        style: TextStyle,
        grid: &Raster,
        label: &SuperBlock,
        page_index: usize,
    ) -> Raster {
        let top = style.top_band();
        let mut page = Raster::filled(
            grid.width(),
            top + grid.height() + style.bottom_band(),
            WHITE,
        );
        page.paste(grid, 0, top);
        let header = pagetext::header_line(
            &label.file_name(),
            label.modified,
            label.original_size,
            page_index + 1,
            self.page_count,
        );
        style.draw_header(&mut page, &header);
        let footer = pagetext::footer_line(self.printer_dpi, self.dot_pitch);
        style.draw_footer(&mut page, top + grid.height(), &footer);
        page
    }

    /// The last page shrinks to the rows it needs, but never below three.
    fn rows_needed(&self, groups: usize) -> usize {
        let blocks = (groups + 1) * (self.redundancy.group_size() + 1) + 1;
        blocks
            .div_ceil(self.layout.nx)
            .max(MIN_ROWS)
            .min(self.layout.ny)
    }
}

/// Decides which block goes into which cell of the page grid.
///
/// Every string (`group_size` data strings plus one recovery string) starts
/// with a superblock. Neighbouring strings are shifted against each other so
/// blocks of one redundancy group never share a column.
fn place_blocks(
    payload: &[u8],
    first_offset: u32,
    groups: usize,
    redundancy: Redundancy,
    columns: usize,
    rows: usize,
    label: &RawBlock,
) -> Vec<(usize, RawBlock)> {
    let group = redundancy.group_size();
    let string_len = groups + 1;
    let wraps = string_len >= columns;
    let rotation = |string: usize| {
        let start = string * string_len;
        (columns / (group + 1) * string + columns - start % columns) % columns
    };
    let cell = |string: usize, position: usize| {
        let start = string * string_len;
        if wraps {
            start + (position + rotation(string)) % string_len
        } else {
            start + position
        }
    };

    let mut placed = Vec::with_capacity(columns * rows);
    for string in 0..=group {
        let start = string * string_len;
        let at = if wraps {
            start + rotation(string)
        } else {
            start
        };
        placed.push((at, label.clone()));
    }
    let mut offset = first_offset;
    for index in 0..groups {
        let mut recovery = [0xFFu8; DATA_LEN];
        let group_start = offset;
        for string in 0..group {
            let mut data = [0u8; DATA_LEN];
            let consumed = (offset - first_offset) as usize;
            if consumed < payload.len() {
                let available = (payload.len() - consumed).min(DATA_LEN);
                data[..available].copy_from_slice(&payload[consumed..consumed + available]);
            }
            for (r, d) in recovery.iter_mut().zip(&data) {
                *r ^= d;
            }
            placed.push((cell(string, index + 1), RawBlock::new(offset, &data)));
            offset += DATA_LEN as u32;
        }
        let address = recovery_address(group_start, redundancy);
        placed.push((cell(group, index + 1), RawBlock::new(address, &recovery)));
    }
    for filler in string_len * (group + 1)..columns * rows {
        placed.push((filler, label.clone()));
    }
    placed
}

/// Drawing surface that knows the page geometry.
struct Canvas<'a> {
    layout: &'a PageLayout,
    rows: usize,
    raster: Raster,
}

impl<'a> Canvas<'a> {
    fn new(layout: &'a PageLayout, rows: usize) -> Self {
        let raster = Raster::filled(layout.image_width(), layout.image_height(rows), WHITE);
        Self {
            layout,
            rows,
            raster,
        }
    }

    fn into_raster(self) -> Raster {
        self.raster
    }

    fn draw_grid_lines(&mut self, frame: bool) {
        let l = self.layout;
        let (width, height) = (self.raster.width(), self.raster.height());
        let line_height = if frame {
            height
        } else {
            self.rows * l.grid_step_y() + l.py
        };
        for column in 0..=l.nx {
            let x = column * l.grid_step_x() + l.border;
            self.raster.fill_rect(
                x,
                if frame { 0 } else { l.border },
                l.px,
                line_height,
                LINE_LEVEL,
            );
        }
        let line_width = if frame {
            width
        } else {
            l.nx * l.grid_step_x() + l.px
        };
        for row in 0..=self.rows {
            let y = row * l.grid_step_y() + l.border;
            self.raster.fill_rect(
                if frame { 0 } else { l.border },
                y,
                line_width,
                l.py,
                LINE_LEVEL,
            );
        }
    }

    /// Alternating dot pattern in the frame, which helps scanners and the
    /// decoder to find the grid.
    fn draw_frame_raster(&mut self) {
        let (nx, ny) = (self.layout.nx as isize, self.rows as isize);
        for row in -1..=ny {
            self.draw_frame_block(-1, row);
            self.draw_frame_block(nx, row);
        }
        for column in 0..nx {
            self.draw_frame_block(column, -1);
            self.draw_frame_block(column, ny);
        }
    }

    fn draw_frame_block(&mut self, column: isize, row: isize) {
        let l = *self.layout;
        let (nx, ny) = (l.nx as isize, self.rows as isize);
        let rows: [u32; BLOCK_DOTS] = std::array::from_fn(|j| {
            if j % 2 == 0 {
                ALIGNMENT_ROW_PATTERN_EVEN
            } else if row < 0 && j <= 24 || row >= ny && j > 8 {
                0
            } else if column < 0 {
                0xAA00_0000
            } else if column >= nx {
                0x0000_00AA
            } else {
                ALIGNMENT_ROW_PATTERN_ODD
            }
        });
        let x = column * l.grid_step_x() as isize + (2 * l.dx + l.border) as isize;
        let y = row * l.grid_step_y() as isize + (2 * l.dy + l.border) as isize;
        self.draw_dots(x, y, &rows);
    }

    fn draw_block(&mut self, cell: usize, block: &RawBlock) {
        let (x, y) = self.layout.cell_origin(cell);
        let rows = block.rows();
        let masked: [u32; BLOCK_DOTS] = std::array::from_fn(|j| rows[j] ^ row_mask(j));
        self.draw_dots(x as isize, y as isize, &masked);
    }

    /// Draws the set bits of `rows` as dots; dots reaching beyond the top or
    /// left edge are clipped.
    fn draw_dots(&mut self, x0: isize, y0: isize, rows: &[u32; BLOCK_DOTS]) {
        let l = *self.layout;
        for (j, bits) in rows.iter().enumerate() {
            for i in 0..BLOCK_DOTS {
                if bits >> i & 1 == 1 {
                    let x = x0 + (i * l.dx) as isize;
                    let y = y0 + (j * l.dy) as isize;
                    let (left, top) = (x.max(0), y.max(0));
                    let visible_width = (l.px as isize - (left - x)).max(0) as usize;
                    let visible_height = (l.py as isize - (top - y)).max(0) as usize;
                    self.raster.fill_rect(
                        left as usize,
                        top as usize,
                        visible_width,
                        visible_height,
                        DOT_LEVEL,
                    );
                }
            }
        }
    }
}

const _: () = assert!(CELL_DOTS == BLOCK_DOTS + 3);

#[cfg(test)]
mod tests {
    use super::*;

    fn small_setup() -> PageSetup {
        PageSetup {
            printer_dpi: 300,
            dot_dpi: 100,
            ..PageSetup::default()
        }
    }

    #[test]
    fn small_input_yields_one_page_of_fixed_width() {
        let options = EncodeOptions {
            setup: small_setup(),
            ..EncodeOptions::default()
        };
        let pages = encode(b"hello paper", &options).unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].raster.width() % 4, 0);
    }

    #[test]
    fn large_input_spans_several_pages() {
        let options = EncodeOptions {
            setup: small_setup(),
            compression: Compression::None,
            ..EncodeOptions::default()
        };
        let capacity = PageLayout::compute(&options.setup, options.redundancy)
            .unwrap()
            .page_capacity(options.redundancy);
        let data = vec![0x5Au8; capacity * 2 + 1];
        assert_eq!(encode(&data, &options).unwrap().len(), 3);
    }

    #[test]
    fn pages_carry_a_title_and_a_hint_unless_switched_off() {
        let with_text = EncodeOptions {
            setup: small_setup(),
            name: "a.txt".into(),
            ..EncodeOptions::default()
        };
        let without_text = EncodeOptions {
            setup: PageSetup {
                text: false,
                ..small_setup()
            },
            ..with_text.clone()
        };
        let titled = encode(b"hello", &with_text).unwrap().remove(0).raster;
        let plain = encode(b"hello", &without_text).unwrap().remove(0).raster;
        let style = TextStyle::for_dpi(300);
        assert_eq!(
            titled.height(),
            plain.height() + style.top_band() + style.bottom_band()
        );
        assert!(
            titled.row(style.top_band() / 2).iter().any(|&p| p != WHITE)
                || titled.pixels()[..style.top_band() * titled.width()]
                    .iter()
                    .any(|&p| p != WHITE)
        );
        assert!(plain.row(0).iter().all(|&p| p == WHITE));
    }

    #[test]
    fn empty_input_is_rejected() {
        assert!(matches!(
            encode(&[], &EncodeOptions::default()),
            Err(Error::EmptyInput)
        ));
    }

    #[test]
    fn every_cell_of_a_page_is_filled_exactly_once() {
        let redundancy = Redundancy::default();
        let (columns, groups) = (12usize, 9usize);
        let rows = ((groups + 1) * 6 + 1).div_ceil(columns).max(MIN_ROWS);
        let payload = vec![1u8; groups * 5 * DATA_LEN];
        let placed = place_blocks(
            &payload,
            0,
            groups,
            redundancy,
            columns,
            rows,
            &RawBlock::zeroed(),
        );
        let mut cells: Vec<usize> = placed.iter().map(|(cell, _)| *cell).collect();
        cells.sort_unstable();
        cells.dedup();
        assert_eq!(cells.len(), placed.len());
        assert!(cells.iter().all(|&c| c < columns * rows));
    }

    #[test]
    fn name_is_cut_on_a_character_boundary() {
        assert_eq!(truncate_at_char_boundary("aäb", 2), "a");
    }
}
