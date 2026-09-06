<!-- SPDX-License-Identifier: MIT-0 -->
<!-- Copyright (C) 2026 Simplebooks Foundation -->
<!-- Copyright (C) 2026 Josh Rodd -->

# Usage

All commands take an output **prefix**, not a directory or filename extension: `--output results/run` produces `results/run.txt`, `results/run.json`, and any mode-specific files. Input `-` means standard input for `--capture` or `--input`. A font can use `--rom -` in `analyze` or `image` only if the capture/image comes from a file; two inputs cannot share stdin. Child capture requires a font file.

## Replay text

```sh
tigt-gfxreader replay --capture transcript.pty --output results/text
producer | tigt-gfxreader replay --capture - --output results/text
```

The transcript is replayed through a 320×80 terminal model, including cursor movement, styles, and alternate-screen changes. The text output trims trailing spaces and trailing blank rows. JSON preserves written cells, original color specifications, effective RGB colors, and style flags. This command does **not** interpret block characters as a bitmap or perform font matching.

## Reconstruct a bitmap

```sh
tigt-gfxreader analyze --capture transcript.pty --output results/frame \
  --width 320 --height 200 --region 0,0,320,200
```

No font is necessary to reconstruct the pixels. The PNG is the specified crop; JSON describes coverage, terminal evidence, and diagnostics. The text file is empty when recognition is disabled.

To recognize exact glyphs, provide your own contiguous 8×8 font:

```sh
tigt-gfxreader analyze --capture transcript.pty --output results/recognized \
  --rom font.bin --font-offset 0 --glyph-count 128 \
  --region 0,0,320,200 --expect READY
```

`--expect` is a printable ASCII substring within one glyph row, not an instruction to invent matching text. It can select an exact candidate from ambiguous masks; a missing expectation causes analysis failure. Fontless analysis cannot use an expectation.

## Analyze an existing PNG

```sh
tigt-gfxreader image --input screen.png --rom font.bin \
  --font-offset 0 --glyph-count 128 --output results/imported \
  --width 640 --height 200 --region 0,0,640,200

cat screen.png | tigt-gfxreader image --input - --rom font.bin \
  --font-offset 0 --glyph-count 128 --output results/streamed
```

The input dimensions must match `--width` and `--height`, even when a crop is requested. The default is 320×200. The output `.original.png` is a byte-for-byte copy of the input, not a re-encoding. The separate `.png` is an RGBA reconstruction of the requested crop. A PNG is decoded directly, never through terminal replay.

## Capture an owned child

```sh
tigt-gfxreader capture --output results/session -- /path/to/program arg

tigt-gfxreader capture --output results/frame --mode bitmap \
  --width 320 --height 200 --region 0,0,320,200 \
  --timeout-ms 5000 --max-bytes 1048576 -- /path/to/renderer
```

Text mode is the default. Bitmap mode accepts the same optional font, dimensions, crop, and expectation settings as `analyze`. Timeout defaults to 10,000 ms; transcript capacity defaults to 67,108,864 bytes. Capture saves raw bytes and lifecycle evidence even when the child fails; available output is still replayed/analyzed. A failed or truncated child run must not be mistaken for a complete screen.

Arguments after `--` name the executable and its literal arguments. Shell syntax is not interpreted unless you explicitly run a shell:

```sh
tigt-gfxreader capture --output results/shell -- /bin/sh -c 'printf "hello\n"'
```

See [instrumentation](instrumentation.md) before running untrusted commands or recording sensitive sessions, and [reference](reference.md) for exit codes and evidence fields.
