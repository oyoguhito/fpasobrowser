# fpasobrowser

[English README](README.md)

`fpasobrowser` は実験的な terminal web viewer です。`lynx` や `w3m` のような
text browser ではありません。Servo browser engine をprocess内に組み込み、pageを描画して
frameを取得し、terminalへ画像として表示します。

Windows native を主な対象とし、macOS と Linux にも対応します。WindowsでWSLを必要とせず、
Chromium / Chrome / Firefox / 外部の`servo`実行fileも起動・要求しません。

## 主な機能

- embedded Servo による navigation、history、scroll、mouse、keyboard input
- Kitty Graphics Protocol対応terminalでは高解像度の画像表示
- 非対応terminal向けのANSI truecolor `half-block` fallback
- SIXEL対応terminal向けの`--backend sixel`
- WindowsでRDP sessionにも対応するANGLE/Direct3D rendering path
- 同一terminal pane内の複数tabとpopup tab

## 起動

既にPATHへ導入済みのbinaryは、URLなしでbuilt-in start pageを開きます。

```sh
fpasobrowser
fpasobrowser https://example.com/
```

source checkoutではplatformごとのlauncherを使います。

```sh
# Linux / macOS
./bin/fpasobrowser https://example.com/
```

```powershell
# Windows PowerShell
.\fpasobrowser.cmd https://example.com/
```

PowerShellではcurrent directoryを自動検索しないため、`fpasobrowser.cmd`ではなく
先頭に`./`相当の`.\`を付けて実行してください。

## sourceから導入してbuildする

source checkoutでは、OSに必要なbuild toolを導入してlocal debug binaryを毎回更新する
installerを使えます。既存の`target\\debug\\fpasobrowser.exe`があっても置き換えます。
checkout rootに古いrelease版`fpasobrowser.exe`が残っていても、source launcherとinstallerは
`target\\debug\\fpasobrowser.exe`を優先します。
初回のServo buildは大きく、時間とdisk spaceを必要とします。

```sh
# Debian / Ubuntu / ChromeOS Linux、Fedora系、Arch Linux、macOS
./install.sh
```

```powershell
# Windows（PowerShell または cmd.exe）
.\install.cmd
```

- `--build-only`: 前提条件の導入を行わず、既存binaryを置き換えてbuildします。
- `--no-build`: 前提条件だけを導入します。
- `--build`: platform launcherでUIを起動せず、必要な場合だけbuildします。
- `--rebuild`: platform launcherでUIを起動せず、既存binaryを必ず置き換えてbuildします。

Linux版installerは`apt`、`dnf`、`pacman`に対応します。Windows版は`winget`を使い、
Rust stable、Python 3、LLVMを必要時だけ導入します。macOSでXcode Command Line Toolsの
導入が求められたときは完了後に同じcommandを再実行してください。

checked-inの`.cargo/config.toml`は、Servoのparallel Stylo CSS workerへServo本体のbuildと同じ
8 MiB stackをcompile時に設定します。これによりdebug source buildで複雑なpageを表示した際の
`StyleThread#N has overflowed its stack`を防ぎます。launcherはこの設定fileもbuild inputとして
確認するため、更新をpullした後に一度`./bin/fpasobrowser --rebuild`または
`.\fpasobrowser.cmd --rebuild`を実行してStyloを再compileしてください。成功時には
`rebuilding Servo's Stylo CSS worker with an 8 MiB stack`と表示されるため、このlineを確認してから
browserを起動してください。

## Windows の確認手順

source buildの前に、launcherの診断を実行できます。

```powershell
.\fpasobrowser.cmd --doctor
```

`result: source-build prerequisites look ready.` と表示されれば準備完了です。既に
`target\debug\fpasobrowser.exe` がある場合、Rust / Python / LLVMの再確認は省略されます。

通常terminalでの互換性を優先する場合は、`half-block`を明示します。

```powershell
.\fpasobrowser.cmd --backend half-block --debug
```

`half-block`はterminal cellへの縮小前に過大なsource surfaceを作らないよう、既定で
`640x360`の軽量viewportを使用します（従来の`960x540`より約56%少ないpixelです）。
このため、memoryに余裕が少ないhostでfpasoterm/RDPと同時に使用する際のServo・ANSI変換・
terminal転送の負荷を抑えます。画像protocol backendと明示的な`--viewport`はこの軽量化の
対象外です。さらに画面下のtoolbarの`[-]` / `[+]` / `[100]`で、page zoomを80–150%の
範囲で変更・resetできます。

`[Ctrl]`、`[Alt]`、`[Meta]`はnoVNCのmodifier buttonと同じく、次のpage keyだけに
modifierを付けます。RDPやterminalがphysical chordを横取りする場合でも、buttonをclickして
からpage内で`c`等のkeyを押すことで`Ctrl+C`等を送れます。armed stateは次のkey送信、zoom操作、
または同じbuttonの再clickで解除されます。

