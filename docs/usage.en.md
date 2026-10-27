# fpasobrowser usage

## Run

The default build embeds Servo. No Chromium, Chrome, Firefox, or separate
`servo` executable is started or required at runtime.

```sh
./bin/fpasobrowser
```

Without a URL, fpasobrowser opens its built-in start page. Press `g` or click
the `[URL]` toolbar control to enter an address, or pass it directly:

```sh
./bin/fpasobrowser https://example.com/
```

## Windows native: source-build prerequisite

The release binary does not require Chromium, Firefox, a separate Servo
executable, or Python. A source build is different: Servo's Stylo CSS build
step needs Python 3 while Cargo compiles it. Python is a build prerequisite,
not a browser runtime dependency.

Install Rust stable and Python 3, reopen PowerShell, then use the checkout
launcher:

```powershell
winget install Rustlang.Rustup
winget install Python.Python.3.12
cd C:\path\to\fpasobrowser
./bin/fpasobrowser.ps1 --version
```

The launcher detects `python.exe` or the Windows `py` launcher and sets
`PYTHON3` for Stylo. For a direct `cargo build`, use an absolute Python path if
it is not on `PATH`:

```powershell
$env:PYTHON3 = 'C:\path\to\python.exe'
cargo build --locked
```

Windows defaults to `half-block`: normal PowerShell and Windows Terminal do
not display Kitty Graphics. In fpasoterm, request the high-resolution backend
explicitly:

```powershell
./bin/fpasobrowser.ps1 --backend kitty
```

Windows uses Servo's ANGLE/Direct3D fallback instead of WGL so it can render
in Remote Desktop sessions. Keep `libEGL.dll` and `libGLESv2.dll` next to
`fpasobrowser.exe`; the source launcher and release archive stage them
automatically.

## Linux / ChromeOS Linux: first build and verification

Build the Servo-containing binary once, then use the generated binary directly
for interactive checks. This avoids Cargo's build check on every attempt.

```sh
cd /home/oyoguhito/github/oyoguhito/fpasobrowser
./bin/fpasobrowser --version
```

For a source checkout, `./bin/fpasobrowser` is the normal entry point. It
builds the debug binary only when missing or out of date, then executes the
binary directly. You do not need to use `cargo run` for every launch. On
Windows PowerShell, use `./bin/fpasobrowser.ps1 --version`.

First perform a frame check without entering terminal raw mode. The bundled
local page should produce a PNG with the *fpasobrowser interaction test*
heading, an input field, and yellow buttons. That confirms embedded Servo
rendering and frame capture without an external browser.

```sh
target/debug/fpasobrowser --capture-only --viewport 1200x800 \
  --dump-screenshot /tmp/fpasobrowser-check.png \
  file://"$PWD"/examples/interaction.html
file /tmp/fpasobrowser-check.png
```

Then test the actual terminal UI. The omitted backend is `kitty` on Linux and
macOS, and portable `half-block` on Windows. In fpasoterm or another terminal
that implements Kitty Graphics Protocol, request the higher-fidelity Kitty
backend explicitly with `--backend kitty`.

```sh
./bin/fpasobrowser \
  file://"$PWD"/examples/interaction.html
```

Press `?` for controls, press `i` before entering page text, and use `q` or
`Ctrl+Q` to exit. A page-provided focus ring is kept as the single indicator;
otherwise fpasobrowser adds a cyan frame. Press `Esc` from input mode to blur
the page field and return to normal mode; printable keys in
normal mode are not forwarded to a stale page focus. The PNG check confirms
Servo rendering; the interactive check separately confirms the terminal
renderer and input forwarding.

After installing a built binary, use:

```sh
fpasobrowser https://example.com/
```

`file://` pages are useful for deterministic interaction checks:

```sh
fpasobrowser \
  file:///absolute/path/to/fpasobrowser/examples/interaction.html
```

The local workspace example is:

```sh
cargo run -- \
  file:///home/oyoguhito/github/oyoguhito/fpasobrowser/examples/interaction.html
```

## Renderer backends

```sh
fpasobrowser --backend auto https://example.com/
fpasobrowser --backend half-block https://example.com/
fpasobrowser --backend sixel https://example.com/
fpasobrowser --backend sixel --sixel-cell-size 8x17 https://example.com/
fpasobrowser https://example.com/
```

