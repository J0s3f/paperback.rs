// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 Josef H.B. Schneider
// Derived from PaperBack 1.10, Copyright (c) 2007 Oleh Yuschuk, parts copyright (c) 2013 Michael Mohr; see NOTICE.md.

//! Compression and encryption of the payload, as done by PaperBack 1.10.

use std::io::{Read, Write};

use aes::Aes192;
use cbc::cipher::block_padding::NoPadding;
use cbc::cipher::{BlockModeDecrypt, BlockModeEncrypt, KeyIvInit};
use sha2::Sha256;

use crate::error::{Error, Result};

/// Payload sizes are multiples of the AES block size.
pub(crate) const ALIGNMENT: usize = 16;
pub(crate) const SALT_LEN: usize = 16;
pub(crate) const IV_LEN: usize = 16;
/// Bytes of the name field that hold the salt followed by the IV.
pub(crate) const SALT_AND_IV_LEN: usize = SALT_LEN + IV_LEN;
pub(crate) const MAX_PASSWORD_LEN: usize = 32;

const KEY_LEN: usize = 24;
const MAX_PREALLOCATION: usize = 1 << 26;
const LEGACY_KEY_LEN: usize = 32;
const KEY_STRETCHING_ROUNDS: u32 = 524_288;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// How strongly the data is compressed before it is printed.
#[non_exhaustive]
pub enum Compression {
    /// Store the data as it is.
    None,
    /// bzip2 with the smallest block size: quick, weaker.
    Fast,
    /// bzip2 with the largest block size: slow, strongest.
    Maximal,
}

impl Compression {
    fn bzip2_level(self) -> Option<u32> {
        match self {
            Self::None => None,
            Self::Fast => Some(1),
            Self::Maximal => Some(9),
        }
    }
}

/// The password as the original program sees it: one byte per character in the
/// Windows ANSI code page. Characters up to U+00FF map to the same byte there
/// (Latin-1), which covers western languages; anything else falls back to UTF-8.
pub(crate) fn password_bytes(password: &str) -> Vec<u8> {
    match password
        .chars()
        .map(u8::try_from)
        .collect::<std::result::Result<Vec<u8>, _>>()
    {
        Ok(bytes) => bytes,
        Err(_) => password.as_bytes().to_vec(),
    }
}

pub(crate) fn align_up(len: usize) -> usize {
    len.div_ceil(ALIGNMENT) * ALIGNMENT
}

/// Compresses `data`; returns `None` when compression is off or does not pay
/// off (the output would not be smaller than the aligned input).
pub(crate) fn compress(data: &[u8], compression: Compression) -> Result<Option<Vec<u8>>> {
    let Some(level) = compression.bzip2_level() else {
        return Ok(None);
    };
    let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::new(level));
    encoder.write_all(data).map_err(Error::Compress)?;
    let packed = encoder.finish().map_err(Error::Compress)?;
    Ok((packed.len() < align_up(data.len())).then_some(packed))
}

/// Unpacks bzip2 data; trailing alignment bytes are ignored.
pub(crate) fn decompress(packed: &[u8], expected_len: usize) -> Result<Vec<u8>> {
    // The size comes from the page label; do not trust it for the up-front allocation.
    let mut unpacked = Vec::with_capacity(expected_len.min(MAX_PREALLOCATION));
    bzip2::read::BzDecoder::new(packed)
        .take(expected_len as u64)
        .read_to_end(&mut unpacked)
        .map_err(Error::Decompress)?;
    Ok(unpacked)
}

pub(crate) fn random_salt_and_iv() -> Result<[u8; SALT_AND_IV_LEN]> {
    let mut bytes = [0u8; SALT_AND_IV_LEN];
    getrandom::fill(&mut bytes).map_err(|e| Error::Random(e.to_string()))?;
    Ok(bytes)
}

/// Bytes of the key stretching beyond the cipher key; the key of the check value comes from the
/// second block of the PBKDF2 output, so the cipher key is the same as PaperBack 1.10 derives.
const MAC_KEY_OFFSET: usize = 32;
const MAC_KEY_LEN: usize = 32;

