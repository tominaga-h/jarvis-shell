# コードスタイルと規約

## 命名規則
- 標準のRust命名規則に準拠
  - 関数・変数: `snake_case`
  - 型・構造体・列挙型: `PascalCase`
  - モジュール: `snake_case`
  - 定数: `SCREAMING_SNAKE_CASE`

## コメント・ドキュメント
- **日本語でコメント・ドキュメントを記述**
- `///` — 公開アイテムのドキュメントコメント
- `//!` — モジュールレベルのドキュメントコメント
- 通常のコメント `//` も日本語

## エラーハンドリング
- `anyhow::Result` を汎用的に使用
- ビルトインコマンドは独自の `CommandResult` 型を返す

## モジュール構成パターン
- 機能ごとにディレクトリで分割（shell, engine, ai, storage, cli）
- 各ディレクトリに `mod.rs` を配置
- 可視性: `pub(super)` や `pub(crate)` を適切に使用

## ビルトインコマンドのパターン
- `clap::Parser` (derive) でサブコマンド引数を定義
- `execute(args: &[&str]) -> CommandResult` 関数を各ビルトインに実装

## 設定
- TOML形式 (`~/.config/jarvish/config.toml`)
- `serde::Deserialize` + `#[serde(default)]` でデフォルト値をサポート

## ロギング
- `tracing` クレートを使用
- `tracing-appender` でファイルログ出力

## 非同期
- `tokio` ランタイム (`#[tokio::main]`)
- `async-openai` でAI APIとの通信

## ドキュメントファイル
- `docs/` フォルダ配下のMarkdownファイルのファイル名は**全て大文字**にすること
