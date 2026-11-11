use super::{RenderBackend, RenderSize};
use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use image::{DynamicImage, ImageFormat};
use std::{
    io::{Cursor, Write},
    process,
    sync::{
        atomic::{AtomicU32, Ordering},
        Mutex,
    },
};

const CHUNK_SIZE: usize = 4096;
const KITTY_ID_NAMESPACE: u32 = 0x4600_0000;

#[derive(Debug)]
pub struct KittyBackend {
    /// The image for the newest frame.
    active_image_id: AtomicU32,
    /// Keep the immediately preceding frame alive while the newest transfer is
    /// decoded. Deleting it in the same PTY batch as a replacement can expose
    /// a black interval in fpasoterm/Herdr even though the transfer appears
    /// before the delete in the byte stream.
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
        let scoped_id = KITTY_ID_NAMESPACE | (process::id() & 0x00ff_ffff);
        Self {
            active_image_id: AtomicU32::new(0),
            retained_image_id: AtomicU32::new(0),
            last_render_size: Mutex::new(None),
            next_image_id: AtomicU32::new(scoped_id),
            next_placement_id: AtomicU32::new(scoped_id),
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
        let png = encode_png(image)?;
        let encoded = STANDARD.encode(png);
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

        if geometry_changed {
            // fpasoterm defers this delete until the following replacement
            // has painted.  Sending it before the transfer lets that renderer
            // pair the two operations, while a different-sized stale frame is
            // never retained long enough to overlap the new bottom/right edge.
            for stale_image_id in [previous_image_id, retained_image_id] {
                if stale_image_id != 0 && stale_image_id != image_id {
                    write!(writer, "\x1b_Ga=d,d=I,i={stale_image_id},q=2\x1b\\")?;
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
        for (index, chunk) in encoded.as_bytes().chunks(CHUNK_SIZE).enumerate() {
            let more = u8::from((index + 1) * CHUNK_SIZE < encoded.len());
            let chunk = std::str::from_utf8(chunk).context("base64 chunk should be UTF-8")?;
            if index == 0 {
                write!(
                    writer,
                    "\x1b_Ga=T,f=100,t=d,i={image_id},p={placement_id},c={columns},r={rows},C=1,q=2,m={more};{chunk}\x1b\\"
                )?;
            } else {
                write!(writer, "\x1b_Gm={more};{chunk}\x1b\\")?;
            }
        }

        // Keep two overlapping placements at the same geometry. The retired ID is two frames old,
        // so deleting it cannot leave the just-visible predecessor absent
        // while this transfer is decoded. A distinct placement id is important
        // here: fpasoterm's deferred delete must remove only that retired
        // canvas, never the newest replacement or its one-frame fallback.
        if !geometry_changed
            && retained_image_id != 0
            && retained_image_id != previous_image_id
            && retained_image_id != image_id
        {
            write!(writer, "\x1b_Ga=d,d=I,i={retained_image_id},q=2\x1b\\")?;
        }

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
            write!(writer, "\x1b_Ga=d,d=I,i={image_id},q=2\x1b\\")?;
        }
        if retained_image_id != 0 && retained_image_id != image_id {
            write!(writer, "\x1b_Ga=d,d=I,i={retained_image_id},q=2\x1b\\")?;
        }
        Ok(())
    }
}

fn encode_png(image: &DynamicImage) -> Result<Vec<u8>> {
    let mut cursor = Cursor::new(Vec::new());
    image
        .write_to(&mut cursor, ImageFormat::Png)
        .context("encode screenshot as PNG for Kitty Graphics Protocol")?;
    Ok(cursor.into_inner())
}
