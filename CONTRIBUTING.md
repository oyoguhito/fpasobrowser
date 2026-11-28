# Reporting bugs

Please include a redacted in-app support report whenever possible.

1. Reproduce the issue in `fpasobrowser`.
2. Press `d` (or `D`) in normal mode to open **FPASOBROWSER DEVTOOLS**.
3. Press `C` in normal mode. This copies the support report through the terminal's
   OSC 52 clipboard path.
4. Paste the report into the issue and describe the expected result, actual result,
   and concise reproduction steps.

The report includes the fpasobrowser build commit, platform, selected rendering
backend and transport, the safe fpasoterm IPC bridge state, browser connection state,
and recent POST/RECV events. It deliberately redacts page URLs, the per-terminal
graphics endpoint, and its capability token. Add a reproduction URL manually only
when it is safe to disclose.

If your terminal does not expose OSC 52 directly to the host clipboard, use its
explicit **Copy Last OSC 52** action after pressing `C`.
