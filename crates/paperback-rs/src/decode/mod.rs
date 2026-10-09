// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider
// Derived from PaperBack 1.10, Copyright (c) 2007 Oleh Yuschuk; see NOTICE.md.

//! Reads pages back into the original file.
//!
//! The grid detection and block reading follow the algorithm of PaperBack 1.10
//! so that the same scans are accepted.

// The decoder is a close port of index-based C code; keeping the loops recognisable eases comparison.
#![allow(clippy::needless_range_loop)]

mod assemble;
mod bitmap;
mod detector;
pub(crate) mod grid;
pub mod hints;
mod known;
pub(crate) mod mesh;
mod peaks;
mod positions;
mod reader;
mod sheet;
mod skew;

pub use assemble::{PageStatistics, Report, RestoredFile};
pub use hints::ScanHints;

use std::collections::{HashMap, HashSet};

use assemble::{Assembler, ScannedBlock, ScannedPage};
use bitmap::Bitmap;
use known::{KnownBlocks, Placement};
use reader::{BlockOutcome, BlockReader, Effort};
use sheet::{BlockKey, LatticeRead, Relation, Sheets, corner_cells, sample_cells};

use crate::block::{BLOCK_DOTS, BlockKind, MAX_FILE_SIZE, RawBlock, SuperBlock};
use crate::error::{Error, Result};
use crate::pbx::{self, Record, SheetId};
use crate::quality::{BlockQuality, PageQuality};
use crate::raster::{Raster, Turn, WHITE};

/// Dot pitch in pixels the reader works best with. Scans at several times the
/// dot density are averaged down to it: the dots are then sampled with a window
/// that forgives the small bends and shifts of crumpled or badly flattened paper.
const WORKING_DOT_PITCH: f64 = 3.0;

/// By how much a scan with this grid is reduced; 1 leaves it as it is.
fn reduction_factor(grid: &grid::Grid) -> usize {
    let pitch = grid.x_step.min(grid.y_step) / (BLOCK_DOTS + 3) as f64;
    (pitch / WORKING_DOT_PITCH + 0.2) as usize
}

/// The extras of reading a page that cost time or memory, asked for by the caller.
#[derive(Clone, Copy)]
struct Wanted {
    quality: bool,
    diagnose: bool,
}

/// Parts of a picture, as shares of its width and height, on which the grid is fitted when
/// fitting it on the whole picture does not lead to readable blocks. On a page that is bent
/// or photographed at an angle the lines are the most nearly straight and evenly spaced in
/// the middle; the rest is followed by the corner mesh.
const CENTRAL_SHARES: [f64; 5] = [0.7, 0.5, 0.4, 0.3, 0.2];

/// Finds where the grid is: on the whole picture if that reads, else on its middle.
fn find_grid(bitmap: &Bitmap) -> Result<(grid::Grid, grid::Intensity)> {
    let whole = grid::locate(bitmap);
    if let Ok((found, intensity)) = &whole
        && reads_something(bitmap, *found, *intensity)
    {
        return whole;
    }
    for share in CENTRAL_SHARES {
        let (width, height) = (
            (bitmap.width() as f64 * share) as usize,
            (bitmap.height() as f64 * share) as usize,
        );
        let (left, bottom) = ((bitmap.width() - width) / 2, (bitmap.height() - height) / 2);
        let middle = bitmap.cropped(left, bottom, width, height);
        if let Ok((found, intensity)) = grid::locate(&middle) {
            let found = found.translated(left as f64, bottom as f64);
            if reads_something(bitmap, found, intensity) {
                return Ok((found, intensity));
            }
        }
    }
    whole
}

fn reads_something(bitmap: &Bitmap, found: grid::Grid, intensity: grid::Intensity) -> bool {
    BlockReader::new(bitmap, found, intensity).finds_blocks(PROBE_COLUMNS, PROBE_ROWS)
}

/// Reads the bitmap as it is: grid search, then every block.
/// What reading a picture that is a transformation of the original needs to know about it.
#[derive(Clone)]
struct Reading<'a> {
    wanted: Wanted,
    /// How the picture relates to the one the caller gave.
    placement: Placement,
    /// Where blocks were read in the original already; they are not read again.
    known: &'a KnownBlocks,
    /// The label found by an earlier reading.
    label: Option<SuperBlock>,
    /// Blocks read from other pictures of the same sheet.
    sheets: &'a Sheets,
}

