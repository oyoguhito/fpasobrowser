use crate::{
    browser::{
        BrowserClient, BrowserOptions, CloseTabOutcome, LinkHintOutcome, PageState, TabInfo,
    },
    input::{self, Command, InputMode},
    render::{BackendChoice, RenderBackend, SelectedBackend, TerminalCellPixels},
    terminal::{Terminal, UiChrome},
    version_info,
};
use anyhow::{Context, Result};
use crossterm::{
    cursor::MoveTo,
    event::{self, Event, KeyCode, KeyEventKind},
    queue,
    style::Print,
    terminal::{Clear, ClearType},
};
use image::DynamicImage;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio::time::{sleep, timeout};
use url::Url;

const ACTION_REDRAW_DELAY: Duration = Duration::from_millis(450);
const SIXEL_ACTION_REDRAW_DELAY: Duration = Duration::from_millis(850);
const ACTION_REDRAW_INTERVAL: Duration = Duration::from_millis(350);
// Scroll reports arrive in bursts (especially from a wheel or touchpad). Keep
// the preceding Kitty frame visible until the burst settles, then transfer one
// final screenshot instead of visibly replacing the full image for every
// notch.
const SCROLL_REDRAW_DELAY: Duration = Duration::from_millis(180);
// A navigation can outlive the short action redraw burst (for example when a
// page starts a redirect after a click). Once Servo reports `loading`, keep
// sampling frames until it reports completion rather than leaving an old page
// visible after a fixed number of attempts.
const LOADING_REDRAW_INTERVAL: Duration = Duration::from_millis(500);
// SIXEL uses a complete image transfer for each changed frame.  Keep polling
// while a navigation is pending so a click that first captured the old page is
// eventually replaced by the destination, but sample less often than the
// ordinary terminal path.  `PresentationFingerprint` suppresses transfers
// when that sampled frame is unchanged.
const SIXEL_LOADING_REDRAW_INTERVAL: Duration = Duration::from_millis(850);
const INPUT_REDRAW_DELAY: Duration = Duration::from_millis(90);
// Re-emitting a complete SIXEL frame for every typed character can saturate
// the PTY and delay the next mouse event. Status feedback stays immediate;
// only the screenshot refresh is debounced a little longer.
const SIXEL_INPUT_REDRAW_DELAY: Duration = Duration::from_millis(350);
const HOVER_UPDATE_MIN_INTERVAL: Duration = Duration::from_millis(70);
const HOVER_TIMEOUT: Duration = Duration::from_millis(350);
const FOCUS_BLUR_TIMEOUT: Duration = Duration::from_millis(350);
const INPUT_MODE_KEY_SUPPRESS_WINDOW: Duration = Duration::from_millis(350);
// Keyboard scrolling remains a useful page-sized movement, but wheel events
// arrive in several terminal reports for one physical notch. Keep the latter
// deliberately smaller so one wheel gesture does not skip interactive content.
const KEY_SCROLL_PIXELS: i64 = 160;
const WHEEL_SCROLL_PIXELS: i64 = 96;
// Servo's screenshot and DOM-query paths can legitimately wait for a frame
// after a click or a navigation. This must exceed FRAME_WAIT_STEPS in the
// engine; otherwise Tokio abandons its reply while the engine is still healthy
// and the UI incorrectly enters the disconnected screen.
const REDRAW_TIMEOUT: Duration = Duration::from_secs(12);
const BROWSER_START_TIMEOUT: Duration = Duration::from_secs(35);
const RELAUNCH_TIMEOUT: Duration = Duration::from_secs(12);
const TOOLBAR_BUTTONS: &[(&str, &str)] = &[
    ("[B]", "back"),
    ("[F]", "forward"),
    ("[R]", "reload"),
    ("[URL]", "url"),
    ("[+Tab]", "new tab"),
    ("[<Tab]", "previous tab"),
    ("[Tab>]", "next tab"),
    ("[V]", "verification"),
    ("[?]", "help"),
];
const DEFAULT_ACTIVITY_MESSAGE: &str =
    "?:help i:input g/t:URL [/] back/forward {/} tabs click/wheel";

#[derive(Debug, Clone, Default)]
pub struct AppOptions {
    pub browser: BrowserOptions,
    pub dump_screenshot: Option<PathBuf>,
    pub capture_only: bool,
    /// Optional physical geometry override for the SIXEL backend. It is kept
    /// outside BrowserOptions until the renderer has been selected, then the
    /// exact same value is supplied to Servo for pointer/viewport alignment.
    pub sixel_cell_pixels: Option<TerminalCellPixels>,
    /// Controls how many terminal rows remain outside the rendered page.
    pub ui_chrome: UiChrome,
}

struct RedrawOptions<'a> {
    input_mode: InputMode,
    message: &'a str,
    show_help: bool,
    /// Monotonic frame counter used only for the loading activity indicator.
    /// It is deliberately independent of page rendering so SIXEL can keep its
    /// lower polling frequency without a separately animated terminal loop.
    loading_frame: u8,
    dump_screenshot: Option<&'a std::path::Path>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RedrawOutcome {
    is_loading: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PresentationFingerprint {
    image_hash: u64,
    image_width: u32,
    image_height: u32,
    size: crate::render::RenderSize,
    show_help: bool,
}

pub async fn run(url: Url, backend_choice: BackendChoice, options: AppOptions) -> Result<()> {
    loop {
        match run_once(url.clone(), backend_choice, options.clone()).await {
            Ok(()) => return Ok(()),
            Err(error) => {
                eprintln!("fpasobrowser recovered from UI error: {error:#}");
                // Keep an unexpected embedded-browser disconnect inside the TUI.
                // Previously the loop immediately re-entered `run_once`; if
                // relaunch failed as well, users saw the pane disappear or a
                // blank/status flicker with no actionable error. Wait in a
                // recovery screen so `r` retries and `q` exits cleanly.
                if !wait_for_run_error(
                    &error,
                    backend_choice,
                    options.sixel_cell_pixels,
                    options.ui_chrome,
                )? {
                    return Ok(());
                }
            }
        }
    }
}

/// Stop Servo on its owner thread before an interactive process exits.
///
/// This is deliberately awaited rather than relying on `BrowserClient::drop`:
/// an unjoined Servo thread can still own Mozilla synchronization primitives
/// while C/C++ process teardown starts, which has produced a segmentation
/// fault after the terminal viewer itself disappeared. The engine loop checks
/// its command channel at most once per frame interval, so a healthy shutdown
/// completes promptly and returns the shell to its prompt cleanly.
async fn shutdown_browser_on_exit(
    terminal: &mut Terminal,
    size: crate::render::RenderSize,
    browser: BrowserClient,
) {
    // Joining the Servo owner thread is intentionally safe rather than a
    // process-level shortcut: terminating while its native resources are
    // still live previously produced a Mozilla mutex error and a crash. Make
    // that wait explicit to the person using the terminal. A second message
    // after a second distinguishes a normal short shutdown from a page/engine
    // operation that is still unwinding.
    let _ = show_working(terminal, size, "closing embedded browser safely");
    let shutdown = browser.shutdown();
    tokio::pin!(shutdown);
    let still_waiting = sleep(Duration::from_secs(1));
    tokio::pin!(still_waiting);
    let result = tokio::select! {
        result = &mut shutdown => result,
        _ = &mut still_waiting => {
            let _ = show_working(terminal, size, "still closing Servo; please wait");
            shutdown.await
        }
    };
    if let Err(error) = result {
        eprintln!("fpasobrowser: embedded Servo shutdown failed: {error:#}");
    }
}

/// Show that the single Servo owner thread is processing a request.
///
/// Commands are intentionally serialized: a screenshot, DOM query, or safe
/// engine shutdown must finish on that thread before the next terminal event
/// can be handled. Keeping this status outside the page image makes a delayed
/// `q`/Ctrl+Q visibly different from a frozen terminal.
fn working_status(operation: &str) -> String {
    format!("[working] {operation}")
}

fn show_working(
    terminal: &mut Terminal,
    size: crate::render::RenderSize,
    operation: &str,
) -> Result<()> {
    let status = working_status(operation);
    terminal.status(
        size.rows,
        &truncate_to_columns(&status, usize::from(size.columns)),
    )?;
    terminal.flush()
}

/// The alternate-screen transition can briefly expose the previous PTY
/// geometry through a multiplexer.  Launching Servo with that stale 80x24-ish
/// size makes its first Kitty placement fill only part of the actual pane, so
/// the first page appears oversized until any later input causes a resize.
///
/// Wait for two identical samples before creating Servo.  This is deliberately
/// a small, bounded startup delay; normal terminals settle after the first
/// sample, while fpasoterm/Herdr gets enough time to apply its pane geometry.
async fn settled_initial_terminal_size(terminal: &Terminal) -> Result<crate::render::RenderSize> {
    const SAMPLE_INTERVAL: Duration = Duration::from_millis(45);
    const STABLE_SAMPLES: u8 = 2;
    const MAX_SAMPLES: u8 = 6;

    let mut size = terminal.size()?;
    let mut unchanged = 0_u8;
    for _ in 0..MAX_SAMPLES {
        sleep(SAMPLE_INTERVAL).await;
        let candidate = terminal.size()?;
        if candidate == size {
            unchanged += 1;
            if unchanged >= STABLE_SAMPLES {
                break;
            }
        } else {
            size = candidate;
            unchanged = 0;
        }
    }
    Ok(size)
}

fn wait_for_run_error(
    error: &anyhow::Error,
    backend_choice: BackendChoice,
    sixel_cell_pixels: Option<TerminalCellPixels>,
    ui_chrome: UiChrome,
) -> Result<bool> {
    let mut terminal = Terminal::enter(ui_chrome).context("enter recovery terminal UI")?;
    let renderer =
        SelectedBackend::from_choice_with_sixel_cell_pixels(backend_choice, sixel_cell_pixels);
    let size = terminal.size()?;
    let message = format!("browser error: {error:#}; press r to retry or q to quit");
    draw_disconnected(&mut terminal, size, &message)?;
    loop {
        match input::read_command(Duration::from_millis(100), InputMode::Normal)? {
            Command::Reload => {
                renderer.cleanup(terminal.writer())?;
                terminal.flush()?;
                return Ok(true);
            }
            Command::Quit => {
                renderer.cleanup(terminal.writer())?;
                terminal.flush()?;
                return Ok(false);
            }
            Command::Help => {
                terminal.status(
                    size.rows,
                    "browser error recovery: r retries the embedded engine, q quits",
                )?;
                terminal.flush()?;
            }
            Command::None => {}
            _ => {}
        }
    }
}

