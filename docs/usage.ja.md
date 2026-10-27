# fpasobrowser 使い方

## 起動

既定のbuildはServoを直接組み込みます。実行時に Chromium / Chrome / Firefox / 別途の
`servo` 実行ファイルを起動・要求しません。

```sh
./bin/fpasobrowser
```

URLを省略すると、組み込みのstart pageを表示します。`g`またはtoolbarの`[URL]`でURLを入力するか、
引数で直接渡せます。

```sh
./bin/fpasobrowser https://example.com/
```

## Windows native: source buildの前提条件

release binaryはChromium、Firefox、別途のServo実行ファイル、Pythonを必要としません。一方、sourceから
buildする場合はServoのStylo CSS build stepがCargoによるcompile中にPython 3を使います。Pythonはbrowserの
実行時依存ではなく、build時だけの前提条件です。

Rust stableとPython 3を導入し、PATHを更新するためPowerShellを開き直してからcheckout launcherを使います。

```powershell
winget install Rustlang.Rustup
winget install Python.Python.3.12
cd C:\path\to\fpasobrowser
./bin/fpasobrowser.ps1 --version
```

launcherは`python.exe`またはWindowsの`py` launcherを検出し、Stylo用の`PYTHON3`を設定します。Pythonが
`PATH`外にある場合に`cargo build`を直接実行するには、絶対pathを指定します。

```powershell
$env:PYTHON3 = 'C:\path\to\python.exe'
cargo build --locked
```

Windowsの既定backendは`half-block`です。通常のPowerShellとWindows TerminalはKitty Graphicsを表示できないためです。
fpasoterm上では高解像度backendを明示指定します。

```powershell
./bin/fpasobrowser.ps1 --backend kitty
```

WindowsではRemote Desktop sessionでも描画できるよう、WGLではなくServoのANGLE/Direct3D fallbackを使います。
`libEGL.dll`と`libGLESv2.dll`は`fpasobrowser.exe`と同じdirectoryに置いてください。source launcherと
release archiveでは自動的に配置されます。

## Linux / ChromeOS Linux: 最初のbuildと確認

まずServoを含むbinaryを一度buildします。以後の対話確認には `cargo run` ではなく、生成済みの
binaryを直接使うと、毎回Cargoのbuild確認を行わずに済みます。

```sh
cd /home/oyoguhito/github/oyoguhito/fpasobrowser
./bin/fpasobrowser --version
```

source checkoutでは`./bin/fpasobrowser`を通常の起動口にします。`target/debug/fpasobrowser`が
存在しない、またはRust/Cargo入力より古い場合だけbuildし、その後は生成済みbinaryを直接実行します。
毎回`cargo run`を入力する必要はありません。Windows PowerShellでは
`./bin/fpasobrowser.ps1 --version`を使います。

最初にterminalのraw modeへ入らないframe確認を行います。同梱ページのPNGが生成され、見出し
`fpasobrowser interaction test`、入力欄、黄色のbuttonが見えれば、外部ChromiumなしのServo描画と
frame取得は成功です。

```sh
target/debug/fpasobrowser --capture-only --viewport 1200x800 \
  --dump-screenshot /tmp/fpasobrowser-check.png \
  file://"$PWD"/examples/interaction.html
file /tmp/fpasobrowser-check.png
```

次に実際のterminal画面を確認します。引数を省略した既定backendはLinux/macOSでは `kitty`、Windowsでは
`half-block` です。fpasotermまたはKitty Graphics Protocol対応terminalで高解像度表示を使う場合は
`--backend kitty` を明示指定します。

```sh
./bin/fpasobrowser \
  file://"$PWD"/examples/interaction.html
```

`?` でキー一覧を表示し、Web formへ入力する前に `i` を押してください。page上でkeyboard focusを
持つ要素は水色の枠で表示されます。input mode中の `Esc` はそのfocusを外してnormal modeへ戻します。
normal mode中の文字キーは古いpage focusへ送られません。`q` または `Ctrl+Q` で終了します。PNG確認は
Servo描画、対話確認は選択したterminal rendererと入力転送をそれぞれ確認するため、分けて実施します。