/// Share of a block's size within which a block read before counts as the one in this cell.
const SAME_BLOCK_SHARE: f64 = 0.4;

fn scan_bitmap(raster: &Raster, reading: &Reading, reduced_by: usize) -> Result<ScannedPage> {
    let wanted = reading.wanted;
    let bitmap = Bitmap::from_raster(raster);
    let (grid, intensity) = find_grid(&bitmap)?;
    let factor = reduction_factor(&grid);
    if let Some(smaller) = raster.reduced(factor).filter(|_| factor > 1) {
        let mut page = scan_bitmap(&smaller, reading, factor)?;
        attach_hints(&mut page, raster, dot_pitch(&grid), wanted);
        return Ok(page);
    }
    let mut reader = BlockReader::new(&bitmap, grid, intensity);
    reader.tune_sharpness(TUNING_COLUMNS, TUNING_ROWS);
    if !reader.finds_blocks(PROBE_COLUMNS, PROBE_ROWS) {
        return Err(Error::Decode("no readable blocks found in the grid".into()));
    }

    let (columns, rows) = (reader.columns(), reader.rows());
    let height = bitmap.height() as f64;
    let in_original = |point: (f64, f64)| {
        // The bitmap's first row is the bottom of the picture.
        reading
            .placement
            .to_original((point.0, height - 1.0 - point.1), reduced_by)
    };
    let block_size = reader.grid().x_step.min(reader.grid().y_step) * reduced_by as f64;
    let (mut outcomes, sheet) = quick_pass(
        &mut reader,
        reading,
        (columns, rows),
        in_original,
        SAME_BLOCK_SHARE * block_size,
    );
    // Then passes of more effort over the blocks that are still missing, and only those.
    for effort in [Effort::Normal, Effort::Deep] {
        reader.set_effort(effort);
        if effort == Effort::Deep {
            train_detector(&mut reader, &mut outcomes, columns);
        }
        follow_bent_paper(&mut reader, &mut outcomes, columns, rows);
    }
    let collected = collect(
        outcomes,
        columns,
        |column, row| reader.read_position(column, row).map(in_original),
        |column, row| in_original(reader.cell_centre(column, row)),
    );
    // The label of an earlier reading serves a reading that skipped the cells that held it.
    let label = collected
        .label
        .or_else(|| reading.label.clone())
        // A picture that shows no label block but is found to show a sheet read before.
        .or_else(|| sheet.map(|(index, _)| reading.sheets.label_of(index)))
        .ok_or(Error::NoReadablePage)?;
    let quality = wanted.quality.then(|| {
        PageQuality::new(
            raster.clone(),
            reader.grid(),
            columns,
            rows,
            collected.cells,
            reader.shift_map(),
            reader.cell_quads(),
        )
    });
    let mut page = ScannedPage {
        label,
        group_size: collected.group_size,
        blocks: collected.blocks,
        statistics: collected.statistics,
        read_places: collected.read_places,
        block_size,
        lattice: LatticeRead {
            columns,
            rows,
            keys: collected.keys,
            sheet,
        },
        records: Vec::new(),
        quality,
    };
    // A reduced picture shows nothing of the scanner's settings; the caller looks at the original.
    if reduced_by == 1 {
        attach_hints(&mut page, raster, dot_pitch(&reader.grid()), wanted);
    }
    Ok(page)
}

/// Blocks read in the first look at a picture; more would only cost time.
const ENOUGH_SAMPLES: usize = 6;

/// What a first look at a picture found: the blocks read, the keys of those that tell where
/// they belong, and the identifier of the sheet if a record showed it.
#[derive(Default)]
struct FirstLook {
    outcomes: Vec<Option<BlockOutcome>>,
    samples: Vec<((usize, usize), BlockKey)>,
    shown_id: Option<SheetId>,
}

impl FirstLook {
    fn add(&mut self, columns: usize, cell: (usize, usize), outcome: BlockOutcome) {
        if let BlockOutcome::Readable { block, .. } = &outcome {
            self.samples.extend(block_key(block).map(|key| (cell, key)));
            self.shown_id = self.shown_id.or_else(|| sheet_id_in(block));
        }
        self.outcomes[cell.1 * columns + cell.0] = Some(outcome);
    }
}

