# Real-world decode tests

Synthetic data (seeded pseudo-random bytes, no personal content) as print sheets, to be printed,
scanned or photographed, and decoded again by `tests/realworld.rs`.

## How to use

1. Print the PDFs in `sheets/` **at 100% / actual size** (not "fit to page"), single sided, plain paper,
   at the printer's highest quality and without any "save toner" or image enhancement. Note the printer.
2. Scan (grayscale, about three times the dot density, i.e. 900 dpi for a 300 dpi dot density; 600 dpi is
   enough for `c-inkjet-300`) or photograph in even light. Leave the picture as it is: no sharpening, no
   cropping needed, tilt and rotation are fine.
3. Save the pictures into `scans/` as `<case>_<page>.<png|jpg|bmp|pdf>`, for example
   `a-laser-600-default_1.png`, `e-two-pages_1.jpg`, `e-two-pages_2.jpg` (a multi-page PDF per case works too).
   Prefer lossless formats for fixtures; JPEG is fine for photos. Mind the file size when committing.
4. `cargo test --test realworld -- --nocapture` reports what was checked. Decode with
   `paperback-rs decode scans/<case>_*.png --quality-overlay q-%d.png -o out` to see where a scan is weak.

## Cases

| Case | Printer / dot density | Redundancy | Pages | What it tests |
|---|---|---|---|---|
| `a-laser-600-default` | 600 / 200 dpi | 5 | 1 | the default setup |
| `b-laser-600-dense` | 600 / 300 dpi | 5 | 1 | the densest sensible setup; needs a good scan (hard) |
| `c-inkjet-300` | 300 / 100 dpi | 5 | 1 | an inkjet or a low resolution printer |
| `d-robust-r2-frame` | 600 / 150 dpi | 2 | 1 | larger dots, strong redundancy, alignment frame |
| `e-two-pages` | 600 / 200 dpi | 5 | 2 | several pages, any scan order |
| `f-encrypted` | 600 / 200 dpi | 5 | 1 | password `paperback-rs-test` (unaudited legacy scheme) |
| `g-letter-small-dots` | 600 / 200 dpi, Letter, 15 mm margins, dots 60% | 8 | 1 | small dots and Letter paper |

Ideas for the harder end: photograph a sheet with a phone, scan it slightly crooked, scan a creased or
coffee-stained copy, or photocopy it once. Add such pictures as extra pages named `<case>_<n>.<ext>`.

The sheets and data were made with `tools/realworld/make-sheets.ps1` and are committed, so the tests do not
depend on it. Do not regenerate them after printing: the scans must match the committed data.

## Scans so far

| Case | Scan | Result |
|---|---|---|
| c-inkjet-300 | inkjet print, 1200 dpi RGB scan, stored as 400 dpi gray JPEG | decodes, 418 blocks, none unreadable |
| c-inkjet-300 | the same print crumpled and flattened, 1200 dpi scan, stored as 400 dpi gray JPEG | decodes, 414 of 418 blocks (the rest are rebuilt from recovery blocks) |
| d-robust-r2-frame | inkjet print, 600 dpi colour JPEG scan, 150 dpi dots | decodes, but only just: most blocks need 8 to 16 corrected bytes |
| d-robust-r2-frame | second print, 1200 dpi colour scan stored as 600 dpi gray JPEG | decodes, 970 blocks read |
| d-robust-r2-frame | same sheet scanned at 300 dpi (2 pixels per dot) | not readable; scan at 600 dpi |
| a-laser-600-default | laser print scaled to about 119% by the print dialog | not readable (about 10% of dots wrong); reprint at 100% |

Raw scans that are too large or not yet decodable go in `evaluate/`, which git ignores.
