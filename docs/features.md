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
- `--redundancy N` adds one recovery block per N data blocks (2 to 10), so a lost block per group
  is rebuilt, and Reed-Solomon codes fix up to 16 bad bytes in every block.
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
  reports a match. `--require-hash` turns a missing or non-matching hash into an error (exit 3). Pictures of one sheet are matched by their identifier as well as by block addresses. `--json` lists `integrity`, the sheet identifiers and `misplaced_blocks`.
- Pages written by paperback.rs tell their layout, so a block read in a cell where it does not belong is dropped
  instead of trusted (`-v` counts them).
- Pages that cannot be read are reported with `-v` and skipped; the file is restored if the
  remaining pages carry everything.
- If blocks are missing, the error lists the pages to scan again.
- Pages written by PaperBack 1.00 and 1.10 are accepted, including encrypted ones.
- `--quality-map` and `--quality-overlay` write a PNG per page that colours every block from green (read
  cleanly) over orange (much error correction) to red (unreadable), on white or laid over the page. They are
  written even if the file cannot be restored.

## Scripting

Standard input and output are used when no file is named, errors go to standard error, nothing is
printed on success, and exit codes distinguish usage, input and password problems.