The omitted default is `kitty` on Linux/macOS and `half-block` on Windows.
`kitty` sends the rendered PNG via Kitty Graphics Protocol; in fpasoterm on
Windows, request it explicitly after verifying that the terminal path supports
Kitty images. Explicit `auto` also currently selects `half-block`, which works
in modern truecolor terminals but is visibly lower resolution. `sixel` uses a
216-color SIXEL stream for SIXEL-capable terminals.
Its default 8×17-pixel cell estimate matches fpasoterm's Linux/ChromeOS grid,
and also sizes Servo's dynamic viewport. That avoids upscaling the screenshot
again in the SIXEL renderer while preserving status/tab/toolbar rows and
aligning mouse coordinates with the image. On a terminal with different
physical cell dimensions, specify `--sixel-cell-size WIDTHxHEIGHT` (or set
`FPASOBROWSER_SIXEL_CELL_SIZE`); the toolbar displays the active geometry, for
example `sixel 8x17`. The option requires `--backend sixel`.

## Running inside a Herdr pane

fpasobrowser needs no Herdr plugin and can be launched as a normal process in
a Herdr pane:

```sh
./bin/fpasobrowser
```

The current Herdr source enables Kitty Graphics by default and implements
mouse-report forwarding for pane applications. When the outer terminal also
supports Kitty Graphics, use `--backend kitty` (it is already the Linux/macOS
default; Windows keeps the portable half-block default). To explicitly
configure it, set the following and restart the Herdr server
or reattach the client:

```toml
[terminal]
kitty_graphics = true
```

An image that is clipped, translated into another pane, or does not receive
mouse input is a Herdr pane-graphics/input-routing issue, not an external
browser dependency in fpasobrowser. `--backend half-block` is a lower-fidelity
diagnostic fallback that does not use Kitty Graphics. SIXEL is not yet a
validated transport path through Herdr.

The three textual rows below a default fpasobrowser page are its local
activity/status line, tab strip, and mouse toolbar. They are not an additional
rendered page or a Herdr Kitty Graphics translation defect. If a shorter
terminal UI is preferred in a Herdr pane, use:

```sh
./bin/fpasobrowser --ui compact
```

`--ui compact` reserves only the activity/status row, returning two rows to
the page. Keyboard tab/navigation controls remain available; the clickable
tab strip and mouse toolbar are intentionally not drawn.

Use `--ui page` when no text row should remain below the page. It suppresses
normal status feedback while retaining one invisible guard row: the current
fpasoterm/Herdr Kitty presentation clips or bottom-anchors an image placement
that consumes every terminal row. Page pointer, wheel, and keyboard interaction
remains enabled.

To diagnose a Herdr mouse problem, launch with `--ui compact` temporarily.
`hover …` in the activity line proves mouse movement already reaches
fpasobrowser. A left-button press, or an orphaned left drag/release reported by
a nested terminal, is normalized to one page click. If a click does not show
`clicked …` and has no page effect after this fallback, capture the Herdr
revision, outer terminal, pane layout, and `kitty_graphics` configuration for
an upstream pane mouse-routing issue. It is separate from Kitty image clipping
or translation.

Hover inspection updates only the activity row. It intentionally does not
schedule a page screenshot replacement, so simply moving the pointer must not
make a Kitty-rendered page blink.

## Diagnostics and frame capture

```sh
fpasobrowser --version
fpasobrowser -v
fpasobrowser --version-info
fpasobrowser --debug --backend kitty https://example.com/
fpasobrowser --capture-only --dump-screenshot page.png https://example.com/
fpasobrowser --viewport 1366x768 https://example.com/
```

`--capture-only` requires `--dump-screenshot` and exits after one PNG. The
interactive `?` panel also displays version and build information.

## Keys and mouse

| Key / input | Action |
| --- | --- |
| `q` in normal mode, `Ctrl+Q` in any mode | Quit |
| `Esc` | In input mode, clear page focus and return to normal mode. In normal mode on a later terminal tab, return to the preceding tab without closing it; never quit |
| `?` | Show or hide the in-terminal help panel |
| `r`, `R` | Reload / redraw |
| `j`, `Down` | Scroll down |
| `k`, `Up` | Scroll up |
| `g` | Enter a URL and navigate the current WebView |
| `b`, `B`, `[` / `F`, `]` or `Alt+Left` / `Alt+Right` | Back / forward in Servo history; lower-case `f` remains link hints |
| `o`, `v`, `V` | Open the current HTTP(S) URL in the operating system's normal browser; local `file:` pages remain in fpasobrowser |
| `i` | Enter browser-input mode |
| `Esc` in input mode | Blur the page field and return to normal mode |
| `Tab`, `Shift+Tab`, `Enter` in input mode | Move focus / activate the focused page element |
| `t`, `x`, `{`, `}`, `Ctrl+Tab`, `Ctrl+Shift+Tab` | New tab, close active tab, previous tab, next tab |
| `f` | Show numbered link/action hints, then enter the hint number |
| Left click | Forward a pointer click to the WebView |
| Mouse movement | Kitty/half-block: forward a rate-limited pointer move and show hovered link URL. SIXEL: suppress cosmetic hover forwarding so queued moves do not delay clicks. |
| Mouse wheel | Forward page scrolling to the WebView |

