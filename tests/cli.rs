// SPDX-License-Identifier: MIT-0
// Copyright (C) 2026 Simplebooks Foundation
// Copyright (C) 2026 Josh Rodd

use serde_json::{json, Value};
use std::fs;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
#[cfg(unix)]
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn command(mode: &str, prefix: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tigt-gfxreader"));
    command.arg(mode).arg("--output").arg(prefix);
    command
}

fn run(command: &mut Command, input: &[u8]) -> Output {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

fn assert_status(output: &Output, status: i32) {
    assert_eq!(
        output.status.code(),
        Some(status),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn artifact(prefix: &Path, suffix: &str) -> PathBuf {
    let mut path = prefix.as_os_str().to_os_string();
    path.push(suffix);
    PathBuf::from(path)
}

fn report(prefix: &Path, suffix: &str) -> Value {
    serde_json::from_slice(&fs::read(artifact(prefix, suffix)).unwrap()).unwrap()
}

fn rgba(prefix: &Path) -> (u32, u32, Vec<u8>) {
    let bytes = fs::read(artifact(prefix, ".png")).unwrap();
    let mut reader = png::Decoder::new(Cursor::new(bytes)).read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut pixels).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgba);
    assert_eq!(info.bit_depth, png::BitDepth::Eight);
    pixels.truncate(info.buffer_size());
    (info.width, info.height, pixels)
}

#[test]
fn replay_file_and_stdin_preserve_text_truecolor_and_styles() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.pty");
    let bytes = b"\x1b[38;2;12;34;56;48;2;90;80;70;1;4;5;7mA\x1b[0m\r\nB";
    fs::write(&input, bytes).unwrap();
    let from_file = dir.path().join("file");
    let from_stdin = dir.path().join("stdin");
    assert_status(
        &run(
            command("replay", &from_file).arg("--capture").arg(&input),
            b"",
        ),
        0,
    );
    assert_status(
        &run(
            command("replay", &from_stdin).args(["--capture", "-"]),
            bytes,
        ),
        0,
    );
    assert_eq!(fs::read(artifact(&from_file, ".txt")).unwrap(), b"A\nB\n");
    let replay = report(&from_file, ".json");
    assert_eq!(replay, report(&from_stdin, ".json"));
    assert_eq!(replay["kind"], "terminal_text");
    assert_eq!(replay["terminal_columns"], 320);
    assert_eq!(replay["terminal_rows"], 80);
    let cells = replay["terminal_cells"].as_array().unwrap();
    let a = cells
        .iter()
        .find(|cell| cell["column"] == 0 && cell["row"] == 0)
        .unwrap();
    assert_eq!(a["text"], "A");
    assert_eq!(a["foreground_rgb"], json!([90, 80, 70]));
    assert_eq!(a["background_rgb"], json!([12, 34, 56]));
    assert_eq!(a["style"]["bold"], true);
    assert_eq!(a["style"]["underline"], true);
    assert_eq!(a["style"]["blink"], true);
    assert_eq!(a["style"]["inverse"], true);
    assert_eq!(a["style"]["foreground"]["value"], json!([12, 34, 56]));
    let b = cells
        .iter()
        .find(|cell| cell["column"] == 0 && cell["row"] == 1)
        .unwrap();
    assert_eq!(b["text"], "B");
    assert_eq!(b["style"]["inverse"], false);
    assert!(!artifact(&from_file, ".png").exists());
    assert!(!artifact(&from_stdin, ".png").exists());
}

