# 開発コマンド一覧

## ビルド・実行
- `cargo build` — プロジェクトをビルド
- `cargo run` — jarvish を実行
- `cargo build --release` — リリースビルド

## テスト
- `cargo test` — 全テスト実行
- `cargo test --all-targets` — 全ターゲットのテスト実行
- `cargo test <test_name>` — 特定テスト実行

## フォーマット・リント
- `cargo fmt --all` — コードフォーマット
- `cargo fmt --all -- --check` — フォーマットチェック（変更なし）
- `cargo clippy --all-targets -- -D warnings` — Clippy lint チェック
- `RUSTFLAGS="-Dwarnings" cargo check --all-targets` — コンパイルチェック（警告をエラーに）

## 総合チェック（CI相当）
- `make check` — pre-push チェック全実行（fmt auto-fix + check + clippy + test）

## Git フック
- `make install-hooks` — pre-push フックをインストール
- `make uninstall-hooks` — pre-push フックを削除

## システムコマンド (macOS/Darwin)
- `git` — バージョン管理
- `ls` — ファイル一覧
- `cd` — ディレクトリ移動

## 注意事項
- `.cursor/rules/general.mdc` の規約により、`cargo build` / `cargo test` / `cargo check` はサンドボックスで実行しないこと
- `docs/` フォルダ配下のMarkdownファイル名は全て大文字にすること
