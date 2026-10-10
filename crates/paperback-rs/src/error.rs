// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

//! The error type shared by all operations.

use std::io;

/// Result with the crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
/// Everything that can go wrong.
#[non_exhaustive]
pub enum Error {
    #[error("the input is empty")]
    /// There is nothing to store.
    EmptyInput,
    #[error("the input is {0} bytes; at most {max} bytes fit", max = crate::block::MAX_FILE_SIZE)]
    /// The input is larger than the format can address.
    InputTooLarge(usize),
    #[error("printable area is too small, reduce margins or dot size or raise the resolution")]
    /// Not enough room on the page for the chosen settings.
    PageTooSmall,
    #[error("invalid setting: {0}")]
    /// A setting is out of range or meaningless.
    InvalidSetting(String),
    #[error("compression failed: {0}")]
    /// Compressing failed.
    Compress(io::Error),
    #[error("unable to unpack data: {0}")]
    /// The data cannot be unpacked.
    Decompress(io::Error),
    #[error("encryption failed")]
    /// Encryption or decryption failed.
    Cipher,
    #[error("no source of randomness: {0}")]
    /// No source of random numbers.
    Random(String),
    #[error("password is required but was not given")]
    /// The backup is encrypted and no password was given.
    PasswordRequired,
    #[error("wrong password")]
    /// The password does not match the backup.
    WrongPassword,
    #[error("the restored file does not match the SHA-256 stored on the pages")]
    /// The pages carry a hash of the file and the restored file differs from it.
    HashMismatch,
    #[error("the pages do not carry a hash that the restored file matches, which was required")]
    /// A hash was required and the pages show none that was checked.
    NotVerified,
    #[error("no page carries a readable label")]
    /// No page carries a readable label.
    NoReadablePage,
    #[error("unable to read image: {0}")]
    /// A picture could not be read.
    Image(String),
    #[error("unable to read PDF: {0}")]
    /// A PDF could not be read.
    Pdf(String),
    #[error("{0}")]
    /// The pages are inconsistent or damaged beyond repair.
    Decode(String),
    #[error(
        "data is incomplete: {recovered} of {total} blocks recovered; pages to scan again: {pages}"
    )]
    /// Blocks are missing, so the file cannot be rebuilt yet.
    Incomplete {
        /// Blocks that could be recovered.
        recovered: usize,
        /// Blocks the file consists of.
        total: usize,
        /// Pages that still lack blocks (1-based, comma separated).
        pages: String,
    },
    #[error(transparent)]
    /// Reading or writing a file failed.
    Io(#[from] io::Error),
}
