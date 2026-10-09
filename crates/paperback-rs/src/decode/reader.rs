// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider
// Derived from PaperBack 1.10, Copyright (c) 2007 Oleh Yuschuk; see NOTICE.md.

//! Reads blocks out of a located grid: resampling, sharpening, dot sampling
//! and error correction.

use super::bitmap::Bitmap;
use super::detector::DotDetector;
use super::grid::{Grid, Intensity};
use super::mesh::{Mesh, Quad, sample};
use super::peaks::find_peaks;
use crate::block::{BLOCK_DOTS, BLOCK_LEN, RawBlock, row_mask};
use crate::crc::crc16;

const SUBBLOCK_SIZE: usize = 8;
const SHIFT_COUNT: usize = 9;
const CENTER_SHIFT: usize = 4;
const GRID_CELL_DOTS: f64 = (BLOCK_DOTS + 3) as f64;
/// Paper that is too far from the scanner glass (creases, curls) is blurred only
/// there, so a block that does not read is cut out again with a stronger filter.
/// Each entry is a factor and an addition to the page-wide sharpness.
const LOCAL_SHARPNESS_BOOSTS: [(f64, f64); 5] =
    [(1.0, 0.0), (1.5, 0.5), (2.0, 1.0), (3.0, 1.5), (4.0, 2.5)];
/// Local rotations tried for a block whose neighbours read, added to theirs.
const TILT_STEPS: [f64; 6] = [0.02, -0.02, 0.04, -0.04, 0.07, -0.07];
/// Guesses at most per block: their average, single neighbours, then the rotations.
const MAX_POSE_TRIES: usize = 12;
/// Guesses tried at normal effort: the average of the neighbours and a couple of single ones.
const NORMAL_POSE_TRIES: usize = 3;
/// Corners of a block that must be found in the picture before it is read through them.
const MIN_CORNERS: usize = 3;
/// Reach of the averaging around a dot, as a share of the distance between two dots; a
/// small window keeps the dot sharp, a wider one forgives errors in its place.
const WARPED_DOT_WINDOWS: [f64; 2] = [0.3, 0.55];
/// Shift of the nine samplings, as a share of the distance between two dots.
const WARPED_SHIFT: f64 = 0.35;
/// Scores of the learned detector are about 1 for a sure dot; certainties are whole numbers.
const CERTAINTY_SCALE: f64 = 1000.0;
/// Settings of how much a mesh trusts the perspective prediction of the next corner.
const MESH_TRUSTS: [f64; 3] = [0.5, 1.0, 0.0];
/// Sampling shifts kept for each quadrant when the checksum decides where the dots are; with
/// four quadrants that is 256 combinations.
const ALIGNED_CANDIDATES: usize = 4;
const MAX_SHARPNESS: f64 = 6.0;
const UNREADABLE: usize = 17;
const MAX_CORRECTIONS: usize = 16;
const CRC_MASK: u16 = 0x55AA;
const CRC_COVERED: usize = 94;
const ECC_PAD: usize = 127;
const NEIGHBOUR_WEIGHTS: [i32; 3] = [1000, 32, 16];
const THRESHOLD_VARIANTS: usize = 9;
/// Threshold variants of the quick pass: the three neighbour weights, one threshold each.
const QUICK_VARIANTS: usize = 3;
const ORIENTATIONS: usize = 8;

type DotGrid = [[u8; BLOCK_DOTS]; BLOCK_DOTS];

#[derive(Debug)]
pub(crate) enum BlockOutcome {
    /// No block grid at this position, probably outside the raster.
    Missing,
    /// A block was found but could not be repaired.
    Unreadable,
    Readable {
        block: RawBlock,
        corrected: usize,
    },
    /// Not read: a block was read at this place in an earlier reading of the page.
    Skipped,
}

/// How much work is spent on a block. Reading goes in passes over the blocks that are still
/// missing, from the quick one that reads most of a good page to the deep one that tries
/// everything, so the expensive attempts are made only on the few blocks that need them and a
/// block that was read is never read again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Effort {
    /// The grid as fitted, one sharpness, a few thresholds.
    Quick,
    /// All sharpness and threshold variants, the block's own corners, a few guesses from
    /// the neighbours.
    Normal,
    /// Everything: erasures, the learned detector, every mesh and every guess.
    Deep,
}

/// The corner mesh of a page, which is only built if a block cannot be read otherwise.
enum MeshState {
    NotBuilt,
    /// There are no grid lines to follow.
    Missing,
    Built(Mesh),
}

pub(crate) struct BlockReader<'a> {
    bitmap: &'a Bitmap,
    grid: Grid,
    intensity: Intensity,
    sharpness: f64,
    base_sharpness: f64,
    /// Sharpness levels to try per block, the one that suited the page best first.
    boosts: Vec<(f64, f64)>,
    border: f64,
    buffer_width: usize,
    buffer_height: usize,
    max_dot_size: usize,
    columns: usize,
    rows: usize,
    orientation: Option<usize>,
    last_good_variant: usize,
    /// Where each readable block really sat compared with the fitted grid, in pixels.
    /// Crumpled paper bends the grid; a block sits near where its neighbours were.
    shifts: Vec<Option<(f64, f64)>>,
    /// Local rotation of each readable block against the page-wide tilt, in the units of
    /// the grid angles. Photographs of bent paper turn blocks by different amounts.
    tilts: Vec<Option<f64>>,
    page_angles: (f64, f64),
    /// The corners of all blocks, found when the first block needed them; `Some(None)` if there
    /// are no grid lines to follow.
    /// One mesh per prediction setting of [`MESH_TRUSTS`], each built when first needed.
    meshes: Vec<MeshState>,
    effort: Effort,
    /// Learns from the blocks that were read how dots look on this page.
    detector: DotDetector,
    rotated: Vec<u8>,
    sharpened: Vec<u8>,
}

