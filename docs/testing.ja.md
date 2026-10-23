# テスト

基本の確認コマンド:

```sh
cargo fmt --check
cargo clippy -- -D warnings
cargo test -j 1 --no-default-features --lib --tests
```

リポジトリの `.cargo/config.toml` では `build.jobs = 3` を設定しています。3本にすることで
メモリ使用量を抑えながら、cold build時のWebRender shader生成用にCargo jobserver tokenを残せます。
cacheを無効化した後に1本または2本へ強制すると、このbuild scriptが待ち続けることがあります。Linux/ChromeOS Linuxでは
同じファイルで `/usr/bin/mold` を `clang` 経由で使います。Windows/macOSはそれぞれのnative toolchainを
使い続けます。

現在のunit/integration testは、完全なServo WebViewを起動しない決定的な箇所を中心にします。

- renderer sizing
- ANSI truecolor half-block output
- command/inputから独立したdata type

以下はOSごとのmanual testが必要です。

- OSごとのServo build/runtime要件
- `fpasobrowser <url>` 起動
- screenshot capture
- terminal resize
- `q`, `r`, `j`, `k`, arrow keys, `o`

## local interaction regression確認

new windowを開くlinkに番号hintを使った後、activity lineがpopup terminal tabと選択番号を表示し、tab stripも
そのtabへ切り替わることを確認します。遅いnavigationでは`loading`の間もframe更新が続き、`ready`へ変わることを
確認します。

公開siteではbot対策やnetwork遅延がinput不具合を隠すため、同梱pageを使って確認します。

```sh
target/debug/fpasobrowser --backend kitty \
  file://"$PWD"/examples/interaction.html
```

1 sessionで次を確認してください。

1. `i` を押す。**Input test** fieldに水色のfocus枠が付く。
2. `focus-check` を入力して `Enter` を押す。結果が `Entered: focus-check` になる。
3. `Esc` を押す。水色の枠が消えるがviewerは終了しない。normal modeで通常の文字を押してもInput test
   fieldが変化しない。終了はnormal mode中の `q` または常時有効な `Ctrl+Q` だけである。
4. 再度 `i` を押し、`Tab` / `Shift+Tab` を押す。水色の枠がpage control間を移動する。
   **Show typed text** 上で `Enter` を押すとbuttonが実行される。
5. editableでないcardをclickするかmouse wheelでscrollする。normal modeへ戻り、`r`、`?`、`q` が
   viewer commandとして動作する。
6. **Go section A** 上へclickせずmouseを移動する。activation前にstatus lineへ `#section-a` のURLが
   表示される。screenshotとkeyboard操作の応答性を保つため、mouse moveはrate limitされる。
7. **Open popup terminal tab** をclickする。activity lineにpopup tab作成が表示され、表示中の
   tab stripには選択済みの `2*` tabが追加される。**Popup terminal tab** pageの緑色headingが同じ
   terminal pane内に表示され、interaction pageの画像のままにならない。元tabでclickしたbuttonの
   水色focus枠も残らない。先頭tab labelをclickする（または `{`）と元へ戻り、popup tab labelを
   clickする（または `}`）と再選択できる。`x` で閉じる。`f` の番号付きhint経由でも同じ確認を行い、
   **Open JS popup terminal tab** で `window.open()` 経路も確認する。
8. interaction tabへ戻り、**Open history navigation test** をclickする。同じtabに紫色の
   **History page A** が表示される。橙色の **History page B** を開き、`[` / `]`（またはtoolbarの
   `[B]` / `[F]`）でA → B、B → Aへ移動する。popup tabへ切り替えてから戻っても、popupのpageと
   history tabのA/B履歴が混ざらないことを確認する。terminalがkey combinationを転送できる場合は
   `Ctrl+Tab` / `Ctrl+Shift+Tab` も確認する。
9. tab stripで最終tab以外に表示される明示的な `×` をclickする。そのtabだけが閉じる。最後のtabの
   `×` では最後のtabを維持するmessageが出ることを確認する。
10. interaction pageへ戻り、**Open iframe input test** を開く。`i` を押すとlocal iframe内の
    **Embedded input** が水色の枠で囲まれることを確認する。`iframe-check` を入力して `Enter` を押し、
    embedded resultが変わることを確認する。そのfieldと **Submit embedded input** buttonをclickし、
    pointer座標と水色枠がchild documentのcontrolへ合うことを確認する。`Esc` は終了せずnormal modeへ
    戻る。この確認はsame-originのみであり、cross-origin CAPTCHAの試験・回避には使わない。
11. embedded page内の **Open iframe navigation result** を左click、または `Tab` で移動して `Enter`
    で実行する。親pageを維持したままiframe内に緑色のresult headingが表示されることを確認する。
    これはsame-origin frame内のnative pointer / keyboard navigation確認であり、`f` のlink hintは
    意図的にtop-level documentだけを対象にする。

Kitty Graphics Protocol非対応terminalでは、同じ確認を `--backend half-block` で行います。画像は
低解像度になりますが、focus安全性とcommand routingは同じでなければなりません。

## SIXEL 手動確認

SIXEL対応terminalでは次を実行します。

```bash
target/debug/fpasobrowser --backend sixel \
  file://"$PWD"/examples/interaction.html
```

既定値はfpasoterm Linux/ChromeOSのphysical cell geometryである`8x17`です。別のSIXEL terminalで
画像下に隙間が出る、またはpointer targetがずれる場合は、計測したcell寸法を指定して再確認します。

```bash
target/debug/fpasobrowser --backend sixel --sixel-cell-size 9x18 \
  file://"$PWD"/examples/interaction.html
```

下部toolbarには選択中の`Sixel WIDTHxHEIGHT`が表示されます。画像placement、Servo viewport、pointer
mappingには必ず同じ値が使われます。

`sixel ... stub` のstatus messageではなく、page全体が1枚の画像として表示されることを確認し、
terminalをresizeして画像がviewport内に収まること、画像下のstatus/tab/toolbarの3行が見えることも
確認します。pointerを動かした後に見えているbuttonまたはinputをleft clickし、全画面点滅を繰り返さず、
pageのloading完了後にclickによる画面遷移結果が表示されることも確認します。SIXELではcosmetic hover stateを意図的に再描画しないため、
操作feedbackにはclick、Tab、または`i`を使います。SIXELは明示指定専用であり、引数省略時は `kitty`、
`--backend auto` は広く互換性のあるhalf-block backendを選び続けます。SIXEL非対応terminalでは空白または
literal control dataになる場合があります。その場合は既定の `kitty` または `--backend half-block` を使います。

CI workflowでは、Clippyで組み込みServo application全体をtype checkした後、graphical
desktop sessionを要求せずrenderer library/integration testを実行します。Cargo jobは3本にして、
linkerの並列度を抑えつつWebRender shader generator用のjobserver tokenを確保します。
embedded rendering contextを対象にしたServo offscreen probeもcompile対象です。