#[test]
fn replay_selects_last_alternate_screen_not_restored_primary() {
    let dir = TempDir::new().unwrap();
    let prefix = dir.path().join("alternate");
    let bytes = "shell\x1b[?1049h\x1b[2J\x1b[Hframe e\u{301}\x1b[?1049lafter";
    assert_status(
        &run(
            command("replay", &prefix).args(["--capture", "-"]),
            bytes.as_bytes(),
        ),
        0,
    );
    let replay = report(&prefix, ".json");
    assert_eq!(replay["terminal_snapshot"], "last_alternate_before_exit");
    assert_eq!(replay["decoded_text"], "frame e\u{301}\n");
    assert_eq!(
        fs::read_to_string(artifact(&prefix, ".txt")).unwrap(),
        "frame e\u{301}\n"
    );
}

fn painted_tile(column: usize, columns: usize) -> Vec<u8> {
    let mut transcript = String::from("\x1b[38;2;17;34;51m");
    for row in 1..=3 {
        transcript.push_str(&format!("\x1b[{row};{column}H{}", "█".repeat(columns)));
    }
    transcript.into_bytes()
}

#[test]
fn bitmap_without_font_reconstructs_pixels_without_claiming_ocr() {
    let dir = TempDir::new().unwrap();
    let prefix = dir.path().join("bitmap");
    assert_status(
        &run(
            command("analyze", &prefix).args(["--capture", "-", "--region", "0,0,8,8"]),
            &painted_tile(1, 4),
        ),
        0,
    );
    let (width, height, pixels) = rgba(&prefix);
    assert_eq!((width, height), (8, 8));
    assert_eq!(pixels, [17, 34, 51, 255].repeat(64));
    let bitmap = report(&prefix, ".json");
    assert_eq!(bitmap["recognition_performed"], false);
    assert_eq!(bitmap["glyphs"], json!([]));
    assert_eq!(bitmap["missing_pixel_count"], 0);
    assert_eq!(fs::read(artifact(&prefix, ".txt")).unwrap(), b"");
}

#[test]
fn bitmap_missing_pixels_remain_transparent_and_fail_analysis() {
    let dir = TempDir::new().unwrap();
    let prefix = dir.path().join("partial");
    assert_status(
        &run(
            command("analyze", &prefix).args(["--capture", "-", "--region", "0,0,8,8"]),
            "\x1b[38;2;17;34;51m█".as_bytes(),
        ),
        1,
    );
    let (_, _, pixels) = rgba(&prefix);
    for y in 0..8 {
        for x in 0..8 {
            let offset = (y * 8 + x) * 4;
            let expected = if x < 2 && y < 3 {
                [17, 34, 51, 255]
            } else {
                [0, 0, 0, 0]
            };
            assert_eq!(&pixels[offset..offset + 4], &expected);
        }
    }
    assert_eq!(report(&prefix, ".json")["missing_pixel_count"], 58);
}

#[test]
fn bitmap_160_uses_doubled_horizontal_pixels() {
    let dir = TempDir::new().unwrap();
    let prefix = dir.path().join("narrow");
    assert_status(
        &run(
            command("analyze", &prefix).args([
                "--capture",
                "-",
                "--width",
                "160",
                "--region",
                "0,0,8,8",
            ]),
            &painted_tile(1, 8),
        ),
        0,
    );
    assert_eq!(rgba(&prefix).2, [17, 34, 51, 255].repeat(64));
    assert_eq!(report(&prefix, ".json")["guest_width"], 160);
}

#[test]
fn bitmap_640_can_reconstruct_rightmost_terminal_columns() {
    let dir = TempDir::new().unwrap();
    let prefix = dir.path().join("wide");
    assert_status(
        &run(
            command("analyze", &prefix).args([
                "--capture",
                "-",
                "--width",
                "640",
                "--region",
                "632,0,8,8",
            ]),
            &painted_tile(317, 4),
        ),
        0,
    );
    assert_eq!(rgba(&prefix).2, [17, 34, 51, 255].repeat(64));
    assert_eq!(report(&prefix, ".json")["region"]["x"], 632);
}

const A_MASK: [u8; 8] = [0x18, 0x24, 0x42, 0x7e, 0x42, 0x42, 0x42, 0];

