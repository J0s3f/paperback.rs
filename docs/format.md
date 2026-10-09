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
the same column; remaining cells repeat the superblock. Details: `plan.rs`.

## Data stream

file -> bzip2 (optional; kept only if smaller) -> zero padding to 16 bytes -> CRC-16 stored in the
superblock -> AES-192-CBC (optional) -> split into 90-byte blocks.

Key: PBKDF2-HMAC-SHA256, password as one byte per character, 16-byte salt, 524288 rounds, 24 bytes.
The IV is stored next to the salt. PaperBack 1.00 instead used AES-256-ECB with the password padded
with zeros as key (decrypt-only here).

## Extension records (PBX1, paperback.rs only)

paperback.rs adds information that PaperBack 1.00 and 1.10 do not know, in a way they ignore. Both originals accept a
data block only if its address is a multiple of 90 below the end of the data (`Addblock` in `Fileproc.cpp`) and drop
any other data block without effect. A **record** is a data block whose address lies *behind* the end of the data, at
`ceil(data size / 90) * 90 + 90 * n`. A page gives up six cells for them (a few blocks of capacity; the label of the
page says how many bytes a page holds, so the originals are not misled). The block checksum and the error correction
protect them like any block.

A record is 90 bytes: `PBX1`, a kind byte, a version byte (1), then the body, zero padded.

| Kind | Address `n` | Body |
|---|---|---|
| 1, sheet | 0 | sheet identifier (16 bytes), page, page count, columns, rows (each 2 bytes, little endian), group size (1 byte) |
| 2, hash | 1 | check value (32 bytes), size of the original file (4 bytes), keyed flag (1 byte) |
| 3, parity | 2 | the bytes after the header of the sheet record and of the hash record, added up without carry (XOR) |

The check value is the SHA-256 of the file as it was given, before compression and encryption, so it also vouches for the
decompression. For an **encrypted** file it is instead HMAC-SHA256 of the file under a key from the second block of the
PBKDF2 output (bytes 32 to 63; the cipher key, bytes 0 to 23, is the one PaperBack 1.10 derives), and the keyed flag is
1: a plain hash of the plaintext on paper would let anyone confirm a guess of the contents. The keyed value also detects a
wrong password, which the 16-bit checksum of the format misses once in 65536 tries.

The sheet identifier is the first 16 bytes of SHA-256 of `"PBX1 sheet"`, the SHA-256 of the data stream as it is on paper
(compressed and encrypted), the page number and the stored size: the same page of the same file always has the same
identifier, and nothing in it depends on the contents of an encrypted file.

The page carries the three records twice: in its first three cells and in its last three, which are at opposite corners of
the page. Any two of the three records give the third, so a stain, a crease or a torn corner that takes one end still
leaves the set, and a half-lost set is completed. The blocks of the data start after the first three cells; shifting all
of them by the same number keeps the blocks of one group in different columns. The mode byte of the label gets bit `0x04`; the originals look at bits `0x01`
(compressed) and `0x02` (encrypted) only, and a decoder that finds the bit but no hash says so.

Never usable: addresses with a non-zero high nibble (the originals take them for recovery blocks and may corrupt
the file), and anything appended to the data stream (it would reach bzip2 and the cipher).

With the layout of a page known, a decoder works out the block each cell must hold (`plan.rs`) and distrusts a block
read in another cell. See `docs/decisions.md`, "Extension records".
