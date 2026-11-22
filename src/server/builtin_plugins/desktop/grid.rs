use anyhow::{Result, ensure};
use image::{ImageFormat, ImageReader, Rgba, RgbaImage};
use std::io::Cursor;

pub(super) const STEP: u32 = 100;
const DIGITS: [[u8; 5]; 10] = [
    [7, 5, 5, 5, 7],
    [2, 6, 2, 2, 7],
    [7, 1, 7, 4, 7],
    [7, 1, 7, 1, 7],
    [5, 5, 7, 1, 1],
    [7, 4, 7, 1, 7],
    [7, 4, 7, 5, 7],
    [7, 1, 1, 1, 1],
    [7, 5, 7, 5, 7],
    [7, 5, 7, 1, 7],
];

pub(super) fn overlay(bytes: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        bytes.len() <= crate::media::visual_artifacts::MAX_ORIGINAL_BYTES,
        "desktop capture exceeds image budget"
    );
    let mut reader = ImageReader::with_format(Cursor::new(bytes), ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(96 * 1024 * 1024);
    reader.limits(limits);
    let mut image = reader.decode()?.into_rgba8();
    let (width, height) = image.dimensions();
    for x in (0..width).step_by(STEP as usize) {
        for y in 0..height {
            blend(&mut image, x, y, [0, 224, 255], 96);
        }
    }
    for y in (0..height).step_by(STEP as usize) {
        for x in 0..width {
            blend(&mut image, x, y, [0, 224, 255], 96);
        }
    }
    for x in (0..width).step_by(STEP as usize) {
        label(&mut image, x.saturating_add(2), 2, x);
    }
    for y in (STEP..height).step_by(STEP as usize) {
        label(&mut image, 2, y.saturating_add(2), y);
    }
    let mut output = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image).write_to(&mut output, ImageFormat::Png)?;
    Ok(output.into_inner())
}
fn blend(image: &mut RgbaImage, x: u32, y: u32, rgb: [u8; 3], alpha: u16) {
    if let Some(pixel) = image.get_pixel_mut_checked(x, y) {
        *pixel = Rgba(std::array::from_fn(|i| {
            if i == 3 {
                pixel.0[i]
            } else {
                ((u16::from(pixel.0[i]) * (255 - alpha) + u16::from(rgb[i]) * alpha) / 255) as u8
            }
        }));
    }
}
fn label(image: &mut RgbaImage, x: u32, y: u32, number: u32) {
    let digits = number.to_string();
    for dy in 0..12 {
        for dx in 0..(digits.len() as u32 * 8 + 2) {
            blend(image, x + dx, y + dy, [0, 0, 0], 180);
        }
    }
    for (i, digit) in digits.bytes().enumerate() {
        for (row, mask) in DIGITS[(digit - b'0') as usize].iter().enumerate() {
            for col in 0..3 {
                if mask & (1 << (2 - col)) != 0 {
                    for dy in 0..2 {
                        for dx in 0..2 {
                            blend(
                                image,
                                x + 1 + i as u32 * 8 + col * 2 + dx,
                                y + 1 + row as u32 * 2 + dy,
                                [255, 255, 255],
                                235,
                            );
                        }
                    }
                }
            }
        }
    }
}