build済みbinaryを導入後は次のように起動します。

```sh
fpasobrowser https://example.com/
```

再現性のある操作確認には `file://` ページを使えます。

```sh
fpasobrowser \
  file:///absolute/path/to/fpasobrowser/examples/interaction.html
```

このworkspaceでの例:

```sh
cargo run -- \
  file:///home/oyoguhito/github/oyoguhito/fpasobrowser/examples/interaction.html
```

## renderer backend

```sh
fpasobrowser --backend auto https://example.com/
fpasobrowser --backend half-block https://example.com/
fpasobrowser --backend sixel https://example.com/
fpasobrowser --backend sixel --sixel-cell-size 8x17 https://example.com/
fpasobrowser https://example.com/
```

引数を省略した既定backendはLinux/macOSではKitty Graphics ProtocolでPNGを転送する `kitty`、Windowsでは
portableな `half-block` です。WindowsのfpasotermでKitty画像表示を使う場合は `--backend kitty` を明示指定します。
明示的な `--backend auto` も現在は互換性優先の `half-block` を選び、truecolor端末で使えますが解像感は低くなります。
`sixel` はSIXEL対応terminal向けに216色SIXEL streamを出力します。
既定ではfpasotermのLinux/ChromeOS gridとmouse座標に合う8×17 pixel cellを使い、Servoのdynamic
viewportも同じ寸法にします。これによりSIXEL rendererでの二重拡大を避けます。異なるphysical cell
寸法のterminalでは `--sixel-cell-size WIDTHxHEIGHT` または環境変数
`FPASOBROWSER_SIXEL_CELL_SIZE` を指定します。toolbarに有効な値（例: `sixel 8x17`）が表示されます。
このoptionは `--backend sixel` と組み合わせて使用してください。

## Herdr paneでの起動

fpasobrowserはHerdr pluginを必要とせず、通常のHerdr pane内で次のように起動できます。

```sh
./bin/fpasobrowser
```

現在確認できるHerdr sourceではKitty Graphicsが既定で有効で、mouse reportingもpane applicationへ
転送する実装があります。そのため、outer terminalもKitty Graphicsに対応していれば、`--backend kitty`
を試す対象です（Linux/macOSでは既定、Windowsでは明示指定）。明示的に設定する場合はHerdr configで次を指定し、server restartまたはclient
reattachを行います。

```toml
[terminal]
kitty_graphics = true
```

imageが別paneへ重なる・clipされる、またはmouse入力がviewerへ届かない場合は、fpasobrowser内の
browser engineではなくHerdrのpane graphics translation/clippingまたはinput routingの問題です。
切り分けにはKitty graphicsを使わない`--backend half-block`を使えますが、画像品質は意図的に低下します。
SIXELは現時点でHerdr上の確認済みtransportではありません。

既定表示でpage下部にある3行の文字は、fpasobrowser自身のactivity/status line、tab strip、mouse
toolbarです。別のpage画像やHerdrのKitty Graphics配置・clip不具合ではありません。pageを2行分広く
使いたい場合は、activity/status lineだけを残すcompact UIを指定します。

```sh
./bin/fpasobrowser --ui compact
```

`--ui compact`ではkeyboardによるtab・navigation操作は使えますが、click用tab stripとtoolbarは意図的に
表示しません。

page下部に文字を一切残したくない場合は`--ui page`を使います。通常のstatus feedbackは表示しません。
ただし現行fpasoterm/HerdrのKitty presentationでは全terminal行を使うplacementが下端へanchorされる、または
clipされるため、表示しない安全用の1行は確保します。page上のpointer click・wheel・keyboard操作は使えます。

