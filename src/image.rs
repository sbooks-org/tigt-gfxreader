// SPDX-License-Identifier: MIT-0
// Copyright (C) 2026 Simplebooks Foundation
// Copyright (C) 2026 Josh Rodd

use crate::{finish_pixels, Analysis, Diagnostic, Options, Report};
use std::collections::BTreeSet;
use std::io::Read;

/// Native, unscaled image samples. Only fully opaque pixels participate in OCR.
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

fn dimensions(width: usize, height: usize) -> Result<(), String> {
    if !matches!(width, 160 | 320 | 640) || height != 200 {
        return Err(
            "image dimensions must be 160x200, 320x200, or 640x200; no scaling is performed".into(),
        );
    }
    Ok(())
}

/// Decode a single PNG, expanding palette/low-bit grayscale without color conversion.
/// 16-bit samples and animations are rejected instead of silently discarding data.
pub fn decode_png(input: impl Read) -> Result<Image, String> {
    let mut decoder = png::Decoder::new(input);
    decoder.set_limits(png::Limits {
        bytes: 16 * 1024 * 1024,
    });
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder
        .read_info()
        .map_err(|e| format!("decoding PNG header: {e}"))?;
    let info = reader.info();
    let width = info.width as usize;
    let height = info.height as usize;
    dimensions(width, height)?;
    if info.bit_depth == png::BitDepth::Sixteen {
        return Err("16-bit PNG samples are unsupported; provide lossless 8-bit samples".into());
    }
    if info.animation_control.is_some() {
        return Err("animated PNG is unsupported; select a frame explicitly".into());
    }
    let mut samples = vec![0; reader.output_buffer_size()];
    let frame = reader
        .next_frame(&mut samples)
        .map_err(|e| format!("decoding PNG pixels: {e}"))?;
    reader.finish().map_err(|e| format!("finishing PNG: {e}"))?;
    samples.truncate(frame.buffer_size());
    if frame.color_type == png::ColorType::Rgba {
        return Ok(Image {
            width,
            height,
            rgba: samples,
        });
    }
    let mut rgba = Vec::with_capacity(width * height * 4);
    let samples = &samples[..frame.buffer_size()];
    match frame.color_type {
        png::ColorType::Rgb => {
            for pixel in samples.as_chunks::<3>().0 {
                rgba.extend_from_slice(pixel);
                rgba.push(255);
            }
        }
        png::ColorType::Rgba => unreachable!("RGBA samples returned without copying"),
        png::ColorType::Grayscale => {
            for &sample in samples {
                rgba.extend_from_slice(&[sample, sample, sample, 255]);
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for pixel in samples.as_chunks::<2>().0 {
                rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]]);
            }
        }
        png::ColorType::Indexed => return Err("PNG palette was not expanded".into()),
    }
    Ok(Image {
        width,
        height,
        rgba,
    })
}

/// Recognize an exact native RGBA image using the same classifier as terminal replay.
pub fn analyze_image(image: &Image, font: &[u8], options: &Options) -> Result<Analysis, String> {
    analyze_samples(&image.rgba, image.width, image.height, 4, font, options)
}

fn analyze_samples(
    samples: &[u8],
    width: usize,
    height: usize,
    channels: usize,
    font: &[u8],
    options: &Options,
) -> Result<Analysis, String> {
    let region = options.validate(font.len())?;
    dimensions(width, height)?;
    if width != options.width || height != options.height {
        return Err(format!(
            "image is {width}x{height}, but requested dimensions are {}x{}",
            options.width, options.height
        ));
    }
    if samples.len() != width * height * channels {
        return Err("sample length does not match image dimensions".into());
    }
    let mut pixels = Vec::with_capacity(region.width * region.height);
    let mut incomplete = BTreeSet::new();
    let mut nonopaque = 0;
    for y in region.y..region.y + region.height {
        for x in region.x..region.x + region.width {
            let pixel = &samples[(y * width + x) * channels..][..channels];
            if channels == 3 || pixel[3] == 255 {
                pixels.push(Some([pixel[0], pixel[1], pixel[2]]));
            } else {
                pixels.push(None);
                incomplete.insert(y);
                nonopaque += 1;
            }
        }
    }
    let diagnostics = if nonopaque == 0 {
        Vec::new()
    } else {
        vec![Diagnostic {
        kind: "nonopaque_pixels",
        message: format!("{nonopaque} nonopaque pixels cannot define an exact RGB glyph; original PNG retains the submitted alpha"),
        guest_position: None,
        terminal_position: None,
    }]
    };
    Ok(finish_pixels(
        pixels,
        Some(font),
        options,
        Report {
            schema_version: 1,
            kind: "image",
            recognition_performed: true,
            recognition_complete: false,
            ambiguous_glyph_count: 0,
            success: false,
            guest_width: width,
            guest_height: height,
            region,
            terminal_columns: 0,
            terminal_rows: 0,
            terminal_snapshot: "native_image",
            font_offset: options.font_offset,
            glyph_count: options.glyph_count,
            expected: options.expect.clone(),
            expected_found: None,
            missing_pixel_count: 0,
            incomplete_scanlines: incomplete.into_iter().collect(),
            diagnostics,
            glyphs: Vec::new(),
            decoded_text: String::new(),
            terminal_cells: Vec::new(),
        },
    ))
}

/// Recognize tightly packed row-major RGB samples of the dimensions in `options`.
pub fn analyze_rgb(rgb: &[u8], font: &[u8], options: &Options) -> Result<Analysis, String> {
    analyze_samples(rgb, options.width, options.height, 3, font, options)
}
