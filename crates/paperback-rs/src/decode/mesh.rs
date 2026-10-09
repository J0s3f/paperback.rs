// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

//! Follows the printed grid lines across a page that is bent, creased or photographed
//! at an angle. The straight grid the reader starts with fits such a page only near its
//! middle; here every corner where four blocks meet is found on its own, starting from
//! the middle and moving outwards, so that each block can be read through its own four
//! corners.

use std::collections::VecDeque;

use super::bitmap::Bitmap;
use super::grid::Grid;

pub(crate) type Point = (f64, f64);

/// Length of the arms of the cross that is matched against a corner, as a share of the
/// distance between two grid lines.
const ARM_SHARE: f64 = 0.2;
/// Distance in pixels from the line to the paper on both sides that it is compared with.
const LINE_OFFSET: f64 = 2.0;
/// How far from the expected place the first corner may be, as a share of a block.
const SEED_RADIUS: f64 = 0.3;
/// How far from the place its neighbours predict the next corner may be.
const FOLLOW_RADIUS: f64 = 0.12;
/// Corners around a corner that must be known before a surface is fitted through them.
const MIN_FIT_CORNERS: usize = 8;
/// Rounds of looking for corners that are missing among found ones.
const GAP_PASSES: usize = 3;
/// Limits of how much a step along a line may differ from the one before it.
const MIN_STEP_RATIO: f64 = 0.85;
const MAX_STEP_RATIO: f64 = 1.15;
/// A corner counts as found when its contrast is this share of the contrast of the corner it
/// was predicted from (lines fade towards the edge of a photograph) ...
const MIN_SCORE_SHARE: f64 = 0.4;
/// ... and at least this share of the first corner's.
const FLOOR_SCORE_SHARE: f64 = 0.12;
/// Contrast, in gray levels, below which nothing is a line.
const MIN_SCORE: f64 = 6.0;
/// Times a corner is looked for from different neighbours before it is given up.
const MAX_ATTEMPTS: u8 = 4;

#[derive(Clone, Copy)]
struct Axes {
    along_x: Point,
    along_y: Point,
}

/// The corners of the blocks of one page.
#[derive(Clone, Debug)]
pub(crate) struct Mesh {
    grid: Grid,
    /// How much of the perspective prediction of the next corner is used, the rest being the
    /// last step: 1 is exact for a camera view but amplifies the error of a corner that is
    /// out of place, 0 follows the last step only. No setting suits every page, so a block
    /// that one mesh cannot read is tried with the others.
    perspective_trust: f64,
    /// Number of grid lines across and up.
    across: usize,
    up: usize,
    found: Vec<Option<Point>>,
    /// Middle of the line between corner (a, b) and (a + 1, b), at `b * across + a`.
    across_middles: Vec<Option<Point>>,
    /// Middle of the line between corner (a, b) and (a, b + 1).
    up_middles: Vec<Option<Point>>,
}

/// One block: its four corners, as found or, where a line was too faint, as estimated, and
/// the middle of each edge where it could be measured. Between them lies a smooth surface,
/// so a block on bent paper or in a photograph taken at an angle is read where its dots are,
/// not where a straight grid would put them.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Quad {
    pub(crate) lower_left: Point,
    pub(crate) lower_right: Point,
    pub(crate) upper_left: Point,
    pub(crate) upper_right: Point,
    /// How many of the four corners were found in the picture.
    pub(crate) corners_found: usize,
    /// Middles of the lower, upper, left and right edge.
    edge_middles: [Point; 4],
}

/// The mapping of a unit square onto a quadrilateral as a camera sees it: straight lines stay
/// straight, but distances along them shrink towards the far side.
#[derive(Clone, Copy, Debug)]
struct Projection {
    x: [f64; 3],
    y: [f64; 3],
    g: f64,
    h: f64,
}

impl Projection {
    /// The projection that takes (0,0), (1,0), (1,1) and (0,1) to the four given points.
    fn onto(p0: Point, p1: Point, p2: Point, p3: Point) -> Self {
        let (dx1, dx2, dx3) = (p1.0 - p2.0, p3.0 - p2.0, p0.0 - p1.0 + p2.0 - p3.0);
        let (dy1, dy2, dy3) = (p1.1 - p2.1, p3.1 - p2.1, p0.1 - p1.1 + p2.1 - p3.1);
        let determinant = dx1 * dy2 - dy1 * dx2;
        let affine = dx3.abs() < f64::EPSILON && dy3.abs() < f64::EPSILON;
        let (g, h) = if affine || determinant.abs() < f64::EPSILON {
            (0.0, 0.0)
        } else {
            (
                (dx3 * dy2 - dx2 * dy3) / determinant,
                (dx1 * dy3 - dy1 * dx3) / determinant,
            )
        };
        Self {
            x: [p1.0 - p0.0 + g * p1.0, p3.0 - p0.0 + h * p3.0, p0.0],
            y: [p1.1 - p0.1 + g * p1.1, p3.1 - p0.1 + h * p3.1, p0.1],
            g,
            h,
        }
    }

