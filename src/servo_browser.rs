//! Servo-backed browser client for the embedded fpasobrowser runtime.
//!
//! Servo requires its event loop and rendering context to stay on one thread.
//! The terminal/Tokio side therefore sends requests to this owner thread and
//! receives images or completion notifications over ordinary channels.

use crate::render::{RenderSize, TerminalCellPixels};
use anyhow::{bail, Context, Result};
use dpi::PhysicalSize;
use embedder_traits::{EventLoopWaker, LoadStatus};
use image::{DynamicImage, GenericImageView};
use servo::{
    Code, CreateNewWebViewRequest, DeviceIntPoint, DeviceIntRect, InputEvent, JSValue, Key,
    KeyState, KeyboardEvent, Location, Modifiers, MouseButton, MouseButtonAction, MouseButtonEvent,
    MouseMoveEvent, NamedKey, RenderingContext, Servo, ServoBuilder, SoftwareRenderingContext,
    WebView, WebViewBuilder, WebViewDelegate,
};
use std::{
    any::Any,
    cell::RefCell,
    panic::{catch_unwind, AssertUnwindSafe},
    rc::Rc,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, Sender},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};
use tokio::task;
use url::Url;

const DEFAULT_BROWSER_VIEWPORT_WIDTH: u32 = 960;
const DEFAULT_BROWSER_VIEWPORT_HEIGHT: u32 = 540;
const ENGINE_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const FRAME_WAIT_STEPS: usize = 600;
const FRAME_WAIT_INTERVAL: Duration = Duration::from_millis(16);
// `take_screenshot` callbacks can be deferred indefinitely by Servo while a
// page swaps documents. The software surface is still readable, so wait only
// briefly for the authoritative callback before using that painted frame.
const SCREENSHOT_CALLBACK_WAIT_STEPS: usize = 60;
// Windows ANGLE can expose the software surface's white clear colour one or
// two paints before WebRender has copied the document scene into it. Keep the
// retry small and only use it for a near-white/transparent uniform frame;
// normal white web pages with actual content still return immediately.
const UNPAINTED_FRAME_RETRIES: usize = 3;
const UNPAINTED_FRAME_RETRY_INTERVAL: Duration = Duration::from_millis(60);

#[derive(Default)]
struct PageSignals {
    frame_serial: AtomicUsize,
}

impl PageSignals {
    fn begin_navigation(&self) -> usize {
        self.frame_serial.load(Ordering::Acquire)
    }
}

pub struct BrowserClient {
    engine: EngineHandle,
    fixed_viewport: bool,
    terminal_cell_pixels: Option<TerminalCellPixels>,
}

struct EngineHandle {
    sender: Sender<EngineCommand>,
    worker: Option<thread::JoinHandle<()>>,
    exit_reason: EngineExitReason,
}

type EngineExitReason = Arc<Mutex<Option<String>>>;