impl<'a> BlockReader<'a> {
    pub(crate) fn new(bitmap: &'a Bitmap, mut grid: Grid, intensity: Intensity) -> Self {
        let border = grid.x_angle.abs().max(grid.y_angle.abs()) * 5.0 + 0.4;
        let dot_pitch = grid.x_step.max(grid.y_step) / GRID_CELL_DOTS;
        let sharpness = (intensity.sharpness + 1.3 / dot_pitch - 0.1).clamp(0.0, 2.0);

        let x_drift = (grid.x_angle * bitmap.height() as f64).abs();
        let x_slack = if grid.x_angle < 0.0 { 0.0 } else { x_drift };
        while grid.x_peak - grid.x_step > -x_slack - grid.x_step * border {
            grid.x_peak -= grid.x_step;
        }
        let columns = ((bitmap.width() as f64 + x_drift) / grid.x_step) as usize;

        let y_drift = (grid.y_angle * bitmap.width() as f64).abs();
        let y_slack = if grid.y_angle < 0.0 { 0.0 } else { y_drift };
        while grid.y_peak - grid.y_step > -y_slack - grid.y_step * border {
            grid.y_peak -= grid.y_step;
        }
        let rows = ((bitmap.height() as f64 + y_drift) / grid.y_step) as usize;

        let buffer_width = (grid.x_step * (2.0 * border + 1.0) + 1.0) as usize;
        let buffer_height = (grid.y_step * (2.0 * border + 1.0) + 1.0) as usize;
        let cell = BLOCK_DOTS + 3;
        let max_dot_size = if grid.x_step < (2 * cell) as f64 || grid.y_step < (2 * cell) as f64 {
            1
        } else if grid.x_step < (3 * cell) as f64 || grid.y_step < (3 * cell) as f64 {
            2
        } else if grid.x_step < (4 * cell) as f64 || grid.y_step < (4 * cell) as f64 {
            3
        } else {
            4
        };
        Self {
            bitmap,
            grid,
            intensity,
            sharpness,
            base_sharpness: sharpness,
            boosts: LOCAL_SHARPNESS_BOOSTS.to_vec(),
            border,
            buffer_width,
            buffer_height,
            max_dot_size,
            columns,
            rows,
            orientation: None,
            last_good_variant: 0,
            shifts: vec![None; columns * rows],
            tilts: vec![None; columns * rows],
            page_angles: (grid.x_angle, grid.y_angle),
            meshes: MESH_TRUSTS.iter().map(|_| MeshState::NotBuilt).collect(),
            effort: Effort::Quick,
            detector: DotDetector::new(),
            rotated: vec![0; buffer_width * buffer_height],
            sharpened: vec![0; buffer_width * buffer_height],
        }
    }

    /// The grid with the page-relative origin used for reading blocks.
    pub(crate) fn grid(&self) -> Grid {
        self.grid
    }

    /// The middle of the cell in the bitmap, as the straight grid has it.
    pub(crate) fn cell_centre(&self, column: usize, row: usize) -> (f64, f64) {
        let g = &self.grid;
        let from_bottom = self.rows - 1 - row;
        let start = (
            g.x_peak + (column as f64 + 0.5) * g.x_step,
            g.y_peak + (from_bottom as f64 + 0.5) * g.y_step,
        );
        let (x_angle, y_angle) = self.page_angles;
        let mut point = start;
        for _ in 0..3 {
            point.0 = start.0 + point.1 * x_angle;
            point.1 = start.1 + point.0 * y_angle;
        }
        point
    }

    /// Where a block that was read lies in the bitmap: the middle of its cell, moved by the
    /// shift that was measured when it was read.
    pub(crate) fn read_position(&self, column: usize, row: usize) -> Option<(f64, f64)> {
        let shift = self.shifts[row * self.columns + column]?;
        let centre = self.cell_centre(column, row);
        Some((centre.0 + shift.0, centre.1 + shift.1))
    }

    pub(crate) fn set_effort(&mut self, effort: Effort) {
        self.effort = effort;
    }

    pub(crate) fn columns(&self) -> usize {
        self.columns
    }

    pub(crate) fn rows(&self) -> usize {
        self.rows
    }

    /// Reads a block where the grid says it is, then where its neighbours' real
    /// positions suggest it is, which follows paper that is bent or crumpled.
    pub(crate) fn read(&mut self, column: usize, row: usize) -> BlockOutcome {
        let every_level = self.effort >= Effort::Normal;
        let outcome = self.read_posed(column, row, (0.0, 0.0), 0.0, every_level);
        if matches!(outcome, BlockOutcome::Readable { .. }) || self.effort == Effort::Quick {
            return outcome;
        }
        // Through its own corners first: it is the cheaper of the two ways to follow bent paper,
        // and the one that works when the straight grid is far off.
        if let Some((block, corrected)) = self.read_warped(column, row) {
            return BlockOutcome::Readable { block, corrected };
        }
        self.read_like_neighbours(column, row, outcome)
    }

    /// Tries the shift and the rotation of the blocks around this one, and a few rotations
    /// near theirs; `outcome` is what reading it where the grid says gave.
    fn read_like_neighbours(
        &mut self,
        column: usize,
        row: usize,
        mut outcome: BlockOutcome,
    ) -> BlockOutcome {
        let neighbours = self.neighbour_shifts(column, row);
        let Some(average) = Self::mean(&neighbours) else {
            return outcome;
        };
        let tilt = self.neighbour_tilt(column, row);
        let single_shifts = neighbours.into_iter().map(|shift| (shift, tilt));
        let tilted = TILT_STEPS.iter().map(|step| (average, tilt + step));
        let guesses = std::iter::once((average, tilt))
            .chain(single_shifts)
            .chain(tilted)
            .take(if self.effort == Effort::Deep {
                MAX_POSE_TRIES
            } else {
                NORMAL_POSE_TRIES
            });
        for (attempt, (shift, tilt)) in guesses.enumerate() {
            if matches!(outcome, BlockOutcome::Readable { .. }) {
                break;
            }
            // Only the first guess gets every sharpness level; the rest are many and cheap.
            let retry = self.read_posed(column, row, shift, tilt, attempt == 0);
            if matches!(retry, BlockOutcome::Readable { .. })
                || matches!(outcome, BlockOutcome::Missing)
            {
                outcome = retry;
            }
        }
        outcome
    }

