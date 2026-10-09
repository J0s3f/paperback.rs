// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider
// Derived from PaperBack 1.10, Copyright (c) 2007 Oleh Yuschuk; see NOTICE.md.

//! The 128-byte units drawn on paper: data blocks and the superblock.

use crate::crc::crc16;
use crate::reed_solomon;

/// Dots per block side.
pub(crate) const BLOCK_DOTS: usize = 32;
/// Useful bytes in a data block.
pub(crate) const DATA_LEN: usize = 90;
/// Size of a block on paper: address, data, CRC and error correction code.
pub(crate) const BLOCK_LEN: usize = 128;
/// Marks the superblock, which describes the file.
pub(crate) const SUPERBLOCK_ADDRESS: u32 = 0xFFFF_FFFF;
/// Largest file the 28-bit block addresses can reach.
pub(crate) const MAX_FILE_SIZE: u32 = 0x0FFF_FF80;

const CRC_OFFSET: usize = 94;
const CRC_COVERED: usize = 4 + DATA_LEN;
const CRC_MASK: u16 = 0x55AA;
/// The block is shortened to 96 message bytes; 127 of the 223 are implied zeros.
const ECC_PAD: usize = 127;
const ECC_OFFSET: usize = 96;
const GROUP_SHIFT: u32 = 28;
const ADDRESS_MASK: u32 = 0x0FFF_FFFF;

pub(crate) const NAME_LEN: usize = 64;
/// Part of the name field that carries text; the rest holds salt and IV.
pub(crate) const NAME_TEXT_LEN: usize = 32;
const NAME_OFFSET: usize = 30;

/// Redundancy group size: one recovery block per this many data blocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Redundancy(u8);

impl Redundancy {
    /// Smallest group size.
    pub const MIN: u8 = 2;
    /// Largest group size.
    pub const MAX: u8 = 10;
    /// Group size used when none is chosen.
    pub const DEFAULT: u8 = 5;

    /// `Some` when `group_size` lies between [`Self::MIN`] and [`Self::MAX`].
    pub fn new(group_size: u8) -> Option<Self> {
        (Self::MIN..=Self::MAX)
            .contains(&group_size)
            .then_some(Self(group_size))
    }

    /// Data blocks per recovery block.
    pub fn group_size(self) -> usize {
        self.0 as usize
    }
}

impl Default for Redundancy {
    fn default() -> Self {
        Self(Self::DEFAULT)
    }
}

/// A block exactly as it appears on paper (before the dot-level XOR mask).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RawBlock(pub [u8; BLOCK_LEN]);

impl RawBlock {
    pub(crate) fn zeroed() -> Self {
        Self([0; BLOCK_LEN])
    }

    pub(crate) fn new(address: u32, payload: &[u8; DATA_LEN]) -> Self {
        let mut block = Self::zeroed();
        block.0[..4].copy_from_slice(&address.to_le_bytes());
        block.0[4..4 + DATA_LEN].copy_from_slice(payload);
        block
    }

    pub(crate) fn address(&self) -> u32 {
        u32::from_le_bytes([self.0[0], self.0[1], self.0[2], self.0[3]])
    }

    pub(crate) fn payload_array(&self) -> [u8; DATA_LEN] {
        std::array::from_fn(|i| self.0[4 + i])
    }

    /// Fills in the CRC and the error correction code.
    pub(crate) fn seal(&mut self) {
        let crc = crc16(&self.0[..CRC_COVERED]) ^ CRC_MASK;
        self.0[CRC_OFFSET..CRC_OFFSET + 2].copy_from_slice(&crc.to_le_bytes());
        let parity = reed_solomon::encode(&self.0, ECC_PAD);
        self.0[ECC_OFFSET..].copy_from_slice(&parity);
    }

    #[cfg(test)]
    pub(crate) fn has_valid_crc(&self) -> bool {
        let stored = u16::from_le_bytes([self.0[CRC_OFFSET], self.0[CRC_OFFSET + 1]]);
        crc16(&self.0[..CRC_COVERED]) ^ CRC_MASK == stored
    }

    /// Repairs the block in place; returns the number of corrected bytes.
    pub(crate) fn correct(&mut self) -> Option<usize> {
        reed_solomon::decode(&mut self.0, ECC_PAD)
    }

    /// Repairs the block in place when the bytes at `erased` are probably the damaged ones.
    pub(crate) fn correct_erasing(&mut self, erased: &[usize]) -> Option<usize> {
        reed_solomon::decode_with_erasures(&mut self.0, ECC_PAD, erased)
    }

