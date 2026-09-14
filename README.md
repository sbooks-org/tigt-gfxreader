<!-- SPDX-License-Identifier: MIT-0 -->
<!-- Copyright (C) 2026 Simplebooks Foundation -->
<!-- Copyright (C) 2026 Josh Rodd -->

# tigt-gfxreader

A standalone Rust CLI and library for turning terminal transcripts or exact-grid PNGs into inspectable evidence. It can replay terminal text and attributes, reconstruct sextant graphics, and optionally compare 8×8 bitmap glyphs against a font you supply. It does not depend on tigt, an emulator, installed system fonts, proprietary assets, or a vision service.

## Build

Install stable Rust, then run:

```sh
cargo build --release
./target/release/tigt-gfxreader --help
```

Owned PTY capture supports macOS and Linux. The manual CI matrix is configured to check formatting, all targets, and Clippy on both platforms. Local Linux Docker verification is provided by [tigt](https://github.com/sbooks-org/tigt/blob/main/docs/instrumentation.md#local-docker-ci), using both local checkouts without hosted credentials.

## Choose a path

```sh
# Terminal text, RGB colors, and style attributes; no bitmap OCR.
tigt-gfxreader replay --capture session.pty --output out/text

# Reconstruct a sextant bitmap without a font or OCR.
tigt-gfxreader analyze --capture session.pty --output out/bitmap

# Compare an exact-grid PNG against your own 8×8 font.
tigt-gfxreader image --input screen.png --rom font.bin \
  --font-offset 0 --glyph-count 128 --expect READY --output out/image

# Run an explicit executable in a bounded 320-column × 80-row PTY.
tigt-gfxreader capture --output out/session --timeout-ms 10000 \
  --max-bytes 67108864 --mode text -- /path/to/program argument

# A transcript may also arrive on standard input.
cat session.pty | tigt-gfxreader replay --capture - --output out/piped
```

`replay` writes `.txt` and `.json`. `analyze` and `image` also write a cropped RGBA `.png`; `image` saves the exact input bytes as `.original.png`. `capture` first saves raw `.pty` bytes and `.capture.json` lifecycle information, then produces the text or bitmap outputs for its selected mode. Extensions are appended to the output prefix.

## Deliberate limits

- Bitmap inputs are exact grids: **160×200, 320×200, or 640×200**, default 320×200. There is no image resizing, antialias removal, or automatic glyph-grid discovery.
- Transcript replay uses a fixed **320×80 terminal**, not the current host terminal. Its default/indexed RGB palette approximates a host palette; explicit truecolor values are preserved.
- Input-mode controls, including DEC save/restore for bracketed paste and mouse reporting, leave the visible transcript unchanged. Unsupported display-affecting saved modes remain errors rather than guessed screen state.
- The last alternate-screen snapshot survives a normal application exit. Missing or invalid bitmap pixels are transparent, not silently filled in.
- Bitmap OCR is optional for `analyze` and bitmap `capture`; `image` requires a caller-supplied font. Unknown or ambiguous glyphs are `?`, unless an exact `--expect` candidate resolves an ambiguity. Text replay is a separate operation, not OCR.
- No font or ROM is bundled. You are responsible for permission to use and distribute any font bytes you supply.

See [usage](docs/usage.md), the [format and CLI reference](docs/reference.md), and [capture/instrumentation and vision handoff](docs/instrumentation.md).

## Library

```rust,no_run
use tigt_gfxreader::{analyze_image, decode_png, Options};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let image = decode_png(std::fs::File::open("screen.png")?)?;
    let font = std::fs::read("font.bin")?;
    let result = analyze_image(&image, &font, &Options {
        width: image.width,
        height: image.height,
        font_offset: 0,
        glyph_count: 128,
        region: None,
        expect: None,
    })?;
    println!("{}", result.report.decoded_text);
    // result.rgba is the cropped image; image.rgba retains all decoded input samples.
    Ok(())
}
```

`analyze_rgb` accepts tightly packed RGB samples; `analyze(capture, font, &Options)` preserves the original terminal-recognition API. `reconstruct(capture, &Options)` reconstructs a bitmap without a font; `replay_text(capture)` returns text and cell attributes without OCR. `terminal::Terminal::replay` remains available for lower-level screen inspection. On Unix, `capture::capture_child` accepts an explicit argv, raw transcript path, and finite `CaptureOptions`; its outcome preserves exit code/signal and collection limits.

## License

The project is [MIT-0](LICENSE). That license does not grant rights to third-party input files, fonts, or captured application content.