    /// The corners of all blocks, found when first needed. `None` if there are no grid lines
    /// to follow.
    fn mesh(&mut self, variant: usize) -> Option<&Mesh> {
        if matches!(self.meshes[variant], MeshState::NotBuilt) {
            self.meshes[variant] = Mesh::build(
                self.bitmap,
                self.grid,
                self.columns,
                self.rows,
                self.page_angles,
                MESH_TRUSTS[variant],
            )
            .map_or(MeshState::Missing, MeshState::Built);
        }
        match &self.meshes[variant] {
            MeshState::Built(mesh) => Some(mesh),
            MeshState::NotBuilt | MeshState::Missing => None,
        }
    }

    /// The four corners of every block, row by row from the top, for drawing the page; follows
    /// the grid lines across the page if that has not been done yet.
    pub(crate) fn cell_quads(&mut self) -> Option<Vec<Quad>> {
        let (columns, rows) = (self.columns, self.rows);
        let mesh = self.mesh(0)?;
        Some(
            (0..rows)
                .flat_map(|row| (0..columns).map(move |column| (column, row)))
                .map(|(column, row)| mesh.quad(column, rows - 1 - row))
                .collect(),
        )
    }

    /// Reads a block through its own four corners, which follow bent or creased paper.
    /// Only blocks with at least three corners found in the picture are tried, so the empty
    /// margin of the page costs nothing.
    fn read_warped(&mut self, column: usize, row: usize) -> Option<(RawBlock, usize)> {
        // A block that one mesh cannot read is tried with the next: whatever any of them reads is kept.
        let meshes = if self.effort == Effort::Deep {
            MESH_TRUSTS.len()
        } else {
            1
        };
        (0..meshes).find_map(|variant| self.read_warped_with(variant, column, row))
    }

    fn read_warped_with(
        &mut self,
        variant: usize,
        column: usize,
        row: usize,
    ) -> Option<(RawBlock, usize)> {
        let from_bottom = self.rows - 1 - row;
        let mesh = self.mesh(variant)?;
        let quad = mesh.quad(column, from_bottom);
        if quad.corners_found < MIN_CORNERS {
            return None;
        }
        let ideal_middle = {
            let (low, high) = (
                mesh.ideal(column, from_bottom),
                mesh.ideal(column + 1, from_bottom + 1),
            );
            (f64::midpoint(low.0, high.0), f64::midpoint(low.1, high.1))
        };
        for window in WARPED_DOT_WINDOWS {
            // Like the original, read the block as sampled and, failing that, from the sharpest
            // sub-blocks of nine slightly shifted samplings; that absorbs what the corners and
            // edge middles do not capture of the bending inside the block.
            let shifted: Vec<DotGrid> = (0..SHIFT_COUNT)
                .map(|shift| {
                    let (dy, dx) = ((shift / 3) as f64 - 1.0, (shift % 3) as f64 - 1.0);
                    self.sample_quad(&quad, window, (dx * WARPED_SHIFT, dy * WARPED_SHIFT))
                })
                .collect();
            let mut found = self
                .recognise(&shifted[CENTER_SHIFT])
                .or_else(|| self.recognise(&best_focused_grid(&shifted)));
            if found.is_none() && self.effort == Effort::Deep {
                found = self.read_aligned(&shifted);
            }
            if let Some(found) = found {
                let middle = quad.at(0.5, 0.5);
                self.shifts[row * self.columns + column] =
                    Some((middle.0 - ideal_middle.0, middle.1 - ideal_middle.1));
                return Some(found);
            }
        }
        None
    }

    /// Reads a block whose dots are not where the corners and edges put them, by letting the
    /// error correction decide where they are: every quadrant of the block takes one of its
    /// sharpest samplings out of the nine, and the combinations are tried until the checksum
    /// agrees. Needs the orientation of the page, which is known once any block was read.
    fn read_aligned(&mut self, shifted: &[DotGrid]) -> Option<(RawBlock, usize)> {
        let orientation = self.orientation?;
        let ranked: Vec<Vec<usize>> =
            (0..QUADRANTS)
                .map(|quadrant| {
                    let (top, left) = quadrant_origin(quadrant);
                    let mut order: Vec<usize> = (0..shifted.len()).collect();
                    order.sort_by(|&a, &b| {
                        sub_block_dispersion(&shifted[b], top, left)
                            .total_cmp(&sub_block_dispersion(&shifted[a], top, left))
                    });
                    order.truncate(ALIGNED_CANDIDATES);
                    order
                })
                .collect();
        for combination in 0..ALIGNED_CANDIDATES.pow(QUADRANTS as u32) {
            let mut grid = [[0u8; BLOCK_DOTS]; BLOCK_DOTS];
            let mut rest = combination;
            for (quadrant, order) in ranked.iter().enumerate() {
                let chosen = &shifted[order[rest % ALIGNED_CANDIDATES]];
                rest /= ALIGNED_CANDIDATES;
                let (top, left) = quadrant_origin(quadrant);
                for j in top..top + QUADRANT {
                    grid[j][left..left + QUADRANT]
                        .copy_from_slice(&chosen[j][left..left + QUADRANT]);
                }
            }
            if let Some((block, corrected)) = self.read_with_one_threshold(&grid, orientation) {
                self.learn_from(&grid, &block);
                return Some((block, corrected));
            }
        }
        None
    }

    /// One plain attempt at turning the gray levels into a block: the variant that read last, a
    /// threshold at the mean. Cheap, for trying many samplings.
    fn read_with_one_threshold(
        &self,
        grid: &DotGrid,
        orientation: usize,
    ) -> Option<(RawBlock, usize)> {
        let (weight, threshold_shift) = self.variant_parameters(self.last_good_variant);
        let adjusted = overlap_corrected(grid, weight, self.intensity.max);
        let limit = adjusted.iter().flatten().sum::<i32>() / 1024 + threshold_shift * weight;
        let mut rows = [0u32; BLOCK_DOTS];
        for (j, row) in rows.iter_mut().enumerate() {
            for i in 0..BLOCK_DOTS {
                let (a, b) = orient(orientation, j, i);
                if adjusted[a][b] < limit {
                    *row |= 1 << i;
                }
            }
            *row ^= row_mask(j);
        }
        let mut block = RawBlock::from_rows(&rows);
        let corrected = block.correct().filter(|&n| n <= MAX_CORRECTIONS)?;
        block_crc_matches(&block).then_some((block, corrected))
    }

