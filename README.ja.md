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

Kitty Graphics Protocolが使えるKitty、またはgraphics bridgeをadvertiseするfpasotermでは、
引数なしの`auto`が`kitty`を選びます。Windows版fpasotermではConPTYがKittyのAPCを
破棄するため、current fpasotermがchild processへ通知する`osc-v1` bridgeを使います。
古いfpasotermでは黒い画面を出さず`half-block`へfallbackします。自動検出されない
対応terminalでは、明示的に指定できます。

```powershell
.\fpasobrowser.cmd --backend kitty --debug
```

Windows上のfpasoterm内では、`--debug`に
`backend=kitty transport=fpasoterm-osc-rgba-zlib`が出ることを確認してください。fpasotermの
Diagnosticsを`graphics`でfilterすると、最初のframeで
`fpasoterm graphics transfer received`と`fpasoterm graphics rendered`が出ます。前者が
出ない場合はfpasotermもこのbridge対応版へ更新してください。

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
| `kitty` | 圧縮済みPNG画像をterminal cellへ配置します。KittyとWindows以外のfpasotermには標準Kitty Graphics Protocol (APC) を使います。Windows fpasotermではConPTYがAPCを破棄するため、zlib圧縮したraw RGBA（`f=32`）をprivate OSC bridgeで送ってCanvasへ直接描画します。debug表示は`fpasoterm-osc-rgba-zlib`です。fpasotermでは非同期描画前にframeを削除しないよう、送信側でのframe retirementを遅延します。 |

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
