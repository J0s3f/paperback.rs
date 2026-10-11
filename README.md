# paperback.rs

Back up a file on paper. paperback.rs turns any file into one or more pages of tiny dots, protected by
error correction. You print the pages, and years later you scan or photograph them and get the file back,
even if parts of the paper are damaged.

It is a command-line port, written in Rust, of **PaperBack** by Oleh Yuschuk. It reads and writes the same
page format, so pages made by the original open here and pages made here open in the original.
There is no GUI and no scanning or printing code: you feed it files and images, and it gives you files and
images.

Homepage: <https://paperback.j0s.at> - Licence: GPL-3.0-or-later (see [NOTICE.md](NOTICE.md))

## Origins

- **PaperBack** is a free Windows program by Oleh Yuschuk (the author of OllyDbg) that prints files as
  large bitmaps and reads them back from a scanner: <https://ollydbg.de/Paperbak/>. Version 1.00 was
  released in 2007; version 1.10 (by Michael Mohr) fixed its weak encryption.
- **paperback.rs** is not the original program and is not affiliated with it. The name ends in `.rs` to keep
  the two apart. The code is a port of the 1.10 source: the page layout, block format, Reed-Solomon coding and
  the grid and block reading follow the original, which is why the two programs are interchangeable.
  Reed-Solomon coding is derived from software by Phil Karn, KA9Q.
- Why a port: the original is Windows-only, GUI-only and needs a TWAIN scanner. paperback.rs is a plain
  command-line tool for scripts and other operating systems, with a library behind it.

Credits and licences of the parts it builds on are in [NOTICE.md](NOTICE.md).

## What it can do

- Write a file as PNG pages, 8-bit BMP pages (the only image format the original opens) or one PDF.
- Read pages from PNG, JPEG, BMP and PDF files, in any order, whether they come from this tool, the original,
  a scanner or a camera. PDFs may hold Flate, LZW, JPEG, JPEG 2000, CCITT fax or JBIG2 images.
