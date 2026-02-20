# Changelog

このプロジェクトに対するすべての注目すべき変更を記録します。
フォーマットは [Keep a Changelog](https://keepachangelog.com/ja/1.1.0/) に基づいています。

## [v1.0.0](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.0.0) - 2026-02-15

Jarvis Shell の最初の正式リリース。Phase 1 (REPL & 実行エンジン)、Phase 2 (永続化)、Phase 3 (AI統合) を実装。

### Added

#### コアアーキテクチャ

- reedline による対話型 REPL ループの構築
- ビルトインコマンドの実装 (`cd`, `exit`, `cwd`) と `std::env::set_current_dir` による正しいディレクトリ変更
- 外部コマンド実行エンジン (`std::process::Command`)
- os_pipe を用いた I/O Capture (tee) — stdout/stderr をユーザーに表示しつつバッファに複製
- 環境変数の展開機能
- パイプ・リダイレクト対応 ([#10](https://github.com/tominaga-h/jarvis-shell/issues/10))

#### The Black Box (永続化)

- SQLite による履歴DB (`history.db`) — コマンド、タイムスタンプ、CWD、終了コード、Blob Hash を記録
- コンテンツアドレッサブル Blob ストレージ (`blobs/`) — SHA-256 ハッシュ + zstd 圧縮
- XDG_DATA_HOME (`~/.local/share/jarvish/`) 準拠のデータディレクトリ
- デバッグログ出力

#### AI 統合 (J.A.R.V.I.S.)

- OpenAI API クライアントによる AI 統合
- ユーザー入力の自然言語/コマンド分類アルゴリズム
- 過去の実行ログ (stderr) をコンテキストとして AI に渡す仕組み
- 直前のコマンドが異常終了した場合、Jarvis が自動調査
- Jarvis AI スマート化 — Tool Call 対応 ([#8](https://github.com/tominaga-h/jarvis-shell/issues/8))
- Ctrl-C で Jarvis の応答を停止可能 ([#21](https://github.com/tominaga-h/jarvis-shell/issues/21))
- Jarvis が実行したコマンドも履歴に登録 ([#28](https://github.com/tominaga-h/jarvis-shell/issues/28))

#### UX

- ユーザー入力のシンタックスハイライト ([#2](https://github.com/tominaga-h/jarvis-shell/issues/2))
- 自然言語入力はハイライトしない ([#15](https://github.com/tominaga-h/jarvis-shell/issues/15))
- 読み込み中の Spinner 表示 ([#4](https://github.com/tominaga-h/jarvis-shell/issues/4))
- File 読み書き中にも Spinner 表示
- コマンド履歴からの補完 ([#24](https://github.com/tominaga-h/jarvis-shell/issues/24))
- 右プロンプトに現在時刻表示 ([#20](https://github.com/tominaga-h/jarvis-shell/issues/20))
- vim/less 等ページャコマンド対応 ([#7](https://github.com/tominaga-h/jarvis-shell/issues/7))
- Welcome/Goodbye メッセージ (ASCII Art 付き)
- 自動 goodbye 実装と終了コード調整 ([#19](https://github.com/tominaga-h/jarvis-shell/issues/19))
- 色の出力対応 (nu_ansi_term)

#### ビルトインコマンド

- `help` コマンド ([#30](https://github.com/tominaga-h/jarvis-shell/issues/30))
- 必須ビルトインコマンド (`which`, `type`, `true`, `false`, `export` 等) を clap で定義 ([#29](https://github.com/tominaga-h/jarvis-shell/issues/29))
- PATH の動的キャッシュ

### Changed

- 大規模リファクタリング — モジュール構造の再編成 ([#22](https://github.com/tominaga-h/jarvis-shell/issues/22))
- ログファイル名の変更 ([#23](https://github.com/tominaga-h/jarvis-shell/issues/23))
- ログの出力先を data_dir に変更 ([#31](https://github.com/tominaga-h/jarvis-shell/issues/31))
- ログのタイムゾーンを JST に変更 ([#18](https://github.com/tominaga-h/jarvis-shell/issues/18))
- Talking モードの廃止とアーキテクチャ改善 ([#14](https://github.com/tominaga-h/jarvis-shell/issues/14))
- ビルトインコマンドのリファクタリング
- プロンプト表示名を `jarvish` から `jarvis` に変更

### Fixed

- cd 実行時の環境変数 `PWD` が更新されない問題 ([#32](https://github.com/tominaga-h/jarvis-shell/issues/32))
- data_dir 取得失敗時の潜在バグ ([#33](https://github.com/tominaga-h/jarvis-shell/issues/33))
- シンボリックリンクを辿らない問題
- `cargo test` で出力がバグる問題
- Ctrl-C 時に改行されない問題

### CI/CD

- GitHub Actions テストワークフロー追加
- pre-push フック作成 (clippy, fmt, test のチェック)
- リリースワークフロー (macOS aarch64 / Linux)