async fn run_once(url: Url, backend_choice: BackendChoice, options: AppOptions) -> Result<()> {
    if options.capture_only {
        return capture_only(url, options).await;
    }

    let mut terminal = Terminal::enter(options.ui_chrome).context("enter terminal UI")?;
    let renderer = SelectedBackend::from_choice_with_sixel_cell_pixels(
        backend_choice,
        options.sixel_cell_pixels,
    );
    let frame_transport_is_expensive = renderer.frame_transport_is_expensive();
    let input_redraw_delay = if frame_transport_is_expensive {
        SIXEL_INPUT_REDRAW_DELAY
    } else {
        INPUT_REDRAW_DELAY
    };
    let action_redraw_delay = if frame_transport_is_expensive {
        SIXEL_ACTION_REDRAW_DELAY
    } else {
        ACTION_REDRAW_DELAY
    };
    let loading_redraw_interval = loading_redraw_delay(frame_transport_is_expensive);
    let action_redraw_burst = if frame_transport_is_expensive { 0 } else { 1 };
    // Keep command handlers independent of a particular image protocol. A
    // normal image transport gets one follow-up sample for late navigation;
    // SIXEL gets none because a full frame is expensive. Three samples made
    // the fpasoterm Kitty path visibly flicker after ordinary page actions.
    let schedule_redraw_burst = |pending_redraw_at: &mut Option<Instant>,
                                 redraw_burst_remaining: &mut u8| {
        schedule_redraw_burst_after(
            pending_redraw_at,
            redraw_burst_remaining,
            action_redraw_burst,
            action_redraw_delay,
        );
    };
    let schedule_input_redraw = |pending_redraw_at: &mut Option<Instant>| {
        schedule_input_redraw_after(pending_redraw_at, input_redraw_delay);
    };
    let mut state = PageState::new(url);
    let mut size = settled_initial_terminal_size(&terminal).await?;
    terminal.status(size.rows, "fpasobrowser: starting embedded Servo...")?;
    terminal.flush()?;
    let mut browser_options = options.browser.clone();
    browser_options.terminal_cell_pixels = renderer.terminal_cell_pixels();
    if browser_options.debug {
        eprintln!("fpasobrowser debug: terminal UI entered; launching browser");
    }
    let mut browser = timeout(
        BROWSER_START_TIMEOUT,
        BrowserClient::launch(size, browser_options.clone(), state.url.clone()),
    )
    .await
    .context("embedded Servo launch timed out")??;
    if browser_options.debug {
        eprintln!(
            "fpasobrowser debug: browser client ready at {state_url}",
            state_url = state.url
        );
    }
    if browser_options.debug {
        eprintln!("fpasobrowser debug: initial navigation completed; entering redraw loop");
    }
    let mut needs_redraw = true;
    let mut pending_redraw_at: Option<Instant> = None;
    let mut redraw_burst_remaining = 0_u8;
    let mut input_mode = InputMode::Normal;
    let mut suppress_initial_input_key_until: Option<Instant> = None;
    let mut show_help = false;
    let mut browser_connected = true;
    let mut presentation: Option<PresentationFingerprint> = None;
    let mut last_hover_at: Option<Instant> = None;
    let mut last_hover_status: Option<String> = None;
    // Herdr and other multiplexers normally forward Down/Drag/Up as a single
    // sequence. In a transition into mouse-reporting mode the first visible
    // event can instead be Drag or Up. Track the sequence locally so the
    // latter becomes one safe fallback activation, never a duplicate click.
    let mut primary_mouse_down = false;
    let mut message = String::from(DEFAULT_ACTIVITY_MESSAGE);
    let mut loading_frame = 0_u8;

    loop {
        if pending_redraw_at.is_some_and(|deadline| Instant::now() >= deadline) {
            pending_redraw_at = None;
            needs_redraw = true;
        }

        let latest_size = terminal.size()?;
        if latest_size != size {
            size = latest_size;
            presentation = None;
            if browser_connected {
                match timeout(Duration::from_secs(2), browser.resize(size)).await {
                    Ok(Ok(())) => {
                        message = String::from("terminal resized");
                    }
                    Ok(Err(error)) => {
                        browser_connected = false;
                        message = format!(
                            "browser resize failed: {error}; press r to relaunch, q to quit"
                        );
                    }
                    Err(_) => {
                        browser_connected = false;
                        message = String::from(
                            "browser resize timed out; press r to relaunch, q to quit",
                        );
                    }
                }
            }
            needs_redraw = true;
        }

        if needs_redraw {
            let redraw_again = false;
            if browser_connected {
                show_working(&mut terminal, size, "rendering page")?;
                let redraw_result = timeout(
                    REDRAW_TIMEOUT,
                    redraw(
                        &mut terminal,
                        &renderer,
                        &browser,
                        size,
                        &mut state,
                        &mut presentation,
                        RedrawOptions {
                            input_mode,
                            message: &message,
                            show_help,
                            loading_frame,
                            dump_screenshot: options.dump_screenshot.as_deref(),
                        },
                    ),
                )
                .await;
                match redraw_result {
                    Ok(Ok(outcome)) => {
                        if outcome.is_loading {
                            loading_frame = loading_frame.wrapping_add(1);
                            pending_redraw_at = Some(Instant::now() + loading_redraw_interval);
                        } else {
                            loading_frame = 0;
                        }
                    }
                    Ok(Err(error)) => {
                        browser_connected = false;
                        message = format!(
                            "browser disconnected: {error:#}; press r to relaunch, q to quit"
                        );
                        draw_disconnected(&mut terminal, size, &message)?;
                    }
                    Err(_) => {
                        browser_connected = false;
                        message = String::from(
                            "browser redraw timed out; press r to relaunch, q to quit",
                        );
                        draw_disconnected(&mut terminal, size, &message)?;
                    }
                }
            } else {
                presentation = None;
                draw_disconnected(&mut terminal, size, &message)?;
            }
            // A resumed engine must immediately render the fresh page. The
            // normal redraw path clears this flag, but a recovery requests
            // another pass so an old disconnected frame cannot remain.
            needs_redraw = redraw_again;
            if redraw_burst_remaining > 0 {
                redraw_burst_remaining -= 1;
                pending_redraw_at = Some(Instant::now() + ACTION_REDRAW_INTERVAL);
            }
        }

        let input_timeout = pending_redraw_at
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            .map(|duration| duration.min(Duration::from_millis(100)))
            .unwrap_or(Duration::from_millis(100));

        let command = match input::read_command(input_timeout, input_mode) {
            Ok(command) => command,
            Err(error) => {
                // A terminal event error must not unwind the application after
                // an external browser handoff. Keep the TUI alive and provide
                // the same recovery controls as a browser disconnect.
                browser_connected = false;
                input_mode = InputMode::Normal;
                message = format!(
                    "terminal input temporarily unavailable: {error}; press r to relaunch, q to quit"
                );
                draw_disconnected(&mut terminal, size, &message)?;
                continue;
            }
        };
        let command = normalize_primary_mouse_command(command, &mut primary_mouse_down);
        if !browser_connected {
            presentation = None;
            match command {
                Command::Quit => {
                    show_working(&mut terminal, size, "closing embedded browser safely")?;
                    renderer.cleanup(terminal.writer())?;
                    terminal.flush()?;
                    shutdown_browser_on_exit(&mut terminal, size, browser).await;
                    break;
                }
                Command::Reload => {
                    match relaunch_browser_with_timeout(
                        &mut browser,
                        &browser_options,
                        size,
                        &state,
                    )
                    .await
                    {
                        Ok(_) => {
                            browser_connected = true;
                            message = String::from("embedded browser relaunched");
                            needs_redraw = true;
                        }
                        Err(error) => {
                            message =
                                format!("relaunch failed: {error}; press r to retry, q to quit");
                            draw_disconnected(&mut terminal, size, &message)?;
                        }
                    }
                }
                Command::Help => {
                    show_help = !show_help;
                    message = if show_help {
                        String::from("browser disconnected; r relaunch | q quit | ? hide help")
                    } else {
                        String::from("browser disconnected; press r to relaunch, q to quit")
                    };
                    draw_disconnected(&mut terminal, size, &message)?;
                }
                Command::OpenExternal | Command::OpenVerification => {
                    match open_in_regular_browser(state.url.as_str()) {
                        Ok(()) => {
                            message = String::from(
                                "opened current URL in regular browser; press r to relaunch",
                            );
                        }
                        Err(error) => {
                            message = format!("open regular browser failed: {error}");
                        }
                    }
                    draw_disconnected(&mut terminal, size, &message)?;
                }
                Command::Click { column, row }
                    if terminal.has_toolbar() && row == toolbar_row(size) =>
                {
                    match toolbar_command(size.columns, column) {
                        Some(Command::Reload) => {
                            match relaunch_browser_with_timeout(
                                &mut browser,
                                &browser_options,
                                size,
                                &state,
                            )
                            .await
                            {
                                Ok(()) => {
                                    browser_connected = true;
                                    message = String::from("embedded browser relaunched");
                                    needs_redraw = true;
                                }
                                Err(error) => {
                                    message = format!(
                                        "relaunch failed: {error}; press r to retry, q to quit"
                                    );
                                    draw_disconnected(&mut terminal, size, &message)?;
                                }
                            }
                        }
                        Some(Command::Help) => {
                            show_help = !show_help;
                            message = String::from("browser disconnected; press r to relaunch");
                            draw_disconnected(&mut terminal, size, &message)?;
                        }
                        Some(Command::OpenVerification) => {
                            match open_in_regular_browser(state.url.as_str()) {
                                Ok(()) => {
                                    message = String::from(
                                        "opened current URL in regular browser; press r to relaunch",
                                    );
                                }
                                Err(error) => {
                                    message = format!("open regular browser failed: {error}");
                                }
                            }
                            draw_disconnected(&mut terminal, size, &message)?;
                        }
                        _ => {
                            message = String::from("browser disconnected; press r to relaunch");
                            draw_disconnected(&mut terminal, size, &message)?;
                        }
                    }
                }
                Command::None => {}
                _ => {
                    message = String::from("browser disconnected; press r to relaunch, q to quit");
                    draw_disconnected(&mut terminal, size, &message)?;
                }
            }
            continue;
        }

        match command {
            Command::Quit => {
                show_working(&mut terminal, size, "closing embedded browser safely")?;
                renderer.cleanup(terminal.writer())?;
                terminal.flush()?;
                shutdown_browser_on_exit(&mut terminal, size, browser).await;
                break;
            }
            Command::Reload => {
                show_working(&mut terminal, size, "reloading page")?;
                match browser.reload().await {
                    Ok(()) => {
                        show_help = false;
                        message = String::from("reloaded");
                        schedule_redraw_burst(&mut pending_redraw_at, &mut redraw_burst_remaining);
                    }
                    Err(error) => {
                        let resumed = relaunch_browser_with_timeout(
                            &mut browser,
                            &browser_options,
                            size,
                            &state,
                        )
                        .await
                        .is_ok();
                        if resumed {
                            message = String::from("embedded browser relaunched; page refreshed");
                            schedule_redraw_burst(
                                &mut pending_redraw_at,
                                &mut redraw_burst_remaining,
                            );
                        } else {
                            message = format!("reload failed: {error}");
                            terminal.status(size.rows, &message)?;
                            terminal.flush()?;
                        }
                    }
                }
            }
            Command::Back => {
                show_working(&mut terminal, size, "going back")?;
                match browser.go_back().await {
                    Ok(()) => {
                        show_help = false;
                        message = String::from("went back");
                        schedule_redraw_burst(&mut pending_redraw_at, &mut redraw_burst_remaining);
                    }
                    Err(error) => {
                        message = format!("back failed: {error}");
                        terminal.status(size.rows, &message)?;
                        terminal.flush()?;
                    }
                }
            }
            Command::Forward => {
                show_working(&mut terminal, size, "going forward")?;
                match browser.go_forward().await {
                    Ok(()) => {
                        show_help = false;
                        message = String::from("went forward");
                        schedule_redraw_burst(&mut pending_redraw_at, &mut redraw_burst_remaining);
                    }
                    Err(error) => {
                        message = format!("forward failed: {error}");
                        terminal.status(size.rows, &message)?;
                        terminal.flush()?;
                    }
                }
            }
            Command::NewTab => {
                if let Some(url) = prompt_url(&mut terminal, size, state.url.as_str())? {
                    show_help = false;
                    show_working(&mut terminal, size, "opening tab")?;
                    match browser.new_tab(url).await {
                        Ok(_) => {
                            message = String::from("opened new tab");
                            schedule_redraw_burst(
                                &mut pending_redraw_at,
                                &mut redraw_burst_remaining,
                            );
                        }
                        Err(error) => {
                            message = format!("new tab failed: {error}");
                            terminal.status(size.rows, &message)?;
                            terminal.flush()?;
                        }
                    }
                }
            }
            Command::CloseTab => {
                show_working(&mut terminal, size, "closing tab")?;
                match browser.close_tab().await {
                    Ok(CloseTabOutcome::Closed { active_index }) => {
                        input_mode = InputMode::Normal;
                        message = format!("closed tab; active tab {}", active_index + 1);
                        needs_redraw = true;
                    }
                    Ok(CloseTabOutcome::KeptLast { active_index }) => {
                        message = format!("kept last tab open; active tab {}", active_index + 1);
                        needs_redraw = true;
                    }
                    Err(error) => {
                        message = format!("close tab failed: {error}");
                        terminal.status(size.rows, &message)?;
                        terminal.flush()?;
                    }
                }
            }
            Command::NextTab => {
                show_working(&mut terminal, size, "switching tab")?;
                match browser.next_tab().await {
                    Ok(index) => {
                        show_help = false;
                        message = format!("switched to tab {}", index + 1);
                        needs_redraw = true;
                    }
                    Err(error) => {
                        message = format!("next tab failed: {error}");
                        terminal.status(size.rows, &message)?;
                        terminal.flush()?;
                    }
                }
            }
            Command::PreviousTab => {
                show_working(&mut terminal, size, "switching tab")?;
                match browser.previous_tab().await {
                    Ok(index) => {
                        show_help = false;
                        message = format!("switched to tab {}", index + 1);
                        needs_redraw = true;
                    }
                    Err(error) => {
                        message = format!("previous tab failed: {error}");
                        terminal.status(size.rows, &message)?;
                        terminal.flush()?;
                    }
                }
            }
            Command::Help => {
                show_help = !show_help;
                message = if show_help {
                    String::from("help shown; click toolbar buttons or press ? to hide")
                } else {
                    String::from("help hidden")
                };
                needs_redraw = true;
            }
            Command::InputMode => {
                show_help = false;
                show_working(&mut terminal, size, "focusing editable field")?;
                let focus_result =
                    timeout(Duration::from_millis(1_200), browser.focus_first_editable()).await;
                match focus_result {
                    Ok(Ok(true)) => {
                        input_mode = InputMode::BrowserInput;
                        suppress_initial_input_key_until =
                            Some(Instant::now() + INPUT_MODE_KEY_SUPPRESS_WINDOW);
                        message = String::from(
                            "input mode: focused editable; type text, Esc normal, Ctrl+Q quit",
                        );
                    }
                    Ok(Ok(false)) => {
                        input_mode = InputMode::BrowserInput;
                        suppress_initial_input_key_until =
                            Some(Instant::now() + INPUT_MODE_KEY_SUPPRESS_WINDOW);
                        message = String::from(
                            "input mode: no editable found; Tab/click then type, Esc normal",
                        );
                    }
                    Ok(Err(error)) => {
                        // A page-side focus query can fail independently of
                        // Servo's command channel (notably during an iframe
                        // focus transition). Keep the live browser and its
                        // keyboard navigation available instead of replacing
                        // the page with the disconnected recovery screen.
                        message = format!(
                            "input mode: focus unavailable ({error}); Tab/click then type, Esc normal"
                        );
                        input_mode = InputMode::Normal;
                        suppress_initial_input_key_until = None;
                    }
                    Err(_) => {
                        message = String::from(
                            "input mode: focus timed out; press r to relaunch, q to quit",
                        );
                        input_mode = InputMode::Normal;
                        suppress_initial_input_key_until = None;
                        browser_connected = false;
                        needs_redraw = true;
                    }
                }
                terminal.status(size.rows, &message)?;
                terminal.flush()?;
                if browser_connected {
                    schedule_input_redraw(&mut pending_redraw_at);
                }
            }
            Command::Escape => {
                // Leaving input mode must also release DOM focus. Otherwise a
                // form can keep receiving characters after the status line
                // says "normal mode", which is especially confusing after a
                // scroll or toolbar action.
                if input_mode == InputMode::BrowserInput {
                    show_working(&mut terminal, size, "leaving input mode")?;
                    let _ =
                        timeout(Duration::from_millis(1_200), browser.blur_active_element()).await;
                    input_mode = InputMode::Normal;
                    suppress_initial_input_key_until = None;
                    message = String::from("normal mode");
                    needs_redraw = true;
                } else {
                    // A popup is displayed as an in-viewer terminal tab, not
                    // as an OS window. Esc is a non-destructive escape route:
                    // return to the preceding tab when one exists, while `x`
                    // remains the only operation that closes a tab.
                    show_working(&mut terminal, size, "checking tabs")?;
                    match browser.tab_info().await {
                        Ok(tab_info) if tab_info.active_index > 0 => {
                            match browser.select_tab(tab_info.active_index - 1).await {
                                Ok(index) => {
                                    show_help = false;
                                    message = format!("returned to tab {}", index + 1);
                                    needs_redraw = true;
                                }
                                Err(error) => {
                                    message = format!("return to previous tab failed: {error}");
                                    terminal.status(size.rows, &message)?;
                                    terminal.flush()?;
                                }
                            }
                        }
                        Ok(_) => {
                            show_help = false;
                            message = String::from("normal mode");
                            needs_redraw = true;
                        }
                        Err(error) => {
                            message = format!("read tabs for Esc failed: {error}");
                            terminal.status(size.rows, &message)?;
                            terminal.flush()?;
                        }
                    }
                }
            }
            Command::GoToUrl => {
                if let Some(url) = prompt_url(&mut terminal, size, state.url.as_str())? {
                    show_help = false;
                    show_working(&mut terminal, size, "navigating to URL")?;
                    match browser.navigate(url.clone()).await {
                        Ok(()) => {
                            state = PageState::new(url);
                            message = String::from("navigated");
                            schedule_redraw_burst(
                                &mut pending_redraw_at,
                                &mut redraw_burst_remaining,
                            );
                        }
                        Err(error) => {
                            message = format!("navigate failed: {error}");
                            terminal.status(size.rows, &message)?;
                            terminal.flush()?;
                        }
                    }
                }
            }
            Command::LinkHints => {
                show_working(&mut terminal, size, "finding links")?;
                match browser.show_link_hints().await {
                    Ok(count) => {
                        let hint_message = format!("{count} link hints; type number then Enter");
                        match redraw(
                            &mut terminal,
                            &renderer,
                            &browser,
                            size,
                            &mut state,
                            &mut presentation,
                            RedrawOptions {
                                input_mode,
                                message: &hint_message,
                                show_help,
                                loading_frame,
                                dump_screenshot: options.dump_screenshot.as_deref(),
                            },
                        )
                        .await
                        {
                            Ok(_) => {
                                if let Some(hint) =
                                    prompt_number(&mut terminal, size, "hint number")?
                                {
                                    show_working(&mut terminal, size, "opening selected link")?;
                                    match browser.activate_link_hint(hint).await {
                                        Ok(outcome) => {
                                            message = format_link_hint_activation(hint, outcome);
                                            schedule_redraw_burst(
                                                &mut pending_redraw_at,
                                                &mut redraw_burst_remaining,
                                            );
                                        }
                                        Err(error) => {
                                            message = format!("activate hint failed: {error}");
                                            browser_connected = false;
                                            needs_redraw = true;
                                        }
                                    }
                                }
                            }
                            Err(error) => {
                                message = format!("link hint redraw failed: {error}");
                                browser_connected = false;
                                needs_redraw = true;
                            }
                        }
                    }
                    Err(error) => {
                        message = format!("link hints failed: {error}");
                        browser_connected = false;
                        needs_redraw = true;
                    }
                }
            }
            Command::ScrollDown => {
                state.scroll_down_by(KEY_SCROLL_PIXELS);
                // Buttons as well as editable controls can retain DOM focus
                // after a click.  Clear it for every scroll command so a
                // focus outline is not left behind on a scrolled-off button.
                show_working(&mut terminal, size, "scrolling page")?;
                if input_mode == InputMode::BrowserInput {
                    let _ = timeout(FOCUS_BLUR_TIMEOUT, browser.blur_active_element()).await;
                }
                input_mode = InputMode::Normal;
                suppress_initial_input_key_until = None;
                match browser.scroll_by(KEY_SCROLL_PIXELS).await {
                    Ok(()) => {
                        schedule_scroll_redraw(&mut pending_redraw_at, &mut redraw_burst_remaining)
                    }
                    Err(error) => {
                        message = format!("scroll failed: {error}");
                        terminal.status(size.rows, &message)?;
                        terminal.flush()?;
                    }
                }
            }
            Command::ScrollUp => {
                state.scroll_up_by(KEY_SCROLL_PIXELS);
                show_working(&mut terminal, size, "scrolling page")?;
                if input_mode == InputMode::BrowserInput {
                    let _ = timeout(FOCUS_BLUR_TIMEOUT, browser.blur_active_element()).await;
                }
                input_mode = InputMode::Normal;
                suppress_initial_input_key_until = None;
                match browser.scroll_by(-KEY_SCROLL_PIXELS).await {
                    Ok(()) => {
                        schedule_scroll_redraw(&mut pending_redraw_at, &mut redraw_burst_remaining)
                    }
                    Err(error) => {
                        message = format!("scroll failed: {error}");
                        terminal.status(size.rows, &message)?;
                        terminal.flush()?;
                    }
                }
            }
            Command::WheelDown => {
                state.scroll_down_by(WHEEL_SCROLL_PIXELS);
                show_working(&mut terminal, size, "scrolling page")?;
                if input_mode == InputMode::BrowserInput {
                    let _ = timeout(FOCUS_BLUR_TIMEOUT, browser.blur_active_element()).await;
                }
                input_mode = InputMode::Normal;
                suppress_initial_input_key_until = None;
                match browser.scroll_by(WHEEL_SCROLL_PIXELS).await {
                    Ok(()) => {
                        schedule_scroll_redraw(&mut pending_redraw_at, &mut redraw_burst_remaining)
                    }
                    Err(error) => {
                        message = format!("scroll failed: {error}");
                        terminal.status(size.rows, &message)?;
                        terminal.flush()?;
                    }
                }
            }
            Command::WheelUp => {
                state.scroll_up_by(WHEEL_SCROLL_PIXELS);
                show_working(&mut terminal, size, "scrolling page")?;
                if input_mode == InputMode::BrowserInput {
                    let _ = timeout(FOCUS_BLUR_TIMEOUT, browser.blur_active_element()).await;
                }
                input_mode = InputMode::Normal;
                suppress_initial_input_key_until = None;
                match browser.scroll_by(-WHEEL_SCROLL_PIXELS).await {
                    Ok(()) => {
                        schedule_scroll_redraw(&mut pending_redraw_at, &mut redraw_burst_remaining)
                    }
                    Err(error) => {
                        message = format!("scroll failed: {error}");
                        terminal.status(size.rows, &message)?;
                        terminal.flush()?;
                    }
                }
            }
            Command::OpenExternal => {
                show_help = false;
                show_working(&mut terminal, size, "opening regular browser")?;
                let current_url = browser
                    .current_url()
                    .await
                    .unwrap_or_else(|_| state.url.to_string());
                match open_in_regular_browser(&current_url) {
                    Ok(()) => {
                        message = format!("opened externally: {current_url}");
                    }
                    Err(error) => {
                        message = format!("open external failed: {error}");
                    }
                }
                needs_redraw = true;
            }
            Command::OpenVerification => {
                show_help = false;
                show_working(
                    &mut terminal,
                    size,
                    "opening verification in regular browser",
                )?;
                let current_url = browser
                    .current_url()
                    .await
                    .unwrap_or_else(|_| state.url.to_string());
                match open_in_regular_browser(&current_url) {
                    Ok(()) => {
                        message = format!(
                            "verification opened in regular browser; complete it there, then press r: {current_url}"
                        );
                    }
                    Err(error) => {
                        message = format!("open verification failed: {error}");
                    }
                }
                terminal.status(size.rows, &message)?;
                terminal.flush()?;
                needs_redraw = true;
            }
            Command::BrowserKey(key) => {
                show_working(&mut terminal, size, &format!("sending key {key}"))?;
                let key_result = timeout(
                    Duration::from_millis(1_200),
                    browser.press_key_with_fallback(key),
                )
                .await;
                match key_result {
                    Ok(Ok(())) => {
                        suppress_initial_input_key_until = None;
                        message = format!("sent key {key}");
                        terminal.status(size.rows, &message)?;
                        terminal.flush()?;
                        if key == "Enter" {
                            schedule_redraw_burst(
                                &mut pending_redraw_at,
                                &mut redraw_burst_remaining,
                            );
                        } else {
                            schedule_input_redraw(&mut pending_redraw_at);
                        }
                    }
                    Ok(Err(error)) => {
                        message = format!("key failed: {error}");
                        browser_connected = false;
                        input_mode = InputMode::Normal;
                        terminal.status(size.rows, &message)?;
                        terminal.flush()?;
                        needs_redraw = true;
                    }
                    Err(_) => {
                        message = format!("key {key} timed out; press r to relaunch, q to quit");
                        browser_connected = false;
                        input_mode = InputMode::Normal;
                        terminal.status(size.rows, &message)?;
                        terminal.flush()?;
                        needs_redraw = true;
                    }
                }
            }
            Command::BrowserText(c) => {
                if c == 'r'
                    && timeout(
                        Duration::from_millis(300),
                        browser.manual_verification_required(),
                    )
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .unwrap_or(false)
                {
                    input_mode = InputMode::Normal;
                    show_working(&mut terminal, size, "reloading verification page")?;
                    match timeout(Duration::from_millis(1_200), browser.reload()).await {
                        Ok(Ok(())) => {
                            message = String::from("verification reload requested");
                            schedule_redraw_burst(
                                &mut pending_redraw_at,
                                &mut redraw_burst_remaining,
                            );
                        }
                        Ok(Err(error)) => {
                            message = format!("verification reload failed: {error}; press r");
                            needs_redraw = true;
                        }
                        Err(_) => {
                            message = String::from("verification reload timed out; press r");
                            needs_redraw = true;
                        }
                    }
                    terminal.status(size.rows, &message)?;
                    terminal.flush()?;
                    continue;
                }
                if c == 'i'
                    && suppress_initial_input_key_until
                        .is_some_and(|deadline| Instant::now() <= deadline)
                {
                    suppress_initial_input_key_until = None;
                    message = String::from("input mode: focused editable; type text, Esc normal");
                    terminal.status(size.rows, &message)?;
                    terminal.flush()?;
                    schedule_input_redraw(&mut pending_redraw_at);
                    continue;
                }
                suppress_initial_input_key_until = None;
                show_working(&mut terminal, size, "entering text")?;
                let text_result = timeout(
                    Duration::from_millis(1_200),
                    browser.type_text(c.to_string()),
                )
                .await;
                match text_result {
                    Ok(Ok(())) => {
                        message = format!("typed {c:?}; r redraw, Enter submit, Ctrl+Q quit");
                    }
                    Ok(Err(error)) => {
                        if c == '?' {
                            // Help must remain reachable even when input mode
                            // was left active after a page navigation and no
                            // editable field is currently focused.
                            input_mode = InputMode::Normal;
                            show_help = true;
                            message =
                                String::from("help shown; press ? to hide, Esc for normal mode");
                            needs_redraw = true;
                            terminal.status(size.rows, &message)?;
                            terminal.flush()?;
                            continue;
                        }
                        if c == 'q' || c == 'Q' {
                            // If input mode has no live editable target, a quit
                            // key must remain an emergency escape hatch. A
                            // focused field still receives q normally because
                            // this branch is reached only after typing failed.
                            show_working(&mut terminal, size, "closing embedded browser safely")?;
                            renderer.cleanup(terminal.writer())?;
                            terminal.flush()?;
                            shutdown_browser_on_exit(&mut terminal, size, browser).await;
                            break;
                        }
                        if c == 'r' || c == 'R' {
                            input_mode = InputMode::Normal;
                            show_working(&mut terminal, size, "reloading page")?;
                            match timeout(Duration::from_millis(1_200), browser.reload()).await {
                                Ok(Ok(())) => {
                                    message = String::from("reloaded");
                                    schedule_redraw_burst(
                                        &mut pending_redraw_at,
                                        &mut redraw_burst_remaining,
                                    );
                                    terminal.status(size.rows, &message)?;
                                    terminal.flush()?;
                                    continue;
                                }
                                Ok(Err(reload_error)) => {
                                    message = format!("reload failed: {reload_error}");
                                }
                                Err(_) => {
                                    message = String::from(
                                        "reload timed out; press q to quit or r to retry",
                                    );
                                }
                            }
                            terminal.status(size.rows, &message)?;
                            terminal.flush()?;
                            continue;
                        }
                        message = format!("text failed: {error}");
                        browser_connected = false;
                        input_mode = InputMode::Normal;
                    }
                    Err(_) => {
                        message = format!("text {c:?} timed out; press r to relaunch, q to quit");
                        browser_connected = false;
                        input_mode = InputMode::Normal;
                    }
                }
                terminal.status(size.rows, &message)?;
                terminal.flush()?;
                if browser_connected {
                    schedule_input_redraw(&mut pending_redraw_at);
                } else {
                    needs_redraw = true;
                }
            }
            Command::Click { column, row } => {
                last_hover_status = None;
                if terminal.has_toolbar() && row == toolbar_row(size) {
                    show_working(&mut terminal, size, "handling toolbar action")?;
                    if input_mode == InputMode::BrowserInput {
                        let _ =
                            timeout(Duration::from_millis(1_200), browser.blur_active_element())
                                .await;
                        input_mode = InputMode::Normal;
                        suppress_initial_input_key_until = None;
                    }
                    if let Some(command) = toolbar_command(size.columns, column) {
                        match command {
                            Command::Back => match browser.go_back().await {
                                Ok(()) => {
                                    message = String::from("went back");
                                    schedule_redraw_burst(
                                        &mut pending_redraw_at,
                                        &mut redraw_burst_remaining,
                                    );
                                }
                                Err(error) => message = format!("back failed: {error}"),
                            },
                            Command::Forward => match browser.go_forward().await {
                                Ok(()) => {
                                    message = String::from("went forward");
                                    schedule_redraw_burst(
                                        &mut pending_redraw_at,
                                        &mut redraw_burst_remaining,
                                    );
                                }
                                Err(error) => message = format!("forward failed: {error}"),
                            },
                            Command::Reload => match browser.reload().await {
                                Ok(()) => {
                                    message = String::from("reloaded");
                                    schedule_redraw_burst(
                                        &mut pending_redraw_at,
                                        &mut redraw_burst_remaining,
                                    );
                                }
                                Err(error) => message = format!("reload failed: {error}"),
                            },
                            Command::GoToUrl => {
                                if let Some(url) =
                                    prompt_url(&mut terminal, size, state.url.as_str())?
                                {
                                    match browser.navigate(url.clone()).await {
                                        Ok(()) => {
                                            state = PageState::new(url);
                                            message = String::from("navigated");
                                            schedule_redraw_burst(
                                                &mut pending_redraw_at,
                                                &mut redraw_burst_remaining,
                                            );
                                        }
                                        Err(error) => {
                                            message = format!("navigate failed: {error}");
                                        }
                                    }
                                }
                            }
                            Command::NewTab => {
                                if let Some(url) =
                                    prompt_url(&mut terminal, size, state.url.as_str())?
                                {
                                    match browser.new_tab(url).await {
                                        Ok(_) => {
                                            message = String::from("opened new tab");
                                            schedule_redraw_burst(
                                                &mut pending_redraw_at,
                                                &mut redraw_burst_remaining,
                                            );
                                        }
                                        Err(error) => message = format!("new tab failed: {error}"),
                                    }
                                }
                            }
                            Command::PreviousTab => match browser.previous_tab().await {
                                Ok(index) => {
                                    message = format!("switched to tab {}", index + 1);
                                    needs_redraw = true;
                                }
                                Err(error) => message = format!("previous tab failed: {error}"),
                            },
                            Command::NextTab => match browser.next_tab().await {
                                Ok(index) => {
                                    message = format!("switched to tab {}", index + 1);
                                    needs_redraw = true;
                                }
                                Err(error) => message = format!("next tab failed: {error}"),
                            },
                            Command::CloseTab => match browser.close_tab().await {
                                Ok(CloseTabOutcome::Closed { active_index }) => {
                                    input_mode = InputMode::Normal;
                                    message =
                                        format!("closed tab; active tab {}", active_index + 1);
                                    needs_redraw = true;
                                }
                                Ok(CloseTabOutcome::KeptLast { active_index }) => {
                                    message = format!(
                                        "kept last tab open; active tab {}",
                                        active_index + 1
                                    );
                                    needs_redraw = true;
                                }
                                Err(error) => message = format!("close tab failed: {error}"),
                            },
                            Command::Help => {
                                show_help = !show_help;
                                message = if show_help {
                                    String::from("help shown; click ? to hide")
                                } else {
                                    String::from("help hidden")
                                };
                                needs_redraw = true;
                            }
                            Command::OpenVerification => {
                                let current_url = browser
                                    .current_url()
                                    .await
                                    .unwrap_or_else(|_| state.url.to_string());
                                match open_in_regular_browser(&current_url) {
                                    Ok(()) => {
                                        message = String::from(
                                            "verification opened in regular browser; complete it, then press r",
                                        );
                                    }
                                    Err(error) => {
                                        message = format!("open verification failed: {error}");
                                    }
                                }
                                needs_redraw = true;
                            }
                            _ => {}
                        }
                        terminal.status(size.rows, &message)?;
                        terminal.flush()?;
                    }
                } else if terminal.has_tab_strip() && row == tab_strip_row(size) {
                    show_working(&mut terminal, size, "handling tab action")?;
                    if input_mode == InputMode::BrowserInput {
                        let _ =
                            timeout(Duration::from_millis(1_200), browser.blur_active_element())
                                .await;
                        input_mode = InputMode::Normal;
                        suppress_initial_input_key_until = None;
                    }
                    match browser.tab_info().await {
                        Ok(tab_info) => match tab_strip_target(size.columns, column, &tab_info) {
                            Some(TabStripAction::Select(index))
                                if index == tab_info.active_index =>
                            {
                                message = format!("tab {} is already active", index + 1);
                            }
                            Some(TabStripAction::Select(index)) => {
                                match browser.select_tab(index).await {
                                    Ok(active_index) => {
                                        show_help = false;
                                        message = format!("switched to tab {}", active_index + 1);
                                        needs_redraw = true;
                                    }
                                    Err(error) => {
                                        message = format!("select tab failed: {error}");
                                    }
                                }
                            }
                            Some(TabStripAction::Close(index)) => {
                                let selected = if index == tab_info.active_index {
                                    Ok(index)
                                } else {
                                    browser.select_tab(index).await
                                };
                                match selected {
                                    Ok(_) => match browser.close_tab().await {
                                        Ok(CloseTabOutcome::Closed { active_index }) => {
                                            input_mode = InputMode::Normal;
                                            show_help = false;
                                            message = format!(
                                                "closed tab {}; active tab {}",
                                                index + 1,
                                                active_index + 1
                                            );
                                            needs_redraw = true;
                                        }
                                        Ok(CloseTabOutcome::KeptLast { active_index }) => {
                                            message = format!(
                                                "kept last tab open; active tab {}",
                                                active_index + 1
                                            );
                                            needs_redraw = true;
                                        }
                                        Err(error) => {
                                            message = format!("close tab failed: {error}");
                                        }
                                    },
                                    Err(error) => {
                                        message = format!("select tab to close failed: {error}");
                                    }
                                }
                            }
                            None => {
                                message = String::from(
                                    "click a tab label to switch, or × to close; { and } also switch tabs",
                                );
                            }
                        },
                        Err(error) => {
                            message = format!("read tabs failed: {error}");
                        }
                    }
                    terminal.status(size.rows, &message)?;
                    terminal.flush()?;
                } else if row < size.rows {
                    const CLICK_TIMEOUT: Duration = Duration::from_millis(1_200);
                    show_working(&mut terminal, size, "handling mouse click")?;
                    let click_result =
                        timeout(CLICK_TIMEOUT, browser.click_js(column, row, size)).await;
                    match click_result {
                        Err(_) => {
                            // A delayed Servo click can still finish on its
                            // owner thread (for example while creating an
                            // auxiliary WebView). Never launch the current
                            // URL automatically here: `file:` URLs may be
                            // associated with a terminal application, and
                            // the explicit o/v command remains available
                            // when a regular-browser handoff is desired.
                            message = String::from(
                                "click is still processing; wait briefly or press r to redraw (o/v opens a regular browser)",
                            );
                            input_mode = InputMode::Normal;
                            terminal.status(size.rows, &message)?;
                            terminal.flush()?;
                        }
                        Ok(click_result) => match click_result {
                            Ok(outcome) => {
                                let focus_detail = match (&outcome.target_tag, &outcome.active_tag)
                                {
                                    (Some(target), Some(active)) => {
                                        format!(" target={target} active={active}")
                                    }
                                    (Some(target), None) => format!(" target={target}"),
                                    (None, Some(active)) => format!(" active={active}"),
                                    (None, None) => String::new(),
                                };
                                let popup_detail = if outcome.opened_popup_tabs == 0 {
                                    String::new()
                                } else {
                                    format!(
                                        "; opened {} popup terminal tab(s)",
                                        outcome.opened_popup_tabs
                                    )
                                };
                                if outcome.editable {
                                    input_mode = InputMode::BrowserInput;
                                    let focus_state = if outcome.active_editable {
                                        " active editable focused"
                                    } else {
                                        ""
                                    };
                                    message = format!(
                                        "clicked editable field {column},{row}; input mode enabled{focus_state}{focus_detail}{popup_detail}"
                                    );
                                } else if outcome.iframe {
                                    // Servo can keep keyboard focus in the
                                    // child browsing context while the parent
                                    // document only exposes its iframe
                                    // element. Preserve browser input mode so
                                    // text, Tab and Shift+Tab reach the
                                    // clicked frame control; Esc returns to
                                    // normal viewer commands.
                                    input_mode = InputMode::BrowserInput;
                                    message = format!(
                                        "clicked iframe control {column},{row}; input mode enabled, Esc normal{focus_detail}{popup_detail}"
                                    );
                                } else if outcome.cdp_fallback {
                                    input_mode = InputMode::Normal;
                                    message = format!(
                                        "clicked {column},{row} via engine input{focus_detail}{popup_detail}"
                                    );
                                } else {
                                    input_mode = InputMode::Normal;
                                    message = format!(
                                        "clicked {column},{row}{focus_detail}{popup_detail}"
                                    );
                                }
                            }
                            Err(error) => {
                                let verification = timeout(
                                    Duration::from_millis(300),
                                    browser.manual_verification_required(),
                                )
                                .await
                                .ok()
                                .and_then(Result::ok)
                                .unwrap_or(false);
                                if verification {
                                    message = format!(
                                        "site verification click failed ({error}); use Tab/Enter or press o/v to open a regular browser"
                                    );
                                } else {
                                    message = format!("click failed: {error}");
                                }
                                // A click inside a cross-origin CAPTCHA iframe can be
                                // rejected by an engine input endpoint even though the
                                // browser is still healthy. Keep the session alive so
                                // the user can retry, use Tab/Enter, or press v/o.
                                input_mode = InputMode::Normal;
                                terminal.status(size.rows, &message)?;
                                terminal.flush()?;
                            }
                        },
                    }
                    schedule_redraw_burst(&mut pending_redraw_at, &mut redraw_burst_remaining);
                }
            }
            Command::MouseMove { column, row } => {
                if frame_transport_is_expensive {
                    // A SIXEL frame is a large DCS stream. Suppress cosmetic
                    // hover inspection while it is selected so a burst of
                    // queued mouse-move events cannot delay a real click.
                    continue;
                }
                if row >= size.rows
                    || last_hover_at.is_some_and(|last| last.elapsed() < HOVER_UPDATE_MIN_INTERVAL)
                {
                    continue;
                }
                last_hover_at = Some(Instant::now());
                match timeout(HOVER_TIMEOUT, browser.hover(column, row, size)).await {
                    Ok(Ok(outcome)) => {
                        let hover_status = format_hover_status(&outcome);
                        if last_hover_status.as_ref() != Some(&hover_status) {
                            last_hover_status = Some(hover_status.clone());
                            message = hover_status;
                            // Hover inspection is a text-only affordance.
                            // Updating the activity row directly avoids
                            // retransmitting a full Kitty frame for every
                            // pointer move through fpasoterm/Herdr.
                            terminal.status(size.rows, &message)?;
                            terminal.flush()?;
                        }
                    }
                    // Pointer movement is cosmetic. Keep a working WebView
                    // connected, but expose a one-line diagnostic rather
                    // than silently leaving an old status message on screen.
                    Ok(Err(error)) => {
                        let hover_status = format!("hover target inspection unavailable: {error}");
                        if last_hover_status.as_ref() != Some(&hover_status) {
                            last_hover_status = Some(hover_status.clone());
                            message = hover_status;
                            terminal.status(size.rows, &message)?;
                            terminal.flush()?;
                        }
                    }
                    Err(_) => {
                        let hover_status = String::from("hover target inspection timed out");
                        if last_hover_status.as_ref() != Some(&hover_status) {
                            last_hover_status = Some(hover_status.clone());
                            message = hover_status;
                            terminal.status(size.rows, &message)?;
                            terminal.flush()?;
                        }
                    }
                }
            }
            Command::MousePrimaryDrag { .. } | Command::MousePrimaryUp { .. } => {
                // `normalize_primary_mouse_command` converts an orphaned
                // Drag/Up to Click. Remaining events belong to an activation
                // that was already sent to Servo, so ignore them.
            }
            Command::None => {}
        }
    }

    Ok(())
}