/// Reads a few cells of a picture that no earlier reading covers and finds out which sheet read
/// before the picture shows, and how its cells relate. The middle of the picture is tried
/// first; a picture that matches no sheet by it may show the identifier of a sheet in a corner.
/// The cells are read with more effort than the quick pass gives, so that they do read.
fn look_for_sheet(
    reader: &mut BlockReader,
    reading: &Reading,
    (columns, rows): (usize, usize),
    in_original: &impl Fn((f64, f64)) -> (f64, f64),
    same_block_within: f64,
) -> (Vec<Option<BlockOutcome>>, Option<(usize, Relation)>) {
    let mut look = FirstLook {
        outcomes: (0..columns * rows).map(|_| None).collect(),
        ..FirstLook::default()
    };
    if reading.sheets.is_empty() {
        return (look.outcomes, None);
    }
    reader.set_effort(Effort::Normal);
    let read_unknown = |reader: &mut BlockReader, look: &mut FirstLook, cell: (usize, usize)| {
        let place = in_original(reader.cell_centre(cell.0, cell.1));
        if !reading.known.is_near(place, same_block_within) {
            let outcome = reader.read(cell.0, cell.1);
            look.add(columns, cell, outcome);
        }
    };
    for cell in sample_cells(columns, rows) {
        if look.samples.len() >= ENOUGH_SAMPLES {
            break;
        }
        read_unknown(reader, &mut look, cell);
    }
    let mut sheet = reading.sheets.recognise(&look.samples, look.shown_id);
    if sheet.is_none() && look.shown_id.is_none() {
        for cell in corner_cells(columns, rows) {
            read_unknown(reader, &mut look, cell);
        }
        sheet = reading.sheets.recognise(&look.samples, look.shown_id);
    }
    reader.set_effort(Effort::Quick);
    (look.outcomes, sheet)
}

/// The first pass over all cells of a picture at the lowest effort, skipping the cells where a
/// block was read already: in an earlier reading of this picture, or in another picture of the
/// sheet. A first look at a few cells says whether other pictures of the sheet were read and
/// how their cells relate to these. Also returns the sheet the picture was found to show.
fn quick_pass(
    reader: &mut BlockReader,
    reading: &Reading,
    (columns, rows): (usize, usize),
    in_original: impl Fn((f64, f64)) -> (f64, f64),
    same_block_within: f64,
) -> (Vec<BlockOutcome>, Option<(usize, Relation)>) {
    let (mut first_look, sheet) = look_for_sheet(
        reader,
        reading,
        (columns, rows),
        &in_original,
        same_block_within,
    );
    let known_in_sheet = sheet.map(|found| reading.sheets.known_cells(found, columns, rows));
    let mut outcomes: Vec<BlockOutcome> = Vec::with_capacity(columns * rows);
    for row in 0..rows {
        for column in 0..columns {
            let at = row * columns + column;
            let place = in_original(reader.cell_centre(column, row));
            outcomes.push(if let Some(done) = first_look[at].take() {
                done
            } else if reading.known.is_near(place, same_block_within)
                || known_in_sheet.as_ref().is_some_and(|known| known[at])
            {
                BlockOutcome::Skipped
            } else {
                reader.read(column, row)
            });
        }
    }
    (outcomes, sheet)
}

/// The identifier of the sheet a block says it belongs to, if it is a sheet record.
fn sheet_id_in(block: &RawBlock) -> Option<SheetId> {
    match (
        BlockKind::of(block.address()),
        Record::parse(&block.payload_array()),
    ) {
        (BlockKind::Data { .. }, Some(Record::Sheet(sheet))) => Some(sheet.id),
        _ => None,
    }
}

/// What the outcomes of reading all cells of a page add up to.
struct Collected {
    label: Option<SuperBlock>,
    group_size: usize,
    blocks: Vec<ScannedBlock>,
    statistics: PageStatistics,
    cells: Vec<BlockQuality>,
    /// Where in the original picture blocks were read.
    read_places: Vec<(f64, f64)>,
    /// The key of the block read in each cell.
    keys: Vec<Option<BlockKey>>,
}

