# Design decisions

## Bit-for-bit compatibility with PaperBack 1.10 (and reading 1.00)

Pages written by the original must decode here and the other way round. This fixes the format, the
Reed-Solomon variant, the CRC mask, the placement of blocks, the XOR mask and the password handling.
Considered: a cleaner new format. Rejected because existing backups must stay readable.
Verified by `tools/interop` against the original executables, and by `tests/reference_vectors.rs`
(vectors produced by compiling the original C code).

## Decoder is a close port of the original

Grid detection (`decode/grid.rs`, `decode/peaks.rs`) and block reading (`decode/reader.rs`) follow
`Decoder.cpp`, including its quirks (for example the transposed neighbour indices in the overlap
correction). Reason: the same scans must be accepted; the original's thresholds were tuned on real
scans. Improvements (adaptive thresholds, perspective correction) are possible later and must be
measured against the port. Known limit: some tilts alias on noise-free synthetic bitmaps; the original
fails on the same images.

## Name

The project is called paperback.rs, not PaperBack, so it is never mistaken for the original program (which stays `"PaperBack`"
in all texts). The crates are `paperback-rs` (library, imported as `paperback_rs`) and `paperback-rs-cli` (the command line,
binary `paperback-rs`). Rust crate names cannot contain a dot, hence the dash. Pages carry the name `paperback.rs` as the
default title, and the hint line says `paperback-rs decode`.

## Library plus CLI crate

`paperback-rs` (library) has no I/O policy beyond image decoding and encoding; `paperback-rs-cli` provides
`encode` and `decode` as filters. Other programs can reuse the library. Rust has no reverse-DNS package
names, so the name `at.j0s.paperback` is not used; the domain appears as homepage only.

## Encryption is for compatibility, not a recommendation

The crypto code is the original's scheme built from RustCrypto crates and is not audited; it has no authentication
beyond a 16-bit checksum. It exists so that old encrypted pages stay readable and new ones can be read by the original.
The README recommends encrypting with an audited tool first (age, Picocrypt and others, with their audit status as
found in October 2026) and using paperback.rs only to encode the result.

## Dependencies (stable, widely used crates first)

| Crate | Why | Considered |
|---|---|---|
| `crc` | CRC-16/XMODEM, same polynomial as the original table | hand table (more code) |
| `bzip2` | compression (original uses libbz2) | `bzip2-rs` decode only |
| `aes`, `cbc`, `pbkdf2`, `hmac`, `sha2` | RustCrypto implementations of the 1.10 scheme | own AES (no) |
| `getrandom` | salt and IV | `rand` (bigger) |
| `image` (png, jpeg, bmp only) | reading scans | `png` + `jpeg-decoder` separately |
| `png` | writing PNG with resolution metadata (the `image` encoder omits it) | none |
| `pdf-writer`, `miniz_oxide` | writing PDF (one flate image per page) | `printpdf` (heavy) |
| `font8x8` | bitmap font for the page title and hint | a TrueType font plus `ab_glyph` (font licensing, more code) |
| `fax`, `hayro-jbig2`, `hayro-jpeg2000` | PDF image filters, see below | `pdfium`, OpenJPEG bindings |
| `lopdf` (no default features) | extracting embedded images from PDFs | `pdfium` (native library, breaks single-binary use) |
| `clap`, `serde_json` | command line, `--json` summary | hand parsing |
| `thiserror` | library errors | manual impls |

Hand-written because no crate fits: Reed-Solomon with the original's parameters
(`reed_solomon.rs`, derived from Phil Karn's code), the BMP writer (the original requires a 40-byte
header and an 8-bit palette), and the grid decoder.

## PDF input reads embedded images only

Scanner PDFs and our own PDFs contain one raster per page. Rendering arbitrary vector PDFs needs a
large native engine. Images with an unsupported filter are rejected with a clear message; the supported
filters are listed under "PDF image filters" below.

## Encryption: write 1.10 only, read 1.00 too

1.00 used AES-256-ECB keyed with the zero-padded password, which leaks structure and has a tiny
effective key space. Decryption is kept so old backups remain recoverable; it is never written. The
decoder tries the 1.10 scheme first and falls back to 1.00; the CRC of the plain data tells which
one is right, so a wrong password fails both.

## Passwords are bytes in the Windows ANSI code page

The original passes one byte per character. Characters up to U+00FF map to the same byte (matches
Windows-1252 for western languages); other characters fall back to UTF-8 bytes. Verified with an
umlaut password against both original versions.

## No interactive prompts

Passwords come from `--password-file` or `--password-env`. A prompt would break pipelines and tests.
Considered: reading from the terminal when attached; postponed.

## Last page and page count

Pages are counted from the aligned data size. The original counted from the unaligned size, which
loses the last bytes in an edge case. The last page shrinks to the rows needed (at least three), as
in the original.

## Title and hint text

Like the original, each page gets a title above and a scanning hint below the grid. Text is drawn with the
8x8 bitmap font of the `font8x8` crate, enlarged by an integer factor to about 1/6 and 1/10 inch, so there is no
font file to ship or license. The hint also says how to restore with this tool. The text bands are part of the page
image; the decoder finds the grid regardless (tested). The original draws the text only on paper; here it is part
of every image because the images are meant to be printed.

## Deskewing instead of a wider tilt search

