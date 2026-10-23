# fpasobrowser architecture

fpasobrowser aims to be a screenshot-based terminal web viewer with an embedded
browser engine. Users should not need to install Chromium, Chrome, Firefox, or
another browser separately.

The implementation embeds Servo's Rust crate directly into fpasobrowser.
Launching a separate `servo` executable or requiring Chromium/Firefox is not
part of the runtime design. Depending on Servo's feature configuration, it may
still launch managed content processes; "embedded" does not promise that the
OS process count is always one.

The project is motivated by the lack of a native Windows `terminal-browser`
workflow. fpasobrowser is a separate Rust CLI rather than a fpasoterm built-in,
so it can be tested and distributed independently on Windows, macOS, and Linux.

## Components

- `src/main.rs` parses the CLI and starts the app.
- `src/app.rs` coordinates terminal lifecycle, browser capture, rendering, and
  input commands.
- `src/servo_browser.rs` owns the Servo thread, tab-specific WebViews, navigation, input,
  and software-frame capture. The terminal/Tokio side sends requests through
  channels because Servo's rendering context must remain on its owner thread.
- `src/terminal.rs` owns raw mode, alternate-screen mode, terminal sizing, and
  cleanup.
- `src/input.rs` maps terminal key events to high-level commands.
- `src/render/mod.rs` defines `RenderBackend`, backend selection, and shared
  sizing types.
- `src/render/half_block.rs` implements the MVP ANSI truecolor half-block
  backend.
- `src/render/sixel.rs` encodes a 216-color SIXEL graphics stream.
- `src/render/kitty.rs` is a Kitty Graphics Protocol backend.

## Rendering model

The browser captures a screenshot and the selected backend presents it in the
terminal. The half-block backend maps two image rows to one terminal row by
using the upper half-block character (`▀`):

- foreground color = upper pixel
- background color = lower pixel

The Kitty backend sends the PNG screenshot through Kitty Graphics Protocol and
places it into terminal cells. It has much higher fidelity than half-block, but
requires a compatible terminal.

The SIXEL backend quantizes the image to a 6×6×6 color cube and writes a SIXEL
DCS stream. It defaults to an 8×17-pixel terminal-cell estimate matching
fpasoterm's Linux/ChromeOS grid because there is no portable terminal query
for this geometry. `--sixel-cell-size WIDTHxHEIGHT` (or
`FPASOBROWSER_SIXEL_CELL_SIZE`) overrides that estimate for a different SIXEL
terminal. The same selected geometry sizes Servo's dynamic viewport, avoiding
a second screenshot enlargement during SIXEL presentation. This keeps
fpasoterm's three UI rows below the image visible and aligns mouse coordinates
with the image, but exact sizing still needs per-terminal refinement.

## Backend selection

The omitted CLI backend is `kitty`. Explicit `--backend auto` currently selects
`half-block`. The backend values are:

- `half-block`: explicit ANSI truecolor fallback.
- `sixel`: 216-color SIXEL image backend for SIXEL-capable terminals.
- `kitty`: default Kitty Graphics Protocol backend.

The `RenderBackend` trait is the boundary for future protocol-specific
implementations. `app.rs` should select a backend and call the trait without
knowing image transport details.

## Portability model

fpasobrowser targets native Windows, macOS, and Linux. It must not require WSL on
Windows. Embedding Servo removes the need to discover a user-installed browser
executable. It does not eliminate native build/runtime dependencies for Servo,
rendering, fonts, TLS, or platform integration, nor does it promise a small
single binary.

## Servo implementation and remaining work

- Servo's official `SoftwareRenderingContext` provides RGBA frame readback to
  the existing renderer. The Servo-owned thread integrates `spin_event_loop`
  with request channels from the crossterm/Tokio loop.
- The current MVP ports navigation, reload, history commands, scrolling,
  pointer events, basic IME/key input, numbered link hints, and multiple
  terminal tabs. Tabs made with `t` receive an independent rendering context.
  The engine reports an atomic active-tab snapshot (title, URL, history
  availability, and load state) to the TUI so the activity line and history
  toolbar reflect one WebView state rather than separately queried values. The
  TUI uses the load state to continue redraws after a navigation-triggering
  action, and link-hint activation reports the resulting selected tab when it
  promotes a popup.
- `target="_blank"` and `window.open()` requests construct their auxiliary
  WebView in Servo's delegate callback, which fulfils the engine's synchronous
  response contract. The completed WebView is then promoted into a selected
  terminal tab by the engine loop. Every terminal tab, including an auxiliary
  popup, owns an independent software rendering context so its screenshot
  cannot show another tab's frame. The focus inspection/marker path descends
  into same-origin iframes, while cross-origin frames are deliberately left to
  Servo's native input dispatch and are not inspected with page JavaScript.
  Remaining work includes durable site data, fuller popup compatibility, and
  lifecycle recovery.
- Servo is evolving. Validate site compatibility, iframes, TLS,
  GPU/software-rendering paths, and platform build requirements; do not assume
  parity with Chromium.
- Do not use the engine change to evade CAPTCHA or anti-abuse checks. Switching
  engines cannot guarantee that such challenges will not appear.

## Extension points

Future renderers should implement the `RenderBackend` trait and avoid leaking
protocol-specific behavior into `app.rs`.

Planned backends:

- Kitty graphics protocol refinements
- iTerm2 image protocol

Browser interaction is layered through `servo_browser.rs` so terminal input
remains independent from Servo's embedding API.
