# 🤵 Jarvis Shell (jarvish)

[![status](https://img.shields.io/github/actions/workflow/status/tominaga-h/jarvis-shell/ci.yml)](https://github.com/tominaga-h/jarvis-shell/actions)
![version](https://img.shields.io/badge/version-UNDER_DEVELOPMENT-red)

> 🌐 [English README](../README.md)

---

## 💡 概要

> _「アイアンマンの J.A.R.V.I.S. のような相棒が欲しい。ただし、ターミナルの中で。」_

**Jarvish** は、Marvel の Iron Man に登場する **J.A.R.V.I.S.** にインスパイアされた、Rust 製の **次世代 AI 統合シェル (Next Generation AI Integrated Shell)** です。日常のシェル体験に AI の知性をネイティブに組み込みます。エラーをブラウザにコピペする必要はもうありません。Jarvis に聞くだけです。

![jarvish](../images/jarvish.png)

⚠️ **注意:** Jarvish はまだ**開発中**です。 Issueは[こちら](https://github.com/tominaga-h/jarvis-shell/issues?q=is%3Aissue%20state%3Aopen%20milestone%3A%22Version%201.0.0%22)

---

## ✨ 主な機能

### 🧠 AI アシスタント

- 💬 シェルプロンプトから直接、**自然言語**で Jarvis と会話
- 🔍 コマンドが失敗すると、Jarvis が stdout/stderr のコンテキストを使って**自動的にエラーを調査**
- 🛠️ Jarvis は AI エージェントとして**ファイルの読み書き**やコマンド実行が可能（ツールコール機能）

### 🐟 Fish ライクな UX

- 🎨 入力中の**リアルタイムシンタックスハイライト**
- ⚡ コマンド（PATH バイナリ、ビルトイン）とファイルパスの**オートコンプリート**
- 📜 `reedline` による履歴ベースのサジェスト

### 📦 The Black Box

- 🗃️ すべてのコマンド実行が**永続化** — コマンド、タイムスタンプ、作業ディレクトリ、終了コード
- 💾 stdout/stderr の出力は **Git ライクなコンテンツアドレッサブル Blob ストレージ**に保存（SHA-256 + zstd 圧縮）
- 🔄 シェルを再起動しても、*「先週のエラー」*について Jarvis に相談可能

### 🔧 シェルの基本機能

- 🔀 **パイプライン** (`cmd1 | cmd2 | cmd3`)
- 📂 **リダイレクト** (`>`, `>>`, `<`)
- 🏠 **チルダ・変数展開** (`~`, `$HOME`, `${VAR}`)
- 📟 対話型プログラム（vim, top 等）のための完全な **PTY サポート**

---

## 🚀 はじめに

### 前提条件

| 必要なもの             | 詳細                                  |
| ---------------------- | ------------------------------------- |
| 🦀 **Rust**            | Stable ツールチェイン（Edition 2021） |
| 🔑 **OpenAI API キー** | AI 機能に必要                         |
| 💻 **OS**              | macOS / Linux                         |

### ビルド

```bash
git clone https://github.com/tominaga-h/jarvis-shell.git
cd jarvis-shell
cargo build --release
```

### 設定

プロジェクトルートに `.env` ファイルを作成してください（`.env.example` を参照）：

```bash
OPENAI_API_KEY=your_openai_api_key
```

### 起動

```bash
./target/release/jarvish
```

---

## 🏗️ アーキテクチャ

Jarvish は4つのコアコンポーネントで構成されています：

```mermaid
graph TB
    User(["ユーザー"]) --> A["Line Editor (reedline)"]
    A --> B["Execution Engine"]
    B --> B1["ビルトインコマンド (cd, cwd, exit)"]
    B --> B2["外部コマンド (PTY + I/O キャプチャ)"]
    B --> D["AI Brain (OpenAI API)"]
    B2 --> C["Black Box"]
    D --> C
    C --> C1[("history.db (SQLite)")]
    C --> C2[("blobs/ (SHA-256 + zstd)")]
```

| コンポーネント          | 説明                                                                                         |
| ----------------------- | -------------------------------------------------------------------------------------------- |
| 🖊️ **Line Editor**      | `reedline` による REPL インターフェース。シンタックスハイライト、補完、履歴機能を提供        |
| ⚙️ **Execution Engine** | 入力をビルトインコマンドまたは外部コマンドに振り分け、PTY テーイングで I/O をキャプチャ      |
| 📦 **Black Box**        | すべての実行履歴と出力を永続化（SQLite インデックス + コンテンツアドレッサブル Blob ストア） |
| 🧠 **AI Brain**         | 入力をコマンド/自然言語に分類し、OpenAI を通じてコンテキストを踏まえた AI アシスタンスを提供 |

---

## 🛠️ 技術スタック

| カテゴリ         | クレート         | 用途                                 |
| ---------------- | ---------------- | ------------------------------------ |
| ラインエディタ   | `reedline`       | Fish ライクな対話型行編集            |
| プロセス管理     | `os_pipe`, `nix` | I/O キャプチャ、PTY 管理             |
| 非同期ランタイム | `tokio`          | 非同期ランタイム                     |
| データベース     | `rusqlite`       | コマンド履歴用 SQLite                |
| ハッシュ         | `sha2`           | SHA-256 コンテンツハッシュ           |
| 圧縮             | `zstd`           | Blob 圧縮                            |
| AI               | `async-openai`   | OpenAI API クライアント              |
| パス解決         | `directories`    | XDG 準拠のパス解決                   |
| ターミナル       | `nu-ansi-term`   | ANSI カラースタイリング              |
| ロギング         | `tracing`        | 日次ローテーション付き構造化ロギング |

---

## 👩‍💻 開発

### Git Hooks

```bash
make install-hooks   # pre-push フックをインストール
make uninstall-hooks # pre-push フックを削除
```

### チェック実行

```bash
make check  # format, clippy, check, test を一括実行
```

### CI パイプライン (GitHub Actions)

すべての push と `main` への PR で CI が実行されます：

| ジョブ    | コマンド                                    |
| --------- | ------------------------------------------- |
| ✅ Check  | `cargo check --all-targets`                 |
| 🧪 Test   | `cargo test --all-targets`                  |
| 📐 Format | `cargo fmt --all -- --check`                |
| 📎 Clippy | `cargo clippy --all-targets -- -D warnings` |