Herdrのmouseを切り分ける間だけは`--ui compact`で起動してください。activity lineの`hover …`はmouse移動が
すでにfpasobrowserまで届いている証拠です。nested terminalが物理button pressの後から転送を開始しても、最初の
left drag/releaseを1回のclickとして扱います。このfallback後も`clicked …`が表示されずpageの操作も起きない場合は、
Herdr revision、outer terminal、pane layout、`kitty_graphics`設定を添えたHerdrのpane mouse routing issueです。
Kitty画像のclip/translationとは別に扱います。

hoverの確認はactivity lineだけを更新し、page screenshotの再送は予約しません。したがってpointerを動かすだけで
Kitty表示のpageが点滅することはありません。

## 診断とframe取得

```sh
fpasobrowser --version
fpasobrowser -v
fpasobrowser --version-info
fpasobrowser --debug --backend kitty https://example.com/
fpasobrowser --capture-only --dump-screenshot page.png https://example.com/
fpasobrowser --viewport 1366x768 https://example.com/
```

`--capture-only` は `--dump-screenshot` が必須で、PNGを1枚保存して終了します。
terminal UIでは `?` のhelp panelにもversion/build情報を表示します。

## キーとmouse操作

| Key / input | 動作 |
| --- | --- |
| normal mode中の `q`、または常時有効な `Ctrl+Q` | 終了 |
| `Esc` | input mode中はpage focusを外してnormal modeへ戻る。normal modeで2個目以降のterminal tabを表示中なら、tabを閉じずに直前のtabへ戻る。viewerは終了しない |
| `?` | terminal内help panelの表示 / 非表示 |
| `r`, `R` | 再読み込み / 再描画 |
| `j`, `Down` | 下へscroll |
| `k`, `Up` | 上へscroll |
| `g` | URLを入力して現在のWebViewで移動 |
| `b`, `B`, `[` / `F`, `]` または `Alt+Left` / `Alt+Right` | Servo履歴を戻る / 進む。小文字の`f`はlink hintのまま |
| `o`, `v`, `V` | 現在のHTTP(S) URLをOS標準browserで開く。local `file:` pageはfpasobrowser内に残す |
| `i` | browser input modeへ入る |
| input mode中の `Esc` | page fieldのfocusを外してnormal modeへ戻る |
| input mode中の `Tab`, `Shift+Tab`, `Enter` | page内focus移動 / 選択 |
| `t`, `x`, `{`, `}`、`Ctrl+Tab`、`Ctrl+Shift+Tab` | 新規tab、active tabを閉じる、前のtab、次のtab |
| `f` | 番号付きlink/action hintを表示し、番号を入力して実行 |
| left click | WebViewへpointer clickを送る |
| mouse移動 | Kitty/half-blockではrate limit付きでpointer moveを送り、link上ではstatus lineにURLを表示。SIXELではclick遅延を避けるためcosmetic hover転送を抑止。 |
| mouse wheel | WebViewへscrollを送る |

normal modeでは `q`、`r`、`g`、`f` などをfpasobrowserが使い、文字キーをpageへ送信しません。Web formへ
文字を入力する前に `i` を押すと、最初に見えているeditable controlへfocusを移します。same-origin iframe内の
editable controlも対象です。page自身に見やすいfocus ringがある場合はその1つだけを使い、ない場合だけ
fpasobrowserが水色の補助枠を表示します。cross-origin iframe（CAPTCHA providerを
含む）はfpasobrowserのpage scriptで検査せず、Servo nativeのframe input処理に委ねます。focus中の要素は
水色の枠で確認でき、input mode中の `Tab` / `Shift+Tab` で移動します。editable controlをclickした場合も
input modeへ入ります。click後にServoが親側でiframeだけを報告した場合も、child controlへの文字入力とfocus
移動を維持するためinput modeを続けます。`Esc`でnormal modeへ戻ります。Servo tabは個別のWebViewとsoftware rendering contextを持ちます。popup tabも
独立したcontextを使うため、別tabの画像が表示されません。
最後のtabを `x` で閉じようとした場合は、terminal UIを消さないためtabを維持します。
pageが `target="_blank"` または `window.open()` でnew windowを要求した場合は、新しい選択済み
terminal tabへ昇格します。activity lineで作成を確認でき、`{` / `}` で元のtabへ戻り、`x` でpopup tabを
閉じられます。activity lineには選択されたtab番号も表示します。click、form送信、hint実行後は、固定回数の
再描画で終わらせず、Servoがactive tabをloadingと報告している間は更新を続けます。

