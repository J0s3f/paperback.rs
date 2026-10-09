// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

//! `paperback-rs`: write files to paper-ready images and read them back.
//!
//! Data goes to standard output, diagnostics to standard error. Without
//! `--verbose` or `--json` the program is silent on success.

mod cli;
mod decode_command;
mod encode_command;
mod error;
mod output;
mod password;

use std::process::ExitCode;

use clap::Parser;

use cli::{Cli, Command};

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Encode(args) => encode_command::run(&args),
        Command::Decode(args) => decode_command::run(&args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("paperback-rs: {error}");
            ExitCode::from(error.kind.exit_code())
        }
    }
}