    /// The 32x32 dots of a block as gray levels, each the average of a few samples around
    /// the dot's place inside the quadrilateral.
    fn sample_quad(&self, quad: &Quad, window: f64, shift: (f64, f64)) -> DotGrid {
        let cell = BLOCK_DOTS as f64 + 3.0;
        let pitch = {
            let length = |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).hypot(a.1 - b.1);
            (length(quad.lower_left, quad.lower_right)
                + length(quad.upper_left, quad.upper_right)
                + length(quad.lower_left, quad.upper_left)
                + length(quad.lower_right, quad.upper_right))
                / 4.0
                / cell
        };
        let reach = window * pitch;
        let white = f64::from(self.intensity.max as u8);
        let mut grid = [[0u8; BLOCK_DOTS]; BLOCK_DOTS];
        for (j, row) in grid.iter_mut().enumerate() {
            for (i, dot) in row.iter_mut().enumerate() {
                let on_page = quad.at((2.0 + i as f64) / cell, (2.0 + j as f64) / cell);
                let middle = (on_page.0 + shift.0 * pitch, on_page.1 + shift.1 * pitch);
                let around = [
                    (0.0, 0.0),
                    (0.0, 0.0),
                    (reach, 0.0),
                    (-reach, 0.0),
                    (0.0, reach),
                    (0.0, -reach),
                ];
                let sum: f64 = around
                    .iter()
                    .map(|&(dx, dy)| {
                        sample(self.bitmap, (middle.0 + dx, middle.1 + dy)).unwrap_or(white)
                    })
                    .sum();
                *dot = (sum / around.len() as f64).round() as u8;
            }
        }
        grid
    }

    /// The average local rotation of the readable blocks around this one.
    fn neighbour_tilt(&self, column: usize, row: usize) -> f64 {
        let known: Vec<f64> = (row.saturating_sub(1)..=(row + 1).min(self.rows - 1))
            .flat_map(|r| {
                (column.saturating_sub(1)..=(column + 1).min(self.columns - 1)).map(move |c| (c, r))
            })
            .filter_map(|(c, r)| self.tilts[r * self.columns + c])
            .collect();
        if known.is_empty() {
            0.0
        } else {
            known.iter().sum::<f64>() / known.len() as f64
        }
    }

    /// Whether the corner mesh has found the lines around a cell, which is then a block of the
    /// page, not empty margin.
    pub(crate) fn mesh_covers(&mut self, column: usize, row: usize) -> bool {
        let from_bottom = self.rows - 1 - row;
        self.mesh(0)
            .is_some_and(|mesh| mesh.quad(column, from_bottom).corners_found >= MIN_CORNERS)
    }

    /// Whether the detector that learns how dots look on this page has seen too few blocks
    /// to be of use yet.
    pub(crate) fn detector_needs_blocks(&self) -> bool {
        !self.detector.is_ready()
    }

    /// Whether a block that is not read yet has a readable neighbour to learn from.
    pub(crate) fn has_neighbour_to_learn_from(&self, column: usize, row: usize) -> bool {
        !self.neighbour_shifts(column, row).is_empty()
    }

    /// The shifts of the readable blocks around this one.
    fn neighbour_shifts(&self, column: usize, row: usize) -> Vec<(f64, f64)> {
        Self::shifts_around(&self.shifts, self.columns, self.rows, column, row)
    }

    fn mean(shifts: &[(f64, f64)]) -> Option<(f64, f64)> {
        let n = shifts.len() as f64;
        (!shifts.is_empty()).then(|| {
            (
                shifts.iter().map(|s| s.0).sum::<f64>() / n,
                shifts.iter().map(|s| s.1).sum::<f64>() / n,
            )
        })
    }

    fn shifts_around(
        shifts: &[Option<(f64, f64)>],
        columns: usize,
        rows: usize,
        column: usize,
        row: usize,
    ) -> Vec<(f64, f64)> {
        (row.saturating_sub(1)..=(row + 1).min(rows - 1))
            .flat_map(|r| {
                (column.saturating_sub(1)..=(column + 1).min(columns - 1)).map(move |c| (c, r))
            })
            .filter(|&at| at != (column, row))
            .filter_map(|(c, r)| shifts[r * columns + c])
            .collect()
    }

    /// The shift of every block; blocks that did not read take the average of
    /// their neighbours', spreading outwards, and what has no neighbour stays put.
    pub(crate) fn shift_map(&self) -> Vec<(f64, f64)> {
        let mut known = self.shifts.clone();
        for _ in 0..self.columns.max(self.rows) {
            let mut next = known.clone();
            for row in 0..self.rows {
                for column in 0..self.columns {
                    if known[row * self.columns + column].is_none() {
                        next[row * self.columns + column] = Self::mean(&Self::shifts_around(
                            &known,
                            self.columns,
                            self.rows,
                            column,
                            row,
                        ));
                    }
                }
            }
            if next == known {
                break;
            }
            known = next;
        }
        known.into_iter().map(|s| s.unwrap_or((0.0, 0.0))).collect()
    }

    /// Reads a few blocks spread over the page at each sharpness level and puts the
    /// level that read most first, so most blocks need no second attempt.
    pub(crate) fn tune_sharpness(&mut self, columns: usize, rows: usize) {
        let levels = self.boosts.clone();
        let mut best = (0, 0);
        for (index, level) in levels.iter().enumerate() {
            self.boosts = vec![*level];
            let readable = self
                .probe_positions(columns, rows)
                .filter(|&(column, row)| {
                    // The whole chain, so a page that only the corner mesh can read is found too.
                    matches!(self.read(column, row), BlockOutcome::Readable { .. })
                })
                .count();
            if readable > best.1 {
                best = (index, readable);
            }
        }
        self.boosts = levels;
        let preferred = self.boosts.remove(best.0);
        self.boosts.insert(0, preferred);
    }

    fn probe_positions(
        &self,
        columns: usize,
        rows: usize,
    ) -> impl Iterator<Item = (usize, usize)> + use<> {
        let (total_columns, total_rows) = (self.columns, self.rows);
        (1..=rows).flat_map(move |r| {
            (1..=columns).map(move |c| {
                (
                    total_columns * c / (columns + 1),
                    total_rows * r / (rows + 1),
                )
            })
        })
    }

    /// Whether any of a few blocks spread over the page can be read; a wrong
    /// grid fit reads nothing, and finding that out is cheap.
    pub(crate) fn finds_blocks(&mut self, columns: usize, rows: usize) -> bool {
        let before = std::mem::replace(&mut self.effort, Effort::Normal);
        let found = self.probe_blocks(columns, rows);
        self.effort = before;
        found
    }

    fn probe_blocks(&mut self, columns: usize, rows: usize) -> bool {
        (1..=rows).any(|r| {
            (1..=columns).any(|c| {
                let column = self.columns * c / (columns + 1);
                let row = self.rows * r / (rows + 1);
                matches!(self.read_at(column, row), BlockOutcome::Readable { .. })
            })
        })
    }

    fn read_at(&mut self, column: usize, row: usize) -> BlockOutcome {
        self.read_posed(column, row, (0.0, 0.0), 0.0, true)
    }

    /// Reads a block cut out at a shifted position and turned by `tilt` against the
    /// page-wide tilt, with every sharpness level or only the preferred one.
    fn read_posed(
        &mut self,
        column: usize,
        row: usize,
        shift: (f64, f64),
        tilt: f64,
        every_level: bool,
    ) -> BlockOutcome {
        let (x_angle, y_angle) = self.page_angles;
        self.grid.x_angle = x_angle + tilt;
        self.grid.y_angle = y_angle - tilt;
        let outcome = self.read_shifted(column, row, shift, tilt, every_level);
        self.grid.x_angle = x_angle;
        self.grid.y_angle = y_angle;
        outcome
    }

    fn read_shifted(
        &mut self,
        column: usize,
        row: usize,
        shift: (f64, f64),
        tilt: f64,
        every_level: bool,
    ) -> BlockOutcome {
        let x0 =
            (self.grid.x_peak + self.grid.x_step * (column as f64 - self.border) + shift.0) as i32;
        let y0 = (self.grid.y_peak
            + self.grid.y_step * (self.rows as f64 - row as f64 - 1.0 - self.border)
            + shift.1) as i32;
        let mut outcome = BlockOutcome::Missing;
        let levels = if every_level { self.boosts.len() } else { 1 };
        for boost in self.boosts.clone().into_iter().take(levels) {
            self.sharpness = (self.base_sharpness * boost.0 + boost.1).min(MAX_SHARPNESS);
            self.resample(x0, y0);
            let guided = shift != (0.0, 0.0) || tilt != 0.0;
            let found = self.locate_block();
            let predicted = found.is_none() && guided;
            let Some(fit) = found.or_else(|| predicted.then(|| self.predicted_fit())) else {
                break;
            };
            match self.read_dots(&fit) {
                Some((block, corrected)) => {
                    let at = row * self.columns + column;
                    self.shifts[at] = Some(self.real_shift(&fit, shift));
                    self.tilts[at] = Some(tilt);
                    outcome = BlockOutcome::Readable { block, corrected };
                    break;
                }
                // A guessed position proves nothing: without lines there may be no block.
                None if predicted => {}
                None => outcome = BlockOutcome::Unreadable,
            }
        }
        self.sharpness = self.base_sharpness;
        outcome
    }

    /// Where the block's grid lines should be when the buffer was cut at the
    /// position its neighbours suggest; used when the block's own lines are too
    /// faint or bent to find.
    fn predicted_fit(&self) -> BlockFit {
        let x_pitch = self.grid.x_step / GRID_CELL_DOTS;
        let y_pitch = self.grid.y_step / GRID_CELL_DOTS;
        BlockFit {
            x_origin: self.border * self.grid.x_step + 2.0 * x_pitch,
            y_origin: self.border * self.grid.y_step + 2.0 * y_pitch,
            x_pitch,
            y_pitch,
        }
    }

    /// How far the block's own grid lines are from where the fitted grid expected
    /// them, given the shift the buffer was cut with.
    fn real_shift(&self, fit: &BlockFit, cut_with: (f64, f64)) -> (f64, f64) {
        let along = |origin: f64, pitch: f64, step: f64| {
            let expected = self.border * step;
            let off = origin - 2.0 * pitch - expected;
            (off + step / 2.0).rem_euclid(step) - step / 2.0
        };
        (
            cut_with.0 + along(fit.x_origin, fit.x_pitch, self.grid.x_step),
            cut_with.1 + along(fit.y_origin, fit.y_pitch, self.grid.y_step),
        )
    }

    /// Cuts the block out of the page, undoing the tilt by bilinear interpolation,
    /// and sharpens it when the scan is blurred.
    fn resample(&mut self, x0: i32, y0: i32) {
        let (bw, bh) = (self.buffer_width, self.buffer_height);
        let (sx, sy) = (self.bitmap.width() as i32, self.bitmap.height() as i32);
        let sharpen = self.sharpness > 0.0;
        let target = if sharpen {
            &mut self.sharpened
        } else {
            &mut self.rotated
        };
        let white = self.intensity.max as u8;
        for j in 0..bh {
            let x_exact = f64::from(x0) + f64::from(y0 + j as i32) * self.grid.x_angle;
            let x_whole = if x_exact >= 0.0 {
                x_exact as i32
            } else {
                (x_exact - 1.0) as i32
            };
            let x_fraction = x_exact - f64::from(x_whole);
            for i in 0..bw {
                let x = x_whole + i as i32;
                let y_exact =
                    f64::from(y0 + j as i32) + f64::from(x0 + i as i32) * self.grid.y_angle;
                let y = if y_exact > 0.0 {
                    y_exact as i32
                } else {
                    (y_exact - 1.0) as i32
                };
                let y_fraction = y_exact - f64::from(y);
                target[j * bw + i] = if x < 0 || x >= sx - 1 || y < 0 || y >= sy - 1 {
                    white
                } else {
                    let (x, y) = (x as usize, y as usize);
                    let p = |dx: usize, dy: usize| f64::from(self.bitmap.at(x + dx, y + dy));
                    ((p(0, 0) + (p(1, 0) - p(0, 0)) * x_fraction) * (1.0 - y_fraction)
                        + (p(0, 1) + (p(1, 1) - p(0, 1)) * x_fraction) * y_fraction)
                        as u8
                };
            }
        }
        if sharpen {
            self.sharpen();
        }
    }

    fn sharpen(&mut self) {
        let (bw, bh) = (self.buffer_width, self.buffer_height);
        let s = self.sharpness;
        let (low, high) = (self.intensity.min, self.intensity.max);
        for j in 0..bh {
            for i in 0..bw {
                let at = j * bw + i;
                self.rotated[at] = if i == 0 || i == bw - 1 || j == 0 || j == bh - 1 {
                    self.sharpened[at]
                } else {
                    let c = f64::from(self.sharpened[at]);
                    let around = f64::from(self.sharpened[at - bw])
                        + f64::from(self.sharpened[at - 1])
                        + f64::from(self.sharpened[at + 1])
                        + f64::from(self.sharpened[at + bw]);
                    ((c * (1.0 + 4.0 * s) - around * s) as i32)
                        .min(high)
                        .max(low) as u8
                };
            }
        }
    }

    /// Finds the exact grid lines of this block; `None` if there is no grid.
    fn locate_block(&self) -> Option<BlockFit> {
        let (bw, bh) = (self.buffer_width, self.buffer_height);
        let mut column_sums = vec![0i32; bw];
        let mut row_sums = vec![0i32; bh];
        for j in 0..bh {
            for i in 0..bw {
                let value = i32::from(self.rotated[j * bw + i]);
                column_sums[i] += value;
                row_sums[j] += value;
            }
        }
        let x = find_peaks(&column_sums).filter(|fit| fit.weight > 0.0)?;
        if (x.step - self.grid.x_step).abs() > self.grid.x_step / 16.0 {
            return None;
        }
        let y = find_peaks(&row_sums).filter(|fit| fit.weight > 0.0)?;
        if (y.step - self.grid.y_step).abs() > self.grid.y_step / 16.0 {
            return None;
        }
        let x_pitch = x.step / GRID_CELL_DOTS;
        let y_pitch = y.step / GRID_CELL_DOTS;
        Some(BlockFit {
            x_origin: x.peak + 2.0 * x_pitch,
            y_origin: y.peak + 2.0 * y_pitch,
            x_pitch,
            y_pitch,
        })
    }

    fn read_dots(&mut self, fit: &BlockFit) -> Option<(RawBlock, usize)> {
        for dot_size in 1..=self.max_dot_size {
            let shifted = self.sample_shifted_grids(fit, dot_size);
            if let Some(found) = self.recognise(&shifted[CENTER_SHIFT]) {
                return Some(found);
            }
            if self.effort == Effort::Quick {
                continue;
            }
            if let Some(found) = self.recognise(&best_focused_grid(&shifted)) {
                return Some(found);
            }
        }
        None
    }

    /// Samples the 32x32 dots nine times, shifted by up to one pixel in every direction.
    fn sample_shifted_grids(&self, fit: &BlockFit, dot_size: usize) -> Vec<DotGrid> {
        let half_dot = dot_size as f64 / 2.0 - 1.0;
        let mut grids = vec![[[0u8; BLOCK_DOTS]; BLOCK_DOTS]; SHIFT_COUNT];
        for j in 0..BLOCK_DOTS {
            let y = (fit.y_origin + fit.y_pitch * j as f64 - half_dot) as i32;
            for i in 0..BLOCK_DOTS {
                let x = (fit.x_origin + fit.x_pitch * i as f64 - half_dot) as i32;
                for (shift, grid) in grids.iter_mut().enumerate() {
                    let (dy, dx) = ((shift / 3) as i32 - 1, (shift % 3) as i32 - 1);
                    grid[j][i] = self.average_dot(x + dx, y + dy, dot_size);
                }
            }
        }
        grids
    }

    fn pixel(&self, x: i32, y: i32) -> i32 {
        if x < 0 || y < 0 || x as usize >= self.buffer_width || y as usize >= self.buffer_height {
            return self.intensity.max;
        }
        i32::from(self.rotated[y as usize * self.buffer_width + x as usize])
    }

    fn average_dot(&self, x: i32, y: i32, dot_size: usize) -> u8 {
        let sum_of = |cells: &[(i32, i32)]| -> i32 {
            cells
                .iter()
                .map(|&(dx, dy)| self.pixel(x + dx, y + dy))
                .sum::<i32>()
                / cells.len() as i32
        };
        let value = match dot_size {
            4 => sum_of(&[
                (1, 0),
                (2, 0),
                (0, 1),
                (1, 1),
                (2, 1),
                (3, 1),
                (0, 2),
                (1, 2),
                (2, 2),
                (3, 2),
                (1, 3),
                (2, 3),
            ]),
            3 => sum_of(&[
                (0, 0),
                (1, 0),
                (2, 0),
                (0, 1),
                (1, 1),
                (2, 1),
                (0, 2),
                (1, 2),
                (2, 2),
            ]),
            2 => sum_of(&[(0, 0), (1, 0), (0, 1), (1, 1)]),
            _ => self.pixel(x, y),
        };
        value as u8
    }

    /// Turns sampled gray levels into a block: by thresholds first, and when that fails with
    /// what was learned from the blocks read so far. Every block read teaches the detector.
    fn recognise(&mut self, grid: &DotGrid) -> Option<(RawBlock, usize)> {
        let mut found = self.recognise_by_threshold(grid);
        if found.is_none() && self.effort == Effort::Deep {
            found = self.recognise_learned(grid);
        }
        if let Some((block, _)) = &found {
            self.learn_from(grid, block);
        }
        found
    }

    /// Gives the detector a block that was read, with the gray levels it was read from. The
    /// orientations of [`orient`] count in the transposed picture, because the neighbour
    /// correction of the original reads the centre dot transposed (see `overlap_corrected`);
    /// the detector works on the samples as they are, hence `b` before `a` below.
    fn learn_from(&mut self, grid: &DotGrid, block: &RawBlock) {
        let Some(orientation) = self.orientation else {
            return;
        };
        let rows = block.rows();
        let mut dark_dots = [[false; BLOCK_DOTS]; BLOCK_DOTS];
        for (j, row) in rows.iter().enumerate() {
            let dark = row ^ row_mask(j);
            for i in 0..BLOCK_DOTS {
                let (a, b) = orient(orientation, j, i);
                dark_dots[b][a] = dark >> i & 1 == 1;
            }
        }
        self.detector.learn(grid, &dark_dots);
    }

    /// Reads the dots with the detector fitted to this page, and repairs the bits, with the
    /// bytes it was least sure about as erasures if need be.
    fn recognise_learned(&self, grid: &DotGrid) -> Option<(RawBlock, usize)> {
        let orientation = self.orientation?;
        if !self.detector.is_ready() {
            return None;
        }
        let scores = self.detector.scores(grid);
        let mut rows = [0u32; BLOCK_DOTS];
        let mut certainty = [i32::MAX; BLOCK_LEN];
        for (j, row) in rows.iter_mut().enumerate() {
            for i in 0..BLOCK_DOTS {
                let (a, b) = orient(orientation, j, i);
                let score = scores[b][a];
                if score > 0.0 {
                    *row |= 1 << i;
                }
                let byte = j * 4 + i / 8;
                certainty[byte] = certainty[byte].min((score.abs() * CERTAINTY_SCALE) as i32);
            }
            *row ^= row_mask(j);
        }
        let block = RawBlock::from_rows(&rows);
        let mut direct = block.clone();
        if let Some(corrected) = direct.correct().filter(|&n| n <= MAX_CORRECTIONS)
            && block_crc_matches(&direct)
        {
            return Some((direct, corrected));
        }
        repair_doubtful_bytes(&block, &certainty)
    }

    /// Turns sampled gray levels into bits and repairs them. Tries every
    /// orientation until one is known, and several neighbour-overlap and
    /// threshold variants, starting with the one that worked last.
    fn recognise_by_threshold(&mut self, grid: &DotGrid) -> Option<(RawBlock, usize)> {
        for orientation in 0..ORIENTATIONS {
            if self.orientation.is_some_and(|known| known != orientation) {
                continue;
            }
            let mut doubtful: Option<(RawBlock, [i32; BLOCK_LEN])> = None;
            let tries: Vec<(usize, bool)> = if self.effort == Effort::Quick {
                (0..QUICK_VARIANTS)
                    .map(|attempt| (attempt, false))
                    .collect()
            } else {
                (0..THRESHOLD_VARIANTS)
                    .flat_map(|attempt| [(attempt, false), (attempt, true)])
                    .collect()
            };
            for (attempt, local) in tries {
                let variant = (attempt + self.last_good_variant) % THRESHOLD_VARIANTS;
                let (weight, threshold_shift) = self.variant_parameters(variant);
                let adjusted = overlap_corrected(grid, weight, self.intensity.max);
                let limits = if local {
                    quadrant_limits(&adjusted, threshold_shift * weight)
                } else {
                    let whole =
                        adjusted.iter().flatten().sum::<i32>() / 1024 + threshold_shift * weight;
                    [[whole; 2]; 2]
                };
                let mut rows = [0u32; BLOCK_DOTS];
                let mut certainty = [i32::MAX; BLOCK_LEN];
                for (j, row) in rows.iter_mut().enumerate() {
                    for i in 0..BLOCK_DOTS {
                        let (a, b) = orient(orientation, j, i);
                        let limit = limits[a / QUADRANT][b / QUADRANT];
                        if adjusted[a][b] < limit {
                            *row |= 1 << i;
                        }
                        let byte = j * 4 + i / 8;
                        certainty[byte] = certainty[byte].min((adjusted[a][b] - limit).abs());
                    }
                    *row ^= row_mask(j);
                }
                let mut block = RawBlock::from_rows(&rows);
                if attempt == 0 && !local {
                    doubtful = Some((block.clone(), certainty));
                }
                let Some(corrected) = block.correct().filter(|&n| n <= MAX_CORRECTIONS) else {
                    continue;
                };
                if block_crc_matches(&block) {
                    self.orientation = Some(orientation);
                    self.last_good_variant = variant;
                    return Some((block, corrected));
                }
            }
            // Nothing read as it is: treat the bytes whose dots were hardest to tell apart
            // as erased, which doubles what the error correction can repair.
            if self.effort == Effort::Deep
                && let Some(found) = doubtful
                    .and_then(|(block, certainty)| repair_doubtful_bytes(&block, &certainty))
            {
                self.orientation = Some(orientation);
                return Some(found);
            }
        }
        None
    }

    fn variant_parameters(&self, variant: usize) -> (i32, i32) {
        let weight = NEIGHBOUR_WEIGHTS[variant % 3];
        let spread = self.intensity.min - self.intensity.max;
        let shift = match variant / 3 {
            0 => 0,
            1 => spread / 16,
            _ => -spread / 16,
        };
        (weight, shift)
    }
}