    fn at(&self, u: f64, v: f64) -> Point {
        let w = self.g * u + self.h * v + 1.0;
        (
            (self.x[0] * u + self.x[1] * v + self.x[2]) / w,
            (self.y[0] * u + self.y[1] * v + self.y[2]) / w,
        )
    }
}

impl Quad {
    /// A block with these corners; the edges run as a camera would see straight ones.
    pub(crate) fn new(
        lower_left: Point,
        lower_right: Point,
        upper_left: Point,
        upper_right: Point,
        corners_found: usize,
    ) -> Self {
        let projection = Projection::onto(lower_left, lower_right, upper_right, upper_left);
        Self {
            lower_left,
            lower_right,
            upper_left,
            upper_right,
            corners_found,
            edge_middles: [
                projection.at(0.5, 0.0),
                projection.at(0.5, 1.0),
                projection.at(0.0, 0.5),
                projection.at(1.0, 0.5),
            ],
        }
    }

    /// The same block with the measured middle of its edges (lower, upper, left, right);
    /// an edge that could not be measured stays as the perspective puts it.
    pub(crate) fn with_edge_middles(mut self, measured: [Option<Point>; 4]) -> Self {
        for (middle, measured) in self.edge_middles.iter_mut().zip(measured) {
            if let Some(point) = measured {
                *middle = point;
            }
        }
        self
    }

    /// The point at `u` across (0 left, 1 right) and `v` up (0 lower edge, 1 upper edge).
    /// The edges are curves through their two corners and their middle; the inside is
    /// blended between them (a Coons patch), which equals the perspective projection while
    /// the edges are straight.
    pub(crate) fn at(&self, u: f64, v: f64) -> Point {
        let curve = |from: Point, middle: Point, to: Point, t: f64| {
            let at_start = (1.0 - t) * (1.0 - 2.0 * t);
            let at_middle = 4.0 * t * (1.0 - t);
            let at_end = t * (2.0 * t - 1.0);
            (
                at_start * from.0 + at_middle * middle.0 + at_end * to.0,
                at_start * from.1 + at_middle * middle.1 + at_end * to.1,
            )
        };
        let [lower_middle, upper_middle, left_middle, right_middle] = self.edge_middles;
        let lower = curve(self.lower_left, lower_middle, self.lower_right, u);
        let upper = curve(self.upper_left, upper_middle, self.upper_right, u);
        let left = curve(self.lower_left, left_middle, self.upper_left, v);
        let right = curve(self.lower_right, right_middle, self.upper_right, v);
        // The corners are counted twice by the edges and taken off once.
        let corners = |pick: fn(Point) -> f64| {
            (1.0 - u) * (1.0 - v) * pick(self.lower_left)
                + u * (1.0 - v) * pick(self.lower_right)
                + (1.0 - u) * v * pick(self.upper_left)
                + u * v * pick(self.upper_right)
        };
        (
            (1.0 - v) * lower.0 + v * upper.0 + (1.0 - u) * left.0 + u * right.0 - corners(|p| p.0),
            (1.0 - v) * lower.1 + v * upper.1 + (1.0 - u) * left.1 + u * right.1 - corners(|p| p.1),
        )
    }
}

impl Mesh {
    /// Finds the corners of a page with `columns` by `rows` blocks, or `None` if not even
    /// one corner can be found.
    pub(crate) fn build(
        bitmap: &Bitmap,
        grid: Grid,
        columns: usize,
        rows: usize,
        page_angles: (f64, f64),
        perspective_trust: f64,
    ) -> Option<Self> {
        let mut mesh = Self {
            perspective_trust,
            grid: Grid {
                x_angle: page_angles.0,
                y_angle: page_angles.1,
                ..grid
            },
            across: columns + 1,
            up: rows + 1,
            found: vec![None; (columns + 1) * (rows + 1)],
            across_middles: vec![None; (columns + 1) * (rows + 1)],
            up_middles: vec![None; (columns + 1) * (rows + 1)],
        };
        let step = mesh.grid.x_step.min(mesh.grid.y_step);
        let (seed, reference) = mesh.find_seed(bitmap, step)?;
        mesh.found[seed.1 * mesh.across + seed.0] = Some(mesh.ideal(seed.0, seed.1));
        mesh.refine_seed(bitmap, seed, step);
        mesh.grow(bitmap, seed, reference, step);
        mesh.fill_gaps(bitmap, step, (reference * FLOOR_SCORE_SHARE).max(MIN_SCORE));
        mesh.measure_edge_middles(bitmap, step, (reference * FLOOR_SCORE_SHARE).max(MIN_SCORE));
        Some(mesh)
    }