impl Drop for EngineHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl EngineHandle {
    async fn request<T, F>(&self, command: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(Sender<Result<T>>) -> EngineCommand + Send + 'static,
    {
        request(self.sender.clone(), self.exit_reason.clone(), command).await
    }

    async fn request_unit<F>(&self, command: F) -> Result<()>
    where
        F: FnOnce(Sender<Result<()>>) -> EngineCommand + Send + 'static,
    {
        self.request(command).await
    }

    fn shutdown(&mut self) {
        let _ = self.sender.send(EngineCommand::Shutdown);
        // A WebView can be inside a platform/Servo callback when a terminal
        // session is being recovered. Joining that worker here would block
        // the UI thread indefinitely and leave raw mode on screen, making q
        // and r appear broken. Dropping a JoinHandle detaches it; the worker
        // receives Shutdown when it is able to resume, and process exit
        // remains the final cleanup boundary for an unrecoverable engine.
        let _ = self.worker.take();
    }

    /// Finish an engine used by a non-interactive one-shot operation.
    ///
    /// Unlike the interactive Drop path, a capture command is not in terminal
    /// raw mode and has no UI responsiveness requirement. Joining here lets
    /// Servo release the WebView and software context on its owner thread
    /// before the process exits. Detaching that thread after writing a PNG can
    /// otherwise race process teardown and produce a post-capture crash.
    fn shutdown_and_join(&mut self) -> Result<()> {
        let _ = self.sender.send(EngineCommand::Shutdown);
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| anyhow::anyhow!("Servo engine thread panicked during shutdown"))?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct BrowserOptions {
    /// Emit Servo startup diagnostics to stderr.
    pub debug: bool,
    pub viewport: Option<BrowserViewport>,
    /// Physical terminal-cell pixels required by the selected renderer. This
    /// is unset for renderers that do not control an image raster directly.
    pub terminal_cell_pixels: Option<TerminalCellPixels>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrowserViewport {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone)]
pub struct PageState {
    pub url: Url,
    pub scroll_y: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClickOutcome {
    pub editable: bool,
    /// A native `<select>` is keyboard-operable but does not accept text.
    /// Keep it separate from `editable` so the terminal can enter its
    /// keyboard mode without pretending that typed characters are valid.
    pub select: bool,
    pub iframe: bool,
    /// An ARIA option (such as a search suggestion) keeps the associated
    /// editor focused while its own pointer activation completes.
    pub suggestion: bool,
    pub cdp_fallback: bool,
    pub target_tag: Option<String>,
    pub active_tag: Option<String>,
    pub active_editable: bool,
    pub opened_popup_tabs: usize,
}

/// Result of activating a numbered page hint.
///
/// A hint can invoke `target="_blank"` or `window.open()`, in which case Servo
/// promotes the requested WebView into a selected terminal tab. Returning the
/// resulting tab state lets the TUI describe that transition immediately,
/// before the next frame has been drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkHintOutcome {
    pub opened_popup_tabs: usize,
    pub active_index: usize,
    pub tab_count: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HoverOutcome {
    pub tag: Option<String>,
    pub href: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseTabOutcome {
    Closed { active_index: usize },
    KeptLast { active_index: usize },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TabInfo {
    pub active_index: usize,
    pub labels: Vec<String>,
    /// Current location of the active WebView, if Servo has assigned one.
    pub active_url: Option<String>,
    /// Whether the active WebView has a previous history entry.
    pub can_go_back: bool,
    /// Whether the active WebView has a next history entry.
    pub can_go_forward: bool,
    /// Whether the active document is still loading.
    pub is_loading: bool,
}

impl PageState {
    #[must_use]
    pub fn new(url: Url) -> Self {
        Self { url, scroll_y: 0 }
    }

    pub fn scroll_down_by(&mut self, delta_y: i64) {
        self.scroll_y = self.scroll_y.saturating_add(delta_y);
    }

    pub fn scroll_up_by(&mut self, delta_y: i64) {
        self.scroll_y = self.scroll_y.saturating_sub(delta_y).max(0);
    }
}

enum EngineCommand {
    Navigate(Url, Sender<Result<()>>),
    NewTab(Url, Sender<Result<usize>>),
    CloseTab(Sender<Result<CloseTabOutcome>>),
    NextTab(Sender<Result<usize>>),
    PreviousTab(Sender<Result<usize>>),
    TabInfo(Sender<Result<TabInfo>>),
    SelectTab(usize, Sender<Result<usize>>),
    Reload(Sender<Result<()>>),
    Back(Sender<Result<()>>),
    Forward(Sender<Result<()>>),
    Screenshot(Sender<Result<DynamicImage>>),
    Scroll(i64, Sender<Result<()>>),
    Click {
        column: u16,
        row: u16,
        size: RenderSize,
        reply: Sender<Result<ClickOutcome>>,
    },
    Hover {
        column: u16,
        row: u16,
        size: RenderSize,
        reply: Sender<Result<HoverOutcome>>,
    },
    TypeText(String, Sender<Result<()>>),
    Key(String, Sender<Result<()>>),
    FocusFirstEditable(Sender<Result<bool>>),
    BlurActiveElement(Sender<Result<()>>),
    ShowLinkHints(Sender<Result<usize>>),
    ActivateLinkHint(usize, Sender<Result<LinkHintOutcome>>),
    ManualVerificationRequired(Sender<Result<bool>>),
    Resize(BrowserViewport, Sender<Result<()>>),
    CurrentUrl(Sender<Result<String>>),
    Shutdown,
}

struct EngineTab {
    webview: WebView,
    rendering_context: Rc<SoftwareRenderingContext>,
    signals: Arc<PageSignals>,
}

type PopupRequests = Rc<RefCell<Vec<EngineTab>>>;

#[derive(Clone)]
struct Waker(Arc<AtomicBool>);

impl EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }

    fn wake(&self) {
        self.0.store(true, Ordering::Release);
    }
}

struct Delegate {
    signals: Arc<PageSignals>,
    viewport: BrowserViewport,
    rendering_context: Rc<SoftwareRenderingContext>,
    popup_requests: PopupRequests,
}

impl WebViewDelegate for Delegate {
    fn notify_new_frame_ready(&self, webview: WebView) {
        webview.paint();
        self.signals.frame_serial.fetch_add(1, Ordering::AcqRel);
    }

    fn request_create_new(&self, _parent_webview: WebView, request: CreateNewWebViewRequest) {
        // CreateNewWebViewRequest owns Servo's response channel. It must be
        // fulfilled during this delegate callback; retaining it for the outer
        // engine loop leaves window.open()/target=_blank waiting and can stall
        // every later input event. The request must also receive a distinct
        // software surface: sharing the parent surface made the tab strip say
        // that the popup was active while screenshots still showed the parent
        // page.
        let Ok(rendering_context) = create_rendering_context(self.viewport) else {
            // Dropping the request's automatic responder tells Servo that the
            // popup could not be created without blocking the current page.
            return;
        };
        let signals = Arc::new(PageSignals::default());
        let webview = request
            .builder(rendering_context.clone())
            .delegate(Rc::new(Self {
                signals: signals.clone(),
                viewport: self.viewport,
                // A popup must retain the context passed to its builder.
                // Retaining the parent context here made a later popup frame
                // paint against the wrong surface and could turn an otherwise
                // unrelated subsequent click into the active error tab.
                rendering_context: rendering_context.clone(),
                popup_requests: self.popup_requests.clone(),
            }))
            .build();
        // Continue processing the initiating input event on its original
        // surface. `select_tab` activates the popup surface immediately after
        // the callback is promoted by the engine loop.
        let _ = self.rendering_context.make_current();
        self.popup_requests.borrow_mut().push(EngineTab {
            webview,
            rendering_context,
            signals,
        });
    }
}

impl BrowserClient {
    /// Start an embedded Servo WebView with `initial_url` as its first page.
    ///
    /// Servo needs the initial URL while its WebView is being created. Loading
    /// a second URL immediately after an `about:blank` WebView has started can
    /// lose that first navigation, leaving a seemingly running but blank TUI.
    pub async fn launch(
        size: RenderSize,
        options: BrowserOptions,
        initial_url: Url,
    ) -> Result<Self> {
        let viewport = browser_viewport(size, options.viewport, options.terminal_cell_pixels);
        let (sender, receiver) = mpsc::channel();
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let debug = options.debug;
        let exit_reason = Arc::new(Mutex::new(None));
        let worker_exit_reason = exit_reason.clone();
        let worker = thread::Builder::new()
            .name("fpasobrowser-servo".to_owned())
            .spawn(move || {
                run_engine(
                    receiver,
                    viewport,
                    debug,
                    initial_url,
                    ready_sender,
                    worker_exit_reason,
                )
            })
            .context("start Servo engine thread")?;
        task::spawn_blocking(move || {
            let ready = ready_receiver
                .recv_timeout(ENGINE_COMMAND_TIMEOUT)
                .context("wait for Servo engine startup")?;
            if let Err(error) = ready {
                let _ = worker.join();
                return Err(error);
            }
            Ok(Self {
                engine: EngineHandle {
                    sender,
                    worker: Some(worker),
                    exit_reason,
                },
                fixed_viewport: options.viewport.is_some(),
                terminal_cell_pixels: options.terminal_cell_pixels,
            })
        })
        .await?
    }

    /// Shut down a one-shot engine and wait for its owner thread to release
    /// Servo resources. Interactive TUI teardown intentionally uses Drop's
    /// non-blocking path instead so a stalled browser cannot hold raw mode.
    pub async fn shutdown(mut self) -> Result<()> {
        task::spawn_blocking(move || self.engine.shutdown_and_join())
            .await
            .context("join Servo engine shutdown task")??;
        Ok(())
    }

    pub async fn navigate(&self, url: Url) -> Result<()> {
        self.engine
            .request_unit(move |reply| EngineCommand::Navigate(url, reply))
            .await
    }

    pub async fn new_tab(&self, url: Url) -> Result<usize> {
        self.engine
            .request(move |reply| EngineCommand::NewTab(url, reply))
            .await
    }

    pub async fn close_tab(&self) -> Result<CloseTabOutcome> {
        self.engine.request(EngineCommand::CloseTab).await
    }

    pub async fn next_tab(&self) -> Result<usize> {
        self.engine.request(EngineCommand::NextTab).await
    }

    pub async fn previous_tab(&self) -> Result<usize> {
        self.engine.request(EngineCommand::PreviousTab).await
    }

    pub async fn tab_info(&self) -> Result<TabInfo> {
        self.engine.request(EngineCommand::TabInfo).await
    }

    pub async fn select_tab(&self, index: usize) -> Result<usize> {
        self.engine
            .request(move |reply| EngineCommand::SelectTab(index, reply))
            .await
    }

    pub async fn reload(&self) -> Result<()> {
        self.engine.request_unit(EngineCommand::Reload).await
    }

    pub async fn go_back(&self) -> Result<()> {
        self.engine.request_unit(EngineCommand::Back).await
    }

    pub async fn go_forward(&self) -> Result<()> {
        self.engine.request_unit(EngineCommand::Forward).await
    }

    pub async fn screenshot(&self, _size: RenderSize) -> Result<DynamicImage> {
        self.engine.request(EngineCommand::Screenshot).await
    }

    pub async fn manual_verification_required(&self) -> Result<bool> {
        self.engine
            .request(EngineCommand::ManualVerificationRequired)
            .await
    }

    pub async fn scroll_by(&self, delta_y: i64) -> Result<()> {
        self.engine
            .request_unit(move |reply| EngineCommand::Scroll(delta_y, reply))
            .await
    }

    pub async fn type_text(&self, text: String) -> Result<()> {
        self.engine
            .request_unit(move |reply| EngineCommand::TypeText(text, reply))
            .await
    }

    pub async fn focus_first_editable(&self) -> Result<bool> {
        self.engine.request(EngineCommand::FocusFirstEditable).await
    }

    pub async fn blur_active_element(&self) -> Result<()> {
        self.engine
            .request_unit(EngineCommand::BlurActiveElement)
            .await
    }

    pub async fn press_key_with_fallback(&self, key: &'static str) -> Result<()> {
        self.engine
            .request_unit(move |reply| EngineCommand::Key(key.to_owned(), reply))
            .await
    }

    pub async fn click_js(&self, column: u16, row: u16, size: RenderSize) -> Result<ClickOutcome> {
        self.engine
            .request(move |reply| EngineCommand::Click {
                column,
                row,
                size,
                reply,
            })
            .await
    }

    /// Forward a pointer move to Servo and return the interactive target under
    /// the terminal cell. The caller rate-limits this because terminal mouse
    /// motion can be considerably noisier than click events.
    pub async fn hover(&self, column: u16, row: u16, size: RenderSize) -> Result<HoverOutcome> {
        self.engine
            .request(move |reply| EngineCommand::Hover {
                column,
                row,
                size,
                reply,
            })
            .await
    }

    pub async fn current_url(&self) -> Result<String> {
        self.engine.request(EngineCommand::CurrentUrl).await
    }

    pub async fn show_link_hints(&self) -> Result<usize> {
        self.engine.request(EngineCommand::ShowLinkHints).await
    }

    pub async fn activate_link_hint(&self, hint: usize) -> Result<LinkHintOutcome> {
        self.engine
            .request(move |reply| EngineCommand::ActivateLinkHint(hint, reply))
            .await
    }

    /// Resize Servo's WebView contexts when the terminal gets larger or smaller.
    /// An explicit `--viewport` remains fixed, which keeps screenshot automation
    /// repeatable regardless of the terminal used to start it.
    pub async fn resize(&self, size: RenderSize) -> Result<()> {
        if self.fixed_viewport {
            return Ok(());
        }
        let viewport = browser_viewport(size, None, self.terminal_cell_pixels);
        self.engine
            .request_unit(move |reply| EngineCommand::Resize(viewport, reply))
            .await
    }
}

async fn request<T, F>(
    sender: Sender<EngineCommand>,
    exit_reason: EngineExitReason,
    command: F,
) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce(Sender<Result<T>>) -> EngineCommand + Send + 'static,
{
    task::spawn_blocking(move || {
        let (reply_sender, reply_receiver) = mpsc::channel();
        sender
            .send(command(reply_sender))
            .map_err(|error| engine_command_error("send Servo command", &exit_reason, error))?;
        reply_receiver
            .recv_timeout(ENGINE_COMMAND_TIMEOUT)
            .map_err(|error| engine_command_error("wait for Servo command", &exit_reason, error))?
    })
    .await?
}

fn engine_command_error(
    operation: &str,
    exit_reason: &EngineExitReason,
    source: impl std::fmt::Display,
) -> anyhow::Error {
    let reason = exit_reason
        .lock()
        .ok()
        .and_then(|reason| reason.clone())
        .unwrap_or_else(|| String::from("Servo engine ended without a reported reason"));
    anyhow::anyhow!("{operation}: {source}; {reason}")
}

fn run_engine(
    receiver: Receiver<EngineCommand>,
    viewport: BrowserViewport,
    debug: bool,
    initial_url: Url,
    ready_sender: mpsc::SyncSender<Result<()>>,
    exit_reason: EngineExitReason,
) {
    let result = catch_unwind(AssertUnwindSafe(|| -> Result<()> {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let servo = ServoBuilder::default()
            .event_loop_waker(Box::new(Waker(Arc::new(AtomicBool::new(false)))))
            .build();
        if debug {
            eprintln!(
                "fpasobrowser debug: Servo engine started at {}x{}",
                viewport.width, viewport.height
            );
        }
        let popup_requests = Rc::new(RefCell::new(Vec::new()));
        let first_tab = create_engine_tab(&servo, viewport, initial_url, popup_requests.clone())?;
        ready_sender.send(Ok(())).ok();
        engine_loop(
            receiver,
            &servo,
            vec![first_tab],
            viewport,
            popup_requests,
            debug,
        );
        Ok(())
    }));
    match result {
        Ok(Ok(())) => {
            record_engine_exit(&exit_reason, "Servo engine loop exited".to_owned());
        }
        Ok(Err(error)) => {
            let reason = format!("Servo engine stopped: {error:#}");
            record_engine_exit(&exit_reason, reason.clone());
            if debug {
                eprintln!("fpasobrowser debug: {reason}");
            }
            let _ = ready_sender.send(Err(error));
        }
        Err(payload) => {
            let message = panic_message(payload);
            let reason = format!("Servo engine panicked: {message}");
            record_engine_exit(&exit_reason, reason.clone());
            if debug {
                eprintln!("fpasobrowser debug: {reason}");
            }
            let _ = ready_sender.send(Err(anyhow::anyhow!(
                "Servo engine panicked during startup: {message}"
            )));
        }
    }
}

fn record_engine_exit(exit_reason: &EngineExitReason, reason: String) {
    if let Ok(mut recorded) = exit_reason.lock() {
        *recorded = Some(reason);
    }
}

fn panic_message(payload: Box<dyn Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_owned();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    String::from("non-string panic payload")
}

fn create_engine_tab(
    servo: &Servo,
    viewport: BrowserViewport,
    initial_url: Url,
    popup_requests: PopupRequests,
) -> Result<EngineTab> {
    let rendering_context = create_rendering_context(viewport)?;
    let signals = Arc::new(PageSignals::default());
    let webview = WebViewBuilder::new(servo, rendering_context.clone())
        .url(initial_url.clone())
        .delegate(Rc::new(Delegate {
            signals: signals.clone(),
            viewport,
            rendering_context: rendering_context.clone(),
            popup_requests,
        }))
        .build();
    // Complete the first navigation before exposing the tab. Servo needs the
    // URL during WebView construction; immediately loading after about:blank
    // can lose the first request.
    wait_for_navigation_frame(
        servo,
        &webview,
        &rendering_context,
        viewport,
        &signals,
        0,
        Some(initial_url),
    )?;
    // Do not expose a WebView immediately after only a surface-read check.
    // On Windows ANGLE that read can be the initial white clear buffer even
    // though Servo has already emitted a frame-ready notification. A real
    // screenshot request drives WebRender through its completion callback;
    // `capture_frame` also retries an unpainted surface for a short interval.
    let _ = capture_frame(servo, &webview, &rendering_context, viewport)?;
    Ok(EngineTab {
        webview,
        rendering_context,
        signals,
    })
}

fn create_rendering_context(viewport: BrowserViewport) -> Result<Rc<SoftwareRenderingContext>> {
    let size = PhysicalSize {
        width: viewport.width,
        height: viewport.height,
    };
    let rendering_context = Rc::new(
        SoftwareRenderingContext::new(size)
            .map_err(|error| anyhow::anyhow!("create Servo software context: {error:?}"))?,
    );
    rendering_context
        .make_current()
        .map_err(|error| anyhow::anyhow!("make Servo context current: {error:?}"))?;
    Ok(rendering_context)
}

fn select_tab(
    servo: &Servo,
    tabs: &mut [EngineTab],
    active: &mut usize,
    index: usize,
) -> Result<usize> {
    if index >= tabs.len() {
        bail!("tab index {index} is outside the open tab list");
    }
    if *active != index {
        // A page button receives DOM focus from a mouse click. Do not leave
        // that button's page-defined focus treatment visible when its tab is
        // no longer selected.
        let previous = &tabs[*active];
        previous
            .rendering_context
            .make_current()
            .map_err(|error| anyhow::anyhow!("activate previous tab context: {error:?}"))?;
        let _ = blur_active_element(servo, &previous.webview);
    }
    tabs[index]
        .rendering_context
        .make_current()
        .map_err(|error| anyhow::anyhow!("activate selected tab context: {error:?}"))?;
    for (tab_index, tab) in tabs.iter().enumerate() {
        tab.webview.set_focused(tab_index == index);
    }
    *active = index;
    Ok(index)
}

fn tab_label(tab: &EngineTab) -> String {
    tab.webview
        .page_title()
        .or_else(|| tab.webview.url().map(|url| url.to_string()))
        .filter(|label| !label.trim().is_empty())
        .unwrap_or_else(|| String::from("new tab"))
}

fn promote_popup_tabs(
    servo: &Servo,
    tabs: &mut Vec<EngineTab>,
    active: &mut usize,
    popup_requests: &PopupRequests,
) -> Result<usize> {
    let mut popup_tabs = std::mem::take(&mut *popup_requests.borrow_mut());
    if popup_tabs.is_empty() {
        return Ok(0);
    }

    let opened = popup_tabs.len();
    tabs.append(&mut popup_tabs);

    let popup_index = tabs.len() - 1;
    select_tab(servo, tabs, active, popup_index)?;
    // Let the auxiliary WebView accept its initial navigation before the next
    // terminal redraw asks it for a screenshot. The screenshot path retains
    // its normal frame wait as a second safeguard for slow documents.
    pump(servo, &tabs[popup_index].webview, 3);
    Ok(opened)
}

fn engine_loop(
    receiver: Receiver<EngineCommand>,
    servo: &Servo,
    mut tabs: Vec<EngineTab>,
    mut viewport: BrowserViewport,
    popup_requests: PopupRequests,
    debug: bool,
) {
    let mut active = 0_usize;
    loop {
        servo.spin_event_loop();
        // The delegate constructs auxiliary WebViews immediately to fulfil
        // Servo's create-new response, then leaves the completed tab here for
        // the engine loop to select and expose through the terminal UI.
        let _ = promote_popup_tabs(servo, &mut tabs, &mut active, &popup_requests);
        match receiver.recv_timeout(FRAME_WAIT_INTERVAL) {
            Ok(EngineCommand::Navigate(url, reply)) => {
                let tab = &tabs[active];
                let frame_serial = tab.signals.begin_navigation();
                let expected_url = url.clone();
                tab.webview.load(url);
                let _ = reply.send(wait_for_navigation_frame(
                    servo,
                    &tab.webview,
                    &tab.rendering_context,
                    viewport,
                    &tab.signals,
                    frame_serial,
                    Some(expected_url),
                ));
            }
            Ok(EngineCommand::NewTab(url, reply)) => {
                let result = create_engine_tab(servo, viewport, url, popup_requests.clone())
                    .and_then(|tab| {
                        tabs.push(tab);
                        let new_active = tabs.len() - 1;
                        select_tab(servo, &mut tabs, &mut active, new_active)
                    });
                let _ = reply.send(result);
            }
            Ok(EngineCommand::CloseTab(reply)) => {
                let result = if tabs.len() == 1 {
                    Ok(CloseTabOutcome::KeptLast { active_index: 0 })
                } else {
                    tabs.remove(active);
                    if active == tabs.len() {
                        active = active.saturating_sub(1);
                    }
                    let new_active = active;
                    select_tab(servo, &mut tabs, &mut active, new_active)
                        .map(|active_index| CloseTabOutcome::Closed { active_index })
                };
                let _ = reply.send(result);
            }
            Ok(EngineCommand::NextTab(reply)) => {
                let next = (active + 1) % tabs.len();
                let _ = reply.send(select_tab(servo, &mut tabs, &mut active, next));
            }
            Ok(EngineCommand::PreviousTab(reply)) => {
                let previous = if active == 0 {
                    tabs.len() - 1
                } else {
                    active - 1
                };
                let _ = reply.send(select_tab(servo, &mut tabs, &mut active, previous));
            }
            Ok(EngineCommand::TabInfo(reply)) => {
                let labels = tabs.iter().map(tab_label).collect();
                let active_tab = &tabs[active];
                let _ = reply.send(Ok(TabInfo {
                    active_index: active,
                    labels,
                    active_url: active_tab.webview.url().map(|url| url.to_string()),
                    can_go_back: active_tab.webview.can_go_back(),
                    can_go_forward: active_tab.webview.can_go_forward(),
                    is_loading: active_tab.webview.load_status() != LoadStatus::Complete,
                }));
            }
            Ok(EngineCommand::SelectTab(index, reply)) => {
                let _ = reply.send(select_tab(servo, &mut tabs, &mut active, index));
            }
            Ok(EngineCommand::Reload(reply)) => {
                let tab = &tabs[active];
                let frame_serial = tab.signals.begin_navigation();
                tab.webview.reload();
                let _ = reply.send(wait_for_navigation_frame(
                    servo,
                    &tab.webview,
                    &tab.rendering_context,
                    viewport,
                    &tab.signals,
                    frame_serial,
                    None,
                ));
            }
            Ok(EngineCommand::Back(reply)) => {
                let tab = &tabs[active];
                if tab.webview.can_go_back() {
                    let frame_serial = tab.signals.begin_navigation();
                    tab.webview.go_back(1);
                    let _ = reply.send(wait_for_navigation_frame(
                        servo,
                        &tab.webview,
                        &tab.rendering_context,
                        viewport,
                        &tab.signals,
                        frame_serial,
                        None,
                    ));
                } else {
                    let _ = reply.send(Err(anyhow::anyhow!("no previous history entry")));
                }
            }
            Ok(EngineCommand::Forward(reply)) => {
                let tab = &tabs[active];
                if tab.webview.can_go_forward() {
                    let frame_serial = tab.signals.begin_navigation();
                    tab.webview.go_forward(1);
                    let _ = reply.send(wait_for_navigation_frame(
                        servo,
                        &tab.webview,
                        &tab.rendering_context,
                        viewport,
                        &tab.signals,
                        frame_serial,
                        None,
                    ));
                } else {
                    let _ = reply.send(Err(anyhow::anyhow!("no next history entry")));
                }
            }
            Ok(EngineCommand::Screenshot(reply)) => {
                let tab = &tabs[active];
                let _ = reply.send(capture_frame(
                    servo,
                    &tab.webview,
                    &tab.rendering_context,
                    viewport,
                ));
            }
            Ok(EngineCommand::Scroll(delta_y, reply)) => {
                let webview = &tabs[active].webview;
                let point = servo::DevicePoint::new(
                    viewport.width as f32 / 2.0,
                    viewport.height as f32 / 2.0,
                );
                webview.notify_input_event(InputEvent::Wheel(servo::WheelEvent::new(
                    servo::WheelDelta {
                        x: 0.0,
                        y: -(delta_y as f64),
                        z: 0.0,
                        mode: servo::WheelMode::DeltaPixel,
                    },
                    point.into(),
                )));
                servo.spin_event_loop();
                let _ = reply.send(Ok(()));
            }
            Ok(EngineCommand::Click {
                column,
                row,
                size,
                reply,
            }) => {
                let (before, focused) = {
                    let webview = &tabs[active].webview;
                    let point = terminal_cell_to_viewport_point(column, row, size, viewport);
                    let before = inspect_click_target(servo, webview, point).unwrap_or_default();
                    // For an editable control that the DOM can identify
                    // precisely, focus it before sending a native pointer
                    // sequence. The DOM focus call gives the offscreen path a
                    // precise target, but it must be followed by the native
                    // down/up pair as well: Servo's keyboard dispatcher keeps
                    // its own focused-area state for default text editing,
                    // select controls, and form submission. Previously the
                    // direct-focus success path skipped those native events,
                    // leaving a visible focused input that could not receive
                    // its editing/default key actions on Windows.
                    let _focused_directly = before.editable
                        && focus_editable_at_point(servo, webview, point).unwrap_or(false);
                    webview.notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(
                        point.into(),
                    )));
                    webview.notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(
                        MouseButtonAction::Down,
                        MouseButton::Primary,
                        point.into(),
                    )));
                    webview.notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(
                        MouseButtonAction::Up,
                        MouseButton::Primary,
                        point.into(),
                    )));
                    pump(servo, webview, 3);
                    // Servo's offscreen input path can deliver pointer events
                    // to an anchor without carrying out its default
                    // navigation. The terminal has already identified the
                    // resolved URL under the pointer, so load it explicitly
                    // in the same WebView. This preserves native click
                    // handlers while making ordinary links (for example
                    // Google About/Store) reliably navigable.
                    if let Some(href) = before.href.clone() {
                        webview.load(href);
                        pump(servo, webview, 2);
                    }
                    let navigation_in_flight = webview.load_status() != LoadStatus::Complete;
                    // Only controls that need keyboard ownership require a
                    // post-click DOM query. A link or button has already
                    // received its native down/up pair; querying and then
                    // blurring the old document can race its asynchronous
                    // navigation, hold this serialized engine command for
                    // the full script callback timeout, and make the queued
                    // redraw look like a browser disconnect.
                    //
                    // Google search suggestions are ARIA options owned by
                    // the still-focused search editor, so they intentionally
                    // take the same no-post-click-query path.
                    let needs_keyboard_focus =
                        before.editable || before.iframe || before.tag.as_deref() == Some("select");
                    let mut focused = if !needs_keyboard_focus || navigation_in_flight {
                        InspectedTarget::default()
                    } else {
                        inspect_active_element(servo, webview).unwrap_or_default()
                    };
                    // Retain a small same-origin fallback for embedded
                    // inputs. A parent document reports its iframe as active,
                    // while the editable child can otherwise be missed during
                    // the short paint/event pump above. This never probes a
                    // cross-origin child frame.
                    if needs_keyboard_focus
                        && !navigation_in_flight
                        && (before.editable || before.iframe)
                        && !focused.editable
                    {
                        let _ = focus_editable_at_point(servo, webview, point);
                        focused = inspect_active_element(servo, webview).unwrap_or_default();
                    }
                    if needs_keyboard_focus
                        && !navigation_in_flight
                        && before.tag.as_deref() == Some("select")
                        && focused.tag.as_deref() != Some("select")
                    {
                        // A software WebView has no OS select popup to own
                        // focus for us. Make the same-origin control the
                        // keyboard target explicitly after its native click.
                        let _ = focus_select_at_point(servo, webview, point);
                        focused = inspect_active_element(servo, webview).unwrap_or_default();
                    }
                    if needs_keyboard_focus
                        && !navigation_in_flight
                        && (focused.editable
                            || focused.iframe
                            || focused.tag.as_deref() == Some("select"))
                    {
                        let _ = refresh_focus_indicator(servo, webview);
                    }
                    (before, focused)
                };
                let popup_result =
                    promote_popup_tabs(servo, &mut tabs, &mut active, &popup_requests);
                let _ = reply.send(popup_result.map(|opened_popup_tabs| ClickOutcome {
                    // The element under the terminal pointer can differ by a
                    // fraction of a cell from the element Servo focused. Use
                    // both observations so a successful input click always
                    // enters input mode.
                    editable: before.editable || focused.editable,
                    select: before.tag.as_deref() == Some("select")
                        || focused.tag.as_deref() == Some("select"),
                    iframe: before.iframe || focused.iframe,
                    suggestion: before.suggestion,
                    cdp_fallback: false,
                    target_tag: before.tag,
                    active_tag: focused.tag,
                    active_editable: focused.editable,
                    opened_popup_tabs,
                }));
            }
            Ok(EngineCommand::Hover {
                column,
                row,
                size,
                reply,
            }) => {
                let webview = &tabs[active].webview;
                let point = terminal_cell_to_viewport_point(column, row, size, viewport);
                webview
                    .notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(point.into())));
                pump(servo, webview, 1);
                let _ = reply.send(inspect_hover_target(servo, webview, point));
            }
            Ok(EngineCommand::TypeText(text, reply)) => {
                let webview = &tabs[active].webview;
                send_text(webview, text);
                pump(servo, webview, 2);
                let _ = reply.send(Ok(()));
            }
            Ok(EngineCommand::Key(key, reply)) => {
                let webview = &tabs[active].webview;
                // Servo 0.7's offscreen embedder does not provide a native
                // select-popup surface.  Choosing with an arrow key must
                // therefore update a focused HTMLSelectElement directly;
                // otherwise ArrowUp/ArrowDown falls through to page scrolling
                // and the terminal appears to have stopped responding.
                if let Some(direction) = select_option_direction(&key) {
                    if advance_focused_select(servo, webview, direction).unwrap_or(false) {
                        pump(servo, webview, 2);
                        let _ = refresh_focus_indicator(servo, webview);
                        let _ = reply.send(Ok(()));
                        continue;
                    }
                }
                send_key(webview, &key);
                pump(servo, webview, 2);
                if key == "Enter" && webview.load_status() == LoadStatus::Complete {
                    // Native Enter is always sent first. Servo's offscreen
                    // keyboard embedder can, however, dispatch the key event
                    // without performing the HTML default form action. Queue
                    // a deliberately narrow search-form submit as a fallback
                    // but do not wait for its JavaScript callback: navigation
                    // tears down that document before such a callback is
                    // guaranteed to arrive.
                    dispatch_focused_search_submit(webview);
                    pump(servo, webview, 2);
                }
                let navigation_in_flight = webview.load_status() != LoadStatus::Complete;
                if debug && key == "Enter" {
                    eprintln!(
                        "fpasobrowser debug: Enter dispatched; navigation-in-flight={navigation_in_flight} load-status={:?}",
                        webview.load_status(),
                    );
                }
                // A form submission can replace the document synchronously.
                // Do not evaluate the focus-overlay script after Enter: even
                // when `load_status` has not flipped during these two pumps,
                // Google may be between its key handler and its navigation.
                // Waiting for that old document's script callback was the
                // actual 9.6-second Enter timeout on Windows.
                if key != "Enter" && !navigation_in_flight {
                    let _ = refresh_focus_indicator(servo, webview);
                }
                let _ = reply.send(Ok(()));
            }
            Ok(EngineCommand::FocusFirstEditable(reply)) => {
                let _ = reply.send(focus_first_editable(servo, &tabs[active].webview));
            }
            Ok(EngineCommand::BlurActiveElement(reply)) => {
                let _ = reply.send(blur_active_element(servo, &tabs[active].webview));
            }
            Ok(EngineCommand::ShowLinkHints(reply)) => {
                let _ = reply.send(show_link_hints(servo, &tabs[active].webview));
            }
            Ok(EngineCommand::ActivateLinkHint(hint, reply)) => {
                let result =
                    activate_link_hint(servo, &tabs[active].webview, hint).and_then(|()| {
                        let opened_popup_tabs =
                            promote_popup_tabs(servo, &mut tabs, &mut active, &popup_requests)?;
                        Ok(LinkHintOutcome {
                            opened_popup_tabs,
                            active_index: active,
                            tab_count: tabs.len(),
                        })
                    });
                let _ = reply.send(result);
            }
            Ok(EngineCommand::ManualVerificationRequired(reply)) => {
                let _ = reply.send(manual_verification_required(servo, &tabs[active].webview));
            }
            Ok(EngineCommand::Resize(new_viewport, reply)) => {
                viewport = new_viewport;
                let size = PhysicalSize::new(viewport.width, viewport.height);
                for tab in &tabs {
                    let _ = tab.rendering_context.make_current();
                    tab.webview.resize(size);
                }
                let _ = tabs[active].rendering_context.make_current();
                for _ in 0..3 {
                    servo.spin_event_loop();
                    tabs[active].webview.paint();
                }
                let _ = reply.send(Ok(()));
            }
            Ok(EngineCommand::CurrentUrl(reply)) => {
                let result = tabs[active]
                    .webview
                    .url()
                    .map(|url| url.to_string())
                    .ok_or_else(|| anyhow::anyhow!("Servo WebView has no current URL"));
                let _ = reply.send(result);
            }
            Ok(EngineCommand::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

#[derive(Default)]
struct InspectedTarget {
    editable: bool,
    iframe: bool,
    suggestion: bool,
    tag: Option<String>,
    href: Option<Url>,
}

fn pump(servo: &Servo, webview: &WebView, steps: usize) {
    for _ in 0..steps {
        servo.spin_event_loop();
        webview.paint();
    }
}

fn evaluate_javascript(servo: &Servo, webview: &WebView, script: &str) -> Result<JSValue> {
    let result = Rc::new(RefCell::new(None));
    let callback_result = result.clone();
    webview.evaluate_javascript(script, move |value| {
        *callback_result.borrow_mut() = Some(value);
    });

    for _ in 0..FRAME_WAIT_STEPS {
        servo.spin_event_loop();
        webview.paint();
        if let Some(value) = result.borrow_mut().take() {
            return value.map_err(|error| anyhow::anyhow!("evaluate Servo JavaScript: {error:?}"));
        }
        thread::sleep(FRAME_WAIT_INTERVAL);
    }
    bail!("timed out evaluating Servo JavaScript")
}

fn focus_first_editable(servo: &Servo, webview: &WebView) -> Result<bool> {
    const SCRIPT: &str = r#"
        (() => {
          const selector = 'input:not([type=hidden]):not([disabled]),textarea:not([disabled]),select:not([disabled]),[contenteditable=true],[contenteditable=""]';
          const isVisible = (element) => {
            const rect = element.getBoundingClientRect();
            return rect.width > 0 && rect.height > 0;
          };
          const own = Array.from(document.querySelectorAll(selector)).find(isVisible);
          if (!own) return false;
          own.focus();
          return document.activeElement === own;
        })()
    "#;
    let direct_focus = match evaluate_javascript(servo, webview, SCRIPT)? {
        JSValue::Boolean(focused) => focused,
        other => bail!("unexpected editable-focus result from Servo: {other:?}"),
    };
    if direct_focus {
        let _ = refresh_focus_indicator(servo, webview);
        return Ok(true);
    }

    // Servo's native focus traversal enters an embedded browsing context even
    // when the offscreen JavaScript binding cannot safely expose
    // `contentDocument`. Querying a child DOM during this transition has
    // disconnected Servo on some same-origin iframe pages. Use native Tab as
    // the narrow fallback: it reaches the child control without page-script
    // access to any iframe, including a cross-origin CAPTCHA frame.
    send_key(webview, "Tab");
    pump(servo, webview, 2);
    let focused = inspect_active_element(servo, webview)?;
    // The parent document can only observe the iframe element while Servo owns
    // the child document's focused control. Treat that keyboard destination as
    // a successful focus result too; it is the native Tab traversal that will
    // deliver subsequent character events to the child control.
    let accepts_keyboard =
        focused.editable || focused.iframe || focused.tag.as_deref() == Some("select");
    if accepts_keyboard {
        let _ = refresh_focus_indicator(servo, webview);
    }
    Ok(accepts_keyboard)
}

fn blur_active_element(servo: &Servo, webview: &WebView) -> Result<()> {
    const SCRIPT: &str = r#"
        (() => {
          const blur = (doc) => {
            const element = doc.activeElement;
            if (!element) return;
            if ((element.tagName || '').toLowerCase() === 'iframe') {
              try {
                if (element.contentDocument) blur(element.contentDocument);
              } catch (_) {
                // Cross-origin frames cannot be inspected by page script.
              }
            }
            if (element.blur) element.blur();
          };
          blur(document);
          return true;
        })()
    "#;
    let _ = evaluate_javascript(servo, webview, SCRIPT)?;
    let _ = clear_focus_indicator(servo, webview);
    Ok(())
}

fn inspect_click_target(
    servo: &Servo,
    webview: &WebView,
    point: servo::DevicePoint,
) -> Result<InspectedTarget> {
    let script = format!(
        r#"(() => {{
          const scale = window.devicePixelRatio || 1;
          const inspect = (doc, x, y, nested) => {{
            const element = doc.elementFromPoint(x, y);
            if (!element) return '';
            const tag = (element.tagName || '').toLowerCase();
            if (tag === 'iframe') {{
              try {{
                const rect = element.getBoundingClientRect();
                const child = element.contentDocument;
                if (child) {{
                  const inside = inspect(child, x - rect.left, y - rect.top, true);
                  if (inside) return inside;
                }}
              }} catch (_) {{
                // Cross-origin frames remain a native input target.
              }}
            }}
            const editable = element.isContentEditable || tag === 'textarea' || (tag === 'input' && element.type !== 'hidden' && !element.disabled);
            let option = element;
            while (option && option !== doc.documentElement) {{
              if ((option.getAttribute && option.getAttribute('role') || '').toLowerCase() === 'option') break;
              option = option.parentElement;
            }}
            const suggestion = !!option;
            let anchor = element;
            while (anchor && anchor !== doc.documentElement) {{
              if ((anchor.tagName || '').toLowerCase() === 'a' && anchor.href) break;
              anchor = anchor.parentElement;
            }}
            const href = anchor && anchor.href ? anchor.href : '';
            return [editable ? '1' : '0', (nested || tag === 'iframe') ? '1' : '0', suggestion ? '1' : '0', tag, href].join('|');
          }};
          return inspect(document, {:.3} / scale, {:.3} / scale, false);
        }})()"#,
        point.x, point.y
    );
    let JSValue::String(value) = evaluate_javascript(servo, webview, &script)? else {
        return Ok(InspectedTarget::default());
    };
    let mut fields = value.splitn(5, '|');
    let editable = fields.next() == Some("1");
    let iframe = fields.next() == Some("1");
    let suggestion = fields.next() == Some("1");
    let tag = fields
        .next()
        .filter(|tag| !tag.is_empty())
        .map(ToOwned::to_owned);
    let href = fields
        .next()
        .filter(|href| !href.is_empty())
        .and_then(|href| Url::parse(href).ok());
    Ok(InspectedTarget {
        editable,
        iframe,
        suggestion,
        tag,
        href,
    })
}

