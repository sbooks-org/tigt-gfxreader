// SPDX-License-Identifier: MIT-0
// Copyright (C) 2026 Simplebooks Foundation
// Copyright (C) 2026 Josh Rodd

#[cfg(unix)]
pub mod capture;
mod image;
pub mod terminal;
pub use image::{analyze_image, analyze_rgb, decode_png, Image};

use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use terminal::{Style, Terminal, COLS};

#[derive(Clone, Copy, Debug, Serialize, Eq, PartialEq)]
pub struct Region {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

impl std::str::FromStr for Region {
    type Err = String;
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let values: Vec<usize> = input
            .split(',')
            .map(str::parse)
            .collect::<Result<_, _>>()
            .map_err(|_| "region must be four nonnegative integers: x,y,w,h".to_string())?;
        if values.len() != 4 {
            return Err("region must be x,y,w,h".into());
        }
        Ok(Self {
            x: values[0],
            y: values[1],
            width: values[2],
            height: values[3],
        })
    }
}

pub struct Options {
    pub width: usize,
    pub height: usize,
    pub font_offset: usize,
    pub glyph_count: usize,
    pub region: Option<Region>,
    pub expect: Option<String>,
}

impl Options {
    pub fn validate(&self, rom_len: usize) -> Result<Region, String> {
        if !(1..=256).contains(&self.glyph_count) {
            return Err("glyph-count must be in 1..=256".into());
        }
        let end = self
            .glyph_count
            .checked_mul(8)
            .and_then(|n| self.font_offset.checked_add(n))
            .ok_or("font range overflows address space")?;
        if end > rom_len {
            return Err(format!(
                "font has {rom_len} bytes; offset {} and {} glyphs require {end} bytes",
                self.font_offset, self.glyph_count
            ));
        }
        self.validate_region()
    }

    /// Validate dimensions and crop without requiring recognition font bytes.
    pub fn validate_region(&self) -> Result<Region, String> {
        if !matches!(self.width, 160 | 320 | 640) || self.height != 200 {
            return Err("guest dimensions must be 160x200, 320x200, or 640x200".into());
        }
        let region = self.region.unwrap_or(Region {
            x: 0,
            y: 0,
            width: self.width,
            height: self.height,
        });
        if region.width == 0
            || region.height == 0
            || region
                .x
                .checked_add(region.width)
                .is_none_or(|n| n > self.width)
            || region
                .y
                .checked_add(region.height)
                .is_none_or(|n| n > self.height)
        {
            return Err("region must be nonempty and entirely within guest dimensions".into());
        }
        if [region.x, region.y, region.width, region.height]
            .iter()
            .any(|v| v % 8 != 0)
        {
            return Err("region x,y,w,h must all be multiples of 8 (guest glyph grid)".into());
        }
        if self
            .expect
            .as_ref()
            .is_some_and(|s| !s.is_ascii() || s.bytes().any(|b| !(32..=126).contains(&b)))
        {
            return Err(
                "expect must contain printable ASCII (matched within one glyph row)".into(),
            );
        }
        Ok(region)
    }
}

pub fn sextant_mask(c: char) -> Option<u8> {
    match c {
        ' ' => Some(0),
        '\u{258c}' => Some(21),
        '\u{2590}' => Some(42),
        '\u{2588}' => Some(63),
        '\u{1fb00}'..='\u{1fb13}' => Some((c as u32 - 0x1fb00 + 1) as u8),
        '\u{1fb14}'..='\u{1fb27}' => Some((c as u32 - 0x1fb14 + 22) as u8),
        '\u{1fb28}'..='\u{1fb3b}' => Some((c as u32 - 0x1fb28 + 43) as u8),
        _ => None,
    }
}