    /// The corners of the block in column `column` and row `row_from_bottom`.
    pub(crate) fn quad(&self, column: usize, row_from_bottom: usize) -> Quad {
        let corners = [
            (column, row_from_bottom),
            (column + 1, row_from_bottom),
            (column, row_from_bottom + 1),
            (column + 1, row_from_bottom + 1),
        ];
        let points = corners.map(|(a, b)| self.corner_or_estimate(a, b));
        let (a, b) = (column, row_from_bottom);
        let at = |a: usize, b: usize| b * self.across + a;
        let found = corners
            .iter()
            .filter(|&&(a, b)| self.found[b * self.across + a].is_some())
            .count();
        Quad::new(points[0], points[1], points[2], points[3], found).with_edge_middles([
            self.across_middles[at(a, b)],
            self.across_middles[at(a, b + 1)],
            self.up_middles[at(a, b)],
            self.up_middles[at(a + 1, b)],
        ])
    }

    /// Where the straight grid puts the corner at line `a` across and `b` up.
    pub(crate) fn ideal(&self, across: usize, up: usize) -> Point {
        let grid = &self.grid;
        let start = (
            grid.x_peak + across as f64 * grid.x_step,
            grid.y_peak + up as f64 * grid.y_step,
        );
        let mut point = start;
        for _ in 0..3 {
            point.0 = start.0 + point.1 * grid.x_angle;
            point.1 = start.1 + point.0 * grid.y_angle;
        }
        point
    }

    fn corner_or_estimate(&self, a: usize, b: usize) -> Point {
        if let Some(point) = self.found[b * self.across + a] {
            return point;
        }
        // The straight grid, moved by how far the found corners around it moved.
        let ideal = self.ideal(a, b);
        let offsets: Vec<Point> = self
            .neighbours(a, b)
            .filter_map(|(na, nb)| {
                let found = self.found[nb * self.across + na]?;
                let expected = self.ideal(na, nb);
                Some((found.0 - expected.0, found.1 - expected.1))
            })
            .collect();
        if offsets.is_empty() {
            return ideal;
        }
        let n = offsets.len() as f64;
        (
            ideal.0 + offsets.iter().map(|o| o.0).sum::<f64>() / n,
            ideal.1 + offsets.iter().map(|o| o.1).sum::<f64>() / n,
        )
    }

    fn neighbours(&self, a: usize, b: usize) -> impl Iterator<Item = (usize, usize)> + use<> {
        let (across, up) = (self.across, self.up);
        (b.saturating_sub(1)..=(b + 1).min(up - 1)).flat_map(move |nb| {
            (a.saturating_sub(1)..=(a + 1).min(across - 1)).map(move |na| (na, nb))
        })
    }

    fn axes(&self) -> Axes {
        let g = &self.grid;
        let unit = |v: Point| {
            let length = v.0.hypot(v.1);
            (v.0 / length, v.1 / length)
        };
        Axes {
            along_x: unit((1.0, g.y_angle)),
            along_y: unit((g.x_angle, 1.0)),
        }
    }

    /// The best corner near the middle of the page, among a few places.
    fn find_seed(&self, bitmap: &Bitmap, step: f64) -> Option<((usize, usize), f64)> {
        let middle = (self.across / 2, self.up / 2);
        let around = [
            (0, 0),
            (1, 0),
            (0, 1),
            (usize::MAX, 0),
            (0, usize::MAX),
            (1, 1),
        ];
        let mut best: Option<((usize, usize), f64)> = None;
        for (da, db) in around {
            let a = middle.0.wrapping_add(da).min(self.across - 1);
            let b = middle.1.wrapping_add(db).min(self.up - 1);
            let Some((_, score)) = refine(
                bitmap,
                self.ideal(a, b),
                step * SEED_RADIUS,
                self.axes(),
                step,
            ) else {
                continue;
            };
            if best.is_none_or(|(_, s)| score > s) {
                best = Some(((a, b), score));
            }
        }
        best.filter(|&(_, score)| score >= MIN_SCORE)
    }

    fn refine_seed(&mut self, bitmap: &Bitmap, seed: (usize, usize), step: f64) {
        let ideal = self.ideal(seed.0, seed.1);
        if let Some((point, _)) = refine(bitmap, ideal, step * SEED_RADIUS, self.axes(), step) {
            self.found[seed.1 * self.across + seed.0] = Some(point);
        }
    }