fn image_fixture(dir: &Path) -> (PathBuf, Vec<u8>, Vec<u8>) {
    let font_path = dir.join("font.bin");
    let mut font = vec![0; 5 + 66 * 8];
    font[..5].copy_from_slice(b"font!");
    font[5 + 65 * 8..5 + 66 * 8].copy_from_slice(&A_MASK);
    fs::write(&font_path, font).unwrap();
    let mut pixels = vec![0; 320 * 200 * 3];
    let mut tile = Vec::new();
    for (y, mask) in A_MASK.iter().enumerate() {
        for x in 0..8 {
            let rgb = if mask & (0x80 >> x) != 0 {
                [240, 120, 60]
            } else {
                [0, 0, 0]
            };
            pixels[(y * 320 + x) * 3..(y * 320 + x + 1) * 3].copy_from_slice(&rgb);
            tile.extend_from_slice(&rgb);
            tile.push(255);
        }
    }
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 320, 200);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .add_text_chunk(
                "Fixture".into(),
                "Generated; preserve these input bytes".into(),
            )
            .unwrap();
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&pixels).unwrap();
        writer.finish().unwrap();
    }
    (font_path, bytes, tile)
}

fn image_command(prefix: &Path, font: &Path) -> Command {
    let mut command = command("image", prefix);
    command.arg("--rom").arg(font).args([
        "--font-offset",
        "5",
        "--glyph-count",
        "66",
        "--region",
        "0,0,8,8",
        "--expect",
        "A",
    ]);
    command
}

#[test]
fn image_file_and_stdin_keep_original_bytes_and_native_crop_pixels() {
    let dir = TempDir::new().unwrap();
    let (font, bytes, expected) = image_fixture(dir.path());
    let input = dir.path().join("input.png");
    fs::write(&input, &bytes).unwrap();
    let from_file = dir.path().join("image-file");
    let from_stdin = dir.path().join("image-stdin");
    assert_status(
        &run(
            image_command(&from_file, &font).arg("--input").arg(&input),
            b"",
        ),
        0,
    );
    assert_status(
        &run(
            image_command(&from_stdin, &font).args(["--input", "-"]),
            &bytes,
        ),
        0,
    );
    assert_eq!(
        fs::read(artifact(&from_file, ".original.png")).unwrap(),
        bytes
    );
    assert_eq!(
        fs::read(artifact(&from_stdin, ".original.png")).unwrap(),
        bytes
    );
    assert_eq!(rgba(&from_file), (8, 8, expected.clone()));
    assert_eq!(rgba(&from_stdin), (8, 8, expected));
    let image = report(&from_file, ".json");
    assert_eq!(image, report(&from_stdin, ".json"));
    assert_eq!(image["guest_width"], 320);
    assert_eq!(image["guest_height"], 200);
    assert_eq!(image["glyphs"][0]["selected_code"], 65);
    assert_eq!(image["expected_found"], true);
    assert_eq!(fs::read(artifact(&from_file, ".txt")).unwrap(), b"A\n");
}

#[test]
fn image_rejects_dimension_mismatch_instead_of_rescaling() {
    let dir = TempDir::new().unwrap();
    let (font, bytes, _) = image_fixture(dir.path());
    let input = dir.path().join("input.png");
    fs::write(&input, &bytes).unwrap();
    let prefix = dir.path().join("mismatch");
    assert_status(
        &run(
            image_command(&prefix, &font)
                .arg("--input")
                .arg(&input)
                .args(["--width", "640"]),
            b"",
        ),
        2,
    );
    assert!(!artifact(&prefix, ".png").exists());
}