#[derive(Debug, Serialize)]
pub struct Diagnostic {
    pub kind: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guest_position: Option<[usize; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_position: Option<[usize; 2]>,
}

#[derive(Debug, Serialize)]
pub struct TerminalCell {
    pub column: usize,
    pub row: usize,
    pub character: char,
    pub codepoint: u32,
    pub text: String,
    pub sextant_mask: Option<u8>,
    pub explicitly_written: bool,
    pub style: Style,
    pub foreground_rgb: [u8; 3],
    pub background_rgb: [u8; 3],
}

#[derive(Clone, Debug, Serialize)]
pub struct GlyphMatch {
    pub code: usize,
    pub character: String,
    pub foreground_rgb: Option<[u8; 3]>,
    pub background_rgb: Option<[u8; 3]>,
    pub mask: [u8; 8],
}

#[derive(Debug, Serialize)]
pub struct NearestGlyph {
    #[serde(flatten)]
    pub candidate: GlyphMatch,
    pub distance: u32,
    pub error_pixels: Vec<[usize; 2]>,
}

#[derive(Debug, Serialize)]
pub struct Glyph {
    pub column: usize,
    pub row: usize,
    pub x: usize,
    pub y: usize,
    pub colors: Vec<[u8; 3]>,
    pub status: &'static str,
    pub alternatives: Vec<GlyphMatch>,
    pub selected_code: Option<usize>,
    pub nearest: Option<NearestGlyph>,
    /// Exact mask similarity, 1 - nearest Hamming distance / 64; not a probability.
    pub confidence: f64,
    pub selection_basis: Option<&'static str>,
}

#[derive(Serialize)]
pub struct Report {
    pub schema_version: u32,
    pub kind: &'static str,
    pub recognition_performed: bool,
    pub recognition_complete: bool,
    pub ambiguous_glyph_count: usize,
    pub success: bool,
    pub guest_width: usize,
    pub guest_height: usize,
    pub region: Region,
    pub terminal_columns: usize,
    pub terminal_rows: usize,
    pub terminal_snapshot: &'static str,
    pub font_offset: usize,
    pub glyph_count: usize,
    pub expected: Option<String>,
    pub expected_found: Option<bool>,
    pub missing_pixel_count: usize,
    pub incomplete_scanlines: Vec<usize>,
    pub diagnostics: Vec<Diagnostic>,
    pub glyphs: Vec<Glyph>,
    pub decoded_text: String,
    pub terminal_cells: Vec<TerminalCell>,
}

pub struct Analysis {
    pub report: Report,
    /// RGBA, cropped to the region; unobserved/corrupt pixels are transparent,
    /// never silently synthesized as a successful black background.
    pub rgba: Vec<u8>,
}

/// IBM PC display-glyph mapping, including the graphical control-code positions.
pub fn cp437_character(code: u8) -> char {
    const LOW: [char; 32] = [
        '\0', '☺', '☻', '♥', '♦', '♣', '♠', '•', '◘', '○', '◙', '♂', '♀', '♪', '♫', '☼', '►', '◄',
        '↕', '‼', '¶', '§', '▬', '↨', '↑', '↓', '→', '←', '∟', '↔', '▲', '▼',
    ];
    const HIGH: [char; 128] = [
        'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å', 'É', 'æ',
        'Æ', 'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', '¢', '£', '¥', '₧', 'ƒ', 'á', 'í', 'ó', 'ú',
        'ñ', 'Ñ', 'ª', 'º', '¿', '⌐', '¬', '½', '¼', '¡', '«', '»', '░', '▒', '▓', '│', '┤', '╡',
        '╢', '╖', '╕', '╣', '║', '╗', '╝', '╜', '╛', '┐', '└', '┴', '┬', '├', '─', '┼', '╞', '╟',
        '╚', '╔', '╩', '╦', '╠', '═', '╬', '╧', '╨', '╤', '╥', '╙', '╘', '╒', '╓', '╫', '╪', '┘',
        '┌', '█', '▄', '▌', '▐', '▀', 'α', 'ß', 'Γ', 'π', 'Σ', 'σ', 'µ', 'τ', 'Φ', 'Θ', 'Ω', 'δ',
        '∞', 'φ', 'ε', '∩', '≡', '±', '≥', '≤', '⌠', '⌡', '÷', '≈', '°', '∙', '·', '√', 'ⁿ', '²',
        '■', '\u{a0}',
    ];
    match code {
        0..=31 => LOW[usize::from(code)],
        32..=126 => char::from(code),
        127 => '⌂',
        _ => HIGH[usize::from(code - 128)],
    }
}

fn character(code: usize) -> String {
    cp437_character(code as u8).to_string()
}

fn classify(x: usize, y: usize, pixels: &[Option<[u8; 3]>; 64], font: &[[u8; 8]]) -> Glyph {
    let colors: Vec<_> = pixels
        .iter()
        .flatten()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut glyph = Glyph {
        column: x / 8,
        row: y / 8,
        x,
        y,
        colors,
        status: "rom_mismatch",
        alternatives: Vec::new(),
        selected_code: None,
        nearest: None,
        confidence: 0.0,
        selection_basis: None,
    };
    if pixels.iter().any(Option::is_none) {
        glyph.status = "unobserved_pixels";
        return glyph;
    }
    if glyph.colors.len() > 2 {
        glyph.status = "too_many_colors";
        return glyph;
    }
    let mut masks = Vec::new();
    if glyph.colors.len() == 1 {
        // A solid tile cannot exclude a nonuniform mask: foreground may equal
        // background. Retain every possible glyph instead of guessing a space.
        let color = glyph.colors[0];
        for (code, mask) in font.iter().enumerate() {
            glyph.alternatives.push(GlyphMatch {
                code,
                character: character(code),
                foreground_rgb: if *mask == [0; 8] { None } else { Some(color) },
                background_rgb: if *mask == [255; 8] { None } else { Some(color) },
                mask: *mask,
            });
        }
    } else {
        for foreground in &glyph.colors {
            let background = glyph
                .colors
                .iter()
                .copied()
                .find(|c| c != foreground)
                .unwrap();
            let mut mask = [0u8; 8];
            for (i, pixel) in pixels.iter().enumerate() {
                if pixel.as_ref() == Some(foreground) {
                    mask[i / 8] |= 0x80 >> (i % 8);
                }
            }
            masks.push((mask, Some(*foreground), Some(background)));
        }
    }
    for (code, rom) in font.iter().enumerate() {
        for (mask, foreground, background) in &masks {
            let distance: u32 = mask
                .iter()
                .zip(rom)
                .map(|(a, b)| (a ^ b).count_ones())
                .sum();
            let is_nearest = glyph
                .nearest
                .as_ref()
                .is_none_or(|best| distance < best.distance);
            if distance != 0 && !is_nearest {
                continue;
            }
            let candidate = GlyphMatch {
                code,
                character: character(code),
                foreground_rgb: *foreground,
                background_rgb: *background,
                mask: *rom,
            };
            if distance == 0 {
                glyph.alternatives.push(candidate.clone());
            }
            if is_nearest {
                let mut error_pixels = Vec::new();
                for row in 0..8 {
                    for column in 0..8 {
                        if (mask[row] ^ rom[row]) & (0x80 >> column) != 0 {
                            error_pixels.push([x + column, y + row]);
                        }
                    }
                }
                glyph.nearest = Some(NearestGlyph {
                    candidate,
                    distance,
                    error_pixels,
                });
            }
        }
    }
    if !glyph.alternatives.is_empty() {
        glyph.confidence = 1.0;
        if glyph.alternatives.len() == 1 {
            glyph.status = "matched";
            glyph.selected_code = Some(glyph.alternatives[0].code);
            glyph.selection_basis = Some("unique_exact");
        } else {
            glyph.status = "ambiguous";
        }
        glyph.nearest = None;
    } else if let Some(nearest) = &glyph.nearest {
        glyph.confidence = 1.0 - f64::from(nearest.distance) / 64.0;
    }
    glyph
}

/// Replay sextant output and recognize 8x8 glyphs. Existing renderer-test API.
pub fn analyze(capture: &[u8], rom: &[u8], options: &Options) -> Result<Analysis, String> {
    options.validate(rom.len())?;
    analyze_terminal(capture, Some(rom), options)
}

/// Reconstruct terminal bitmap pixels without doing recognition or requiring a font.
pub fn reconstruct(capture: &[u8], options: &Options) -> Result<Analysis, String> {
    if options.expect.is_some() {
        return Err("expected text requires a recognition font".into());
    }
    analyze_terminal(capture, None, options)
}

fn analyze_terminal(
    capture: &[u8],
    rom: Option<&[u8]>,
    options: &Options,
) -> Result<Analysis, String> {
    let region = options.validate_region()?;
    let terminal = Terminal::replay(capture);
    let (cells, snapshot) = terminal.cells();
    let mut diagnostics: Vec<_> = terminal
        .errors
        .iter()
        .map(|error| Diagnostic {
            kind: "terminal_replay",
            message: error.clone(),
            guest_position: None,
            terminal_position: None,
        })
        .collect();
    let horizontal_scale = if options.width == 160 { 2 } else { 1 };
    let mut used_cells = BTreeMap::new();
    let mut invalid_cells = BTreeSet::new();
    let mut pixels = vec![None; region.width * region.height];
    let mut incomplete_scanlines = BTreeSet::new();
    for local_y in 0..region.height {
        let y = region.y + local_y;
        for local_x in 0..region.width {
            let x = region.x + local_x;
            let terminal_x = x * horizontal_scale / 2;
            let terminal_y = y / 3;
            let cell = &cells[terminal_y * COLS + terminal_x];
            let (fg, bg) = cell.style.colors();
            let mask = sextant_mask(cell.character);
            used_cells
                .entry((terminal_y, terminal_x))
                .or_insert_with(|| TerminalCell {
                    column: terminal_x,
                    row: terminal_y,
                    character: cell.character,
                    codepoint: cell.character as u32,
                    text: cell.text(),
                    sextant_mask: mask,
                    explicitly_written: cell.written,
                    style: cell.style,
                    foreground_rgb: fg,
                    background_rgb: bg,
                });
            let reason = if !cell.written {
                Some((
                    "missing_cell",
                    "graphics cell was not explicitly painted".to_string(),
                ))
            } else if cell.combined {
                Some((
                    "invalid_sextant",
                    "graphics cell contains combining characters".into(),
                ))
            } else if mask.is_none() {
                Some((
                    "invalid_sextant",
                    format!(
                        "unsupported graphics character U+{:04X}",
                        cell.character as u32
                    ),
                ))
            } else {
                None
            };
            if let Some((kind, message)) = reason {
                if invalid_cells.insert((terminal_y, terminal_x)) {
                    diagnostics.push(Diagnostic {
                        kind,
                        message,
                        guest_position: Some([x, y]),
                        terminal_position: Some([terminal_x, terminal_y]),
                    });
                }
                incomplete_scanlines.insert(y);
                continue;
            }
            let mask = mask.unwrap();
            let subpixel = (y % 3) * 2 + (x * horizontal_scale % 2);
            let color = if mask & (1 << subpixel) != 0 { fg } else { bg };
            if horizontal_scale == 2 {
                let right = if mask & (1 << (subpixel + 1)) != 0 {
                    fg
                } else {
                    bg
                };
                if color != right {
                    diagnostics.push(Diagnostic { kind: "unequal_horizontal_pair",
                        message: format!("160-pixel mode requires identical horizontal pairs: {color:?} != {right:?}"),
                        guest_position: Some([x, y]), terminal_position: Some([terminal_x, terminal_y]) });
                    incomplete_scanlines.insert(y);
                    continue;
                }
            }
            pixels[local_y * region.width + local_x] = Some(color);
        }
    }
    Ok(finish_pixels(
        pixels,
        rom,
        options,
        Report {
            schema_version: 1,
            kind: "terminal_bitmap",
            recognition_performed: rom.is_some(),
            recognition_complete: false,
            ambiguous_glyph_count: 0,
            success: false,
            guest_width: options.width,
            guest_height: options.height,
            region,
            terminal_columns: COLS,
            terminal_rows: terminal::ROWS,
            terminal_snapshot: snapshot,
            font_offset: options.font_offset,
            glyph_count: if rom.is_some() {
                options.glyph_count
            } else {
                0
            },
            expected: options.expect.clone(),
            expected_found: None,
            missing_pixel_count: 0,
            incomplete_scanlines: incomplete_scanlines.into_iter().collect(),
            diagnostics,
            glyphs: Vec::new(),
            decoded_text: String::new(),
            terminal_cells: used_cells.into_values().collect(),
        },
    ))
}

fn finish_pixels(
    pixels: Vec<Option<[u8; 3]>>,
    rom: Option<&[u8]>,
    options: &Options,
    mut report: Report,
) -> Analysis {
    let region = report.region;
    report.missing_pixel_count = pixels.iter().filter(|p| p.is_none()).count();
    let font: Vec<[u8; 8]> = rom
        .map(|rom| {
            rom[options.font_offset..options.font_offset + options.glyph_count * 8]
                .chunks_exact(8)
                .map(|chunk| chunk.try_into().unwrap())
                .collect()
        })
        .unwrap_or_default();
    let mut glyphs = Vec::new();
    if !font.is_empty() {
        for gy in (0..region.height).step_by(8) {
            for gx in (0..region.width).step_by(8) {
                let mut tile = [None; 64];
                for y in 0..8 {
                    tile[y * 8..y * 8 + 8].copy_from_slice(
                        &pixels[(gy + y) * region.width + gx..(gy + y) * region.width + gx + 8],
                    );
                }
                let glyph = classify(region.x + gx, region.y + gy, &tile, &font);
                if !matches!(glyph.status, "matched" | "ambiguous") {
                    let message = match glyph.status {
                        "too_many_colors" => format!(
                            "8x8 glyph contains {} colors; maximum is 2",
                            glyph.colors.len()
                        ),
                        "unobserved_pixels" => {
                            "8x8 glyph contains unobserved or invalid pixels".into()
                        }
                        _ => {
                            let nearest = glyph.nearest.as_ref().unwrap();
                            format!(
                            "no ROM glyph matches; nearest code 0x{:02X} ({}) differs in {} pixels",
                            nearest.candidate.code, nearest.candidate.character, nearest.distance
                        )
                        }
                    };
                    report.diagnostics.push(Diagnostic {
                        kind: glyph.status,
                        message,
                        guest_position: Some([glyph.x, glyph.y]),
                        terminal_position: None,
                    });
                }
                glyphs.push(glyph);
            }
        }
    }
    let columns = region.width / 8;
    report.expected_found = options.expect.as_ref().map(|expected| {
        if expected.is_empty() {
            return true;
        }
        let wanted = expected.as_bytes();
        if wanted.len() > columns {
            return false;
        }
        for row in 0..region.height / 8 {
            for column in 0..=columns - wanted.len() {
                let start = row * columns + column;
                if wanted.iter().enumerate().all(|(i, code)| {
                    glyphs[start + i]
                        .alternatives
                        .iter()
                        .any(|m| m.code == usize::from(*code))
                }) {
                    for (i, code) in wanted.iter().enumerate() {
                        glyphs[start + i].selected_code = Some(usize::from(*code));
                        glyphs[start + i].selection_basis = Some("expected_text");
                    }
                    return true;
                }
            }
        }
        false
    });
    if report.expected_found == Some(false) {
        report.diagnostics.push(Diagnostic {
            kind: "expected_text_missing",
            message: format!(
                "expected substring {:?} not found among ROM match alternatives",
                options.expect.as_ref().unwrap()
            ),
            guest_position: None,
            terminal_position: None,
        });
    }
    let mut decoded_text = String::new();
    for row in glyphs.chunks_exact(columns) {
        let line: String = row
            .iter()
            .map(|glyph| match glyph.selected_code {
                Some(code) => cp437_character(code as u8),
                None => '?',
            })
            .collect();
        decoded_text.push_str(line.trim_end_matches(' '));
        decoded_text.push('\n');
    }
    let mut rgba = Vec::with_capacity(pixels.len() * 4);
    for pixel in pixels {
        match pixel {
            Some(rgb) => {
                rgba.extend_from_slice(&rgb);
                rgba.push(255);
            }
            None => rgba.extend_from_slice(&[0, 0, 0, 0]),
        }
    }
    report.success = report.diagnostics.is_empty();
    report.recognition_complete =
        report.recognition_performed && glyphs.iter().all(|g| g.selected_code.is_some());
    report.ambiguous_glyph_count = glyphs.iter().filter(|g| g.alternatives.len() > 1).count();
    report.glyphs = glyphs;
    report.decoded_text = decoded_text;
    Analysis { report, rgba }
}

#[derive(Serialize)]
pub struct TextReport {
    pub schema_version: u32,
    pub kind: &'static str,
    pub success: bool,
    pub terminal_columns: usize,
    pub terminal_rows: usize,
    pub terminal_snapshot: &'static str,
    pub decoded_text: String,
    pub terminal_cells: Vec<TerminalCell>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Replay text and styles directly; Unicode text is never interpreted as sextant graphics.
pub fn replay_text(capture: &[u8]) -> TextReport {
    let terminal = Terminal::replay(capture);
    let (cells, snapshot) = terminal.cells();
    let mut decoded_text = String::new();
    let mut terminal_cells = Vec::new();
    for (row, line) in cells.chunks_exact(COLS).enumerate() {
        let mut text = String::new();
        for (column, cell) in line.iter().enumerate() {
            text.push_str(&cell.text());
            if !cell.written {
                continue;
            }
            let (foreground_rgb, background_rgb) = cell.style.colors();
            terminal_cells.push(TerminalCell {
                column,
                row,
                character: cell.character,
                codepoint: cell.character as u32,
                text: cell.text(),
                sextant_mask: sextant_mask(cell.character),
                explicitly_written: true,
                style: cell.style,
                foreground_rgb,
                background_rgb,
            });
        }
        decoded_text.push_str(text.trim_end_matches(' '));
        decoded_text.push('\n');
    }
    while decoded_text.ends_with('\n') {
        decoded_text.pop();
    }
    if !decoded_text.is_empty() {
        decoded_text.push('\n');
    }
    let diagnostics: Vec<_> = terminal
        .errors
        .iter()
        .map(|error| Diagnostic {
            kind: "terminal_replay",
            message: error.clone(),
            guest_position: None,
            terminal_position: None,
        })
        .collect();
    TextReport {
        schema_version: 1,
        kind: "terminal_text",
        success: diagnostics.is_empty(),
        terminal_columns: COLS,
        terminal_rows: terminal::ROWS,
        terminal_snapshot: snapshot,
        decoded_text,
        terminal_cells,
        diagnostics,
    }
}

#[cfg(test)]
mod tests;
