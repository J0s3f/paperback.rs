# Features

Described from the user's point of view.

## Encode a file as pages

`paperback-rs encode FILE -o OUTPUT` writes the file as dense dot grids.

- Output is a PDF (`-o backup.pdf`), numbered PNG or BMP files (`-o page-%03d.png`), or one image
  for a single page. `-o -` writes to standard output and needs `--format`.
- BMP output exists because the original PaperBack can open bitmaps only.
- PDF pages are pure black and white (one bit per pixel): half the size, and the dots print as solid black instead of
  being halftoned. PNG and BMP keep the gray levels of the original program.
- Large files continue on further pages; the last page shrinks to the rows it needs.
- `--redundancy N` adds one recovery block of PaperBack per N data blocks (2 to 10, default 10; 0 for none), so a lost
  block per group is rebuilt, also by the original programs, and Reed-Solomon codes fix up to 16 bad bytes in every block.
- `--parity PERCENT` adds parity blocks (default 10 per 100 data blocks): lost data blocks up to their number are
  rebuilt, on whichever pages they were lost; with enough of them (100 / (pages - 1) percent) a whole page. Above
  16384 data blocks the file is cut into interleaved stripes, each rebuilt only from its own share of the parity
  blocks: a lost page spreads evenly over them, losses that fall on one stripe are limited by that share. As many
  parity blocks as fit are allowed (at least 300 per 100, more for smaller files); `encode` names the most when asked
  for more. PaperBack
  ignores them. Both kinds can be combined (`docs/format.md`, "Parity blocks and filled pages").
- `--fill-pages` uses the whole paper of every page for more parity blocks; the number of pages stays the same.
  `--fill-pages false` (or `=false`) turns it off, also after `--modern`; give the input file before the option or
  write `--fill-pages=true`, since the option takes an optional value.
- `--legacy` writes pages as PaperBack 1.10 does: recovery blocks only (one per 5 data blocks), no parity blocks, no
  records. `--balanced` is the default mix.
- `--modern` writes pages for paperback.rs: no recovery blocks of PaperBack but as many more parity blocks (20 per
  100), filled pages (unless `--fill-pages false`), and the hash records, required. `decode --modern` requires the
  hash.
- `--compression` selects bzip2 level (`fast`, `max`) or `none`. Data that does not shrink is
  stored unpacked.
- A password (`--password-file`, `--password-env`) encrypts the data.
- The input name and modification time are stored on the pages and restored in `--json` output.
- Pages carry a SHA-256 of the file and an identifier and layout for each sheet, in a form that PaperBack 1.00 and 1.10
  ignore (`docs/format.md`, "Extension records"). `--no-extensions` leaves them out.
- Every page has a title (name, date, size, page x of y) above the dots and a hint (recommended scanner resolution,
  how to restore) below; `--no-text` omits them.

## Decode pages

`paperback-rs decode PAGE... -o FILE` rebuilds the file from page images.

- Accepts PNG, JPEG, BMP and PDF with embedded page images, any number, in any order.
- Finds the grid itself: rotated by 90 degrees, upside down or mirrored. A tilted page is measured and straightened
  first, up to 45 degrees (the original manages about 5).
- PDF page images may use Flate, LZW, JPEG, JPEG 2000, CCITT fax (group 3, group 4) or JBIG2.
- Several pictures of one sheet are combined (see the README). A picture is read only where earlier pictures left blocks
  missing, and pictures after the one that completes the file are not read.
- A block that does not read as it is gets a second chance: the bytes whose dots were hardest to tell apart are treated
  as known-bad (erasures), which lets the error correction repair up to twice as much damage.
- With `-v`, a page that read badly is followed by hints on likely causes: fewer than 2.5 pixels per dot, a scan that
  is pure black and white, stretched contrast or bright halos from sharpening. `--json` lists them as `hints`. Without
  `-v` or `--json` nothing is analysed and nothing is printed.
- When the pages carry a SHA-256 the restored file is compared with it; a difference is an error (exit 3) and `-v`
  reports a match. Pages that carry different hashes of the file are an error as well, whichever the file matches. `--require-hash` turns a missing or non-matching hash into an error (exit 3). Pictures of one sheet are matched by their identifier as well as by block addresses. `--json` lists `integrity`, the sheet identifiers and `misplaced_blocks`.
- Pages written by paperback.rs tell their layout, so a block read in a cell where it does not belong is dropped
  instead of trusted (`-v` counts them).
- `--threads N` sets how many threads read the blocks that do not read at once (default: all; 1 is one after the other); the same
  blocks are read either way.
- Pages that cannot be read are reported with `-v` and skipped; the file is restored if the
  remaining pages carry everything.
- Blocks rebuilt from parity blocks are checked against one parity block more, when one was read; a parity block that
  does not fit (forged, or of another file) then rebuilds nothing instead of wrong bytes. Parity blocks beyond the number
  the pages announce, and parity blocks read twice with different contents, are not used.
- A page stops being read once its recovery or parity blocks cover what it lacks. If the file still lacks blocks in the
  end, those pages are read again with every attempt; the counts of `-v` and `--json` are then those of the last
  reading.
- If blocks are missing, the error lists the pages to scan again.
- Pages written by PaperBack 1.00 and 1.10 are accepted, including encrypted ones.
- `--quality-map` and `--quality-overlay` write a PNG per page that colours every block from green (read
  cleanly) over orange (much error correction) to red (unreadable), on white or laid over the page. They are
  written even if the file cannot be restored. They show how each picture was printed and scanned: every cell of
  every picture is read, also after the file is complete, where another picture showed the block already, and where
  recovery or parity blocks would rebuild it.
- A cell without dots never reads the block next to it, and a cell between blocks that lost even its grid lines counts
  as unreadable. The bytes corrected on a page are those of the blocks it gave; 0 for a page as printed.
- The readings of one picture (as it is, straightened, nudged) are combined cell by cell: a cell is red only if no
  reading read it, and the unreadable count of `-v` is the number of red cells, the same with or without the pictures.
  Blocks that read only in a later pass or reading are drawn hatched and counted ("N only with more effort").

## Scripting

Standard input and output are used when no file is named, errors go to standard error, nothing is
printed on success, and exit codes distinguish usage, input and password problems.
