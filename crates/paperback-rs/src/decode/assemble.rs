// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider
// Derived from PaperBack 1.10, Copyright (c) 2007 Oleh Yuschuk; see NOTICE.md.

//! Gathers the blocks of all scanned pages of one backup, repairs missing
//! blocks from the recovery blocks and restores the original file.

use crate::block::{DATA_LEN, FileTime, SuperBlock};
use crate::codec::{self, SALT_AND_IV_LEN};
use crate::crc::crc16;
use crate::error::{Error, Result};
use crate::quality::PageQuality;

/// What was read from one page.
#[derive(Debug)]
pub(crate) struct ScannedPage {
    pub(crate) label: SuperBlock,
    /// Group size announced by recovery blocks on this page; 0 if none was read.
    pub(crate) group_size: usize,
    pub(crate) blocks: Vec<ScannedBlock>,
    pub(crate) statistics: PageStatistics,
    /// How well each block read, when asked for.
    pub(crate) quality: Option<PageQuality>,
}

impl ScannedPage {
    /// Adds the blocks of another reading of the same page that are not yet known.
    pub(crate) fn absorb(&mut self, other: ScannedPage) {
        let known: std::collections::HashSet<(bool, u32)> =
            self.blocks.iter().map(ScannedBlock::identity).collect();
        let before = self.blocks.len();
        self.blocks.extend(
            other
                .blocks
                .into_iter()
                .filter(|b| !known.contains(&b.identity())),
        );
        self.statistics.good_blocks += self.blocks.len() - before;
        self.statistics.bad_blocks = self.statistics.bad_blocks.min(other.statistics.bad_blocks);
        self.statistics.restored_bytes = self
            .statistics
            .restored_bytes
            .max(other.statistics.restored_bytes);
        if self.group_size == 0 {
            self.group_size = other.group_size;
        }
        let better = |a: &Option<PageQuality>, b: &Option<PageQuality>| match (a, b) {
            (Some(a), Some(b)) => b.readable_blocks() > a.readable_blocks(),
            (None, Some(_)) => true,
            _ => false,
        };
        if better(&self.quality, &other.quality) {
            self.quality = other.quality;
        }
    }
}

#[derive(Debug)]
pub(crate) enum ScannedBlock {
    Data {
        offset: u32,
        payload: [u8; DATA_LEN],
    },
    Recovery {
        offset: u32,
        group_size: usize,
        payload: [u8; DATA_LEN],
    },
}

