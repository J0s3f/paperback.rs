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
beyond a 16-bit checksum (pages of paperback.rs 1.2 and later carry a keyed check value as well, see
"Extension records"; it protects only if the reader requires it with `--require-hash`). It exists so that old encrypted pages stay readable and new ones can be read by the original.
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
processing written for this project.

A page counts as complete as soon as every data block is read or can be rebuilt from its group's recovery block;
only incomplete pages are rotated and read again, which keeps good scans fast.

Photographs of crumpled paper turn neighbouring blocks by different amounts, so a block that does not read is also tried with
the local rotation of its neighbours and a few small steps around it. That roughly doubles the blocks read on a phone photo of a
crumpled sheet.

## Erasures, scan hints and black and white PDFs

- **Erasures.** For a block that does not read as it is, the reader marks the 16 (then 24) bytes whose dots were closest to
  the black/white limit as erasures and repairs the block with the Reed-Solomon decoder, which was extended with the
  erasure locator of Phil Karn's original routine. An erased byte costs one parity byte instead of two. At most 24 are
  used and the 16-bit checksum must still match, because with fewer parity bytes left over to check the result a wrong
  block would pass more easily. Pages on paper are unchanged, so compatibility is not affected. On the test scans it read
  4% to 8% more blocks of the borderline inkjet scans and about half as many again of one phone photo, and every restored
  file stayed identical.
- **Hints.** `decode/hints.rs` looks at pages that read badly (any unreadable block or more than four corrected bytes per
  block) and names likely causes. They are advice for people, never used by the reader, and computed only when `-v` or
  `--json` asks for them (`DecodeOptions::diagnose`). Idea from the scan diagnostics of the paperback-cli project; the
  checks are our own.
- **PDF output.** The encoder draws dots at gray 64, lines at 0 and text at 128, like the original's bitmaps. For PDF
  output only, pages are converted to pure black and white and stored with one bit per pixel. PNG and BMP pages are
  untouched.

## Following bent paper block by block (corner mesh)

A straight, tilted grid fits a flat scan. A crumpled page, or a photograph taken at an angle, needs more, so a block that
does not read as the grid says is read through its own corners (`decode/mesh.rs`):

- **Mesh.** Every corner where four blocks meet is found as the crossing of a dark horizontal and a dark vertical line,
  starting in the middle of the page and moving outwards. Each next corner is searched where the last steps (including
  the trend of their length, as in a perspective) predict it, and counts as found when its lines are clearly darker than
  the paper beside them, relative to the corner it was predicted from because lines fade towards the edge of a photo. The
  middle of every line between two corners is measured too.
- **Block.** A block is its four corners plus the middle of its four edges. Inside, the dots lie on a perspective
  projection of the corners, bent along the measured edges (a Coons patch), so a block that is foreshortened or curved is
  sampled where its dots really are. Like the original, nine slightly shifted samplings are tried and the sharpest
  sub-blocks combined.
- **Where the grid is found.** If fitting the grid on the whole picture does not lead to readable blocks (strong
  perspective bends the lines too much), it is fitted on the middle part of the picture instead and the mesh follows the rest.
- **Quality pictures** are drawn through the same corners, so the colours lie on the blocks of a bent page. Blocks whose
  lines were not found are not drawn as damaged: they are outside the page.
- **Measured** on test pages bent by a sine of 8 to 20 pixels (a block is 105 pixels): 8 pixels went from 54 to all 101
  blocks, 20 pixels from 10 to 57; foreshortening by 10% from 29 to all, by 15% from nothing to 95 and by 25% from nothing to
  73. On phone photos of a crumpled sheet 118 of 334 blocks became 292 and 151 became 280. Not enough for a full restore of
  those photos, but the pictures are a lot more useful for deciding what to scan again.

The mesh is built only when a block cannot be read otherwise, so pages that read cleanly pay nothing for it.

## Improving the reading of bad pages, step by step

Ideas from the literature on distorted lattices, 2D barcodes and page-oriented optical storage were tried one at a time on a
fixed set of 13 difficult pages (the pages in `tests/fixtures/synthetic` and `tests/fixtures/realworld/scans`, and two phone
photographs of a crumpled sheet). Blocks restored, of the 334 that two of the photos hold and of the 101 of the synthetic pages:

