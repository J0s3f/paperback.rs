// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

//! Where encoded pages go: files, numbered files or standard output.

use std::io::Write;
use std::path::PathBuf;

use paperback_rs::encode::Page;
use paperback_rs::imageio::{self, OutputFormat};

use crate::cli::Format;
use crate::error::{CliError, Result};

const STDOUT: &str = "-";

pub(crate) enum Destination {
    Stdout(OutputFormat),
    File {
        path: PathBuf,
        format: OutputFormat,
    },
    Numbered {
        numbering: PageNumbering,
        format: OutputFormat,
    },
}

impl Destination {
    pub(crate) fn resolve(output: &str, format: Option<Format>) -> Result<Self> {
        let explicit = format.map(output_format);
        if output == STDOUT {
            let format = explicit
                .ok_or_else(|| CliError::other("writing to standard output needs --format"))?;
            return Ok(Self::Stdout(format));
        }
        let format = explicit
            .or_else(|| OutputFormat::from_extension(output))
            .ok_or_else(|| {
                CliError::other(
                    "cannot tell the format from the output name; use --format or .png, .bmp, .pdf",
                )
            })?;
        if let (true, Some(numbering)) = (format != OutputFormat::Pdf, PageNumbering::parse(output))
        {
            return Ok(Self::Numbered { numbering, format });
        }
        Ok(Self::File {
            path: PathBuf::from(output),
            format,
        })
    }

    pub(crate) fn write(&self, pages: &[Page]) -> Result<()> {
        match self {
            Self::Stdout(format) => {
                let bytes = single_document(*format, pages)?;
                let mut stdout = std::io::stdout().lock();
                stdout.write_all(&bytes)?;
                stdout.flush()?;
                Ok(())
            }
            Self::File { path, format } => {
                let bytes = single_document(*format, pages)?;
                std::fs::write(path, bytes)
                    .map_err(|e| CliError::other(format!("{}: {e}", path.display())))
            }
            Self::Numbered { numbering, format } => {
                for (index, page) in pages.iter().enumerate() {
                    let path = numbering.name(index + 1);
                    let bytes = encode_page(*format, page)?;
                    std::fs::write(&path, bytes)
                        .map_err(|e| CliError::other(format!("{path}: {e}")))?;
                }
                Ok(())
            }
        }
    }
}

fn output_format(format: Format) -> OutputFormat {
    match format {
        Format::Png => OutputFormat::Png,
        Format::Bmp => OutputFormat::Bmp,
        Format::Pdf => OutputFormat::Pdf,
    }
}

/// One document holding all pages: a PDF, or one image if there is a single page.
fn single_document(format: OutputFormat, pages: &[Page]) -> Result<Vec<u8>> {
    match (format, pages) {
        (OutputFormat::Pdf, _) => Ok(imageio::write_pdf(&printable(pages))),
        (_, [page]) => encode_page(format, page),
        (_, _) => Err(CliError::other(format!(
            "the data needs {} pages; name the output like page-%03d.png or use a .pdf",
            pages.len()
        ))),
    }
}

/// PDFs are made for printing, so their pages are pure black and white (see
/// [`Page::black_and_white`]); PNG and BMP keep the gray levels of the original program.
fn printable(pages: &[Page]) -> Vec<Page> {
    pages.iter().map(Page::black_and_white).collect()
}

fn encode_page(format: OutputFormat, page: &Page) -> Result<Vec<u8>> {
    match format {
        OutputFormat::Png => Ok(imageio::write_png(page)?),
        OutputFormat::Bmp => Ok(imageio::write_bmp(page)),
        OutputFormat::Pdf => Ok(imageio::write_pdf(&printable(std::slice::from_ref(page)))),
    }
}

/// A printf-style page number field in an output name: `%d`, `%3d` or `%03d`.
pub(crate) struct PageNumbering {
    prefix: String,
    suffix: String,
    width: usize,
    zero_padded: bool,
}

impl PageNumbering {
    fn parse(template: &str) -> Option<Self> {
        let (prefix, rest) = template.split_once('%')?;
        let digits_end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        let (digits, after) = rest.split_at(digits_end);
        let suffix = after.strip_prefix('d')?;
        Some(Self {
            prefix: prefix.to_owned(),
            suffix: suffix.to_owned(),
            width: digits.trim_start_matches('0').parse().unwrap_or(0),
            zero_padded: digits.starts_with('0'),
        })
    }

    fn name(&self, number: usize) -> String {
        let width = self.width;
        let digits = if self.zero_padded {
            format!("{number:0width$}")
        } else {
            format!("{number:width$}")
        };
        format!("{}{digits}{}", self.prefix, self.suffix)
    }
}
/// The file name for page `page` (1-based) of `pages` from a name that may
/// contain a page number field; a name without one needs a single page.
pub(crate) fn page_path(template: &str, page: usize, pages: usize) -> Result<String> {
    match PageNumbering::parse(template) {
        Some(numbering) => Ok(numbering.name(page)),
        None if pages == 1 => Ok(template.to_owned()),
        None => Err(CliError::other(format!(
            "{pages} pages need a numbered name such as page-%03d.png, not {template}"
        ))),
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn name(template: &str, number: usize) -> Option<String> {
        PageNumbering::parse(template).map(|numbering| numbering.name(number))
    }

    #[test]
    fn expands_zero_padded_numbers() {
        assert_eq!(name("out-%03d.png", 7).as_deref(), Some("out-007.png"));
    }

    #[test]
    fn expands_plain_numbers() {
        assert_eq!(name("p%d.bmp", 12).as_deref(), Some("p12.bmp"));
    }

    #[test]
    fn templates_without_placeholder_are_not_numbered() {
        assert!(name("out.png", 1).is_none());
        assert!(name("100%.png", 1).is_none());
    }

    #[test]
    fn pdf_names_are_never_numbered() {
        let destination = Destination::resolve("out-%d.pdf", None).unwrap();
        assert!(matches!(
            destination,
            Destination::File {
                format: OutputFormat::Pdf,
                ..
            }
        ));
    }

    #[test]
    fn stdout_needs_an_explicit_format() {
        assert!(Destination::resolve("-", None).is_err());
        assert!(Destination::resolve("-", Some(Format::Pdf)).is_ok());
    }
}
