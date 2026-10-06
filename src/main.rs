// Recent Rust toolchains surface successful macOS linker stderr as warnings.
// Servo's monolithic binary can exceed ld's compact-unwind encoding budget;
// the linker already falls back to DWARF unwind data and this is not a build
// or runtime failure. Suppress that platform-specific toolchain diagnostic
// without changing the application's panic or exception-handling behaviour.
#![cfg_attr(target_os = "macos", allow(linker_messages))]

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use clap::Parser;
use fpasobrowser::{
    detailed_version_info,
    render::{BackendChoice, TerminalCellPixels},
};
use std::{
    io::{self, Write},
    path::PathBuf,
};
use url::Url;

mod app;
#[path = "servo_browser.rs"]
mod browser;
mod input;
mod terminal;

use app::AppOptions;
use browser::{BrowserOptions, BrowserViewport};
pub use fpasobrowser::{render, version_info};
use terminal::UiChrome;

#[derive(Debug, Parser)]
#[command(author, version, about, long_version = detailed_version_info())]
struct Cli {
    /// Print detailed version/build information and exit.
    #[arg(long)]
    version_info: bool,

    /// Rendering backend to use. The Windows default is portable half-block; other platforms default to Kitty.
    #[arg(long, value_enum, env = "FPASOBROWSER_BACKEND")]
    backend: Option<BackendChoice>,

    /// Terminal UI outside the page: "full" shows status, tabs, and mouse toolbar; "compact" keeps only status; "page" hides all text chrome.
    #[arg(long, value_enum, default_value_t = UiChrome::Full, env = "FPASOBROWSER_UI")]
    ui: UiChrome,

    /// Browser viewport in pixels, for example 1280x720.
    #[arg(long, value_parser = parse_viewport, env = "FPASOBROWSER_VIEWPORT")]
    viewport: Option<BrowserViewport>,

    /// Physical pixel size of one terminal cell for the SIXEL backend, for example 8x17.
    /// This keeps the SIXEL raster, Servo viewport, and mouse coordinates aligned.
    #[arg(
        long,
        value_parser = parse_terminal_cell_pixels,
        env = "FPASOBROWSER_SIXEL_CELL_SIZE",
        value_name = "WIDTHxHEIGHT"
    )]
    sixel_cell_size: Option<TerminalCellPixels>,

    /// Save the captured screenshot PNG to this path on each redraw.
    #[arg(long)]
    dump_screenshot: Option<PathBuf>,

    /// Capture one screenshot and exit without entering the terminal UI.
    #[arg(long)]
    capture_only: bool,

    /// Print diagnostic progress messages to stderr.
    #[arg(long)]
    debug: bool,

    /// URL to open. When omitted, show fpasobrowser's built-in start page.
    url: Option<Url>,
}

fn main() -> Result<()> {
    // Clap owns the conventional `--version` / `-V` flags. `-v` is kept as a
    // fpasoterm-compatible alias, but must be handled before parsing so it
    // cannot become a required field or initialize the embedded Servo runtime.
    if std::env::args_os().skip(1).any(|argument| argument == "-v") {
        print_version_info_and_exit()?;
    }
    let cli = Cli::parse();
    if cli.version_info {
        print_version_info_and_exit()?;
    }
    let capture_only = cli.capture_only;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("create fpasobrowser async runtime")?;
    let result = runtime.block_on(run(cli));

    if capture_only {
        // Servo can leave process-global platform resources behind after an
        // otherwise successful offscreen capture. The PNG is already flushed
        // before run returns; bypassing Tokio/Servo's process-exit teardown
        // avoids a post-capture crash while retaining the conventional exit
        // status for this one-shot command.
        match result {
            Ok(()) => std::process::exit(0),
            Err(error) => {
                eprintln!("Error: {error:#}");
                std::process::exit(1);
            }
        }
    }

    // The interactive path explicitly joins its Servo owner thread before
    // returning.  Let Rust/Tokio then unwind normally: forcing process exit
    // while Servo still owns Mozilla mutexes can race its native destructors
    // and crash at shutdown.
    result
}

fn print_version_info_and_exit() -> Result<()> {
    // Do not construct Tokio or enter Servo's linked runtime for a simple
    // metadata query. In particular, process termination after a Tokio
    // runtime had been created could wait indefinitely on this platform.
    // Flushing before the explicit exit preserves the expected CLI output.
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{}", version_info()).context("write version information")?;
    stdout.flush().context("flush version information")?;
    std::process::exit(0);
}

async fn run(cli: Cli) -> Result<()> {
    let backend = cli.backend.unwrap_or_else(default_backend);
    let url = match cli.url {
        Some(url) => url,
        None => default_start_page_url()?,
    };
    if cli.sixel_cell_size.is_some() && backend != BackendChoice::Sixel {
        anyhow::bail!("--sixel-cell-size requires --backend sixel");
    }
    let options = AppOptions {
        browser: BrowserOptions {
            debug: cli.debug,
            viewport: cli.viewport,
            terminal_cell_pixels: None,
        },
        dump_screenshot: cli.dump_screenshot,
        capture_only: cli.capture_only,
        sixel_cell_pixels: cli.sixel_cell_size,
        ui_chrome: cli.ui,
    };
    app::run(url, backend, options).await
}

