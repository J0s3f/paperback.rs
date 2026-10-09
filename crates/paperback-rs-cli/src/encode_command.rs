// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

use std::io::Read;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use paperback_rs::block::{FileTime, Redundancy};
use paperback_rs::codec::Compression;
use paperback_rs::encode::{EncodeOptions, encode};
use paperback_rs::layout::{Margins, PageSetup, Paper};

use crate::cli::{CompressionLevel, EncodeArgs, PaperSize};
use crate::error::{CliError, Result};
use crate::output::Destination;
use crate::password;

const MILS_PER_MM: f64 = 1000.0 / 25.4;
const STDIN_NAME: &str = "stdin";

pub(crate) fn run(args: &EncodeArgs) -> Result<()> {
    let destination = Destination::resolve(&args.output, args.format)?;
    let (data, source_name, modified) = read_input(args.input.as_deref())?;
    let options = EncodeOptions {
        setup: page_setup(args)?,
        redundancy: Redundancy::new(args.redundancy)
            .ok_or_else(|| CliError::other("--redundancy must be between 2 and 10"))?,
        compression: compression(args.compression),
        password: password::read(&args.password)?,
        name: args.name.clone().unwrap_or(source_name),
        modified,
        extensions: !args.no_extensions,
        ..EncodeOptions::default()
    };
    let pages = encode(&data, &options)?;
    destination.write(&pages)
}

fn read_input(path: Option<&Path>) -> Result<(Vec<u8>, String, FileTime)> {
    match path.filter(|p| *p != Path::new("-")) {
        None => {
            let mut data = Vec::new();
            std::io::stdin().lock().read_to_end(&mut data)?;
            Ok((data, STDIN_NAME.to_owned(), unix_seconds_now()))
        }
        Some(path) => {
            let describe = |e: std::io::Error| CliError::other(format!("{}: {e}", path.display()));
            let data = std::fs::read(path).map_err(describe)?;
            let modified = std::fs::metadata(path)
                .and_then(|m| m.modified())
                .map_or_else(|_| unix_seconds_now(), to_file_time);
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            Ok((data, name, modified))
        }
    }
}

fn to_file_time(time: SystemTime) -> FileTime {
    let seconds = time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    FileTime::from_unix_seconds(seconds)
}

fn unix_seconds_now() -> FileTime {
    to_file_time(SystemTime::now())
}

fn page_setup(args: &EncodeArgs) -> Result<PageSetup> {
    let mut setup = PageSetup {
        paper: match args.paper {
            PaperSize::A4 => Paper::A4,
            PaperSize::Letter => Paper::Letter,
        },
        printer_dpi: args.printer_dpi,
        dot_dpi: args.dot_dpi,
        dot_percent: args.dot_size,
        frame: args.frame,
        text: !args.no_text,
        ..PageSetup::default()
    };
    if let Some(mm) = &args.margins_mm {
        if mm.iter().any(|m| !m.is_finite() || *m < 0.0) {
            return Err(CliError::other("margins must be non-negative"));
        }
        let mils = |value: f64| (value * MILS_PER_MM).round() as usize;
        setup.margins = Margins {
            left: mils(mm[0]),
            right: mils(mm[1]),
            top: mils(mm[2]),
            bottom: mils(mm[3]),
        };
    }
    Ok(setup)
}

fn compression(level: CompressionLevel) -> Compression {
    match level {
        CompressionLevel::None => Compression::None,
        CompressionLevel::Fast => Compression::Fast,
        CompressionLevel::Max => Compression::Maximal,
    }
}