impl ScannedBlock {
    /// Whether it is a recovery block, and the offset it covers.
    fn identity(&self) -> (bool, u32) {
        match *self {
            Self::Data { offset, .. } => (false, offset),
            Self::Recovery { offset, .. } => (true, offset),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
/// Block counts of one scanned page.
pub struct PageStatistics {
    /// Data and recovery blocks read.
    pub good_blocks: usize,
    /// Blocks found but too damaged to read.
    pub bad_blocks: usize,
    /// Label blocks read.
    pub superblocks: usize,
    /// Bytes repaired by error correction.
    pub restored_bytes: usize,
}

/// Summary of a restoration, for diagnostics.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Pages that could be read.
    pub pages_read: usize,
    /// Blocks read in total.
    pub good_blocks: usize,
    /// Blocks found but too damaged to read.
    pub bad_blocks: usize,
    /// Bytes repaired by error correction.
    pub restored_bytes: usize,
    /// Blocks rebuilt from recovery blocks.
    pub recovered_blocks: usize,
}

#[derive(Clone, Debug)]
/// A restored file and how the restoration went.
pub struct RestoredFile {
    /// The file contents.
    pub data: Vec<u8>,
    /// The file name stored on the pages.
    pub name: String,
    /// The modification time stored on the pages.
    pub modified: Option<FileTime>,
    /// Windows file attributes of the original file.
    pub attributes: u8,
    /// Statistics of the restoration.
    pub report: Report,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Slot {
    Missing,
    Data,
    Recovery,
}

struct Backup {
    label: SuperBlock,
    page_size: usize,
    block_count: usize,
    slots: Vec<Slot>,
    data: Vec<u8>,
    valid_blocks: usize,
    group_size: usize,
    legacy_name: bool,
    lowest_address: u32,
    highest_address: u32,
    report: Report,
}

impl Backup {
    fn new(label: &SuperBlock) -> Self {
        let block_count = (label.data_size as usize).div_ceil(DATA_LEN);
        Self {
            label: label.clone(),
            page_size: label.page_size as usize,
            block_count,
            slots: vec![Slot::Missing; block_count],
            data: vec![0; block_count * DATA_LEN],
            valid_blocks: 0,
            group_size: 0,
            legacy_name: false,
            lowest_address: u32::MAX,
            highest_address: 0,
            report: Report::default(),
        }
    }

    fn belongs_to(&self, label: &SuperBlock) -> bool {
        let same_name = self.label.name.eq_ignore_ascii_case(&label.name);
        same_name
            && self.label.mode == label.mode
            && self.label.modified == label.modified
            && self.label.data_size == label.data_size
            && self.label.original_size == label.original_size
    }

    fn page_count(&self) -> usize {
        if self.page_size == 0 {
            0
        } else {
            (self.label.data_size as usize).div_ceil(self.page_size)
        }
    }

    fn start_page(&mut self, page: &ScannedPage) {
        if self.page_size != page.label.page_size as usize {
            self.page_size = 0;
        }
        self.group_size = page.group_size;
        self.lowest_address = u32::MAX;
        self.highest_address = 0;
    }

    fn add_block(&mut self, block: &ScannedBlock) {
        match *block {
            ScannedBlock::Data {
                offset,
                ref payload,
            } => {
                if !(offset as usize).is_multiple_of(DATA_LEN) {
                    return;
                }
                let index = offset as usize / DATA_LEN;
                if index >= self.block_count {
                    return;
                }
                if self.slots[index] != Slot::Data {
                    self.data[offset as usize..offset as usize + DATA_LEN].copy_from_slice(payload);
                    self.slots[index] = Slot::Data;
                    self.valid_blocks += 1;
                }
                self.lowest_address = self.lowest_address.min(offset);
                self.highest_address = self.highest_address.max(offset + DATA_LEN as u32);
            }
            ScannedBlock::Recovery {
                offset,
                group_size,
                ref payload,
            } => {
                let span = (group_size * DATA_LEN) as u32;
                if group_size != self.group_size || offset % span != 0 {
                    return;
                }
                let first = offset as usize / DATA_LEN;
                for index in first..first + group_size {
                    if index >= self.block_count {
                        return;
                    }
                    if self.slots[index] != Slot::Missing {
                        continue;
                    }
                    self.data[index * DATA_LEN..(index + 1) * DATA_LEN].copy_from_slice(payload);
                    self.slots[index] = Slot::Recovery;
                }
                self.lowest_address = self.lowest_address.min(offset);
                self.highest_address = self.highest_address.max(offset + span);
            }
        }
    }

    /// Rebuilds blocks that are missing but whose group has a recovery block:
    /// the recovery block is the inverted XOR of all blocks of its group.
    fn repair_groups(&mut self) {
        if self.group_size == 0 {
            return;
        }
        let span = (DATA_LEN * self.group_size) as u32;
        let first = (self.lowest_address / span) as usize * self.group_size;
        let last = (self.highest_address / span) as usize * self.group_size;
        for start in (first..=last).step_by(self.group_size) {
            if start + self.group_size > self.block_count {
                break;
            }
            let group = start..start + self.group_size;
            let recovery_slots: Vec<usize> = group
                .clone()
                .filter(|&i| self.slots[i] == Slot::Recovery)
                .collect();
            for &i in &recovery_slots {
                self.slots[i] = Slot::Missing;
            }
            if let [missing] = recovery_slots[..] {
                let mut rebuilt = [0u8; DATA_LEN];
                for (byte, stored) in rebuilt.iter_mut().zip(&self.data[missing * DATA_LEN..]) {
                    *byte = !stored;
                }
                for i in group.filter(|&i| i != missing) {
                    for (byte, source) in rebuilt
                        .iter_mut()
                        .zip(&self.data[i * DATA_LEN..(i + 1) * DATA_LEN])
                    {
                        *byte ^= source;
                    }
                }
                self.data[missing * DATA_LEN..(missing + 1) * DATA_LEN].copy_from_slice(&rebuilt);
                self.slots[missing] = Slot::Data;
                self.valid_blocks += 1;
                self.report.recovered_blocks += 1;
            }
        }
    }

    fn is_complete(&self) -> bool {
        self.valid_blocks == self.block_count
    }

    /// 1-based numbers of pages that still lack blocks.
    fn incomplete_pages(&self) -> Vec<usize> {
        if self.page_size == 0 {
            return Vec::new();
        }
        let blocks_per_page = self.page_size / DATA_LEN;
        (0..self.page_count())
            .filter(|&page| {
                let start = page * blocks_per_page;
                (start..(start + blocks_per_page).min(self.block_count))
                    .any(|i| self.slots[i] != Slot::Data)
            })
            .map(|page| page + 1)
            .collect()
    }

    fn restore(mut self, password: Option<&str>) -> Result<RestoredFile> {
        let label = self.label.clone();
        if label.mode.is_encrypted() {
            self.decrypt(password)?;
        }
        let data_len = label.data_size as usize;
        let original_len = label.original_size as usize;
        let data = if label.mode.is_compressed() {
            let expected = if original_len == 0 {
                data_len * 4
            } else {
                original_len
            };
            codec::decompress(&self.data[..data_len], expected)?
        } else {
            self.data[..original_len.min(data_len)].to_vec()
        };
        Ok(RestoredFile {
            data,
            name: if self.legacy_name {
                label.full_name()
            } else {
                label.file_name()
            },
            modified: Some(label.modified),
            attributes: label.attributes,
            report: self.report,
        })
    }

    fn decrypt(&mut self, password: Option<&str>) -> Result<()> {
        let data_len = self.label.data_size as usize;
        if !data_len.is_multiple_of(codec::ALIGNMENT) {
            return Err(Error::Decode("encrypted data is not aligned".into()));
        }
        let password = password.ok_or(Error::PasswordRequired)?;
        let salt_and_iv: [u8; SALT_AND_IV_LEN] = self.label.salt_and_iv();
        let encrypted = &self.data[..data_len];
        let file_crc = self.label.file_crc;
        let attempt = |decrypt: &dyn Fn(&mut [u8]) -> Result<()>| -> Result<Option<Vec<u8>>> {
            let mut plain = encrypted.to_vec();
            decrypt(&mut plain)?;
            Ok((crc16(&plain) == file_crc).then_some(plain))
        };
        let plain =
            if let Some(plain) = attempt(&|data| codec::decrypt(data, password, &salt_and_iv))? {
                plain
            } else {
                // Pages from PaperBack 1.00 carry no salt; the name field is plain text.
                let plain = attempt(&|data| codec::decrypt_legacy(data, password))?
                    .ok_or(Error::WrongPassword)?;
                self.legacy_name = true;
                plain
            };
        self.data[..data_len].copy_from_slice(&plain);
        Ok(())
    }
}

/// Collects scanned pages; pages of different backups are kept apart.
#[derive(Default)]
pub(crate) struct Assembler {
    backups: Vec<Backup>,
}

impl Assembler {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn add_page(&mut self, page: &ScannedPage) {
        let position = self
            .backups
            .iter()
            .position(|backup| backup.belongs_to(&page.label));
        let position = position.unwrap_or_else(|| {
            self.backups.push(Backup::new(&page.label));
            self.backups.len() - 1
        });
        let backup = &mut self.backups[position];
        backup.start_page(page);
        for block in &page.blocks {
            backup.add_block(block);
        }
        backup.report.pages_read += 1;
        backup.report.good_blocks += page.statistics.good_blocks + page.statistics.superblocks;
        backup.report.bad_blocks += page.statistics.bad_blocks;
        backup.report.restored_bytes += page.statistics.restored_bytes;
        backup.repair_groups();
    }

    /// Restores the file if all of its blocks are present.
    pub(crate) fn finish(mut self, password: Option<&str>) -> Result<RestoredFile> {
        let backup = match self.backups.len() {
            0 => return Err(Error::NoReadablePage),
            1 => self.backups.remove(0),
            n => {
                return Err(Error::Decode(format!(
                    "the pages belong to {n} different backups; give only the pages of one"
                )));
            }
        };
        if !backup.is_complete() {
            let pages = backup.incomplete_pages();
            return Err(Error::Incomplete {
                recovered: backup.valid_blocks,
                total: backup.block_count,
                pages: if pages.is_empty() {
                    "unknown".into()
                } else {
                    pages
                        .iter()
                        .map(usize::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                },
            });
        }
        backup.restore(password)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{Mode, NAME_LEN};

    fn label(data_size: u32, page_size: u32) -> SuperBlock {
        SuperBlock {
            data_size,
            page_size,
            original_size: data_size,
            mode: Mode(0),
            attributes: 0x20,
            page: 1,
            modified: FileTime::from_unix_seconds(1_600_000_000),
            file_crc: 0,
            name: [0; NAME_LEN],
        }
    }

    fn payload(seed: u8) -> [u8; DATA_LEN] {
        std::array::from_fn(|i| seed.wrapping_add(i as u8))
    }

    fn page_with(blocks: Vec<ScannedBlock>, group_size: usize) -> ScannedPage {
        ScannedPage {
            label: label(10 * DATA_LEN as u32, 10 * DATA_LEN as u32),
            group_size,
            blocks,
            statistics: PageStatistics::default(),
            quality: None,
        }
    }

    #[test]
    fn a_missing_block_is_rebuilt_from_the_recovery_block() {
        let group: Vec<[u8; DATA_LEN]> = (0..5).map(|i| payload(i * 11)).collect();
        let mut recovery = [0xFFu8; DATA_LEN];
        for block in &group {
            for (r, b) in recovery.iter_mut().zip(block) {
                *r ^= b;
            }
        }
        let mut blocks: Vec<ScannedBlock> = group
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != 2)
            .map(|(i, p)| ScannedBlock::Data {
                offset: (i * DATA_LEN) as u32,
                payload: *p,
            })
            .collect();
        blocks.push(ScannedBlock::Recovery {
            offset: 0,
            group_size: 5,
            payload: recovery,
        });

        let mut assembler = Assembler::new();
        assembler.add_page(&page_with(blocks, 5));
        let backup = &assembler.backups[0];
        assert_eq!(backup.report.recovered_blocks, 1);
        assert_eq!(&backup.data[2 * DATA_LEN..3 * DATA_LEN], &group[2]);
    }

    #[test]
    fn incomplete_backups_name_the_pages_to_rescan() {
        let mut assembler = Assembler::new();
        assembler.add_page(&page_with(Vec::new(), 0));
        match assembler.finish(None) {
            Err(Error::Incomplete {
                recovered: 0,
                total: 10,
                pages,
            }) => assert_eq!(pages, "1"),
            other => panic!("unexpected {other:?}"),
        }
    }
}
