// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider
// Derived from PaperBack 1.10, Copyright (c) 2007 Oleh Yuschuk; see NOTICE.md.

//! Checks the Reed-Solomon code and block layout against vectors produced by the
//! original C implementation (see `tests/fixtures/original_blocks.txt`).

use crate::block::{DATA_LEN, RawBlock};

struct Case {
    block: Vec<u8>,
    damaged: Vec<u8>,
    corrected: i32,
    fixed: Vec<u8>,
    wrecked: Vec<u8>,
    wreck_result: i32,
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len() / 2)
        .map(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

fn load_cases() -> Vec<Case> {
    let text = include_str!("../tests/fixtures/original_blocks.txt");
    let value = |line: &str| line.split_whitespace().nth(1).unwrap().to_owned();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines
        .chunks(7)
        .map(|c| Case {
            block: unhex(&value(c[0])),
            damaged: unhex(&value(c[1])),
            corrected: value(c[2]).parse().unwrap(),
            fixed: unhex(&value(c[3])),
            wrecked: unhex(&value(c[4])),
            wreck_result: value(c[5]).parse().unwrap(),
        })
        .collect()
}

#[test]
fn sealed_blocks_match_the_original_bit_for_bit() {
    for (seed, case) in load_cases().iter().enumerate() {
        let address = match seed {
            0 => 0,
            1 => 450,
            _ => 0xFFFF_FFFF,
        };
        let payload: [u8; DATA_LEN] = std::array::from_fn(|i| (i * 7 + seed * 13 + 3) as u8);
        let mut block = RawBlock::new(address, &payload);
        block.seal();
        assert_eq!(block.0.to_vec(), case.block, "case {seed}");
    }
}

#[test]
fn error_correction_agrees_with_the_original() {
    for (seed, case) in load_cases().iter().enumerate() {
        let mut damaged = RawBlock(case.damaged.clone().try_into().unwrap());
        assert_eq!(
            damaged.correct().map_or(-1, |n| n as i32),
            case.corrected,
            "case {seed}"
        );
        assert_eq!(damaged.0.to_vec(), case.fixed, "case {seed}");

        let mut wrecked = RawBlock(case.wrecked.clone().try_into().unwrap());
        assert_eq!(
            wrecked.correct().map_or(-1, |n| n as i32),
            case.wreck_result,
            "case {seed}"
        );
    }
}
