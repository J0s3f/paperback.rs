# Bent and slanted pages

Pages of the same 9000 bytes (`(i * 131 + i / 3) as u8`) written at 300 dpi with 100 dpi dots, then warped like a
page that is crumpled or photographed at an angle. The warps are in `tests/robustness.rs` (`bent`, `foreshortened`);
a block is about 105 pixels wide.

| File | Warp | Blocks the decoder restores |
|---|---|---|
| `bent8.png` | smooth bend, dots moved by up to 8 pixels | all |
| `bent12.png` | up to 12 pixels | all |
| `bent16.png` | up to 16 pixels | all |
| `bent20.png` | up to 20 pixels | most (see the test) |
| `slant10.png` | upper edge 10% farther away, as seen from below | all |
| `slant15.png` | 15% | all |
| `slant25.png` | 25% | all |

`tests/synthetic.rs` decodes every file. Pages that restore completely must keep doing so; `bent20.png` is a regression
guard with a minimum number of blocks.
