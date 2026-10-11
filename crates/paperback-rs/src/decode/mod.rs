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
use reader::{BlockOutcome, BlockReader, Effort, Speculation};
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
    threads: usize,
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
    /// Whether the passes of more effort are left out once the parity blocks read cover the
    /// blocks that are missing.
    parity_suffices: bool,
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
    reader.set_threads(wanted.threads);
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
    // The cells the first pass could not read: a block read there later took more effort.
    let failed_first: Vec<bool> = outcomes
        .iter()
        .map(|outcome| matches!(outcome, BlockOutcome::Missing | BlockOutcome::Unreadable))
        .collect();
    // Then passes of more effort over the blocks that are still missing, and only those, unless
    // the parity blocks read already cover them.
    let mut parity_shortcut = false;
    for effort in [Effort::Normal, Effort::Deep] {
        if reading.parity_suffices && outcomes_covered_by_parity(&outcomes, reading.label.as_ref())
        {
            parity_shortcut = true;
            break;
        }
        reader.set_effort(effort);
        if effort == Effort::Deep {
            train_detector(&mut reader, &mut outcomes, columns);
        }
        follow_bent_paper(&mut reader, &mut outcomes, columns, rows);
    }
    let read_late: Vec<bool> = outcomes
        .iter()
        .zip(&failed_first)
        .map(|(outcome, &failed)| failed && matches!(outcome, BlockOutcome::Readable { .. }))
        .collect();
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
    let quality = cell_quality(
        &mut reader,
        wanted.quality.then(|| raster.clone()),
        collected.cells,
        in_original,
        (SAME_BLOCK_SHARE * block_size, read_late),
    );
    let mut statistics = collected.statistics;
    statistics.bad_blocks = quality.unreadable_blocks();
    statistics.effort_blocks = quality.blocks_read_with_effort();
    let mut page = ScannedPage {
        label,
        group_size: collected.group_size,
        blocks: collected.blocks,
        statistics,
        read_places: collected.read_places,
        block_size,
        lattice: LatticeRead {
            columns,
            rows,
            keys: collected.keys,
            sheet,
        },
        records: Vec::new(),
        quality: Some(quality),
        reads: collected.reads,
        parity_shortcut,
        corrections: collected.corrections,
    };
    // A reduced picture shows nothing of the scanner's settings; the caller looks at the original.
    if reduced_by == 1 {
        attach_hints(&mut page, raster, dot_pitch(&reader.grid()), wanted);
    }
    Ok(page)
}

/// The cells of a reading as read, for the counts of the page and for adding other readings of the
/// picture to them: `in_original` places a point of the reading in the picture the caller gave, and
/// a block within `reach` of the middle of a cell is its block; `read_late` tells the cells read only
/// in the passes after the first. The picture and the corners of the blocks, which take the grid
/// lines of the whole page to trace, only when the quality is to be drawn (`picture`).
fn cell_quality(
    reader: &mut BlockReader,
    picture: Option<Raster>,
    cells: Vec<BlockQuality>,
    in_original: impl Fn((f64, f64)) -> (f64, f64),
    (reach, read_late): (f64, Vec<bool>),
) -> PageQuality {
    let (columns, rows) = (reader.columns(), reader.rows());
    let shifts = reader.shift_map();
    let centres: Vec<(f64, f64)> = (0..columns * rows)
        .map(|at| {
            let (centre, shift) = (reader.cell_centre(at % columns, at / columns), shifts[at]);
            in_original((centre.0 + shift.0, centre.1 + shift.1))
        })
        .collect();
    let quads = if picture.is_some() {
        reader.cell_quads()
    } else {
        None
    };
    PageQuality::new(picture, reader.grid(), columns, rows, cells, shifts, quads)
        .with_placed_cells(centres, reach, read_late)
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
    // A few blocks that only tell which sheet this is are not worth building the corner mesh of
    // the page; whatever does not read here is read with it in the passes that follow.
    reader.allow_meshes(false);
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
    reader.allow_meshes(true);
    reader.set_effort(Effort::Quick);
    (look.outcomes, sheet)
}

