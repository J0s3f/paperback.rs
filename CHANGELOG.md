# Changelog

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