/// Sorts the blocks that were read into data, recovery and label, and counts the rest.
/// `place_of` tells where in the original picture the block of a cell was read, `centre_of`
/// where the cell is.
fn collect(
    outcomes: Vec<BlockOutcome>,
    columns: usize,
    place_of: impl Fn(usize, usize) -> Option<(f64, f64)>,
    centre_of: impl Fn(usize, usize) -> (f64, f64),
) -> Collected {
    let mut collected = Collected {
        label: None,
        group_size: 0,
        blocks: Vec::new(),
        statistics: PageStatistics::default(),
        cells: Vec::with_capacity(outcomes.len()),
        read_places: Vec::new(),
        keys: Vec::with_capacity(outcomes.len()),
    };
    for (at, outcome) in outcomes.into_iter().enumerate() {
        let (column, row) = (at % columns, at / columns);
        collected.cells.push(match &outcome {
            BlockOutcome::Missing | BlockOutcome::Skipped => BlockQuality::Absent,
            BlockOutcome::Unreadable => BlockQuality::Unreadable,
            BlockOutcome::Readable { corrected, .. } => BlockQuality::Readable(*corrected),
        });
        collected.keys.push(match &outcome {
            BlockOutcome::Readable { block, .. } => block_key(block),
            _ => None,
        });
        match outcome {
            BlockOutcome::Missing => {}
            // A block known from elsewhere: a later reading need not read this place either.
            BlockOutcome::Skipped => collected.read_places.push(centre_of(column, row)),
            BlockOutcome::Unreadable => collected.statistics.bad_blocks += 1,
            BlockOutcome::Readable { block, corrected } => {
                collected.read_places.extend(place_of(column, row));
                collected.statistics.restored_bytes += corrected;
                collected.sort_block(&block);
            }
        }
    }
    collected
}

/// Where a block belongs in the file; `None` for labels, which every string carries.
fn block_key(block: &RawBlock) -> Option<BlockKey> {
    match BlockKind::of(block.address()) {
        BlockKind::Superblock => None,
        // Records are the same on every sheet of a file and say nothing about the cell.
        BlockKind::Data { .. } if pbx::is_record(&block.payload_array()) => None,
        BlockKind::Data { offset } => Some((false, offset)),
        BlockKind::Recovery { offset, .. } => Some((true, offset)),
    }
}

impl Collected {
    fn sort_block(&mut self, block: &RawBlock) {
        match BlockKind::of(block.address()) {
            BlockKind::Superblock => {
                let candidate = SuperBlock::from_raw(block);
                if label_is_plausible(&candidate) {
                    self.label = Some(candidate);
                }
                self.statistics.superblocks += 1;
            }
            BlockKind::Data { offset } => {
                self.statistics.good_blocks += 1;
                self.blocks.push(ScannedBlock::Data {
                    offset,
                    payload: block.payload_array(),
                });
            }
            BlockKind::Recovery { offset, group_size } => {
                self.statistics.good_blocks += 1;
                self.group_size = group_size;
                self.blocks.push(ScannedBlock::Recovery {
                    offset,
                    group_size,
                    payload: block.payload_array(),
                });
            }
        }
    }
}

/// Pixels between two dots in a picture with this grid.
fn dot_pitch(grid: &grid::Grid) -> f64 {
    grid.x_step.min(grid.y_step) / (BLOCK_DOTS + 3) as f64
}

/// More corrected bytes per block than this is a page that read with difficulty.
const TROUBLE_CORRECTIONS_PER_BLOCK: usize = 4;

/// Diagnoses the scan, but only for pages that read badly: a page that read cleanly needs no advice.
fn attach_hints(page: &mut ScannedPage, raster: &Raster, pixels_per_dot: f64, wanted: Wanted) {
    if !wanted.diagnose {
        return;
    }
    let stats = &page.statistics;
    let blocks = stats.good_blocks.max(1);
    let in_trouble =
        stats.bad_blocks > 0 || stats.restored_bytes > TROUBLE_CORRECTIONS_PER_BLOCK * blocks;
    if in_trouble {
        page.statistics.hints = hints::assess(raster, pixels_per_dot);
    }
}

/// Rounds of rereading blocks next to ones that read, so that a crease is
/// followed block by block from its readable edge inwards.
const FOLLOW_ROUNDS: usize = 64;

/// How many of the cells around (`column`, `row`) hold a block that was read.
fn readable_neighbours(
    outcomes: &[BlockOutcome],
    (column, row): (usize, usize),
    (columns, rows): (usize, usize),
) -> usize {
    (row.saturating_sub(1)..=(row + 1).min(rows - 1))
        .flat_map(|r| {
            (column.saturating_sub(1)..=(column + 1).min(columns - 1)).map(move |c| (c, r))
        })
        .filter(|&(c, r)| (c, r) != (column, row))
        .filter(|&(c, r)| matches!(outcomes[r * columns + c], BlockOutcome::Readable { .. }))
        .count()
}

