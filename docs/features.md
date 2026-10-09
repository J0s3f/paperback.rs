# Features

Described from the user's point of view.

## Encode a file as pages

`paperback-rs encode FILE -o OUTPUT` writes the file as dense dot grids.

- Output is a PDF (`-o backup.pdf`), numbered PNG or BMP files (`-o page-%03d.png`), or one image
  for a single page. `-o -` writes to standard output and needs `--format`.
- BMP output exists because the original PaperBack can open bitmaps only.
- Large files continue on further pages; the last page shrinks to the rows it needs.
- `--redundancy N` adds one recovery block per N data blocks (2 to 10), so a lost block per group
  is rebuilt, and Reed-Solomon codes fix up to 16 bad bytes in every block.
- `--compression` selects bzip2 level (`fast`, `max`) or `none`. Data that does not shrink is
  stored unpacked.
- A password (`--password-file`, `--password-env`) encrypts the data.
- The input name and modification time are stored on the pages and restored in `--json` output.
- Every page has a title (name, date, size, page x of y) above the dots and a hint (recommended scanner resolution,
  how to restore) below; `--no-text` omits them.

## Decode pages

`paperback-rs decode PAGE... -o FILE` rebuilds the file from page images.

- Accepts PNG, JPEG, BMP and PDF with embedded page images, any number, in any order.
- Finds the grid itself: rotated by 90 degrees, upside down or mirrored. A tilted page is measured and straightened
  first, up to 45 degrees (the original manages about 5).
- PDF page images may use Flate, LZW, JPEG, JPEG 2000, CCITT fax (group 3, group 4) or JBIG2.
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
