# ビルド方法（Windows）

MMD Viewer は、x86_64 の Windows 10 / 11 で、MSVC 版の Rust ツールチェーンと Direct3D 12 を対象にしています。

Linux と macOS は対象外です。

## 1. 必要なもの

| ツール | 用途 |
| --- | --- |
| Windows 10 / 11 SDK | Win32 API とダイアログ |
| Visual Studio Build Tools（MSVC x64）、または MSVC を含む Visual Studio | Rust のリンクと、**Bullet（C++）のコンパイル** |
| rustup（stable） | Rust のツールチェーン |
| Git | リポジトリと Bullet のサブモジュールの取得 |

`rust-toolchain.toml` が stable の Rust、`x86_64-pc-windows-msvc` ターゲット、rustfmt、Clippy を指定しています。

ビルド前に、MSVC 版のホストになっているかを確認します。

```bash
rustup show
rustc -vV
```

`rustc -vV` の出力に `host: x86_64-pc-windows-msvc` があれば正しい状態です。GNU 版のホストが入っている場合は、MSVC 版を入れて既定にします。

```bash
rustup toolchain install stable-x86_64-pc-windows-msvc
rustup default stable-x86_64-pc-windows-msvc
```

## 2. 取得

物理演算の Bullet 3.25 は `vendor/bullet3` に Git サブモジュールとして入っています。サブモジュールごと取得してください。

```bash
git clone --recurse-submodules <リポジトリの URL>
# すでに取得済みの場合
git submodule update --init vendor/bullet3
```

`vendor/bullet3` がないと、`mmd-physics` の `build.rs` が次のメッセージで止まります。

```text
vendor/bullet3 is missing. Run `git submodule update --init vendor/bullet3` and check out tag 3.25.
```

## 3. コマンド

リポジトリのルートで実行します。

```bash
cargo fmt --all -- --check                                    # 整形の確認
cargo clippy --workspace --all-targets --all-features -- -D warnings  # 静的解析（警告はエラー扱い）
cargo test --workspace                                        # テスト
cargo check --workspace                                       # 型チェック
cargo run -p mmd-viewer                                       # アプリの起動（開発ビルド）
cargo run --release -p mmd-viewer                             # アプリの起動（リリースビルド）
```

- Cargo のリゾルバはバージョン 3（Rust 2024 エディションに対応）です。依存のバージョンは `Cargo.lock` で固定しています。
- Bullet は `build.rs` で C++17・最適化あり（`/O2`）でビルドされます。初回のビルドは Bullet のコンパイルのぶん時間がかかります。

### 開発ビルドの速度について

ワークスペースの `Cargo.toml` に開発ビルド用の最適化設定はありません。開発ビルド（`cargo run -p mmd-viewer`）では、画像・音声のデコーダや、アニメーション評価・物理の Rust 側の処理が最適化されないため、モデルや曲の読み込みが遅くなります（手元の計測では、4,000 万画素分のテクスチャで約 5 秒、mp3 1 曲で約 3 秒）。再生の速さや読み込み時間を確かめるときは、`--release` でビルドしてください。

## 4. 改行コード

リポジトリのテキストファイルは LF で統一しています。

- `rustfmt.toml` が `newline_style = "Unix"` を指定しているので、CRLF のファイルは `cargo fmt --check` で失敗します。
- `.gitattributes` が `* text=auto eol=lf` を指定しているので、`core.autocrlf=true` の Windows 環境（GitHub Actions の Windows ランナーを含む）でも LF で取り出されます。
- 画像・モデル・モーション・音声ファイルは `binary` 指定で、変換しません。

## 5. 実行時のログ

| 項目 | 内容 |
| --- | --- |
| 出力先 | 標準エラー出力と、作業ディレクトリの `logs/` 以下の日次ファイル（`mmd-viewer.log.<日付>`） |
| 既定のフィルタ | `info,wgpu_core=warn,wgpu_hal=warn,naga=warn` |
| フィルタの変更 | 起動前に環境変数 `RUST_LOG` を設定する。不正な値なら既定に戻し、標準エラー出力に知らせる |
| コンソール | 開発ビルドはコンソールを開く。リリースビルドは Windows サブシステムで、コンソールを開かない |

wgpu と egui が使う `log` クレートの出力も、同じログに転送されます。

## 6. リリース

`v` で始まるバージョンタグ（例: `v0.1.0`、`v0.2.0-beta.1`）を push すると、[.github/workflows/release.yml](../../.github/workflows/release.yml) が GitHub のリリースを作ります。

```bash
git tag v0.1.0
git push origin v0.1.0
```

| 手順 | 内容 |
| --- | --- |
| バージョンの確認 | タグから `v` を除いた値が、`Cargo.toml` のワークスペースのバージョンと一致しなければ失敗する。タグを打つ前にバージョンを上げておく |
| テストとビルド | `cargo test --workspace` のあと、`mmd-viewer.exe` をリリースビルドする |
| 配布物 | `mmd-viewer-<バージョン>-x86_64-pc-windows-msvc.zip`（`mmd-viewer.exe` と `README.md`）。シェーダーは実行ファイルに埋め込まれているため同梱しない |
| リリースノート | タイトルは `MMD Viewer <バージョン>`。本文は GitHub の自動生成（前のタグからの変更と PR の一覧） |
| プレリリース | バージョンに `-` を含むもの（`1.0.0-rc.1` など）はプレリリースとして公開する |