Kitty Graphics Protocolが使えるKitty、またはgraphics bridgeをadvertiseするfpasotermでは、
引数なしの`auto`が`kitty`を選びます。Windows版fpasotermではConPTYがKittyのAPCを
破棄するため、current fpasotermがchild processへ通知する`osc-v1` bridgeを使います。
`auto`では、この通知が無い古いfpasotermを黒い画面にせず`half-block`へfallbackします。
一方、`--backend kitty`は明示指定を尊重し、通知を継承できていない既存tab/RDP sessionでも
Windows用OSC bridgeを試行します。imageが出ない場合はfpasotermを更新・完全再起動してから
再試行してください。

```powershell
.\fpasobrowser.cmd --backend kitty --debug
```

Windows上のfpasoterm内では、`--debug`に
`backend=kitty transport=fpasoterm-osc-rgba-zlib`が出ることを確認してください。fpasotermの
Diagnosticsを`graphics`でfilterすると、最初のframeで
`fpasoterm graphics transfer received`と`fpasoterm graphics rendered`が出ます。前者が
出ない場合はfpasotermもこのbridge対応版へ更新してください。

`--debug`では、標準errorへの出力に加えてpage上部へ **FPASOBROWSER DEVTOOLS** の
operation timelineを表示します。`POST engine click` / `POST engine key Enter`はbrowserへの
要求送信、対応する`RECV`はreply、`TIMEOUT`はapp側の待機切れです。screenshot、tab情報、
verification queryも同じ表示で追跡できます。通常起動でこの表示だけを使うには
`--debug-overlay`を指定します。normal modeで`d`または`D`を押すと表示・非表示を切替できます。

Windows ConPTYの長いOSC sequenceが壊れる既知の経路を避けるため、bridgeはpayloadを短い
OSC packetへ分割して送ります。bridgeではWebView2のPNG合成を通さず、zlib圧縮したraw RGBA
（`format=32`、`compression=z`）をfpasotermのCanvasへ直接描画します。成功時は
`paint=raw-rgba-putImageData`が表示されます。

Windows release archiveを展開した場合は、同じdirectoryの`fpasobrowser.cmd`から起動します。
archiveには`fpasobrowser.exe`、`libEGL.dll`、`libGLESv2.dll`が含まれるため、Cargo、Python、
LLVMは不要です。

## 描画backend

| backend | 用途 |
| --- | --- |
| `auto` | 既定。Kitty、またはbridge対応のfpasotermを検出できれば`kitty`、それ以外は`half-block`を選択します。Windowsで古いfpasotermを検出した場合も`half-block`へfallbackします。 |
| `half-block` | ANSI truecolorのみを使う可搬性重視のfallbackです。 |
| `sixel` | SIXEL対応terminal用の216色画像表示です。 |
| `kitty` | 圧縮済みPNG画像をterminal cellへ配置します。KittyとWindows以外のfpasotermには標準Kitty Graphics Protocol (APC) を使います。ConPTYはAPCを破棄するため、Windowsで明示的にKittyを選んだ場合は、native Kitty（`KITTY_WINDOW_ID`または`TERM=xterm-kitty`）と確実に判定できる時以外、fpasotermのprivate OSC bridgeを使います。これは`TERM_PROGRAM`を失った復元済みRDP tabにも適用されます。bridgeはzlib圧縮raw RGBA（`f=32`）をCanvasへ直接描画し、debug表示は`fpasoterm-osc-rgba-zlib`です。fpasotermでは非同期描画前にframeを削除しないよう、送信側でのframe retirementを遅延します。 |

## 操作

- `g`: URL入力
- `r`: reload
- `[` / `]`: back / forward
- `j` / `k` またはarrow keys: scroll
- `t`: 新しいtab
- `{` / `}`: tab切替
- `x`: active tabを閉じる（最後のtabは維持）
- `?`: help
- `q` または `Ctrl+Q`: 終了

formへ入力する前は`i`でinput modeに入り、`Esc`でnormal modeへ戻ります。

## 日本語ドキュメント

- [使い方](docs/usage.ja.md)
- [アーキテクチャ](docs/architecture.ja.md)
- [処理フロー](docs/flow.ja.md)
- [テスト](docs/testing.ja.md)
- [Servo PoC](docs/servo-poc.ja.md)

## 現在の制限

- 描画はscreenshotベースであり、text selectionやsemantic linkの公開は未対応です。
- `half-block`の画質は画像protocol backendより低くなります。
- Windows TerminalはKitty Graphics ProtocolとSIXELをadvertiseしないため、通常は
  `half-block` fallbackを使います。
- login automation、video/audio、browser extensionは対象外です。
- Servoは発展途上のbrowser engineであり、複雑なsite、frame、authentication flowの互換性は
  mature desktop browserと同等ではありません。

## license

本repositoryのlicenseは[LICENSE](LICENSE)を参照してください。binary release archiveには
`THIRD_PARTY_NOTICES.md`、exact `Cargo.lock`、`THIRD_PARTY_LICENSES/`も含まれます。再配布時は
これらをarchiveと共に保持してください。
