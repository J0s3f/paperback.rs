// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider
// Derived from PaperBack 1.10, Copyright (c) 2007 Oleh Yuschuk; see NOTICE.md.

//! Turns a file into printable pages.

use crate::block::{
    BLOCK_DOTS, DATA_LEN, FileTime, MAX_FILE_SIZE, Mode, NAME_LEN, NAME_TEXT_LEN, RawBlock,
    Redundancy, SuperBlock, row_mask,
};
use crate::codec::{self, Compression, MAX_PASSWORD_LEN, SALT_AND_IV_LEN};
use crate::crc::crc16;
use crate::error::{Error, Result};
use crate::layout::{CELL_DOTS, PageLayout, PageSetup};
use crate::pagetext::{self, TextStyle};
use crate::parity::{self, Code};
use crate::pbx::{self, HashRecord, Record, SheetId, SheetRecord};
use crate::plan::{
    Demand, PageShare, Pagination, RECORD_CELLS, RECORD_CELLS_PER_END, Slot, Strings,
    behind_strings, plan_page,
};
use crate::raster::{Raster, WHITE};
use rayon::prelude::*;

/// Gray level of data dots; deliberately not black so the grid lines stand out.
const DOT_LEVEL: u8 = 64;
const LINE_LEVEL: u8 = 0;
const ALIGNMENT_ROW_PATTERN_ODD: u32 = 0xAAAA_AAAA;
const ALIGNMENT_ROW_PATTERN_EVEN: u32 = 0x5555_5555;
/// Windows "archive" attribute, the usual state of a plain file.
pub const DEFAULT_ATTRIBUTES: u8 = 0x20;

#[derive(Clone, Debug)]
/// Settings for [`encode`].
pub struct EncodeOptions {
    /// Page geometry and print settings.
    pub setup: PageSetup,
    /// PaperBack's redundancy: one recovery block per this many data blocks, which the
    /// original programs use too. `None` leaves the recovery blocks out.
    pub redundancy: Option<Redundancy>,
    /// Parity blocks per 100 data blocks, as many as fit the code: at least 300, and more for
    /// files of fewer than 16384 data blocks (`100 * (65536 - blocks) / blocks`); [`encode`]
    /// says how many when asked for more. Data blocks up to their number can be rebuilt from
    /// them, on whichever pages they are lost; the original programs ignore them. A file of more than 16384 data blocks is cut into
    /// `ceil(blocks / 16384)` interleaved stripes (block `i` in stripe `i % stripes`), and each
    /// stripe is rebuilt only from its own parity blocks: consecutive losses, such as a whole
    /// page, spread evenly over them, but losses that fall on one stripe are limited by its share.
    pub parity_percent: u32,
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
    /// Whether to add what the original programs do not know: a SHA-256 of the file and an
    /// identifier and layout for each page, in cells that would repeat the label. The
    /// originals read such pages as before. Only used when there is room for it.
    pub extensions: bool,
    /// Use the whole paper of every page, which is printed anyway: every free cell holds another
    /// parity block. The number of pages stays the same.
    pub fill_pages: bool,
    /// Fail rather than leave out the records (the hash of the file) when a page has no room
    /// for them.
    pub require_extensions: bool,
}

/// Group size of PaperBack's redundancy that [`EncodeOptions::default`] uses.
pub const DEFAULT_GROUP_SIZE: u8 = 10;
/// Parity blocks per 100 data blocks that [`EncodeOptions::default`] uses.
pub const DEFAULT_PARITY_PERCENT: u32 = 10;
/// Parity blocks per 100 data blocks that [`EncodeOptions::modern`] uses: the default parity and
/// as many blocks again as the default recovery blocks take.
pub const MODERN_PARITY_PERCENT: u32 = DEFAULT_PARITY_PERCENT + 100 / DEFAULT_GROUP_SIZE as u32;

