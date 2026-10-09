// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider

//! PBX1: information that paperback.rs adds to its pages and that PaperBack 1.00 and 1.10
//! ignore.
//!
//! The originals accept a data block only if its address is a multiple of 90 below the end of
//! the data, and drop every other data block without effect. PBX1 records are data blocks with
//! addresses *behind* the end of the data, so the originals read such a page as if they were
//! not there, while this program finds them. They sit in cells that would otherwise repeat the
//! page label, and the block checksum and error correction protect them like any block.
//!
//! The records say what the originals' 16-bit checksum cannot: a SHA-256 of the whole file, and
//! which sheet a page is, with the layout of its cells.
//!
//! Addresses with a non-zero high nibble would be read as recovery blocks by the originals and
//! must never be used.

use sha2::{Digest, Sha256};

use crate::block::{ADDRESS_MASK, DATA_LEN};

/// First bytes of every record.
const MAGIC: [u8; 4] = *b"PBX1";
/// The only version of the records so far.
const VERSION: u8 = 1;
const KIND_OFFSET: usize = 4;
const VERSION_OFFSET: usize = 5;
const BODY_OFFSET: usize = 6;

const SHEET_KIND: u8 = 1;
const HASH_KIND: u8 = 2;
const PARITY_KIND: u8 = 3;

/// Bytes of a SHA-256.
pub(crate) const HASH_LEN: usize = 32;
/// Bytes of a sheet identifier.
pub(crate) const SHEET_ID_LEN: usize = 16;

/// Identifies one sheet: the same page of the same file always has the same identifier, so
/// every picture of it can be told from pictures of other sheets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SheetId(pub [u8; SHEET_ID_LEN]);

impl SheetId {
    /// The identifier of page `page` of the file whose stored bytes (compressed and encrypted,
    /// as on paper) have the hash `stream_hash`. Nothing here depends on the contents of an
    /// encrypted file.
    pub(crate) fn derive(stream_hash: &[u8; HASH_LEN], page: u16, data_size: u32) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"PBX1 sheet");
        hasher.update(stream_hash);
        hasher.update(page.to_le_bytes());
        hasher.update(data_size.to_le_bytes());
        let digest = hasher.finalize();
        let mut id = [0u8; SHEET_ID_LEN];
        id.copy_from_slice(&digest[..SHEET_ID_LEN]);
        Self(id)
    }
}

impl std::fmt::Display for SheetId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}

/// What a decoder learned about the correctness of the restored file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Integrity {
    /// The pages carry no hash; only the checksums of the blocks vouch for the file.
    #[default]
    Unchecked,
    /// The pages say they carry a hash, but none was read.
    Missing,
    /// The file has the SHA-256 that the pages announce.
    Verified,
}

/// The SHA-256 of a file.
pub(crate) fn file_hash(data: &[u8]) -> [u8; HASH_LEN] {
    Sha256::digest(data).into()
}

/// What a sheet says about itself: which one it is, and how its cells are laid out, so that a
/// decoder knows which block belongs in which cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SheetRecord {
    pub(crate) id: SheetId,
    pub(crate) page: u16,
    pub(crate) page_count: u16,
    pub(crate) columns: u16,
    pub(crate) rows: u16,
    /// Data blocks per recovery block.
    pub(crate) group_size: u8,
}

/// A check value of the file the pages hold, and its size to tie it to this file. It is the
/// SHA-256 of the file, or for an encrypted file an HMAC-SHA256 under a key from the password,
/// because the hash of the plaintext of an encrypted file must not be readable from the paper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HashRecord {
    pub(crate) digest: [u8; HASH_LEN],
    pub(crate) original_size: u32,
    /// Whether `digest` is keyed with the password.
    pub(crate) keyed: bool,
}

/// The bytes after the header of a record.
type Body = [u8; DATA_LEN - BODY_OFFSET];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Record {
    Sheet(SheetRecord),
    Hash(HashRecord),
    /// The bytes of the sheet record and of the hash record added up without carry; either
    /// of those can be had back from the other and this one.
    Parity(Body),
}