struct BlockFit {
    x_origin: f64,
    y_origin: f64,
    x_pitch: f64,
    y_pitch: f64,
}

/// Half a block; shadows of creases change the paper's brightness from one
/// quadrant to the next, so each gets its own black/white limit.
const QUADRANT: usize = BLOCK_DOTS / 2;
/// Quadrants of a block.
const QUADRANTS: usize = 4;

fn quadrant_limits(adjusted: &[[i32; BLOCK_DOTS]; BLOCK_DOTS], shift: i32) -> [[i32; 2]; 2] {
    let mut limits = [[0; 2]; 2];
    for (q_row, limit_row) in limits.iter_mut().enumerate() {
        for (q_col, limit) in limit_row.iter_mut().enumerate() {
            let sum: i32 = adjusted[q_row * QUADRANT..(q_row + 1) * QUADRANT]
                .iter()
                .flat_map(|row| &row[q_col * QUADRANT..(q_col + 1) * QUADRANT])
                .sum();
            *limit = sum / (QUADRANT * QUADRANT) as i32 + shift;
        }
    }
    limits
}

/// Erased bytes tried for a block that does not read as it is. Each erased byte costs one
/// of the 32 parity bytes (an unknown error costs two), and wrongly erased good bytes waste
/// that; with the checksum as the judge, a few sizes are tried. At most 24 are used: the
/// fewer parity bytes are left over to check the result, the more likely a wrong block passes.
const ERASURE_COUNTS: [usize; 2] = [16, 24];

