// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider
// Derived from PaperBack 1.10, Copyright (c) 2007 Oleh Yuschuk; see NOTICE.md.

//! Gathers the blocks of all scanned pages of one backup, repairs missing
//! blocks from the recovery blocks and restores the original file.

use std::collections::{HashMap, HashSet};

use super::hints::ScanHints;
use super::sheet::LatticeRead;
use crate::block::{DATA_LEN, FileTime, SuperBlock};
use crate::codec::{self, SALT_AND_IV_LEN};
use crate::crc::crc16;
use crate::error::{Error, Result};
use crate::parity::{self, Code};
use crate::pbx::{self, HashRecord, Integrity, Record, SheetId};
use crate::quality::PageQuality;

/// What was read from one page.
#[derive(Debug)]
pub(crate) struct ScannedPage {
    pub(crate) label: SuperBlock,
    /// Group size announced by recovery blocks on this page; 0 if none was read.
    pub(crate) group_size: usize,
    pub(crate) blocks: Vec<ScannedBlock>,
    pub(crate) statistics: PageStatistics,
    /// Where in the picture given by the caller blocks were read, one point each.
    pub(crate) read_places: Vec<(f64, f64)>,
    /// Distance between neighbouring blocks in the picture given by the caller.
    pub(crate) block_size: f64,
    /// The grid of the first reading of the picture and the block read in each cell.
    pub(crate) lattice: LatticeRead,
    /// The PBX1 records read from the page.
    pub(crate) records: Vec<Record>,
    /// How well each cell read, in all readings of the picture; with the picture to draw it on
    /// only when the quality was asked for.
    pub(crate) quality: Option<PageQuality>,
    /// Every cell that read, in all readings of the picture so far: where it lies in the
    /// picture given by the caller, and the bytes corrected in it.
    pub(crate) reads: Vec<((f64, f64), usize)>,
    /// Whether passes of more effort were left out because the parity blocks read on the page
    /// cover the blocks that are missing.
    pub(crate) parity_shortcut: bool,
    /// Bytes corrected in each data and recovery block read, by [`ScannedBlock::identity`].
    pub(crate) corrections: HashMap<(bool, u32), usize>,
}

impl ScannedPage {
    /// Adds the blocks of another reading of the same page that are not yet known.
    pub(crate) fn absorb(&mut self, other: ScannedPage) {
        let known: HashSet<(bool, u32)> = self.blocks.iter().map(ScannedBlock::identity).collect();
        let before = self.blocks.len();
        self.blocks.extend(
            other
                .blocks
                .into_iter()
                .filter(|b| !known.contains(&b.identity())),
        );
        self.statistics.good_blocks += self.blocks.len() - before;
        // Only the blocks this reading adds count: a block it read again, perhaps worse through
        // a grid a little off, was read and corrected already.
        for block in &self.blocks[before..] {
            let identity = block.identity();
            let corrected = other.corrections.get(&identity).copied().unwrap_or(0);
            self.statistics.restored_bytes += corrected;
            self.corrections.insert(identity, corrected);
        }
        self.read_places.extend(other.read_places);
        if self.lattice.columns == 0 {
            self.lattice = other.lattice;
        }
        self.records.extend(other.records);
        if self.block_size <= 0.0 {
            self.block_size = other.block_size;
        }
        if self.group_size == 0 {
            self.group_size = other.group_size;
        }
        self.parity_shortcut |= other.parity_shortcut;
        self.merge_quality(other.quality, other.reads);
    }