The original's grid search covers about 5 degrees. Widening it would need a better skew model, so the page is
straightened before reading instead: the angle is estimated from the projection of the dark pixels on a disc
in the middle of the page (the sharpest projection wins), the page is rotated once and read again. Pages that read
completely as they are are never rotated, and blocks from several attempts are combined. A disc is used because a
square window always projects sharpest when upright. Considered: a Hough transform (slower, no benefit),
widening MAX_TILT (aliasing and shear error grow with angle).

## PDF image filters

| Filter | Crate | Note |
|---|---|---|
| CCITT group 3/4 | `fax` (pdf-rs, about 60 million downloads) | mixed coding (K > 0) is reported as unsupported |
| JBIG2 | `hayro-jbig2` | pure Rust; alternative: a C binding |
| JPEG 2000 | `hayro-jpeg2000` | pure Rust; alternative `jpeg2k` (OpenJPEG, needs a C toolchain) |

The two `hayro` crates come from a single author's PDF stack (3 to 4 million downloads each); they are the only
pure-Rust decoders with real use. They require Rust 1.92, hence the workspace 
`rust-version`. If a native
dependency becomes acceptable, `jpeg2k` is the more widely used JPEG 2000 option.

## Fixtures from the original programs

`tests/fixtures/original` holds pages written by the real 1.00 and 1.10 executables from neutral input with a fixed
timestamp (see its README), so CI proves compatibility without the executables. The interop scripts in
`tools/interop` stay a manual, Windows-only check.

## Untrusted input

Scans and PDFs come from outside, so the code has no `unsafe` (both crates `forbid(unsafe_code)`) and treats every size
it reads as hostile: pictures are limited to about 540 megapixels (`raster::MAX_PIXELS`), PDF images must announce a
sane size and a bit depth of 1, 2, 4, 8 or 16, fax widths are capped, decoder output is capped at the announced size,
labels with impossible sizes are ignored, settings that would need absurd memory are refused, and the pre-allocation
for decompression never trusts the label. `tests/hostile_input.rs` feeds random, truncated, bit-flipped and
oversized input and expects errors, never panics. Dependencies are checked for known advisories in CI.

Both crates deny `clippy::unwrap_used` and `clippy::expect_used` outside tests, so a new unwrap fails CI. Invariants are
expressed in types instead (for example `PageNumbering` is parsed once, so naming a page cannot fail).

## Lint policy and public API

Lints live in the workspace manifest (`[workspace.lints]`): `unsafe_code` is forbidden, `unwrap`/`expect` are denied outside tests
(`clippy.toml`), documentation and `Debug` are required for public items, and clippy's `pedantic` group is on. Casts between
integer and float types are allowed because pixel arithmetic is bounded by the checked picture sizes. The library exposes only
what a user needs (`encode`, `decode`, `imageio`, `raster`, `layout` settings, `Compression`, `Redundancy`, `FileTime`,
`Error`); block layout, Reed-Solomon, CRC and page text are crate-private, so they can change without breaking users.
`Error`, `Compression` and `Paper` are `#[non_exhaustive]`.

## Quality pictures

The decoder records for every grid cell whether the block was absent, unreadable or read after N corrected bytes and
draws it by mapping each pixel back to its cell through the fitted grid (including tilt), so no geometry is duplicated.
When several attempts (straightening) were needed, the attempt that read the most blocks is shown, on the picture that
attempt used. Missing cells between found blocks are drawn as unreadable: a block too damaged to show grid lines is
indistinguishable from empty margin otherwise. Colour is a plain HSV ramp, no extra crate; output is PNG only.

## Crumpled and high-resolution scans

Three measures, found by testing a crumpled inkjet print (scanned at 1200 dpi):

- **Reduction.** The reader samples each dot with a window of a few pixels, so it needs a dot pitch of about 3
  pixels. A scan with a larger pitch is averaged down by a whole factor first (`reduction_factor`). The crumpled page
  read 282 of 418 blocks at full size and 385 reduced by four. The quality pictures then show the reduced page.
- **Following the paper.** The first grid fit is one straight, tilted grid. Every block that reads records how far it
  sat from that grid; blocks that do not read are tried again at the position their neighbours show (their average,
  then each one), with the block's own grid lines predicted when they are too faint to find, and unreadable
  blocks are retried in rounds so a crease is followed inwards from its readable edge. The quality pictures use the
  same shifts, so the colours lie on the blocks.
- **Local fixes.** The sharpening level is chosen per page by reading a sample of 25 blocks at five levels, and a block
  that does not read is cut out again at the other levels (paper away from the glass is blurred only there) and with a black/white limit per quarter block (creases cast shadows).

The result is 414 of 418 blocks on the crumpled page, and the file is restored. All of it is ordinary image
processing written for this project; the scan-cleaning tools unpaper (GPL-2.0-only, not compatible with GPL-3.0-or-later)
and a demo without a licence were looked at for ideas only and no code was taken.

A page counts as complete as soon as every data block is read or can be rebuilt from its group's recovery block;
only incomplete pages are rotated and read again, which keeps good scans fast.

Photographs of crumpled paper turn neighbouring blocks by different amounts, so a block that does not read is also tried with
the local rotation of its neighbours and a few small steps around it. That roughly doubles the blocks read on a phone photo of a
crumpled sheet but is not enough for a full restore; a warp from the printed grid lines (one quadrilateral per block) would be
the next step.