/// Tries the least certain bytes of `block` as erasures; the repaired block must pass its
/// checksum. Returns it with the number of bytes that changed.
fn repair_doubtful_bytes(
    block: &RawBlock,
    certainty: &[i32; BLOCK_LEN],
) -> Option<(RawBlock, usize)> {
    let mut by_certainty: Vec<usize> = (0..BLOCK_LEN).collect();
    by_certainty.sort_by_key(|&byte| (certainty[byte], byte));
    ERASURE_COUNTS.iter().find_map(|&count| {
        let mut repaired = block.clone();
        repaired.correct_erasing(&by_certainty[..count])?;
        let changed = (0..BLOCK_LEN)
            .filter(|&byte| repaired.0[byte] != block.0[byte])
            .count();
        block_crc_matches(&repaired).then_some((repaired, changed))
    })
}

fn block_crc_matches(block: &RawBlock) -> bool {
    debug_assert_eq!(ECC_PAD + 1, 128);
    crc16(&block.0[..CRC_COVERED]) ^ CRC_MASK
        == u16::from_le_bytes([block.0[CRC_COVERED], block.0[CRC_COVERED + 1]])
}

/// Maps a destination bit position to a source cell for one of the eight
/// rotations and mirrorings of the page.
fn orient(orientation: usize, j: usize, i: usize) -> (usize, usize) {
    let last = BLOCK_DOTS - 1;
    match orientation {
        0 => (j, i),
        1 => (i, last - j),
        2 => (last - j, last - i),
        3 => (last - i, j),
        4 => (i, j),
        5 => (j, last - i),
        6 => (last - i, last - j),
        _ => (last - j, i),
    }
}

