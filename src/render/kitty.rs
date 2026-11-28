use super::{RenderBackend, RenderSize};
use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use flate2::{write::ZlibEncoder, Compression};
use image::{DynamicImage, ImageFormat, RgbaImage};
use std::{
    fs,
    io::{Cursor, Read, Write},
    net::{IpAddr, SocketAddr, TcpStream},
    path::PathBuf,
    process,
    sync::{
        atomic::{AtomicU32, Ordering},
        Mutex,
    },
    time::Duration,
};

const CHUNK_SIZE: usize = 4096;
// Legacy fpasoterm OSC compatibility keeps its base64 slices deliberately
// small. Current Windows fpasoterm uses the authenticated IPC bridge below;
// native Kitty APC keeps its efficient 4 KiB chunks.
const FPASOTERM_OSC_CHUNK_SIZE: usize = 96;
const KITTY_ID_NAMESPACE: u32 = 0x4600_0000;
const FPASOTERM_IPC_MAGIC: [u8; 8] = *b"FPTGFX1\0";
const FPASOTERM_IPC_TOKEN_BYTES: usize = 32;
const FPASOTERM_IPC_TIMEOUT: Duration = Duration::from_millis(750);
const FPASOTERM_GRAPHICS_DISCOVERY_FILE: &str = "terminal-graphics-bridge-v1";
const FPASOTERM_GRAPHICS_DISCOVERY_HEADER: &str = "fpasoterm-graphics-bridge-v1";

/// Safe, value-free readiness state for the Windows fpasoterm graphics
/// bridge.  In particular, it never exposes the random loopback endpoint or
/// the per-terminal capability token in an issue report or terminal overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FpasotermGraphicsBridgeStatus {
    NotRequired,
    Ready,
    ReadyRecovered,
    TransportMissing,
    TransportUnsupported,
    EndpointMissing,
    EndpointInvalid,
    EndpointNotLoopback,
    TokenMissing,
    TokenInvalid,
    DiscoveryMissing,
    DiscoveryUnreadable,
    DiscoveryInvalid,
}

impl FpasotermGraphicsBridgeStatus {
    #[must_use]
    pub const fn report_label(self) -> &'static str {
        match self {
            Self::NotRequired => "not required on this platform",
            Self::Ready => "ready (inherited ipc-v1; endpoint and token redacted)",
            Self::ReadyRecovered => {
                "ready (recovered local ipc-v1 bridge; endpoint and token redacted)"
            }
            Self::TransportMissing => "unavailable: transport capability is missing",
            Self::TransportUnsupported => "unavailable: transport capability is not ipc-v1",
            Self::EndpointMissing => "unavailable: endpoint capability is missing",
            Self::EndpointInvalid => "unavailable: endpoint capability is invalid",
            Self::EndpointNotLoopback => "unavailable: endpoint is not loopback",
            Self::TokenMissing => "unavailable: token capability is missing",
            Self::TokenInvalid => "unavailable: token capability is invalid",
            Self::DiscoveryMissing => {
                "unavailable: capability was not inherited and no live fpasoterm bridge record was found"
            }
            Self::DiscoveryUnreadable => {
                "unavailable: fpasoterm bridge record could not be read"
            }
            Self::DiscoveryInvalid => {
                "unavailable: fpasoterm bridge record is invalid"
            }
        }
    }

    #[must_use]
    pub const fn is_ready(self) -> bool {
        matches!(self, Self::Ready | Self::ReadyRecovered)
    }
}

/// Describe whether the current process inherited a usable fpasoterm bridge,
/// without returning any sensitive capability value.
#[must_use]
pub fn fpasoterm_graphics_bridge_status() -> FpasotermGraphicsBridgeStatus {
    if !cfg!(target_os = "windows") {
        return FpasotermGraphicsBridgeStatus::NotRequired;
    }
    match fpasoterm_ipc_bridge_from_environment(&|name| std::env::var(name).ok()) {
        Ok(_) => FpasotermGraphicsBridgeStatus::Ready,
        // Herdr and comparable terminal servers can create panes from a
        // long-lived server that predates the fpasoterm terminal process.  In
        // that one case, recover the bounded authenticated bridge from
        // fpasoterm's current-user, short-lived record.
        Err(FpasotermGraphicsBridgeStatus::TransportMissing) => {
            match recover_fpasoterm_ipc_bridge() {
                Ok(_) => FpasotermGraphicsBridgeStatus::ReadyRecovered,
                Err(status) => status,
            }
        }
        Err(status) => status,
    }
}

