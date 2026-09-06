// SPDX-License-Identifier: MIT-0
// Copyright (C) 2026 Simplebooks Foundation
// Copyright (C) 2026 Josh Rodd

use clap::{Args, Parser, Subcommand, ValueEnum};
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, BufWriter, Read};
use std::path::{Path, PathBuf};
use tigt_gfxreader::{
    analyze, analyze_image, decode_png, reconstruct, replay_text, Analysis, Options, Region,
};

type Error = Box<dyn std::error::Error>;
const MAX_INPUT_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Parser)]
#[command(
    version,
    about = "Capture terminal screens and recognize exact bitmap glyphs with a caller-supplied font"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Args)]
struct BitmapArgs {
    /// Caller-owned font bytes; omitted for font-free terminal bitmap reconstruction.
    #[arg(long, alias = "font")]
    rom: Option<PathBuf>,
    /// Byte offset of 8-byte-per-glyph, MSB-left CP437 font; decimal or 0xHEX.
    #[arg(long, default_value = "0", value_parser = parse_integer)]
    font_offset: usize,
    #[arg(long, default_value = "128", value_parser = parse_integer)]
    glyph_count: usize,
    #[arg(long, default_value_t = 320)]
    width: usize,
    #[arg(long, default_value_t = 200)]
    height: usize,
    /// Native pixel crop x,y,w,h; all values must be multiples of 8.
    #[arg(long)]
    region: Option<Region>,
    /// Require printable ASCII substring, considering all exact glyph alternatives.
    #[arg(long)]
    expect: Option<String>,
}