page下部の3行はterminal UI用です。操作直後の上のactivity lineは、狭いterminalでも確認できるよう
`reloaded`、`went back`、`help shown`などのfeedbackを先頭に表示します。idle時はactive tab番号、
`loading` / `ready`状態、page title、現在URLを先頭に表示します。中央は
同じterminal pane内でtabを選べるtab stripです。下部toolbarの履歴表示は、戻る/進む先がない場合は
`(B)` / `(F)`、可能な場合は `[B]` / `[F]` になります。表示中のtab labelをclickするか、`{` / `}`、
terminalが報告できる場合は `Ctrl+Tab` / `Ctrl+Shift+Tab` を使うと切り替えられます。各tabに表示する
明示的な `×` をclickすると閉じられ、keyboardの `x` でもactive tabを閉じられます（最後のtabは維持）。
下のtoolbarからmouseでreload、履歴、URL入力、新規tab、通常browserへの引き継ぎ、helpを操作できます。

`--ui compact`（または`FPASOBROWSER_UI=compact`）を指定すると、activity lineだけを残してpageへ2行
返します。このmodeでもkeyboard操作は使えますが、表示中のtab stripとmouse toolbarは省略されます。
`--ui page`を指定すると最後のactivity lineも省略します。nested terminalでKitty画像を確実に配置するため、
見えない安全用1行だけを残します。この場合は通常のstatus feedbackを表示しません。

Servoが1件の要求を処理中は、activity lineに操作名付きの`[working]`を表示します。active pageが
読み込み中なら、予約されたframe確認ごとに`[loading |]`、`[loading /]`、`[loading -]`、
`[loading \]`が切り替わります。これにより、遅いpageの読み込みとidle状態を区別できます。`q`は
normal mode中、`Ctrl+Q`はすべてのmode中で終了を要求します。ただしServo要求を実行中に受けた終了は、
その直列要求が戻った後に処理されます。安全なengine終了を待つ間もactivity lineに状態を表示します。

## CAPTCHA / site verification

CAPTCHAやunusual trafficはサイト側の判定です。fpasobrowserは検知回避・自動解決を行いません。
サイトが手動確認を許可している場合は、`o` または `v` で現在URLを通常browserに引き継ぎます。
通常browserは別sessionのため、そのCookieや確認結果がServoへ自動で取り込まれることはありません。
local `file:` のtest pageでは、OSの関連付けによりbrowserではなくterminal applicationが起動することを
防ぐため、`o` / `v` は外部起動せずviewer内に案内を表示します。

## 要件と対象platform

fpasobrowserはnative Windows、macOS、Linuxを対象にし、WindowsでWSLを必要としません。
Servoの初回buildは大きく、Rustに加えてgraphics、font、TLS、linker関連のnative要件が必要です。
配布済みfpasobrowser binaryにはengineが組み込まれるため、利用者が別途web browserを導入する
必要はありません。

高品質なterminal表示には Kitty Graphics Protocol対応端末を使います。Windows Terminalは現在
このprotocolを実装していないため、native Windowsでは追加のimage backend実装までtruecolor
half-block rendererを使います。

Linux / ChromeOS Linuxのrepository buildは `.cargo/config.toml` でCargo jobを3本に制限し、
インストール済みの `/usr/bin/mold` を `clang` 経由で使用します。これはServoの大きなlinkを
複数同時に走らせてメモリ不足になることを避ける設定です。Windows/macOSには適用しません。

## fpasotermとの関係

fpasobrowserはfpasotermとは独立したCLIです。将来的にfpasoterm pluginから外部CLIとして
起動することはできますが、fpasotermなしでも使える設計です。
