// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider
// Derived from PaperBack 1.10, Copyright (c) 2007 Oleh Yuschuk; see NOTICE.md.

//! The scanned page as the decoder sees it.

use crate::raster::Raster;

/// 8-bit gray bitmap with the first row at the bottom, like a Windows DIB.
/// The decoder's coordinate conventions follow this layout.
pub(crate) struct Bitmap {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}

impl Bitmap {
    pub(crate) fn from_raster(raster: &Raster) -> Self {
        let flipped = raster.flipped_vertically();
        Self {
            width: flipped.width(),
            height: flipped.height(),
            pixels: flipped.into_pixels(),
        }
    }

    /// The part of the bitmap that starts at (`x0`, `y0`) and is `width` by `height` pixels.
    pub(crate) fn cropped(&self, x0: usize, y0: usize, width: usize, height: usize) -> Self {
        let mut pixels = Vec::with_capacity(width * height);
        for y in y0..y0 + height {
            let start = y * self.width + x0;
            pixels.extend_from_slice(&self.pixels[start..start + width]);
        }
        Self {
            width,
            height,
            pixels,
        }
    }

    pub(crate) fn width(&self) -> usize {
        self.width
    }

    pub(crate) fn height(&self) -> usize {
        self.height
    }

    /// Pixel at column `x` of memory row `y`; callers keep within bounds.
    pub(crate) fn at(&self, x: usize, y: usize) -> u8 {
        self.pixels[y * self.width + x]
    }
}