/// Focus a visible same-origin editable element located beneath a viewport
/// point. This is deliberately narrow: it provides reliable terminal text
/// entry for a confirmed editable control, while ordinary buttons and links
/// continue through Servo's native click implementation. It does not inspect
/// cross-origin frames.
fn focus_editable_at_point(
    servo: &Servo,
    webview: &WebView,
    point: servo::DevicePoint,
) -> Result<bool> {
    let script = format!(
        r#"(() => {{
          const scale = window.devicePixelRatio || 1;
          const editable = (element) => {{
            const tag = (element.tagName || '').toLowerCase();
            return element.isContentEditable || tag === 'textarea' || (tag === 'input' && element.type !== 'hidden' && !element.disabled);
          }};
          const focusAt = (doc, x, y) => {{
            const element = doc.elementFromPoint(x, y);
            if (!element) return false;
            if ((element.tagName || '').toLowerCase() === 'iframe') {{
              try {{
                const rect = element.getBoundingClientRect();
                const child = element.contentDocument;
                if (child) {{
                  element.focus();
                  if (child.defaultView) child.defaultView.focus();
                  return focusAt(child, x - rect.left, y - rect.top);
                }}
              }} catch (_) {{
                return false;
              }}
            }}
            if (!editable(element)) return false;
            element.focus();
            return true;
          }};
          return focusAt(document, {:.3} / scale, {:.3} / scale);
        }})()"#,
        point.x, point.y
    );
    match evaluate_javascript(servo, webview, &script)? {
        JSValue::Boolean(focused) => Ok(focused),
        other => bail!("unexpected point-focus result from Servo: {other:?}"),
    }
}