#[test]
fn image_rejects_corrupt_png_and_short_font() {
    let dir = TempDir::new().unwrap();
    let (font, bytes, _) = image_fixture(dir.path());
    let corrupt = dir.path().join("corrupt");
    assert_status(
        &run(
            image_command(&corrupt, &font).args(["--input", "-"]),
            b"not a PNG",
        ),
        2,
    );
    assert!(!artifact(&corrupt, ".png").exists());
    fs::write(&font, [0; 8]).unwrap();
    let short = dir.path().join("short-font");
    assert_status(
        &run(image_command(&short, &font).args(["--input", "-"]), &bytes),
        2,
    );
    assert!(!artifact(&short, ".png").exists());
}

#[cfg(unix)]
#[test]
fn capture_preserves_raw_output_child_status_and_pty_dimensions() {
    let dir = TempDir::new().unwrap();
    let prefix = dir.path().join("child");
    assert_status(
        &run(
            command("capture", &prefix).args([
                "--",
                "/bin/sh",
                "-c",
                r"printf '\033[38;2;12;34;56mchild'; stty size; exit 7",
            ]),
            b"",
        ),
        7,
    );
    let raw = fs::read(artifact(&prefix, ".pty")).unwrap();
    assert!(raw.starts_with(b"\x1b[38;2;12;34;56mchild"));
    assert!(String::from_utf8_lossy(&raw).contains("80 320"));
    let lifecycle = report(&prefix, ".capture.json");
    assert_eq!(lifecycle["exit_code"], 7);
    assert_eq!(lifecycle["signal"], Value::Null);
    assert_eq!(lifecycle["timed_out"], false);
    assert_eq!(lifecycle["output_limit_reached"], false);
    assert_eq!(lifecycle["bytes_captured"], raw.len());
    let replay = report(&prefix, ".json");
    assert_eq!(replay["decoded_text"], "child80 320\n");
    assert_eq!(
        replay["terminal_cells"][0]["foreground_rgb"],
        json!([12, 34, 56])
    );
    assert!(!artifact(&prefix, ".png").exists());
}

#[cfg(unix)]
#[test]
fn capture_timeout_is_bounded_and_retains_partial_transcript() {
    let dir = TempDir::new().unwrap();
    let prefix = dir.path().join("timeout");
    let start = Instant::now();
    let output = run(
        command("capture", &prefix).args([
            "--timeout-ms",
            "500",
            "--",
            "/bin/sh",
            "-c",
            "printf ready; exec sleep 20",
        ]),
        b"",
    );
    assert!(start.elapsed() < Duration::from_secs(10));
    assert_status(&output, 124);
    assert_eq!(fs::read(artifact(&prefix, ".pty")).unwrap(), b"ready");
    let lifecycle = report(&prefix, ".capture.json");
    assert_eq!(lifecycle["timed_out"], true);
    assert_eq!(lifecycle["output_limit_reached"], false);
    assert_eq!(lifecycle["bytes_captured"], 5);
    assert_eq!(fs::read(artifact(&prefix, ".txt")).unwrap(), b"ready\n");
}

#[cfg(unix)]
#[test]
fn capture_byte_cap_stops_a_producer_and_preserves_only_bounded_bytes() {
    let dir = TempDir::new().unwrap();
    let prefix = dir.path().join("limited");
    let start = Instant::now();
    let output = run(
        command("capture", &prefix).args([
            "--max-bytes",
            "1024",
            "--timeout-ms",
            "5000",
            "--",
            "/bin/sh",
            "-c",
            "while :; do printf 0123456789abcdef; done",
        ]),
        b"",
    );
    assert!(start.elapsed() < Duration::from_secs(10));
    assert_status(&output, 125);
    let raw = fs::read(artifact(&prefix, ".pty")).unwrap();
    assert_eq!(raw, b"0123456789abcdef".repeat(64));
    let lifecycle = report(&prefix, ".capture.json");
    assert_eq!(lifecycle["output_limit_reached"], true);
    assert_eq!(lifecycle["timed_out"], false);
    assert_eq!(lifecycle["bytes_captured"], 1024);
}

