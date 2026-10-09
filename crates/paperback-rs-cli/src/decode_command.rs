// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use paperback_rs::block::FileTime;
use paperback_rs::decode::{DecodeOptions, PageOutcome, RestoredFile, decode};
use paperback_rs::imageio;
use paperback_rs::raster::Raster;
use serde_json::json;

use crate::cli::DecodeArgs;
use crate::error::{CliError, Result};
use crate::output::page_path;
use crate::password;

pub(crate) fn run(args: &DecodeArgs) -> Result<()> {
    let pages = read_pages(&args.inputs)?;
    let reports = QualityReports::new(args, pages.len())?;
    let options = DecodeOptions {
        password: password::read(&args.password)?,
        quality: reports.wanted(),
        diagnose: args.verbose || args.json,
    };
    let restored = decode(&pages, &options, |outcome| {
        if args.verbose {
            report_page(&outcome);
        }
        reports.write(&outcome);
    });
    // The pictures are most useful when decoding failed, so they are written either way.
    reports.finish()?;
    let restored = restored?;
    write_output(args.output.as_deref(), &restored.data)?;
    if args.json {
        eprintln!("{}", summary(&restored));
    }
    Ok(())
}

/// Writes the quality pictures requested on the command line as pages are read.
struct QualityReports<'a> {
    map: Option<&'a str>,
    overlay: Option<&'a str>,
    pages: usize,
    failure: std::cell::RefCell<Option<CliError>>,
}

impl<'a> QualityReports<'a> {
    fn new(args: &'a DecodeArgs, pages: usize) -> Result<Self> {
        let reports = Self {
            map: args.quality_map.as_deref(),
            overlay: args.quality_overlay.as_deref(),
            pages,
            failure: std::cell::RefCell::new(None),
        };
        // Fail before the slow work if a name cannot hold several pages.
        for template in [reports.map, reports.overlay].into_iter().flatten() {
            page_path(template, 1, pages)?;
        }
        Ok(reports)
    }

    fn wanted(&self) -> bool {
        self.map.is_some() || self.overlay.is_some()
    }

    fn write(&self, outcome: &PageOutcome) {
        let Some(quality) = &outcome.quality else {
            return;
        };
        let page = outcome.index + 1;
        let pictures = [(self.map, quality.map()), (self.overlay, quality.overlay())];
        for (template, picture) in pictures {
            let Some(template) = template else { continue };
            let written = page_path(template, page, self.pages).and_then(|path| {
                let png = imageio::write_color_png(&picture)?;
                std::fs::write(&path, png).map_err(|e| CliError::other(format!("{path}: {e}")))
            });
            if let Err(error) = written {
                self.failure.borrow_mut().get_or_insert(error);
            }
        }
    }

    fn finish(&self) -> Result<()> {
        self.failure.take().map_or(Ok(()), Err)
    }
}
fn read_pages(inputs: &[PathBuf]) -> Result<Vec<Raster>> {
    let stdin_only = [PathBuf::from("-")];
    let inputs = if inputs.is_empty() {
        &stdin_only[..]
    } else {
        inputs
    };
    let mut pages = Vec::new();
    for input in inputs {
        let bytes = read_bytes(input)?;
        let found = imageio::read_pages(&bytes)
            .map_err(CliError::from)
            .map_err(|e| CliError::new(e.kind, format!("{}: {e}", input.display())))?;
        pages.extend(found);
    }
    Ok(pages)
}

fn read_bytes(path: &Path) -> Result<Vec<u8>> {
    if path == Path::new("-") {
        let mut bytes = Vec::new();
        std::io::stdin().lock().read_to_end(&mut bytes)?;
        Ok(bytes)
    } else {
        std::fs::read(path).map_err(|e| CliError::other(format!("{}: {e}", path.display())))
    }
}

fn write_output(path: Option<&Path>, data: &[u8]) -> Result<()> {
    if let Some(path) = path {
        return std::fs::write(path, data)
            .map_err(|e| CliError::other(format!("{}: {e}", path.display())));
    }
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(data)?;
    stdout.flush()?;
    Ok(())
}

fn report_page(outcome: &PageOutcome) {
    let number = outcome.index + 1;
    match &outcome.result {
        Ok(stats) => {
            eprintln!(
                "page {number}: {} blocks read, {} unreadable, {} bytes corrected",
                stats.good_blocks + stats.superblocks,
                stats.bad_blocks,
                stats.restored_bytes
            );
            for advice in stats.hints.advice() {
                eprintln!("page {number}: hint: {advice}");
            }
        }
        Err(reason) => eprintln!("page {number}: skipped, {reason}"),
    }
}

fn summary(restored: &RestoredFile) -> String {
    let report = &restored.report;
    json!({
        "name": restored.name,
        "size": restored.data.len(),
        "modified_unix": restored.modified.and_then(FileTime::to_unix_seconds),
        "attributes": restored.attributes,
        "pages_read": report.pages_read,
        "good_blocks": report.good_blocks,
        "bad_blocks": report.bad_blocks,
        "corrected_bytes": report.restored_bytes,
        "recovered_blocks": report.recovered_blocks,
        "hints": report.hints.names(),
    })
    .to_string()
}
