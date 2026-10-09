# Notice

paperback.rs is a command-line port of **PaperBack 1.10**. It reads and writes the same on-paper
format and contains code derived from the original, so it is distributed under the same terms:
the **GNU General Public License, version 3 or (at your option) any later version**. See `LICENSE`.

Copyright (c) 2026 Josef H.B. Schneider, for the port and everything added to it.

## Origin

- PaperBack, Copyright (c) 2007 Oleh Yuschuk. <https://ollydbg.de/Paperbak/>
  The page layout, block format, grid detection and block reading in this repository follow
  the original source (`paperbak-1.10.src`).
- PaperBack 1.10 changes (key stretching, CBC mode, selectable key length), parts copyright (c) 2013
  Michael Mohr.
- Reed-Solomon (255,223) coding in `crates/paperback-rs/src/reed_solomon.rs` is derived from software by
  Phil Karn, KA9Q, Copyright 2002, used under the GPL.
- Decryption of pages written by PaperBack 1.00 follows the 1.00 source (`Paperbak.zip`). Only
  decryption is implemented; the scheme is weak and this program never writes it.

## What is not taken from the original

The original contains the bzip2 library (Julian R. Seward) and AES/SHA code (Christophe Devine,
Brian Gladman). This port does not include them; it uses the `bzip2`, `aes`, `cbc`, `pbkdf2`,
`hmac` and `sha2` crates instead. The graphical interface, TWAIN scanning and printing of the
original are not part of this port. Page text uses the glyphs of the `font8x8` crate (public-domain font by Daniel Hepper, MIT-licensed crate).

## Warranty

As in the original, there is no warranty of any kind, and no guarantee against patent or
trademark infringement.