    /// Finds the other corners one neighbour after the other, predicting each from the ones
    /// already found.
    fn grow(&mut self, bitmap: &Bitmap, seed: (usize, usize), reference: f64, step: f64) {
        let mut attempts = vec![0u8; self.found.len()];
        let mut scores = vec![0.0f64; self.found.len()];
        scores[seed.1 * self.across + seed.0] = reference;
        let mut queue: VecDeque<(usize, usize)> = VecDeque::new();
        queue.push_back(seed);
        let floor = (reference * FLOOR_SCORE_SHARE).max(MIN_SCORE);
        while let Some((a, b)) = queue.pop_front() {
            let here_at = b * self.across + a;
            let Some(here) = self.found[here_at] else {
                continue;
            };
            let minimum = (scores[here_at] * MIN_SCORE_SHARE).max(floor);
            for (na, nb, da, db) in self.next_to(a, b) {
                let at = nb * self.across + na;
                if self.found[at].is_some() || attempts[at] >= MAX_ATTEMPTS {
                    continue;
                }
                attempts[at] += 1;
                let (line_guess, axes) = self.predict(here, (a, b), (da, db));
                // Many corners around it say more than the line it was reached along.
                let guess = self.fitted_corner(na, nb).unwrap_or(line_guess);
                if let Some((point, score)) =
                    refine(bitmap, guess, step * FOLLOW_RADIUS, axes, step)
                    && score >= minimum
                {
                    self.found[at] = Some(point);
                    scores[at] = score;
                    queue.push_back((na, nb));
                } else if attempts[at] < MAX_ATTEMPTS {
                    // Another neighbour may predict it better later.
                    queue.push_back((a, b));
                }
            }
        }
    }

    /// Where the corner (`a`, `b`) lies on the smooth surface through the corners found around
    /// it: x and y are each fitted as a quadratic in the grid position over the 5 x 5 corners
    /// around it, nearer ones counting more. `None` if fewer corners than needed to pin the
    /// surface down are found, or if they all lie on one side.
    fn fitted_corner(&self, a: usize, b: usize) -> Option<Point> {
        const REACH: isize = 2;
        let mut samples: Vec<(f64, f64, Point)> = Vec::new();
        for db in -REACH..=REACH {
            for da in -REACH..=REACH {
                if (da, db) == (0, 0) {
                    continue;
                }
                if let Some(point) = self.found_at(a as isize + da, b as isize + db) {
                    samples.push((da as f64, db as f64, point));
                }
            }
        }
        if samples.len() < MIN_FIT_CORNERS {
            return None;
        }
        // Corners on both sides in each direction, or the quadratic runs away.
        let spread = |pick: fn(&(f64, f64, Point)) -> f64| {
            samples.iter().any(|s| pick(s) < 0.0) && samples.iter().any(|s| pick(s) > 0.0)
        };
        if !spread(|s| s.0) || !spread(|s| s.1) {
            return None;
        }
        let terms = |da: f64, db: f64| [1.0, da, db, da * da, da * db, db * db];
        let mut normal = vec![vec![0.0; 8]; 6];
        for &(da, db, point) in &samples {
            let weight = 1.0 / (1.0 + da * da + db * db);
            let t = terms(da, db);
            for (row, &left) in t.iter().enumerate() {
                for (column, &right) in t.iter().enumerate() {
                    normal[row][column] += weight * left * right;
                }
                normal[row][6] += weight * left * point.0;
                normal[row][7] += weight * left * point.1;
            }
        }
        let solution = solve_two(&mut normal)?;
        Some(solution)
    }

    /// Looks for the corners that were not found although corners around them were, at the
    /// place the surface through those puts them.
    fn fill_gaps(&mut self, bitmap: &Bitmap, step: f64, floor: f64) {
        for _ in 0..GAP_PASSES {
            let mut filled = false;
            for b in 0..self.up {
                for a in 0..self.across {
                    let at = b * self.across + a;
                    if self.found[at].is_some() {
                        continue;
                    }
                    let Some(guess) = self.fitted_corner(a, b) else {
                        continue;
                    };
                    let axes = self.axes();
                    if let Some((point, score)) =
                        refine(bitmap, guess, step * FOLLOW_RADIUS, axes, step)
                        && score >= floor
                    {
                        self.found[at] = Some(point);
                        filled = true;
                    }
                }
            }
            if !filled {
                break;
            }
        }
    }

    /// Measures where the line between two neighbouring corners that were both found passes
    /// through the middle; lines are not straight on bent paper.
    fn measure_edge_middles(&mut self, bitmap: &Bitmap, step: f64, minimum: f64) {
        for b in 0..self.up {
            for a in 0..self.across {
                let at = b * self.across + a;
                let Some(here) = self.found[at] else {
                    continue;
                };
                if a + 1 < self.across
                    && let Some(next) = self.found[at + 1]
                {
                    self.across_middles[at] = edge_middle(bitmap, here, next, step, minimum);
                }
                if b + 1 < self.up
                    && let Some(next) = self.found[at + self.across]
                {
                    self.up_middles[at] = edge_middle(bitmap, here, next, step, minimum);
                }
            }
        }
    }