/// Reads the cells around (`column`, `row`) that were skipped as already known, for what they
/// tell about the page; the block is the same one and is not added a second time.
fn read_skipped_neighbours(
    reader: &mut BlockReader,
    outcomes: &mut [BlockOutcome],
    (column, row): (usize, usize),
    (columns, rows): (usize, usize),
) {
    for r in row.saturating_sub(1)..=(row + 1).min(rows - 1) {
        for c in column.saturating_sub(1)..=(column + 1).min(columns - 1) {
            if matches!(outcomes[r * columns + c], BlockOutcome::Skipped) {
                outcomes[r * columns + c] = reader.read(c, r);
            }
        }
    }
}

/// Blocks read in order to teach the detector what dots look like on this page, when too few
/// were read in this reading.
const TRAINING_BLOCKS: usize = 6;

/// Reads a few of the skipped cells while the detector knows too little, so that what it
/// learns serves the blocks that are missing.
fn train_detector(reader: &mut BlockReader, outcomes: &mut [BlockOutcome], columns: usize) {
    let mut read = 0;
    for at in 0..outcomes.len() {
        if read >= TRAINING_BLOCKS || !reader.detector_needs_blocks() {
            break;
        }
        if matches!(outcomes[at], BlockOutcome::Skipped) {
            outcomes[at] = reader.read(at % columns, at / columns);
            read += 1;
        }
    }
}

/// Rereads the blocks that are still missing, at the effort the reader is set to and at
/// the positions their readable neighbours show; blocks that were read are left alone.
fn follow_bent_paper(
    reader: &mut BlockReader,
    outcomes: &mut [BlockOutcome],
    columns: usize,
    rows: usize,
) {
    // What a block's neighbours told when it was last tried; trying again is of use only when
    // more neighbours have been read since.
    let mut tried_with = vec![usize::MAX; outcomes.len()];
    for _ in 0..FOLLOW_ROUNDS {
        let mut improved = false;
        for row in 0..rows {
            for column in 0..columns {
                let at = row * columns + column;
                let worth_retrying = match outcomes[at] {
                    BlockOutcome::Readable { .. } | BlockOutcome::Skipped => false,
                    BlockOutcome::Unreadable => true,
                    BlockOutcome::Missing => {
                        reader.has_neighbour_to_learn_from(column, row)
                            || reader.mesh_covers(column, row)
                    }
                };
                if !worth_retrying {
                    continue;
                }
                let readable_around = readable_neighbours(outcomes, (column, row), (columns, rows));
                if tried_with[at] == readable_around {
                    continue;
                }
                tried_with[at] = readable_around;
                // The neighbours' positions and rotations guide the retry: those that were not
                // read in this reading, because a block was read there before, are read now.
                read_skipped_neighbours(reader, outcomes, (column, row), (columns, rows));
                let retry = reader.read(column, row);
                if matches!(retry, BlockOutcome::Readable { .. }) {
                    improved = true;
                    outcomes[at] = retry;
                }
            }
        }
        if !improved {
            break;
        }
    }
}

/// If the page does not read completely as it is, it is straightened by the
/// estimated skew; further attempts nudge that angle, because the grid search
/// can alias at an unlucky angle on very clean images.
const NUDGES: [f64; 4] = [0.7, -0.7, 2.0, -2.0];

/// Blocks tried when judging whether a grid fit is any good.
/// Blocks read to choose the sharpness level for a page.
const TUNING_COLUMNS: usize = 5;
const TUNING_ROWS: usize = 5;
const PROBE_COLUMNS: usize = 4;
const PROBE_ROWS: usize = 4;
/// A page needs no further attempts when every data block it carries is read or can
/// be rebuilt: a group of blocks survives the loss of one block if its recovery
/// block is there.
fn reads_cleanly(page: &ScannedPage) -> bool {
    let label = &page.label;
    let page_size = label.page_size as usize;
    let before = usize::from(label.page)
        .saturating_sub(1)
        .saturating_mul(page_size);
    let on_page = (label.data_size as usize)
        .saturating_sub(before)
        .min(page_size);
    let first = before / crate::block::DATA_LEN;
    let data_blocks = on_page.div_ceil(crate::block::DATA_LEN);
    let data_read: HashSet<usize> = page
        .blocks
        .iter()
        .filter_map(|block| match block {
            ScannedBlock::Data { offset, .. } => Some(*offset as usize / crate::block::DATA_LEN),
            ScannedBlock::Recovery { .. } => None,
        })
        .collect();
    let recovery_groups: HashSet<usize> = page
        .blocks
        .iter()
        .filter_map(|block| match block {
            ScannedBlock::Recovery {
                offset, group_size, ..
            } => Some(*offset as usize / (group_size * crate::block::DATA_LEN)),
            ScannedBlock::Data { .. } => None,
        })
        .collect();
    let mut missing_per_group: HashMap<usize, usize> = HashMap::new();
    for index in (first..first + data_blocks).filter(|index| !data_read.contains(index)) {
        let group = match page.group_size {
            0 => index,
            size => index / size,
        };
        *missing_per_group.entry(group).or_default() += 1;
    }
    missing_per_group.iter().all(|(group, &missing)| {
        page.group_size > 0 && missing == 1 && recovery_groups.contains(group)
    })
}