/// Normalize a physical left-button gesture into at most one page activation.
///
/// Nested terminal clients may begin forwarding after the physical press, in
/// which case a Drag or Up is the first event received by fpasobrowser. Treat
/// that first observed event as a click. A normal Down/Drag/Up sequence still
/// emits exactly the original Down click.
fn normalize_primary_mouse_command(command: Command, primary_mouse_down: &mut bool) -> Command {
    match command {
        Command::Click { column, row } => {
            *primary_mouse_down = true;
            Command::Click { column, row }
        }
        Command::MousePrimaryDrag { column, row } => {
            if *primary_mouse_down {
                Command::MousePrimaryDrag { column, row }
            } else {
                *primary_mouse_down = true;
                Command::Click { column, row }
            }
        }
        Command::MousePrimaryUp { column, row } => {
            let was_down = std::mem::replace(primary_mouse_down, false);
            if was_down {
                Command::MousePrimaryUp { column, row }
            } else {
                Command::Click { column, row }
            }
        }
        command => command,
    }
}

fn format_hover_status(outcome: &crate::browser::HoverOutcome) -> String {
    let sanitize = |value: &str| {
        value
            .chars()
            .filter(|character| !character.is_control())
            .collect::<String>()
    };
    if let Some(href) = outcome.href.as_deref() {
        return format!("hover link: {}", sanitize(href));
    }
    match outcome.tag.as_deref() {
        Some("button") => String::from("hover button: click or Tab then Enter"),
        Some("input") | Some("textarea") => String::from("hover input: click or press i"),
        Some(tag) => format!("hover {tag}"),
        None => String::from("?:help i:input g/t:URL [/] back/forward {/} tabs click/wheel"),
    }
}

