# Scadline

OpenSCAD (`.scad`) ファイルを読み込み、マウスで自由に確認できる Rust 製デスクトップビューアです。

## 必要なもの

- Rust 1.95.0（`rust-toolchain.toml` で固定）
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

対応するOpenSCADでは、高速なManifoldバックエンドをCLIから選択できます。

```bash
cargo run --release -- --backend manifold examples/demo.scad
```

`--backend` には `manifold`、`cgal`、または `openrscad` を指定できます。CLIの指定は設定ファイルより
優先されます。バックエンドを指定しなければOpenSCAD自身の既定値を使用します。

[OpenRSCAD](https://openrscad.com/)をインストールして `openrscad` コマンドをPATHに置けば、
互換性のあるモデルを次のように生成できます。

```bash
cargo run --release -- --backend openrscad examples/demo.scad
```

Scadlineが実行するコマンドは、OpenRSCAD公式CLIと同じ
`openrscad model.scad -o out.stl` 形式です。

## 操作

| 操作 | マウス / キーボード |
|---|---|
| ファイルを開く | `Ctrl+O` |
| 再生成 | `Ctrl+R` |
| 回転 | 左ドラッグ / 右上のViewCubeをドラッグ |
| 平行移動 | 右ドラッグ / 中ドラッグ |
| ズーム | ホイール |
| 表示を戻す | ダブルクリック / `Home` / `0` |
| 正面・右・上面 | `1` / `2` / `3`、またはViewCubeの面をクリック |
| エッジ表示 | `E` |
| 座標軸表示 | `A` |

右上のViewCubeはモデルと同じ向きに回転します。面をクリックすると、その面へ視点を
切り替えられます。

「自動更新」が有効な間は、開いている `.scad` ファイルを保存すると再生成します。
OpenSCAD の構文エラーは画面下部に表示されます。

## Git タイムライン

Git リポジトリ内の `.scad` を開くと、画面下部にタイムラインが現れます。
左端が最古のコミット、右端の緑色の点が現在の作業ツリーです。各点が1コミットを
表しており、クリックまたはドラッグで移動できます。ブランチを切り替えることもできます。

過去のモデルはlibgit2でリポジトリ全体を一時展開してから生成します。そのため、
そのコミットに含まれる `include` / `use` ファイルも一緒に再現されます。閲覧中のリポジトリを
checkout したり、作業ツリーを書き換えたりはしません。

## アーキテクチャ

コードは `domain`、`application`、`infrastructure`、`presentation`に分割しています。
責務、依存方向、egui向けの単方向UIフローは[設計ドキュメント](docs/architecture.md)を参照してください。

生成済みの過去コミットはOS標準のキャッシュ領域へ保存します。

- Linux: `$XDG_CACHE_HOME/scadline`（未設定時は `~/.cache/scadline`）
- macOS: `~/Library/Caches/scadline`
- Windows: `%LOCALAPPDATA%\\scadline`

STLは `<repo-hash>/<commit-hash>/[backend-]<scad-path-hash>.stl` の順に整理されます。
同じコミットを再訪したときは OpenSCAD を再実行せず、キャッシュから即座に表示します。
スライダー操作中も最後に選択した位置だけを生成するため、途中のコミットを無駄に
レンダリングしません。キャッシュ保存先は `CacheLocation` として分離されているため、
テストや将来の設定画面から別のディレクトリを注入できます。

表示用レンダリングが終わって500ミリ秒操作がなければ、現在位置の前後2コミットを
近い順にバックグラウンド生成します。タイムライン上ではキャッシュ済みの点が青色になります。

## 設定

初回起動時に `~/.config/scadline/config.toml` を作成します。

```toml
[cache]
max_size_mb = 256

[openscad]
# backend = "manifold"
```

`openscad.backend` は省略可能で、`"manifold"`、`"cgal"`、または `"openrscad"` を指定できます。
ScadlineはManifoldやOpenRSCADをライブラリとして組み込まず、インストール済みの外部CLIを
子プロセスとして呼び出します。このため依存crateは増えず、ルーティング用コード以外に
Scadline本体のバイナリサイズやビルド時間を大きくする要因はありません。

OpenRSCAD自体は `Apache-2.0 OR MIT` のデュアルライセンスで、MITを選択して同梱することも
ライセンス上は可能です。一方、Rust coreを直接リンクするとOpenRSCADの各crateに加え、native
Manifold kernelのビルドにCMakeとC/C++コンパイラが必要になり、Scadlineのビルド時間、バイナリ
サイズ、クロスコンパイル要件が増えます。そのため現在は軽量な外部CLI方式を採用しています。
将来インストーラへ実行ファイルを同梱する場合は、MITライセンス文を配布物へ含めます。
OpenRSCADで使用する `.scad` 機能の互換性についても配布元を確認してください。

キャッシュ全体がこの上限を超えると、最後に表示してから時間が経ったSTLから削除します。
STLをキャッシュから表示するたびに利用時刻を更新するため、よく使う履歴は残りやすくなります。
標準値は256 MBです。単体で上限を超えるSTLは表示できますが、キャッシュには残りません。
旧版の `stl-v2` キャッシュは初回起動時に現在の配置へ移動し、互換性のない `stl-v1` は削除します。