    /// The four corners next to (`a`, `b`), with the direction of the step as -1 or 1.
    fn next_to(&self, a: usize, b: usize) -> Vec<(usize, usize, isize, isize)> {
        let mut next = Vec::with_capacity(4);
        if a + 1 < self.across {
            next.push((a + 1, b, 1, 0));
        }
        if a > 0 {
            next.push((a - 1, b, -1, 0));
        }
        if b + 1 < self.up {
            next.push((a, b + 1, 0, 1));
        }
        if b > 0 {
            next.push((a, b - 1, 0, -1));
        }
        next
    }

    fn found_at(&self, a: isize, b: isize) -> Option<Point> {
        let (a, b) = (usize::try_from(a).ok()?, usize::try_from(b).ok()?);
        (a < self.across && b < self.up)
            .then(|| self.found[b * self.across + a])
            .flatten()
    }

    /// Where the corner one step from `here` should be, and how its lines run: as the step
    /// before it went if there was one, else as the straight grid.
    fn predict(&self, here: Point, at: (usize, usize), step: (isize, isize)) -> (Point, Axes) {
        let to = (
            (at.0 as isize + step.0) as usize,
            (at.1 as isize + step.1) as usize,
        );
        let straight = {
            let (from, next) = (self.ideal(at.0, at.1), self.ideal(to.0, to.1));
            (next.0 - from.0, next.1 - from.1)
        };
        let back = |times: isize| {
            self.found_at(
                at.0 as isize - step.0 * times,
                at.1 as isize - step.1 * times,
            )
        };
        let vector = match (back(1), back(2)) {
            (Some(previous), Some(earlier)) => {
                let last = (here.0 - previous.0, here.1 - previous.1);
                let last_length = last.0.hypot(last.1).max(f64::EPSILON);
                // Between the last step and what perspective says: a point out of place by a pixel
                // must not make the next one wrong by several.
                let next_length = perspective_next_step(earlier, previous, here)
                    .map_or(last_length, |cross| {
                        self.perspective_trust * cross
                            + (1.0 - self.perspective_trust) * last_length
                    })
                    .clamp(last_length * MIN_STEP_RATIO, last_length * MAX_STEP_RATIO);
                let ratio = next_length / last_length;
                (last.0 * ratio, last.1 * ratio)
            }
            (Some(previous), None) => (here.0 - previous.0, here.1 - previous.1),
            _ => straight,
        };
        let length = vector.0.hypot(vector.1).max(f64::EPSILON);
        let along = (vector.0 / length, vector.1 / length);
        let across = (-along.1, along.0);
        let axes = if step.0 != 0 {
            Axes {
                along_x: along,
                along_y: across,
            }
        } else {
            Axes {
                along_x: across,
                along_y: along,
            }
        };
        ((here.0 + vector.0, here.1 + vector.1), axes)
    }
}

/// How far the next point lies from `here` on a line along which equally spaced points were
/// photographed in perspective, given the last three (`earlier`, `previous`, `here`): lengths
/// along a line in perspective keep their cross-ratio, which for the points -2, -1, 0 and 1
/// is 4/3. `None` if the three points are not in the order a perspective view can give.
fn perspective_next_step(earlier: Point, previous: Point, here: Point) -> Option<f64> {
    let first = (previous.0 - earlier.0).hypot(previous.1 - earlier.1);
    let both = (here.0 - earlier.0).hypot(here.1 - earlier.1);
    let denominator = 4.0 * first - both;
    if first <= f64::EPSILON || both <= first || denominator <= f64::EPSILON {
        return None;
    }
    Some(3.0 * first * both / denominator - both)
}

/// Solves the 6 x 6 system whose augmented matrix has two right-hand sides; the answer is
/// the value at the origin of both, the constant terms. `None` if the system is singular.
fn solve_two(matrix: &mut [Vec<f64>]) -> Option<Point> {
    let n = matrix.len();
    for column in 0..n {
        let pivot = (column..n)
            .max_by(|&p, &q| matrix[p][column].abs().total_cmp(&matrix[q][column].abs()))?;
        if matrix[pivot][column].abs() < 1e-12 {
            return None;
        }
        matrix.swap(column, pivot);
        for row in column + 1..n {
            let factor = matrix[row][column] / matrix[column][column];
            for k in column..n + 2 {
                matrix[row][k] -= factor * matrix[column][k];
            }
        }
    }
    let mut solved = [[0.0; 2]; 6];
    for row in (0..n).rev() {
        for side in 0..2 {
            let known: f64 = (row + 1..n).map(|k| matrix[row][k] * solved[k][side]).sum();
            solved[row][side] = (matrix[row][n + side] - known) / matrix[row][row];
        }
    }
    Some((solved[0][0], solved[0][1]))
}

