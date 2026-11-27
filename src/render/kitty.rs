use super::{RenderBackend, RenderSize};
use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use image::{DynamicImage, ImageFormat, RgbaImage};
use std::{
    io::{Cursor, Write},
    process,
    sync::{
        atomic::{AtomicU32, Ordering},
        Mutex,
    },
};

const CHUNK_SIZE: usize = 4096;
// Older and some remote Windows ConPTY paths corrupt OSC sequences longer
// than 256 bytes.  The bridge header consumes roughly 90 bytes on a wide
// pane, so retain ample room for the ST terminator and keep its base64 slice
// deliberately small.  Native Kitty APC keeps its efficient 4 KiB chunks.
const FPASOTERM_OSC_CHUNK_SIZE: usize = 96;
const KITTY_ID_NAMESPACE: u32 = 0x4600_0000;

/// The Windows ConPTY path consumes APC sequences (`ESC _ ... ESC \\`) before
/// they reach the terminal host.  fpasoterm's Windows renderer therefore
/// accepts a bounded Kitty command/payload pair inside its private OSC 777
/// channel. Other terminals continue to receive the standard protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KittyTransport {
    StandardApc,
    FpasotermOsc,
}

#[derive(Debug)]
pub struct KittyBackend {
    transport: KittyTransport,
    /// A normal terminal can release the frame from two updates ago as soon as
    /// the next transfer is submitted. fpasoterm's WebView2 decoder is
    /// asynchronous, however, and can still be decoding that frame when its
    /// delete reaches the parser. In that mode the receiver's bounded image
    /// store owns retirement instead.
    retire_previous_frames: bool,
    /// The image for the newest frame.
    active_image_id: AtomicU32,
    /// Track the immediately preceding frame for normal terminals, where it
    /// remains visible while the newest transfer is decoded. The fpasoterm
    /// policy leaves retirement to its bounded receiver instead.
    retained_image_id: AtomicU32,
    /// Geometry controls the Kitty placement footprint.  A retained frame at
    /// a different geometry cannot safely be kept as a visual fallback: its
    /// uncovered rows/columns visibly stack below the new page.
    last_render_size: Mutex<Option<RenderSize>>,
    next_image_id: AtomicU32,
    next_placement_id: AtomicU32,
}

impl KittyBackend {
    #[must_use]
    pub fn new() -> Self {
        Self::with_frame_retirement(true, KittyTransport::StandardApc)
    }

    /// Use the standard PNG protocol for fpasoterm, while allowing its bounded
    /// receiver to retire delayed frames after they have had a chance to paint.
    #[must_use]
    pub fn for_fpasoterm() -> Self {
        Self::with_frame_retirement(false, KittyTransport::StandardApc)
    }

    /// Use fpasoterm's OSC bridge on Windows. ConPTY discards the APC
    /// introducer used by the Kitty Graphics Protocol, whereas OSC 777 reaches
    /// the renderer unchanged. The bridge keeps PNG (`f=100`) inside that OSC
    /// envelope so fpasoterm presents it through a DOM `<img>` rather than a
    /// WebView2 Canvas. In an RDP session, the Canvas raw-RGBA path can be
    /// black despite correct bytes, while the connected image element is
    /// composited correctly.
    #[must_use]
    pub fn for_windows_fpasoterm() -> Self {
        Self::with_frame_retirement(false, KittyTransport::FpasotermOsc)
    }

    fn with_frame_retirement(retire_previous_frames: bool, transport: KittyTransport) -> Self {
        let scoped_id = KITTY_ID_NAMESPACE | (process::id() & 0x00ff_ffff);
        Self {
            transport,
            retire_previous_frames,
            active_image_id: AtomicU32::new(0),
            retained_image_id: AtomicU32::new(0),
            last_render_size: Mutex::new(None),
            next_image_id: AtomicU32::new(scoped_id),
            next_placement_id: AtomicU32::new(scoped_id),
        }
    }

    /// Describe the active wire representation for human-readable diagnostics.
    #[must_use]
    pub const fn transport_label(&self) -> &'static str {
        match self.transport {
            KittyTransport::StandardApc => "kitty-png",
            KittyTransport::FpasotermOsc => "fpasoterm-osc-png",
        }
    }

    fn write_delete(&self, writer: &mut dyn Write, image_id: u32) -> Result<()> {
        match self.transport {
            KittyTransport::StandardApc => {
                write!(writer, "\x1b_Ga=d,d=I,i={image_id},q=2\x1b\\")?;
            }
            KittyTransport::FpasotermOsc => {
                write!(
                    writer,
                    "\x1b]777;graphics;v=1;a=d,d=I,i={image_id},q=2;\x1b\\"
                )?;
            }
        }
        Ok(())
    }

    const fn chunk_size(&self) -> usize {
        match self.transport {
            KittyTransport::StandardApc => CHUNK_SIZE,
            KittyTransport::FpasotermOsc => FPASOTERM_OSC_CHUNK_SIZE,
        }
    }
}