impl BitmapArgs {
    fn options(&self) -> Options {
        Options {
            width: self.width,
            height: self.height,
            font_offset: self.font_offset,
            glyph_count: self.glyph_count,
            region: self.region,
            expect: self.expect.clone(),
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum Mode {
    Text,
    Bitmap,
}

#[derive(Subcommand)]
enum Command {
    /// Reconstruct sextant bitmap output from a raw terminal file/stream ("-" for stdin).
    Analyze {
        #[arg(long)]
        capture: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[command(flatten)]
        bitmap: BitmapArgs,
    },
    /// Recognize a native PNG; --rom is required. Preserve input as .original.png.
    Image {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[command(flatten)]
        bitmap: BitmapArgs,
    },
    /// Replay terminal text and RGB attributes directly, without bitmap interpretation.
    Replay {
        #[arg(long)]
        capture: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// Run explicit argv on an owned 320x80 PTY, save .pty/.capture.json and analyze.
    Capture {
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value_t = 10_000, value_parser = clap::value_parser!(u64).range(1..))]
        timeout_ms: u64,
        #[arg(long, default_value_t = 67_108_864)]
        max_bytes: usize,
        #[arg(long, value_enum, default_value = "text")]
        mode: Mode,
        #[command(flatten)]
        bitmap: BitmapArgs,
        /// Executable and arguments, after --. No implicit shell or shell expansion.
        #[arg(required = true, trailing_var_arg = true, num_args = 1..)]
        command: Vec<OsString>,
    },
}

fn parse_integer(value: &str) -> Result<usize, String> {
    let result = if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        usize::from_str_radix(hex, 16)
    } else {
        value.parse()
    };
    result.map_err(|_| format!("invalid nonnegative integer {value:?} (use decimal or 0xHEX)"))
}

fn output_path(prefix: &Path, extension: &str) -> PathBuf {
    let mut name = prefix.as_os_str().to_os_string();
    name.push(extension);
    PathBuf::from(name)
}

fn output_directory(prefix: &Path) -> io::Result<()> {
    if let Some(parent) = prefix.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn read_input(path: &Path) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    let mut source: Box<dyn Read> = if path == Path::new("-") {
        Box::new(io::stdin())
    } else {
        Box::new(File::open(path).map_err(|e| format!("opening {}: {e}", path.display()))?)
    };
    source
        .by_ref()
        .take(MAX_INPUT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_INPUT_BYTES {
        return Err("input exceeds the 64 MiB limit".into());
    }
    Ok(bytes)
}

fn write_analysis(output: &Path, analysis: Analysis) -> Result<u8, Error> {
    output_directory(output)?;
    let mut encoder = png::Encoder::new(
        BufWriter::new(File::create(output_path(output, ".png"))?),
        analysis.report.region.width as u32,
        analysis.report.region.height as u32,
    );
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&analysis.rgba)?;
    writer.finish()?;
    fs::write(
        output_path(output, ".json"),
        serde_json::to_vec_pretty(&analysis.report)?,
    )?;
    fs::write(output_path(output, ".txt"), &analysis.report.decoded_text)?;
    print!("{}", analysis.report.decoded_text);
    for diagnostic in analysis.report.diagnostics.iter().take(20) {
        eprintln!(
            "{} at {:?}: {}",
            diagnostic.kind, diagnostic.guest_position, diagnostic.message
        );
    }
    eprintln!(
        "{}: {} glyphs, {} ambiguous, {} diagnostics; snapshot={}; outputs: {}.{{png,json,txt}}",
        if analysis.report.success {
            "PASS"
        } else {
            "FAIL"
        },
        analysis.report.glyphs.len(),
        analysis.report.ambiguous_glyph_count,
        analysis.report.diagnostics.len(),
        analysis.report.terminal_snapshot,
        output.display()
    );
    Ok(u8::from(!analysis.report.success))
}

fn bitmap_capture(bytes: &[u8], output: &Path, bitmap: &BitmapArgs) -> Result<u8, Error> {
    let options = bitmap.options();
    let analysis = if let Some(path) = &bitmap.rom {
        analyze(bytes, &read_input(path)?, &options)?
    } else {
        reconstruct(bytes, &options)?
    };
    write_analysis(output, analysis)
}

fn text_capture(bytes: &[u8], output: &Path) -> Result<u8, Error> {
    let report = replay_text(bytes);
    output_directory(output)?;
    fs::write(
        output_path(output, ".json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    fs::write(output_path(output, ".txt"), &report.decoded_text)?;
    print!("{}", report.decoded_text);
    for diagnostic in &report.diagnostics {
        eprintln!("{}: {}", diagnostic.kind, diagnostic.message);
    }
    Ok(u8::from(!report.success))
}

#[cfg(unix)]
fn child_capture(
    output: &Path,
    timeout_ms: u64,
    max_bytes: usize,
    mode: Mode,
    bitmap: &BitmapArgs,
    command: &[OsString],
) -> Result<u8, Error> {
    use std::time::Duration;
    use tigt_gfxreader::capture::{capture_child, CaptureOptions};
    if max_bytes as u64 > MAX_INPUT_BYTES {
        return Err("--max-bytes cannot exceed the 64 MiB input limit".into());
    }
    if matches!(mode, Mode::Text)
        && (bitmap.rom.is_some() || bitmap.expect.is_some() || bitmap.region.is_some())
    {
        return Err("font, expect, and region options require --mode bitmap".into());
    }
    // Validate font/crop before launching any program with potentially invalid arguments.
    if bitmap.rom.as_deref() == Some(Path::new("-")) {
        return Err("child capture requires a font file, not stdin".into());
    }
    let font = bitmap
        .rom
        .as_ref()
        .map(|path| read_input(path))
        .transpose()?;
    let options = bitmap.options();
    if matches!(mode, Mode::Bitmap) {
        if let Some(font) = &font {
            options.validate(font.len())?;
        } else {
            options.validate_region()?;
            if options.expect.is_some() {
                return Err("expected text requires a recognition font".into());
            }
        }
    }
    output_directory(output)?;
    let transcript = output_path(output, ".pty");
    let outcome = capture_child(
        command,
        &transcript,
        &CaptureOptions {
            timeout: Duration::from_millis(timeout_ms),
            max_bytes,
        },
    )?;
    fs::write(
        output_path(output, ".capture.json"),
        serde_json::to_vec_pretty(&outcome)?,
    )?;
    let status = outcome.shell_status();
    let analysis = read_input(&transcript).and_then(|bytes| match mode {
        Mode::Text => text_capture(&bytes, output),
        Mode::Bitmap => {
            let analysis = if let Some(font) = &font {
                analyze(&bytes, font, &options)?
            } else {
                reconstruct(&bytes, &options)?
            };
            write_analysis(output, analysis)
        }
    });
    if status != 0 {
        if let Err(error) = analysis {
            eprintln!("analyzing captured output: {error}");
        }
        return Ok(status);
    }
    analysis
}

fn run(cli: Cli) -> Result<u8, Error> {
    match cli.command {
        Command::Analyze {
            capture,
            output,
            bitmap,
        } => {
            if capture == Path::new("-") && bitmap.rom.as_deref() == Some(Path::new("-")) {
                return Err("capture and font cannot both read stdin".into());
            }
            bitmap_capture(&read_input(&capture)?, &output, &bitmap)
        }
        Command::Replay { capture, output } => text_capture(&read_input(&capture)?, &output),
        Command::Image {
            input,
            output,
            bitmap,
        } => {
            let path = bitmap
                .rom
                .as_ref()
                .ok_or("image recognition requires --rom (caller-supplied font)")?;
            if input == Path::new("-") && path == Path::new("-") {
                return Err("image and font cannot both read stdin".into());
            }
            let original = read_input(&input)?;
            let image = decode_png(original.as_slice())?;
            let analysis = analyze_image(&image, &read_input(path)?, &bitmap.options())?;
            output_directory(&output)?;
            fs::write(output_path(&output, ".original.png"), original)?;
            write_analysis(&output, analysis)
        }
        Command::Capture {
            output,
            timeout_ms,
            max_bytes,
            mode,
            bitmap,
            command,
        } => {
            #[cfg(unix)]
            {
                child_capture(&output, timeout_ms, max_bytes, mode, &bitmap, &command)
            }
            #[cfg(not(unix))]
            {
                let _ = (output, timeout_ms, max_bytes, mode, bitmap, command);
                Err("PTY child capture requires Unix (macOS or Linux)".into())
            }
        }
    }
}

fn main() -> std::process::ExitCode {
    match run(Cli::parse()) {
        Ok(code) => std::process::ExitCode::from(code),
        Err(error) => {
            eprintln!("tigt-gfxreader: {error}");
            std::process::ExitCode::from(2)
        }
    }
}