/// Hand an actual web URL to the operating system's regular browser.
///
/// `open::that` deliberately follows OS associations for `file:` URLs. In a
/// terminal workflow that can mean opening another terminal application rather
/// than a browser, which is surprising for the `o` / `v` commands. Local files
/// remain in the current fpasobrowser tab; only HTTP(S) locations are eligible
/// for a browser handoff.
fn open_in_regular_browser(value: &str) -> Result<()> {
    let url = Url::parse(value).with_context(|| format!("parse current URL {value:?}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        anyhow::bail!(
            "regular-browser handoff supports only http(s) URLs; {} stays in fpasobrowser",
            url.scheme()
        );
    }
    open::that(url.as_str()).context("ask operating system to open regular browser")
}

fn schedule_redraw_burst_after(
    pending_redraw_at: &mut Option<Instant>,
    burst_remaining: &mut u8,
    additional_frames: u8,
    delay: Duration,
) {
    *pending_redraw_at = Some(Instant::now() + delay);
    *burst_remaining = additional_frames;
}

fn schedule_input_redraw_after(pending_redraw_at: &mut Option<Instant>, delay: Duration) {
    *pending_redraw_at = Some(Instant::now() + delay);
}

fn schedule_scroll_redraw(
    pending_redraw_at: &mut Option<Instant>,
    redraw_burst_remaining: &mut u8,
) {
    *pending_redraw_at = Some(Instant::now() + SCROLL_REDRAW_DELAY);
    // A scroll is already the final visual operation the person requested;
    // cancel a pending click/navigation sampling burst rather than placing
    // stale intermediate frames over the final scroll position.
    *redraw_burst_remaining = 0;
}