#[cfg(test)]
fn fpasoterm_graphics_bridge_status_in_environment<F>(
    is_windows: bool,
    environment: &F,
) -> FpasotermGraphicsBridgeStatus
where
    F: Fn(&str) -> Option<String>,
{
    if !is_windows {
        FpasotermGraphicsBridgeStatus::NotRequired
    } else {
        fpasoterm_ipc_bridge_from_environment(environment)
            .map(|_| FpasotermGraphicsBridgeStatus::Ready)
            .unwrap_or_else(|status| status)
    }
}

/// Standard Kitty APC is the normal transport. `FpasotermOsc` remains only for
/// compatibility with experimental older fpasoterm builds; current Windows
/// fpasoterm uses `FpasotermIpc` because ConPTY can consume both APC and OSC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KittyTransport {
    StandardApc,
    FpasotermOsc,
    FpasotermIpc,
}

/// An inherited, per-terminal capability for the Windows fpasoterm graphics
/// bridge.  The listener is loopback-only and the token is generated by the
/// parent terminal process; neither is read from configuration or CLI input.
struct FpasotermIpcBridge {
    endpoint: SocketAddr,
    token: [u8; FPASOTERM_IPC_TOKEN_BYTES],
}

impl std::fmt::Debug for FpasotermIpcBridge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The endpoint and bearer token are intentionally absent even from
        // debug output: callers often include Debug values in support logs.
        formatter
            .debug_struct("FpasotermIpcBridge")
            .finish_non_exhaustive()
    }
}

impl FpasotermIpcBridge {
    fn from_environment() -> Option<Self> {
        if !cfg!(target_os = "windows") {
            return None;
        }
        match fpasoterm_ipc_bridge_from_environment(&|name| std::env::var(name).ok()) {
            Ok(bridge) => Some(bridge),
            Err(FpasotermGraphicsBridgeStatus::TransportMissing) => {
                recover_fpasoterm_ipc_bridge().ok()
            }
            Err(_) => None,
        }
    }

    fn send(
        &self,
        action: u8,
        image_id: u32,
        placement_id: u32,
        size: RenderSize,
        png: &[u8],
    ) -> Result<()> {
        let payload_len = u32::try_from(png.len()).context("fpasoterm IPC PNG is too large")?;
        let mut stream = TcpStream::connect_timeout(&self.endpoint, FPASOTERM_IPC_TIMEOUT)
            .with_context(|| {
                format!("connect to fpasoterm graphics bridge at {}", self.endpoint)
            })?;
        stream
            .set_write_timeout(Some(FPASOTERM_IPC_TIMEOUT))
            .context("set fpasoterm graphics bridge write timeout")?;
        stream
            .set_read_timeout(Some(FPASOTERM_IPC_TIMEOUT))
            .context("set fpasoterm graphics bridge read timeout")?;
        stream.write_all(&FPASOTERM_IPC_MAGIC)?;
        stream.write_all(&self.token)?;
        stream.write_all(&[action])?;
        stream.write_all(&image_id.to_be_bytes())?;
        stream.write_all(&placement_id.to_be_bytes())?;
        stream.write_all(&size.columns.max(1).to_be_bytes())?;
        stream.write_all(&size.rows.max(1).to_be_bytes())?;
        stream.write_all(&payload_len.to_be_bytes())?;
        stream.write_all(png)?;
        stream.flush()?;
        let mut reply = [0_u8; 1];
        stream.read_exact(&mut reply)?;
        anyhow::ensure!(reply == [1], "fpasoterm graphics bridge rejected the frame");
        Ok(())
    }

    fn send_delete(&self, image_id: u32) -> Result<()> {
        self.send(
            2,
            image_id,
            0,
            RenderSize {
                columns: 0,
                rows: 0,
            },
            &[],
        )
    }
}

