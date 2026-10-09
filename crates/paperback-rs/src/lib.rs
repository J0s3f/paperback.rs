// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

//! Store files on paper as dense, error-corrected bitmaps.
//!
//! Compatible with PaperBack 1.10 by Oleh Yuschuk: pages written here can be
//! read by the original program and the other way round.
//!
//! [`encode::encode`] turns bytes into page pictures; print them or save them
//! with [`imageio`]. [`decode::decode`] reads scanned or rendered pages back,
//! in any order, and rebuilds the file.
//!
//! ```
//! use paperback_rs::decode::{DecodeOptions, decode};
//! use paperback_rs::encode::{EncodeOptions, encode};
//! use paperback_rs::layout::PageSetup;
//!
//! # fn main() -> paperback_rs::Result<()> {
//! let options = EncodeOptions {
//!     setup: PageSetup {
//!         printer_dpi: 300,
//!         dot_dpi: 100,
//!         ..PageSetup::default()
//!     },
//!     name: "hello.txt".into(),
//!     ..EncodeOptions::default()
//! };
//! let pages = encode(b"Hello, paper!", &options)?;
//! let scans: Vec<_> = pages.into_iter().map(|page| page.raster).collect();
//! let restored = decode(&scans, &DecodeOptions::default(), |_| {})?;
//! assert_eq!(restored.data, b"Hello, paper!");
//! assert_eq!(restored.name, "hello.txt");
//! # Ok(())
//! # }
//! ```

pub mod block;
pub mod codec;
mod crc;
pub mod decode;
pub mod encode;
pub mod error;
pub mod imageio;
pub mod layout;
mod pagetext;
mod pbx;
mod pdfimage;
mod plan;
pub mod quality;
pub mod raster;
mod reed_solomon;

pub use error::{Error, Result};
pub use pbx::{Integrity, SheetId};

#[cfg(test)]
mod original_vectors;
