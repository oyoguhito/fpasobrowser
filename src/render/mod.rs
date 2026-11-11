pub mod half_block;
pub mod kitty;
pub mod sixel;

use anyhow::Result;
use clap::ValueEnum;
use image::DynamicImage;
use std::io::Write;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderSize {
    pub columns: u16,
    pub rows: u16,
}

/// Physical pixel geometry used by an image renderer for one terminal cell.
///
/// Most renderers can work with the generic screenshot size. Image protocols
/// such as SIXEL, however, need an explicit raster size. Supplying that same
/// geometry to the browser keeps the screenshot and the terminal placement at
/// one-to-one scale, which is also essential for pointer coordinate mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalCellPixels {
    pub width: u32,
    pub height: u32,
}

impl RenderSize {
    #[must_use]
    pub fn image_width(self) -> u32 {
        u32::from(self.columns)
    }

    #[must_use]
    pub fn image_height(self) -> u32 {
        u32::from(self.rows) * 2
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BackendChoice {
    Auto,
    HalfBlock,
    Sixel,
    Kitty,
}

pub trait RenderBackend {
    fn name(&self) -> &'static str;

    /// A full SIXEL frame is substantially larger and more disruptive to the
    /// terminal than a Kitty placement replacement or ANSI cell update. The
    /// app uses this hint to debounce cosmetic redraws while still forwarding
    /// pointer and keyboard input immediately.
    fn frame_transport_is_expensive(&self) -> bool {
        false
    }

    /// Return the physical pixel size that this backend assigns to one
    /// terminal cell, when it has one. The application uses it to size the
    /// dynamic browser viewport before a screenshot is captured.
    fn terminal_cell_pixels(&self) -> Option<TerminalCellPixels> {
        None
    }

    /// Whether changing a frame must first erase terminal cells.
    ///
    /// ANSI and SIXEL renderers paint terminal cells directly, so clearing is
    /// required to remove pixels left by the preceding frame. Kitty replaces
    /// its process-scoped placement itself; a terminal-wide clear before every
    /// replacement creates a visible blank interval in nested terminal
    /// renderers such as fpasoterm/Herdr.
    fn clears_terminal_before_frame(&self) -> bool {
        true
    }

    fn render(&self, image: &DynamicImage, size: RenderSize, writer: &mut dyn Write) -> Result<()>;

    fn cleanup(&self, _writer: &mut dyn Write) -> Result<()> {
        Ok(())
    }
}

pub enum SelectedBackend {
    HalfBlock(half_block::HalfBlockBackend),
    Sixel(sixel::SixelBackend),
    Kitty(kitty::KittyBackend),
}

impl SelectedBackend {
    #[must_use]
    pub fn from_choice(choice: BackendChoice) -> Self {
        Self::from_choice_with_sixel_cell_pixels(choice, None)
    }

    /// Select a renderer, optionally overriding SIXEL's physical cell size.
    /// The setting is intentionally ignored for image protocols that negotiate
    /// their own placement geometry.
    #[must_use]
    pub fn from_choice_with_sixel_cell_pixels(
        choice: BackendChoice,
        sixel_cell_pixels: Option<TerminalCellPixels>,
    ) -> Self {
        match choice {
            BackendChoice::Auto | BackendChoice::HalfBlock => {
                Self::HalfBlock(half_block::HalfBlockBackend::new())
            }
            BackendChoice::Sixel => Self::Sixel(match sixel_cell_pixels {
                Some(cell_pixels) => sixel::SixelBackend::with_cell_pixels(cell_pixels),
                None => sixel::SixelBackend::new(),
            }),
            BackendChoice::Kitty => Self::Kitty(kitty::KittyBackend::new()),
        }
    }
}

impl RenderBackend for SelectedBackend {
    fn name(&self) -> &'static str {
        match self {
            Self::HalfBlock(backend) => backend.name(),
            Self::Sixel(backend) => backend.name(),
            Self::Kitty(backend) => backend.name(),
        }
    }

    fn frame_transport_is_expensive(&self) -> bool {
        match self {
            Self::HalfBlock(backend) => backend.frame_transport_is_expensive(),
            Self::Sixel(backend) => backend.frame_transport_is_expensive(),
            Self::Kitty(backend) => backend.frame_transport_is_expensive(),
        }
    }

    fn terminal_cell_pixels(&self) -> Option<TerminalCellPixels> {
        match self {
            Self::HalfBlock(backend) => backend.terminal_cell_pixels(),
            Self::Sixel(backend) => backend.terminal_cell_pixels(),
            Self::Kitty(backend) => backend.terminal_cell_pixels(),
        }
    }

    fn clears_terminal_before_frame(&self) -> bool {
        match self {
            Self::HalfBlock(backend) => backend.clears_terminal_before_frame(),
            Self::Sixel(backend) => backend.clears_terminal_before_frame(),
            Self::Kitty(backend) => backend.clears_terminal_before_frame(),
        }
    }

    fn render(&self, image: &DynamicImage, size: RenderSize, writer: &mut dyn Write) -> Result<()> {
        match self {
            Self::HalfBlock(backend) => backend.render(image, size, writer),
            Self::Sixel(backend) => backend.render(image, size, writer),
            Self::Kitty(backend) => backend.render(image, size, writer),
        }
    }

    fn cleanup(&self, writer: &mut dyn Write) -> Result<()> {
        match self {
            Self::HalfBlock(backend) => backend.cleanup(writer),
            Self::Sixel(backend) => backend.cleanup(writer),
            Self::Kitty(backend) => backend.cleanup(writer),
        }
    }
}