fn loading_redraw_delay(frame_transport_is_expensive: bool) -> Duration {
    if frame_transport_is_expensive {
        SIXEL_LOADING_REDRAW_INTERVAL
    } else {
        LOADING_REDRAW_INTERVAL
    }
}

fn draw_disconnected(
    terminal: &mut Terminal,
    size: crate::render::RenderSize,
    message: &str,
) -> Result<()> {
    terminal.clear()?;
    queue!(
        terminal.writer(),
        MoveTo(0, 0),
        Print("fpasobrowser: browser disconnected"),
        MoveTo(0, 2),
        Print("Press r to relaunch the current URL."),
        MoveTo(0, 3),
        Print("Press v/o to open the current URL in your regular browser."),
        MoveTo(0, 4),
        Print("Press q to quit."),
        MoveTo(0, 5),
        Print("Press ? for help; press r to relaunch the browser."),
        MoveTo(0, 6),
        Print("Page input, scrolling, and Tab focus resume after the engine reconnects."),
    )?;
    terminal.status(size.rows, message)?;
    if terminal.has_tab_strip() {
        terminal.status(
            tab_strip_row(size),
            "Tabs: unavailable while browser is disconnected",
        )?;
    }
    if terminal.has_toolbar() {
        terminal.status(
            toolbar_row(size),
            &toolbar_text(size.columns, &TabInfo::default()),
        )?;
    }
    terminal.flush()?;
    Ok(())
}

