// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

//! An 8-bit grayscale page, rows stored top to bottom.

#[derive(Clone, Debug, PartialEq, Eq)]
/// An 8-bit grayscale picture, rows stored top to bottom.
pub struct Raster {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}

/// Gray level of blank paper.
pub const WHITE: u8 = 255;

/// Largest picture accepted, in pixels (about 540 megapixels). Hostile or corrupt
/// files announce absurd sizes; this keeps them from exhausting memory.
pub const MAX_PIXELS: usize = 1 << 29;

/// Whether a picture of this size is allowed.
pub fn size_is_allowed(width: usize, height: usize) -> bool {
    width > 0
        && height > 0
        && width
            .checked_mul(height)
            .is_some_and(|pixels| pixels <= MAX_PIXELS)
}

impl Raster {
    /// A picture of a single gray level.
    pub fn filled(width: usize, height: usize, value: u8) -> Self {
        Self {
            width,
            height,
            pixels: vec![value; width * height],
        }
    }

    /// Wraps row-major pixels; `None` unless there are exactly `width * height` of them.
    pub fn from_pixels(width: usize, height: usize, pixels: Vec<u8>) -> Option<Self> {
        (width.checked_mul(height) == Some(pixels.len())).then_some(Self {
            width,
            height,
            pixels,
        })
    }

    /// Width in pixels.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Height in pixels.
    pub fn height(&self) -> usize {
        self.height
    }

    /// All pixels, row by row from the top.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Takes the pixels out of the picture.
    pub fn into_pixels(self) -> Vec<u8> {
        self.pixels
    }

    /// The pixels of row `y`, where row 0 is the top.
    pub fn row(&self, y: usize) -> &[u8] {
        &self.pixels[y * self.width..(y + 1) * self.width]
    }

    /// Fills the rectangle, clipped to the raster.
    pub fn fill_rect(&mut self, x: usize, y: usize, width: usize, height: usize, value: u8) {
        let x_end = (x + width).min(self.width);
        for row in y..(y + height).min(self.height) {
            self.pixels[row * self.width + x.min(x_end)..row * self.width + x_end].fill(value);
        }
    }

    /// Copies other into this raster with its top-left corner at (x, y), clipped.
    pub fn paste(&mut self, other: &Raster, x: usize, y: usize) {
        for row in 0..other.height.min(self.height.saturating_sub(y)) {
            let columns = other.width.min(self.width.saturating_sub(x));
            let target = (y + row) * self.width + x;
            self.pixels[target..target + columns].copy_from_slice(&other.row(row)[..columns]);
        }
    }

    /// Turns the picture counter-clockwise by `degrees` on a canvas large
    /// enough to hold all of it; uncovered areas become `fill`. `None` if the
    /// turned picture would be too large.
    #[allow(clippy::similar_names, reason = "x and y pairs of the same quantity")]
    pub fn rotated(&self, degrees: f64, fill: u8) -> Option<Self> {
        let (sin, cos) = degrees.to_radians().sin_cos();
        let (w, h) = (self.width as f64, self.height as f64);
        // The epsilon keeps exact quarter turns from growing by a pixel through rounding error.
        let new_width = (w * cos.abs() + h * sin.abs() - 1e-9).ceil() as usize;
        let new_height = (w * sin.abs() + h * cos.abs() - 1e-9).ceil() as usize;
        let (dest_cx, dest_cy) = (new_width as f64 / 2.0, new_height as f64 / 2.0);
        let (src_cx, src_cy) = (w / 2.0, h / 2.0);
        if !size_is_allowed(new_width, new_height) {
            return None;
        }
        let mut pixels = Vec::with_capacity(new_width * new_height);
        for y in 0..new_height {
            let dy = y as f64 + 0.5 - dest_cy;
            for x in 0..new_width {
                let dx = x as f64 + 0.5 - dest_cx;
                let source_x = dx * cos - dy * sin + src_cx - 0.5;
                let source_y = dx * sin + dy * cos + src_cy - 0.5;
                pixels.push(self.sample(source_x, source_y).unwrap_or(fill));
            }
        }
        Some(Self {
            width: new_width,
            height: new_height,
            pixels,
        })
    }

    /// Bilinear interpolation; `None` outside the picture.
    fn sample(&self, x: f64, y: f64) -> Option<u8> {
        if x < 0.0 || y < 0.0 || x > (self.width - 1) as f64 || y > (self.height - 1) as f64 {
            return None;
        }
        let (x0, y0) = (x as usize, y as usize);
        let (x1, y1) = ((x0 + 1).min(self.width - 1), (y0 + 1).min(self.height - 1));
        let (fx, fy) = (x - x0 as f64, y - y0 as f64);
        let at = |px: usize, py: usize| f64::from(self.pixels[py * self.width + px]);
        let top = at(x0, y0) * (1.0 - fx) + at(x1, y0) * fx;
        let bottom = at(x0, y1) * (1.0 - fx) + at(x1, y1) * fx;
        Some((top * (1.0 - fy) + bottom * fy).round() as u8)
    }

    /// The picture with every `factor` x `factor` square of pixels averaged into
    /// one. `None` for a factor of 0 or a picture smaller than one square.
    #[must_use]
    pub(crate) fn reduced(&self, factor: usize) -> Option<Self> {
        let (width, height) = (self.width / factor.max(1), self.height / factor.max(1));
        if factor == 0 || width == 0 || height == 0 {
            return None;
        }
        let area = factor * factor;
        let mut pixels = Vec::with_capacity(width * height);
        for y in 0..height {
            for x in 0..width {
                let sum: usize = (y * factor..(y + 1) * factor)
                    .map(|row| {
                        self.row(row)[x * factor..(x + 1) * factor]
                            .iter()
                            .map(|&v| usize::from(v))
                            .sum::<usize>()
                    })
                    .sum();
                pixels.push((sum / area) as u8);
            }
        }
        Self::from_pixels(width, height, pixels)
    }

    /// The picture upside down.
    #[must_use]
    pub fn flipped_vertically(&self) -> Self {
        let rows: Vec<u8> = (0..self.height)
            .rev()
            .flat_map(|y| self.row(y).iter().copied())
            .collect();
        Self {
            width: self.width,
            height: self.height,
            pixels: rows,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_rect_is_clipped() {
        let mut raster = Raster::filled(4, 3, WHITE);
        raster.fill_rect(3, 2, 5, 5, 0);
        assert_eq!(raster.row(2), &[255, 255, 255, 0]);
        assert_eq!(raster.row(1), &[255; 4]);
    }

    #[test]
    fn rotation_by_zero_keeps_the_picture() {
        let raster = Raster::from_pixels(3, 2, vec![10, 20, 30, 40, 50, 60]).unwrap();
        assert_eq!(raster.rotated(0.0, 0).unwrap(), raster);
    }

    #[test]
    fn quarter_turn_swaps_the_dimensions() {
        let raster = Raster::filled(40, 10, 7);
        let turned = raster.rotated(90.0, 0).unwrap();
        assert_eq!((turned.width(), turned.height()), (10, 40));
        assert_eq!(turned.row(20)[5], 7);
    }

    #[test]
    fn flip_reverses_rows() {
        let raster = Raster::from_pixels(2, 2, vec![1, 2, 3, 4]).unwrap();
        assert_eq!(raster.flipped_vertically().pixels(), &[3, 4, 1, 2]);
    }
}
