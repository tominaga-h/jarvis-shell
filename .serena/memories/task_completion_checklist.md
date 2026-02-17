# タスク完了時のチェックリスト

タスクを完了した際、以下を確認すること:

## 1. コードフォーマット
```bash
cargo fmt --all
```

## 2. コンパイルチェック
```bash
RUSTFLAGS="-Dwarnings" cargo check --all-targets
```

## 3. Clippy lint
```bash
cargo clippy --all-targets -- -D warnings
```

## 4. テスト実行
```bash
cargo test --all-targets
```

## 一括実行
```bash
make check
```
これは上記すべてを順番に実行する（fmtは自動修正モード）。

## 注意
- **サンドボックスでは `cargo build` / `cargo test` / `cargo check` を実行しないこと**（ワークスペースルール）
- `docs/` フォルダのMarkdownファイル名は全て大文字
- 毎回 `docs/OVERVIEW.md` を読み込みプロジェクト概要を確認すること
