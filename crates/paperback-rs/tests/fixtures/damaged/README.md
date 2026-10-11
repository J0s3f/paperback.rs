# Pages with a wiped column

Pages of the same 90000 bytes (`(i * 131 + i / 3) as u8`, file time 2024-01-02 03:04:05 UTC) written at 300 dpi with
200 dpi dots and no compression, in every profile, with column 5 of the first page wiped white from top to bottom, as
by a fold or a streak of the scanner. Made with `tools/make-damaged-fixtures.ps1`.

| Files | Written with | Wiped |
|---|---|---|
| `legacy-1.png` | `--legacy` | the dots of column 5 |
| `balanced-1.png` | the defaults | the dots of column 5 |
| `parity-1.png`, `parity-2.png` | `--redundancy 0 --parity 40` | the dots of column 5 on page 1 |
| `modern-1.png`, `modern-2.png` | `--modern` | the dots of column 5 on page 1 |
| `legacy-lines-1.png` | `--legacy` | column 5 with its grid lines |

`tests/damaged.rs` decodes every case. The file must be restored, and the first page must report exactly the wiped
cells as unreadable, in its statistics and in its quality, and no corrected bytes where nothing else was damaged.
Before these pages, an empty cell far from the corner of the page could read the block next to it, and pages read
again took the corrections of readings that added nothing.
