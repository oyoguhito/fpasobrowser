# Testing

The default checks are:

```sh
cargo fmt --check
cargo clippy -- -D warnings
cargo test -j 1 --no-default-features --lib --tests
```

The repository sets `build.jobs = 3` in `.cargo/config.toml`. Three jobs keep
memory bounded while leaving WebRender's shader-generation build script a
Cargo jobserver token during a cold build; forcing one or two jobs can stall
that script after the cache has been invalidated. On Linux and ChromeOS Linux,
the same file invokes the installed `/usr/bin/mold` through `clang`; Windows
and macOS keep their native toolchains.

The current unit/integration tests focus on deterministic pieces that do not
launch a full Servo WebView:

- renderer sizing
- ANSI truecolor half-block output
- command/input-independent data types

Manual browser tests are still required for:

- Servo build/runtime prerequisites on each OS
- `fpasobrowser <url>` startup
- screenshot capture
- terminal resize behavior
- `q`, `r`, `j`, `k`, arrow keys, and `o`

## Local interaction regression check

After using a numbered hint for a link that opens a new window, confirm that
the activity line reports the popup terminal tab and that the tab strip selects
it. On slow navigation, confirm that `loading` remains visible and frames keep
refreshing until it changes to `ready`.

Use the bundled page instead of a public site so bot protection and network
latency cannot hide an input regression:

```sh
target/debug/fpasobrowser --backend kitty \
  file://"$PWD"/examples/interaction.html
```

Check the following in one session:

1. Press `i`. The **Input test** field has a cyan focus frame.
2. Type `focus-check`, then press `Enter`. The result changes to
   `Entered: focus-check`.
3. Press `Esc`. The cyan frame disappears and the viewer remains open. Type
   ordinary letters; the Input test field must not change in normal mode. Only
   `q` in normal mode or `Ctrl+Q` closes the viewer.
4. Press `i`, then use `Tab` and `Shift+Tab`. The cyan frame follows the active
   page control. Press `Enter` on **Show typed text** to activate it.
5. Click a non-editable card or scroll with the wheel. The viewer returns to
   normal mode; `r`, `?`, and `q` continue to operate as viewer commands.
6. Move the pointer over **Go section A** without clicking. The status line
   shows its `#section-a` URL before activation. Mouse movement is rate-limited
   to keep screenshots and keyboard handling responsive.
7. Click **Open popup terminal tab**. The activity line reports a popup tab and
   the visible tab strip gains a selected `2*` tab; the **Popup terminal tab**
   page (green heading) is shown in the same terminal pane, not the interaction
   page. The clicked button on the original tab must not retain a cyan focus
   outline. Click the first tab label (or press `{`) to return, click the popup
   tab label (or press `}`) to select it again, then `x` to close it. Repeat
   using `f` and the numbered hint to cover keyboard activation, and use **Open
   JS popup terminal tab** to check the `window.open()` path.
8. Return to the interaction tab and click **Open history navigation test**.
   The purple **History page A** opens in that tab. Open the orange **History
   page B**, then use `[` / `]` (or `[B]` / `[F]` in the toolbar) to move A → B
   and B → A. Switch to the popup tab and back: its independent page and the
   history tab's A/B history must not be mixed. Verify `Ctrl+Tab` and
   `Ctrl+Shift+Tab` too when the terminal forwards those key combinations.
9. Click the explicit `×` in a non-final tab in the tab strip. Only that tab
   closes; clicking `×` on the final tab reports that the final tab is kept.
10. Return to the interaction page and open **Open iframe input test**. Press
    `i`: the cyan frame must surround the **Embedded input** inside the local
    iframe. Type `iframe-check`, press `Enter`, and confirm that the embedded
    result changes. Click that field and then its **Submit embedded input**
    button; the pointer coordinate and cyan frame must line up with the child
    document controls. `Esc` returns to normal mode without exiting. This is a
    same-origin regression check only; do not use it to test or bypass a
    cross-origin CAPTCHA.
11. In the embedded page, activate **Open iframe navigation result** by left
    click or by moving to it with `Tab` and pressing `Enter`. The green result
    heading must appear inside the iframe while the parent page remains in
    place. This verifies native pointer and keyboard navigation inside a
    same-origin frame; `f` link hints intentionally remain top-level only.

Repeat the same test with `--backend half-block` when validating a terminal
without Kitty Graphics Protocol support. The image is lower-resolution, but
focus safety and command routing must be identical.

## SIXEL manual check

On a terminal with SIXEL support, run:

```bash
target/debug/fpasobrowser --backend sixel \
  file://"$PWD"/examples/interaction.html
```

The default is fpasoterm's Linux/ChromeOS `8x17` physical cell geometry. If a
different SIXEL terminal presents a gap below the image or pointer targets are
offset, retry with its measured cell size, for example:

```bash
target/debug/fpasobrowser --backend sixel --sixel-cell-size 9x18 \
  file://"$PWD"/examples/interaction.html
```

The bottom toolbar reports the selected `sixel WIDTHxHEIGHT` value; use the
same value for image placement, Servo viewport, and pointer mapping.

Confirm that the page is presented as a single image rather than a `sixel ...
stub` status message, and resize the terminal to confirm that the image stays
within the viewport and that all three status/tab/toolbar rows remain visible
below it. Move the pointer freely, then left-click a visible button or input:
the click must be accepted and its resulting page change must appear after the
page finishes loading, without repeated full-screen blinking. SIXEL
intentionally does not redraw cosmetic hover states, so use click, Tab, or
`i` for interaction feedback. SIXEL is explicit-only: the omitted backend is
`kitty`, while `--backend auto` continues to select the broadly compatible
half-block backend. A terminal without SIXEL support can show a blank result or
literal control data; use the default `kitty` or `--backend half-block` there
instead.

The CI workflow type-checks the full embedded-Servo application with Clippy,
then runs the renderer library/integration tests without requiring a graphical
desktop session. It uses three Cargo build jobs: that keeps linking bounded
while leaving a jobserver token for WebRender's shader generator during a cold
build.
The Servo offscreen probe additionally checks that the embedded rendering
context can be compiled.