/// Focus a visible same-origin native select at the pointer position.
///
/// It is intentionally separate from `focus_editable_at_point`: a select does
/// not accept text, but it does need to become the keyboard destination for
/// terminal Up/Down selection.
fn focus_select_at_point(
    servo: &Servo,
    webview: &WebView,
    point: servo::DevicePoint,
) -> Result<bool> {
    let script = format!(
        r#"(() => {{
          const scale = window.devicePixelRatio || 1;
          const element = document.elementFromPoint({:.3} / scale, {:.3} / scale);
          if (!element || (element.tagName || '').toLowerCase() !== 'select' || element.disabled) return false;
          element.focus();
          return document.activeElement === element;
        }})()"#,
        point.x, point.y
    );
    match evaluate_javascript(servo, webview, &script)? {
        JSValue::Boolean(focused) => Ok(focused),
        other => bail!("unexpected select-focus result from Servo: {other:?}"),
    }
}

fn inspect_hover_target(
    servo: &Servo,
    webview: &WebView,
    point: servo::DevicePoint,
) -> Result<HoverOutcome> {
    let script = format!(
        "(() => {{ const scale = window.devicePixelRatio || 1; const element = document.elementFromPoint({:.3} / scale, {:.3} / scale); if (!element) return ''; const tag = (element.tagName || '').toLowerCase(); let node = element; while (node) {{ const nodeTag = (node.tagName || '').toLowerCase(); if (nodeTag === 'a' && node.getAttribute && node.getAttribute('href')) {{ return ['a', node.href || node.getAttribute('href') || ''].join('|'); }} node = node.parentNode; }} return [tag, ''].join('|'); }})()",
        point.x, point.y
    );
    let JSValue::String(value) = evaluate_javascript(servo, webview, &script)? else {
        return Ok(HoverOutcome::default());
    };
    let (tag, href) = value.split_once('|').unwrap_or((&value, ""));
    Ok(HoverOutcome {
        tag: (!tag.is_empty()).then(|| tag.to_owned()),
        href: (!href.is_empty()).then(|| href.to_owned()),
    })
}