/// Subtracts the neighbours' brightness to undo dots bleeding into each other.
/// Indexing mirrors the original program exactly.
fn overlap_corrected(grid: &DotGrid, weight: i32, white: i32) -> [[i32; BLOCK_DOTS]; BLOCK_DOTS] {
    let last = BLOCK_DOTS - 1;
    let mut result = [[0i32; BLOCK_DOTS]; BLOCK_DOTS];
    for j in 0..BLOCK_DOTS {
        for i in 0..BLOCK_DOTS {
            let mut c = i32::from(grid[i][j]) * weight;
            c -= if i > 0 {
                i32::from(grid[j][i - 1])
            } else {
                white
            };
            c -= if i < last {
                i32::from(grid[j][i + 1])
            } else {
                white
            };
            c -= if j > 0 {
                i32::from(grid[j - 1][i])
            } else {
                white
            };
            c -= if j < last {
                i32::from(grid[j + 1][i])
            } else {
                white
            };
            result[j][i] = c;
        }
    }
    result
}

/// Assembles a grid from the shifted version with the strongest contrast in
/// each 8x8 sub-block; this compensates small distortions of the scan.
fn best_focused_grid(shifted: &[DotGrid]) -> DotGrid {
    let mut combined = [[0u8; BLOCK_DOTS]; BLOCK_DOTS];
    for top in (0..BLOCK_DOTS).step_by(SUBBLOCK_SIZE) {
        for left in (0..BLOCK_DOTS).step_by(SUBBLOCK_SIZE) {
            let dispersions: Vec<f64> = shifted
                .iter()
                .map(|grid| dispersion(grid, top, left))
                .collect();
            let min = dispersions.iter().copied().fold(f64::INFINITY, f64::min);
            let (mut best, mut max) = (0, f64::NEG_INFINITY);
            for (shift, &value) in dispersions.iter().enumerate() {
                if value > max {
                    max = value;
                    best = shift;
                }
            }
            if max - min < max / 5.0 {
                best = CENTER_SHIFT;
            }
            for y in top..top + SUBBLOCK_SIZE {
                combined[y][left..left + SUBBLOCK_SIZE]
                    .copy_from_slice(&shifted[best][y][left..left + SUBBLOCK_SIZE]);
            }
        }
    }
    combined
}

