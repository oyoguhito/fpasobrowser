# Servo terminal PoC

`examples/servo_offscreen.rs` は、Servoを外部ブラウザー実行ファイルなしで組み込み、`SoftwareRenderingContext` からRGBAフレームを取得する最初の移行検証です。

```sh
cargo check --no-default-features --features servo-engine --example servo_offscreen
cargo run --no-default-features --features servo-engine --example servo_offscreen -- \
  file:///absolute/path/to/page.html servo-offscreen.png
```

同じ `SoftwareRenderingContext` とServo所有のevent loopは、現在feature-gatedの
fpasobrowser terminal経路でも使用しています。残作業はDOM由来のfocus表示、link hint、
複数WebView、recoveryなどの操作互換です。

Servoのビルドは非常に大きく、初回コンパイルには多数のnative依存、十分なディスク容量、フォント/TLS/グラフィックス関連の開発環境が必要です。