| Step | Photo A | Photo B | bent 16 | bent 20 | slant 15 | slant 25 | Verdict |
|---|---|---|---|---|---|---|---|
| start | 280 | 292 | 99 | 71 | 95 | 73 | |
| 1 detector learned from the page | 291 | 292 | 99 | 71 | 95 | 73 | kept; small. Raw dot errors on decoded blocks are about the same with a threshold (2.2%) and with the learned detector, so the errors are position and noise, not blur between neighbours |
| 3a cross-ratio prediction, several meshes | 301 | 305 | all | 72 | all | 97 | kept; the union of meshes made it never worse than one mesh |
| 3b surface fitted through corners | 300 | 310 | all | 79 | all | 92 | kept; better on average |
| passes of rising effort, blocks read once | 302 | 309 | all | 79 | all | 94 | kept; same results, 40% faster than before it |
| 2 checksum chooses the sampling per quadrant | 304 | 315 | all | 92 | all | all | kept; the largest gain on bent pages |
| 3c corners from fitted lines | 304 | 313 | all | 88 | all | 93 | dropped: dots sampled more exactly (fewer corrections) but fewer pages complete |
| 4 ladder of erasure counts | 304 | 315 | all | 92 | all | all | dropped: same results, slower |

Blocks are read in passes of rising effort (quick, normal, deep), each pass only visiting blocks that are still missing; a
picture that is read again after being turned skips every cell where a block was read before (mapped back to the original
picture), except cells read again as neighbours or as training for the detector because a step needs them. Everything
that was read is kept.

## Several pictures of one sheet

All pictures of a sheet show the same grid of cells, each numbered from its own corner and possibly turned or mirrored
(`decode/sheet.rs`). Blocks have no address before they are read, so a new picture is first looked at in a few cells
spread over its middle (read with normal effort); the addresses found there vote for which of the eight turns and mirrors,
and which shift, relate its grid to the earlier ones. If most agree, every cell whose block an earlier picture gave is
skipped, and what the picture reads is added to the sheet's map for the next one. A picture that shows no label block but
is found to show a known sheet takes that sheet's label. When the assembler holds the whole file, later pictures are not
read. Skipped cells still count as known places for the later attempts on the same picture (the rotated copies), and
neighbours that a retry needs are read like any other block. Not done: a map from cells to addresses without reading
(which would need the layout of the sheet, or a layout record on the page).

## Extension records

Question: can paperback.rs put more on its pages (a sheet identifier, a strong checksum) and still be read by PaperBack
1.00 and 1.10? Source for the answer: the C++ of both programs (`Decoder.cpp`, `Fileproc.cpp`, `Printer.cpp`), read for
what they validate, and then tested against the real executables (`tools/interop`).

What the originals check is little: the checksum of each block, and the label fields that identify a file (name, mode,
time, sizes). `Addblock` drops a data block whose address is not a multiple of 90 or lies at or behind the end of
the data, without any effect; the return value is not even looked at. That is the opening: **records are data blocks
behind the end of the data.** They take cells that would otherwise repeat the label, so the capacity of a page is
unchanged.

Chosen:

- **SHA-256 of the original file** in a record; for an encrypted file an HMAC-SHA256 under a key stretched from the
  password (a second PBKDF2 block, so the cipher key stays the one 1.10 derives). A plain hash of the plaintext
  on paper would be a way to test guesses of what the file says. The 16-bit file checksum of the format covers encrypted files only and
  the block checksums cannot tell a block from another file's block. The hash is computed before compression and
  encryption, so it also catches a wrong decompression. A mismatch is an error, not a warning: a file that fails
  its own hash is not what was backed up.
- **Sheet identifier and layout** (columns, rows, group size, page, page count) in a record. The identifier is derived
  from the hash of the data stream as it is on paper and the page, so printing a page again gives the same sheet; no randomness, and the output stays
  reproducible. The layout lets the decoder work out the block of every cell and check what it read (`positions.rs`).
- **Mode bit 0x04** marks the pages. The originals compare the mode between pages but test only bits 0 and 1.

- **Records far apart, with a parity record.** The first design put two copies of each record side by side in the spare
  cells at the end of the page, where one stain or crease takes all of them. Now the page keeps three cells at its start and
  three at its end for a full set, and a third record is the XOR of the other two, so any one of the three can be lost at
  either end. The records are not part of a recovery group of the page (they are not in the data stream); the costs of this
  are six cells per page and that capacity of a page in the label is a few blocks smaller than the grid allows.

- **The identifier in matching.** A picture whose first look (the middle cells) matches no known sheet is looked at in its
  four corners, where a record can be whatever the turn. A picture showing an identifier is never matched to a sheet with
  another one, and one showing the same identifier needs two agreeing blocks instead of three. The vote on block
  addresses stays the judge of how the grids relate; the identifier says only which sheet.

Rejected, with the reason:

- *A hash trailer inside the data stream.* The stream is read by bzip2 and the cipher; whether the original bzip2
  library tolerates trailing bytes is an assumption that could not be checked against its source.
