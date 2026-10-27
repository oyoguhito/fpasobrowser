# fpasobrowser

`fpasobrowser` is an experimental terminal web viewer.

It is **not** a text browser like `lynx` or `w3m`. It embeds the Servo browser
engine, renders the requested page in-process, reads back its frame, and draws
that image in the terminal. It provides an ANSI truecolor half-block fallback
renderer plus a Kitty Graphics Protocol backend for terminals that support real
image transport.

The project exists because `terminal-browser` is not available as a native
Windows terminal workflow. fpasobrowser is an independent Rust CLI that targets
Windows native first, while also supporting macOS and Linux. It is intentionally
separate from fpasoterm, but keeps the command-line interface simple enough that
a future fpasoterm plugin can invoke it.

## Goals

- Native Windows, macOS, and Linux support.
- No WSL dependency on Windows.
- Screenshot-based web rendering through embedded Servo.
- Terminal image rendering with a truecolor fallback when Kitty, SIXEL, or
  iTerm2 image support is unavailable.
- A renderer trait so richer backends can be added later.

## MVP usage

```sh
fpasobrowser
```

Without a URL, fpasobrowser shows its built-in start page. Press `g` or click
`[URL]` to enter an address, or pass one directly:

```sh
fpasobrowser https://example.com/
```

## Quick start and verification (Linux / ChromeOS Linux)

Build Servo once, then run the generated binary directly. Running the binary
avoids repeating Cargo's build check on every interactive attempt.

```sh
cd /home/oyoguhito/github/oyoguhito/fpasobrowser
./bin/fpasobrowser --version
```

`bin/fpasobrowser` is the recommended source-checkout launcher. It builds
`target/debug/fpasobrowser` only when it is missing or Rust/Cargo inputs are
newer, then directly executes it. Subsequent launches do not invoke `cargo
run`. On Windows PowerShell use `./bin/fpasobrowser.ps1 --version` instead.

## Windows native: source-build prerequisite

The released Windows binary needs neither an external browser nor Python. A
**source build** embeds Servo, whose Stylo CSS build step requires Python 3 at
build time. It is not a runtime browser dependency.

Install Rust stable and Python 3, reopen PowerShell so PATH is refreshed, then
use the launcher:

```powershell
winget install Rustlang.Rustup
winget install Python.Python.3.12
cd C:\path\to\fpasobrowser
./bin/fpasobrowser.ps1 --version
```

The launcher locates `python.exe` or the Windows `py` launcher and supplies
the required `PYTHON3` value automatically. If Python is installed in a custom
location, set it explicitly before a direct Cargo build:

```powershell
$env:PYTHON3 = 'C:\path\to\python.exe'
cargo build --locked
```

Windows defaults to the portable `half-block` backend because the normal
PowerShell and Windows Terminal hosts do not display Kitty Graphics. In
fpasoterm, select the high-resolution image transport explicitly:

```powershell
./bin/fpasobrowser.ps1 --backend kitty
```

The Windows build uses Servo's ANGLE/Direct3D software fallback instead of
legacy WGL so it can start in Remote Desktop sessions. Keep `libEGL.dll` and
`libGLESv2.dll` beside `fpasobrowser.exe`; the source launcher and release
archive do this automatically.

First verify the embedded engine without entering terminal raw mode. This writes
a deterministic image from the bundled local page; opening the PNG should show
the *fpasobrowser interaction test* heading, input field, and yellow buttons.

```sh
target/debug/fpasobrowser --capture-only --viewport 1200x800 \
  --dump-screenshot /tmp/fpasobrowser-check.png \
  file://"$PWD"/examples/interaction.html
file /tmp/fpasobrowser-check.png
```

Then test the terminal UI in an interactive terminal. The omitted backend is
`kitty` on Linux/macOS and portable `half-block` on Windows. In fpasoterm or
another verified Kitty Graphics Protocol terminal, request high-resolution
output explicitly with `--backend kitty`.

```sh
./bin/fpasobrowser \
  file://"$PWD"/examples/interaction.html
```

Press `?` to show controls, use `i` before entering page text, and use `q` or
`Ctrl+Q` to exit. When a page supplies its own focus ring, it remains the
single focus indicator; otherwise fpasobrowser adds a cyan frame. Press `Esc`
from input mode to blur that element and return to normal mode;
printable keys in normal mode are never sent to a stale page field. The PNG
check confirms Servo rendering; the interactive step separately confirms the
selected terminal image backend and input forwarding.

