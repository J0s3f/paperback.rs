# Changelog

## 1.3.0

- Faster writing: pages are drawn and compressed side by side, and a PNG page is stored with a palette of 2 bits per pixel
  (the same gray levels): 3 MB as 19 PNG pages in 1.4 s instead of 10 s, and 19% smaller files. PDFs are the same byte for byte
  and written 2.8 times as fast.
- Faster reading: the corner mesh of a page is built only for the blocks that need it, not for the few blocks that match a
  picture with the sheets read before, and only in the settings the effort uses. Good pages read 2 to 2.6 times as fast,
  with the same blocks.
- Parity blocks (`encode --parity PERCENT`, library: `EncodeOptions::parity_percent`): an erasure code over all data
  blocks, a Cauchy matrix over GF(2^16) as in PAR2. Lost data blocks up to their number are rebuilt, wherever they
  were; with enough of them a whole page. Above 16384 data blocks the file is cut into interleaved stripes, each rebuilt
  only from its own share of the parity blocks (README, "Stripes"). `--parity` takes as many as fit (at least 300 per
  100; more for smaller files), and `encode` names the most when asked for more. They sit at addresses between those of data blocks, which PaperBack 1.00 and
  1.10 drop, so the originals read the pages as before. The decoder alternates them with PaperBack's recovery blocks.
- PaperBack's recovery blocks can be left out (`--redundancy 0`, library: `EncodeOptions::redundancy: None`).
- New defaults: one recovery block per 10 data blocks and 10 parity blocks per 100, on as much paper as before.
- `encode --fill-pages` uses the whole paper of every page for more parity blocks, with the file spread evenly over the
  pages; the number of pages stays the same.
- `encode --modern` (library: `EncodeOptions::modern`): no recovery blocks, 20 parity blocks per 100, filled pages and
  the hash records, required. `decode --modern` requires the hash. `--fill-pages false` (or `--fill-pages=false`)
  keeps the last page short; `--fill-pages` takes an optional `true` or `false`.
- Profiles for `encode`: `--legacy` (pages as PaperBack 1.10 writes them: recovery blocks only, no parity, no
  records), `--balanced` (the default) and `--modern` (library: `EncodeOptions::legacy`, `balanced`, `modern`).
- Parity blocks are computed and rebuilt on several threads (rebuilding on `decode --threads`); 5000 lost blocks of a
  file of 33000 take a quarter of a second, 10000 of one stripe under a second. Lost blocks are rebuilt with the closed
  form of the inverse of a Cauchy matrix, in the square of their number rather than its cube and without a matrix in
  memory (`docs/decisions.md`, "Rebuilding without elimination").
- A damaged page is no longer read again and again when the parity blocks read on it cover the blocks it lacks; if the file
  is still incomplete in the end, those pages are read again with every attempt. Damaged test pages decode 6 to 28 times
  faster (`docs/decisions.md`). `tools/damage.py` makes such pages.
- Every 8th cell behind the strings repeats the label.
- The sheet record carries the group size 0 for pages without recovery blocks and where the parity blocks of the page
  are. A recovery block is kept until its group can be rebuilt, also from blocks of other pages.
- Fixed: a block tried turned by a degree (from the turn of its neighbours) was turned about the corner of the page,
  not about its middle, which far from the corner moved it by up to a whole cell: an empty cell could read the block
  next to it, and the quality showed it as read. Turned tries now also work on bent paper away from the corner.
- Fixed: with `--quality-map` or `--quality-overlay` every picture is read in full, also after the file is complete
  where another picture of the sheet showed the block already and where the parity blocks of the page cover its losses,
  so that the quality shows each picture as it is.
- Fixed: cells between blocks that lost even their grid lines are drawn and counted as unreadable (they were left
  out of the pictures and of the count).
- Fixed: the bytes corrected on a page are those of the blocks it gave. A page read again (turned) took the
  corrections of the reading with the most, also of blocks it had read already; an undamaged page now says 0.
- Fixed: a page without parity blocks that lacks nothing (or only what its recovery blocks rebuild) no longer gets the
  passes of more effort. The grid of a picture covers the whole picture, so the cells over the title and the hint hold
  no block; those passes tried them and followed the grid lines of the whole page for them. A clean `--legacy` page
  decodes 1.6 times as fast, one with a wiped column twice as fast.
- Fixed: the quality of a picture read more than once (straightened, nudged) was the map of one reading, and its
  unreadable count the smallest of all, so the two could disagree. The readings are now combined cell by cell: a cell is
  unreadable only if no reading read it, and the count is that of the map, with or without `--quality-map` or
  `--quality-overlay`. Cells over the title whose corners the drawing traced no longer count as unreadable blocks.
- Blocks that read only with more effort (a later pass, or the picture straightened) are drawn hatched and counted:
  `-v` prints "N blocks read (M only with more effort)" (library: `PageStatistics::effort_blocks`,
  `PageQuality::blocks_read_with_effort`).
- Fixed: a parity block counted as the data block before its address when deciding whether a page was read
  completely, so a page lacking that block could stop being read and be left out of the reading again at the end.