fn inspect_active_element(servo: &Servo, webview: &WebView) -> Result<InspectedTarget> {
    const SCRIPT: &str = r#"
        (() => {
          const element = document.activeElement;
          if (!element || element === document.body || element === document.documentElement) return '';
          const tag = (element.tagName || '').toLowerCase();
          const editable = element.isContentEditable || tag === 'textarea' || (tag === 'input' && element.type !== 'hidden' && !element.disabled);
          // An iframe is a valid native keyboard destination. Do not recurse
          // into its child document from Servo's offscreen JS bridge.
          return [editable ? '1' : '0', tag === 'iframe' ? '1' : '0', tag].join('|');
        })()
    "#;
    let JSValue::String(value) = evaluate_javascript(servo, webview, SCRIPT)? else {
        return Ok(InspectedTarget::default());
    };
    let mut fields = value.splitn(3, '|');
    let editable = fields.next() == Some("1");
    let iframe = fields.next() == Some("1");
    let tag = fields
        .next()
        .filter(|tag| !tag.is_empty())
        .map(ToOwned::to_owned);
    Ok(InspectedTarget {
        editable,
        iframe,
        suggestion: false,
        tag,
        href: None,
    })
}

fn refresh_focus_indicator(servo: &Servo, webview: &WebView) -> Result<bool> {
    const SCRIPT: &str = r#"
        (() => {
          const id = '__fpasobrowser_focus_indicator__';
          const old = document.getElementById(id);
          const locate = (doc, offsetX, offsetY) => {
            const target = doc.activeElement;
            if (!target || target === doc.body || target === doc.documentElement) return null;
            const tag = (target.tagName || '').toLowerCase();
            if (tag === 'iframe') {
              try {
                const frameRect = target.getBoundingClientRect();
                const child = target.contentDocument;
                if (child) {
                  const inside = locate(child, offsetX + frameRect.left, offsetY + frameRect.top);
                  if (inside) return inside;
                }
              } catch (_) {
                // The outer iframe rectangle is the only safe marker for a
                // cross-origin frame.
              }
            }
            const rect = target.getBoundingClientRect();
            if (rect.width <= 0 || rect.height <= 0) return null;
            return {
              element: target,
              left: offsetX + rect.left,
              top: offsetY + rect.top,
              width: rect.width,
              height: rect.height
            };
          };
          const target = locate(document, 0, 0);
          // Pages commonly provide their own :focus outline.  Drawing our
          // terminal-assistance marker on top of that ring produces two or
          // three cyan rectangles around a single input, which is both noisy
          // and misleading.  Keep the marker as a fallback for pages without
          // a visible focus treatment, but let a page's explicit outline or
          // focus box shadow be the one authoritative indicator when present.
          const hasVisiblePageFocus = (element) => {
            try {
              const style = element.ownerDocument.defaultView.getComputedStyle(element);
              const outlineWidth = Number.parseFloat(style.outlineWidth || '0');
              const outline = style.outlineStyle && style.outlineStyle !== 'none'
                && (outlineWidth > 0 || style.outlineStyle === 'auto');
              return outline || (style.boxShadow && style.boxShadow !== 'none');
            } catch (_) {
              return false;
            }
          };
          if (!target || hasVisiblePageFocus(target.element)) {
            if (old) old.remove();
            return false;
          }
          const marker = old || document.createElement('div');
          marker.id = id;
          marker.setAttribute('aria-hidden', 'true');
          marker.style.cssText = [
            'position:fixed',
            'pointer-events:none',
            'z-index:2147483647',
            'box-sizing:border-box',
            'border:3px solid #21d4fd',
            'border-radius:4px',
            'box-shadow:0 0 0 2px #001b2d,0 0 12px #21d4fd'
          ].join(';');
          marker.style.left = `${Math.max(0, target.left - 3)}px`;
          marker.style.top = `${Math.max(0, target.top - 3)}px`;
          marker.style.width = `${Math.max(1, target.width + 6)}px`;
          marker.style.height = `${Math.max(1, target.height + 6)}px`;
          marker.textContent = '';
          if (!old) document.documentElement.appendChild(marker);
          return true;
        })()
    "#;
    match evaluate_javascript(servo, webview, SCRIPT)? {
        JSValue::Boolean(shown) => Ok(shown),
        other => bail!("unexpected focus-indicator result from Servo: {other:?}"),
    }
}

