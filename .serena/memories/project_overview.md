# Jarvis Shell (jarvish) - プロジェクト概要

## 目的
アイアンマンのJ.A.R.V.I.S.にインスパイアされた、次世代AI統合シェル。
Rustで構築された対話型コマンドラインシェルで、AIによるエラー調査・解決策提示、
Fish-likeなUX（シンタックスハイライト・オートサジェスト）、コマンド履歴の永続化を提供する。

## 技術スタック
- **言語**: Rust (Edition 2021, MSRV 1.75)
- **ラインエディタ**: reedline
- **非同期ランタイム**: tokio
- **データベース**: rusqlite (SQLite, bundled)
- **AI**: async-openai (OpenAI API)
- **CLI引数解析**: clap (derive)
- **設定ファイル**: toml + serde
- **ロギング**: tracing + tracing-subscriber + tracing-appender
- **エラーハンドリング**: anyhow
- **プロセス管理**: os_pipe, nix (PTY)
- **その他**: git2, chrono, sha2, zstd, directories, which

## バイナリ名
`jarvish` (src/main.rs がエントリーポイント)

## 環境変数
- `OPENAI_API_KEY` が必要 (.env ファイルで管理、dotenvy で読み込み)
- 設定ファイル: `~/.config/jarvish/config.toml`

## コードベース構成
```
src/
├── main.rs          # エントリーポイント (tokio::main)
├── config.rs        # 設定ファイル管理 (~/.config/jarvish/config.toml)
├── logging.rs       # ロギング初期化
├── shell/           # シェルコア
│   ├── mod.rs       # Shell構造体、REPLメインループ
│   ├── editor.rs    # reedlineエディタ設定
│   ├── input.rs     # ユーザー入力処理
│   ├── ai_router.rs # AIルーティング
│   └── investigate.rs # エラー調査
├── engine/          # コマンド実行エンジン
│   ├── mod.rs       # モジュール定義
│   ├── dispatch.rs  # コマンド振り分け
│   ├── exec.rs      # 外部コマンド実行
│   ├── pty.rs       # PTY管理
│   ├── parser.rs    # コマンドパーサー
│   ├── classifier.rs # コマンド分類
│   ├── redirect.rs  # リダイレクト処理
│   ├── io.rs        # I/Oキャプチャ
│   ├── expand.rs    # 変数展開
│   ├── terminal.rs  # ターミナル制御
│   └── builtins/    # ビルトインコマンド
│       ├── mod.rs, cd.rs, cwd.rs, exit.rs,
│       ├── export.rs, unset.rs, history.rs, help.rs
├── ai/              # AI統合
│   ├── mod.rs       # モジュール定義
│   ├── client.rs    # OpenAI APIクライアント
│   ├── prompts.rs   # プロンプト定義
│   ├── stream.rs    # ストリーミング応答
│   ├── types.rs     # AI関連型定義
│   └── tools/       # AIツール呼び出し
│       ├── mod.rs, definitions.rs, executor.rs, call.rs
├── storage/         # 永続化 (The Black Box)
│   ├── mod.rs       # ストレージ初期化
│   ├── history.rs   # 履歴DB (SQLite)
│   └── blob.rs      # Blobストレージ (SHA-256 + zstd)
└── cli/             # CLI UI
    ├── mod.rs       # モジュール定義
    ├── prompt.rs    # プロンプト表示
    ├── banner.rs    # 起動バナー
    ├── color.rs     # カラーユーティリティ
    ├── highlighter.rs # シンタックスハイライト
    ├── completer.rs # コマンド補完
    └── jarvis.rs    # Jarvisメッセージ表示
```