/// The first pass over all cells of a picture at the lowest effort, skipping the cells where a
/// block was read already: in an earlier reading of this picture, or in another picture of the
/// sheet. A first look at a few cells says whether other pictures of the sheet were read and
/// how their cells relate to these. Also returns the sheet the picture was found to show.
/// When the quality is wanted the cells known from other pictures are read as well: the map
/// shows how each cell reads in this picture, not whether its block is known from elsewhere.
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
                || (!reading.wanted.quality
                    && known_in_sheet.as_ref().is_some_and(|known| known[at]))
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
    /// Bytes corrected in each data and recovery block read, by [`ScannedBlock::identity`].
    corrections: HashMap<(bool, u32), usize>,
    /// Every cell that read: where it lies in the original picture, and the bytes corrected.
    reads: Vec<((f64, f64), usize)>,
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
        corrections: HashMap::new(),
        reads: Vec::new(),
    };
    for (at, outcome) in outcomes.into_iter().enumerate() {
        let (column, row) = (at % columns, at / columns);
        collected.cells.push(match &outcome {
            BlockOutcome::Missing => BlockQuality::Absent,
            BlockOutcome::Skipped => BlockQuality::Known,
            BlockOutcome::Unreadable => BlockQuality::Unreadable,
            BlockOutcome::Readable { corrected, .. } => BlockQuality::Readable(*corrected),
        });
        collected.keys.push(match &outcome {
            BlockOutcome::Readable { block, .. } => block_key(block),
            _ => None,
        });
        match outcome {
            // Unreadable blocks are counted with the quality, which also counts the cells between
            // blocks that lost even their grid lines.
            BlockOutcome::Missing | BlockOutcome::Unreadable => {}
            // A block known from elsewhere: a later reading need not read this place either.
            BlockOutcome::Skipped => collected.read_places.push(centre_of(column, row)),
            BlockOutcome::Readable { block, corrected } => {
                let place = place_of(column, row);
                collected.read_places.extend(place);
                let place = place.unwrap_or_else(|| centre_of(column, row));
                collected.reads.push((place, corrected));
                collected.statistics.restored_bytes += corrected;
                collected.sort_block(&block, corrected);
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
    fn sort_block(&mut self, block: &RawBlock, corrected: usize) {
        match BlockKind::of(block.address()) {
            BlockKind::Superblock => {
                let candidate = SuperBlock::from_raw(block);
                if label_is_plausible(&candidate) {
                    self.label = Some(candidate);
                }
                self.statistics.superblocks += 1;
            }
            BlockKind::Data { offset } => {
                self.note_corrections((false, offset), corrected);
                self.statistics.good_blocks += 1;
                self.blocks.push(ScannedBlock::Data {
                    offset,
                    payload: block.payload_array(),
                });
            }
            BlockKind::Recovery { offset, group_size } => {
                self.note_corrections((true, offset), corrected);
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

    /// Keeps the fewest corrections a block was read with, when it was read in several cells.
    fn note_corrections(&mut self, identity: (bool, u32), corrected: usize) {
        let least = self.corrections.entry(identity).or_insert(corrected);
        *least = (*least).min(corrected);
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
) -> Vec<usize> {
    let mut read = Vec::new();
    for r in row.saturating_sub(1)..=(row + 1).min(rows - 1) {
        for c in column.saturating_sub(1)..=(column + 1).min(columns - 1) {
            if matches!(outcomes[r * columns + c], BlockOutcome::Skipped) {
                outcomes[r * columns + c] = reader.read(c, r);
                if matches!(outcomes[r * columns + c], BlockOutcome::Readable { .. }) {
                    read.push(r * columns + c);
                }
            }
        }
    }
    read
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

/// Whether the block (`column`, `row`) is to be tried again now, and with how many readable
/// neighbours it was last tried if so.
fn retry_with(
    reader: &mut BlockReader,
    outcomes: &[BlockOutcome],
    tried_with: &[usize],
    (columns, rows): (usize, usize),
    at: usize,
) -> Option<usize> {
    let (column, row) = (at % columns, at / columns);
    let worth_retrying = match outcomes[at] {
        BlockOutcome::Readable { .. } | BlockOutcome::Skipped => false,
        BlockOutcome::Unreadable => true,
        BlockOutcome::Missing => {
            reader.has_neighbour_to_learn_from(column, row) || reader.mesh_covers(column, row)
        }
    };
    if !worth_retrying {
        return None;
    }
    let readable_around = readable_neighbours(outcomes, (column, row), (columns, rows));
    (tried_with[at] != readable_around).then_some(readable_around)
}

/// Whether the cells `a` and `b` (indices) are the same or neighbours.
fn next_to(a: usize, b: usize, columns: usize) -> bool {
    (a % columns).abs_diff(b % columns) <= 1 && (a / columns).abs_diff(b / columns) <= 1
}

/// Whether a block around (`column`, `row`) was skipped as known: reading it is part of
/// reading this one.
fn has_skipped_neighbour(
    outcomes: &[BlockOutcome],
    (column, row): (usize, usize),
    (columns, rows): (usize, usize),
) -> bool {
    (row.saturating_sub(1)..=(row + 1).min(rows - 1)).any(|r| {
        (column.saturating_sub(1)..=(column + 1).min(columns - 1))
            .any(|c| matches!(outcomes[r * columns + c], BlockOutcome::Skipped))
    })
}

/// What a walk over the missing blocks knows about the readings made side by side so far.
struct Walk<'a> {
    /// Readings made and not used yet, each with how many blocks had been read in the walk when
    /// it was made.
    readings: HashMap<usize, (Speculation<'a>, usize)>,
    /// The blocks read in the walk, in order.
    read: Vec<usize>,
    /// How many blocks had been read when what every block starts from last changed.
    changed_at: usize,
    /// What every block starts from, as it was then.
    start: (Option<usize>, usize, usize),
}

impl<'a> Walk<'a> {
    fn new(reader: &BlockReader<'a>) -> Self {
        Self {
            readings: HashMap::new(),
            read: Vec::new(),
            changed_at: 0,
            start: reader.global_state(),
        }
    }

    /// Notes that the block `at` was read.
    fn note_read(&mut self, reader: &BlockReader<'a>, at: usize) {
        self.read.push(at);
        let now = reader.global_state();
        if now != self.start {
            self.start = now;
            self.changed_at = self.read.len();
        }
    }

    /// The blocks among `at` whose reading is missing or does not stand any more: a reading
    /// stands if none of the blocks read since it was made is next to its block or changed what
    /// every block starts from.
    fn to_read(&self, cells: &[usize], columns: usize) -> Vec<usize> {
        cells
            .iter()
            .copied()
            .filter(|at| {
                !self.readings.get(at).is_some_and(|&(_, version)| {
                    self.changed_at <= version
                        && !self.read[version..]
                            .iter()
                            .any(|&done| next_to(done, *at, columns))
                })
            })
            .collect()
    }
}

/// Goes over the blocks that are missing again and again, row by row, while that finds more.
/// A block is tried again only if more of its neighbours have been read since it was tried.
///
/// With several threads the next blocks to try are read side by side. What that gives is what
/// trying them one after the other gives: a reading is used only if no block read since it was
/// made is next to its block or changed what every block starts from (the orientation of the
/// page, the model of the dots); otherwise it is made again. After a block reads, the walk goes on
/// with the block after it, so that what the new block makes worth trying is seen at once.
fn follow_bent_paper(
    reader: &mut BlockReader,
    outcomes: &mut [BlockOutcome],
    columns: usize,
    rows: usize,
) {
    let size = (columns, rows);
    // What a block's neighbours told when it was last tried; trying again is of use only when
    // more neighbours have been read since.
    let mut tried_with = vec![usize::MAX; outcomes.len()];
    if outcomes
        .iter()
        .any(|o| matches!(o, BlockOutcome::Missing | BlockOutcome::Unreadable))
    {
        reader.prepare_meshes();
    }
    let mut walk = Walk::new(reader);
    for _ in 0..FOLLOW_ROUNDS {
        let mut improved = false;
        let mut next = 0;
        while next < outcomes.len() {
            let width = reader.batch_width();
            // The blocks the walk would try next, as long as none of them has to be read with
            // the help of a block that is read first.
            let mut batch: Vec<(usize, usize)> = Vec::new();
            let mut scan = next;
            while scan < outcomes.len() && batch.len() < width {
                if let Some(around) = retry_with(reader, outcomes, &tried_with, size, scan) {
                    let alone =
                        has_skipped_neighbour(outcomes, (scan % columns, scan / columns), size);
                    if alone && !batch.is_empty() {
                        break;
                    }
                    batch.push((scan, around));
                    scan += 1;
                    if alone {
                        break;
                    }
                } else {
                    scan += 1;
                }
            }
            next = scan;
            if batch.is_empty() {
                continue;
            }
            if width == 1
                || has_skipped_neighbour(
                    outcomes,
                    (batch[0].0 % columns, batch[0].0 / columns),
                    size,
                ) && batch.len() == 1
            {
                let (at, around) = batch[0];
                let (column, row) = (at % columns, at / columns);
                tried_with[at] = around;
                // The neighbours' positions and rotations guide the retry: those that were not
                // read in this reading, because a block was read there before, are read now.
                for read in read_skipped_neighbours(reader, outcomes, (column, row), size) {
                    walk.note_read(reader, read);
                }
                let retry = reader.read(column, row);
                if matches!(retry, BlockOutcome::Readable { .. }) {
                    improved = true;
                    outcomes[at] = retry;
                    walk.note_read(reader, at);
                }
                continue;
            }
            // Readings of these blocks that do not stand any more are made again, side by side.
            let cells_of: Vec<usize> = batch.iter().map(|&(at, _)| at).collect();
            let to_read = walk.to_read(&cells_of, columns);
            if !to_read.is_empty() {
                let cells: Vec<(usize, usize)> = to_read
                    .iter()
                    .map(|&at| (at % columns, at / columns))
                    .collect();
                let version = walk.read.len();
                // Cells after one that read were left out; the walk stops at that one.
                for (&at, reading) in to_read.iter().zip(reader.read_side_by_side(&cells)) {
                    if let Some(reading) = reading {
                        walk.readings.insert(at, (reading, version));
                    }
                }
            }
            for &(at, around) in &batch {
                let Some((reading, _)) = walk.readings.remove(&at) else {
                    continue;
                };
                tried_with[at] = around;
                if matches!(reading.outcome, BlockOutcome::Readable { .. }) {
                    outcomes[at] = reader.accept(reading, at % columns, at / columns);
                    improved = true;
                    walk.note_read(reader, at);
                    // The walk goes on after this block, looking at what it changes.
                    next = at + 1;
                    break;
                }
                reader.note_failures(&reading);
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
    let data_read: HashSet<usize> = page.blocks.iter().filter_map(file_block_index).collect();
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
    for index in own_blocks(&page.label).filter(|index| !data_read.contains(index)) {
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

/// The data blocks of the file that the page with this label carries; none for a page beyond
/// the end of the file, which [`label_is_plausible`] does not let through.
fn own_blocks(label: &SuperBlock) -> std::ops::Range<usize> {
    let page_size = label.page_size as usize;
    let before = usize::from(label.page)
        .saturating_sub(1)
        .saturating_mul(page_size);
    let on_page = (label.data_size as usize)
        .saturating_sub(before)
        .min(page_size);
    let first = before / crate::block::DATA_LEN;
    first..first + on_page.div_ceil(crate::block::DATA_LEN)
}

/// Whether the page lacks none of its data blocks, so that no other reading can add to it.
fn lacks_nothing(page: &ScannedPage) -> bool {
    let data_read: HashSet<usize> = page.blocks.iter().filter_map(file_block_index).collect();
    own_blocks(&page.label).all(|index| data_read.contains(&index))
}

/// Which data block of the file a block read is, by its address; `None` for a recovery block
/// and for a parity block, whose address lies between those of data blocks and would otherwise
/// pass for the one before it.
fn file_block_index(block: &ScannedBlock) -> Option<usize> {
    match *block {
        ScannedBlock::Data { offset, .. } if crate::parity::index_of(offset).is_none() => {
            Some(offset as usize / crate::block::DATA_LEN)
        }
        ScannedBlock::Data { .. } | ScannedBlock::Recovery { .. } => None,
    }
}

/// Whether the page leaves nothing to read for checking the file: it announces no PBX1 records
/// (pages of the originals, `--legacy`, `--no-extensions`), or a hash record that the file will be
/// checked against was read or can be rebuilt from its sheet and parity records. A page that stops
/// early must not lose the hash.
fn integrity_settled(label: &SuperBlock, blocks: &[ScannedBlock]) -> bool {
    if !label.mode.is_extended() {
        return true;
    }
    let records: Vec<Record> = blocks
        .iter()
        .filter_map(|block| block.record(label.data_size))
        .collect();
    pbx::with_missing_rebuilt(records)
        .iter()
        .any(|record| matches!(record, Record::Hash(hash) if assemble::hash_fits(label, hash)))
}

/// Whether the data blocks of the page that are neither read nor rebuilt from a recovery block
/// are covered by the parity blocks read on the page: in every stripe at least as many parity
/// blocks as missing data blocks. If every page covers its own losses so, the parity blocks of
/// all pages rebuild the file. A page that lacks nothing is covered without any parity block:
/// the passes of more effort would only try the empty cells around its blocks, and follow
/// the grid lines of the whole page to do so.
fn covered_by_parity(page: &ScannedPage) -> bool {
    blocks_covered_by_parity(&page.label, page.group_size, &page.blocks)
}

/// [`covered_by_parity`] for the blocks of a page with this label and group size.
fn blocks_covered_by_parity(
    label: &SuperBlock,
    group_size: usize,
    blocks: &[ScannedBlock],
) -> bool {
    let code =
        crate::parity::Code::new((label.data_size as usize).div_ceil(crate::block::DATA_LEN));
    let mut parity_per_stripe = vec![0usize; code.stripes()];
    // A parity block read in two cells (a grid one cell off) counts once, as for the assembler.
    let mut parity_read = HashSet::new();
    let mut data_read = HashSet::new();
    let mut recovery_groups = HashSet::new();
    for block in blocks {
        match *block {
            ScannedBlock::Data { offset, .. } => match crate::parity::index_of(offset) {
                Some(index) => {
                    if let Some(stripe) = code.stripe_of_parity(index)
                        && parity_read.insert(index)
                    {
                        parity_per_stripe[stripe] += 1;
                    }
                }
                None => {
                    data_read.insert(offset as usize / crate::block::DATA_LEN);
                }
            },
            ScannedBlock::Recovery {
                offset, group_size, ..
            } => {
                recovery_groups.insert(offset as usize / (group_size * crate::block::DATA_LEN));
            }
        }
    }
    let mut missing_per_group: HashMap<usize, Vec<usize>> = HashMap::new();
    for index in own_blocks(label).filter(|index| !data_read.contains(index)) {
        let group = match group_size {
            0 => usize::MAX - index,
            size => index / size,
        };
        missing_per_group.entry(group).or_default().push(index);
    }
    let mut missing_per_stripe = vec![0usize; code.stripes()];
    for (group, missing) in &missing_per_group {
        let rebuilt = group_size > 0 && missing.len() == 1 && recovery_groups.contains(group);
        if !rebuilt {
            for &index in missing {
                missing_per_stripe[index % code.stripes()] += 1;
            }
        }
    }
    missing_per_stripe
        .iter()
        .zip(&parity_per_stripe)
        .all(|(missing, parity)| missing <= parity)
}

/// Whether the blocks read so far in one reading of a page leave nothing to the passes of more
/// effort: its parity blocks cover what is missing. `label` is the label of an earlier reading.
fn outcomes_covered_by_parity(outcomes: &[BlockOutcome], label: Option<&SuperBlock>) -> bool {
    let mut found_label = None;
    let mut group_size = 0;
    let mut blocks = Vec::new();
    for outcome in outcomes {
        let BlockOutcome::Readable { block, .. } = outcome else {
            continue;
        };
        match BlockKind::of(block.address()) {
            BlockKind::Superblock => {
                let candidate = SuperBlock::from_raw(block);
                if label_is_plausible(&candidate) {
                    found_label = Some(candidate);
                }
            }
            BlockKind::Data { offset } => blocks.push(ScannedBlock::Data {
                offset,
                payload: block.payload_array(),
            }),
            BlockKind::Recovery {
                offset,
                group_size: size,
            } => {
                group_size = size;
                blocks.push(ScannedBlock::Recovery {
                    offset,
                    group_size: size,
                    payload: block.payload_array(),
                });
            }
        }
    }
    found_label.as_ref().or(label).is_some_and(|label| {
        blocks_covered_by_parity(label, group_size, &blocks) && integrity_settled(label, &blocks)
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
    /// Whether a page that lacks blocks counts as complete when its parity blocks cover them.
    parity_suffices: bool,
    /// Whether a page counts as complete only when it lacks no data block, not already when its
    /// recovery blocks would rebuild the missing ones: for a page read again because the file
    /// lacks blocks, where that may not have held.
    thorough: bool,
    /// Whether the readings stopped before every attempt was made.
    stopped_early: bool,
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
                let complete = if self.thorough {
                    lacks_nothing(&merged)
                } else {
                    reads_cleanly(&merged)
                };
                let stopped_on_parity = !complete
                    && self.parity_suffices
                    && (merged.parity_shortcut
                        || (covered_by_parity(&merged)
                            && integrity_settled(&merged.label, &merged.blocks)));
                self.stopped_early = (merged.parity_shortcut || complete || stopped_on_parity)
                    && !lacks_nothing(&merged);
                self.combined = Some(merged);
                complete || stopped_on_parity
            }
            Err(error) => {
                self.last_error = Some(error);
                false
            }
        }
    }

    /// The page, and whether its readings stopped with blocks missing before every attempt was
    /// made: because its parity blocks or its recovery blocks seemed to cover them.
    fn into_result(self) -> Result<(ScannedPage, bool)> {
        let stopped_early = self.stopped_early;
        self.combined
            .map(|page| (page, stopped_early))
            .ok_or_else(|| self.last_error.unwrap_or(Error::NoReadablePage))
    }
}

/// Reads one page image into blocks and their statistics. When the page does
/// not read completely as it is, it is turned and read again; blocks found in
/// any attempt are combined. A turned picture is read only where no block was read
/// before, and nothing that was read is ever dropped. A first reading (`thorough` false) needs no
/// further attempts once the recovery blocks or the parity blocks read on the page cover the
/// blocks that are missing; the second value tells whether it stopped so, with blocks missing. The
/// parity blocks do not count when the quality is wanted: then every cell gets every attempt,
/// also where they would rebuild its block, since the quality shows the page and not what the file
/// needs. A `thorough` reading stops only when the page lacks no block.
fn scan_page(
    raster: &Raster,
    wanted: Wanted,
    sheets: &Sheets,
    thorough: bool,
) -> Result<(ScannedPage, bool)> {
    let parity_suffices = !thorough && !wanted.quality;
    let mut readings = Readings {
        parity_suffices,
        thorough,
        ..Readings::default()
    };
    let nothing_known = KnownBlocks::default();
    let first = Reading {
        wanted,
        placement: Placement::default(),
        known: &nothing_known,
        label: None,
        sheets,
        parity_suffices,
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
            parity_suffices,
        };
        if readings.add(scan_bitmap(&turned, &turned_reading, 1)) {
            break;
        }
    }
    readings.into_result()
}

/// A label that passes its CRC can still be garbage or hostile; sizes beyond what
/// the format can address would only make the assembler allocate absurd amounts, and a page
/// beyond the end of the data would carry no data block, so that it would seem to lack none.
fn label_is_plausible(label: &SuperBlock) -> bool {
    let limit = MAX_FILE_SIZE as usize + crate::codec::ALIGNMENT;
    let data_size = label.data_size as usize;
    (1..=limit).contains(&data_size)
        && label.original_size as usize <= limit
        && label.page >= 1
        && label.page_size > 0
        && (usize::from(label.page) - 1).saturating_mul(label.page_size as usize) < data_size
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
    /// Every picture is then read in full, also after the file is complete and where another
    /// picture of the sheet showed a block already, so that the quality shows how each picture
    /// was printed and scanned; this is slower.
    pub quality: bool,
    /// Look at pages that read badly for likely causes (see [`PageStatistics::hints`]); costs a
    /// pass over the pixels of each such page.
    pub diagnose: bool,
    /// Refuse a file that the pages do not vouch for: without this, pages that carry no hash (or
    /// had it stripped) are accepted, which is how pages of the original programs read. Needed to
    /// notice that someone removed the hash from encrypted pages and changed them.
    pub require_hash: bool,
    /// Threads to try ways of reading a block on: 0 is as many as the processor has, 1 is
    /// one after the other. The blocks read are the same either way.
    pub threads: usize,
}

/// Decodes the given pages (in any order) into the original file. Unreadable
/// pages are skipped and reported through `on_page`; decoding succeeds as long
/// as the remaining pages contain the whole file.
pub fn decode(
    pages: &[Raster],
    options: &DecodeOptions,
    mut on_page: impl FnMut(PageOutcome),
) -> Result<RestoredFile> {
    let mut assembler = Assembler::with_threads(options.threads);
    let mut sheets = Sheets::default();
    let wanted = Wanted {
        quality: options.quality,
        diagnose: options.diagnose,
        threads: options.threads,
    };
    // Pages whose readings stopped with blocks missing because their recovery or parity blocks
    // seemed to cover them, with their counts.
    let mut stopped_early = Vec::new();
    for (index, raster) in pages.iter().enumerate() {
        // Pictures after the one that completed the file are not read at all, unless their
        // quality is wanted.
        if assembler.is_complete() && !options.quality {
            on_page(PageOutcome {
                index,
                result: Err("not read: the file is complete already".into()),
                quality: None,
            });
            continue;
        }
        match scan_page(raster, wanted, &sheets, false) {
            Ok((mut page, stopped)) => {
                positions::examine(&mut page);
                sheets.record(&page.lattice, &page.label, page.statistics.sheet_id);
                on_page(PageOutcome {
                    index,
                    result: Ok(page.statistics),
                    // Every reading keeps its cells; the caller gets them only if it asked.
                    quality: if options.quality {
                        page.quality.take()
                    } else {
                        None
                    },
                });
                assembler.add_page(&page);
                if stopped {
                    stopped_early.push((index, page.statistics));
                }
            }
            Err(error) => on_page(PageOutcome {
                index,
                result: Err(error.to_string()),
                quality: None,
            }),
        }
    }
    // The parity or recovery blocks did not suffice after all (a page is missing, a recovery block
    // turned out to be in the wrong cell, two pages tell different group sizes): the pages that
    // stopped early are read with every attempt for the blocks that are still missing.
    for (index, earlier) in stopped_early {
        if assembler.is_complete() {
            break;
        }
        if let Ok((mut page, _)) = scan_page(&pages[index], wanted, &sheets, true) {
            positions::examine(&mut page);
            sheets.record(&page.lattice, &page.label, page.statistics.sheet_id);
            assembler.add_more_blocks(&page, &earlier);
        }
    }
    let restored = assembler.finish(options.password.as_deref())?;
    if options.require_hash && restored.report.integrity != crate::Integrity::Verified {
        return Err(Error::NotVerified);
    }
    Ok(restored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::DATA_LEN;
    use crate::codec::Compression;
    use crate::encode::{EncodeOptions, encode};
    use crate::layout::{PageLayout, PageSetup};

    const FILE_LEN: usize = 3000;

    /// A page of a file of ten data blocks holding the data blocks `data` and the parity blocks
    /// `parity`.
    fn page_holding(data: &[usize], parity: &[usize]) -> ScannedPage {
        use crate::block::{FileTime, Mode, NAME_LEN};
        let block = |offset: u32| ScannedBlock::Data {
            offset,
            payload: [0; DATA_LEN],
        };
        ScannedPage {
            label: SuperBlock {
                data_size: 10 * DATA_LEN as u32,
                page_size: 10 * DATA_LEN as u32,
                original_size: 10 * DATA_LEN as u32,
                mode: Mode(0),
                attributes: 0,
                page: 1,
                modified: FileTime::default(),
                file_crc: 0,
                name: [0; NAME_LEN],
            },
            group_size: 0,
            blocks: data
                .iter()
                .map(|&i| block((i * DATA_LEN) as u32))
                .chain(parity.iter().map(|&k| block(crate::parity::address(k))))
                .collect(),
            statistics: PageStatistics::default(),
            read_places: Vec::new(),
            block_size: 0.0,
            lattice: LatticeRead::default(),
            records: Vec::new(),
            quality: None,
            reads: Vec::new(),
            parity_shortcut: false,
            corrections: HashMap::new(),
        }
    }

    #[test]
    fn a_parity_block_read_in_two_cells_counts_once() {
        // A grid one cell off can read a block in two cells. Counted twice, one parity block
        // covered two lost blocks, and the page stopped short of what the file needs.
        let eight = [0, 1, 2, 3, 5, 6, 8, 9];
        assert!(!covered_by_parity(&page_holding(&eight, &[4, 4])));
        let label = page_holding(&[], &[]).label;
        let blocks = page_holding(&eight, &[4, 4]).blocks;
        assert!(!blocks_covered_by_parity(&label, 0, &blocks));
    }

    #[test]
    fn a_page_beyond_the_end_of_the_file_is_not_taken() {
        // Such a page carries no data block, so it lacked none and needed no effort.
        let label = page_holding(&[], &[]).label;
        assert!(label_is_plausible(&label));
        for (page, page_size) in [(2, label.page_size), (0xFFFF, label.page_size), (1, 0)] {
            let beyond = SuperBlock {
                page_size,
                page,
                ..label.clone()
            };
            assert!(!label_is_plausible(&beyond), "page {page}");
        }
        let half = SuperBlock {
            page: 2,
            page_size: label.data_size / 2,
            ..label.clone()
        };
        assert!(label_is_plausible(&half));
    }

    #[test]
    fn only_a_hash_that_will_check_the_file_settles_its_integrity() {
        // A hash record of another size, or keyed for a file that is not encrypted, is not used to
        // check the file; once a page stopped early on it and the hash went unread.
        let mut label = page_holding(&[], &[]).label;
        label.mode = crate::block::Mode(crate::block::Mode::EXTENDED);
        let hash = pbx::HashRecord {
            digest: [1; pbx::HASH_LEN],
            original_size: label.original_size,
            keyed: false,
        };
        let holding = |hash: pbx::HashRecord| {
            vec![ScannedBlock::Data {
                offset: pbx::record_address(label.data_size, 1).unwrap(),
                payload: Record::Hash(hash).to_payload(),
            }]
        };
        assert!(integrity_settled(&label, &holding(hash)));
        let other_size = pbx::HashRecord {
            original_size: hash.original_size + 1,
            ..hash
        };
        assert!(!integrity_settled(&label, &holding(other_size)));
        let keyed = pbx::HashRecord {
            keyed: true,
            ..hash
        };
        assert!(!integrity_settled(&label, &holding(keyed)));
    }

    #[test]
    fn a_page_that_stops_on_its_recovery_blocks_is_read_again_if_the_file_lacks_blocks() {
        // Block 3 is missing, and the recovery block of its group would rebuild it, so the page
        // stops early. Should the file still lack blocks (the recovery block was in the wrong
        // cell, say), it is read again, and then only a page that lacks nothing stops.
        let mut page = page_holding(&[0, 1, 2, 4, 5, 6, 7, 8, 9], &[]);
        page.group_size = 5;
        page.blocks.push(ScannedBlock::Recovery {
            offset: 0,
            group_size: 5,
            payload: [0; DATA_LEN],
        });
        let first_reading = |page: ScannedPage| {
            let mut readings = Readings {
                parity_suffices: true,
                ..Readings::default()
            };
            (readings.add(Ok(page)), readings.into_result().unwrap().1)
        };
        assert_eq!(first_reading(page_copy(&page)), (true, true));
        let mut again = Readings {
            thorough: true,
            ..Readings::default()
        };
        assert!(!again.add(Ok(page_copy(&page))));
        // A page that lacks nothing stops, and needs no other reading.
        let complete = page_holding(&(0..10).collect::<Vec<_>>(), &[]);
        assert_eq!(first_reading(complete), (true, false));
    }

    fn page_copy(page: &ScannedPage) -> ScannedPage {
        let mut copy = page_holding(&[], &[]);
        copy.group_size = page.group_size;
        copy.blocks = page
            .blocks
            .iter()
            .map(|block| match *block {
                ScannedBlock::Data { offset, payload } => ScannedBlock::Data { offset, payload },
                ScannedBlock::Recovery {
                    offset,
                    group_size,
                    payload,
                } => ScannedBlock::Recovery {
                    offset,
                    group_size,
                    payload,
                },
            })
            .collect();
        copy
    }

    #[test]
    fn a_page_is_covered_when_its_parity_blocks_match_its_losses() {
        let eight = [0, 1, 2, 3, 5, 6, 8, 9];
        assert!(covered_by_parity(&page_holding(&eight, &[0, 1])));
        assert!(covered_by_parity(&page_holding(&eight, &[3, 4, 7])));
        assert!(!covered_by_parity(&page_holding(&eight, &[2])));
        assert!(!covered_by_parity(&page_holding(&eight, &[])));
    }
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
            // The cells of these pages are those of PaperBack's default layout.
            redundancy: Some(crate::block::Redundancy::default()),
            parity_percent: 0,
            ..EncodeOptions::default()
        };
        let layout = PageLayout::compute(
            &options.setup,
            crate::plan::Strings::new(options.redundancy),
        )
        .unwrap();
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
        small_page_with(None)
    }

    fn small_page_with(password: Option<&str>) -> (Vec<u8>, Raster, PageLayout, usize) {
        let data: Vec<u8> = (0..FILE_LEN).map(|i| (i * 131 + i / 3) as u8).collect();
        let options = EncodeOptions {
            setup: PageSetup {
                printer_dpi: 300,
                dot_dpi: 100,
                ..PageSetup::default()
            },
            compression: Compression::None,
            password: password.map(str::to_owned),
            ..EncodeOptions::default()
        };
        let layout = PageLayout::compute(
            &options.setup,
            crate::plan::Strings::new(options.redundancy),
        )
        .unwrap();
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

    /// Readable cells in the quality of each picture, `None` for a picture without one.
    fn readable_cells(pages: &[Raster]) -> Vec<Option<usize>> {
        let options = DecodeOptions {
            quality: true,
            ..DecodeOptions::default()
        };
        let mut cells = vec![None; pages.len()];
        let _ = decode(pages, &options, |page| {
            cells[page.index] = page.quality.map(|q| q.readable_blocks());
        });
        cells
    }

    #[test]
    fn the_quality_of_a_second_picture_of_a_sheet_covers_the_cells_read_in_the_first() {
        // The first page of two: the file stays incomplete, so every picture is read.
        let (raster, layout, rows) = full_page();
        let alone = readable_cells(std::slice::from_ref(&raster))[0].unwrap();
        assert_eq!(alone, layout.nx * rows);
        let damaged = wiped_column(raster.clone(), &layout, rows, 5, false);
        let cells = readable_cells(&[damaged, raster]);
        assert_eq!(cells[0], Some(alone - rows));
        // The second picture shows the same sheet; its cells read in the first count all the same.
        assert_eq!(cells[1], Some(alone));
    }

    #[test]
    fn pictures_after_the_file_is_complete_get_their_quality() {
        let (_, raster, ..) = small_page();
        let cells = readable_cells(&[raster.clone(), raster.clone()]);
        assert_eq!(cells[1], cells[0]);
        assert!(cells[1].is_some());
        // Without the quality the second picture is not read at all.
        let mut read = Vec::new();
        decode(
            &[raster.clone(), raster],
            &DecodeOptions::default(),
            |page| {
                read.push(page.result.is_ok());
            },
        )
        .unwrap();
        assert_eq!(read, [true, false]);
    }

    /// The first of the pages of a file that fills it, at two pixels per dot as from a 300 dpi
    /// printer; the geometry to find its cells, and its rows.
    fn full_page() -> (Raster, PageLayout, usize) {
        full_page_with(EncodeOptions::default())
    }

    /// [`full_page`] with the redundancy, parity and filling of `base`.
    fn full_page_with(base: EncodeOptions) -> (Raster, PageLayout, usize) {
        let mut state = 7u32;
        let data: Vec<u8> = (0..150_000)
            .map(|_| {
                state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                (state >> 16) as u8
            })
            .collect();
        let options = EncodeOptions {
            setup: PageSetup {
                printer_dpi: 300,
                dot_dpi: 200,
                ..PageSetup::default()
            },
            compression: Compression::None,
            ..base
        };
        let layout = PageLayout::compute(
            &options.setup,
            crate::plan::Strings::new(options.redundancy),
        )
        .unwrap();
        let raster = encode(&data, &options).unwrap().remove(0).raster;
        let bands = layout.text.map_or(0, |t| t.top_band() + t.bottom_band());
        let rows = (1..=layout.ny)
            .find(|&rows| layout.image_height(rows) + bands == raster.height())
            .unwrap();
        (raster, layout, rows)
    }

    /// Whitens the cells of one column from the top to the bottom of the page, the dots only
    /// or with the grid lines around them.
    fn wiped_column(
        raster: Raster,
        layout: &PageLayout,
        rows: usize,
        column: usize,
        lines: bool,
    ) -> Raster {
        let (width, height) = (raster.width(), raster.height());
        let mut pixels = raster.into_pixels();
        let top_band = layout.text.map_or(0, |text| text.top_band());
        let (x, _) = layout.cell_origin(column);
        let (x, cell_width) = if lines {
            (x - 2 * layout.dx, crate::layout::CELL_DOTS * layout.dx + 1)
        } else {
            (x, 32 * layout.dx)
        };
        let top = top_band + layout.cell_origin(column).1 - 2 * layout.dy;
        for row in top..top + rows * crate::layout::CELL_DOTS * layout.dy {
            pixels[row * width + x..row * width + x + cell_width].fill(255);
        }
        Raster::from_pixels(width, height, pixels).unwrap()
    }

    /// The statistics and the readable cells of a single picture.
    fn read_alone(page: Raster) -> (PageStatistics, usize) {
        let options = DecodeOptions {
            quality: true,
            ..DecodeOptions::default()
        };
        let mut seen = None;
        let _ = decode(&[page], &options, |page| {
            seen = Some((
                page.result.unwrap(),
                page.quality.unwrap().readable_blocks(),
            ));
        });
        seen.unwrap()
    }

    #[test]
    fn a_cell_without_dots_does_not_read_the_block_next_to_it() {
        // A turn of a degree tried for a block far from the corner of the page once moved it
        // by a whole cell: an empty cell then read its neighbour, as if it were readable.
        let (raster, layout, rows) = full_page();
        let page = wiped_column(raster, &layout, rows, 5, false);
        let (statistics, readable) = read_alone(page);
        assert_eq!(statistics.bad_blocks, rows);
        assert_eq!(readable, layout.nx * rows - rows);
    }

    #[test]
    fn cells_that_lost_their_grid_lines_between_blocks_count_as_unreadable() {
        let (raster, layout, rows) = full_page();
        let page = wiped_column(raster, &layout, rows, 5, true);
        let (statistics, readable) = read_alone(page);
        assert_eq!(statistics.bad_blocks, rows);
        assert!(
            readable >= layout.nx * rows - rows - 2 * rows,
            "the columns beside it read"
        );
    }

    #[test]
    fn a_parity_block_is_no_data_block() {
        // Parity block 0 has the address 1, which divided by 90 is data block 0.
        let without_the_first: Vec<usize> = (1..10).collect();
        assert!(!reads_cleanly(&page_holding(&without_the_first, &[0])));
        let all: Vec<usize> = (0..10).collect();
        assert!(reads_cleanly(&page_holding(&all, &[0])));
    }

    #[test]
    fn a_page_whose_hash_did_not_read_gets_the_passes_of_more_effort() {
        // Every data block reads at once and parity would cover any loss, but the records at
        // both ends of the page are gone: stopping early would give up the hash.
        let (raster, layout, rows) = full_page_with(EncodeOptions::modern());
        let ends = crate::plan::RECORD_CELLS_PER_END;
        let last = layout.nx * rows - 1;
        let records: Vec<usize> = (0..ends).chain(last + 1 - ends..=last).collect();
        let page = wiped(raster, &layout, &records);
        let wanted = Wanted {
            quality: false,
            diagnose: false,
            threads: 0,
        };
        let (scanned, stopped) = scan_page(&page, wanted, &Sheets::default(), false).unwrap();
        assert!(!scanned.parity_shortcut && !stopped);
        // With the records there, the same page does stop early.
        let (raster, ..) = full_page_with(EncodeOptions::modern());
        let column = wiped_column(raster, &layout, rows, 5, false);
        let (scanned, _) = scan_page(&column, wanted, &Sheets::default(), false).unwrap();
        assert!(scanned.parity_shortcut);
    }

    #[test]
    fn a_page_that_lacks_nothing_is_covered_without_parity_blocks() {
        let all: Vec<usize> = (0..10).collect();
        assert!(covered_by_parity(&page_holding(&all, &[])));
        assert!(!covered_by_parity(&page_holding(&all[1..], &[])));
    }

    #[test]
    fn a_clean_page_without_parity_blocks_is_not_followed_across_its_margin() {
        // The grid of a page reaches over the title and the hint, whose cells hold no block. A
        // page that lacks nothing once needed parity blocks to skip the passes of more effort,
        // which tried those cells and followed the grid lines of the whole page for them.
        let (raster, ..) = full_page_with(EncodeOptions::legacy());
        let wanted = Wanted {
            quality: false,
            diagnose: false,
            threads: 0,
        };
        let (page, stopped) = scan_page(&raster, wanted, &Sheets::default(), false).unwrap();
        assert!(page.parity_shortcut, "the passes of more effort were made");
        assert!(!stopped, "a page that lacks nothing did not stop short");
        assert_eq!(page.statistics.bad_blocks, 0);
    }

    #[test]
    fn the_unreadable_count_is_the_same_with_and_without_the_quality() {
        // A clean page turned by 6 degrees: the drawing of the quality once took cells over the
        // title, whose letters gave the traced grid lines four corners, for unreadable blocks.
        // The page of `tests/fixtures/damaged/legacy-1.png` before it was damaged.
        let data: Vec<u8> = (0..90_000usize).map(|i| (i * 131 + i / 3) as u8).collect();
        let options = EncodeOptions {
            setup: PageSetup {
                printer_dpi: 300,
                dot_dpi: 200,
                ..PageSetup::default()
            },
            compression: Compression::None,
            ..EncodeOptions::legacy()
        };
        let raster = encode(&data, &options).unwrap().remove(0).raster;
        let clean = raster.rotated(6.0, WHITE).unwrap();
        let unreadable = |page: &Raster, quality| {
            let options = DecodeOptions {
                quality,
                ..DecodeOptions::default()
            };
            let mut counts = None;
            let _ = decode(std::slice::from_ref(page), &options, |outcome| {
                let statistics = outcome.result.unwrap();
                let shown = outcome.quality.map(|q| q.unreadable_blocks());
                counts = Some((statistics.bad_blocks, shown));
            });
            counts.unwrap()
        };
        assert_eq!(unreadable(&clean, false), (0, None));
        assert_eq!(unreadable(&clean, true), (0, Some(0)));
    }

    #[test]
    fn a_turned_page_shows_exactly_its_damage() {
        let (raster, layout, rows) = full_page_with(EncodeOptions::legacy());
        let page = wiped_column(raster, &layout, rows, 5, false)
            .rotated(8.0, WHITE)
            .unwrap();
        let unreadable = |quality| {
            let options = DecodeOptions {
                quality,
                ..DecodeOptions::default()
            };
            let mut counts = None;
            let _ = decode(std::slice::from_ref(&page), &options, |outcome| {
                let statistics = outcome.result.unwrap();
                let shown = outcome.quality.map(|q| q.unreadable_blocks());
                counts = Some((statistics.bad_blocks, shown));
            });
            counts.unwrap()
        };
        assert_eq!(unreadable(false), (rows, None));
        assert_eq!(unreadable(true), (rows, Some(rows)));
    }

    #[test]
    fn the_quality_gives_every_cell_every_attempt_even_where_parity_covers_it() {
        let (raster, layout, rows) = full_page_with(EncodeOptions::modern());
        let page = wiped_column(raster, &layout, rows, 5, false);
        let wanted = |quality| Wanted {
            quality,
            diagnose: false,
            threads: 0,
        };
        let sheets = Sheets::default();
        // Without the quality the parity blocks on the page cover the column, and reading stops.
        let (plain, stopped) = scan_page(&page, wanted(false), &sheets, false).unwrap();
        assert!(stopped && plain.parity_shortcut);
        let (checked, stopped) = scan_page(&page, wanted(true), &sheets, false).unwrap();
        assert!(!stopped && !checked.parity_shortcut);
    }

    #[test]
    fn a_page_read_again_counts_the_corrections_of_the_blocks_it_adds_only() {
        // A page without recovery blocks is read again, turned, when blocks are missing; those
        // readings once set the corrections of the page although they added no block.
        let (raster, layout, rows) = full_page_with(EncodeOptions::modern());
        let page = wiped_column(raster, &layout, rows, 5, false);
        let (statistics, _) = read_alone(page);
        assert_eq!(statistics.bad_blocks, rows);
        assert_eq!(statistics.restored_bytes, 0);
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
    fn an_encrypted_page_without_its_records_decrypts_unless_a_hash_is_required() {
        use crate::plan::RECORD_CELLS_PER_END;
        let (data, raster, layout, rows) = small_page_with(Some("hunter2"));
        let last = layout.nx * rows - 1;
        let both_ends: Vec<usize> = (0..RECORD_CELLS_PER_END)
            .chain(last + 1 - RECORD_CELLS_PER_END..=last)
            .collect();
        let stripped = wiped(raster, &layout, &both_ends);
        let open = DecodeOptions {
            password: Some("hunter2".into()),
            ..DecodeOptions::default()
        };
        assert_eq!(
            decode(std::slice::from_ref(&stripped), &open, |_| {})
                .unwrap()
                .data,
            data
        );
        let strict = DecodeOptions {
            require_hash: true,
            ..open
        };
        assert!(matches!(
            decode(&[stripped], &strict, |_| {}),
            Err(Error::NotVerified)
        ));
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
        let layout = PageLayout::compute(
            &options.setup,
            crate::plan::Strings::new(options.redundancy),
        )
        .unwrap();
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