fn clear_focus_indicator(servo: &Servo, webview: &WebView) -> Result<()> {
    const SCRIPT: &str = r#"
        (() => {
          const marker = document.getElementById('__fpasobrowser_focus_indicator__');
          if (marker) marker.remove();
          return true;
        })()
    "#;
    let _ = evaluate_javascript(servo, webview, SCRIPT)?;
    Ok(())
}

fn show_link_hints(servo: &Servo, webview: &WebView) -> Result<usize> {
    const SCRIPT: &str = r#"
        (() => {
          const overlayId = '__fpasobrowser_link_hints__';
          const old = document.getElementById(overlayId);
          if (old) old.remove();
          document.querySelectorAll('[data-fpasobrowser-hint]').forEach((element) => element.removeAttribute('data-fpasobrowser-hint'));
          const overlay = document.createElement('div');
          overlay.id = overlayId;
          overlay.style.cssText = 'position:fixed;inset:0;z-index:2147483647;pointer-events:none;font:12px monospace';
          const candidates = Array.from(document.querySelectorAll('a[href],button,input:not([type=hidden]),select,textarea,[role=button],[onclick]'))
            .filter((element) => { const rect = element.getBoundingClientRect(); return rect.width > 0 && rect.height > 0 && !element.disabled; })
            .slice(0, 99);
          candidates.forEach((element, index) => {
            const hint = index + 1;
            const rect = element.getBoundingClientRect();
            element.setAttribute('data-fpasobrowser-hint', String(hint));
            const label = document.createElement('span');
            label.textContent = String(hint);
            label.style.cssText = `position:fixed;left:${Math.max(0, rect.left)}px;top:${Math.max(0, rect.top)}px;background:#ffe600;color:#000;border:1px solid #222;padding:1px 2px;line-height:1;`;
            overlay.appendChild(label);
          });
          document.documentElement.appendChild(overlay);
          return candidates.length;
        })()
    "#;
    match evaluate_javascript(servo, webview, SCRIPT)? {
        JSValue::Number(count) if count >= 0.0 => Ok(count as usize),
        other => bail!("unexpected link-hint result from Servo: {other:?}"),
    }
}

