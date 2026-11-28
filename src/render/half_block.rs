use super::{RenderBackend, RenderSize};
use anyhow::Result;
use image::{imageops::FilterType, DynamicImage, GenericImageView, Rgba};
use std::io::Write;

#[derive(Debug, Default, Clone, Copy)]
pub struct HalfBlockBackend;

impl HalfBlockBackend {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl RenderBackend for HalfBlockBackend {
    fn name(&self) -> &'static str {
        "half-block"
    }

    fn render(&self, image: &DynamicImage, size: RenderSize, writer: &mut dyn Write) -> Result<()> {
        let target_width = size.image_width().max(1);
        let target_height = size.image_height().max(2);
        let resized = image.resize_exact(target_width, target_height, FilterType::Triangle);

        for y in (0..target_height).step_by(2) {
            for x in 0..target_width {
                let upper = resized.get_pixel(x, y);
                let lower = resized.get_pixel(x, y + 1);
                write_half_block_cell(writer, upper, lower)?;
            }
            writer.write_all(b"\x1b[0m\r\n")?;
        }
        writer.write_all(b"\x1b[0m")?;
        Ok(())
    }
}

fn write_half_block_cell(writer: &mut dyn Write, upper: Rgba<u8>, lower: Rgba<u8>) -> Result<()> {
    write!(
        writer,
        "\x1b[38;2;{};{};{}m\x1b[48;2;{};{};{}m▀",
        upper[0], upper[1], upper[2], lower[0], lower[1], lower[2]
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, RgbaImage};

    #[test]
    fn renders_two_vertical_pixels_as_one_terminal_cell() {
        let mut image = RgbaImage::new(1, 2);
        image.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
        image.put_pixel(0, 1, Rgba([0, 0, 255, 255]));

        let mut output = Vec::new();
        HalfBlockBackend::new()
            .render(
                &DynamicImage::ImageRgba8(image),
                RenderSize {
                    columns: 1,
                    rows: 1,
                },
                &mut output,
            )
            .expect("render");

        let rendered = String::from_utf8(output).expect("utf8");
        assert!(rendered.contains("\x1b[38;2;255;0;0m"));
        assert!(rendered.contains("\x1b[48;2;0;0;255m"));
        assert!(rendered.contains('▀'));
    }
}