## Embedded Servo engine (no external browser)

The default build starts an embedded Servo WebView. It does not launch or
require Chromium, Chrome, Firefox, or a separate `servo` executable.

```sh
cargo run -- \
  file:///absolute/path/to/page.html
```

The current Servo path supports URL navigation, reload, back/forward history,
screenshots, scrolling, pointer clicks, keyboard/IME input, multiple terminal
tabs, numbered link hints, and first-editable-field focus. It does not use an
external browser executable. See [`docs/servo-poc.en.md`](docs/servo-poc.en.md)
for the standalone frame probe and compatibility limits.

## Herdr

fpasobrowser can run as an ordinary process in a Herdr pane; it does not need a
plugin or a separate browser process. Current Herdr source enables Kitty
Graphics by default and forwards pane mouse reporting, so the normal command
to try in a Herdr pane is:

```sh
./bin/fpasobrowser
```

On Linux and macOS, the default `kitty` backend requires both the Herdr
server/client and the outer terminal to handle Kitty Graphics correctly. On
Windows, the omitted backend is the portable `half-block`; request `--backend
kitty` only after confirming that the fpasoterm/Herdr path renders Kitty
images. Herdr configuration can explicitly enable it with `[terminal]
kitty_graphics = true`; changing that setting requires a Herdr restart or
client reattach. If an image is clipped,
translated into another pane, or mouse input does not reach the viewer, that is
a Herdr pane-graphics/input-routing issue rather than a Chromium dependency in
fpasobrowser. As a diagnostic fallback, `--backend half-block` needs no Kitty
Graphics transport but has intentionally lower fidelity. SIXEL is not a
validated Herdr transport path yet.

The three text rows below the page in the default view are **fpasobrowser's
own terminal UI** (activity/status, tab strip, and mouse toolbar). They are not
a second page image or a Herdr placement/clipping fault. To give two of those
rows back to the rendered page, use compact UI mode; it retains the activity
line and keyboard controls but hides the in-viewer tab strip and mouse toolbar:

```sh
./bin/fpasobrowser --ui compact
```

For a page-only pane, use `--ui page`. It hides every status/tab/toolbar text
line, so it is appropriate when the remaining compact activity line is
unwanted. It retains one invisible guard row: current fpasoterm/Herdr Kitty
presentation clips or bottom-anchors a placement that consumes every terminal
row. Page clicks, wheel scrolling, and keyboard controls still work; feedback
such as loading and recovery status is intentionally not drawn while the page
is healthy.

For a Herdr mouse diagnosis, temporarily use `--ui compact`: moving over a
page should update its activity line to `hover …`, and a left click should
briefly report `clicked …` before the page redraws. The hover report proves
that Herdr is already forwarding pane mouse reports. Hover updates alter only
that activity line; they intentionally do not request a page-image redraw, so
ordinary pointer movement must not make a Kitty page blink. fpasobrowser also accepts
a left drag or release as the first event of a click gesture, because a nested
terminal may begin forwarding after the physical button press. If neither the
click status nor page action occurs after that fallback, record the Herdr
revision, outer terminal, pane layout, and `kitty_graphics` setting for a
Herdr mouse-routing issue; it is not a Kitty image placement issue.

Check the installed or built version:

```sh
fpasobrowser --version
fpasobrowser -v
fpasobrowser --version-info
```

The interactive `?` help panel also shows the version/build information so you
can confirm which binary is running.

For interaction testing without bot protection, use the bundled local page:

```sh
fpasobrowser file:///home/oyoguhito/github/oyoguhito/fpasobrowser/examples/interaction.html
```

The omitted renderer backend is `kitty`, for terminal-browser-like image
fidelity in fpasoterm or another Kitty Graphics Protocol compatible terminal.
Choose another backend explicitly when needed:

```sh
fpasobrowser https://example.com/
fpasobrowser --backend auto https://example.com/
fpasobrowser --backend half-block https://example.com/
fpasobrowser --backend sixel https://example.com/
fpasobrowser --backend sixel --sixel-cell-size 8x17 https://example.com/
```

`--backend auto` intentionally continues to select `half-block` as a portable
fallback. `FPASOBROWSER_BACKEND` can override the omitted default.

For an automated frame capture without entering the terminal UI:

