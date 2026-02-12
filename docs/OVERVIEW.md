# プロジェクト要件定義書: Jarvis Shell (jarvish)

## 開発背景とユーザー体験

### 背景

> **アイアンマンのJ.A.R.V.I.S.のような相棒が欲しい。ただし、ターミナルの中で。**

本プロジェクトの目的は、普段使うシェルに、映画アイアンマンにでてくるJ.A.R.V.I.S.のようなAIの知性をネイティブに組み込んだ **次世代AIシェル (Next Generation AI Integrated Shell)** をRustで構築することです。

### 目指す体験

1. **Jarvis Support**: エラーが出たり、ビルドに失敗しても、もうググる必要はありません。標準出力や標準エラーの内容をもとに、Jarvisがその原因を調査し、解決策を提示してくれます。
2. **Fish-like UX**: 入力中のリアルタイムなシンタックスハイライト、コマンド履歴に基づく強力なオートサジェスト。
3. **Deep Context Awareness (The Black Box)**: Git のように、過去のコマンド実行結果（stdout/stderr）や AI との対話を永続化・インデックス化する。これにより、シェル再起動後でも「先週のエラー」について AI に相談できる。
4. **The "Iron Man" Vibe**: 映画の J.A.R.V.I.S.を感じさせる、有能な執事としての振る舞い。

## プロジェクト概要

**Jarvis Shell (jarvish)** は、Rust 製の対話型コマンドラインシェルです。ユーザーは既存のターミナルアプリ上で jarvish を実行し、これをメインのシェルとして使用します。

## Core Architecture

### 【A】 The Line Editor

- **役割**: reedline を用いたプロンプトの表示、ユーザー入力の受け付け(REPL)。

### 【B】 The Execution Engine

- **役割**: コマンド実行の振り分け。
- **Builtin Commands**: シェルの状態を変更するコマンド（cd, exit, export 等）は、**必ずシェルプロセス内部で処理**しなければならない。
  - ⚠️ **Pitfall**: cd を外部プロセス ( `/usr/bin/cd` ) として実行しても、親プロセス（シェル）のカレントディレクトリは変わらないため、必ず `std::env::set_current_dir` を使用すること。
- **External Commands**: それ以外（ls, grep, git 等）は `std::process::Command` で子プロセスとして起動する。
- **I/O Capture (Teeing)**: 子プロセスの実行中、stdout/stderr をユーザーに見せつつ（継承またはパイプ）、同時にメモリ上のバッファにも複製して保存する。

### 【C】 The Black Box

- **役割**: すべての履歴とコンテキストを永続化する。
- **Storage Strategy**:
  - **Index (history.db)**: SQLite を使用。メタデータ（コマンド、Timestamp、CWD、Exit Code、Blob Hash）を記録。
  - **Blob Storage (blobs/)**: Git のようなコンテンツアドレッサブルストレージ。実行結果（stdout/stderr）や AI 回答テキストを SHA-256 でハッシュ化し、zstd 圧縮して保存する。
- **Directory**: **XDG_DATA_HOME** ( `~/.local/share/jarvish/` ) に準拠。

### 【D】 The AI - J.A.R.V.I.S. - Brain

- **役割**: 自然言語の解釈。ユーザーが入力したものが単なるコマンドが自然言語かを解釈する。
- **Context Retrieval**: ユーザーが「Jarvis（または略称「J」でもOK）, さっきのエラーは？」と聞いた時、history.db から直前のコマンドの Blob Hash を引き、blobs/ から展開したテキストを System Prompt に注入する。

## 技術スタック (Rust)

- **Language**: Rust (Edition 2021)
- **Line Editor**: reedline (Fish ライクな UX を実現するライブラリ)
- **Process Management**: std::process, os_pipe (I/O キャプチャ用)
- **Async Runtime**: tokio
- **Database**: rusqlite (SQLite)
- **Hashing & Compression**: sha2 (SHA-256), zstd (ログ圧縮)
- **Directories**: directories (クロスプラットフォームなパス解決)
- **AI Client**: async-openai
- **Config**: config, serde

## 実装ロードマップ

**IMPORTANT**: Phase 1 での「ビルトインコマンドの実装」と「I/O キャプチャ」が最重要です。

### Phase 1: The Basic REPL & Execution Engine

- **ゴール**: ls が動き、cd で移動でき、exit で終了できるシェル。
- **タスク**:
  - Cargo.toml セットアップ（reedline, tokio, os_pipe 等）。
  - reedline で REPL ループ構築。
  - engine/builtin.rs:
    - cd: 引数を受け取り、std::env::set_current_dir を実行するロジック。
    - cwd: 現在のディレクトリを出力するロジック。
    - exit: ループを抜けるシグナルを返すロジック。
  - engine/exec.rs:
    - 外部コマンド実行の実装。
    - **重要**: os_pipe を使い、子プロセスの出力を「親プロセスの stdout に流す」処理と「メモリ上のバッファに溜める」処理を同時に行う（tee のような）実装を行う。

### Phase 2: The Black Box (Persistence)

- **ゴール**: 実行ログをファイルと DB に保存する。
- **タスク**:
  1. storage/mod.rs: rusqlite で history.db を初期化・マイグレーションする処理。
  2. storage/blob.rs: 文字列を受け取り、圧縮・ハッシュ化して ~/.local/share/jarvish/blobs/ に保存するロジック。
  3. REPL ループ終了時（コマンド完了時）に、メモリ上のバッファを Blob 化し、DB にインサートする処理を統合。

### Phase 3: AI Integration & Context Retrieval

- **ゴール**: 過去のログを参照して AI が回答する。
- **タスク**:
  1. ai/client.rs: OpenAI API クライアント。
  2. ユーザーのプロンプトをAIを介して自然言語かコマンドかを解釈する。
  3. DB から「直前のコマンド（あるいは指定した ID のコマンド）」の stderr_hash を取得し、Blob を復元してプロンプトに含める。
  4. 自然言語の場合、ユーザーのプロンプトとコンテキストから、回答を生成する。(API経由)
  5. コマンドの場合、コマンドを実行する。

### Phase 4: Fish-like UX & Special Features

- **タスク**: シンタックスハイライト、オートサジェスト、mark コマンド等の実装。

**Instruction for Cursor:**

まずは **Phase 1: The Basic REPL & Execution Engine** から着手します。

Cargo.toml の作成と、cd が正しくビルトインとして動作する src/engine/builtin.rs およびメインループの実装から提案してください。