impl Record {
    /// The 90 bytes of the block that carries this record.
    pub(crate) fn to_payload(self) -> [u8; DATA_LEN] {
        let mut payload = [0u8; DATA_LEN];
        payload[..MAGIC.len()].copy_from_slice(&MAGIC);
        payload[VERSION_OFFSET] = VERSION;
        payload[KIND_OFFSET] = match self {
            Self::Sheet(_) => SHEET_KIND,
            Self::Hash(_) => HASH_KIND,
            Self::Parity(_) => PARITY_KIND,
        };
        let body = &mut payload[BODY_OFFSET..];
        match self {
            Self::Sheet(sheet) => {
                body[..SHEET_ID_LEN].copy_from_slice(&sheet.id.0);
                let numbers = [sheet.page, sheet.page_count, sheet.columns, sheet.rows];
                for (at, number) in numbers.iter().enumerate() {
                    let start = SHEET_ID_LEN + at * 2;
                    body[start..start + 2].copy_from_slice(&number.to_le_bytes());
                }
                body[SHEET_ID_LEN + 8] = sheet.group_size;
            }
            Self::Hash(hash) => {
                body[..HASH_LEN].copy_from_slice(&hash.digest);
                body[HASH_LEN..HASH_LEN + 4].copy_from_slice(&hash.original_size.to_le_bytes());
                body[HASH_LEN + 4] = u8::from(hash.keyed);
            }
            Self::Parity(parity) => body.copy_from_slice(&parity),
        }
        payload
    }

    /// The record in a block's payload; `None` if the block is not a PBX1 record of a
    /// version this program knows.
    pub(crate) fn parse(payload: &[u8; DATA_LEN]) -> Option<Self> {
        if !is_record(payload) || payload[VERSION_OFFSET] != VERSION {
            return None;
        }
        let body = &payload[BODY_OFFSET..];
        let number = |at: usize| u16::from_le_bytes([body[at], body[at + 1]]);
        match payload[KIND_OFFSET] {
            SHEET_KIND => {
                let mut id = [0u8; SHEET_ID_LEN];
                id.copy_from_slice(&body[..SHEET_ID_LEN]);
                Some(Self::Sheet(SheetRecord {
                    id: SheetId(id),
                    page: number(SHEET_ID_LEN),
                    page_count: number(SHEET_ID_LEN + 2),
                    columns: number(SHEET_ID_LEN + 4),
                    rows: number(SHEET_ID_LEN + 6),
                    group_size: body[SHEET_ID_LEN + 8],
                }))
            }
            HASH_KIND => {
                let mut digest = [0u8; HASH_LEN];
                digest.copy_from_slice(&body[..HASH_LEN]);
                let size = &body[HASH_LEN..HASH_LEN + 4];
                Some(Self::Hash(HashRecord {
                    digest,
                    original_size: u32::from_le_bytes([size[0], size[1], size[2], size[3]]),
                    keyed: body[HASH_LEN + 4] == 1,
                }))
            }
            PARITY_KIND => {
                let mut parity = [0u8; DATA_LEN - BODY_OFFSET];
                parity.copy_from_slice(body);
                Some(Self::Parity(parity))
            }
            _ => None,
        }
    }

    fn body(self) -> Body {
        let mut body = [0u8; DATA_LEN - BODY_OFFSET];
        body.copy_from_slice(&self.to_payload()[BODY_OFFSET..]);
        body
    }

    /// The parity record of a sheet record and a hash record.
    pub(crate) fn parity_of(sheet: &SheetRecord, hash: &HashRecord) -> Self {
        let mut parity = Self::Sheet(*sheet).body();
        parity
            .iter_mut()
            .zip(Self::Hash(*hash).body())
            .for_each(|(p, h)| *p ^= h);
        Self::Parity(parity)
    }
}

/// The records read from a page, with the sheet record or the hash record rebuilt from the
/// other two when one of them did not read.
pub(crate) fn with_missing_rebuilt(mut records: Vec<Record>) -> Vec<Record> {
    let find =
        |records: &[Record], wanted: fn(&Record) -> bool| records.iter().copied().find(wanted);
    let sheet = find(&records, |r| matches!(r, Record::Sheet(_)));
    let hash = find(&records, |r| matches!(r, Record::Hash(_)));
    let Some(Record::Parity(parity)) = find(&records, |r| matches!(r, Record::Parity(_))) else {
        return records;
    };
    let rebuilt = match (sheet, hash) {
        (None, Some(known)) => Some((SHEET_KIND, known)),
        (Some(known), None) => Some((HASH_KIND, known)),
        _ => None,
    };
    if let Some((kind, known)) = rebuilt {
        let mut payload = [0u8; DATA_LEN];
        payload[..MAGIC.len()].copy_from_slice(&MAGIC);
        payload[KIND_OFFSET] = kind;
        payload[VERSION_OFFSET] = VERSION;
        for (at, (p, k)) in parity.iter().zip(known.body()).enumerate() {
            payload[BODY_OFFSET + at] = p ^ k;
        }
        records.extend(Record::parse(&payload));
    }
    records
}

/// Whether a block payload starts like a PBX1 record, of whatever version or kind.
pub(crate) fn is_record(payload: &[u8; DATA_LEN]) -> bool {
    payload.starts_with(&MAGIC)
}

