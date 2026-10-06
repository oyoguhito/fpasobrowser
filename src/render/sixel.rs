use super::{RenderBackend, RenderSize, TerminalCellPixels};
use anyhow::Result;
use image::{imageops::FilterType, DynamicImage, Rgba};
use std::io::Write;

// SIXEL has no portable way to query a terminal cell's pixel geometry.
// fpasoterm's Linux / ChromeOS default (14 px font with 1.08 line height)
// measures close to 8×17 CSS px per terminal cell. The raster must use the
// same scale as mouse-report cells: a smaller raster leaves a visual gap below
// the page and maps a visible input/button click to the wrong Servo viewport
// coordinate. A future device-pixel query can replace this default estimate.
pub const DEFAULT_CELL_PIXELS: TerminalCellPixels = TerminalCellPixels {
    width: 8,
    height: 17,
};
const PALETTE_LEVELS: u8 = 6;
const PALETTE_SIZE: usize = (PALETTE_LEVELS as usize).pow(3);

#[derive(Debug, Clone, Copy)]
pub struct SixelBackend {
    cell_pixels: TerminalCellPixels,
}

impl Default for SixelBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl SixelBackend {
    #[must_use]
    pub fn new() -> Self {
        Self::with_cell_pixels(DEFAULT_CELL_PIXELS)
    }

    /// Build a SIXEL renderer with the physical terminal cell geometry that
    /// will also be supplied to Servo for viewport and pointer mapping.
    #[must_use]
    pub fn with_cell_pixels(cell_pixels: TerminalCellPixels) -> Self {
        Self { cell_pixels }
    }
}

impl RenderBackend for SixelBackend {
    fn name(&self) -> &'static str {
        "sixel"
    }

    fn frame_transport_is_expensive(&self) -> bool {
        true
    }

    fn terminal_cell_pixels(&self) -> Option<TerminalCellPixels> {
        Some(self.cell_pixels)
    }

    fn render(&self, image: &DynamicImage, size: RenderSize, writer: &mut dyn Write) -> Result<()> {
        let width = u32::from(size.columns.max(1));
        let height = u32::from(size.rows.max(1));
        let target_width = width * self.cell_pixels.width;
        let target_height = height * self.cell_pixels.height;
        let pixels = image
            .resize_exact(target_width, target_height, FilterType::Triangle)
            .to_rgba8();
        let indices = quantize_pixels(&pixels);
        let palette = used_palette(&indices);

        // DCS q starts a SIXEL data stream. Raster attributes declare one
        // square pixel and the intended physical dimensions before palette
        // definitions and bitmap data follow.
        write!(writer, "\x1bPq\"1;1;{target_width};{target_height}")?;
        for color in palette
            .iter()
            .enumerate()
            .filter_map(|(index, used)| used.then_some(index))
        {
            let (red, green, blue) = palette_color(color as u8);
            write!(writer, "#{color};2;{red};{green};{blue}")?;
        }

        for start_y in (0..target_height).step_by(6) {
            let band_colors = colors_in_band(&indices, target_width, target_height, start_y);
            for (position, color) in band_colors.iter().enumerate() {
                write!(writer, "#{color}")?;
                write_sixel_plane(
                    writer,
                    &indices,
                    target_width,
                    target_height,
                    start_y,
                    *color,
                )?;
                if position + 1 < band_colors.len() {
                    writer.write_all(b"$")?;
                }
            }
            if start_y + 6 < target_height {
                writer.write_all(b"-")?;
            }
        }

        // String Terminator ends the DCS sequence. The terminal's normal
        // screen clear before each render removes the preceding SIXEL frame.
        writer.write_all(b"\x1b\\")?;
        Ok(())
    }
}

fn quantize_pixels(image: &image::RgbaImage) -> Vec<u8> {
    image
        .pixels()
        .map(|pixel| palette_index(composite_pixel(*pixel)))
        .collect()
}

fn composite_pixel(pixel: Rgba<u8>) -> [u8; 3] {
    // Servo screenshots are opaque in normal operation. Composite the rare
    // transparent pixel over black so every SIXEL cell has a deterministic
    // palette entry instead of depending on terminal background state.
    let alpha = u16::from(pixel[3]);
    [
        ((u16::from(pixel[0]) * alpha) / 255) as u8,
        ((u16::from(pixel[1]) * alpha) / 255) as u8,
        ((u16::from(pixel[2]) * alpha) / 255) as u8,
    ]
}