fn stretch(password: &str, salt: &[u8]) -> [u8; MAC_KEY_OFFSET + MAC_KEY_LEN] {
    let mut keys = [0u8; MAC_KEY_OFFSET + MAC_KEY_LEN];
    pbkdf2::pbkdf2_hmac::<Sha256>(
        &password_bytes(password),
        salt,
        KEY_STRETCHING_ROUNDS,
        &mut keys,
    );
    keys
}

fn derive_key(password: &str, salt: &[u8]) -> [u8; KEY_LEN] {
    let mut key = [0u8; KEY_LEN];
    key.copy_from_slice(&stretch(password, salt)[..KEY_LEN]);
    key
}

/// A check value of `plain` that only the password can reproduce: HMAC-SHA256 under a key
/// stretched from the password and the salt. A plain hash of the plaintext would let anyone
/// who sees the page confirm a guess of the contents.
pub(crate) fn authenticate(
    plain: &[u8],
    password: &str,
    salt_and_iv: &[u8; SALT_AND_IV_LEN],
) -> [u8; MAC_KEY_LEN] {
    use hmac::{KeyInit, Mac};
    let keys = stretch(password, &salt_and_iv[..SALT_LEN]);
    // A key of any length is accepted by HMAC.
    let mut mac = hmac::Hmac::<Sha256>::new_from_slice(&keys[MAC_KEY_OFFSET..])
        .unwrap_or_else(|_| unreachable!("HMAC takes keys of any length"));
    mac.update(plain);
    mac.finalize().into_bytes().into()
}

/// AES-192-CBC without padding; `salt_and_iv` is the 32 bytes stored in the
/// name field of the superblock.
pub(crate) fn encrypt(
    data: &mut [u8],
    password: &str,
    salt_and_iv: &[u8; SALT_AND_IV_LEN],
) -> Result<()> {
    let (salt, iv) = salt_and_iv.split_at(SALT_LEN);
    let key = derive_key(password, salt);
    let cipher = cbc::Encryptor::<Aes192>::new_from_slices(&key, iv).map_err(|_| Error::Cipher)?;
    cipher
        .encrypt_padded::<NoPadding>(data, data.len())
        .map_err(|_| Error::Cipher)?;
    Ok(())
}

pub(crate) fn decrypt(data: &mut [u8], password: &str, salt_and_iv: &[u8]) -> Result<()> {
    let (salt, iv) = salt_and_iv.split_at(SALT_LEN);
    let key = derive_key(password, salt);
    let cipher = cbc::Decryptor::<Aes192>::new_from_slices(&key, iv).map_err(|_| Error::Cipher)?;
    cipher
        .decrypt_padded::<NoPadding>(data)
        .map_err(|_| Error::Cipher)?;
    Ok(())
}

/// Decrypts data written by PaperBack 1.00: AES-256 in ECB mode, keyed
/// directly with the zero-padded password.
///
/// Reading only. The scheme leaks structure and has a tiny effective key space,
/// so this program never writes it.
pub(crate) fn decrypt_legacy(data: &mut [u8], password: &str) -> Result<()> {
    if !data.len().is_multiple_of(ALIGNMENT) {
        return Err(Error::Cipher);
    }
    let mut key = [0u8; LEGACY_KEY_LEN];
    let text = password_bytes(password);
    let used = text.len().min(LEGACY_KEY_LEN);
    key[..used].copy_from_slice(&text[..used]);
    decrypt_ecb(data, &key)
}

