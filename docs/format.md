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

### Parity blocks and filled pages (paperback.rs only)

Without parity and `--fill-pages` the pages are filled one after the other, as by the originals, and the last page
shrinks to the rows it needs. With parity blocks, or with `--fill-pages`, the file is spread evenly: with `p` pages and
`T` groups, every page holds `ceil(T / p)` groups (the label's page size is that many groups in bytes, which keeps
groups whole, as the originals expect), and `p` is the fewest pages that also have room for the parity asked for.

Behind the strings come the **parity blocks** of the page, a run of consecutive indices; every 8th of these cells is a
copy of the label instead, in case label blocks do not read. The runs are shared out evenly over the pages. With
`--fill-pages` every page uses all its rows and every free cell holds a parity block, so the file has as many parity
blocks as fit; otherwise the remaining cells repeat the label, as in the originals.

A page without PaperBack's recovery blocks (`--redundancy 0`) has six data strings and no recovery string.

**Parity block addresses** lie between those of data blocks: parity block `k` has the address
`90 * (k / 89) + k % 89 + 1`, which is never a multiple of 90. Both originals accept a data block only if its address is
a multiple of 90 below the end of the data (`Addblock` in `Fileproc.cpp`), so they drop parity blocks without effect,
whatever the size of the file. This needs no room behind the end of the data, unlike the records, gives 89 parity
addresses per data block, and cannot be mistaken for a data block, a recovery block or a record.

**The code** (`parity.rs`): a block is 45 symbols of 16 bits (little endian), over GF(2^16) with the polynomial
`x^16 + x^12 + x^3 + x + 1` (`0x1100B`) and generator 2, as in PAR2. The data blocks are cut into `s = ceil(n / 16384)`
interleaved stripes (`n` data blocks): data block `i` is column `j = i / s` of stripe `i % s`, parity block `k` is row
`r = k / s` of stripe `k % s`. Row `r` of a stripe is the sum over its columns of `d_j / ((0xFFFF - r) XOR j)`, a Cauchy
matrix: any `m` parity blocks of a stripe rebuild any `m` missing data blocks of it. Every stripe spans all pages, so a
lost page costs every stripe the same share. Rows and columns must be distinct elements of the field, so a stripe of
`c` columns has at most `65536 - c` rows: at least three per column for a full stripe of 16384. The last data block is padded with zeros. A reader alternates PaperBack's
recovery blocks and the parity blocks as long as either rebuilds blocks.

The rows and the missing columns form a Cauchy matrix `1 / (x_i + y_j)` (`x = 0xFFFF - r`, `y = j`), whose inverse has a
closed form: with `A(t)` the product of `t + x_i` over the rows used and `B(t)` that of `t + y_j` over the missing
columns, missing column `j` is the sum over the rows of `B(x_i) A(y_j) / ((x_i + y_j) A'(x_i) B'(y_j))` times what row
`i` holds less the known columns. A reader uses the lowest rows it has; if it read one row more, it checks the rebuilt
blocks against it and rebuilds nothing from that stripe if they do not fit, since one of the parity blocks is then not
what its address says. It takes no parity block beyond the number the sheet records give for the file, and neither copy
of a parity block read twice with different contents.

The sheet record says where the parity blocks of the page are (see "Extension records"). A reader needs no record to
use parity blocks, only to check that they were read in the right cell and that the file has them.

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
| 1, sheet | 0 | sheet identifier (16 bytes), page, page count, columns, rows (each 2 bytes, little endian), group size (1 byte, 0 without recovery blocks), first parity block of the page, parity blocks on the page, parity blocks of the file (each 4 bytes) |
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