#[cfg(unix)]
#[test]
fn capture_reports_child_signal_without_losing_prior_output() {
    let dir = TempDir::new().unwrap();
    let prefix = dir.path().join("signal");
    assert_status(
        &run(
            command("capture", &prefix).args([
                "--",
                "/bin/sh",
                "-c",
                "printf before; kill -TERM $$",
            ]),
            b"",
        ),
        143,
    );
    let lifecycle = report(&prefix, ".capture.json");
    assert_eq!(lifecycle["exit_code"], Value::Null);
    assert_eq!(lifecycle["signal"], 15);
    assert_eq!(fs::read(artifact(&prefix, ".pty")).unwrap(), b"before");
}

#[cfg(unix)]
#[test]
fn capture_spawn_error_preserves_an_empty_raw_artifact() {
    let dir = TempDir::new().unwrap();
    let prefix = dir.path().join("spawn-error");
    let missing = dir.path().join("nonexistent-executable");
    assert_status(
        &run(command("capture", &prefix).arg("--").arg(missing), b""),
        2,
    );
    assert_eq!(fs::read(artifact(&prefix, ".pty")).unwrap(), b"");
}

#[test]
fn image_ambiguity_stays_unknown_until_expect_selects_an_exact_candidate() {
    let dir = TempDir::new().unwrap();
    let (font, bytes, _) = image_fixture(dir.path());
    let mut duplicate = fs::read(&font).unwrap();
    duplicate[5 + 64 * 8..5 + 65 * 8].copy_from_slice(&A_MASK);
    fs::write(&font, duplicate).unwrap();
    let unresolved = dir.path().join("ambiguous");
    assert_status(
        &run(
            command("image", &unresolved).arg("--rom").arg(&font).args([
                "--input",
                "-",
                "--font-offset",
                "5",
                "--glyph-count",
                "66",
                "--region",
                "0,0,8,8",
            ]),
            &bytes,
        ),
        0,
    );
    let evidence = report(&unresolved, ".json");
    assert_eq!(evidence["recognition_complete"], false);
    assert_eq!(evidence["ambiguous_glyph_count"], 1);
    assert_eq!(evidence["glyphs"][0]["selected_code"], Value::Null);
    assert_eq!(evidence["glyphs"][0]["status"], "ambiguous");
    assert_eq!(fs::read(artifact(&unresolved, ".txt")).unwrap(), b"?\n");
    let resolved = dir.path().join("resolved");
    assert_status(
        &run(
            image_command(&resolved, &font).args(["--input", "-"]),
            &bytes,
        ),
        0,
    );
    let evidence = report(&resolved, ".json");
    assert_eq!(evidence["glyphs"][0]["selected_code"], 65);
    assert_eq!(evidence["expected_found"], true);
    assert_eq!(fs::read(artifact(&resolved, ".txt")).unwrap(), b"A\n");
    assert_eq!(rgba(&resolved), rgba(&unresolved));
}

#[cfg(unix)]
#[test]
fn capture_bitmap_mode_reconstructs_without_a_font() {
    let dir = TempDir::new().unwrap();
    let prefix = dir.path().join("captured-bitmap");
    let transcript = String::from_utf8(painted_tile(1, 4)).unwrap();
    assert_status(
        &run(
            command("capture", &prefix).args([
                "--mode",
                "bitmap",
                "--region",
                "0,0,8,8",
                "--",
                "/bin/sh",
                "-c",
                "printf '%s' \"$1\"",
                "fixture",
                &transcript,
            ]),
            b"",
        ),
        0,
    );
    assert_eq!(
        fs::read(artifact(&prefix, ".pty")).unwrap(),
        transcript.as_bytes()
    );
    assert_eq!(report(&prefix, ".capture.json")["exit_code"], 0);
    assert_eq!(report(&prefix, ".json")["recognition_performed"], false);
    assert_eq!(rgba(&prefix), (8, 8, [17, 34, 51, 255].repeat(64)));
}