    /// The 32 rows of 32 dots, bit 0 of each row being the leftmost dot.
    pub(crate) fn rows(&self) -> [u32; BLOCK_DOTS] {
        std::array::from_fn(|row| {
            let at = row * 4;
            u32::from_le_bytes([self.0[at], self.0[at + 1], self.0[at + 2], self.0[at + 3]])
        })
    }

    pub(crate) fn from_rows(rows: &[u32; BLOCK_DOTS]) -> Self {
        let mut block = Self::zeroed();
        for (row, value) in rows.iter().enumerate() {
            block.0[row * 4..row * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        block
    }
}

/// Alternating mask that keeps empty blocks from printing as solid areas.
pub(crate) fn row_mask(row: usize) -> u32 {
    if row.is_multiple_of(2) {
        0x5555_5555
    } else {
        0xAAAA_AAAA
    }
}

/// Address of a recovery block: group start offset tagged with the group size.
pub(crate) fn recovery_address(group_start: u32, redundancy: Redundancy) -> u32 {
    group_start ^ ((redundancy.group_size() as u32) << GROUP_SHIFT)
}

/// What a decoded block address means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BlockKind {
    Superblock,
    Data { offset: u32 },
    Recovery { offset: u32, group_size: usize },
}

impl BlockKind {
    pub(crate) fn of(address: u32) -> Self {
        if address == SUPERBLOCK_ADDRESS {
            return Self::Superblock;
        }
        let offset = address & ADDRESS_MASK;
        match (address >> GROUP_SHIFT) & 0xF {
            0 => Self::Data { offset },
            group_size => Self::Recovery {
                offset,
                group_size: group_size as usize,
            },
        }
    }
}

/// Windows FILETIME: 100 ns ticks since 1601-01-01 UTC.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FileTime {
    /// Low 32 bits of the tick count.
    pub low: u32,
    /// High 32 bits of the tick count.
    pub high: u32,
}

const TICKS_PER_SECOND: u64 = 10_000_000;
const UNIX_EPOCH_AS_FILETIME_SECONDS: u64 = 11_644_473_600;

impl FileTime {
    /// The time `seconds` after 1970-01-01 UTC.
    pub fn from_unix_seconds(seconds: u64) -> Self {
        let ticks = (seconds + UNIX_EPOCH_AS_FILETIME_SECONDS) * TICKS_PER_SECOND;
        Self {
            low: ticks as u32,
            high: (ticks >> 32) as u32,
        }
    }

    /// Seconds since 1970-01-01 UTC; `None` for earlier times, including the unset value.
    pub fn to_unix_seconds(self) -> Option<u64> {
        let ticks = (u64::from(self.high) << 32) | u64::from(self.low);
        (ticks / TICKS_PER_SECOND).checked_sub(UNIX_EPOCH_AS_FILETIME_SECONDS)
    }
}

/// Mode bits of a backup.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Mode(pub u8);

impl Mode {
    pub(crate) const COMPRESSED: u8 = 0x01;
    pub(crate) const ENCRYPTED: u8 = 0x02;

    pub(crate) fn is_compressed(self) -> bool {
        self.0 & Self::COMPRESSED != 0
    }

    pub(crate) fn is_encrypted(self) -> bool {
        self.0 & Self::ENCRYPTED != 0
    }
}

/// The identification block repeated on every page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SuperBlock {
    /// Size of the (compressed, encrypted) data, aligned to 16 bytes.
    pub(crate) data_size: u32,
    /// Bytes of that data carried by one full page.
    pub(crate) page_size: u32,
    pub(crate) original_size: u32,
    pub(crate) mode: Mode,
    pub(crate) attributes: u8,
    /// 1-based page number.
    pub(crate) page: u16,
    pub(crate) modified: FileTime,
    /// CRC-16 of the compressed data before encryption.
    pub(crate) file_crc: u16,
    /// File name (31 bytes at most) followed by salt and IV when encrypted.
    pub(crate) name: [u8; NAME_LEN],
}

impl SuperBlock {
    pub(crate) fn to_raw(&self) -> RawBlock {
        let mut raw = RawBlock::zeroed();
        let bytes = &mut raw.0;
        bytes[0..4].copy_from_slice(&SUPERBLOCK_ADDRESS.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.data_size.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.page_size.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.original_size.to_le_bytes());
        bytes[16] = self.mode.0;
        bytes[17] = self.attributes;
        bytes[18..20].copy_from_slice(&self.page.to_le_bytes());
        bytes[20..24].copy_from_slice(&self.modified.low.to_le_bytes());
        bytes[24..28].copy_from_slice(&self.modified.high.to_le_bytes());
        bytes[28..30].copy_from_slice(&self.file_crc.to_le_bytes());
        bytes[NAME_OFFSET..NAME_OFFSET + NAME_LEN].copy_from_slice(&self.name);
        raw
    }

