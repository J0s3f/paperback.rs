// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

use std::fmt;

/// Exit status classes; usage errors exit with 2 as clap does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Other,
    BadInput,
    Password,
}

impl Kind {
    pub(crate) fn exit_code(self) -> u8 {
        match self {
            Self::Other => 1,
            Self::BadInput => 3,
            Self::Password => 4,
        }
    }
}

#[derive(Debug)]
pub(crate) struct CliError {
    pub(crate) kind: Kind,
    message: String,
}

impl CliError {
    pub(crate) fn new(kind: Kind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub(crate) fn other(message: impl Into<String>) -> Self {
        Self::new(Kind::Other, message)
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<std::io::Error> for CliError {
    fn from(error: std::io::Error) -> Self {
        Self::other(error.to_string())
    }
}

impl From<paperback_rs::Error> for CliError {
    fn from(error: paperback_rs::Error) -> Self {
        use paperback_rs::Error as E;
        let kind = match error {
            E::PasswordRequired | E::WrongPassword => Kind::Password,
            E::Image(_)
            | E::Pdf(_)
            | E::Decode(_)
            | E::NoReadablePage
            | E::Incomplete { .. }
            | E::HashMismatch
            | E::Decompress(_) => Kind::BadInput,
            _ => Kind::Other,
        };
        Self::new(kind, error.to_string())
    }
}

pub(crate) type Result<T> = std::result::Result<T, CliError>;