/// Select an image transport that is visible in the ordinary terminal for the
/// current platform. Windows Terminal and the PowerShell host do not implement
/// Kitty Graphics, whereas fpasoterm does; callers can explicitly opt into
/// `--backend kitty` (or `FPASOBROWSER_BACKEND=kitty`) in that host.
const fn default_backend() -> BackendChoice {
    #[cfg(target_os = "windows")]
    {
        BackendChoice::HalfBlock
    }
    #[cfg(not(target_os = "windows"))]
    {
        BackendChoice::Kitty
    }
}

/// Create a portable, self-contained start page for URL-less launches.
///
/// A `data:` URL avoids a path next to the executable, so the same entry point
/// works from a source checkout, a release archive, and an installed binary.
/// The page deliberately keeps navigation in the terminal viewer: `g` or the
/// visible `[URL]` toolbar opens the normal URL prompt.
fn default_start_page_url() -> Result<Url> {
    const PAGE: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>fpasobrowser</title>
<style>
body { background:#10151d; color:#e6edf3; font:18px system-ui,sans-serif; margin:0; }
main { max-width:760px; margin:12vh auto; padding:0 2rem; }
h1 { color:#7dd3fc; margin-bottom:.2rem; } code { color:#facc15; }
a { color:#7dd3fc; } li { margin:.6rem 0; } .note { color:#a7b1c2; }
</style></head><body><main>
<h1>fpasobrowser</h1>
<p>Embedded Servo terminal web viewer</p>
<p>Press <code>g</code> or click <code>[URL]</code> in the toolbar to open an address.</p>
<ul>
<li><a href="https://example.com/">Open example.com</a></li>
<li><a href="https://www.google.com/">Open Google</a></li>
<li><a href="https://www.rust-lang.org/">Open rust-lang.org</a></li>
</ul>
<p class="note">Press <code>?</code> for controls. Use <code>i</code> before typing into a page field.</p>
</main></body></html>"#;
    let encoded = STANDARD.encode(PAGE);
    Url::parse(&format!("data:text/html;charset=utf-8;base64,{encoded}"))
        .context("construct built-in start page URL")
}

fn parse_viewport(value: &str) -> Result<BrowserViewport> {
    let (width, height) = value
        .split_once('x')
        .or_else(|| value.split_once('X'))
        .with_context(|| format!("viewport must be WIDTHxHEIGHT, got {value:?}"))?;
    let width = width
        .parse::<u32>()
        .with_context(|| format!("invalid viewport width in {value:?}"))?;
    let height = height
        .parse::<u32>()
        .with_context(|| format!("invalid viewport height in {value:?}"))?;
    if width == 0 || height == 0 {
        anyhow::bail!("viewport dimensions must be greater than zero");
    }
    Ok(BrowserViewport { width, height })
}

fn parse_terminal_cell_pixels(value: &str) -> Result<TerminalCellPixels> {
    let (width, height) = value
        .split_once('x')
        .or_else(|| value.split_once('X'))
        .with_context(|| {
            format!(
                "SIXEL cell size must be WIDTHxHEIGHT, got {value:?}; use the terminal cell's physical pixel dimensions"
            )
        })?;
    let width = width
        .parse::<u32>()
        .with_context(|| format!("invalid SIXEL cell width in {value:?}"))?;
    let height = height
        .parse::<u32>()
        .with_context(|| format!("invalid SIXEL cell height in {value:?}"))?;
    if width == 0 || height == 0 {
        anyhow::bail!("SIXEL cell dimensions must be greater than zero");
    }
    Ok(TerminalCellPixels { width, height })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omitted_backend_uses_the_platform_default() {
        let cli = Cli::try_parse_from(["fpasobrowser", "https://example.com/"])
            .expect("parse default CLI invocation");
        assert_eq!(cli.backend, None);
        #[cfg(target_os = "windows")]
        assert_eq!(default_backend(), BackendChoice::HalfBlock);
        #[cfg(not(target_os = "windows"))]
        assert_eq!(default_backend(), BackendChoice::Kitty);
    }

    #[test]
    fn omitted_url_opens_the_embedded_start_page() {
        let cli = Cli::try_parse_from(["fpasobrowser"]).expect("parse URL-less invocation");
        assert!(cli.url.is_none());

        let url = default_start_page_url().expect("create start page URL");
        assert_eq!(url.scheme(), "data");
        assert!(url.as_str().contains("base64,"));
    }

    #[test]
    fn compact_ui_option_is_parsed() {
        let cli = Cli::try_parse_from(["fpasobrowser", "--ui", "compact"])
            .expect("parse compact terminal UI option");
        assert_eq!(cli.ui, UiChrome::Compact);
    }

    #[test]
    fn sixel_cell_size_accepts_physical_pixel_dimensions() {
        assert_eq!(
            parse_terminal_cell_pixels("8x17").expect("parse SIXEL cell size"),
            TerminalCellPixels {
                width: 8,
                height: 17
            }
        );
        assert_eq!(
            Cli::try_parse_from([
                "fpasobrowser",
                "--backend",
                "sixel",
                "--sixel-cell-size",
                "9X18",
                "https://example.com/"
            ])
            .expect("parse SIXEL override")
            .sixel_cell_size,
            Some(TerminalCellPixels {
                width: 9,
                height: 18
            })
        );
        assert!(parse_terminal_cell_pixels("0x17").is_err());
    }
}