fn activate_link_hint(servo: &Servo, webview: &WebView, hint: usize) -> Result<()> {
    let script = format!(
        "(() => {{ const element = document.querySelector('[data-fpasobrowser-hint=\\\"{hint}\\\"]'); const overlay = document.getElementById('__fpasobrowser_link_hints__'); if (overlay) overlay.remove(); document.querySelectorAll('[data-fpasobrowser-hint]').forEach((item) => item.removeAttribute('data-fpasobrowser-hint')); if (!element) return false; element.focus(); element.click(); return true; }})()"
    );
    match evaluate_javascript(servo, webview, &script)? {
        JSValue::Boolean(true) => {
            pump(servo, webview, 3);
            Ok(())
        }
        JSValue::Boolean(false) => bail!("link hint {hint} is no longer available"),
        other => bail!("unexpected link activation result from Servo: {other:?}"),
    }
}

/// Detect a visible verification challenge in the page without attempting to
/// evade or solve it. This only informs the TUI that `o`/`v` may be the more
/// useful next action.
fn manual_verification_required(servo: &Servo, webview: &WebView) -> Result<bool> {
    const SCRIPT: &str = r#"
        (() => {
          const iframe = Array.from(document.querySelectorAll('iframe')).some((frame) => {
            const source = (frame.src || '').toLowerCase();
            const title = (frame.title || '').toLowerCase();
            return source.includes('recaptcha') || source.includes('hcaptcha') || title.includes('captcha');
          });
          const text = ((document.body && document.body.innerText) || '').toLowerCase();
          return iframe || text.includes('unusual traffic') || text.includes('verify you are human') || text.includes('site verification required');
        })()
    "#;
    match evaluate_javascript(servo, webview, SCRIPT)? {
        JSValue::Boolean(required) => Ok(required),
        other => bail!("unexpected verification-detection result from Servo: {other:?}"),
    }
}

fn capture_frame(
    servo: &Servo,
    webview: &WebView,
    rendering_context: &SoftwareRenderingContext,
    viewport: BrowserViewport,
) -> Result<DynamicImage> {
    // The active WebView may have changed after a popup, tab selection, or a
    // platform compositor callback. In particular, ANGLE requires its
    // context to be current before a software-surface read; otherwise the
    // previous white clear surface can be returned as a valid-looking frame.
    rendering_context
        .make_current()
        .map_err(|error| anyhow::anyhow!("make Servo context current for screenshot: {error:?}"))?;
    let bounds = DeviceIntRect::new(
        DeviceIntPoint::zero(),
        DeviceIntPoint::new(viewport.width as i32, viewport.height as i32),
    );
    // A form submission or link activation can leave the WebView in Loading
    // state for several seconds. `take_screenshot` may deliberately defer its
    // callback until that navigation has a new scene, which previously made
    // the terminal's bounded redraw timeout misclassify a healthy Google
    // navigation as a disconnected browser. Present the last painted frame
    // while loading instead; the app polls again until Servo reports Complete.
    if webview.load_status() != LoadStatus::Complete {
        if let Some(image) = rendering_context.read_to_image(bounds) {
            let image = DynamicImage::ImageRgba8(image);
            // Do not present an untouched ANGLE/SoftwareRenderingContext
            // surface as the page. Fall through to Servo's screenshot API,
            // which requests a scene update and waits for it to complete.
            if !is_probably_unpainted_surface(&image) {
                return Ok(image);
            }
        }
    }
    // A direct read of SoftwareRenderingContext can return the previous
    // `about:blank` frame. Servo's screenshot API requests a scene update and
    // invokes this callback only when WebRender has produced the image.
    for attempt in 0..=UNPAINTED_FRAME_RETRIES {
        let screenshot = Rc::new(RefCell::new(None));
        let callback_screenshot = screenshot.clone();
        webview.take_screenshot(None, move |result| {
            *callback_screenshot.borrow_mut() = Some(result);
        });

        let mut received_screenshot = false;
        for _ in 0..SCREENSHOT_CALLBACK_WAIT_STEPS {
            servo.spin_event_loop();
            webview.paint();
            if let Some(result) = screenshot.borrow_mut().take() {
                received_screenshot = true;
                let image = result
                    .map(DynamicImage::ImageRgba8)
                    .map_err(|error| anyhow::anyhow!("capture Servo screenshot: {error:?}"))?;
                if attempt == UNPAINTED_FRAME_RETRIES || !is_probably_unpainted_surface(&image) {
                    return Ok(image);
                }
                thread::sleep(UNPAINTED_FRAME_RETRY_INTERVAL);
                break;
            }
            thread::sleep(FRAME_WAIT_INTERVAL);
        }
        if !received_screenshot {
            // The current software surface is valid even when Servo has
            // deferred `take_screenshot` during a document transition. A
            // real, painted frame is preferable to declaring a live engine
            // disconnected; the next redraw can obtain the authoritative
            // callback once the page settles.
            if let Some(image) = rendering_context.read_to_image(bounds) {
                let image = DynamicImage::ImageRgba8(image);
                if !is_probably_unpainted_surface(&image) {
                    return Ok(image);
                }
            }
            if attempt < UNPAINTED_FRAME_RETRIES {
                thread::sleep(UNPAINTED_FRAME_RETRY_INTERVAL);
                continue;
            }
            bail!("timed out waiting for a Servo screenshot and no painted surface was available");
        }
    }
    bail!("timed out waiting for a Servo screenshot")
}

/// Return true for the white or transparent, uniform clear surface that an
/// offscreen renderer can expose before it has painted its first document
/// frame. Sampling avoids walking every pixel of a large screenshot on each
/// regular redraw. A genuine mostly-white page normally includes text, rules,
/// or controls at one of these points; even a fully blank white document only
/// incurs the short startup retry before it is shown.
fn is_probably_unpainted_surface(image: &DynamicImage) -> bool {
    let width = image.width();
    let height = image.height();
    if width == 0 || height == 0 {
        return true;
    }
    let reference = image.get_pixel(0, 0);
    let transparent = reference[3] == 0;
    let nearly_white = reference[0] >= 250 && reference[1] >= 250 && reference[2] >= 250;
    if !transparent && !nearly_white {
        return false;
    }
    for y in [0, height / 4, height / 2, height.saturating_sub(1)] {
        for x in [0, width / 4, width / 2, width.saturating_sub(1)] {
            let pixel = image.get_pixel(x, y);
            if transparent {
                if pixel[3] != 0 {
                    return false;
                }
            } else if pixel[3] == 0 || pixel[0] < 250 || pixel[1] < 250 || pixel[2] < 250 {
                return false;
            }
        }
    }
    true
}

fn wait_for_navigation_frame(
    servo: &Servo,
    webview: &WebView,
    rendering_context: &SoftwareRenderingContext,
    viewport: BrowserViewport,
    page_signals: &PageSignals,
    previous_frame_serial: usize,
    expected_url: Option<Url>,
) -> Result<()> {
    rendering_context
        .make_current()
        .map_err(|error| anyhow::anyhow!("make Servo context current for navigation: {error:?}"))?;
    let bounds = DeviceIntRect::new(
        DeviceIntPoint::zero(),
        DeviceIntPoint::new(viewport.width as i32, viewport.height as i32),
    );
    for _ in 0..FRAME_WAIT_STEPS {
        servo.spin_event_loop();
        webview.paint();
        let fresh_frame = page_signals.frame_serial.load(Ordering::Acquire) > previous_frame_serial;
        // `load()` commonly reaches a different final URL: Google selects a
        // regional host, an identity provider may insert a consent page, and
        // an ordinary HTTP URL can upgrade to HTTPS. A fresh frame plus a
        // completed WebView is the reliable navigation boundary; requiring
        // byte-for-byte equality with the requested URL turned successful
        // redirects into a fatal startup timeout and a blank recovery screen.
        // Keep `expected_url` for diagnostic context below, but accept the
        // WebView's final URL whenever it has one.
        let final_url_loaded = webview.url().is_some();
        if webview.load_status() == LoadStatus::Complete
            && fresh_frame
            && final_url_loaded
            && rendering_context.read_to_image(bounds).is_some()
        {
            return Ok(());
        }
        thread::sleep(FRAME_WAIT_INTERVAL);
    }
    bail!(
        "timed out waiting for Servo navigation to render (expected URL: {expected_url:?}, current URL: {:?}, load status: {:?}, frame serial: {})",
        webview.url(),
        webview.load_status(),
        page_signals.frame_serial.load(Ordering::Acquire),
    )
}

