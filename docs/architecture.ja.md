# fpasobrowser アーキテクチャ

fpasobrowser は browser engine をプロセス内へ組み込む screenshot ベースの
terminal web viewer を目指します。利用者に Chromium / Chrome / Firefox 等の
別途インストールを要求しないことを目標とします。

実装はServo Rust crateをfpasobrowserへ直接組み込みます。別途 `servo` 実行ファイルを
起動したり、Chromium/Firefoxを利用者に要求したりする方式は採用しません。Servoのfeature
設定によっては内部content process等を起動し得るため、OSプロセスが必ず1つという意味では
ありません。

このプロジェクトは、`terminal-browser` が Windows native の terminal workflow に
対応していないことを補う独自実装として始めます。fpasoterm本体へ組み込むのではなく、
Windows、macOS、Linuxで独立して検証・配布できるRust CLIにします。

## 構成要素

- `src/main.rs`: CLIを解釈し、appを起動します。
- `src/app.rs`: terminal lifecycle、browser capture、rendering、input command
  をまとめます。
- `src/servo_browser.rs`: Servo threadとtabごとのWebView、navigation、input、software frame取得を
  所有します。Servoのrendering contextは所有threadに留める必要があるため、terminal/Tokio側とは
  channelでrequestを交換します。
- `src/terminal.rs`: raw mode、alternate-screen、terminal size、cleanup を扱います。
- `src/input.rs`: terminal key event を高レベル command に変換します。
- `src/render/mod.rs`: `RenderBackend`、backend selection、共通size型を定義します。
- `src/render/half_block.rs`: MVPの ANSI truecolor half-block backend を実装します。
- `src/render/sixel.rs`: 216色へquantizeしたSIXEL graphics streamを出力します。
- `src/render/kitty.rs`: Kitty Graphics Protocol backendです。

## 描画モデル

browser screenshot を取得し、選択された backend で terminal に表示します。
half-block backend は 2 image rows を 1 terminal row に対応させます。使用する文字は
upper half-block character (`▀`) です。

- foreground color = 上側pixel
- background color = 下側pixel

Kitty backend は PNG screenshot を Kitty Graphics Protocol で送り、terminal cell
範囲に placement します。half-block より高品質ですが、対応端末が必要です。

SIXEL backendは画像を6×6×6 color cubeへquantizeしてSIXEL DCS streamとして出力します。
terminal cellのpixel geometryにはportableなqueryがないため、既定ではfpasotermのLinux/ChromeOS gridに
合わせた8×17 pixelを使います。別のSIXEL terminalでは `--sixel-cell-size WIDTHxHEIGHT` または
`FPASOBROWSER_SIXEL_CELL_SIZE` でoverrideできます。同じ選択済みgeometryでServoのdynamic viewportも決め、
SIXEL表示時にscreenshotを二重拡大しないようにします。画像とmouse座標を合わせつつ画像下の3 UI行を残します。

## backend selection

引数を省略したCLIの既定backendは `kitty` です。明示的な`--backend auto`は現時点では
`half-block` を選びます。指定できる値は以下です。

- `half-block`: 明示指定するANSI truecolor fallback。
- `sixel`: SIXEL対応terminal向けの216色SIXEL image backend。
- `kitty`: 既定のKitty Graphics Protocol backend。

`RenderBackend` traitをprotocol固有実装の境界にし、`app.rs` はbackendを選んでtraitを
呼ぶだけにします。

## portability

fpasobrowser は native Windows、macOS、Linux を対象にします。Windows では WSL に
依存しない方針です。Servoを組み込むことで、利用者側のbrowser executable discoveryは
不要にします。ただしServoと描画・暗号・font等のnative依存をビルド/配布する必要が
あり、「外部browser不要」は「システム依存ゼロ」や「小さな単一バイナリ」を意味しません。

## Servo実装と残作業

- Servo公式の `SoftwareRenderingContext` からRGBA frameを読み出し、既存rendererへ渡します。
  Servoを所有するthreadは `spin_event_loop` を実行し、crossterm/Tokio側とはrequest channelで
  統合します。
- 現在のMVPはnavigation、reload、履歴、scroll、pointer event、基本IME/key input、番号付きlink hint、
  複数WebViewのterminal tabを移行済みです。`t` で作成するtabには独立した描画contextを割り当てます。
  engineはactive tabのtitle、URL、履歴可否、load状態を1つのsnapshotとしてTUIへ返すため、activity lineと
  履歴toolbarは別々に取得した値ではなく同一WebViewの状態を表示します。TUIはこのload状態に従い、
  navigationを起こす操作後も更新を続けます。hint実行でpopupを昇格した場合は選択済みtabも返します。
- `target="_blank"` と `window.open()` の要求では、Servoの同期response契約を満たすためdelegate
  callback内でauxiliary WebViewを生成します。生成済みWebViewはengine loopで選択済みterminal tabへ
  昇格します。popupを含む全terminal tabには独立したsoftware rendering contextを割り当て、別tabの
  screenshotが表示されないようにします。focus判定と水色markerはsame-origin iframeの内側まで辿ります。
  cross-origin iframeはpage JavaScriptで検査せず、Servo nativeのinput dispatchに委ねます。永続site data、
  より完全なpopup互換性、lifecycle/recoveryは残作業です。
- Servoは発展中のengineです。サイト互換性、iframe、TLS、GPU/software rendering、
  platform build要件は実機検証し、既存Chromiumと同等とは仮定しません。
- Google等のCAPTCHA/anti-abuse判定を回避する目的には利用しません。engine変更で
  challengeが表示されなくなる保証もありません。

## 拡張点

将来の renderer は `RenderBackend` trait を実装し、protocol固有の処理を `app.rs` に
漏らさないようにします。

予定している backend:

- Kitty graphics protocolの改善
- iTerm2 image protocol

browser interaction は `servo_browser.rs` 側に集約し、terminal input とServo embeddingを
分離します。