- *Non-zero address nibbles.* The originals take them for recovery blocks: the last one read sets the group size of
  the whole page and its payload is XORed into free slots.
- *Per-block checks.* All 128 bytes of a block are used.
- *Marks on the frame or inside the dot grid.* The original finds the grid from it.
- *A second recovery block per group.* 90 bytes per group, and the originals gain nothing.

The position check (a block must be in the cell the layout gives it) protects against the one weakness of the
checksums: with many reading attempts per block and erasures that leave the error correction little to check with
(24 of 32 parity bytes), a damaged block can now and then pass as a different block. Estimated by calculation, not
observed; the check is cheap insurance. It needs the layout, so it
applies to pages written by this program; other pages are read as before. It only acts if most of the blocks of a picture
agree with the layout (the same vote as for combining pictures, `decode/sheet.rs`), so a picture that does not match
changes nothing.

Not done: a map of cells to addresses used to read *less* (skip blocks) or *more* (aim the effort at cells that must
hold data), a second
recovery layer.

Authenticated encryption, as far as it goes: the check value of an encrypted file is an HMAC-SHA256, so damage and a
wrong password are always caught when the value is on the pages. It cannot defend against someone who can change the pages:
the record, the mode bit and the label are not themselves authenticated, so a changed page set with the value taken off
reads as a page set of the original programs. `decode --require-hash` closes that: it refuses a file without a matching
value. It is an option because pages of the original programs, and pages made with `--no-extensions`, have none.

## Where the decoding time goes, and what was done about it

Decoding a difficult photograph takes minutes because the decoder tries many things for every block that does not read.
To see where the time goes there are two tools: `tools/profile.ps1` samples the program with the Windows Performance
Toolkit (administrator rights; `--features profile` stops the compiler from inlining the functions the report has to tell
apart), and `tools/timing.sh` decodes a fixed set of eight difficult pictures one after the other and prints the seconds
and the blocks restored, which is also the check that a change did not change what is read. A sampling profile of the first
state (eight pictures, 480 s of CPU) said:

| Where | Share |
|---|---|
| corner search of the mesh (`mesh::refine`, bilinear sampling along the arms of a cross) | 29% |
| Reed-Solomon decoding (`decode_with_erasures`) | 28% |
| cutting out, sharpening and reading dots of a block (`read_posed`) | 25% |
| choosing thresholds and orientations (`recognise`) | 6% |

Changes that give the same blocks on every picture of the set (eight pictures, seconds):

| Step | Seconds |
|---|---|
| start (version 1.2.1) | 450 |
| corner search: start with the centre and drop a place after its horizontal arm when it cannot win, no table of all scores; syndromes by table; stop when the locator polynomial has more roots than can be used; shifted samplings only for blocks that do not read from the first one; neighbour correction once per variant | 342 |
| cut a block out once for all sharpness levels (it was cut for each level); sharpening without a branch per pixel; roots of the locator polynomial by table rows; syndrome loop bounded by the locator's degree | 250 |
| corner search: places and interpolation weights of the arms worked out once per search (moving a cross by whole pixels keeps the weights); skip words that failed already | 212 |

Tried and dropped: a small search window first for the corners (it was about 10% faster but changed what was read, up
on some pictures and down on `bent20`, below its guard); the order of the results of the corner search is kept exactly,
including ties.

### Vector code for the error correction

`simd.rs` is the only module with `unsafe` code (the workspace lint is `deny`, the module allows it, and every block says why
it is sound; `undocumented_unsafe_blocks` is denied). It multiplies rows of bytes in GF(256) with the field polynomial of the
format (`dst ^= constant * src`) in four ways that give the same bytes: plain table lookups; `pshufb` with two 16-entry nibble
tables (SSSE3, 16 bytes at a time; AVX2, 32 bytes); and `gf2p8affineqb` with the 8 x 8 bit matrix of the constant (GFNI with
AVX2), which takes any field polynomial, unlike `gf2p8mulb`, which is fixed to the one of AES. The level is the best the processor
offers at the first use, found with `is_x86_feature_detected!`, and the kernel for it is stored once in a function pointer, so a
multiplication costs an indirect jump and no check of the processor. A vector kernel is handed out only after its feature was
seen, which is what makes the safe wrappers around the `unsafe` functions sound. `PAPERBACK_SIMD` asks for a lower level;
`decode -v` prints the one in use. Processors on which the cores differ (big.LITTLE) are fine as long as the instruction set the
program sees is the same on all cores, which is so for the Intel hybrid processors (AVX-512, where they differ, is not used) and
for ARM under Linux (it reports the common set); this was reasoned, not tried on such hardware, and ARM has no vector kernel here
anyway: it runs the plain code.