```sh
fpasobrowser --capture-only --dump-screenshot page.png https://example.com/
```

Site verification remains controlled by the site; press `o` to hand the current
URL to the OS default browser when manual verification is necessary.

Controls:

- Three lines below the page are reserved for terminal UI. The upper activity
  line shows action feedback first after an operation (for example `reloaded`,
  `went back`, or `help shown`) so it remains visible on narrow terminals; at
  idle it starts with the active tab number, load state (`loading` or `ready`),
  title, and current URL. The middle line is
  a visible in-viewer tab strip; the
  bottom line starts with a mouse toolbar:
  `[B] [F] [R] [URL] [+Tab] [<Tab] [Tab>] [V] [?]`. History controls are shown
  as `(B)` / `(F)` when the active tab has no backward / forward entry, and as
  `[B]` / `[F]` when the corresponding action is available.
  After a click, form submission, or link-hint activation, fpasobrowser keeps
  refreshing while Servo reports that the active tab is loading; `[loading |]`
  advances on those refreshes so a slow page is visibly still progressing. A
  temporary `[working]` message identifies an in-flight Servo operation such as
  a click, screenshot, or safe shutdown. This avoids a
  fixed short redraw window leaving an older frame on screen. A link hint that
  opens a new window reports the selected terminal-tab number immediately.
  Click a tab label in the tab strip, or use `{` / `}`, to switch pages in the
  same terminal pane. `Ctrl+Tab` / `Ctrl+Shift+Tab` are also supported when
  the terminal reports them. Click a tab's explicit `×` mark (or press `x`) to
  close it; the final tab is kept open. Click toolbar buttons for back,
  forward, reload, URL entry, new-tab creation, verification handoff, and
  help.
  Use `--ui compact` (or `FPASOBROWSER_UI=compact`) when page height matters:
  it reserves only the activity/status line. Keyboard tab and navigation
  commands remain available, while the clickable tab strip and toolbar are
  intentionally hidden.
  Use `--ui page` to hide all text UI; it uses one invisible guard row for
  reliable Kitty placement in nested terminals, with status feedback
  intentionally suppressed.
- `q` in normal mode or `Ctrl+Q` in any mode: quit. During a Servo request the
  quit key is handled after that serialized request; `[working] closing…` and
  then `still closing Servo…` make a safe engine join visible instead of
  looking like a frozen terminal.
- `Esc`: leave input mode, clear page focus, and return to normal mode. It
  never quits the viewer.
- `r` / `R`: reload current URL
- `j` / `Down`: scroll down
- `k` / `Up`: scroll up
- `o`, `v` / `V`: open the current HTTP(S) URL in the OS default browser. Local `file:`
  pages remain in fpasobrowser so OS file associations cannot start a terminal
  application instead of a browser.
- `i`: browser input mode; typed characters, arrows, Enter, Tab, and Backspace
  are sent to the current page. It searches the visible same-origin document
  tree, including embedded iframes, for the first editable target. A page's
  own focus ring is used when it provides one; otherwise fpasobrowser adds a
  cyan frame. `Esc` removes focus and returns to normal mode.
- `g`: prompt for a new URL and navigate the current Servo WebView
- `b` / `B` / `[` and `F` / `]`: go back / forward in Servo history. Lower-case
  `f` remains the link-hint command.
- `t`: prompt for a URL and open it in a new Servo tab; `x` closes the active
  tab (the final tab is kept open); `{` / `}` or `Ctrl+Tab` / `Ctrl+Shift+Tab`
  select the previous / next tab
- Page requests for a new window (`target="_blank"` or `window.open()`) are
  promoted to a new, selected terminal tab. The activity line confirms this;
  use `{` / `}` to return and `x` to close it.
- `f`: add numbered link/action hints, then enter a hint number to activate it
- Mouse left click: forwarded to the embedded WebView. Editable targets enter
  input mode automatically; when Servo exposes only an iframe as the focused
  parent-side target, input mode is retained so the child control keeps
  receiving text, Tab, and Shift+Tab. Press `Esc` to return to normal mode.
  `i` focuses the first visible editable target. A non-editable click outside
  an iframe returns to normal mode, where printable keys remain fpasobrowser
  commands rather than page input.
- Mouse movement: forwarded with rate limiting for Kitty and half-block.
  SIXEL intentionally suppresses cosmetic hover forwarding so queued movement
  events cannot delay a real click; left click and wheel scrolling remain
  forwarded.