/// Top left dot of a quadrant, which are counted row by row.
fn quadrant_origin(quadrant: usize) -> (usize, usize) {
    ((quadrant / 2) * QUADRANT, (quadrant % 2) * QUADRANT)
}

/// How sharp a quadrant is: the contrast of its sub-blocks together.
fn sub_block_dispersion(grid: &DotGrid, top: usize, left: usize) -> f64 {
    [
        (0, 0),
        (0, SUBBLOCK_SIZE),
        (SUBBLOCK_SIZE, 0),
        (SUBBLOCK_SIZE, SUBBLOCK_SIZE),
    ]
    .iter()
    .map(|&(down, right)| dispersion(grid, top + down, left + right))
    .sum()
}

fn dispersion(grid: &DotGrid, top: usize, left: usize) -> f64 {
    let (mut sum, mut squares) = (0.0f64, 0.0f64);
    for row in &grid[top..top + SUBBLOCK_SIZE] {
        for &value in &row[left..left + SUBBLOCK_SIZE] {
            let c = f64::from(value);
            sum += c;
            squares += c * c;
        }
    }
    squares * (SUBBLOCK_SIZE * SUBBLOCK_SIZE) as f64 - sum * sum
}

const _: () = assert!(UNREADABLE == MAX_CORRECTIONS + 1);