- Fixed: a page whose parity blocks covered its losses stopped being read even when its hash record had not been read
  yet; a page that announces PBX1 records now stops only once the hash is read or can be rebuilt from the sheet and
  parity records. Records found when a page is read again at the end now count, so the file can still be verified.
- Blocks rebuilt from parity blocks are checked against one parity block more, if one was read; a parity block that is
  not what its address says then rebuilds nothing rather than wrong bytes. Parity blocks beyond the number the sheet
  records announce, and parity blocks read twice with different contents, are not used.
- Fixed: pages that carry different hashes of the file are refused (`Error::ConflictingHashes`); the first hash read
  was used before, so a page read first decided.
- Fixed: a page that stopped being read because its recovery blocks seemed to cover what it lacked was not read again
  when the file stayed incomplete; every page that stopped early is now read again then, until it lacks nothing. The
  counts of such a page in `--json` are those of the last reading (the blocks it added were not counted).
- Fixed: a parity block read in two cells counted twice when deciding whether a page could stop; a label whose page
  lies beyond the end of the data made the page seem complete; a hash record the file is not checked against (another
  size, keyed for a file that is not encrypted) let a page stop before the hash was read.
- Fixed: a page with exactly the cells for the records and one group (one cell short of the spare) was taken to have
  room for the records, and the file failed to encode instead of going without them.
- Fixed: parity that needs more pages than can be numbered is refused naming the most that fits, instead of the pages
  the data would need. A sheet record that places the parity blocks of a page beyond those of the file gives no layout
  (the addresses were cut to 32 bits, and on 32-bit systems the sum could overflow).
- Pages with a wiped column in every profile as fixtures (`tests/fixtures/damaged`, `tools/make-damaged-fixtures.ps1`).

## 1.2.2

- Several threads: blocks that do not read at once are tried in several ways side by side, and the next missing blocks are read ahead;
  the blocks read are the same as with one thread. `decode --threads N` (library: `DecodeOptions::threads`), default all threads. The eight
  hardest test pictures take half the time.
- Speed: the error correction on the vector units of the processor (SSSE3, AVX2, GFNI, NEON, chosen when the program starts), faster
  cutting out and reading of blocks, caches of what failed; the same blocks are read. The eight hardest test pictures took 450 seconds and
  take 82 with 16 threads (166 with one).
- Fix: pages with extension records are no longer tried with the 1.00 decryption, which with a wrong password passed the check about
  once in 65536 times and gave garbage.

## 1.2.1

- `decode --require-hash` (library: `DecodeOptions::require_hash`) fails unless the pages carry a hash or keyed check value
  that the restored file matches. Without it pages whose check value was taken off are still read, so the README no longer
  suggests that encrypted pages are protected against tampering by default.

## 1.2.0

- Pages carry a SHA-256 of the file and an identifier and layout for each sheet, in records that PaperBack 1.00 and
  1.10 ignore. The restored file is checked against the hash; `--no-extensions` leaves the records out.
- Blocks read in a cell where the page's layout does not put them are dropped.
- The sheet identifier helps to match pictures of one sheet: a picture showing another identifier is never taken for a
  known sheet, and one showing the same needs fewer agreeing blocks. The corners are looked at for it when the middle of a
  picture matches no sheet.
- The records sit in the first and last three cells of a page with a third, parity record; any one of the three can be lost.
  A page holds six cells less. Tested with damaged record cells and with 1.1 reading the new pages.

## 1.1.0

- Blocks that do not read are repaired a second time with the least certain bytes treated as erasures, which reads
  more of badly printed or photographed pages.
- With `-v` (and in the `--json` summary) pages that read badly come with hints: too few pixels per dot, a black and
  white scan, stretched contrast, sharpening halos.
- PDF pages are stored as 1-bit black and white images, half the size and printed as solid dots.
- Reading goes in passes of rising effort that visit only the blocks still missing; a turned picture is read only where
  no block was read before. The checksum picks the best sampling per quadrant for blocks nothing else reads.
- Several photos or scans of one sheet are combined; a later picture is read only where earlier ones left blocks missing,
  and reading stops once the file is complete.
- Bent, creased and photographed pages: every block is read through its own corners, found by following the printed
  grid lines, with perspective and curved edges taken into account. The quality pictures follow the real blocks.

## 1.0.0

- First version: `encode` and `decode` commands.
- Writes PNG, BMP and PDF pages; reads PNG, JPEG, BMP and PDF.
- Compatible with PaperBack 1.10 in both directions and with 1.00 for plain data; reads
  PaperBack 1.00 encrypted pages.
- bzip2 compression, AES-192-CBC encryption, Reed-Solomon error correction, recovery blocks.
- Title and scanning hint on every page.
- Pages tilted by up to 45 degrees are straightened before reading.
- Crumpled or curled paper reads: blocks are followed across the page, blurred blocks are sharpened locally, and
  scans with far more pixels than dots need are averaged down first.
- PDF input supports Flate, LZW, JPEG, JPEG 2000, CCITT fax (group 3 and 4) and JBIG2 images.
- Tests with pages written by the original PaperBack 1.00 and 1.10; GitHub Actions workflow.
- Quality pictures of decoded pages (`--quality-map`, `--quality-overlay`).