fn fpasoterm_ipc_bridge_from_environment<F>(
    environment: &F,
) -> Result<FpasotermIpcBridge, FpasotermGraphicsBridgeStatus>
where
    F: Fn(&str) -> Option<String>,
{
    fpasoterm_ipc_bridge_from_values(
        environment("FPASOTERM_GRAPHICS_TRANSPORT").as_deref(),
        environment("FPASOTERM_GRAPHICS_ENDPOINT").as_deref(),
        environment("FPASOTERM_GRAPHICS_TOKEN").as_deref(),
    )
}

fn fpasoterm_ipc_bridge_from_values(
    transport: Option<&str>,
    endpoint: Option<&str>,
    token: Option<&str>,
) -> Result<FpasotermIpcBridge, FpasotermGraphicsBridgeStatus> {
    match transport {
        None => return Err(FpasotermGraphicsBridgeStatus::TransportMissing),
        Some(value) if !value.eq_ignore_ascii_case("ipc-v1") => {
            return Err(FpasotermGraphicsBridgeStatus::TransportUnsupported);
        }
        Some(_) => {}
    }
    let endpoint: SocketAddr = endpoint
        .ok_or(FpasotermGraphicsBridgeStatus::EndpointMissing)?
        .parse()
        .map_err(|_| FpasotermGraphicsBridgeStatus::EndpointInvalid)?;
    if !matches!(endpoint.ip(), IpAddr::V4(address) if address.is_loopback()) {
        return Err(FpasotermGraphicsBridgeStatus::EndpointNotLoopback);
    }
    let token = STANDARD
        .decode(token.ok_or(FpasotermGraphicsBridgeStatus::TokenMissing)?)
        .map_err(|_| FpasotermGraphicsBridgeStatus::TokenInvalid)?;
    let token: [u8; FPASOTERM_IPC_TOKEN_BYTES] = token
        .try_into()
        .map_err(|_| FpasotermGraphicsBridgeStatus::TokenInvalid)?;
    Ok(FpasotermIpcBridge { endpoint, token })
}

fn recover_fpasoterm_ipc_bridge() -> Result<FpasotermIpcBridge, FpasotermGraphicsBridgeStatus> {
    let path = std::env::var_os("LOCALAPPDATA")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(|root| {
            root.join("fpasoterm")
                .join(FPASOTERM_GRAPHICS_DISCOVERY_FILE)
        })
        .ok_or(FpasotermGraphicsBridgeStatus::DiscoveryMissing)?;
    let record = fs::read_to_string(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            FpasotermGraphicsBridgeStatus::DiscoveryMissing
        } else {
            FpasotermGraphicsBridgeStatus::DiscoveryUnreadable
        }
    })?;
    parse_fpasoterm_graphics_discovery_record(&record)
}

fn parse_fpasoterm_graphics_discovery_record(
    record: &str,
) -> Result<FpasotermIpcBridge, FpasotermGraphicsBridgeStatus> {
    let mut lines = record.lines();
    let header = lines.next();
    let transport = lines.next();
    let endpoint = lines.next();
    let token = lines.next();
    if header != Some(FPASOTERM_GRAPHICS_DISCOVERY_HEADER) || lines.next().is_some() {
        return Err(FpasotermGraphicsBridgeStatus::DiscoveryInvalid);
    }
    fpasoterm_ipc_bridge_from_values(transport, endpoint, token)
        .map_err(|_| FpasotermGraphicsBridgeStatus::DiscoveryInvalid)
}

#[derive(Debug)]
pub struct KittyBackend {
    transport: KittyTransport,
    ipc_bridge: Option<FpasotermIpcBridge>,
    /// A normal terminal can release the frame from two updates ago as soon as
    /// the next transfer is submitted. fpasoterm's decoder is asynchronous,
    /// however, and needs its own deferred replacement protocol.
    retire_previous_frames: bool,
    /// fpasoterm defers a delete sent immediately before a replacement until
    /// that replacement paints. This preserves one fallback frame without
    /// leaving a second full-screen placement after rendering becomes idle.
    defer_previous_frame_retirement: bool,
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
    /// Successful complete frame submissions from this browser process.
    emitted_frame_count: AtomicU32,
    /// Successful Kitty/fpasoterm delete commands sent by this process.
    emitted_delete_count: AtomicU32,
    /// Deletes deliberately sent before a replacement for fpasoterm's
    /// deferred-retirement receiver policy.
    emitted_replacement_delete_count: AtomicU32,
}