/// Where the record number `index` of a file whose data takes `data_size` bytes is stored:
/// behind the last data block, which the originals do not accept. `None` if the address
/// space of the format ends before.
pub(crate) fn record_address(data_size: u32, index: usize) -> Option<u32> {
    let data_blocks = (data_size as usize).div_ceil(DATA_LEN);
    let address = (data_blocks + index) * DATA_LEN;
    // The record must lie in the low 28 bits, which the originals take for a data address.
    u32::try_from(address)
        .ok()
        .filter(|&address| address + DATA_LEN as u32 <= ADDRESS_MASK)
}

/// Whether a block at `address` is behind the data and so may be a PBX1 record.
pub(crate) fn is_record_address(data_size: u32, address: u32) -> bool {
    let first = (data_size as usize).div_ceil(DATA_LEN) * DATA_LEN;
    address as usize >= first
        && address <= ADDRESS_MASK
        && (address as usize).is_multiple_of(DATA_LEN)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_sheet() -> SheetRecord {
        SheetRecord {
            id: SheetId([7; SHEET_ID_LEN]),
            page: 3,
            page_count: 9,
            columns: 21,
            rows: 33,
            group_size: 5,
        }
    }

    #[test]
    fn a_sheet_record_survives_its_block() {
        let record = Record::Sheet(a_sheet());
        assert_eq!(Record::parse(&record.to_payload()), Some(record));
    }

    #[test]
    fn a_hash_record_survives_its_block() {
        let record = Record::Hash(HashRecord {
            digest: file_hash(b"abc"),
            original_size: 3,
            keyed: true,
        });
        assert_eq!(Record::parse(&record.to_payload()), Some(record));
    }

    fn a_hash() -> HashRecord {
        HashRecord {
            digest: file_hash(b"abc"),
            original_size: 3,
            keyed: false,
        }
    }

    #[test]
    fn the_parity_record_brings_back_whichever_record_is_missing() {
        let (sheet, hash) = (Record::Sheet(a_sheet()), Record::Hash(a_hash()));
        let parity = Record::parity_of(&a_sheet(), &a_hash());
        assert_eq!(Record::parse(&parity.to_payload()), Some(parity));
        let without_sheet = with_missing_rebuilt(vec![hash, parity]);
        assert!(without_sheet.contains(&sheet));
        let without_hash = with_missing_rebuilt(vec![parity, sheet]);
        assert!(without_hash.contains(&hash));
        // Nothing to rebuild from with two missing, and nothing added when all are there.
        assert_eq!(with_missing_rebuilt(vec![sheet, parity]).len(), 3);
        assert_eq!(with_missing_rebuilt(vec![sheet, hash, parity]).len(), 3);
        assert_eq!(with_missing_rebuilt(vec![parity]).len(), 1);
        assert_eq!(with_missing_rebuilt(vec![hash]).len(), 1);
    }

    #[test]
    fn a_block_of_data_is_no_record() {
        assert_eq!(Record::parse(&[0x41; DATA_LEN]), None);
        assert!(!is_record(&[0; DATA_LEN]));
    }

    #[test]
    fn records_of_a_later_version_or_unknown_kind_are_not_read() {
        let mut payload = Record::Sheet(a_sheet()).to_payload();
        payload[VERSION_OFFSET] = VERSION + 1;
        assert_eq!(Record::parse(&payload), None);
        let mut payload = Record::Sheet(a_sheet()).to_payload();
        payload[KIND_OFFSET] = 99;
        assert_eq!(Record::parse(&payload), None);
        assert!(is_record(&payload));
    }

    #[test]
    fn records_lie_behind_the_data_on_block_boundaries() {
        // 1000 bytes take 12 blocks of 90 bytes.
        assert_eq!(record_address(1000, 0), Some(12 * 90));
        assert_eq!(record_address(1000, 1), Some(13 * 90));
        assert_eq!(record_address(1080, 0), Some(12 * 90));
        assert!(is_record_address(1000, 12 * 90));
        assert!(!is_record_address(1000, 11 * 90));
        assert!(!is_record_address(1000, 12 * 90 + 1));
    }

    #[test]
    fn a_file_that_fills_the_address_space_has_room_for_one_record_only() {
        assert!(record_address(crate::block::MAX_FILE_SIZE, 0).is_some());
        assert_eq!(record_address(crate::block::MAX_FILE_SIZE, 1), None);
    }

    #[test]
    fn a_sheet_has_an_identifier_of_its_own() {
        let hash = file_hash(b"file");
        let first = SheetId::derive(&hash, 1, 900);
        assert_eq!(first, SheetId::derive(&hash, 1, 900));
        assert_ne!(first, SheetId::derive(&hash, 2, 900));
        assert_ne!(first, SheetId::derive(&file_hash(b"other"), 1, 900));
        assert_eq!(first.to_string().len(), 2 * SHEET_ID_LEN);
    }
}