fn decrypt_ecb(data: &mut [u8], key: &[u8; LEGACY_KEY_LEN]) -> Result<()> {
    use aes::cipher::{BlockCipherDecrypt, KeyInit};
    let cipher = aes::Aes256::new_from_slice(key).map_err(|_| Error::Cipher)?;
    for block in data.as_chunks_mut::<ALIGNMENT>().0 {
        cipher.decrypt_block(block.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latin_one_characters_become_single_bytes() {
        assert_eq!(password_bytes("pässwort"), b"p\xe4sswort");
        assert_eq!(password_bytes("€uro"), "€uro".as_bytes());
    }

    #[test]
    fn alignment_rounds_up_to_sixteen() {
        assert_eq!(align_up(0), 0);
        assert_eq!(align_up(1), 16);
        assert_eq!(align_up(16), 16);
        assert_eq!(align_up(17), 32);
    }

    #[test]
    fn compression_round_trips_and_ignores_padding() {
        let text = b"paper paper paper paper paper paper paper paper".repeat(20);
        let mut packed = compress(&text, Compression::Maximal).unwrap().unwrap();
        packed.resize(align_up(packed.len()), 0);
        assert_eq!(decompress(&packed, text.len()).unwrap(), text);
    }

    #[test]
    fn incompressible_data_is_stored_raw() {
        assert!(compress(b"x", Compression::Fast).unwrap().is_none());
        assert!(compress(b"anything", Compression::None).unwrap().is_none());
    }

    #[test]
    fn ecb_decryption_matches_the_fips_197_vector() {
        let key: [u8; 32] = std::array::from_fn(|i| i as u8);
        let mut block = [
            0x8e, 0xa2, 0xb7, 0xca, 0x51, 0x67, 0x45, 0xbf, 0xea, 0xfc, 0x49, 0x90, 0x4b, 0x49,
            0x60, 0x89,
        ];
        decrypt_ecb(&mut block, &key).unwrap();
        let expected: [u8; 16] = std::array::from_fn(|i| (i * 0x11) as u8);
        assert_eq!(block, expected);
    }

    #[test]
    fn legacy_decryption_pads_the_password_with_zeros() {
        use aes::cipher::{BlockCipherEncrypt, KeyInit};
        let mut key = [0u8; 32];
        key[..6].copy_from_slice(b"secret");
        let cipher = aes::Aes256::new_from_slice(&key).unwrap();
        let plain = *b"sixteen byte msg";
        let mut data = plain;
        cipher.encrypt_block((&mut data).into());
        assert_ne!(data, plain);
        decrypt_legacy(&mut data, "secret").unwrap();
        assert_eq!(data, plain);
    }

    #[test]
    fn the_check_value_depends_on_the_password_the_salt_and_the_data() {
        let salt_and_iv = [9u8; SALT_AND_IV_LEN];
        let value = authenticate(b"data", "correct horse", &salt_and_iv);
        assert_eq!(value, authenticate(b"data", "correct horse", &salt_and_iv));
        assert_ne!(value, authenticate(b"data", "battery staple", &salt_and_iv));
        assert_ne!(value, authenticate(b"datb", "correct horse", &salt_and_iv));
        assert_ne!(
            value,
            authenticate(b"data", "correct horse", &[8u8; SALT_AND_IV_LEN])
        );
    }

    #[test]
    fn the_cipher_key_is_the_first_block_of_the_stretching_alone() {
        // PaperBack 1.10 asks PBKDF2 for 24 bytes; asking for more must not change them.
        let mut short = [0u8; KEY_LEN];
        pbkdf2::pbkdf2_hmac::<Sha256>(b"pw", &[1u8; SALT_LEN], KEY_STRETCHING_ROUNDS, &mut short);
        assert_eq!(derive_key("pw", &[1u8; SALT_LEN]), short);
    }

    #[test]
    fn encryption_round_trips_with_the_right_password_only() {
        let salt_and_iv = [9u8; SALT_AND_IV_LEN];
        let plain = b"0123456789abcdef0123456789abcdef".to_vec();
        let mut data = plain.clone();
        encrypt(&mut data, "correct horse", &salt_and_iv).unwrap();
        assert_ne!(data, plain);

        let mut right = data.clone();
        decrypt(&mut right, "correct horse", &salt_and_iv).unwrap();
        assert_eq!(right, plain);

        let mut wrong = data;
        decrypt(&mut wrong, "battery staple", &salt_and_iv).unwrap();
        assert_ne!(wrong, plain);
    }
}