impl KittyBackend {
    #[must_use]
    pub fn new() -> Self {
        Self::with_frame_retirement(true, false, KittyTransport::StandardApc)
    }

    /// Use fpasoterm's deferred-delete replacement protocol with the standard
    /// PNG transport, preventing completed frames from stacking in idle panes.
    #[must_use]
    pub fn for_fpasoterm() -> Self {
        Self::with_frame_retirement(false, true, KittyTransport::StandardApc)
    }

    /// Use the legacy experimental fpasoterm OSC bridge. New Windows builds
    /// use [`Self::for_windows_fpasoterm_ipc`] instead.
    #[must_use]
    pub fn for_windows_fpasoterm() -> Self {
        Self::with_frame_retirement(false, true, KittyTransport::FpasotermOsc)
    }

    /// Use fpasoterm's authenticated local bridge when Windows ConPTY strips
    /// both APC and OSC control strings before xterm.js can parse them.
    #[must_use]
    pub fn for_windows_fpasoterm_ipc() -> Option<Self> {
        FpasotermIpcBridge::from_environment().map(|bridge| {
            let mut backend =
                Self::with_frame_retirement(false, false, KittyTransport::FpasotermIpc);
            backend.ipc_bridge = Some(bridge);
            backend
        })
    }

    fn with_frame_retirement(
        retire_previous_frames: bool,
        defer_previous_frame_retirement: bool,
        transport: KittyTransport,
    ) -> Self {
        let scoped_id = KITTY_ID_NAMESPACE | (process::id() & 0x00ff_ffff);
        Self {
            transport,
            ipc_bridge: None,
            retire_previous_frames,
            defer_previous_frame_retirement,
            active_image_id: AtomicU32::new(0),
            retained_image_id: AtomicU32::new(0),
            last_render_size: Mutex::new(None),
            next_image_id: AtomicU32::new(scoped_id),
            next_placement_id: AtomicU32::new(scoped_id),
            emitted_frame_count: AtomicU32::new(0),
            emitted_delete_count: AtomicU32::new(0),
            emitted_replacement_delete_count: AtomicU32::new(0),
        }
    }