/// Where the dark line from `from` to `to` passes the middle, searched across the line around
/// the middle of the straight connection; `None` if there is no clear line there.
fn edge_middle(bitmap: &Bitmap, from: Point, to: Point, step: f64, minimum: f64) -> Option<Point> {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let length = dx.hypot(dy);
    if length < 1.0 {
        return None;
    }
    let along = (dx / length, dy / length);
    let side = (-along.1, along.0);
    let middle = (f64::midpoint(from.0, to.0), f64::midpoint(from.1, to.1));
    let arm = (step * ARM_SHARE).max(4.0);
    let reach = (step * FOLLOW_RADIUS).ceil() as isize;
    let scores: Vec<Option<f64>> = (-reach..=reach)
        .map(|d| {
            let on = (middle.0 + side.0 * d as f64, middle.1 + side.1 * d as f64);
            ridge(bitmap, on, along, side, arm)
        })
        .collect();
    let (best, score) = scores
        .iter()
        .enumerate()
        .filter_map(|(i, s)| s.map(|s| (i, s)))
        .max_by(|a, b| a.1.total_cmp(&b.1))?;
    if score < minimum {
        return None;
    }
    let before = best.checked_sub(1).and_then(|i| scores[i]);
    let after = scores.get(best + 1).copied().flatten();
    let fine = match (before, after) {
        (Some(l), Some(u)) if 2.0 * score - l - u > f64::EPSILON => {
            0.5 * (l - u) / (l - 2.0 * score + u)
        }
        _ => 0.0,
    };
    let offset = best as f64 - reach as f64 + fine.clamp(-0.5, 0.5);
    Some((middle.0 + side.0 * offset, middle.1 + side.1 * offset))
}

/// Searches around `center` for the place where a horizontal and a vertical dark line cross.
/// Returns the place to a fraction of a pixel and the contrast of the lines with the paper.
fn refine(
    bitmap: &Bitmap,
    center: Point,
    radius: f64,
    axes: Axes,
    step: f64,
) -> Option<(Point, f64)> {
    let arm = (step * ARM_SHARE).max(4.0);
    let reach = radius.ceil() as isize;
    let mut best: Option<(isize, isize, f64)> = None;
    let mut scores = std::collections::HashMap::new();
    for dy in -reach..=reach {
        for dx in -reach..=reach {
            let point = (center.0 + dx as f64, center.1 + dy as f64);
            let Some(score) = cross_contrast(bitmap, point, axes, arm) else {
                continue;
            };
            scores.insert((dx, dy), score);
            if best.is_none_or(|(_, _, s)| score > s) {
                best = Some((dx, dy, score));
            }
        }
    }
    let (dx, dy, score) = best?;
    // The top of a parabola through the best score and its two neighbours, per axis.
    let top = |lower: Option<&f64>, middle: f64, upper: Option<&f64>| match (lower, upper) {
        (Some(&l), Some(&u)) if 2.0 * middle - l - u > f64::EPSILON => {
            0.5 * (l - u) / (l - 2.0 * middle + u)
        }
        _ => 0.0,
    };
    let fine_x = top(scores.get(&(dx - 1, dy)), score, scores.get(&(dx + 1, dy)));
    let fine_y = top(scores.get(&(dx, dy - 1)), score, scores.get(&(dx, dy + 1)));
    Some((
        (
            center.0 + dx as f64 + fine_x.clamp(-0.5, 0.5),
            center.1 + dy as f64 + fine_y.clamp(-0.5, 0.5),
        ),
        score,
    ))
}

/// How much darker than the paper beside them the lines through `point` are: the smaller
/// of the contrast of the horizontal and of the vertical arm. `None` outside the picture.
fn cross_contrast(bitmap: &Bitmap, point: Point, axes: Axes, arm: f64) -> Option<f64> {
    let horizontal = ridge(bitmap, point, axes.along_x, axes.along_y, arm)?;
    let vertical = ridge(bitmap, point, axes.along_y, axes.along_x, arm)?;
    Some(horizontal.min(vertical))
}

/// The contrast of a dark line through `point` along `direction` (`side` is across it),
/// averaged over `arm` pixels on both sides of the point.
fn ridge(bitmap: &Bitmap, point: Point, direction: Point, side: Point, arm: f64) -> Option<f64> {
    let mut sum = 0.0;
    let mut count = 0.0;
    let mut t = -arm;
    while t <= arm {
        let on = (point.0 + direction.0 * t, point.1 + direction.1 * t);
        let line = sample(bitmap, on)?;
        let left = sample(
            bitmap,
            (on.0 + side.0 * LINE_OFFSET, on.1 + side.1 * LINE_OFFSET),
        )?;
        let right = sample(
            bitmap,
            (on.0 - side.0 * LINE_OFFSET, on.1 - side.1 * LINE_OFFSET),
        )?;
        sum += f64::midpoint(left, right) - line;
        count += 1.0;
        t += 1.0;
    }
    Some(sum / count)
}

