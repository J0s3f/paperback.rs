# On-paper format

This documents what the code implements; it matches PaperBack 1.10. Multi-byte integers are
little-endian.

## Blocks

A page is a grid of cells. Each cell holds one block of 32 x 32 dots (1024 bits = 128 bytes).

```
data block (128 bytes)           superblock (128 bytes)
 0  u32  address                  0  u32  0xFFFFFFFF
 4  90 B payload                  4  u32  data size (aligned to 16)
94  u16  CRC                      8  u32  bytes per full page
96  32 B Reed-Solomon parity     12  u32  original size
                                 16  u8   mode (1 compressed, 2 encrypted)
                                 17  u8   file attributes
                                 18  u16  page number (1-based)
                                 20  u64  modification time (FILETIME)
                                 28  u16  CRC of the unencrypted data
                                 30  64 B name (31 characters; bytes 32..48 salt, 48..64 IV when encrypted)
                                 94  u16  CRC
                                 96  32 B parity
```

- CRC: CRC-16/XMODEM over bytes 0..94, XOR 0x55AA.
- Parity: Reed-Solomon (255,223) over GF(2^8), polynomial 0x187, first root alpha^112,
  primitive element alpha^11, shortened by 127 bytes (96 message bytes).
- Dots: row `j` is the little-endian u32 at offset `4j`, bit 0 being the leftmost dot, XORed with
  0x55555555 on even rows and 0xAAAAAAAA on odd rows (so empty blocks are not solid).
- Addresses: data blocks hold the byte offset in the (compressed, encrypted) stream. A recovery
  block holds `offset ^ (group_size << 28)` and 90 bytes of `0xFF` XOR all payloads of its group.

## Page

Cells are `35 x dot pitch` pixels wide: a 32-dot block plus two dots before and one after a black grid
line. Rows run left to right. A group of `r` data strings plus one recovery string (each string is a
superblock followed by one block per group) is laid out so that members of one group are never in
the same column; remaining cells repeat the superblock. Details: `encode.rs`, `place_blocks`.

## Data stream

file -> bzip2 (optional; kept only if smaller) -> zero padding to 16 bytes -> CRC-16 stored in the
superblock -> AES-192-CBC (optional) -> split into 90-byte blocks.

Key: PBKDF2-HMAC-SHA256, password as one byte per character, 16-byte salt, 524288 rounds, 24 bytes.
The IV is stored next to the salt. PaperBack 1.00 instead used AES-256-ECB with the password padded
with zeros as key (decrypt-only here).
