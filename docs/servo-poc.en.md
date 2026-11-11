# Servo terminal PoC

`examples/servo_offscreen.rs` is the first migration probe: it embeds Servo without an external browser executable and reads an RGBA frame from `SoftwareRenderingContext`.

```sh
cargo check --no-default-features --features servo-engine --example servo_offscreen
cargo run --no-default-features --features servo-engine --example servo_offscreen -- \
  file:///absolute/path/to/page.html servo-offscreen.png
```

The same `SoftwareRenderingContext` and Servo-owned event loop are now used by
the feature-gated fpasobrowser terminal path. The remaining work is interaction
parity: DOM focus feedback, link hints, multiple WebViews, and recovery.

Servo's build is large and requires substantial disk space plus native font, TLS, and graphics development dependencies.