/// The places where blocks were read in the attempts so far.
fn known_blocks(page: Option<&ScannedPage>) -> KnownBlocks {
    let Some(page) = page else {
        return KnownBlocks::default();
    };
    let mut known = KnownBlocks::new(page.block_size);
    for &place in &page.read_places {
        known.add(place);
    }
    known
}

/// The readings of one page so far: the blocks of all of them, and the last failure.
#[derive(Default)]
struct Readings {
    combined: Option<ScannedPage>,
    last_error: Option<Error>,
}

impl Readings {
    /// Adds a reading; whether the page is complete now.
    fn add(&mut self, page: Result<ScannedPage>) -> bool {
        match page {
            Ok(page) => {
                let merged = match self.combined.take() {
                    Some(mut earlier) => {
                        earlier.absorb(page);
                        earlier
                    }
                    None => page,
                };
                let complete = reads_cleanly(&merged);
                self.combined = Some(merged);
                complete
            }
            Err(error) => {
                self.last_error = Some(error);
                false
            }
        }
    }

    fn into_result(self) -> Result<ScannedPage> {
        self.combined
            .ok_or_else(|| self.last_error.unwrap_or(Error::NoReadablePage))
    }
}

/// Reads one page image into blocks and their statistics. When the page does
/// not read completely as it is, it is turned and read again; blocks found in
/// any attempt are combined. A turned picture is read only where no block was read
/// before, and nothing that was read is ever dropped.
fn scan_page(raster: &Raster, wanted: Wanted, sheets: &Sheets) -> Result<ScannedPage> {
    let mut readings = Readings::default();
    let nothing_known = KnownBlocks::default();
    let first = Reading {
        wanted,
        placement: Placement::default(),
        known: &nothing_known,
        label: None,
        sheets,
    };
    if readings.add(scan_bitmap(raster, &first, 1)) {
        return readings.into_result();
    }
    let skew = skew::estimate(raster);
    let angles = std::iter::once(skew).chain(NUDGES.iter().map(|nudge| skew + nudge));
    for degrees in angles {
        if degrees.abs() < skew::NEGLIGIBLE_DEGREES {
            continue;
        }
        let Some(turned) = raster.rotated(degrees, WHITE) else {
            continue;
        };
        let known = known_blocks(readings.combined.as_ref());
        let (turn, _, _) = Turn::new(raster.width(), raster.height(), degrees);
        let turned_reading = Reading {
            wanted,
            placement: Placement::turned(turn),
            known: &known,
            label: readings.combined.as_ref().map(|page| page.label.clone()),
            sheets,
        };
        if readings.add(scan_bitmap(&turned, &turned_reading, 1)) {
            break;
        }
    }
    readings.into_result()
}

/// A label that passes its CRC can still be garbage or hostile; sizes beyond what
/// the format can address would only make the assembler allocate absurd amounts.
fn label_is_plausible(label: &SuperBlock) -> bool {
    let limit = MAX_FILE_SIZE as usize + crate::codec::ALIGNMENT;
    (1..=limit).contains(&(label.data_size as usize))
        && label.original_size as usize <= limit
        && label.page >= 1
}

/// What the caller learns about each page while decoding.
#[derive(Clone, Debug)]
pub struct PageOutcome {
    /// Position of the page in the list given to [`decode`], counting from 0.
    pub index: usize,
    /// Block statistics, or the reason the page was skipped.
    pub result: std::result::Result<PageStatistics, String>,
    /// Block by block quality, when [`DecodeOptions::quality`] is set and the page was read.
    pub quality: Option<PageQuality>,
}

