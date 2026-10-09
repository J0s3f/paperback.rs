// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider
// Derived from PaperBack 1.10, Copyright (c) 2007 Oleh Yuschuk; see NOTICE.md.

//! CRC-16/XMODEM as used by PaperBack (polynomial 0x1021, initial value 0).

use crc::{CRC_16_XMODEM, Crc};

const CRC16: Crc<u16> = Crc::<u16>::new(&CRC_16_XMODEM);

pub(crate) fn crc16(bytes: &[u8]) -> u16 {
    CRC16.checksum(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_ccitt_check_value() {
        assert_eq!(crc16(b"123456789"), 0x31C3);
    }

    #[test]
    fn empty_input_is_zero() {
        assert_eq!(crc16(&[]), 0);
    }
}