The Reed-Solomon decoder was rebuilt around it (`reed_solomon.rs`), and the original routine is kept in the tests as the
oracle: random words with errors and erasures are decoded by both, at every level the processor has, and must agree on the result
and on the corrected bytes.

- The syndromes come from the remainder of the word divided by the generator polynomial (the shift register of the encoder, with
  a table of multiples of the generator, on four 64-bit words), evaluated at the roots with table rows chosen by its bytes. This
  needs no vector instructions and runs for every processor: 1.8 microseconds became 0.3.
- Berlekamp-Massey keeps its polynomials as they are, not as logarithms, so that the update is one multiplication of a row by a
  constant; the search for the roots of the locator polynomial adds up rows of powers times its coefficients.
- A call on a word that is not a codeword (`cargo test --release -- --ignored --nocapture decoder_speed`): 5.4 microseconds
  before, 4.1 plain, 2.2 with SSSE3, 2.0 with AVX2. GFNI was tried on a second machine (AMD EPYC Genoa, which has it): the
  whole test suite passes with GFNI in use, and the same
  call took 4.8 microseconds plain, 2.8 with SSSE3, 2.6 with AVX2 and 2.6 with GFNI. So GFNI gives the same bytes but, on this
  processor, no measurable speed over AVX2.
  On the first machine, which lacks GFNI, the kernel is tested against a software model of the instruction (the bit matrices
  multiply as claimed for every constant). `cargo test -p paperback-rs --release --lib simd -- --nocapture` shows which levels
  were tried.
- Eight pictures, seconds: 212 before this step, 176 with AVX2, 197 with the plain code, 450 at the start of the speed work, all with
  the same blocks read.

What is left that vectors could do: the rest of the time is the corner search (bilinear samples; a different method would be
needed to vectorise it, which would change what is read), cutting out blocks and sharpening (plain loops the compiler vectorises
when the build is allowed AVX2: about 8% in an experiment, which the run-time choice cannot give them), and the discrepancy
step of Berlekamp-Massey (a dot product of two vectors, which would want `gf2p8mulb` and a change of basis to the field of AES).

### Further steps after the vector code

Profile after the vector code (eight pictures, 195 s of CPU): cutting a block out of the page 18%, the corner search 21%,
turning gray levels into bits 10.5%, Reed-Solomon 13%, sharpening 7%, averaging the dots 8%. Changes with the same blocks on
every picture:

- cutting out: the rise of each column, the pixel index and the row of the bitmap are worked out once instead of for every
  pixel; averaging a dot needs no check of its pixels when it lies well inside the buffer; the neighbour correction runs over
  rows with a white border added, without a test for the edge;
- turning gray levels into bits: the sums behind the limits are made once per variant, the orientations are looked up in
  tables instead of computed per dot, and how sure each byte is (needed only by the repair of the first reading) is worked
  out only then.

Eight pictures, seconds: 176 before, 167 after the cutting out, 162 after the bits. Tried and dropped: a 64 KB multiplication
table for the discrepancy of Berlekamp-Massey (2.2 microseconds per call against 2.05 with logarithms: the table costs more in
cache misses than it saves in lookups).

What is left is mostly the corner search and the blocks themselves, and one thing they share: everything runs on one thread.
The reads of the cells of a page are independent in principle, but the reader learns as it goes (the orientation, the variant that
worked last, the shifts of the neighbours that later reads start from), so reading them in parallel would change which variant
succeeds first in marginal cases and with it the blocks read; the search windows of the corner search are independent and could
be spread over threads with the same results.

### Caches of what failed

Counting showed how much of the work repeats: about half of the words handed to the Reed-Solomon decoder had been tried before
(13 to 18% within one reading of a block, a third more from other cuts of the same block), and about a quarter of the sampled
grids that reach the threshold reading at the normal or deep effort were identical to a grid that had failed already. Cuts of a
block that is read again and again, with shifts of a fraction of a pixel, come out the same. The reader now keeps a 64-bit
fingerprint of each grid that no variant could read and of each word that did not decode, and skips what it has seen fail. This
is exact: the threshold reading is a function of the grid and the effort (at the quick effort the variants tried depend on the one
that worked last, so grids are remembered only at the normal and deep effort, apart from each other), and decoding is a function
of the word. The fingerprint is not made to resist someone who wants two grids to collide, which would at most make the reader
skip a block that another attempt reads. About 6% faster on `bent20.png` (25.7, 27.1 and 26.0 seconds before; 24.5, 25.4 and 24.3
after), the same blocks on every picture of the set. Whole cut-outs repeat much less (10 to 19%) and only at one effort level, so
skipping them was not done.