impl Default for KittyBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderBackend for KittyBackend {
    fn name(&self) -> &'static str {
        "kitty"
    }

    fn clears_terminal_before_frame(&self) -> bool {
        false
    }

    fn render(&self, image: &DynamicImage, size: RenderSize, writer: &mut dyn Write) -> Result<()> {
        let encoded = match self.transport {
            KittyTransport::StandardApc => STANDARD.encode(encode_png(image)?),
            KittyTransport::FpasotermOsc => STANDARD.encode(encode_png(image)?),
        };
        let columns = size.columns.max(1);
        let rows = size.rows.max(1);
        let previous_size = {
            let mut last_size = self
                .last_render_size
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let previous_size = *last_size;
            *last_size = Some(size);
            previous_size
        };
        let geometry_changed = previous_size.is_some_and(|previous| previous != size);
        // Re-transmitting bytes for an existing Kitty image id deletes every
        // placement for that id before the replacement can be decoded. That
        // creates a black/blank interval in terminals without fpasoterm's
        // deferred-delete implementation. Instead, transmit a fresh image id
        // into a fresh placement and retain the preceding frame. On the next
        // successful replacement we may release the frame before that one.
        // Thus at least one fully submitted old frame survives while each new
        // frame crosses the nested fpasoterm/Herdr PTY and decode queues.
        let image_id = self.next_image_id.fetch_add(1, Ordering::Relaxed);
        let previous_image_id = self.active_image_id.swap(image_id, Ordering::Relaxed);
        let retained_image_id = if geometry_changed {
            self.retained_image_id.swap(0, Ordering::Relaxed)
        } else {
            self.retained_image_id
                .swap(previous_image_id, Ordering::Relaxed)
        };
        let placement_id = self.next_placement_id.fetch_add(1, Ordering::Relaxed);

        if self.retire_previous_frames && geometry_changed {
            // fpasoterm defers this delete until the following replacement
            // has painted.  Sending it before the transfer lets that renderer
            // pair the two operations, while a different-sized stale frame is
            // never retained long enough to overlap the new bottom/right edge.
            for stale_image_id in [previous_image_id, retained_image_id] {
                if stale_image_id != 0 && stale_image_id != image_id {
                    self.write_delete(writer, stale_image_id)?;
                }
            }
        }

        // Kitty placements are anchored at the terminal cursor. `redraw`
        // also queues MoveTo(0, 0), but an activity/status write can be
        // buffered by an outer terminal or multiplexer until the graphics
        // payload is parsed. Put the anchor reset immediately adjacent to the
        // APC transfer so scroll/navigation frames cannot inherit that status
        // row and visibly drift down the pane.
        write!(writer, "\x1b[H")?;
        let chunk_size = self.chunk_size();
        for (index, chunk) in encoded.as_bytes().chunks(chunk_size).enumerate() {
            let more = u8::from((index + 1) * chunk_size < encoded.len());
            let chunk = std::str::from_utf8(chunk).context("base64 chunk should be UTF-8")?;
            let control = if index == 0 {
                match self.transport {
                    KittyTransport::StandardApc => format!(
                        "a=T,f=100,t=d,i={image_id},p={placement_id},c={columns},r={rows},C=1,q=2,m={more}"
                    ),
                    KittyTransport::FpasotermOsc => format!(
                        "a=T,f=100,t=d,i={image_id},p={placement_id},c={columns},r={rows},C=1,q=2,m={more}",
                    ),
                }
            } else {
                format!("m={more}")
            };
            match self.transport {
                KittyTransport::StandardApc => {
                    write!(writer, "\x1b_G{control};{chunk}\x1b\\")?;
                }
                KittyTransport::FpasotermOsc => {
                    write!(writer, "\x1b]777;graphics;v=1;{control};{chunk}\x1b\\")?;
                }
            }
        }

        // Keep two overlapping placements at the same geometry. The retired ID is two frames old,
        // so deleting it cannot leave the just-visible predecessor absent
        // while this transfer is decoded. A distinct placement id is important
        // here: fpasoterm's deferred delete must remove only that retired
        // canvas, never the newest replacement or its one-frame fallback.
        if self.retire_previous_frames
            && !geometry_changed
            && retained_image_id != 0
            && retained_image_id != previous_image_id
            && retained_image_id != image_id
        {
            self.write_delete(writer, retained_image_id)?;
        }

        // Do not leave a complete frame in a buffered stdout writer. The
        // Windows fpasoterm PTY consumes Kitty chunks asynchronously; making
        // the frame boundary explicit avoids waiting for a later status line
        // before its first APC becomes readable.
        writer.flush().context("flush Kitty Graphics frame")?;

        Ok(())
    }

    fn cleanup(&self, writer: &mut dyn Write) -> Result<()> {
        let image_id = self.active_image_id.swap(0, Ordering::Relaxed);
        let retained_image_id = self.retained_image_id.swap(0, Ordering::Relaxed);
        *self
            .last_render_size
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        if image_id != 0 {
            self.write_delete(writer, image_id)?;
        }
        if retained_image_id != 0 && retained_image_id != image_id {
            self.write_delete(writer, retained_image_id)?;
        }
        Ok(())
    }
}

fn encode_png(image: &DynamicImage) -> Result<Vec<u8>> {
    let mut cursor = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(opaque_terminal_rgba(image))
        .write_to(&mut cursor, ImageFormat::Png)
        .context("encode screenshot as PNG for Kitty Graphics Protocol")?;
    Ok(cursor.into_inner())
}

/// Terminal graphics have no browser background beneath them. Servo's Windows
/// software snapshot can retain meaningful RGB values while returning an
/// all-zero alpha channel. ANSI half-block rendering ignores that alpha, but
/// PNG and Canvas correctly treat it as fully transparent and leave the
/// terminal's black background visible. Present every browser frame as an
/// opaque terminal image so all backends use the same visible RGB content.
fn opaque_terminal_rgba(image: &DynamicImage) -> RgbaImage {
    let mut rgba = image.to_rgba8();
    for pixel in rgba.pixels_mut() {
        pixel[3] = u8::MAX;
    }
    rgba
}