fn send_key(webview: &WebView, key: &str) {
    let (named, code, modifiers) = match key {
        "Enter" => (Some(NamedKey::Enter), Code::Enter, Modifiers::empty()),
        "Tab" => (Some(NamedKey::Tab), Code::Tab, Modifiers::empty()),
        "Shift+Tab" => (Some(NamedKey::Tab), Code::Tab, Modifiers::SHIFT),
        "Backspace" => (
            Some(NamedKey::Backspace),
            Code::Backspace,
            Modifiers::empty(),
        ),
        "Delete" => (Some(NamedKey::Delete), Code::Delete, Modifiers::empty()),
        "ArrowLeft" => (
            Some(NamedKey::ArrowLeft),
            Code::ArrowLeft,
            Modifiers::empty(),
        ),
        "ArrowRight" => (
            Some(NamedKey::ArrowRight),
            Code::ArrowRight,
            Modifiers::empty(),
        ),
        "ArrowUp" => (Some(NamedKey::ArrowUp), Code::ArrowUp, Modifiers::empty()),
        "ArrowDown" => (
            Some(NamedKey::ArrowDown),
            Code::ArrowDown,
            Modifiers::empty(),
        ),
        _ => (None, Code::Unidentified, Modifiers::empty()),
    };
    if let Some(named) = named {
        let key = Key::Named(named);
        let event = |state, key| {
            KeyboardEvent::new_without_event(
                state,
                key,
                code,
                Location::Standard,
                modifiers,
                false,
                false,
            )
        };
        webview.notify_input_event(InputEvent::Keyboard(event(KeyState::Down, key.clone())));
        webview.notify_input_event(InputEvent::Keyboard(event(KeyState::Up, key)));
    }
}

/// Queue a default form submission for the currently focused search control.
///
/// This intentionally has no synchronous result: on a successful submission
/// the page may replace its document before Servo invokes the evaluation
/// callback. Restricting it to search-like controls preserves ordinary
/// textarea newline behavior.
fn dispatch_focused_search_submit(webview: &WebView) {
    const SCRIPT: &str = r#"(() => {
      const active = document.activeElement;
      if (!active || active.disabled || active.readOnly || active.isContentEditable) return false;
      const tag = (active.tagName || '').toLowerCase();
      const type = (active.getAttribute('type') || 'text').toLowerCase();
      const role = (active.getAttribute('role') || '').toLowerCase();
      const hint = (active.getAttribute('enterkeyhint') || '').toLowerCase();
      const input = tag === 'input' && !['button', 'checkbox', 'file', 'hidden', 'image', 'radio', 'reset', 'submit'].includes(type);
      const textarea = tag === 'textarea' && (role === 'searchbox' || role === 'combobox' || ['search', 'go', 'send', 'done'].includes(hint));
      if (!input && !textarea) return false;
      const form = active.form || active.closest('form');
      if (!form) return false;
      const submitter = form.querySelector('button[type="submit"]:not([disabled]), input[type="submit"]:not([disabled])');
      try {
        if (typeof form.requestSubmit === 'function') {
          form.requestSubmit(submitter || undefined);
          return true;
        }
        if (submitter && typeof submitter.click === 'function') {
          submitter.click();
          return true;
        }
        if (typeof form.submit === 'function') {
          form.submit();
          return true;
        }
      } catch (_) {}
      return false;
    })()"#;
    webview.evaluate_javascript(SCRIPT, |_| {});
}

fn select_option_direction(key: &str) -> Option<i32> {
    match key {
        "ArrowUp" => Some(-1),
        "ArrowDown" => Some(1),
        _ => None,
    }
}

/// Select an enabled option in the currently focused native `<select>`.
///
/// Servo's software/offscreen embedder has no separate select-popup widget to
/// display above a terminal frame.  The DOM update keeps the control usable
/// through the same keyboard contract as a terminal: arrow keys choose an
/// option and normal `input`/`change` listeners still observe the result.
fn advance_focused_select(servo: &Servo, webview: &WebView, direction: i32) -> Result<bool> {
    let script = format!(
        r#"(() => {{
          const select = document.activeElement;
          if (!select || (select.tagName || '').toLowerCase() !== 'select' || select.disabled) return false;
          const options = Array.from(select.options || []);
          if (!options.length) return false;
          let index = Number(select.selectedIndex);
          if (!Number.isInteger(index)) index = -1;
          for (let step = 0; step < options.length; step += 1) {{
            index = (index + ({direction}) + options.length) % options.length;
            const option = options[index];
            const parent = option && option.parentElement;
            if (!option || option.disabled || (parent && parent.disabled)) continue;
            if (select.selectedIndex !== index) {{
              select.selectedIndex = index;
              select.dispatchEvent(new Event('input', {{ bubbles: true }}));
              select.dispatchEvent(new Event('change', {{ bubbles: true }}));
            }}
            return true;
          }}
          return false;
        }})()"#
    );
    match evaluate_javascript(servo, webview, &script)? {
        JSValue::Boolean(changed) => Ok(changed),
        other => bail!("unexpected focused-select result from Servo: {other:?}"),
    }
}

fn send_text(webview: &WebView, text: String) {
    // The terminal has already completed any platform IME composition before
    // crossterm hands us this Unicode character. Servo's `Ime::Composition`
    // notifications expose composition events to page script, but do *not*
    // perform the focused editor's default text insertion. In contrast, a
    // normal character keydown runs Servo's editing action and inserts text in
    // an input/textarea, including one focused inside a same-origin iframe.
    let key = Key::Character(text);
    let event = |state, key| {
        KeyboardEvent::new_without_event(
            state,
            key,
            Code::Unidentified,
            Location::Standard,
            Modifiers::empty(),
            false,
            false,
        )
    };
    webview.notify_input_event(InputEvent::Keyboard(event(KeyState::Down, key.clone())));
    webview.notify_input_event(InputEvent::Keyboard(event(KeyState::Up, key)));
}

fn browser_viewport(
    size: RenderSize,
    explicit: Option<BrowserViewport>,
    terminal_cell_pixels: Option<TerminalCellPixels>,
) -> BrowserViewport {
    if let Some(viewport) = explicit {
        return viewport;
    }
    if let Some(pixels) = terminal_cell_pixels {
        // SIXEL owns this physical raster. Do not apply the generic minimum:
        // doing so would force the renderer to enlarge a smaller screenshot
        // back into the exact terminal area and make text look oversized.
        return BrowserViewport {
            width: u32::from(size.columns.max(1)) * pixels.width,
            height: u32::from(size.rows.max(1)) * pixels.height,
        };
    }
    BrowserViewport {
        width: size.image_width().max(DEFAULT_BROWSER_VIEWPORT_WIDTH),
        height: size.image_height().max(DEFAULT_BROWSER_VIEWPORT_HEIGHT),
    }
}

fn terminal_cell_to_viewport_point(
    column: u16,
    row: u16,
    size: RenderSize,
    viewport: BrowserViewport,
) -> servo::DevicePoint {
    servo::DevicePoint::new(
        (f32::from(column) + 0.5) / f32::from(size.columns.max(1)) * viewport.width as f32,
        (f32::from(row) + 0.5) / f32::from(size.rows.max(1)) * viewport.height as f32,
    )
}

#[cfg(test)]
mod tests {
    use super::{
        browser_viewport, is_probably_unpainted_surface, terminal_cell_to_viewport_point,
        BrowserViewport,
    };
    use crate::render::{RenderSize, TerminalCellPixels};
    use image::{DynamicImage, Rgba, RgbaImage};

    #[test]
    fn click_coordinates_use_terminal_cell_centers() {
        let point = terminal_cell_to_viewport_point(
            0,
            0,
            RenderSize {
                columns: 100,
                rows: 50,
            },
            BrowserViewport {
                width: 1_000,
                height: 500,
            },
        );
        assert_eq!(point.x, 5.0);
        assert_eq!(point.y, 5.0);
    }

    #[test]
    fn sixel_cell_geometry_sizes_the_dynamic_viewport_without_resampling() {
        let viewport = browser_viewport(
            RenderSize {
                columns: 135,
                rows: 45,
            },
            None,
            Some(TerminalCellPixels {
                width: 8,
                height: 17,
            }),
        );
        assert_eq!(
            viewport,
            BrowserViewport {
                width: 1_080,
                height: 765,
            }
        );
    }

    #[test]
    fn sixel_viewport_does_not_apply_the_generic_minimum_size() {
        assert_eq!(
            browser_viewport(
                RenderSize {
                    columns: 80,
                    rows: 24,
                },
                None,
                Some(TerminalCellPixels {
                    width: 8,
                    height: 17,
                }),
            ),
            BrowserViewport {
                width: 640,
                height: 408,
            }
        );
    }

    #[test]
    fn white_clear_surface_is_retried_but_document_content_is_not() {
        let white =
            DynamicImage::ImageRgba8(RgbaImage::from_pixel(8, 8, Rgba([255, 255, 255, 255])));
        assert!(is_probably_unpainted_surface(&white));

        let transparent = DynamicImage::ImageRgba8(RgbaImage::from_pixel(8, 8, Rgba([0, 0, 0, 0])));
        assert!(is_probably_unpainted_surface(&transparent));

        let mut page = RgbaImage::from_pixel(8, 8, Rgba([255, 255, 255, 255]));
        page.put_pixel(4, 4, Rgba([20, 40, 60, 255]));
        assert!(!is_probably_unpainted_surface(&DynamicImage::ImageRgba8(
            page
        )));
    }
}
