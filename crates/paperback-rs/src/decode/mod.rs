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
pub(crate) mod grid;
mod peaks;
mod reader;
mod skew;

pub use assemble::{PageStatistics, Report, RestoredFile};

use std::collections::{HashMap, HashSet};

use assemble::{Assembler, ScannedBlock, ScannedPage};
use bitmap::Bitmap;
use reader::{BlockOutcome, BlockReader};

use crate::block::{BLOCK_DOTS, BlockKind, MAX_FILE_SIZE, SuperBlock};
use crate::error::{Error, Result};
use crate::quality::{BlockQuality, PageQuality};
use crate::raster::{Raster, WHITE};

/// Dot pitch in pixels the reader works best with. Scans at several times the
/// dot density are averaged down to it: the dots are then sampled with a window
/// that forgives the small bends and shifts of crumpled or badly flattened paper.
const WORKING_DOT_PITCH: f64 = 3.0;

/// By how much a scan with this grid is reduced; 1 leaves it as it is.
fn reduction_factor(grid: &grid::Grid) -> usize {
    let pitch = grid.x_step.min(grid.y_step) / (BLOCK_DOTS + 3) as f64;
    (pitch / WORKING_DOT_PITCH + 0.2) as usize
}

/// Reads the bitmap as it is: grid search, then every block.
fn scan_bitmap(raster: &Raster, with_quality: bool) -> Result<ScannedPage> {
    let bitmap = Bitmap::from_raster(raster);
    let (grid, intensity) = grid::locate(&bitmap)?;
    let factor = reduction_factor(&grid);
    if let Some(smaller) = raster.reduced(factor).filter(|_| factor > 1) {
        return scan_bitmap(&smaller, with_quality);
    }
    let mut reader = BlockReader::new(&bitmap, grid, intensity);
    reader.tune_sharpness(TUNING_COLUMNS, TUNING_ROWS);
    if !reader.finds_blocks(PROBE_COLUMNS, PROBE_ROWS) {
        return Err(Error::Decode("no readable blocks found in the grid".into()));
    }

    let mut label: Option<SuperBlock> = None;
    let mut group_size = 0;
    let mut blocks = Vec::new();
    let mut statistics = PageStatistics::default();
    let (columns, rows) = (reader.columns(), reader.rows());
    let mut cells = vec![BlockQuality::Absent; columns * rows];
    let mut outcomes: Vec<BlockOutcome> = (0..rows)
        .flat_map(|row| (0..columns).map(move |column| (column, row)))
        .map(|(column, row)| reader.read(column, row))
        .collect();
    follow_bent_paper(&mut reader, &mut outcomes, columns, rows);
    for row in 0..rows {
        for column in 0..columns {
            let outcome =
                std::mem::replace(&mut outcomes[row * columns + column], BlockOutcome::Missing);
            cells[row * columns + column] = match &outcome {
                BlockOutcome::Missing => BlockQuality::Absent,
                BlockOutcome::Unreadable => BlockQuality::Unreadable,
                BlockOutcome::Readable { corrected, .. } => BlockQuality::Readable(*corrected),
            };
            match outcome {
                BlockOutcome::Missing => {}
                BlockOutcome::Unreadable => statistics.bad_blocks += 1,
                BlockOutcome::Readable { block, corrected } => {
                    statistics.restored_bytes += corrected;
                    match BlockKind::of(block.address()) {
                        BlockKind::Superblock => {
                            let candidate = SuperBlock::from_raw(&block);
                            if label_is_plausible(&candidate) {
                                label = Some(candidate);
                            }
                            statistics.superblocks += 1;
                        }
                        BlockKind::Data { offset } => {
                            statistics.good_blocks += 1;
                            blocks.push(ScannedBlock::Data {
                                offset,
                                payload: block.payload_array(),
                            });
                        }
                        BlockKind::Recovery {
                            offset,
                            group_size: size,
                        } => {
                            statistics.good_blocks += 1;
                            group_size = size;
                            blocks.push(ScannedBlock::Recovery {
                                offset,
                                group_size: size,
                                payload: block.payload_array(),
                            });
                        }
                    }
                }
            }
        }
    }
    let label = label.ok_or(Error::NoReadablePage)?;
    let quality = with_quality.then(|| {
        PageQuality::new(
            raster.clone(),
            reader.grid(),
            columns,
            rows,
            cells,
            reader.shift_map(),
        )
    });
    Ok(ScannedPage {
        label,
        group_size,
        blocks,
        statistics,
        quality,
    })
}

/// Rounds of rereading blocks next to ones that read, so that a crease is
/// followed block by block from its readable edge inwards.
const FOLLOW_ROUNDS: usize = 64;

/// Rereads unreadable blocks at the position their readable neighbours show.
fn follow_bent_paper(
    reader: &mut BlockReader,
    outcomes: &mut [BlockOutcome],
    columns: usize,
    rows: usize,
) {
    for _ in 0..FOLLOW_ROUNDS {
        let mut improved = false;
        for row in 0..rows {
            for column in 0..columns {
                let at = row * columns + column;
                if matches!(outcomes[at], BlockOutcome::Readable { .. })
                    || !reader.has_neighbour_to_learn_from(column, row)
                {
                    continue;
                }
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

/// Reads one page image into blocks and their statistics. When the page does
/// not read completely as it is, it is turned and read again; blocks found in
/// any attempt are combined.
fn scan_page(raster: &Raster, with_quality: bool) -> Result<ScannedPage> {
    let mut combined: Option<ScannedPage> = None;
    let mut last_error = None;
    let mut attempt = |page: Result<ScannedPage>| -> Option<ScannedPage> {
        match page {
            Ok(page) => {
                let merged = match combined.take() {
                    Some(mut earlier) => {
                        earlier.absorb(page);
                        earlier
                    }
                    None => page,
                };
                if reads_cleanly(&merged) {
                    return Some(merged);
                }
                combined = Some(merged);
            }
            Err(error) => last_error = Some(error),
        }
        None
    };

    if let Some(page) = attempt(scan_bitmap(raster, with_quality)) {
        return Ok(page);
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
        if let Some(page) = attempt(scan_bitmap(&turned, with_quality)) {
            return Ok(page);
        }
    }
    combined.ok_or_else(|| last_error.unwrap_or(Error::NoReadablePage))
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
    for (index, raster) in pages.iter().enumerate() {
        match scan_page(raster, options.quality) {
            Ok(mut page) => {
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
        for cell in string_len + 2..string_len + 5 {
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
}
