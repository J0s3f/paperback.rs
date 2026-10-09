// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

use std::io::Read;
use std::path::Path;

use crate::cli::PasswordArgs;
use crate::error::{CliError, Result};

/// The password given on the command line side channels, if any. The program
/// never prompts, so it stays usable in pipelines.
pub(crate) fn read(args: &PasswordArgs) -> Result<Option<String>> {
    if let Some(path) = &args.password_file {
        return read_first_line(path).map(Some);
    }
    if let Some(variable) = &args.password_env {
        return std::env::var(variable)
            .map(Some)
            .map_err(|_| CliError::other(format!("environment variable {variable} is not set")));
    }
    Ok(None)
}

fn read_first_line(path: &Path) -> Result<String> {
    let mut text = String::new();
    if path == Path::new("-") {
        std::io::stdin().read_to_string(&mut text)?;
    } else {
        text = std::fs::read_to_string(path)
            .map_err(|e| CliError::other(format!("{}: {e}", path.display())))?;
    }
    Ok(first_line(&text).to_owned())
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_first_line_counts() {
        assert_eq!(first_line("secret\r\nignored"), "secret");
        assert_eq!(first_line(""), "");
    }
}