/// The brightness at a point between pixels (bilinear); `None` outside the picture.
pub(crate) fn sample(bitmap: &Bitmap, (x, y): Point) -> Option<f64> {
    if x < 0.0 || y < 0.0 {
        return None;
    }
    let (x0, y0) = (x as usize, y as usize);
    if x0 + 1 >= bitmap.width() || y0 + 1 >= bitmap.height() {
        return None;
    }
    let (fx, fy) = (x - x0 as f64, y - y0 as f64);
    let p = |dx: usize, dy: usize| f64::from(bitmap.at(x0 + dx, y0 + dy));
    let top = p(0, 0) + (p(1, 0) - p(0, 0)) * fx;
    let bottom = p(0, 1) + (p(1, 1) - p(0, 1)) * fx;
    Some(top + (bottom - top) * fy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raster::Raster;

    /// A page with a grid of dark lines every `step` pixels, on white.
    fn grid_picture(
        width: usize,
        height: usize,
        step: usize,
        warp: impl Fn(f64, f64) -> Point,
    ) -> Bitmap {
        let mut pixels = vec![255u8; width * height];
        for y in 0..height {
            for x in 0..width {
                // The bitmap the decoder works on has its first row at the bottom.
                let (sx, sy) = warp(x as f64, (height - 1 - y) as f64);
                let on_line = |v: f64| {
                    let m = v.rem_euclid(step as f64);
                    m < 1.5 || m > step as f64 - 0.5
                };
                if on_line(sx) || on_line(sy) {
                    pixels[y * width + x] = 20;
                }
            }
        }
        Bitmap::from_raster(&Raster::from_pixels(width, height, pixels).unwrap())
    }

    fn straight_grid(step: f64, offset: f64) -> Grid {
        Grid {
            x_peak: offset,
            x_step: step,
            x_angle: 0.0,
            y_peak: offset,
            y_step: step,
            y_angle: 0.0,
        }
    }

    #[test]
    fn the_corners_of_a_straight_grid_are_found_where_the_lines_cross() {
        let bitmap = grid_picture(420, 420, 60, |x, y| (x - 0.5, y - 0.5));
        let mesh = Mesh::build(&bitmap, straight_grid(60.0, 60.0), 5, 5, (0.0, 0.0), 0.5).unwrap();
        let quad = mesh.quad(2, 2);
        assert_eq!(quad.corners_found, 4);
        for (found, expected) in [
            (quad.lower_left, (180.0, 180.0)),
            (quad.upper_right, (240.0, 240.0)),
        ] {
            assert!((found.0 - expected.0).abs() < 2.5 && (found.1 - expected.1).abs() < 2.5);
        }
    }

    #[test]
    fn the_mesh_follows_a_grid_that_bends_away_from_the_straight_one() {
        // The lines drift sideways by up to 14 pixels (a quarter of a block) towards the edge.
        let bend = |x: f64, y: f64| {
            let shift = 14.0 * ((x / 420.0) * std::f64::consts::PI).sin() * (y / 420.0);
            (x - shift, y + 6.0 * (x / 420.0))
        };
        let bitmap = grid_picture(420, 420, 60, bend);
        let mesh = Mesh::build(&bitmap, straight_grid(60.0, 60.0), 5, 5, (0.0, 0.0), 0.5).unwrap();
        // A corner far from the middle sits where the bent lines cross, not where the straight grid is.
        let far = mesh.quad(4, 4).upper_right;
        let straight = mesh.ideal(5, 5);
        assert!((far.0 - straight.0).abs() > 3.0 || (far.1 - straight.1).abs() > 3.0);
        let (sx, sy) = bend(far.0, far.1);
        let off = |v: f64| {
            let m = v.rem_euclid(60.0);
            m.min(60.0 - m)
        };
        assert!(off(sx) < 2.0 && off(sy) < 2.0, "{far:?} -> {sx} {sy}");
    }

    #[test]
    fn the_next_point_of_a_perspective_line_follows_from_the_cross_ratio() {
        // Equally spaced points of a line as a camera sees them: s(i) = (a * i + b) / (c * i + 1).
        let seen = |i: f64| (40.0 * i + 300.0) / (0.06 * i + 1.0);
        let point = |i: f64| (seen(i), 0.0);
        let predicted = perspective_next_step(point(-2.0), point(-1.0), point(0.0)).unwrap();
        let real = seen(1.0) - seen(0.0);
        assert!(
            (predicted - real).abs() < 1e-9,
            "{predicted} against {real}"
        );
        // Where a constant step would be off.
        assert!((real - (seen(0.0) - seen(-1.0))).abs() > 0.5);
    }

    #[test]
    fn evenly_spaced_points_continue_evenly() {
        let point = |i: f64| (10.0 * i, 3.0 * i);
        let step = perspective_next_step(point(-2.0), point(-1.0), point(0.0)).unwrap();
        assert!((step - 10.0_f64.hypot(3.0)).abs() < 1e-9);
    }

    #[test]
    fn a_missing_corner_is_found_on_the_surface_through_the_corners_around_it() {
        // A smoothly curved lattice: corner (a, b) at a quadratic position.
        let place = |a: f64, b: f64| (60.0 * a + 0.8 * a * b + 40.0, 55.0 * b + 0.6 * a * a + 30.0);
        let mut mesh = Mesh {
            grid: straight_grid(60.0, 0.0),
            perspective_trust: 0.5,
            across: 7,
            up: 7,
            found: vec![None; 49],
            across_middles: vec![None; 49],
            up_middles: vec![None; 49],
        };
        for b in 0..7 {
            for a in 0..7 {
                if (a, b) != (3, 3) {
                    mesh.found[b * 7 + a] = Some(place(a as f64, b as f64));
                }
            }
        }
        let guess = mesh.fitted_corner(3, 3).unwrap();
        let truth = place(3.0, 3.0);
        assert!(
            (guess.0 - truth.0).abs() < 1e-6 && (guess.1 - truth.1).abs() < 1e-6,
            "{guess:?} {truth:?}"
        );
    }

    #[test]
    fn no_surface_is_fitted_through_a_few_corners() {
        let mut mesh = Mesh {
            grid: straight_grid(60.0, 0.0),
            perspective_trust: 0.5,
            across: 7,
            up: 7,
            found: vec![None; 49],
            across_middles: vec![None; 49],
            up_middles: vec![None; 49],
        };
        mesh.found[2 * 7 + 3] = Some((180.0, 120.0));
        mesh.found[3 * 7 + 2] = Some((120.0, 180.0));
        assert!(mesh.fitted_corner(3, 3).is_none());
    }

    #[test]
    fn a_picture_without_lines_has_no_mesh() {
        let blank = Bitmap::from_raster(&Raster::filled(300, 300, 255));
        assert!(Mesh::build(&blank, straight_grid(60.0, 30.0), 3, 3, (0.0, 0.0), 0.5).is_none());
    }

    #[test]
    fn a_quad_interpolates_between_its_corners() {
        let quad = Quad::new((0.0, 0.0), (10.0, 0.0), (0.0, 20.0), (12.0, 24.0), 4);
        let close = |a: Point, b: Point| (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9;
        assert!(close(quad.at(0.0, 0.0), (0.0, 0.0)));
        assert!(close(quad.at(1.0, 1.0), (12.0, 24.0)));
        assert!(close(quad.at(1.0, 0.0), (10.0, 0.0)));
        assert!(close(quad.at(0.0, 1.0), (0.0, 20.0)));
    }

    #[test]
    fn a_rectangle_is_interpolated_evenly() {
        let quad = Quad::new((0.0, 0.0), (10.0, 0.0), (0.0, 20.0), (10.0, 20.0), 4);
        let (x, y) = quad.at(0.25, 0.75);
        assert!((x - 2.5).abs() < 1e-9 && (y - 15.0).abs() < 1e-9);
    }

    #[test]
    fn the_far_side_of_a_perspective_view_is_compressed() {
        // The upper edge is shorter, as if farther from the camera: the middle of the block
        // lies above the middle of the picture, because the far half shrinks.
        let quad = Quad::new((0.0, 0.0), (100.0, 0.0), (30.0, 100.0), (70.0, 100.0), 4);
        let (_, y) = quad.at(0.5, 0.5);
        assert!(y > 55.0, "{y}");
    }

    #[test]
    fn a_measured_edge_middle_bends_the_edge() {
        let straight = Quad::new((0.0, 0.0), (10.0, 0.0), (0.0, 10.0), (10.0, 10.0), 4);
        let bowed = straight.with_edge_middles([Some((5.0, 1.5)), None, None, None]);
        assert!((bowed.at(0.5, 0.0).1 - 1.5).abs() < 1e-9);
        // The bow fades towards the opposite edge.
        assert!(bowed.at(0.5, 0.5).1 > straight.at(0.5, 0.5).1);
        assert!((bowed.at(0.5, 1.0).1 - 10.0).abs() < 1e-9);
    }
}