    pub(crate) fn from_raw(raw: &RawBlock) -> Self {
        let b = &raw.0;
        let u32_at = |at: usize| u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        let u16_at = |at: usize| u16::from_le_bytes([b[at], b[at + 1]]);
        let mut name = [0u8; NAME_LEN];
        name.copy_from_slice(&b[NAME_OFFSET..NAME_OFFSET + NAME_LEN]);
        Self {
            data_size: u32_at(4),
            page_size: u32_at(8),
            original_size: u32_at(12),
            mode: Mode(b[16]),
            attributes: b[17],
            page: u16_at(18),
            modified: FileTime {
                low: u32_at(20),
                high: u32_at(24),
            },
            file_crc: u16_at(28),
            name,
        }
    }

    /// The file name written by PaperBack 1.00, which may fill the whole field.
    pub(crate) fn full_name(&self) -> String {
        let end = self.name.iter().position(|&b| b == 0).unwrap_or(NAME_LEN);
        decode_ansi(&self.name[..end])
    }

    /// Salt and IV of an encrypted backup: the second half of the name field.
    pub(crate) fn salt_and_iv(&self) -> [u8; NAME_LEN - NAME_TEXT_LEN] {
        std::array::from_fn(|i| self.name[NAME_TEXT_LEN + i])
    }

    /// The file name: bytes up to the first NUL within the text part.
    pub(crate) fn file_name(&self) -> String {
        let text = &self.name[..NAME_TEXT_LEN];
        let end = text.iter().position(|&b| b == 0).unwrap_or(text.len());
        decode_ansi(&text[..end])
    }
}

/// Names are stored in the Windows ANSI code page; accept UTF-8 as well.
fn decode_ansi(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => bytes.iter().map(|&b| char::from(b)).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_superblock() -> SuperBlock {
        let mut name = [0u8; NAME_LEN];
        name[..9].copy_from_slice(b"notes.txt");
        SuperBlock {
            data_size: 1600,
            page_size: 4500,
            original_size: 1500,
            mode: Mode(Mode::COMPRESSED),
            attributes: 0x20,
            page: 3,
            modified: FileTime::from_unix_seconds(1_700_000_000),
            file_crc: 0xBEEF,
            name,
        }
    }

    #[test]
    fn superblock_round_trips_through_raw_bytes() {
        let block = sample_superblock();
        assert_eq!(SuperBlock::from_raw(&block.to_raw()), block);
    }

    #[test]
    fn superblock_uses_the_documented_offsets() {
        let raw = sample_superblock().to_raw();
        assert_eq!(raw.address(), SUPERBLOCK_ADDRESS);
        assert_eq!(raw.0[16], Mode::COMPRESSED);
        assert_eq!(&raw.0[30..39], b"notes.txt");
    }

    #[test]
    fn sealed_block_has_valid_crc_and_survives_damage() {
        let mut block = RawBlock::new(180, &[7; DATA_LEN]);
        block.seal();
        assert!(block.has_valid_crc());
        let sealed = block.clone();
        for at in [1usize, 20, 50, 99, 120] {
            block.0[at] ^= 0xFF;
        }
        assert_eq!(block.correct(), Some(5));
        assert_eq!(block, sealed);
    }

    #[test]
    fn block_kind_distinguishes_data_recovery_and_super() {
        assert_eq!(BlockKind::of(SUPERBLOCK_ADDRESS), BlockKind::Superblock);
        assert_eq!(BlockKind::of(450), BlockKind::Data { offset: 450 });
        let address = recovery_address(450, Redundancy::default());
        assert_eq!(
            BlockKind::of(address),
            BlockKind::Recovery {
                offset: 450,
                group_size: 5
            }
        );
    }

    #[test]
    fn rows_round_trip() {
        let mut block = RawBlock::new(0, &[0xA5; DATA_LEN]);
        block.seal();
        assert_eq!(RawBlock::from_rows(&block.rows()), block);
    }

    #[test]
    fn filetime_round_trips() {
        let time = FileTime::from_unix_seconds(1_700_000_000);
        assert_eq!(time.to_unix_seconds(), Some(1_700_000_000));
    }

    #[test]
    fn redundancy_rejects_values_outside_two_to_ten() {
        assert!(Redundancy::new(1).is_none());
        assert!(Redundancy::new(11).is_none());
        assert_eq!(Redundancy::new(7).map(Redundancy::group_size), Some(7));
    }
}