impl EncodeOptions {
    /// Pages as PaperBack 1.10 writes them: its recovery blocks with its default group size, and
    /// nothing the originals do not know (no parity blocks, no records).
    #[must_use]
    pub fn legacy() -> Self {
        Self {
            redundancy: Some(Redundancy::default()),
            parity_percent: 0,
            fill_pages: false,
            extensions: false,
            require_extensions: false,
            ..Self::default()
        }
    }

    /// The default: some recovery blocks of PaperBack, which the originals use too, some parity
    /// blocks, and the records with the hash of the file. The same as [`EncodeOptions::default`].
    #[must_use]
    pub fn balanced() -> Self {
        Self::default()
    }

    /// Settings for pages meant for paperback.rs: no recovery blocks of PaperBack but as many
    /// more parity blocks, filled pages, and the records with the hash of the file, required.
    /// The original programs still read the pages but cannot rebuild lost blocks.
    #[must_use]
    pub fn modern() -> Self {
        Self {
            redundancy: None,
            parity_percent: MODERN_PARITY_PERCENT,
            fill_pages: true,
            extensions: true,
            require_extensions: true,
            ..Self::default()
        }
    }
}

impl Default for EncodeOptions {
    fn default() -> Self {
        Self {
            setup: PageSetup::default(),
            redundancy: Some(Redundancy::new(DEFAULT_GROUP_SIZE).unwrap_or_default()),
            parity_percent: DEFAULT_PARITY_PERCENT,
            compression: Compression::Maximal,
            password: None,
            name: String::new(),
            modified: FileTime::default(),
            attributes: DEFAULT_ATTRIBUTES,
            extensions: true,
            fill_pages: false,
            require_extensions: false,
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
    let strings = Strings::new(options.redundancy);
    let layout = PageLayout::compute(&options.setup, strings)?;
    let records = options.extensions && layout.has_room_for(strings, RECORD_CELLS);
    let payload = Payload::prepare(data, options, salt_and_iv, records)?;
    if options.require_extensions && payload.seal.is_none() {
        return Err(Error::InvalidSetting(
            "the pages have no room for the hash of the file".into(),
        ));
    }
    let reserved = if payload.seal.is_some() {
        RECORD_CELLS
    } else {
        0
    };
    let blocks = payload.bytes.len().div_ceil(DATA_LEN);
    let code = Code::new(blocks);
    let demand = Demand {
        blocks,
        strings,
        size: (layout.nx, layout.ny),
        reserved,
        // In 64 bits: blocks times a percentage can overflow 32.
        parity: usize::try_from((blocks as u64 * u64::from(options.parity_percent)).div_ceil(100))
            .unwrap_or(usize::MAX),
        max_parity: code.max_parity(),
        fill: options.fill_pages,
    };
    let plan = paginate(&demand, usize::from(u16::MAX))?;
    let page_count = plan.pages.len();
    let page_size = plan.groups_per_page * strings.group_bytes();
    let label = payload.label(options, page_size);
    let renderer = PageRenderer {
        layout,
        frame: options.setup.frame,
        strings,
        parity: code.encode(&payload.bytes, plan.parity),
        plan,
        page_size,
        seal: payload.seal,
        dot_pitch: layout.dx,
        printer_dpi: options.setup.printer_dpi,
    };
    // The pages are drawn side by side.
    Ok((0..page_count)
        .into_par_iter()
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
    /// The check value and sheet seed to put on the pages, if they get any.
    seal: Option<Seal>,
}

/// What the pages carry to vouch for the file.
#[derive(Clone, Copy)]
struct Seal {
    /// SHA-256 of the file, or for an encrypted file a check value only the password gives.
    digest: [u8; pbx::HASH_LEN],
    keyed: bool,
    /// Hash of the bytes as they are on paper, from which the sheet identifiers come.
    stream_hash: [u8; pbx::HASH_LEN],
}

impl Payload {
    fn prepare(
        data: &[u8],
        options: &EncodeOptions,
        salt_and_iv: Option<[u8; SALT_AND_IV_LEN]>,
        records: bool,
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
        // The originals ignore the mode bits they do not know, so this one marks the pages for
        // readers that look for the records.
        let has_room = pbx::record_address(bytes.len() as u32, RECORDS_PER_FILE - 1).is_some();
        let seal = (records && has_room).then(|| Seal {
            digest: match (password, salt_and_iv) {
                (Some(password), Some(salt_and_iv)) => {
                    codec::authenticate(data, password, &salt_and_iv)
                }
                _ => pbx::file_hash(data),
            },
            keyed: salt_and_iv.is_some(),
            stream_hash: pbx::file_hash(&bytes),
        });
        if seal.is_some() {
            mode.0 |= Mode::EXTENDED;
        }
        Ok(Self {
            bytes,
            original_size: data.len(),
            mode,
            file_crc,
            salt_and_iv,
            seal,
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

/// Records of a page: which sheet it is, the hash, and the parity of the two.
const RECORDS_PER_FILE: usize = 3;

struct PageRenderer {
    layout: PageLayout,
    frame: bool,
    strings: Strings,
    /// What every page holds.
    plan: Pagination,
    /// The parity blocks of the file.
    parity: Vec<[u8; DATA_LEN]>,
    /// Bytes of the file on a page; the last page may hold fewer.
    page_size: usize,
    seal: Option<Seal>,
    dot_pitch: usize,
    printer_dpi: usize,
}

impl PageRenderer {
    fn render(&self, payload: &[u8], page_index: usize, label: &SuperBlock) -> Raster {
        let offset = page_index * self.page_size;
        let share = self.plan.pages[page_index];
        let rows = share.rows;
        let behind = behind_strings(share.parity_first, share.parity_count);

        let mut label = label.clone();
        label.page = (page_index + 1) as u16;
        let slots = plan_page(
            offset as u32,
            share.groups,
            self.strings,
            (self.layout.nx, rows),
            self.reserved() / 2,
            &behind,
        );
        let blocks = self.fill(&slots, payload, &label, share);

        let mut canvas = Canvas::new(&self.layout, rows);
        canvas.draw_grid_lines(self.frame);
        if self.frame {
            canvas.draw_frame_raster();
        }
        for (cell, mut block) in blocks.into_iter().enumerate() {
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
            self.plan.pages.len(),
        );
        style.draw_header(&mut page, &header);
        let footer = pagetext::footer_line(self.printer_dpi, self.dot_pitch);
        style.draw_footer(&mut page, top + grid.height(), &footer);
        page
    }

    /// Cells of a page kept free for records.
    fn reserved(&self) -> usize {
        if self.seal.is_some() { RECORD_CELLS } else { 0 }
    }

    /// The block of every cell of a page, in the order of the cells.
    fn fill(
        &self,
        slots: &[Slot],
        payload: &[u8],
        label: &SuperBlock,
        share: PageShare,
    ) -> Vec<RawBlock> {
        let group = self.strings.data;
        let data_at = |offset: u32| -> [u8; DATA_LEN] {
            let mut data = [0u8; DATA_LEN];
            let consumed = offset as usize;
            if consumed < payload.len() {
                let available = (payload.len() - consumed).min(DATA_LEN);
                data[..available].copy_from_slice(&payload[consumed..consumed + available]);
            }
            data
        };
        let label_block = label.to_raw();
        let records = self.records(label, share);
        let last = slots.len() - 1;
        // One copy of the records at the start of the page, one at its end.
        let record_in = |cell: usize| {
            if records.is_empty() {
                None
            } else if cell < RECORD_CELLS_PER_END {
                Some(records[cell].clone())
            } else if last - cell < RECORD_CELLS_PER_END {
                Some(records[RECORD_CELLS_PER_END - 1 - (last - cell)].clone())
            } else {
                None
            }
        };
        slots
            .iter()
            .enumerate()
            .map(|(cell, slot)| match *slot {
                Slot::Data(offset) => RawBlock::new(offset, &data_at(offset)),
                Slot::Recovery(group_start) => {
                    let mut recovery = [0xFFu8; DATA_LEN];
                    for string in 0..group {
                        let data = data_at(group_start + (string * DATA_LEN) as u32);
                        recovery.iter_mut().zip(&data).for_each(|(r, d)| *r ^= d);
                    }
                    let address = slot.address(self.strings).unwrap_or(group_start);
                    RawBlock::new(address, &recovery)
                }
                Slot::Parity(index) => RawBlock::new(parity::address(index), &self.parity[index]),
                Slot::Label => label_block.clone(),
                Slot::Spare => record_in(cell).unwrap_or_else(|| label_block.clone()),
            })
            .collect()
    }

    /// The records of a page: the sheet, the hash and the parity of the two. The page carries
    /// the set twice, at its start and at its end.
    fn records(&self, label: &SuperBlock, share: PageShare) -> Vec<RawBlock> {
        let Some(seal) = self.seal else {
            return Vec::new();
        };
        let page = label.page;
        let sheet_record = SheetRecord {
            id: SheetId::derive(&seal.stream_hash, page, label.data_size),
            page,
            page_count: self.plan.pages.len() as u16,
            columns: self.layout.nx as u16,
            rows: share.rows as u16,
            group_size: self.strings.recovery.map_or(0, |r| r.group_size() as u8),
            parity_first: share.parity_first as u32,
            parity_count: share.parity_count as u32,
            parity_total: self.plan.parity as u32,
        };
        let hash_record = HashRecord {
            digest: seal.digest,
            original_size: label.original_size,
            keyed: seal.keyed,
        };
        let (sheet, hash) = (Record::Sheet(sheet_record), Record::Hash(hash_record));
        let parity = Record::parity_of(&sheet_record, &hash_record);
        [sheet, hash, parity]
            .into_iter()
            .enumerate()
            .map(|(index, record)| {
                // Room was checked for the highest index when the payload was prepared.
                let address = pbx::record_address(label.data_size, index).unwrap_or_default();
                RawBlock::new(address, &record.to_payload())
            })
            .collect()
    }
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

/// How the file is spread over at most `most_pages` pages, or why it cannot be: too many parity
/// blocks for the code, or for the pages, which are named with the most there can be.
fn paginate(demand: &Demand, most_pages: usize) -> Result<Pagination> {
    let blocks = demand.blocks;
    if demand.parity > demand.max_parity {
        return Err(Error::InvalidSetting(format!(
            "{} parity blocks do not fit the code; a file of {blocks} data blocks can have at most \
             {} ({} per 100 data blocks)",
            demand.parity,
            demand.max_parity,
            demand.max_parity * 100 / blocks
        )));
    }
    let fits = |parity| {
        Demand { parity, ..*demand }
            .paginate()
            .filter(|plan| plan.pages.len() <= most_pages)
    };
    if let Some(plan) = fits(demand.parity) {
        return Ok(plan);
    }
    if demand.parity == 0 || fits(0).is_none() {
        let pages = Demand {
            parity: 0,
            ..*demand
        }
        .paginate()
        .map_or(0, |plan| plan.pages.len());
        return Err(Error::InvalidSetting(format!(
            "the data needs {pages} pages; at most {most_pages} are possible"
        )));
    }
    // Every page carries data, so a small file has few pages to put parity blocks on, and a
    // large one may run out of page numbers.
    let (mut most, mut too_many) = (0, demand.parity);
    while too_many - most > 1 {
        let middle = most + (too_many - most) / 2;
        if fits(middle).is_some() {
            most = middle;
        } else {
            too_many = middle;
        }
    }
    Err(Error::InvalidSetting(format!(
        "{} parity blocks do not fit on the pages; with this page setup a file of {blocks} data \
         blocks can have at most {most} ({} per 100 data blocks)",
        demand.parity,
        most * 100 / blocks
    )))
}

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
    fn parity_that_needs_more_pages_than_can_be_numbered_is_refused_with_the_most() {
        // Once the parity asked for could take the pages past the last page number and the
        // error said only how many pages the data needs. Here a page holds 17 groups, and three
        // pages are all there can be.
        let demand = Demand {
            blocks: 40 * 5,
            strings: Strings::new(Redundancy::new(5)),
            size: (12, 10),
            reserved: RECORD_CELLS,
            parity: 0,
            max_parity: 1_000_000,
            fill: false,
        };
        assert_eq!(paginate(&demand, 3).unwrap().pages.len(), 3);
        let fits = |parity| paginate(&Demand { parity, ..demand }, 3);
        let most = (0..200)
            .take_while(|&parity| fits(parity).is_ok())
            .last()
            .unwrap();
        let message = fits(most + 1).unwrap_err().to_string();
        assert!(message.contains(&format!("at most {most} ")), "{message}");
        // Without parity the pages themselves are too many.
        let message = paginate(&demand, 2).unwrap_err().to_string();
        assert!(message.contains("needs 3 pages; at most 2"), "{message}");
    }

    #[test]
    fn as_much_parity_as_fits_is_allowed_and_more_is_refused_with_the_most() {
        // One data block on one page: the page holds a few hundred parity blocks, far fewer than
        // the code would allow, and the message says how many.
        let asking = |parity_percent| {
            encode(
                b"hello paper",
                &EncodeOptions {
                    setup: small_setup(),
                    compression: Compression::None,
                    parity_percent,
                    ..EncodeOptions::default()
                },
            )
        };
        let Err(Error::InvalidSetting(message)) = asking(1_000_000) else {
            panic!("a million percent fitted");
        };
        let most: u32 = message
            .split('(')
            .nth(1)
            .and_then(|rest| rest.split(' ').next())
            .and_then(|number| number.parse().ok())
            .unwrap_or_else(|| panic!("no maximum in {message:?}"));
        assert!(most > 1000, "{message}");
        assert_eq!(asking(most).unwrap().len(), 1);
        assert!(asking(most + 100).is_err());
    }

    #[test]
    fn large_input_spans_several_pages() {
        let options = EncodeOptions {
            setup: small_setup(),
            compression: Compression::None,
            ..EncodeOptions::default()
        };
        let capacity = PageLayout::compute(&options.setup, Strings::new(options.redundancy))
            .unwrap()
            .page_capacity(Strings::new(options.redundancy), 0);
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
    fn the_hash_of_an_encrypted_file_cannot_be_checked_without_the_password() {
        let data = b"a secret that is easy to guess";
        let salt_and_iv = [3u8; SALT_AND_IV_LEN];
        let encrypted = EncodeOptions {
            password: Some("pw".into()),
            ..EncodeOptions::default()
        };
        let sealed = Payload::prepare(data, &encrypted, Some(salt_and_iv), true)
            .unwrap()
            .seal
            .unwrap();
        assert!(sealed.keyed);
        assert_ne!(sealed.digest, pbx::file_hash(data));
        let plain = Payload::prepare(data, &EncodeOptions::default(), None, true)
            .unwrap()
            .seal
            .unwrap();
        assert!(!plain.keyed);
        assert_eq!(plain.digest, pbx::file_hash(data));
        // What identifies the sheet comes from the bytes on paper, which anyone can see.
        let on_paper = Payload::prepare(data, &encrypted, Some(salt_and_iv), true).unwrap();
        assert_eq!(sealed.stream_hash, pbx::file_hash(&on_paper.bytes));
    }

    #[test]
    fn name_is_cut_on_a_character_boundary() {
        assert_eq!(truncate_at_char_boundary("aäb", 2), "a");
    }
}
