# Changelog

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
