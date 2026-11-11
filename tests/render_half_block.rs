use anyhow::Result;
use fpasobrowser::render::{half_block::HalfBlockBackend, RenderBackend, RenderSize};
use image::{DynamicImage, Rgba, RgbaImage};

#[test]
fn half_block_renderer_maps_two_image_rows_to_one_terminal_row() -> Result<()> {
    let mut image = RgbaImage::new(2, 2);
    image.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
    image.put_pixel(0, 1, Rgba([0, 0, 255, 255]));
    image.put_pixel(1, 0, Rgba([0, 255, 0, 255]));
    image.put_pixel(1, 1, Rgba([255, 255, 255, 255]));

    let mut output = Vec::new();
    HalfBlockBackend::new().render(
        &DynamicImage::ImageRgba8(image),
        RenderSize {
            columns: 2,
            rows: 1,
        },
        &mut output,
    )?;

    let rendered = String::from_utf8(output)?;
    assert!(rendered.contains("\x1b[38;2;255;0;0m"));
    assert!(rendered.contains("\x1b[48;2;0;0;255m"));
    assert!(rendered.contains("\x1b[38;2;0;255;0m"));
    assert!(rendered.contains("\x1b[48;2;255;255;255m"));
    assert_eq!(rendered.matches('▀').count(), 2);
    Ok(())
}