- Mouse wheel: forwarded as page scroll
- `?`: show or hide the in-terminal help panel

## Current implementation

- CLI parsing with `clap`
- Terminal raw mode and input with `crossterm`
- Embedded Servo WebView frame capture, navigation, scrolling, keyboard, and
  pointer interaction
- PNG/JPEG decode and resizing with `image`
- URL validation with `url`
- OS default browser handoff with `open`
- Default renderer: `kitty` on Linux/macOS; portable ANSI truecolor
  `half-block` on Windows
- Explicit fallback renderers: ANSI truecolor + upper half block (`--backend half-block`)
  and SIXEL (`--backend sixel`)

## Render backends

| Backend | Status | Characteristics |
| --- | --- | --- |
| `auto` | Available | Explicit portable choice; currently resolves to `half-block`. Later it can probe terminal capabilities. |
| `half-block` | Available | ANSI truecolor renderer using `▀`. Works in ordinary modern terminals and does not need Kitty/SIXEL/iTerm2 image protocols. Lower fidelity than real image protocols. |
| `sixel` | Available | Encodes the screenshot as a 216-color SIXEL image. Its default 8×17-pixel cell estimate matches fpasoterm's Linux/ChromeOS terminal grid. Servo's dynamic viewport uses exactly the same geometry, so the screenshot is not enlarged again during SIXEL presentation and mouse coordinates remain aligned while preserving the three UI lines. Use `--sixel-cell-size WIDTHxHEIGHT` (or `FPASOBROWSER_SIXEL_CELL_SIZE`) when another SIXEL terminal uses different physical cell dimensions; the toolbar shows the active value. Full-frame updates are deduplicated, hover redraw is suppressed, and loading is polled at a lower rate so click navigation finishes without idle flicker. |
| `kitty` | Available; default on Linux/macOS | Sends PNG screenshots through Kitty Graphics Protocol and places them in terminal cells. On Windows, request it explicitly in fpasoterm/another verified Kitty-compatible terminal. |

## Documentation

- [Usage](docs/usage.en.md) / [使い方](docs/usage.ja.md)
- [Architecture](docs/architecture.en.md) / [アーキテクチャ](docs/architecture.ja.md)
- [Processing flows](docs/flow.en.md) / [処理フロー](docs/flow.ja.md)
- [Testing](docs/testing.en.md) / [テスト](docs/testing.ja.md)

## Current limits

- Rendering is screenshot-based, so selectable text and semantic links are not
  exposed yet.
- Scrolling currently re-captures the page after moving `window.scrollY`.
- Image quality with `half-block` is intentionally limited. The omitted
  backend is `kitty` on Linux/macOS and `half-block` on Windows; Windows users
  can explicitly select `kitty` in a verified compatible terminal.
- Windows Terminal does not currently support Kitty Graphics Protocol or SIXEL,
  so native Windows Terminal uses `half-block`. SIXEL-capable Windows terminals
  can use `--backend sixel`.
- Login automation is intentionally out of scope.
- Video, audio, and browser extensions are intentionally out of scope.
- Same-origin iframe input and focus markers are supported. Cross-origin
  frames (including CAPTCHA providers) cannot be inspected or scripted by the
  containing page; their compatibility depends on Servo's native input path.
- Servo is still an evolving browser engine. Compatibility with all sites,
  complex frames, and authentication flows is not yet equivalent to a mature
  desktop browser.

## Future work

- Better Windows-native high-fidelity image backend
- iTerm2 image renderer backend
- More robust link selection
- More robust mouse click forwarding
- Better viewport/device-scale handling
- Durable site-data profiles and fuller popup/new-window compatibility
- fpasoterm plugin that invokes `fpasobrowser`

## Development

This repository uses three Cargo build jobs in `.cargo/config.toml`. Embedded
Servo has large native artifacts, but WebRender's shader generator needs a
spare Cargo jobserver token during a cold build; one or two jobs can stall.
On Linux and ChromeOS Linux the same project-local configuration uses an
installed `/usr/bin/mold` through `clang`; Windows and macOS continue using
their native toolchains. Renderer tests explicitly disable the default Servo
feature because they do not need a WebView.

```sh
cargo fmt
cargo clippy -- -D warnings
cargo test -j 1 --no-default-features --lib --tests
```