#[derive(Clone, Debug, Default)]
/// Settings for [`decode`].
pub struct DecodeOptions {
    /// Needed only for encrypted backups.
    pub password: Option<String>,
    /// Keep the block quality of every page (see [`PageOutcome::quality`]); costs a copy of each page.
    pub quality: bool,
    /// Look at pages that read badly for likely causes (see [`PageStatistics::hints`]); costs a
    /// pass over the pixels of each such page.
    pub diagnose: bool,
}

/// Decodes the given pages (in any order) into the original file. Unreadable
/// pages are skipped and reported through `on_page`; decoding succeeds as long
/// as the remaining pages contain the whole file.
pub fn decode(
    pages: &[Raster],
    options: &DecodeOptions,
    mut on_page: impl FnMut(PageOutcome),
) -> Result<RestoredFile> {
    let mut assembler = Assembler::new();
    let mut sheets = Sheets::default();
    for (index, raster) in pages.iter().enumerate() {
        // Pictures after the one that completed the file are not read at all.
        if assembler.is_complete() {
            on_page(PageOutcome {
                index,
                result: Err("not read: the file is complete already".into()),
                quality: None,
            });
            continue;
        }
        match scan_page(
            raster,
            Wanted {
                quality: options.quality,
                diagnose: options.diagnose,
            },
            &sheets,
        ) {
            Ok(mut page) => {
                positions::examine(&mut page);
                sheets.record(&page.lattice, &page.label, page.statistics.sheet_id);
                on_page(PageOutcome {
                    index,
                    result: Ok(page.statistics),
                    quality: page.quality.take(),
                });
                assembler.add_page(&page);
            }
            Err(error) => on_page(PageOutcome {
                index,
                result: Err(error.to_string()),
                quality: None,
            }),
        }
    }
    assembler.finish(options.password.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::DATA_LEN;
    use crate::codec::Compression;
    use crate::encode::{EncodeOptions, encode};
    use crate::layout::{PageLayout, PageSetup};

    const FILE_LEN: usize = 3000;
    const GROUP: usize = 5;

    #[test]
    fn blocks_wiped_from_the_page_are_rebuilt_from_recovery_blocks() {
        let data: Vec<u8> = (0..FILE_LEN).map(|i| (i * 131 + i / 3) as u8).collect();
        let options = EncodeOptions {
            setup: PageSetup {
                printer_dpi: 300,
                dot_dpi: 100,
                ..PageSetup::default()
            },
            compression: Compression::None,
            ..EncodeOptions::default()
        };
        let layout = PageLayout::compute(&options.setup, options.redundancy).unwrap();
        let raster = encode(&data, &options).unwrap().remove(0).raster;

        // Three neighbouring cells of one string belong to three different groups.
        let string_len = FILE_LEN.div_ceil(DATA_LEN).div_ceil(GROUP) + 1;
        assert!(string_len < layout.nx);
        let (width, height) = (raster.width(), raster.height());
        let mut pixels = raster.into_pixels();
        let top_band = layout.text.map_or(0, |text| text.top_band());
        // The blocks start after the cells kept for the records.
        let first = crate::plan::RECORD_CELLS_PER_END + string_len;
        for cell in first + 2..first + 5 {
            let (x, y) = layout.cell_origin(cell);
            for row in top_band + y..top_band + y + 32 * layout.dy {
                pixels[row * width + x..row * width + x + 32 * layout.dx].fill(255);
            }
        }
        let raster = Raster::from_pixels(width, height, pixels).unwrap();

        let file = decode(&[raster], &DecodeOptions::default(), |_| {}).unwrap();
        assert_eq!(file.data, data);
        assert_eq!(file.report.recovered_blocks, 3);
    }

    /// A page of the small file, and the page geometry to find its cells.
    fn small_page() -> (Vec<u8>, Raster, PageLayout, usize) {
        let data: Vec<u8> = (0..FILE_LEN).map(|i| (i * 131 + i / 3) as u8).collect();
        let options = EncodeOptions {
            setup: PageSetup {
                printer_dpi: 300,
                dot_dpi: 100,
                ..PageSetup::default()
            },
            compression: Compression::None,
            ..EncodeOptions::default()
        };
        let layout = PageLayout::compute(&options.setup, options.redundancy).unwrap();
        let raster = encode(&data, &options).unwrap().remove(0).raster;
        let bands = layout.text.map_or(0, |t| t.top_band() + t.bottom_band());
        let rows = (1..=layout.ny)
            .find(|&rows| layout.image_height(rows) + bands == raster.height())
            .unwrap();
        (data, raster, layout, rows)
    }

    fn wiped(raster: Raster, layout: &PageLayout, cells: &[usize]) -> Raster {
        let (width, height) = (raster.width(), raster.height());
        let mut pixels = raster.into_pixels();
        let top_band = layout.text.map_or(0, |text| text.top_band());
        for &cell in cells {
            let (x, y) = layout.cell_origin(cell);
            for row in top_band + y..top_band + y + 32 * layout.dy {
                pixels[row * width + x..row * width + x + 32 * layout.dx].fill(255);
            }
        }
        Raster::from_pixels(width, height, pixels).unwrap()
    }

    #[test]
    fn the_records_survive_the_loss_of_one_end_of_the_page() {
        use crate::plan::RECORD_CELLS_PER_END;
        let (data, raster, layout, rows) = small_page();
        let last = layout.nx * rows - 1;
        for lost in [
            (0..RECORD_CELLS_PER_END).collect::<Vec<_>>(),
            (last + 1 - RECORD_CELLS_PER_END..=last).collect(),
        ] {
            let page = wiped(raster.clone(), &layout, &lost);
            let file = decode(&[page], &DecodeOptions::default(), |_| {}).unwrap();
            assert_eq!(file.data, data);
            assert_eq!(file.report.integrity, crate::Integrity::Verified);
        }
    }

    #[test]
    fn a_lost_record_is_rebuilt_from_the_parity_record() {
        let (data, raster, layout, rows) = small_page();
        let last = layout.nx * rows - 1;
        // The hash record at both ends: the second cell of the first set, the middle one of the last.
        let page = wiped(raster, &layout, &[1, last - 1]);
        let file = decode(&[page], &DecodeOptions::default(), |_| {}).unwrap();
        assert_eq!(file.data, data);
        assert_eq!(file.report.integrity, crate::Integrity::Verified);
    }

    #[test]
    fn a_page_that_lost_all_its_records_still_restores_the_file() {
        use crate::plan::RECORD_CELLS_PER_END;
        let (data, raster, layout, rows) = small_page();
        let last = layout.nx * rows - 1;
        let both_ends: Vec<usize> = (0..RECORD_CELLS_PER_END)
            .chain(last + 1 - RECORD_CELLS_PER_END..=last)
            .collect();
        let page = wiped(raster, &layout, &both_ends);
        let file = decode(&[page], &DecodeOptions::default(), |_| {}).unwrap();
        assert_eq!(file.data, data);
        assert_eq!(file.report.integrity, crate::Integrity::Missing);
    }

    /// Writes the pages of a file with damaged record cells, for `tools/interop/damaged.ps1`,
    /// which has the original programs and an older paperback.rs read them. Settings as
    /// `tools/interop/matrix.ps1` uses.
    #[test]
    #[ignore = "writes files; run by tools/interop/damaged.ps1"]
    fn write_pages_with_damaged_records() {
        use crate::encode::Page;
        use crate::plan::RECORD_CELLS_PER_END as END;
        let (Ok(input), Ok(out)) = (std::env::var("PB_INPUT"), std::env::var("PB_OUT")) else {
            return;
        };
        let data = std::fs::read(input).unwrap();
        let options = EncodeOptions {
            setup: PageSetup {
                printer_dpi: 300,
                dot_dpi: 200,
                ..PageSetup::default()
            },
            ..EncodeOptions::default()
        };
        let layout = PageLayout::compute(&options.setup, options.redundancy).unwrap();
        let bands = layout.text.map_or(0, |t| t.top_band() + t.bottom_band());
        for (at, page) in encode(&data, &options).unwrap().into_iter().enumerate() {
            let rows = (1..=layout.ny)
                .find(|&rows| layout.image_height(rows) + bands == page.raster.height())
                .unwrap();
            let last = layout.nx * rows - 1;
            let cases: [(&str, Vec<usize>); 3] = [
                ("head", (0..END).collect()),
                ("ends", (0..END).chain(last + 1 - END..=last).collect()),
                ("hash", vec![1, last - 1]),
            ];
            for (name, cells) in cases {
                let raster = wiped(page.raster.clone(), &layout, &cells);
                let damaged = Page {
                    raster,
                    dpi: page.dpi,
                };
                let path = format!("{out}/{name}-{:02}.bmp", at + 1);
                std::fs::write(path, crate::imageio::write_bmp(&damaged)).unwrap();
            }
        }
    }
}