async fn relaunch_browser_with_timeout(
    browser: &mut BrowserClient,
    browser_options: &BrowserOptions,
    size: crate::render::RenderSize,
    state: &PageState,
) -> Result<()> {
    timeout(
        RELAUNCH_TIMEOUT,
        relaunch_browser(browser, browser_options, size, state),
    )
    .await
    .context("browser relaunch timed out")?
}

async fn relaunch_browser(
    browser: &mut BrowserClient,
    browser_options: &BrowserOptions,
    size: crate::render::RenderSize,
    state: &PageState,
) -> Result<()> {
    let mut last_error = None;
    for attempt in 0..10 {
        match BrowserClient::launch(size, browser_options.clone(), state.url.clone()).await {
            Ok(relaunched_browser) => {
                *browser = relaunched_browser;
                return Ok(());
            }
            Err(error) => {
                last_error = Some(error);
                if attempt < 9 {
                    // An embedded engine can still fail transiently during
                    // initialization; retry before returning to the
                    // disconnected screen.
                    sleep(Duration::from_millis(500)).await;
                }
            }
        }
    }
    Err(last_error.expect("relaunch attempts must produce an error"))
}

async fn capture_only(url: Url, options: AppOptions) -> Result<()> {
    let size = crate::render::RenderSize {
        columns: 1280,
        rows: 360,
    };
    let dump_path = options
        .dump_screenshot
        .context("--capture-only requires --dump-screenshot <path>")?;
    let browser = BrowserClient::launch(size, options.browser, url).await?;
    let image = browser.screenshot(size).await?;
    image
        .save(&dump_path)
        .with_context(|| format!("write screenshot to {}", dump_path.display()))?;
    eprintln!("fpasobrowser: wrote screenshot to {}", dump_path.display());
    browser.shutdown().await?;
    Ok(())
}

async fn redraw(
    terminal: &mut Terminal,
    renderer: &SelectedBackend,
    browser: &BrowserClient,
    size: crate::render::RenderSize,
    state: &mut PageState,
    presentation: &mut Option<PresentationFingerprint>,
    options: RedrawOptions<'_>,
) -> Result<RedrawOutcome> {
    let image = match browser.screenshot(size).await {
        Ok(image) => image,
        Err(error) => {
            terminal.status(size.rows, &format!("redraw failed: {error}"))?;
            terminal.flush()?;
            return Err(error).context("capture browser screenshot");
        }
    };
    // Query all visible browser state in one engine round trip. This keeps the
    // URL, history affordances, and active tab label from one WebView snapshot.
    let tab_info = browser.tab_info().await.unwrap_or_default();
    if let Some(current_url) = &tab_info.active_url {
        if let Ok(url) = Url::parse(current_url) {
            state.url = url;
        }
    }
    if let Some(path) = options.dump_screenshot {
        image
            .save(path)
            .with_context(|| format!("write screenshot to {}", path.display()))?;
    }
    // Verification detection is informational. A page can be between frames
    // after a click, so do not turn a transient DOM-query failure into a full
    // engine disconnect.
    let verification_required = browser
        .manual_verification_required()
        .await
        .unwrap_or(false);
    let fingerprint = presentation_fingerprint(&image, size, options.show_help);
    if presentation.as_ref() != Some(&fingerprint) {
        // ANSI and SIXEL write directly into terminal cells, so they must
        // clear the previous frame. Kitty's placement is replaced by the
        // graphics backend; clearing all cells before every update causes a
        // blank frame while a nested fpasoterm/Herdr path catches up. We do
        // still clear for the initial frame and help visibility changes,
        // because those can leave text from a previous help panel behind.
        if should_clear_before_frame(renderer, presentation.as_ref(), &fingerprint) {
            terminal.clear()?;
        }
        queue!(terminal.writer(), MoveTo(0, 0))?;
        renderer.render(&image, size, terminal.writer())?;
        if options.show_help {
            draw_help_panel(terminal, size)?;
        }
        *presentation = Some(fingerprint);
    }
    let active_tab_status = active_tab_status(&tab_info, state);
    let status_message = compose_activity_status(
        &active_tab_status,
        options.message,
        verification_required,
        tab_info.is_loading,
        options.loading_frame,
    );
    let toolbar = toolbar_text(size.columns, &tab_info);
    let renderer_label = renderer.terminal_cell_pixels().map_or_else(
        || renderer.name().to_string(),
        |cell_pixels| {
            format!(
                "{} {}x{}",
                renderer.name(),
                cell_pixels.width,
                cell_pixels.height
            )
        },
    );
    let toolbar_text = format!(
        "{toolbar} fpasobrowser [{renderer_label} {:?}]",
        options.input_mode,
    );
    if terminal.has_toolbar() {
        terminal.status(
            toolbar_row(size),
            &truncate_to_columns(&toolbar_text, usize::from(size.columns)),
        )?;
    }
    terminal.status(
        size.rows,
        &truncate_to_columns(&status_message, usize::from(size.columns)),
    )?;
    if terminal.has_tab_strip() {
        terminal.status(
            tab_strip_row(size),
            &tab_strip_text(size.columns, &tab_info),
        )?;
    }
    terminal.flush()?;
    Ok(RedrawOutcome {
        is_loading: tab_info.is_loading,
    })
}

