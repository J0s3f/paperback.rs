// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

/// Back up files on paper as dense, error-corrected bitmaps (PaperBack 1.10 compatible).
///
/// `encode` turns a file into page images (PNG, BMP) or one PDF; `decode`
/// reads scanned or rendered pages (PNG, JPEG, BMP, PDF) back into the file.
/// Use `-` or omit the input to read standard input.
///
/// Exit status: 0 success, 1 failure, 2 usage error, 3 unreadable or incomplete
/// input, 4 missing or wrong password.
#[derive(Parser, Debug)]
#[command(name = "paperback-rs", version, about, long_about)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Subcommand, Debug)]
pub(crate) enum Command {
    /// Write a file as printable pages.
    Encode(EncodeArgs),
    /// Restore a file from page images.
    Decode(DecodeArgs),
}

#[derive(Args, Debug)]
pub(crate) struct PasswordArgs {
    /// Read the password from the first line of this file ('-' for standard input).
    #[arg(long, value_name = "FILE", conflicts_with = "password_env")]
    pub(crate) password_file: Option<PathBuf>,
    /// Read the password from this environment variable.
    #[arg(long, value_name = "VAR")]
    pub(crate) password_env: Option<String>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum PaperSize {
    A4,
    Letter,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum CompressionLevel {
    None,
    Fast,
    Max,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum Format {
    Png,
    Bmp,
    Pdf,
}

// One bool per command line switch.
#[allow(clippy::struct_excessive_bools)]
#[derive(Args, Debug)]
pub(crate) struct EncodeArgs {
    /// File to back up; '-' or omitted reads standard input.
    pub(crate) input: Option<PathBuf>,

    /// Output: a PDF file, or a PNG/BMP name where `%d` or `%03d` stands for the page number.
    /// A single page needs no number. '-' writes to standard output (needs --format).
    #[arg(short, long, value_name = "PATH")]
    pub(crate) output: String,

    /// Output format when it cannot be told from the file name.
    #[arg(long, value_enum)]
    pub(crate) format: Option<Format>,

    /// Name stored on the pages (default: the input file name).
    #[arg(long)]
    pub(crate) name: Option<String>,

    #[arg(long, value_enum, default_value = "a4")]
    pub(crate) paper: PaperSize,

    /// Printer resolution the pages are meant for, dots per inch.
    #[arg(long, default_value_t = 600, value_name = "DPI")]
    pub(crate) printer_dpi: usize,

    /// Density of the data dots, dots per inch (at most half the printer resolution).
    #[arg(long, default_value_t = 200, value_name = "DPI")]
    pub(crate) dot_dpi: usize,

    /// Dot size in percent of the dot pitch.
    #[arg(long, default_value_t = 70, value_name = "PERCENT")]
    pub(crate) dot_size: usize,

    /// PaperBack's redundancy: one recovery block per this many data blocks (2 to 10), which
    /// rebuilds one lost block per group, also in the original programs. 0 leaves them out.
    /// [default: 10, with --legacy 5]
    #[arg(long, value_name = "N", conflicts_with = "modern")]
    pub(crate) redundancy: Option<u8>,

    /// Parity blocks per 100 data blocks, as many as fit the code (at least 300; more for smaller
    /// files). Lost blocks up to their number are rebuilt, on whichever pages they were; above
    /// 16384 data blocks the file is cut into interleaved stripes, each rebuilt only from its own
    /// share of the parity blocks. The original programs ignore them.
    /// [default: 10, with --modern 20]
    #[arg(long, value_name = "PERCENT")]
    pub(crate) parity: Option<u32>,

    /// Use the whole paper of every page: the free cells hold more parity blocks. The number of
    /// pages stays the same. On with --modern; `--fill-pages false` turns it off again.
    /// [default: false, with --modern true]
    #[arg(
        long,
        value_name = "BOOL",
        num_args = 0..=1,
        default_missing_value = "true",
        value_parser = clap::builder::BoolishValueParser::new()
    )]
    pub(crate) fill_pages: Option<bool>,

    /// Pages as PaperBack 1.10 writes them: its recovery blocks (one per 5 data blocks unless
    /// --redundancy says otherwise) and nothing else, no parity blocks and no records.
    #[arg(long, conflicts_with_all = ["modern", "balanced", "parity", "fill_pages"])]
    pub(crate) legacy: bool,

    /// The default: recovery blocks that the original programs use too (one per 10 data blocks),
    /// 10 parity blocks per 100 data blocks and the records with the hash of the file.
    #[arg(long, conflicts_with_all = ["modern", "legacy"])]
    pub(crate) balanced: bool,

    /// Pages for paperback.rs: no recovery blocks of PaperBack but as many more parity blocks,
    /// filled pages, and the hash of the file (or keyed check value) on every page, required.
    /// The original programs still read the pages but cannot rebuild lost blocks.
    #[arg(long, conflicts_with = "no_extensions")]
    pub(crate) modern: bool,

    #[arg(long, value_enum, default_value = "max")]
    pub(crate) compression: CompressionLevel,

    /// Surround the data with an alignment frame.
    #[arg(long)]
    pub(crate) frame: bool,

    /// Leave out the title above and the scanning hint below the data.
    #[arg(long)]
    pub(crate) no_text: bool,

    /// Leave out the SHA-256 of the file and the sheet identifier that paperback.rs adds to
    /// its pages. PaperBack 1.00 and 1.10 read the pages either way.
    #[arg(long)]
    pub(crate) no_extensions: bool,

    /// Left, right, top and bottom page margins in millimetres.
    #[arg(long, num_args = 4, value_names = ["LEFT", "RIGHT", "TOP", "BOTTOM"])]
    pub(crate) margins_mm: Option<Vec<f64>>,

    #[command(flatten)]
    pub(crate) password: PasswordArgs,
}

// One bool per command line switch.
#[allow(clippy::struct_excessive_bools)]
#[derive(Args, Debug)]
pub(crate) struct DecodeArgs {
    /// Page images or PDFs; '-' or none reads one image from standard input.
    pub(crate) inputs: Vec<PathBuf>,

    /// Write the restored file here (default: standard output).
    #[arg(short, long, value_name = "FILE")]
    pub(crate) output: Option<PathBuf>,

    /// Print a summary of the restoration as JSON on standard error.
    #[arg(long)]
    pub(crate) json: bool,

    /// Report every page on standard error.
    #[arg(short, long)]
    pub(crate) verbose: bool,

    /// Write a PNG per page showing how well each block read, green (clean) over orange
    /// (much error correction) to red (unreadable). Use %d or %03d in the name for the page number.
    #[arg(long, value_name = "PNG")]
    pub(crate) quality_map: Option<String>,

    /// Like --quality-map, but laid over the page as it was read (straightened if it was tilted).
    #[arg(long, value_name = "PNG")]
    pub(crate) quality_overlay: Option<String>,

    /// Threads for reading a block that does not read at once: 0 is as many as the processor has,
    /// 1 is one after the other; more than 256 count as 256. The same blocks are read either way.
    #[arg(long, default_value_t = 0, value_name = "N")]
    pub(crate) threads: usize,

    /// Fail unless the pages carry a hash (or, for encrypted files, a keyed check value) that the
    /// restored file matches. Pages from PaperBack 1.00 and 1.10 and from `--no-extensions` have
    /// none. Without this, someone who can change the pages can also take the hash off them.
    #[arg(long)]
    pub(crate) require_hash: bool,

    /// For pages written with `encode --modern`: the same as --require-hash.
    #[arg(long)]
    pub(crate) modern: bool,

    #[command(flatten)]
    pub(crate) password: PasswordArgs,
}
