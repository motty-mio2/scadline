# Scadline

OpenSCAD (`.scad`) ファイルを読み込み、マウスで自由に確認できる Rust 製デスクトップビューアです。

## 必要なもの

- Rust 1.85 以降
- OpenSCAD（`openscad` コマンドが PATH にあること）

Ubuntu / Debian では OpenSCAD を次のようにインストールできます。

```bash
sudo apt install openscad
```

## 起動

```bash
cargo run --release
```

起動後に「開く」を押して `.scad` ファイルを選択します。動作確認には
`examples/demo.scad` を利用できます。

ファイルを指定して直接起動することも、ウィンドウへドラッグ＆ドロップすることもできます。

```bash
cargo run --release -- examples/demo.scad
```

## 操作

| 操作 | マウス |
|---|---|
| 回転 | 左ドラッグ |
| 平行移動 | 右ドラッグ / 中ドラッグ |
| ズーム | ホイール |
| 表示を戻す | ダブルクリック / ツールバー |

「自動更新」が有効な間は、開いている `.scad` ファイルを保存すると再生成します。
OpenSCAD の構文エラーは画面下部に表示されます。