Normal mode reserves commands such as `q`, `r`, `g`, and `f` for
fpasobrowser, and printable keys are not forwarded to the page. Press `i` to
focus the first visible editable control before typing into a page form, or
click an editable control to enter input mode. If Servo reports only the parent
iframe after a click, fpasobrowser retains input mode so the child control can
receive text and focus traversal; press `Esc` to return to normal mode. The
focus search also supports a same-origin iframe. When the page itself provides
a visible focus ring, fpasobrowser leaves that single ring in place; otherwise
it adds a cyan fallback frame. Cross-origin frames, including CAPTCHA
providers, are not inspected by fpasobrowser page script; their input behavior
depends on Servo's native frame handling. `Tab` and `Shift+Tab` move focus
while in input mode.
Servo tabs have independent WebViews and software rendering contexts,
including auxiliary popup tabs. Closing the last tab intentionally keeps it
open so the terminal UI does not disappear.
When a page requests a new window through `target="_blank"` or `window.open()`,
fpasobrowser promotes the request into a new selected terminal tab. The
activity line reports the promotion and selected tab number; use `{` / `}` to
return to the prior tab and `x` to close the popup tab. After a click, form
submission, or hint activation, the viewer keeps refreshing while Servo reports
that the active tab is loading rather than stopping after a fixed short redraw
burst.

Three lines below the page are reserved for terminal UI: after an operation,
the upper activity line starts with its feedback (for example `reloaded`,
`went back`, or `help shown`) so it remains visible on narrow terminals. When
idle it starts with the active tab number, `loading` / `ready` state, page
title, and current URL. The middle line is a visible tab strip. The
history labels on the bottom toolbar are
`(B)` / `(F)` when unavailable and `[B]` / `[F]` when a backward / forward
entry exists. Click a visible tab label, or use `{` / `}` or `Ctrl+Tab` /
`Ctrl+Shift+Tab`, to switch tabs in the same terminal pane. Each tab has an
explicit `×` mark that can be clicked to close it; `x` closes the active tab
and the final tab is kept open. The bottom toolbar offers mouse access to
reload, history, address entry, new-tab creation, tab selection,
external-browser handoff, and help.

Use `--ui compact` (or `FPASOBROWSER_UI=compact`) to retain only the activity
line and return two terminal rows to the page. In this mode keyboard controls
still work, while the visible tab strip and mouse toolbar are omitted.
`--ui page` removes the final activity line as well. It keeps one invisible
guard row for reliable Kitty placement in nested terminals, at the cost of
suppressing ordinary status feedback.

While Servo is processing a request, the activity line temporarily shows
`[working]` with the operation name. When the active page remains in its load
state, `[loading |]`, `[loading /]`, `[loading -]`, and `[loading \]` advance
on each scheduled frame check. This distinguishes a slow page from an idle
viewer. `q` works in normal mode and `Ctrl+Q` works in every mode; a quit
request received during an in-flight Servo call is handled once that serialized
call returns, then the status line shows the safe shutdown wait.

## CAPTCHA and site verification

Sites can show CAPTCHA or unusual-traffic pages based on their own policies.
fpasobrowser does not evade or automatically solve them. Use `o` or `v` to
open the current URL in a regular browser when a site allows manual
verification. That regular browser is a separate session; its cookies and
verification result are not automatically imported into Servo.
For local `file:` test pages, `o` / `v` intentionally show an in-viewer notice
instead of following an OS association, which could otherwise launch a terminal
application rather than a browser.

## Requirements and platforms

fpasobrowser targets native Windows, macOS, and Linux; Windows does not depend
on WSL. Building Servo for the first time is large and needs its Rust/native
graphics, font, TLS, and linker prerequisites. A distributed fpasobrowser
binary embeds the engine, so an end user does not install a separate web
browser.

For high-fidelity terminal output, use a terminal with Kitty Graphics Protocol
support. Windows Terminal does not currently provide that protocol, so native
Windows uses the truecolor half-block backend until an additional image
backend is implemented.

On Linux and ChromeOS Linux, the repository's `.cargo/config.toml` limits Cargo
to one job and uses installed `/usr/bin/mold` through `clang`. This avoids
running several large Servo linkers concurrently on memory-constrained systems.
The setting does not apply to Windows or macOS.

## fpasoterm relationship

fpasobrowser is independent of fpasoterm. A future fpasoterm plugin may launch
it as an external CLI, but it remains useful outside fpasoterm.