    /// Adds the cells another reading of the picture read to the quality of this one, where
    /// they lie in the picture: a cell is unreadable only if no reading read it. Should the
    /// other reading have read more on its own (the grid of this one was off), its quality is
    /// kept instead, with the cells of this one added. The counts of the page follow the
    /// quality, so that they say what it shows.
    fn merge_quality(&mut self, other: Option<PageQuality>, reads: Vec<((f64, f64), usize)>) {
        self.quality = match (self.quality.take(), other) {
            (Some(mut mine), Some(mut theirs)) => {
                if theirs.readable_blocks() > mine.readable_blocks() {
                    for &(place, corrected) in &self.reads {
                        theirs.add_read(place, corrected);
                    }
                    Some(theirs)
                } else {
                    for &(place, corrected) in &reads {
                        mine.add_read(place, corrected);
                    }
                    Some(mine)
                }
            }
            (mine, theirs) => mine.or(theirs),
        };
        self.reads.extend(reads);
        if let Some(quality) = &self.quality {
            self.statistics.bad_blocks = quality.unreadable_blocks();
            self.statistics.effort_blocks = quality.blocks_read_with_effort();
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
    /// The PBX1 record this block is, if it lies behind the data of a file of `data_size` bytes.
    pub(crate) fn record(&self, data_size: u32) -> Option<Record> {
        match self {
            Self::Data { offset, payload } if pbx::is_record_address(data_size, *offset) => {
                Record::parse(payload)
            }
            _ => None,
        }
    }

    /// Whether it is a recovery block, and the offset it covers.
    pub(crate) fn identity(&self) -> (bool, u32) {
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
    /// Cells of the picture whose block no reading of it could read: found but too damaged to
    /// read, or between blocks that were found but without even their grid lines. The cells
    /// the quality of the page shows as unreadable.
    pub bad_blocks: usize,
    /// Cells whose block read only with more effort than the first pass over the picture gives:
    /// in a later pass over the blocks still missing (sharper settings, the dots of the page
    /// learnt, along bent grid lines) or in the picture straightened. The quality draws them
    /// hatched.
    pub effort_blocks: usize,
    /// Label blocks read.
    pub superblocks: usize,
    /// Bytes repaired by error correction.
    pub restored_bytes: usize,
    /// Blocks that were read in a cell where the layout of the page does not put them, and
    /// that were dropped. Without a layout on the page there is nothing to compare with.
    pub misplaced_blocks: usize,
    /// Which sheet the page is, when it says so.
    pub sheet_id: Option<SheetId>,
    /// What looks wrong with the scan; only filled in for pages that read badly.
    pub hints: ScanHints,
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
    /// Blocks rebuilt from PaperBack's recovery blocks.
    pub recovered_blocks: usize,
    /// Parity blocks read.
    pub parity_blocks: usize,
    /// Blocks rebuilt from parity blocks.
    pub parity_recovered_blocks: usize,
    /// Blocks dropped because they were read in the wrong cell of their page.
    pub misplaced_blocks: usize,
    /// The sheets the pages are, as far as they say so.
    pub sheets: Vec<SheetId>,
    /// Whether the restored file matches the hash on the pages.
    pub integrity: Integrity,
    /// What looks wrong with the scans that read badly, over all pages.
    pub hints: ScanHints,
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

struct Backup {
    label: SuperBlock,
    page_size: usize,
    block_count: usize,
    /// Which data blocks are known.
    present: Vec<bool>,
    data: Vec<u8>,
    valid_blocks: usize,
    group_size: usize,
    /// PaperBack's recovery blocks of the groups not yet complete, by their first block.
    recoveries: HashMap<usize, [u8; DATA_LEN]>,
    /// The parity blocks read, by index.
    parity: HashMap<usize, [u8; DATA_LEN]>,
    /// Parity blocks read with different contents in different places; neither is trusted.
    conflicting: HashSet<usize>,
    /// Parity blocks of the file, as the sheet records say; blocks beyond are not taken.
    parity_total: Option<usize>,
    code: Code,
    legacy_name: bool,
    /// The hashes the pages carry for the whole file: one, unless pages disagree.
    hashes: Vec<HashRecord>,
    report: Report,
}

impl Backup {
    fn new(label: &SuperBlock) -> Self {
        let block_count = (label.data_size as usize).div_ceil(DATA_LEN);
        Self {
            label: label.clone(),
            page_size: label.page_size as usize,
            block_count,
            present: vec![false; block_count],
            data: vec![0; block_count * DATA_LEN],
            valid_blocks: 0,
            group_size: 0,
            recoveries: HashMap::new(),
            parity: HashMap::new(),
            conflicting: HashSet::new(),
            parity_total: None,
            code: Code::new(block_count),
            legacy_name: false,
            hashes: Vec::new(),
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
        if self.group_size == 0 {
            self.group_size = page.group_size;
        }
    }

    /// Takes note of what a page says about the file and about itself: its hash, and how many
    /// parity blocks the file has.
    fn add_records(&mut self, page: &ScannedPage) {
        for record in &page.records {
            match *record {
                Record::Hash(hash)
                    if hash_fits(&self.label, &hash) && !self.hashes.contains(&hash) =>
                {
                    self.hashes.push(hash);
                }
                Record::Sheet(sheet) => {
                    let total = sheet.parity_total as usize;
                    let total = self.parity_total.map_or(total, |known| known.min(total));
                    self.parity_total = Some(total);
                    self.parity.retain(|&index, _| index < total);
                    self.report.parity_blocks = self.parity.len();
                }
                Record::Hash(_) | Record::Parity(_) => {}
            }
        }
        if let Some(id) = page.statistics.sheet_id
            && !self.report.sheets.contains(&id)
        {
            self.report.sheets.push(id);
        }
    }

    fn add_block(&mut self, block: &ScannedBlock) {
        match *block {
            ScannedBlock::Data {
                offset,
                ref payload,
            } => {
                if let Some(index) = parity::index_of(offset) {
                    self.add_parity(index, payload);
                    return;
                }
                let index = offset as usize / DATA_LEN;
                if index < self.block_count && !self.present[index] {
                    self.data[offset as usize..offset as usize + DATA_LEN].copy_from_slice(payload);
                    self.present[index] = true;
                    self.valid_blocks += 1;
                }
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
                if first < self.block_count {
                    self.recoveries.entry(first).or_insert(*payload);
                }
            }
        }
    }

    /// Takes a parity block, if the file can have it. One that was read before with other
    /// contents is dropped, both copies: one of them is not what it says it is.
    fn add_parity(&mut self, index: usize, payload: &[u8; DATA_LEN]) {
        let possible = index < self.code.max_parity()
            && self.parity_total.is_none_or(|total| index < total)
            && !self.conflicting.contains(&index);
        if !possible {
            return;
        }
        match self.parity.get(&index) {
            None => {
                self.parity.insert(index, *payload);
            }
            Some(known) if known != payload => {
                self.parity.remove(&index);
                self.conflicting.insert(index);
            }
            Some(_) => {}
        }
        self.report.parity_blocks = self.parity.len();
    }

    /// Rebuilds what the recovery and parity blocks allow, each in turn as long as either
    /// brings back blocks: a block rebuilt from parity may complete a group but one, and the
    /// other way round.
    fn repair(&mut self, pool: Option<&rayon::ThreadPool>) {
        loop {
            let mut rebuilt = self.repair_groups();
            if !self.is_complete() && !self.parity.is_empty() {
                let from_parity =
                    self.code
                        .recover(&mut self.data, &mut self.present, &self.parity, pool);
                self.valid_blocks += from_parity;
                self.report.parity_recovered_blocks += from_parity;
                rebuilt += from_parity;
            }
            if rebuilt == 0 {
                break;
            }
        }
    }

    /// Rebuilds the one missing block of groups that have a recovery block: the recovery block
    /// is the inverted XOR of all blocks of its group (blocks behind the data count as zeros).
    fn repair_groups(&mut self) -> usize {
        let mut rebuilt = 0;
        let recoveries = std::mem::take(&mut self.recoveries);
        for (first, recovery) in recoveries {
            let group = first..first + self.group_size;
            let missing: Vec<usize> = group
                .clone()
                .filter(|&i| i < self.block_count && !self.present[i])
                .collect();
            match missing[..] {
                [] => {}
                [missing] => {
                    let mut block = recovery.map(|byte| !byte);
                    for i in group.filter(|&i| i != missing && i < self.block_count) {
                        for (byte, source) in block
                            .iter_mut()
                            .zip(&self.data[i * DATA_LEN..(i + 1) * DATA_LEN])
                        {
                            *byte ^= source;
                        }
                    }
                    self.data[missing * DATA_LEN..(missing + 1) * DATA_LEN].copy_from_slice(&block);
                    self.present[missing] = true;
                    self.valid_blocks += 1;
                    self.report.recovered_blocks += 1;
                    rebuilt += 1;
                }
                _ => {
                    self.recoveries.insert(first, recovery);
                }
            }
        }
        rebuilt
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
                (start..(start + blocks_per_page).min(self.block_count)).any(|i| !self.present[i])
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
        self.report.integrity = self.verify(&data, password)?;
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

    /// Compares the file with the check values on the pages that fit the file. Pages that carry
    /// different ones are an error even if the file matches one of them: either could be the one
    /// that is not genuine.
    fn verify(&self, data: &[u8], password: Option<&str>) -> Result<Integrity> {
        let computed = match (self.label.mode.is_encrypted(), password) {
            _ if self.hashes.is_empty() => None,
            (false, _) => Some(pbx::file_hash(data)),
            (true, Some(password)) if !self.legacy_name => Some(codec::authenticate(
                data,
                password,
                &self.label.salt_and_iv(),
            )),
            (true, _) => None,
        };
        match computed {
            Some(_) if self.hashes.len() > 1 => Err(Error::ConflictingHashes),
            Some(computed) if self.hashes[0].digest == computed => Ok(Integrity::Verified),
            Some(_) => Err(Error::HashMismatch),
            None if self.label.mode.is_extended() => Ok(Integrity::Missing),
            None => Ok(Integrity::Unchecked),
        }
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
                // Pages with records come from this program, never from PaperBack 1.00: trying its
                // scheme as well would only give a wrong password one more chance in 65536 of passing
                // the 16-bit checksum and handing back garbage.
                if self.label.mode.is_extended() {
                    return Err(Error::WrongPassword);
                }
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

/// Whether a hash record can be the check value of the file with this label: it is for a file of
/// its size, and keyed exactly when the file is encrypted. Others are not taken.
pub(crate) fn hash_fits(label: &SuperBlock, hash: &HashRecord) -> bool {
    hash.original_size == label.original_size && hash.keyed == label.mode.is_encrypted()
}

/// Collects scanned pages; pages of different backups are kept apart.
#[derive(Default)]
pub(crate) struct Assembler {
    backups: Vec<Backup>,
    /// Where the parity blocks are worked on; rayon's global pool if none.
    pool: Option<&'static rayon::ThreadPool>,
}

impl Assembler {
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// An assembler that rebuilds blocks from parity on `threads` threads: 0 is as many as the
    /// processor has, 1 is one after the other.
    pub(crate) fn with_threads(threads: usize) -> Self {
        let threads = if threads == 0 {
            std::thread::available_parallelism().map_or(1, usize::from)
        } else {
            threads
        };
        Self {
            backups: Vec::new(),
            pool: super::reader::pool_of(threads),
        }
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
        // The records first: the sheet record bounds the parity blocks.
        backup.add_records(page);
        for block in &page.blocks {
            backup.add_block(block);
        }
        backup.report.misplaced_blocks += page.statistics.misplaced_blocks;
        backup.report.pages_read += 1;
        backup.report.good_blocks += page.statistics.good_blocks + page.statistics.superblocks;
        backup.report.bad_blocks += page.statistics.bad_blocks;
        backup.report.restored_bytes += page.statistics.restored_bytes;
        backup.report.hints.merge(&page.statistics.hints);
        backup.repair(self.pool);
    }

    /// Adds the blocks of another reading of a page given before (whose counts were `earlier`),
    /// without counting the page again: the counts of the page become those of this reading.
    pub(crate) fn add_more_blocks(&mut self, page: &ScannedPage, earlier: &PageStatistics) {
        if let Some(backup) = self
            .backups
            .iter_mut()
            .find(|backup| backup.belongs_to(&page.label))
        {
            let report = &mut backup.report;
            let now = &page.statistics;
            let replace = |total: &mut usize, before: usize, after: usize| {
                *total = *total - before.min(*total) + after;
            };
            replace(
                &mut report.good_blocks,
                earlier.good_blocks + earlier.superblocks,
                now.good_blocks + now.superblocks,
            );
            replace(&mut report.bad_blocks, earlier.bad_blocks, now.bad_blocks);
            replace(
                &mut report.restored_bytes,
                earlier.restored_bytes,
                now.restored_bytes,
            );
            replace(
                &mut report.misplaced_blocks,
                earlier.misplaced_blocks,
                now.misplaced_blocks,
            );
            report.hints.merge(&now.hints);
            backup.start_page(page);
            // The hash and the sheet identifier read now count, but not the page a second time.
            backup.add_records(page);
            for block in &page.blocks {
                backup.add_block(block);
            }
            backup.repair(self.pool);
        }
    }

    /// Whether the pages given so far hold the whole file, so that no more are needed.
    pub(crate) fn is_complete(&self) -> bool {
        matches!(self.backups.as_slice(), [backup] if backup.is_complete())
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
    use crate::quality::BlockQuality;

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
            read_places: Vec::new(),
            block_size: 0.0,
            lattice: LatticeRead::default(),
            records: Vec::new(),
            quality: None,
            reads: Vec::new(),
            parity_shortcut: false,
            corrections: HashMap::new(),
        }
    }

    /// A reading of blocks at these offsets (in blocks), each with the corrections given.
    fn reading(blocks: &[(u32, usize)]) -> ScannedPage {
        let mut page = page_with(
            blocks
                .iter()
                .map(|&(at, _)| ScannedBlock::Data {
                    offset: at * DATA_LEN as u32,
                    payload: payload(at as u8),
                })
                .collect(),
            0,
        );
        for &(at, corrected) in blocks {
            page.corrections
                .insert((false, at * DATA_LEN as u32), corrected);
        }
        page.statistics.good_blocks = blocks.len();
        page.statistics.restored_bytes = blocks.iter().map(|&(_, c)| c).sum();
        page
    }

    /// A reading with this quality of three cells in a row, 10 pixels apart, that read the
    /// cells at `reads` (with the bytes corrected).
    fn reading_of_cells(cells: Vec<BlockQuality>, reads: &[((f64, f64), usize)]) -> ScannedPage {
        let grid = crate::decode::grid::Grid {
            x_peak: 0.0,
            x_step: 10.0,
            x_angle: 0.0,
            y_peak: 0.0,
            y_step: 10.0,
            y_angle: 0.0,
        };
        let quality = PageQuality::new(None, grid, 3, 1, cells, vec![(0.0, 0.0); 3], None)
            .with_placed_cells(
                vec![(5.0, 5.0), (15.0, 5.0), (25.0, 5.0)],
                4.0,
                vec![false; 3],
            );
        let mut page = page_with(Vec::new(), 0);
        page.statistics.bad_blocks = quality.unreadable_blocks();
        page.quality = Some(quality);
        page.reads = reads.to_vec();
        page
    }

    #[test]
    fn a_cell_that_another_reading_reads_is_no_longer_unreadable() {
        use BlockQuality::{Known, Readable, Unreadable};
        let mut page = reading_of_cells(
            vec![Readable(0), Unreadable, Unreadable],
            &[((5.0, 5.0), 0)],
        );
        assert_eq!(page.statistics.bad_blocks, 2);
        // The picture straightened: the cell read before is known, the middle one reads now.
        page.absorb(reading_of_cells(
            vec![Known, Readable(2), Unreadable],
            &[((15.5, 4.0), 2)],
        ));
        assert_eq!(page.statistics.bad_blocks, 1);
        assert_eq!(page.statistics.effort_blocks, 1);
        assert_eq!(page.quality.as_ref().unwrap().unreadable_blocks(), 1);
    }

    #[test]
    fn a_reading_that_read_more_on_its_own_keeps_its_quality() {
        use BlockQuality::{Absent, Readable, Unreadable};
        // The grid of the first reading was off: it read one cell where the next reads two.
        let mut page = reading_of_cells(vec![Readable(0), Absent, Unreadable], &[((5.0, 5.0), 0)]);
        page.absorb(reading_of_cells(
            vec![Unreadable, Readable(0), Readable(1)],
            &[((15.0, 5.0), 0), ((25.0, 5.0), 1)],
        ));
        let quality = page.quality.as_ref().unwrap();
        assert_eq!(
            quality.readable_blocks(),
            3,
            "the cell of the first reading added"
        );
        assert_eq!(
            (page.statistics.bad_blocks, page.statistics.effort_blocks),
            (0, 1)
        );
    }

    #[test]
    fn another_reading_adds_the_corrections_of_its_new_blocks_only() {
        let mut page = reading(&[(0, 0), (1, 0)]);
        // Block 1 again, read worse, and block 2, new.
        page.absorb(reading(&[(1, 9), (2, 3)]));
        assert_eq!(page.statistics.good_blocks, 3);
        assert_eq!(page.statistics.restored_bytes, 3);
        // Nothing new: nothing more corrected.
        page.absorb(reading(&[(0, 12), (2, 7)]));
        assert_eq!(page.statistics.restored_bytes, 3);
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
    fn missing_blocks_are_rebuilt_from_parity_blocks() {
        let file: Vec<u8> = (0..10).flat_map(|i| payload(i as u8 * 7)).collect();
        let parity = Code::new(10).encode(&file, 3);
        let mut blocks: Vec<ScannedBlock> = (0..10)
            .filter(|i| ![1, 5, 9].contains(i))
            .map(|i| ScannedBlock::Data {
                offset: (i * DATA_LEN) as u32,
                payload: payload(i as u8 * 7),
            })
            .collect();
        blocks.extend(parity.iter().enumerate().map(|(k, p)| ScannedBlock::Data {
            offset: parity::address(k),
            payload: *p,
        }));
        let mut assembler = Assembler::new();
        assembler.add_page(&page_with(blocks, 0));
        let restored = assembler.finish(None).unwrap();
        assert_eq!(restored.data, file);
        assert_eq!(restored.report.parity_recovered_blocks, 3);
        assert_eq!(restored.report.parity_blocks, 3);
    }

    #[test]
    fn parity_and_recovery_blocks_help_each_other() {
        // Groups of 5: blocks 0, 1 and 5 are lost and two parity blocks are read. The recovery
        // block of the second group brings back block 5, and then the parity blocks suffice.
        let file: Vec<u8> = (0..10).flat_map(|i| payload(i as u8 * 3)).collect();
        let parity = Code::new(10).encode(&file, 2);
        let mut recovery = [0xFFu8; DATA_LEN];
        for block in file[5 * DATA_LEN..].chunks(DATA_LEN) {
            for (r, b) in recovery.iter_mut().zip(block) {
                *r ^= b;
            }
        }
        let mut blocks: Vec<ScannedBlock> = [2, 3, 4, 6, 7, 8, 9]
            .into_iter()
            .map(|i| ScannedBlock::Data {
                offset: (i * DATA_LEN) as u32,
                payload: payload(i as u8 * 3),
            })
            .collect();
        blocks.push(ScannedBlock::Recovery {
            offset: 5 * DATA_LEN as u32,
            group_size: 5,
            payload: recovery,
        });
        blocks.extend(parity.iter().enumerate().map(|(k, p)| ScannedBlock::Data {
            offset: parity::address(k),
            payload: *p,
        }));
        let mut assembler = Assembler::new();
        assembler.add_page(&page_with(blocks, 5));
        let restored = assembler.finish(None).unwrap();
        assert_eq!(restored.data, file);
        assert_eq!(restored.report.recovered_blocks, 1);
        assert_eq!(restored.report.parity_recovered_blocks, 2);
    }

    /// A file of ten blocks, the reading of a page that lacks blocks 1, 5 and 9, and the parity
    /// blocks `0..count` of the file.
    fn lacking_three(count: usize) -> (Vec<u8>, ScannedPage, Vec<[u8; DATA_LEN]>) {
        let file: Vec<u8> = (0..10).flat_map(|i| payload(i as u8 * 7)).collect();
        let parity = Code::new(10).encode(&file, count);
        let blocks = (0..10)
            .filter(|i| ![1, 5, 9].contains(i))
            .map(|i| ScannedBlock::Data {
                offset: (i * DATA_LEN) as u32,
                payload: payload(i as u8 * 7),
            })
            .collect();
        (file, page_with(blocks, 0), parity)
    }

    fn parity_block(index: usize, payload: [u8; DATA_LEN]) -> ScannedBlock {
        ScannedBlock::Data {
            offset: parity::address(index),
            payload,
        }
    }

    fn sheet_record(parity_total: u32) -> Record {
        Record::Sheet(pbx::SheetRecord {
            id: SheetId([7; pbx::SHEET_ID_LEN]),
            page: 1,
            page_count: 1,
            columns: 12,
            rows: 10,
            group_size: 0,
            parity_first: 0,
            parity_count: parity_total,
            parity_total,
        })
    }

    #[test]
    fn a_parity_block_beyond_those_the_pages_announce_is_not_taken() {
        // The file has three parity blocks; a fourth index, from a page that is not genuine,
        // would be the spare row that checks the other three and fails the file.
        let (file, mut page, parity) = lacking_three(3);
        page.blocks
            .extend(parity.iter().enumerate().map(|(k, p)| parity_block(k, *p)));
        page.blocks.push(parity_block(3, [0x5A; DATA_LEN]));
        page.records.push(sheet_record(3));
        let mut assembler = Assembler::new();
        assembler.add_page(&page);
        let restored = assembler.finish(None).unwrap();
        assert_eq!(restored.data, file);
        assert_eq!(restored.report.parity_blocks, 3);
        // Also when the sheet record comes with a later page.
        let (file, mut page, parity) = lacking_three(3);
        page.blocks
            .extend(parity.iter().enumerate().map(|(k, p)| parity_block(k, *p)));
        page.blocks.push(parity_block(3, [0x5A; DATA_LEN]));
        let mut later = page_with(Vec::new(), 0);
        later.records.push(sheet_record(3));
        let mut assembler = Assembler::new();
        assembler.add_page(&page);
        assembler.add_page(&later);
        assert_eq!(assembler.finish(None).unwrap().data, file);
    }

    #[test]
    fn a_parity_block_read_with_other_contents_elsewhere_is_not_trusted() {
        // A forged parity block 0 comes first; the genuine one on another page shows that one of
        // them is not what it says. Neither is used, and the other three rebuild the file. Once
        // the first was kept and the spare row, which caught it, left the file incomplete.
        let (file, mut page, parity) = lacking_three(4);
        page.blocks.push(parity_block(0, [0x5A; DATA_LEN]));
        let mut other = page_with(Vec::new(), 0);
        other
            .blocks
            .extend(parity.iter().enumerate().map(|(k, p)| parity_block(k, *p)));
        let mut assembler = Assembler::new();
        assembler.add_page(&page);
        assembler.add_page(&other);
        let restored = assembler.finish(None).unwrap();
        assert_eq!(restored.data, file);
        assert_eq!(restored.report.parity_blocks, 3);
    }

    #[test]
    fn pages_with_different_hashes_of_the_file_are_refused_in_any_order() {
        // Once the first hash read was the one used: with the genuine one first, the file was
        // verified although another page said it was a different file.
        let genuine = hash_of_the_file();
        let mut other = genuine;
        other.digest[0] ^= 1;
        for (first, second) in [(genuine, other), (other, genuine)] {
            let mut assembler = Assembler::new();
            assembler.add_page(&complete_page(vec![Record::Hash(first)]));
            assembler.add_page(&complete_page(vec![Record::Hash(second)]));
            assert!(matches!(
                assembler.finish(None),
                Err(Error::ConflictingHashes)
            ));
        }
        // The same hash on every page is no conflict.
        let mut assembler = Assembler::new();
        assembler.add_page(&complete_page(vec![Record::Hash(genuine)]));
        assembler.add_page(&complete_page(vec![Record::Hash(genuine)]));
        let restored = assembler.finish(None).unwrap();
        assert_eq!(restored.report.integrity, Integrity::Verified);
    }

    #[test]
    fn a_page_read_again_counts_as_its_last_reading() {
        // Once the blocks a page gave when read again were not counted, while the parity blocks
        // among them were.
        let mut first = complete_page(Vec::new());
        first.blocks.truncate(8);
        first.statistics = PageStatistics {
            good_blocks: 8,
            bad_blocks: 2,
            superblocks: 1,
            restored_bytes: 4,
            ..PageStatistics::default()
        };
        let mut again = complete_page(Vec::new());
        again.statistics = PageStatistics {
            good_blocks: 10,
            bad_blocks: 0,
            superblocks: 1,
            restored_bytes: 9,
            misplaced_blocks: 1,
            ..PageStatistics::default()
        };
        let mut assembler = Assembler::new();
        assembler.add_page(&first);
        assembler.add_more_blocks(&again, &first.statistics);
        let report = assembler.finish(None).unwrap().report;
        assert_eq!(
            (
                report.pages_read,
                report.good_blocks,
                report.bad_blocks,
                report.restored_bytes,
                report.misplaced_blocks
            ),
            (1, 11, 0, 9, 1)
        );
    }

    fn complete_page(records: Vec<Record>) -> ScannedPage {
        let blocks = (0..10)
            .map(|i| ScannedBlock::Data {
                offset: (i * DATA_LEN) as u32,
                payload: payload(i as u8),
            })
            .collect();
        ScannedPage {
            records,
            ..page_with(blocks, 0)
        }
    }

    fn hash_of_the_file() -> HashRecord {
        let file: Vec<u8> = (0..10).flat_map(|i| payload(i as u8)).collect();
        HashRecord {
            digest: pbx::file_hash(&file),
            original_size: 10 * DATA_LEN as u32,
            keyed: false,
        }
    }

    #[test]
    fn a_file_with_the_hash_on_the_pages_is_verified() {
        let mut assembler = Assembler::new();
        assembler.add_page(&complete_page(vec![Record::Hash(hash_of_the_file())]));
        let restored = assembler.finish(None).unwrap();
        assert_eq!(restored.report.integrity, Integrity::Verified);
    }

    #[test]
    fn a_file_that_differs_from_the_hash_on_the_pages_is_refused() {
        let mut wrong = hash_of_the_file();
        wrong.digest[0] ^= 1;
        let mut assembler = Assembler::new();
        assembler.add_page(&complete_page(vec![Record::Hash(wrong)]));
        assert!(matches!(assembler.finish(None), Err(Error::HashMismatch)));
    }

    #[test]
    fn a_hash_of_another_file_is_not_taken_for_this_one() {
        let mut other = hash_of_the_file();
        other.original_size += 1;
        other.digest[0] ^= 1;
        let mut assembler = Assembler::new();
        assembler.add_page(&complete_page(vec![Record::Hash(other)]));
        let restored = assembler.finish(None).unwrap();
        assert_eq!(restored.report.integrity, Integrity::Unchecked);
    }

    #[test]
    fn a_hash_found_when_a_page_is_read_again_checks_the_file() {
        // The first reading lacks the last block and the hash; reading the page again with more
        // effort finds both.
        let mut first = complete_page(Vec::new());
        first.blocks.pop();
        first.label.mode = Mode(Mode::EXTENDED);
        let mut again = complete_page(vec![Record::Hash(hash_of_the_file())]);
        again.label.mode = Mode(Mode::EXTENDED);
        let mut assembler = Assembler::new();
        assembler.add_page(&first);
        assert!(!assembler.is_complete());
        assembler.add_more_blocks(&again, &first.statistics);
        let restored = assembler.finish(None).unwrap();
        assert_eq!(restored.report.integrity, Integrity::Verified);
        assert_eq!(
            restored.report.pages_read, 1,
            "the page is not counted twice"
        );
    }

    #[test]
    fn pages_that_announce_a_hash_but_show_none_are_reported() {
        let mut page = complete_page(Vec::new());
        page.label.mode = Mode(Mode::EXTENDED);
        let mut assembler = Assembler::new();
        assembler.add_page(&page);
        let restored = assembler.finish(None).unwrap();
        assert_eq!(restored.report.integrity, Integrity::Missing);
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
