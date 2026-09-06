<!-- SPDX-License-Identifier: MIT-0 -->
<!-- Copyright (C) 2026 Simplebooks Foundation -->
<!-- Copyright (C) 2026 Josh Rodd -->

# CLI and evidence reference

## Commands

```text
analyze --capture FILE|- --output PREFIX
        [--rom FILE --font-offset N --glyph-count N --expect ASCII]
        [--width 160|320|640 --height 200 --region x,y,w,h]
image   --input FILE|- --rom FILE --output PREFIX
        [--font-offset N --glyph-count N --expect ASCII]
        [--width 160|320|640 --height 200 --region x,y,w,h]
replay  --capture FILE|- --output PREFIX
capture --output PREFIX [--timeout-ms N] [--max-bytes N]
        [--mode text|bitmap] [bitmap/font options] -- COMMAND ARGS...
```

The bitmap default is 320×200; a missing region selects the full image. Region coordinates and dimensions must be multiples of eight, nonempty, and entirely inside the configured image. This is a fixed glyph grid anchored at `(0,0)`; arbitrary screenshot geometry is not discovered. Width 160 represents doubled horizontal terminal pixels, while 320 and 640 use two bitmap pixels per sextant cell. The terminal remains 320 columns by 80 rows for all widths. A sextant cell represents a 2×3 mask; inconsistent doubled pixels are not repaired.

PNG input supports 8-bit RGB, RGBA, grayscale, and indexed color, including packed low-bit palette/grayscale samples expanded losslessly. Sixteen-bit and animated PNGs are rejected. Any alpha below 255 is unknown evidence, not a chosen background color. There is no scaling, interpolation, antialiasing tolerance, or nearest-match substitution. Input files/streams are limited to 64 MiB; stdin is read until EOF. Child capture requires a font file rather than a font on stdin.

## Caller-supplied font

`--rom` is a byte container, not a required machine ROM format. The selected font starts at `--font-offset` bytes and contains `--glyph-count` consecutive glyphs, from code zero. Each glyph is eight row bytes, top to bottom; bit 7 is the leftmost pixel and bit 0 the rightmost. The required byte range is:

```text
[font_offset, font_offset + glyph_count * 8)
```

Offset defaults to zero and count to 128; counts must be from 1 through 256. Specify the offset and count explicitly when importing a font; there is no automatic font discovery. Offset and count accept decimal or `0x` hexadecimal integers. A short or out-of-range font fails instead of reading beyond the file. All glyph bytes, including padding/control-code slots, come from the caller. You must obtain the necessary rights to use and redistribute them. No bundled font, IBM asset, system-font lookup, or network download is needed.

Each 8×8 crop tile is matched against exact font masks and observed colors. A nearest candidate and its pixel-error positions are diagnostic evidence only. `selected_code` is absent/null when no candidate is selected; unknown and unresolved ambiguous text is `?`. An expectation can select only an existing exact match. It does not hide mask errors or fill missing pixels. Exact but ambiguous evidence can be successful while recognition remains incomplete: inspect the recognition and ambiguity fields, not only `success`.

A solid-color tile can hide any glyph when foreground equals background, so all font codes remain candidates. A blank-looking image alone cannot prove that its character is a space. The expected substring is a caller constraint, never an independent observation of invisible text.

## Output files

| Command/mode | Appended extensions |
| --- | --- |
| `replay` | `.txt`, `.json` |
| `analyze` | `.png`, `.txt`, `.json` |
| `image` | `.original.png`, `.png`, `.txt`, `.json` |
| `capture --mode text` | `.pty`, `.capture.json`, `.txt`, `.json` |
| `capture --mode bitmap` | `.pty`, `.capture.json`, `.png`, `.txt`, `.json` |

PNG reconstruction uses RGBA and the region dimensions. Unobserved/invalid pixels are transparent. `.original.png` preserves the exact imported file bytes, including metadata and encoding. `.pty` preserves captured bytes, including control sequences; it is not normalized text.

### Text replay JSON

`schema_version: 1`, `kind: "terminal_text"`, `success`, `terminal_columns`, `terminal_rows`, `terminal_snapshot`, `decoded_text`, `diagnostics`, and `terminal_cells` describe the selected terminal snapshot. Only written cells are emitted. Each cell includes zero-based `column`/`row`, `character`, `codepoint`, `text`, `explicitly_written`, `style`, `foreground_rgb`, and `background_rgb`. The text field preserves cell text beyond a single codepoint.

`style` records original foreground/background color specifications and `bold`, `underline`, `blink`, `inverse`, and `invisible` flags. RGB values are effective rendered colors. Default, ANSI, and indexed colors use a deterministic approximation of a host palette; explicit RGB is not a screenshot measurement of the host terminal.

### Bitmap/image JSON

Core fields include `success`, `guest_width`, `guest_height`, `region`, `decoded_text`, `diagnostics`, `missing_pixel_count`, `incomplete_scanlines`, `glyphs`, and terminal evidence where applicable. `recognition_performed`, `recognition_complete`, and `ambiguous_glyph_count` distinguish reconstruction from recognition. With no font, `glyphs` and decoded text are empty; that is not a blank recognized screen.

Each glyph reports zero-based `column`/`row`, absolute `x`/`y`, `colors`, `status`, `alternatives`, `selected_code`, `nearest`, `confidence`, and `selection_basis`. Status distinguishes `matched`, `ambiguous`, `rom_mismatch`, `unobserved_pixels`, and `too_many_colors`. Exact alternatives include CP437 code, actual Unicode character, foreground/background RGB, and eight-byte font mask; nearest candidates additionally expose Hamming `distance` and absolute `error_pixels`. An unobservable foreground/background on a solid tile is null. `confidence` is mask similarity `1 - distance/64` (zero when comparison is unavailable), not the probability of a character: ambiguous exact alternatives all have similarity 1. `selection_basis` is `unique_exact`, `expected_text`, or null. Preserve all alternatives when handing evidence to another tool.

### Capture lifecycle JSON

`exit_code` and `signal` are nullable child termination details. `timed_out`, `output_limit_reached`, and `bytes_captured` describe collection limits. This file is independent of the analysis `.json`: a readable final screen does not make a timed-out or failed child successful.

## Exit statuses

- `0`: replay/analysis succeeded, and any captured child succeeded.
- `1`: replay/analysis reported failure after a successful child, if any.
- `2`: invocation, input, spawn, or I/O error.
- Nonzero captured child exit: preserved as the command status.
- Captured child signal: `128 + signal` (for example, SIGTERM → 143).
- `124`: capture deadline exceeded.
- `125`: capture byte limit reached.

A child's own exit can have the same numeric value as a tool status. Consult `.capture.json` to distinguish them. Analysis failure does not override a nonzero child status. Do not treat the mere existence of output files as proof of success.