fn palette_index([red, green, blue]: [u8; 3]) -> u8 {
    let red = quantize_component(red);
    let green = quantize_component(green);
    let blue = quantize_component(blue);
    red * PALETTE_LEVELS * PALETTE_LEVELS + green * PALETTE_LEVELS + blue
}

fn quantize_component(value: u8) -> u8 {
    ((u16::from(value) * u16::from(PALETTE_LEVELS - 1) + 127) / 255) as u8
}

fn palette_color(index: u8) -> (u8, u8, u8) {
    let red = index / (PALETTE_LEVELS * PALETTE_LEVELS);
    let green = (index / PALETTE_LEVELS) % PALETTE_LEVELS;
    let blue = index % PALETTE_LEVELS;
    (
        palette_percent(red),
        palette_percent(green),
        palette_percent(blue),
    )
}

fn palette_percent(level: u8) -> u8 {
    (u16::from(level) * 100 / u16::from(PALETTE_LEVELS - 1)) as u8
}

fn used_palette(indices: &[u8]) -> [bool; PALETTE_SIZE] {
    let mut used = [false; PALETTE_SIZE];
    for color in indices {
        used[usize::from(*color)] = true;
    }
    used
}

fn colors_in_band(indices: &[u8], width: u32, height: u32, start_y: u32) -> Vec<u8> {
    let mut used = [false; PALETTE_SIZE];
    for y in start_y..(start_y + 6).min(height) {
        let row_start = (y * width) as usize;
        for color in &indices[row_start..row_start + width as usize] {
            used[usize::from(*color)] = true;
        }
    }
    used.iter()
        .enumerate()
        .filter_map(|(index, used)| used.then_some(index as u8))
        .collect()
}

fn write_sixel_plane(
    writer: &mut dyn Write,
    indices: &[u8],
    width: u32,
    height: u32,
    start_y: u32,
    color: u8,
) -> Result<()> {
    let mut run = None;
    let mut run_length = 0_usize;
    for x in 0..width {
        let mut sixel = 0_u8;
        for bit in 0..6 {
            let y = start_y + bit;
            if y < height && indices[(y * width + x) as usize] == color {
                sixel |= 1 << bit;
            }
        }
        let character = sixel + b'?';
        if run == Some(character) {
            run_length += 1;
        } else {
            if let Some(previous) = run {
                write_sixel_run(writer, previous, run_length)?;
            }
            run = Some(character);
            run_length = 1;
        }
    }
    if let Some(character) = run {
        write_sixel_run(writer, character, run_length)?;
    }
    Ok(())
}

fn write_sixel_run(writer: &mut dyn Write, character: u8, length: usize) -> Result<()> {
    if length >= 3 {
        write!(writer, "!{length}{}", char::from(character))?;
    } else {
        for _ in 0..length {
            writer.write_all(&[character])?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::RgbaImage;

    #[test]
    fn renders_quantized_sixel_data_and_terminates_dcs() {
        let image = DynamicImage::ImageRgba8(RgbaImage::from_pixel(10, 20, Rgba([255, 0, 0, 255])));
        let mut output = Vec::new();
        SixelBackend::new()
            .render(
                &image,
                RenderSize {
                    columns: 1,
                    rows: 1,
                },
                &mut output,
            )
            .expect("render SIXEL");

        let rendered = String::from_utf8(output).expect("SIXEL is ASCII");
        assert!(rendered.starts_with("\x1bPq\"1;1;8;17"));
        assert!(rendered.contains("#180;2;100;0;0"));
        assert!(rendered.contains("!8~"));
        assert!(rendered.ends_with("\x1b\\"));
    }

    #[test]
    fn palette_uses_a_six_by_six_by_six_color_cube() {
        assert_eq!(palette_index([0, 0, 0]), 0);
        assert_eq!(palette_index([255, 0, 0]), 180);
        assert_eq!(palette_index([0, 255, 0]), 30);
        assert_eq!(palette_index([0, 0, 255]), 5);
        assert_eq!(palette_color(215), (100, 100, 100));
    }
}