fn presentation_fingerprint(
    image: &DynamicImage,
    size: crate::render::RenderSize,
    show_help: bool,
) -> PresentationFingerprint {
    // FNV-1a is sufficient for an in-process redraw cache. It is not used for
    // a security decision; it only avoids retransmitting an identical frame.
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let rgba = image.to_rgba8();
    let mut image_hash = OFFSET_BASIS;
    for byte in rgba.as_raw() {
        image_hash ^= u64::from(*byte);
        image_hash = image_hash.wrapping_mul(PRIME);
    }
    PresentationFingerprint {
        image_hash,
        image_width: rgba.width(),
        image_height: rgba.height(),
        size,
        show_help,
    }
}

/// Decide whether a terminal-cell clear is required before sending a new
/// image frame.
///
/// Kitty owns a placement layer rather than terminal cells, so a normal frame
/// replacement must leave those cells alone.  In particular, clearing them
/// first produces a visible flash when the image protocol crosses the
/// fpasoterm/Herdr PTY boundary. A resize is safe without a cell clear: the
/// old placement scales with the terminal until the replacement arrives. Only
/// help visibility can leave ordinary text in the grid, so it still requires
/// a clear.
fn should_clear_before_frame(
    renderer: &SelectedBackend,
    previous: Option<&PresentationFingerprint>,
    next: &PresentationFingerprint,
) -> bool {
    renderer.clears_terminal_before_frame()
        || previous.is_none()
        || previous.is_some_and(|frame| frame.show_help != next.show_help)
}

fn prompt_url(
    terminal: &mut Terminal,
    row: crate::render::RenderSize,
    initial: &str,
) -> Result<Option<Url>> {
    loop {
        let Some(value) = prompt_line(terminal, row.rows, "URL", initial)? else {
            return Ok(None);
        };
        match Url::parse(&value) {
            Ok(url) => return Ok(Some(url)),
            Err(error) => {
                terminal.status(row.rows, &format!("invalid URL: {error}; Esc cancels"))?;
                terminal.flush()?;
            }
        }
    }
}

fn prompt_number(
    terminal: &mut Terminal,
    row: crate::render::RenderSize,
    label: &str,
) -> Result<Option<usize>> {
    let Some(value) = prompt_line(terminal, row.rows, label, "")? else {
        return Ok(None);
    };
    Ok(value.trim().parse::<usize>().ok())
}

fn prompt_line(
    terminal: &mut Terminal,
    row: u16,
    label: &str,
    initial: &str,
) -> Result<Option<String>> {
    let mut value = initial.to_string();
    loop {
        terminal.status(row, &format!("{label}: {value}"))?;
        terminal.flush()?;
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Esc => return Ok(None),
                KeyCode::Enter => return Ok(Some(value)),
                KeyCode::Backspace => {
                    value.pop();
                }
                KeyCode::Char(c) => value.push(c),
                _ => {}
            }
        }
    }
}

fn toolbar_text(columns: u16, tab_info: &TabInfo) -> String {
    let mut text = String::new();
    for (index, (label, _description)) in TOOLBAR_BUTTONS.iter().enumerate() {
        // Disabled history labels retain the display width of [B]/[F]. The
        // mouse cell mapping in toolbar_command therefore remains stable.
        let label = match index {
            0 if !tab_info.can_go_back => "(B)",
            1 if !tab_info.can_go_forward => "(F)",
            _ => label,
        };
        let separator = if text.is_empty() { "" } else { " " };
        if text.len() + separator.len() + label.len() > usize::from(columns) {
            break;
        }
        text.push_str(separator);
        text.push_str(label);
    }
    text
}

fn active_tab_status(tab_info: &TabInfo, state: &PageState) -> String {
    if tab_info.labels.is_empty() {
        return format!("tab unavailable: {}", state.url);
    }

    let title = tab_info.labels.get(tab_info.active_index).map_or_else(
        || String::from("new tab"),
        |label| {
            label
                .chars()
                .filter(|character| !character.is_control())
                .map(|character| {
                    if character.is_whitespace() {
                        ' '
                    } else {
                        character
                    }
                })
                .collect::<String>()
                .trim()
                .to_string()
        },
    );
    let location = tab_info.active_url.as_deref().unwrap_or(state.url.as_str());
    let loading = if tab_info.is_loading {
        "loading"
    } else {
        "ready"
    };
    format!(
        "tab {}/{} {loading}: {title} — {location}",
        tab_info.active_index + 1,
        tab_info.labels.len()
    )
}

fn compose_activity_status(
    active_tab_status: &str,
    message: &str,
    verification_required: bool,
    is_loading: bool,
    loading_frame: u8,
) -> String {
    if verification_required {
        let loading = if is_loading {
            format!("[loading {}] ", loading_indicator(loading_frame))
        } else {
            String::new()
        };
        return format!(
            "{loading}SITE VERIFICATION: o/v opens a separate browser; its session cannot sync back | {active_tab_status}"
        );
    }

    // A navigation can take longer than the next terminal event. Put a small
    // ASCII spinner before the normal feedback so that a page which is still
    // requesting resources is visibly active, including on terminals whose
    // fonts do not contain Unicode spinner glyphs.
    if is_loading {
        return format!(
            "[loading {}] {message} | {active_tab_status}",
            loading_indicator(loading_frame)
        );
    }

    // Keep command feedback at the beginning of the activity line. A long
    // page title or file URL used to hide successful actions such as reload,
    // help, and history navigation entirely, making the toolbar look inert.
    // The compact idle hint is the one exception: in that state the current
    // tab identity is more useful as the leading information.
    if message != DEFAULT_ACTIVITY_MESSAGE {
        format!("{message} | {active_tab_status}")
    } else {
        format!("{active_tab_status} | {message}")
    }
}

fn loading_indicator(frame: u8) -> char {
    const FRAMES: [char; 4] = ['|', '/', '-', '\\'];
    FRAMES[usize::from(frame) % FRAMES.len()]
}

fn format_link_hint_activation(hint: usize, outcome: LinkHintOutcome) -> String {
    let tab = outcome.active_index + 1;
    if outcome.opened_popup_tabs == 0 {
        format!("activated hint {hint} in tab {tab}/{}", outcome.tab_count)
    } else {
        format!(
            "activated hint {hint}; opened {} popup terminal tab(s), switched to tab {tab}/{}",
            outcome.opened_popup_tabs, outcome.tab_count
        )
    }
}

fn toolbar_row(size: crate::render::RenderSize) -> u16 {
    size.rows.saturating_add(2)
}