- Rebuild the file from parity blocks spread over all pages, in the manner of PAR2: lost blocks up to their number
  (per stripe of 16,384 data blocks in larger files, see [Profiles](#profiles-legacy-balanced-modern)), with enough of
  them a whole lost page. The original program ignores them.
- Rebuild the file when pages are tilted (up to 45 degrees; the original copes with about 5), rotated by a
  quarter turn, upside down, mirrored, crumpled and flattened again, photographed at an angle, or lightly damaged: every block has its own error correction, and a
  recovery block per group of data blocks rebuilds one lost block.
- Compress with bzip2 before printing.
- Work in pipelines: standard input and output, quiet on success, clear exit codes.

## Install

From [crates.io](https://crates.io/crates/paperback-rs-cli):

```
cargo install paperback-rs-cli
```

This installs the program `paperback-rs`. Ready-made binaries for Windows, Linux and macOS are attached to each
[release](https://github.com/J0s3f/paperback.rs/releases). From a checkout:

```
cargo install --path crates/paperback-rs-cli
```

or `cargo build --release` and take `target/release/paperback-rs`. It needs Rust 1.92 or newer.

As a library: `cargo add paperback-rs`.

## Usage

```
paperback-rs encode report.tar -o page-%03d.png       # one PNG per page: page-001.png, page-002.png, ...
paperback-rs encode report.tar -o backup.pdf          # one PDF with all pages
paperback-rs encode report.tar -o page-%02d.bmp       # BMP, for opening the pages in the original PaperBack
paperback-rs encode - -o backup.pdf < report.tar      # read standard input

paperback-rs decode page-*.png -o report.tar          # pages in any order
paperback-rs decode backup.pdf > report.tar           # PDF with embedded page images
paperback-rs decode -v --json scan1.jpg scan2.jpg -o report.tar   # per page details, JSON summary on stderr
```

Useful options for `encode`: `--printer-dpi 600` (resolution of your printer), `--dot-dpi 200` (dot density,
at most half the printer resolution), `--dot-size 70` (percent of the dot pitch), `--redundancy 10` (one
recovery block per ten data blocks, 2 to 10, 0 for none), `--parity 10` (parity blocks per 100 data
blocks; lost blocks up to their number are rebuilt, per stripe in files of more than 16,384 data blocks), `--fill-pages` (use the whole paper of every page for more
parity), `--legacy`, `--balanced`, `--modern` (see [Profiles](#profiles-legacy-balanced-modern)), `--compression max|fast|none`, `--paper a4|letter`,
`--margins-mm L R T B`, `--frame`, `--no-text` (leave out the title above and the scanning hint below the
dots). `paperback-rs --help` lists everything.

Printing and scanning tips (the same as for the original): use a laser printer at its full resolution without
scaling, scan in grayscale at about three times the dot density (900 dpi for the defaults on a 600 dpi
printer) and check the result before you throw the digital copy away. One A4 page holds about 155
kilobytes with the default settings and about 170 with `--legacy` (before compression).

Exit status: 0 success, 1 failure, 2 usage error, 3 unreadable or incomplete input (the message names the pages
to scan again), 4 missing or wrong password.

## Profiles: legacy, balanced, modern

Pages can protect themselves in two ways. **Recovery blocks** are PaperBack's own: one per group of N data blocks
(`--redundancy N`), each rebuilds one lost block of its group, and the original program uses them too. **Parity
blocks** (`--parity PERCENT`, per 100 data blocks) are paperback.rs only: like PAR2 they rebuild lost blocks up to
their number, wherever they were lost, so with enough of them a whole lost page. The original program reads pages with
parity blocks but skips them. A page can carry both. Three profiles pick a mix for `encode`:

| | `--legacy` | `--balanced` (default) | `--modern` |
|---|---|---|---|
| Recovery blocks (PaperBack) | 1 per 5 data blocks | 1 per 10 data blocks | none |
| Parity blocks (paperback.rs) | none | 10 per 100 data blocks | 20 per 100, and the whole rest of every page |
| SHA-256 / keyed check value, sheet records | no | yes | yes, required |
| Pages used to the last row | no | no | yes (`--fill-pages`; `--fill-pages false` turns it off) |
| Paper for the same file | as PaperBack | about the same as `--legacy` | about the same, last page full |
| Read by PaperBack 1.10 | yes, with its recovery | yes, with its recovery | yes, but without any recovery |
| Best for | pages that must look exactly like PaperBack's | most backups | backups that will be read with paperback.rs |

- `--legacy` writes the pages PaperBack 1.10 itself would write: no parity blocks and no PBX1 records (no hash, no
  sheet identifier). `--redundancy` can still change the group size.
- `--balanced` is what you get without a profile. Its recovery blocks still help when the pages are read by the
  original program; its parity blocks rebuild far more in paperback.rs: with ten parity blocks per hundred data
  blocks, about a tenth of the data blocks may be lost (see stripes below for files of more than 16,384 blocks). `--redundancy`, `--parity`, `--fill-pages` and `--no-extensions` change
  single parts.
- `--modern` spends the paper of the recovery blocks on parity blocks instead (20 per 100), fills every page to the
  last row with more of them, and requires the hash records: it fails rather than write pages without them. Use
  `decode --modern` to also insist on the hash when reading (the same as `--require-hash`). `--parity` changes the
  amount; `--fill-pages false` (or `--fill-pages=false`) keeps the last page short, as with the other profiles.
  `--fill-pages` takes an optional `true` or `false`: give the input file before it, or write `--fill-pages=true`, so that the
  file name is not taken for its value.

To survive the loss of a whole page out of `p`, a file needs about `100 / (p - 1)` parity blocks per 100 data blocks,
for example `--parity 50` for three pages. A single page cannot stand in for itself.

**Stripes: the limit for larger files.** A file of up to 16,384 data blocks (about 1.4 MB as stored, after
compression, some 9 pages with the default settings) is one stripe, and the rule above holds exactly: any lost data
blocks, up to the number of parity blocks that were read, wherever they were. A larger file is cut into
`s = ceil(blocks / 16384)` interleaved stripes (data block `i` belongs to stripe `i mod s`, and so does parity block
`i`), and each stripe can only be rebuilt from its own parity blocks, about a `1/s` share of them.

- Losses of consecutive blocks, such as a whole page or a part of one, spread evenly over the stripes, so for them the
  total still counts, give or take one block per stripe.
- Losses that happen to fall unevenly are limited by the stripe that lost most. For example, a file of 16,385 blocks
  with `--redundancy 0 --parity 1` has 164 parity blocks, 82 in each stripe. Losing 83 blocks that are all
  even-numbered (all in stripe 0) cannot be repaired, although all 164 parity blocks survive.

`--parity` takes as many parity blocks as fit: the code allows at least three per data block (300), a file of fewer
than 16,384 blocks more (`100 * (65536 - blocks) / blocks`), and a small file is limited by the paper, since every
page carries data. Asked for more, `encode` says how many fit.

Choose `--parity` with some margin for large files, and keep PaperBack's recovery blocks (`--redundancy`) where
damage tends to hit single blocks. `docs/format.md` describes the code.

## Combining several photos or scans of the same page

A page that is creased, folded, shadowed or photographed from a bad angle often reads in parts. You do not need one
perfect picture: give the decoder several pictures of the same sheet and it combines what each one shows.

```
paperback-rs decode photo1.jpg photo2.jpg photo3.jpg -o report.tar -v
```

- **How it works.** Every block that is read carries its address in the file, so blocks from different pictures slot
  together and duplicates are dropped. Which blocks are still missing after one picture does not matter; the next
  picture only has to show them.
- **Which pictures belong together.** The decoder recognises the sheet from information stored in the dots, not from the
  printed text. Each page repeats a few *label blocks* among the data (the file name, size, date and page number, as
  dots like everything else), pages from this version also carry a sheet identifier, and every block has its address in the
  file. From these it works out which file and page a picture shows and how its grid lies against the other pictures.
  You can pass the pictures in any order, turned or mirrored, at different sizes, and mix photos with scans. Pictures of
  different pages of one file work the same way. Pictures of different files are refused with a message. The title
  above the dots and the hint below them are for people only; the decoder never reads them.
- **What helps most.** Pictures that fail in different places: another angle, other light, the creased part laid
  flat, a second scanner. Two pictures with the same shadow have the same gaps. A close-up of a corner is of use only if
  the grid of blocks is clearly visible in it.
- **It saves time.** A picture is read only where the pictures before it left blocks missing, and when the file is
  complete the remaining pictures are not read at all (`-v` says "not read: the file is complete already"). With
  `--quality-map` or `--quality-overlay` every picture is read in full instead, so that each picture's quality is shown.
- **Checking.** `-v` prints what each picture gave, and `--quality-overlay q-%d.png` writes one picture per input that
  shows which blocks it read, so you can see what the next photo has to cover.

## Checking the quality of a scan

`decode` can show how well every block of a page was read, as a picture per page:

```
paperback-rs decode scan.png --quality-map map.png --quality-overlay overlay.png -o report.tar
paperback-rs decode scan-*.png --quality-overlay q-%02d.png -o report.tar     # one picture per page
```

Green blocks read cleanly, blocks that needed more and more error correction shade through yellow to orange,
and red blocks could not be read. The colours show how each picture was printed and scanned, not whether the file
needed the block: every cell of every picture is read, also after the file is complete, where another picture of the
sheet showed the block already, and where recovery or parity blocks would rebuild it. That takes longer than a plain
decode. `--quality-map` draws the colours on white paper; `--quality-overlay` lays
them over the scan so you can see where the damage is (a dirty spot, a fold, a bad scanner edge). The pictures
show the page as it was read, which is the scan itself unless it had to be straightened or, because it has many more pixels than the dots need, reduced first. They are written
even when the file cannot be restored, which is when they are most useful: they tell you what to rescan.

- **Red means no attempt could read the block.** A picture that does not read completely is read again
  straightened, and with its angle nudged; every block a later reading finds is put into its cell of the first one.
  The count `-v` prints ("44 unreadable") is the number of red cells, with or without the pictures.
- **Hatched means it took effort.** A block that read only in a later pass (sharper settings, the dots of the page
  learnt, along bent grid lines) or only in the straightened picture is drawn with dark stripes over its colour, and
  `-v` counts them: "1188 blocks read (10 only with more effort)". The colour still says how many bytes had to be
  corrected; the stripes say that the block was hard to find or to sample, as on a creased, tilted or blurred scan.
- **No colour** is where no block is: the margin, the title and the hint below the blocks. The grid of a picture
  covers the whole picture, because the decoder cannot know where the blocks start, so a cell over the title is no
  damaged block.

## Encryption: please read this

paperback.rs can encrypt with a password (`--password-file` or `--password-env`, never a prompt) and decrypt
backups made by the original program. **This cryptographic code is not audited.** It is there mainly for
**backward compatibility** with the original PaperBack, so that old encrypted pages can be read and new pages
can be written that the original can read. Treat it as that, not as a vault.

What it is, so you can judge:

- PaperBack 1.10 scheme: PBKDF2-HMAC-SHA256 (524288 rounds, random salt) to derive an AES-192 key, AES-CBC.
  The primitives come from the RustCrypto crates (`aes`, `cbc`, `pbkdf2`, `hmac`, `sha2`); the way they are
  combined is the original's design.
- **Authentication only if you ask for it.** Pages from paperback.rs 1.2 and later carry a check value of the file
  under a key derived from the password (HMAC-SHA256), and `decode` refuses a file that does not match it, so damage
  and a wrong password are caught. But the check value is part of the pages, and whoever can change the pages can
  take it off as well; the file then still decrypts, with only the original's 16-bit checksum to notice, and AES-CBC
  lets an attacker flip chosen bits of the file. So against an attacker, decode with `--require-hash`: it fails unless
  the pages carry a check value that the restored file matches. Pages of the original programs, and pages written with
  `--no-extensions`, carry none and cannot be read that way. Without `--require-hash` the 16-bit checksum is the
  only protection a page without a check value has, and it is also the only "wrong password" check for them.
  Pages that carry different check values for the file are refused, whichever the file matches: either page could be
  the one that was changed.
- PaperBack 1.00 scheme (AES-256 in ECB mode keyed with the bare password): **decryption only**, so old
  backups stay readable. It is weak and paperback.rs will never write it.

**Recommended instead: encrypt with a secure tool first and use paperback.rs only to encode.** The encrypted
file is just bytes to paperback.rs, so the two compose with a pipe:

```
# compress first (encrypted data does not compress), then encrypt, then put it on paper
tar -c documents | zstd | age -R recipients.txt | paperback-rs encode - -o backup.pdf

# and back
paperback-rs decode backup.pdf | age -d -i key.txt | zstd -d | tar -x
```

### Which tool?

All of these work on a single file or a stream, so they fit in front of `paperback-rs encode`. The audit
column is what I could find (October 2026, from public discussions and project pages, not from the audit
reports themselves); please verify it before you rely on it.

| Tool | Audit status found | Notes |
|---|---|---|
| [age](https://github.com/FiloSottile/age) / [rage](https://github.com/str4d/rage) | No published independent audit found | Small, well specified, pipe-friendly: `age -p` (password) or `age -r` (public key). A 2024 advisory about plugin names affected both implementations and was fixed. |
| [GnuPG](https://gnupg.org/) (`gpg`) | No single published full audit found; decades of public review and the most widely deployed OpenPGP tool | Large and complex, with a long history of fixes. `gpg --symmetric` (password) or `gpg --encrypt -r`. Available everywhere, and the OpenPGP format is an open standard, which helps if you need to decrypt it in twenty years. |
| [Picocrypt](https://github.com/Picocrypt/Picocrypt) | Audited by Radically Open Security in 2024; most findings fixed | Makes one encrypted file. The original is archived; [Picocrypt-NG](https://github.com/Picocrypt-NG/Picocrypt-NG) continues it. |
| [Kryptor](https://github.com/samuel-lucas6/Kryptor) | No audit found | Single maintainer; builds on libsodium, which has been audited; no commits for over two years at the time of the discussion. |

Examples with a password and with GnuPG:

```
tar -c documents | zstd | gpg --symmetric --cipher-algo AES256 | paperback-rs encode - -o backup.pdf
paperback-rs decode backup.pdf | gpg --decrypt | zstd -d | tar -x
```

Sources: [Privacy Guides: file encryption](https://privacyguides.org/software/file-encryption/),
[Picocrypt audit results](https://discuss.privacyguides.net/t/picocrypt-security-audit-results/20615),
[Kryptor, age and minisign have no audits](https://discuss.privacyguides.net/t/should-kryptor-still-be-recommended-on-the-site/30368),
[RustSec advisory for rage](https://rustsec.org/advisories/RUSTSEC-2024-0432).

Whatever you choose, think about the year you will need to decrypt: keep a copy of the tool (or its format
specification) with the backup, and test a full restore before you rely on it.
## Compatibility with the original

| Direction | PaperBack 1.10 | PaperBack 1.00 |
|---|---|---|
| original writes, paperback.rs reads | yes, plain and encrypted | yes, plain and encrypted (read only) |
| paperback.rs writes, original reads | yes, plain and encrypted | plain only |

This is tested two ways: `crates/paperback-rs/tests/fixtures/original` holds pages written by the real 1.00
and 1.10 programs, which run in CI without the original executables, and `tools/interop` holds Windows scripts
that drive the original programs through their file dialogs to check both directions on demand (they never
print or scan).

## Speed on newer processors

Blocks that do not read at once are tried in several ways side by side, and the next ones are read ahead, on all the threads of
the processor; the blocks read are the same as with one thread. `decode --threads N` sets the number (1 is one after the other;
the default, 0, uses them all). It speeds up the pictures that need much work: crumpled pages and photographs.

The error correction runs on the vector units of the processor when it has them: SSSE3, AVX2, or GFNI with AVX2 on x86-64, NEON on aarch64,
chosen when the program starts, or NEON on ARM (Linux, macOS on Apple silicon); a processor with none of them runs the plain code, which gives the same result. `decode -v`
says which is used. To compare, `PAPERBACK_SIMD=scalar` (or `ssse3`, `avx2`, `gfni`) asks for a lower level; a level the
processor lacks is never used.

## Extras that the original does not have

Pages carry **parity blocks** (see [Profiles](#profiles-legacy-balanced-modern)); the original ignores them.

Pages written by paperback.rs carry a SHA-256 of the file and an identifier and layout for each sheet. The originals
ignore them (tested with 1.00 and 1.10); paperback.rs uses them to check the restored file against the hash, to tell
sheets apart and to distrust a block read in the wrong place. `decode -v` reports them. To write plain pages without them
use `--no-extensions` (or `--legacy`). Details: `docs/format.md`.

## Status

Working: everything above. Not yet: PDFs with mixed one- and two-dimensional fax coding, vector-only PDF pages
(there must be an image to read).

## Library

The `paperback-rs` crate (imported as `paperback_rs`) holds the engine; the command-line tool is a thin layer on
top. See the crate documentation (`cargo doc --open`) for an example.

## More

[docs/features.md](docs/features.md) - what each part does -
[docs/format.md](docs/format.md) - the on-paper format -
[docs/decisions.md](docs/decisions.md) - design decisions and the crates used -
[CHANGELOG.md](CHANGELOG.md).

Development: `cargo test`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check`.