    /// Describe the active wire representation for human-readable diagnostics.
    #[must_use]
    pub const fn transport_label(&self) -> &'static str {
        match self.transport {
            KittyTransport::StandardApc => "kitty-png",
            KittyTransport::FpasotermOsc => "fpasoterm-osc-rgba-zlib",
            KittyTransport::FpasotermIpc => "fpasoterm-ipc-png",
        }
    }

    /// Report only browser-side writes. This deliberately avoids claiming a
    /// receiver acknowledgement: a terminal multiplexer may consume, rewrite,
    /// or delay a control sequence after this process has submitted it.
    #[must_use]
    pub fn sender_diagnostic_summary(&self) -> String {
        format!(
            "emitted frames={} deletes={} pre-replacement deletes={}; receiver acknowledgement unavailable",
            self.emitted_frame_count.load(Ordering::Relaxed),
            self.emitted_delete_count.load(Ordering::Relaxed),
            self.emitted_replacement_delete_count.load(Ordering::Relaxed),
        )
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
            KittyTransport::FpasotermIpc => self
                .ipc_bridge
                .as_ref()
                .context("fpasoterm IPC transport has no bridge")?
                .send_delete(image_id)?,
        }
        self.emitted_delete_count.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    const fn chunk_size(&self) -> usize {
        match self.transport {
            KittyTransport::StandardApc => CHUNK_SIZE,
            KittyTransport::FpasotermOsc => FPASOTERM_OSC_CHUNK_SIZE,
            KittyTransport::FpasotermIpc => CHUNK_SIZE,
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
            KittyTransport::StandardApc => Some(STANDARD.encode(encode_png(image)?)),
            KittyTransport::FpasotermOsc => Some(STANDARD.encode(compress_rgba(image)?)),
            KittyTransport::FpasotermIpc => None,
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

        if self.defer_previous_frame_retirement && previous_image_id != 0 {
            // fpasoterm snapshots this deletion and applies it only once the
            // following placement has painted.  Sending it before the new
            // transfer avoids both a black gap and an old full-page frame
            // remaining forever when the browser becomes idle after two
            // renders (the duplicated Baguette/Herdr symptom).
            self.write_delete(writer, previous_image_id)?;
            self.emitted_replacement_delete_count
                .fetch_add(1, Ordering::Relaxed);
        } else if self.retire_previous_frames && geometry_changed {
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

        if let Some(encoded) = encoded {
            // Kitty placements are anchored at the terminal cursor. `redraw`
            // also queues MoveTo(0, 0), but an activity/status write can be
            // buffered by an outer terminal or multiplexer until the graphics
            // payload is parsed. Put the anchor reset immediately adjacent to
            // the terminal transport so a navigation frame cannot inherit a
            // status row and visibly drift down the pane.
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
                            "a=T,f=32,t=d,i={image_id},p={placement_id},c={columns},r={rows},s={},v={},o=z,C=1,q=2,m={more}",
                            image.width(),
                            image.height(),
                        ),
                        KittyTransport::FpasotermIpc => unreachable!("IPC frames are not terminal chunks"),
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
                    KittyTransport::FpasotermIpc => {
                        unreachable!("IPC frames are not terminal chunks")
                    }
                }
            }
        } else {
            self.ipc_bridge
                .as_ref()
                .context("fpasoterm IPC transport has no bridge")?
                .send(1, image_id, placement_id, size, &encode_png(image)?)?;
        }

        // Keep two overlapping placements at the same geometry. The retired ID is two frames old,
        // so deleting it cannot leave the just-visible predecessor absent
        // while this transfer is decoded. A distinct placement id is important
        // here: fpasoterm's deferred delete must remove only that retired
        // canvas, never the newest replacement or its one-frame fallback.
        if self.retire_previous_frames
            && !self.defer_previous_frame_retirement
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
        self.emitted_frame_count.fetch_add(1, Ordering::Relaxed);

        Ok(())
    }

    fn sender_diagnostic_summary(&self) -> String {
        Self::sender_diagnostic_summary(self)
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

fn compress_rgba(image: &DynamicImage) -> Result<Vec<u8>> {
    let rgba = opaque_terminal_rgba(image);
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
    encoder
        .write_all(&rgba.into_raw())
        .context("compress raw RGBA for fpasoterm graphics bridge")?;
    encoder
        .finish()
        .context("finish compressed fpasoterm RGBA graphics payload")
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::TcpListener, thread};

    #[test]
    fn graphics_bridge_report_redacts_a_ready_capability() {
        let token = STANDARD.encode([7_u8; FPASOTERM_IPC_TOKEN_BYTES]);
        let status = fpasoterm_graphics_bridge_status_in_environment(true, &|name| match name {
            "FPASOTERM_GRAPHICS_TRANSPORT" => Some(String::from("ipc-v1")),
            "FPASOTERM_GRAPHICS_ENDPOINT" => Some(String::from("127.0.0.1:45678")),
            "FPASOTERM_GRAPHICS_TOKEN" => Some(token.clone()),
            _ => None,
        });

        assert_eq!(status, FpasotermGraphicsBridgeStatus::Ready);
        assert_eq!(
            status.report_label(),
            "ready (inherited ipc-v1; endpoint and token redacted)"
        );
        assert!(!status.report_label().contains("45678"));
        assert!(!status.report_label().contains(&token));
    }

    #[test]
    fn sender_report_is_explicitly_sender_only_before_any_frame() {
        let backend = KittyBackend::for_fpasoterm();
        assert_eq!(
            backend.sender_diagnostic_summary(),
            "emitted frames=0 deletes=0 pre-replacement deletes=0; receiver acknowledgement unavailable"
        );
    }

    #[test]
    fn graphics_bridge_report_explains_missing_inheritance() {
        let status = fpasoterm_graphics_bridge_status_in_environment(true, &|_| None);
        assert_eq!(status, FpasotermGraphicsBridgeStatus::TransportMissing);
        assert_eq!(
            status.report_label(),
            "unavailable: transport capability is missing"
        );
    }

    #[test]
    fn graphics_bridge_recovery_record_keeps_the_capability_redacted() {
        let token = STANDARD.encode([9_u8; FPASOTERM_IPC_TOKEN_BYTES]);
        let record =
            format!("{FPASOTERM_GRAPHICS_DISCOVERY_HEADER}\nipc-v1\n127.0.0.1:45678\n{token}\n");

        let bridge = parse_fpasoterm_graphics_discovery_record(&record)
            .expect("valid current-user fpasoterm bridge record");
        assert_eq!(bridge.endpoint, "127.0.0.1:45678".parse().unwrap());
        assert_eq!(bridge.token, [9_u8; FPASOTERM_IPC_TOKEN_BYTES]);
        assert!(!format!("{bridge:?}").contains("45678"));
        assert!(!format!("{bridge:?}").contains(&token));
        assert_eq!(
            FpasotermGraphicsBridgeStatus::ReadyRecovered.report_label(),
            "ready (recovered local ipc-v1 bridge; endpoint and token redacted)"
        );
    }

    #[test]
    fn graphics_bridge_recovery_rejects_extra_or_malformed_fields() {
        let record = format!(
            "{FPASOTERM_GRAPHICS_DISCOVERY_HEADER}\nipc-v1\n127.0.0.1:45678\nnot-base64\nextra\n"
        );
        assert!(matches!(
            parse_fpasoterm_graphics_discovery_record(&record),
            Err(FpasotermGraphicsBridgeStatus::DiscoveryInvalid)
        ));
    }

    #[test]
    fn fpasoterm_ipc_bridge_sends_a_bounded_authenticated_png_frame() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback test listener");
        let endpoint = listener.local_addr().expect("read loopback address");
        let token = [42_u8; FPASOTERM_IPC_TOKEN_BYTES];
        let expected_token = token;
        let receiver = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept bridge connection");
            let mut magic = [0_u8; FPASOTERM_IPC_MAGIC.len()];
            stream.read_exact(&mut magic).expect("read magic");
            assert_eq!(magic, FPASOTERM_IPC_MAGIC);
            let mut received_token = [0_u8; FPASOTERM_IPC_TOKEN_BYTES];
            stream.read_exact(&mut received_token).expect("read token");
            assert_eq!(received_token, expected_token);
            let mut action = [0_u8; 1];
            stream.read_exact(&mut action).expect("read action");
            assert_eq!(action, [1]);
            let mut image_id = [0_u8; 4];
            stream.read_exact(&mut image_id).expect("read image id");
            assert_eq!(u32::from_be_bytes(image_id), 7);
            let mut placement_id = [0_u8; 4];
            stream
                .read_exact(&mut placement_id)
                .expect("read placement id");
            assert_eq!(u32::from_be_bytes(placement_id), 8);
            let mut columns = [0_u8; 2];
            stream.read_exact(&mut columns).expect("read columns");
            assert_eq!(u16::from_be_bytes(columns), 10);
            let mut rows = [0_u8; 2];
            stream.read_exact(&mut rows).expect("read rows");
            assert_eq!(u16::from_be_bytes(rows), 5);
            let mut payload_length = [0_u8; 4];
            stream
                .read_exact(&mut payload_length)
                .expect("read payload length");
            let mut payload = vec![0_u8; u32::from_be_bytes(payload_length) as usize];
            stream.read_exact(&mut payload).expect("read payload");
            assert_eq!(payload, b"png");
            stream.write_all(&[1]).expect("acknowledge frame");
        });
        let bridge = FpasotermIpcBridge { endpoint, token };
        bridge
            .send(
                1,
                7,
                8,
                RenderSize {
                    columns: 10,
                    rows: 5,
                },
                b"png",
            )
            .expect("send bridge frame");
        receiver.join().expect("join bridge receiver");
    }
}