fn tab_strip_row(size: crate::render::RenderSize) -> u16 {
    size.rows.saturating_add(1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TabStripAction {
    Select(usize),
    Close(usize),
}

fn tab_strip_segment(index: usize, active_index: usize, label: &str) -> String {
    const MAX_TAB_LABEL_CHARS: usize = 20;
    let mut label = label
        .chars()
        .filter(|character| !character.is_control())
        .map(|character| {
            if character.is_whitespace() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .trim()
        .chars()
        .take(MAX_TAB_LABEL_CHARS)
        .collect::<String>();
    if label.is_empty() {
        label = String::from("new tab");
    }
    let active_marker = if index == active_index { "*" } else { "" };
    format!("[{}{} {} ×]", index + 1, active_marker, label)
}

fn tab_strip_text(columns: u16, tab_info: &TabInfo) -> String {
    if tab_info.labels.is_empty() {
        return String::from("Tabs: unavailable");
    }

    let max_columns = usize::from(columns);
    let mut text = String::from("Tabs:");
    for (index, label) in tab_info.labels.iter().enumerate() {
        let segment = tab_strip_segment(index, tab_info.active_index, label);
        if text.chars().count() + 1 + segment.chars().count() > max_columns {
            break;
        }
        text.push(' ');
        text.push_str(&segment);
    }
    text
}

fn tab_strip_target(columns: u16, column: u16, tab_info: &TabInfo) -> Option<TabStripAction> {
    let max_columns = usize::from(columns);
    let mut cursor = "Tabs:".chars().count();
    let column = usize::from(column);
    for (index, label) in tab_info.labels.iter().enumerate() {
        let segment = tab_strip_segment(index, tab_info.active_index, label);
        let start = cursor + 1;
        let end = start + segment.chars().count();
        if end > max_columns {
            return None;
        }
        if (start..end).contains(&column) {
            // A tab always ends in ` ×]`. Only the explicit multiplication
            // sign closes it; every other visible character selects it.
            let close_column = end.saturating_sub(2);
            return Some(if column == close_column {
                TabStripAction::Close(index)
            } else {
                TabStripAction::Select(index)
            });
        }
        cursor = end;
    }
    None
}

fn toolbar_command(columns: u16, column: u16) -> Option<Command> {
    let mut cursor = 0_usize;
    let column = usize::from(column);
    for (index, (label, _description)) in TOOLBAR_BUTTONS.iter().enumerate() {
        let separator = usize::from(cursor > 0);
        if cursor + separator + label.len() > usize::from(columns) {
            return None;
        }
        cursor += separator;
        let start = cursor;
        let end = start + label.len();
        if (start..end).contains(&column) {
            return match index {
                0 => Some(Command::Back),
                1 => Some(Command::Forward),
                2 => Some(Command::Reload),
                3 => Some(Command::GoToUrl),
                4 => Some(Command::NewTab),
                5 => Some(Command::PreviousTab),
                6 => Some(Command::NextTab),
                7 => Some(Command::OpenVerification),
                8 => Some(Command::Help),
                _ => None,
            };
        }
        cursor = end;
    }
    None
}

fn truncate_to_columns(value: &str, columns: usize) -> String {
    if columns == 0 {
        return String::new();
    }
    let mut text = value.chars().take(columns).collect::<String>();
    if value.chars().count() > columns && columns > 1 {
        text.pop();
        text.push('…');
    }
    text
}

fn draw_help_panel(terminal: &mut Terminal, size: crate::render::RenderSize) -> Result<()> {
    let lines = [
        version_info(),
        "Mouse toolbar: [B] back [F] forward [R] reload [URL] go [+Tab] new [<Tab]/[Tab>] switch [V] verify [?] help",
        "Keys: q/Ctrl+Q quit | Esc blur/previous tab | r/R reload | g URL | b/B or [ back | F or ] forward | t new tab | x close tab",
        "Tabs: {/}/Ctrl+Tab switch | v/V open browser for verification | f numbered link hints",
        "Tabs: click a visible label to switch; click its × to close (the final tab is kept)",
        "Page: click page | wheel scroll | j/k or Up/Down scroll | Tab/Shift+Tab focus | Enter activate | f link hints",
        "Input: i enters input mode; the page focus ring is used when available, otherwise cyan marks focus; Tab/Shift+Tab moves it; Esc blurs",
        "Status: [working] means Servo is processing one request; q works in normal mode, Ctrl+Q works in every mode after it completes",
    ];
    let start = size.rows.saturating_sub(lines.len() as u16 + 1);
    for (offset, line) in lines.iter().enumerate() {
        queue!(
            terminal.writer(),
            MoveTo(0, start + offset as u16),
            Clear(ClearType::CurrentLine),
            Print(*line)
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        active_tab_status, compose_activity_status, format_hover_status,
        format_link_hint_activation, loading_indicator, loading_redraw_delay,
        normalize_primary_mouse_command, open_in_regular_browser, presentation_fingerprint,
        schedule_scroll_redraw, should_clear_before_frame, tab_strip_row, tab_strip_target,
        tab_strip_text, toolbar_row, toolbar_text, working_status, TabStripAction,
    };
    use crate::browser::{HoverOutcome, LinkHintOutcome, TabInfo};
    use crate::input::Command;
    use crate::render::{BackendChoice, RenderSize, SelectedBackend};
    use image::{DynamicImage, Rgba, RgbaImage};
    use std::time::Instant;

    #[test]
    fn hover_link_status_uses_the_destination_url() {
        assert_eq!(
            format_hover_status(&HoverOutcome {
                tag: Some(String::from("span")),
                href: Some(String::from("https://example.test/docs")),
            }),
            "hover link: https://example.test/docs"
        );
    }

    #[test]
    fn hover_status_removes_terminal_control_characters() {
        assert_eq!(
            format_hover_status(&HoverOutcome {
                tag: Some(String::from("a")),
                href: Some(String::from("https://example.test/\u{1b}[2J")),
            }),
            "hover link: https://example.test/[2J"
        );
    }

    #[test]
    fn identical_frame_does_not_require_a_second_terminal_presentation() {
        let size = RenderSize {
            columns: 80,
            rows: 22,
        };
        let image = DynamicImage::ImageRgba8(RgbaImage::from_pixel(2, 2, Rgba([1, 2, 3, 255])));
        let same = presentation_fingerprint(&image, size, false);
        assert_eq!(same, presentation_fingerprint(&image, size, false));

        let changed = DynamicImage::ImageRgba8(RgbaImage::from_pixel(2, 2, Rgba([4, 5, 6, 255])));
        assert_ne!(same, presentation_fingerprint(&changed, size, false));
    }

    #[test]
    fn kitty_frame_replacement_keeps_terminal_cells_across_resize() {
        let size = RenderSize {
            columns: 80,
            rows: 22,
        };
        let image = DynamicImage::ImageRgba8(RgbaImage::from_pixel(2, 2, Rgba([1, 2, 3, 255])));
        let previous = presentation_fingerprint(&image, size, false);
        let changed = DynamicImage::ImageRgba8(RgbaImage::from_pixel(2, 2, Rgba([4, 5, 6, 255])));
        let next = presentation_fingerprint(&changed, size, false);
        let kitty = SelectedBackend::from_choice(BackendChoice::Kitty);

        assert!(!should_clear_before_frame(&kitty, Some(&previous), &next));
        assert!(should_clear_before_frame(
            &kitty,
            Some(&previous),
            &presentation_fingerprint(&changed, size, true),
        ));
        assert!(!should_clear_before_frame(
            &kitty,
            Some(&previous),
            &presentation_fingerprint(
                &changed,
                RenderSize {
                    columns: 81,
                    rows: 22,
                },
                false,
            ),
        ));
    }

    #[test]
    fn scroll_redraw_is_debounced_and_cancels_action_sampling() {
        let before = Instant::now();
        let mut pending = None;
        let mut burst_remaining = 1;

        schedule_scroll_redraw(&mut pending, &mut burst_remaining);

        assert_eq!(burst_remaining, 0);
        assert!(pending.expect("scroll schedules one final frame") >= before);
    }

    #[test]
    fn sixel_keeps_loading_polling_at_a_lower_frequency() {
        assert!(loading_redraw_delay(true) > loading_redraw_delay(false));
    }

    #[test]
    fn regular_browser_handoff_rejects_local_files_before_os_association() {
        let error = open_in_regular_browser("file:///tmp/fpasobrowser-test.html")
            .expect_err("file URL must stay in fpasobrowser");
        assert_eq!(
            error.to_string(),
            "regular-browser handoff supports only http(s) URLs; file stays in fpasobrowser"
        );
    }

    #[test]
    fn failed_handoff_feedback_precedes_a_long_active_tab_status() {
        let active =
            "tab 1/1 ready: interaction page — file:///very/long/local/path/interaction.html";
        let message = "open external failed: regular-browser handoff supports only http(s) URLs; file stays in fpasobrowser";

        assert!(compose_activity_status(active, message, false, false, 0)
            .starts_with("open external failed:"));
    }

    #[test]
    fn successful_toolbar_feedback_precedes_a_long_active_tab_status() {
        let active =
            "tab 1/1 ready: interaction page — file:///very/long/local/path/interaction.html";

        assert_eq!(
            compose_activity_status(active, "reloaded", false, false, 0),
            format!("reloaded | {active}")
        );
        assert!(
            compose_activity_status(active, "help shown; click ? to hide", false, false, 0)
                .starts_with("help shown;")
        );
    }

    #[test]
    fn loading_status_has_an_ascii_progress_indicator() {
        assert_eq!(loading_indicator(0), '|');
        assert_eq!(loading_indicator(1), '/');
        assert_eq!(loading_indicator(2), '-');
        assert_eq!(loading_indicator(3), '\\');
        assert!(
            compose_activity_status("tab 1/1 loading: Example", "navigated", false, true, 1)
                .starts_with("[loading /]")
        );
    }

    #[test]
    fn working_status_is_visible_and_identifies_the_operation() {
        assert_eq!(
            working_status("closing embedded browser safely"),
            "[working] closing embedded browser safely"
        );
    }

    #[test]
    fn toolbar_has_a_row_separate_from_activity_status() {
        let size = RenderSize {
            columns: 80,
            rows: 22,
        };

        assert_eq!(tab_strip_row(size), 23);
        assert_eq!(toolbar_row(size), 24);
    }

    #[test]
    fn orphaned_drag_or_release_becomes_one_click_without_duplicating_a_press() {
        let mut pressed = false;
        let drag = Command::MousePrimaryDrag { column: 8, row: 4 };
        assert_eq!(
            normalize_primary_mouse_command(drag, &mut pressed),
            Command::Click { column: 8, row: 4 }
        );
        assert!(pressed);
        assert_eq!(
            normalize_primary_mouse_command(
                Command::MousePrimaryUp { column: 8, row: 4 },
                &mut pressed,
            ),
            Command::MousePrimaryUp { column: 8, row: 4 }
        );
        assert!(!pressed);

        assert_eq!(
            normalize_primary_mouse_command(
                Command::MousePrimaryUp { column: 3, row: 2 },
                &mut pressed,
            ),
            Command::Click { column: 3, row: 2 }
        );
    }

    #[test]
    fn tab_strip_marks_the_active_tab_and_maps_clicks() {
        let tabs = TabInfo {
            active_index: 1,
            labels: vec![String::from("first page"), String::from("second page")],
            ..TabInfo::default()
        };
        let text = tab_strip_text(80, &tabs);

        assert!(text.contains("[1 first page ×]"));
        let second_start = text.find("[2* second page ×]").expect("second tab label");
        assert_eq!(
            tab_strip_target(80, (second_start + 2) as u16, &tabs),
            Some(TabStripAction::Select(1))
        );
        let close_column = text.find('×').expect("close label");
        assert_eq!(
            tab_strip_target(80, close_column as u16, &tabs),
            Some(TabStripAction::Close(0))
        );
        assert_eq!(tab_strip_target(80, 0, &tabs), None);
    }

    #[test]
    fn toolbar_marks_unavailable_history_without_changing_click_columns() {
        let unavailable = toolbar_text(80, &TabInfo::default());
        assert!(unavailable.starts_with("(B) (F) [R]"));

        let available = toolbar_text(
            80,
            &TabInfo {
                can_go_back: true,
                can_go_forward: true,
                ..TabInfo::default()
            },
        );
        assert!(available.starts_with("[B] [F] [R]"));
        assert_eq!(unavailable.chars().count(), available.chars().count());
    }

    #[test]
    fn active_tab_status_includes_loading_title_and_location() {
        let state = crate::browser::PageState::new(
            url::Url::parse("https://fallback.example/").expect("valid test URL"),
        );
        let status = active_tab_status(
            &TabInfo {
                active_index: 0,
                labels: vec![String::from("Example\n page")],
                active_url: Some(String::from("https://current.example/path")),
                is_loading: true,
                ..TabInfo::default()
            },
            &state,
        );

        assert_eq!(
            status,
            "tab 1/1 loading: Example page — https://current.example/path"
        );
    }

    #[test]
    fn link_hint_status_describes_popup_tab_selection() {
        assert_eq!(
            format_link_hint_activation(
                7,
                LinkHintOutcome {
                    opened_popup_tabs: 1,
                    active_index: 2,
                    tab_count: 3,
                },
            ),
            "activated hint 7; opened 1 popup terminal tab(s), switched to tab 3/3"
        );
        assert_eq!(
            format_link_hint_activation(
                2,
                LinkHintOutcome {
                    opened_popup_tabs: 0,
                    active_index: 0,
                    tab_count: 1,
                },
            ),
            "activated hint 2 in tab 1/1"
        );
    }
}
