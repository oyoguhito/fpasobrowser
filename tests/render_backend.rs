use anyhow::Result;
use fpasobrowser::render::{
    kitty::KittyBackend, sixel::SixelBackend, BackendChoice, RenderBackend, RenderSize,
    SelectedBackend, TerminalCellPixels,
};
use image::{DynamicImage, RgbaImage};

#[test]
fn auto_selects_half_block_backend() {
    let backend = SelectedBackend::from_choice(BackendChoice::Auto);
    assert_eq!(backend.name(), "half-block");
}

#[test]
fn explicit_half_block_selects_half_block_backend() {
    let backend = SelectedBackend::from_choice(BackendChoice::HalfBlock);
    assert_eq!(backend.name(), "half-block");
    assert!(!backend.frame_transport_is_expensive());
}

#[test]
fn sixel_marks_full_frame_transport_as_expensive() {
    let backend = SelectedBackend::from_choice(BackendChoice::Sixel);
    assert!(backend.frame_transport_is_expensive());
    assert_eq!(
        backend.terminal_cell_pixels(),
        Some(fpasobrowser::render::TerminalCellPixels {
            width: 8,
            height: 17,
        })
    );
}

#[test]
fn sixel_override_changes_raster_and_viewport_geometry_together() -> Result<()> {
    let cell_pixels = TerminalCellPixels {
        width: 9,
        height: 18,
    };
    let backend = SelectedBackend::from_choice_with_sixel_cell_pixels(
        BackendChoice::Sixel,
        Some(cell_pixels),
    );
    assert_eq!(backend.terminal_cell_pixels(), Some(cell_pixels));

    let image = DynamicImage::ImageRgba8(RgbaImage::new(1, 2));
    let mut output = Vec::new();
    backend.render(
        &image,
        RenderSize {
            columns: 2,
            rows: 3,
        },
        &mut output,
    )?;
    assert!(String::from_utf8(output)?.starts_with("\x1bPq\"1;1;18;54"));
    Ok(())
}

#[test]
fn sixel_backend_emits_sixel_graphics_sequences() -> Result<()> {
    let image = DynamicImage::ImageRgba8(RgbaImage::new(1, 2));
    let mut output = Vec::new();
    SixelBackend::new().render(
        &image,
        RenderSize {
            columns: 1,
            rows: 1,
        },
        &mut output,
    )?;
    let output = String::from_utf8(output)?;
    assert!(output.starts_with("\x1bPq\"1;1;8;17"));
    assert!(output.ends_with("\x1b\\"));
    Ok(())
}

#[test]
fn kitty_backend_emits_kitty_graphics_sequences() -> Result<()> {
    let image = DynamicImage::ImageRgba8(RgbaImage::new(1, 2));
    let mut output = Vec::new();
    KittyBackend::new().render(
        &image,
        RenderSize {
            columns: 10,
            rows: 5,
        },
        &mut output,
    )?;

    let output = String::from_utf8(output)?;
    assert!(output.starts_with("\x1b[H\x1b_Ga=T,f=100,t=d,i="));
    assert!(output.contains(",c=10,r=5,C=1,q=2,"));
    assert!(!output.contains("\x1b_Ga=d,d=I,i="));
    assert!(output.contains(";"));
    assert!(output.ends_with("\x1b\\"));
    Ok(())
}

#[test]
fn kitty_backend_keeps_one_complete_frame_while_replacing() -> Result<()> {
    let image = DynamicImage::ImageRgba8(RgbaImage::new(1, 2));
    let backend = KittyBackend::new();
    let size = RenderSize {
        columns: 10,
        rows: 5,
    };
    let mut first = Vec::new();
    backend.render(&image, size, &mut first)?;

    let mut second = Vec::new();
    backend.render(&image, size, &mut second)?;
    let second = String::from_utf8(second)?;

    assert!(second.starts_with("\x1b[H\x1b_Ga=T,f=100,t=d,i="));
    assert!(second.contains(",p="));
    assert!(
        !second.contains("\x1b_Ga=d,d=I,i="),
        "the immediately preceding image remains visible while this frame decodes"
    );

    let mut third = Vec::new();
    backend.render(&image, size, &mut third)?;
    let third = String::from_utf8(third)?;
    let delete = third
        .find("\x1b_Ga=d,d=I,i=")
        .expect("the two-frames-old image is reclaimed after a fresh transmit");
    let transmit = third
        .find("\x1b_Ga=T,f=100,t=d,i=")
        .expect("replacement transmits a fresh image first");
    assert!(transmit < delete);
    assert_eq!(third.find("\x1b[H"), Some(0));
    Ok(())
}

#[test]
fn kitty_backend_retires_a_differently_sized_frame_before_replacement() -> Result<()> {
    let image = DynamicImage::ImageRgba8(RgbaImage::new(1, 2));
    let backend = KittyBackend::new();
    backend.render(
        &image,
        RenderSize {
            columns: 80,
            rows: 22,
        },
        &mut Vec::new(),
    )?;

    let mut resized = Vec::new();
    backend.render(
        &image,
        RenderSize {
            columns: 120,
            rows: 40,
        },
        &mut resized,
    )?;
    let resized = String::from_utf8(resized)?;
    let delete = resized
        .find("\x1b_Ga=d,d=I,i=")
        .expect("the stale geometry is deleted");
    let transmit = resized
        .find("\x1b_Ga=T,f=100,t=d,i=")
        .expect("a resized replacement is transmitted");
    assert!(
        delete < transmit,
        "fpasoterm can defer the old placement until this replacement paints"
    );
    let home = resized
        .find("\x1b[H")
        .expect("a resized replacement explicitly resets the Kitty anchor");
    assert!(delete < home && home < transmit);
    assert!(resized.contains(",c=120,r=40,C=1,q=2,"));
    Ok(())
}

#[test]
fn kitty_backend_cleanup_deletes_its_scoped_image() -> Result<()> {
    let image = DynamicImage::ImageRgba8(RgbaImage::new(1, 2));
    let backend = KittyBackend::new();
    backend.render(
        &image,
        RenderSize {
            columns: 1,
            rows: 1,
        },
        &mut Vec::new(),
    )?;
    backend.render(
        &image,
        RenderSize {
            columns: 1,
            rows: 1,
        },
        &mut Vec::new(),
    )?;
    let mut output = Vec::new();
    backend.cleanup(&mut output)?;

    let output = String::from_utf8(output)?;
    assert_eq!(output.matches("\x1b_Ga=d,d=I,i=").count(), 2);
    assert!(output.ends_with(",q=2\x1b\\"));
    Ok(())
}
