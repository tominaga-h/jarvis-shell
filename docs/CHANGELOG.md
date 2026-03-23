# Changelog

このプロジェクトに対するすべての注目すべき変更を記録します。
フォーマットは [Keep a Changelog](https://keepachangelog.com/ja/1.1.0/) に基づいています。

## [v1.5.0](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.5.0) - 2026-03-24

### Added

- Starship プロンプト連携: `prompt.starship = true` で Starship をプロンプトとしてネイティブサポート ([#57](https://github.com/tominaga-h/jarvis-shell/issues/57))

### Changed

- プロンプトモジュールのリファクタリング: `mod.rs` から `JarvisPrompt` を専用ファイルに分割

## [v1.4.0](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.4.0) - 2026-03-19

### Added

- AI の賢さ改善: プロンプトの改善、再調査時のコンテキスト引き継ぎ、`search_replace` ツールの追加 ([#85](https://github.com/tominaga-h/jarvis-shell/issues/85))
- ブランチ補完の設定可能化: `completion.git_branch_commands` でブランチ補完対象の Git サブコマンドをカスタマイズ可能に ([#84](https://github.com/tominaga-h/jarvis-shell/issues/84), [#86](https://github.com/tominaga-h/jarvis-shell/issues/86))

### Fixed

- `extract_shell_command` のショートサーキット処理を修正 ([#85](https://github.com/tominaga-h/jarvis-shell/issues/85))

### Changed

- AI のコンテキスト認識を改善

## [v1.3.0](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.3.0) - 2026-03-09

### Added

- セッション機能を実装（セッション ID によりコマンド履歴やログを分割）([#75](https://github.com/tominaga-h/jarvis-shell/issues/75), [#78](https://github.com/tominaga-h/jarvis-shell/issues/78))
- `-c` オプションを実装（コマンド文字列を引数で渡して実行）([#81](https://github.com/tominaga-h/jarvis-shell/issues/81))
- 特定のコマンドのみ自動調査を無効化する設定 `ignore_auto_investigation_cmds` を追加 ([#82](https://github.com/tominaga-h/jarvis-shell/issues/82))

### Fixed

- ブランチ補完で現在のブランチが先頭に表示されない問題を修正 ([#76](https://github.com/tominaga-h/jarvis-shell/issues/76))
- テストが失敗する問題を修正

## [v1.2.1](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.2.1) - 2026-03-04

### Added

- `which` / `type` ビルトインコマンドを実装 ([#74](https://github.com/tominaga-h/jarvis-shell/issues/74))
- `pushd` / `popd` / `dirs` ビルトインコマンドを実装 ([#73](https://github.com/tominaga-h/jarvis-shell/issues/73))
- `pwd` コマンドを `cwd` のエイリアスとして追加

### Changed

- README を再構築し、目次を追加（英語版・日本語版）

## [v1.2.0](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.2.0) - 2026-03-04

### Added

- AI リダイレクト機能 (`> ai "..."`) を実装 ([#67](https://github.com/tominaga-h/jarvis-shell/issues/67))
- 機密情報のサニタイズ機能を実装（API キー・トークン値の流出防御）([#68](https://github.com/tominaga-h/jarvis-shell/issues/68))
- AI の `temperature` 設定を `config.toml` で変更可能に ([#66](https://github.com/tominaga-h/jarvis-shell/issues/66))
- AsyncGitState の導入により Git 情報取得時の CPU 使用率を改善 ([#49](https://github.com/tominaga-h/jarvis-shell/issues/49))
- CPU 使用率をデバッグログに組み込み ([#56](https://github.com/tominaga-h/jarvis-shell/issues/56))

### Fixed

- jarvish 内で jarvish を再帰的に実行できないバグを修正 ([#71](https://github.com/tominaga-h/jarvis-shell/issues/71))
- エイリアスが解除されたコマンドが履歴に残るバグを修正 ([#65](https://github.com/tominaga-h/jarvis-shell/issues/65))
- 日本語版 README のリンク切れを修正

### Changed

- 全体リファクタリングを実施（モジュール構造の整理）([#69](https://github.com/tominaga-h/jarvis-shell/issues/69))
- reedline を 0.45 にアップデート
- version バッヂにリリースページへのリンクを追加 ([#64](https://github.com/tominaga-h/jarvis-shell/issues/64))
- 新しいデモ GIF を作成 ([#63](https://github.com/tominaga-h/jarvis-shell/issues/63))
- AI リダイレクトとマスキングについて README に追記
- release コマンドの手順を調整・改善

## [v1.1.2](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.1.2) - 2026-03-03

### Added

- 機密情報のサニタイズ機能を実装（API キー・トークン値の流出防御）([#68](https://github.com/tominaga-h/jarvis-shell/issues/68))
- AI の `temperature` 設定を `config.toml` で変更可能に ([#66](https://github.com/tominaga-h/jarvis-shell/issues/66))

### Fixed

- エイリアスが解除されたコマンドが履歴に残るバグを修正 ([#65](https://github.com/tominaga-h/jarvis-shell/issues/65))

### Changed

- version バッヂにリリースページへのリンクを追加 ([#64](https://github.com/tominaga-h/jarvis-shell/issues/64))
- 新しいデモ GIF を作成 ([#63](https://github.com/tominaga-h/jarvis-shell/issues/63))
- release コマンドの手順を調整

## [v1.1.1](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.1.1) - 2026-03-03

### Fixed

- プロンプト Git 情報取得時の CPU 使用率を改善（AsyncGitState の導入）([#49](https://github.com/tominaga-h/jarvis-shell/issues/49))
- 日本語版 README のリンク切れを修正

### Changed

- CPU 使用率をデバッグログに組み込み ([#56](https://github.com/tominaga-h/jarvis-shell/issues/56))
- reedline を 0.45 にアップデート
- release コマンドの手順を追加・改善

## [v1.1.0](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.1.0) - 2026-03-02

### Added

- AI パイプ機能 (`| ai "..."`) の実装 ([#58](https://github.com/tominaga-h/jarvis-shell/issues/58))
- システムプロンプトに README の内容を注入 ([#59](https://github.com/tominaga-h/jarvis-shell/issues/59))
- オフライン検知によるバナー表示の動的切り替え
- AI 応答の Markdown 判定ロジックの追加
- `git push` 時のブランチ補完対応

### Fixed

- push 時と PR 作成時に CI Workflow が同時実行される問題を修正

## [v1.0.2](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.0.2) - 2026-03-01

### Added

- AI 応答の Markdown レンダリング対応 ([#27](https://github.com/tominaga-h/jarvis-shell/issues/27))
- Markdown レンダリングの設定オプション (`markdown_rendering`)
- `--version` / `-v` オプションの追加 ([#52](https://github.com/tominaga-h/jarvis-shell/issues/52))
- Git エイリアスでのブランチ補完対応 ([#54](https://github.com/tominaga-h/jarvis-shell/issues/54))

### Changed

- `source` コマンドの出力結果を改善・表示変更 ([#55](https://github.com/tominaga-h/jarvis-shell/issues/55))
- README の整備（ロゴ削除、不要な絵文字の削除、デモ GIF 追加）

### Fixed

- CPU 使用率のバグを修正 ([#47](https://github.com/tominaga-h/jarvis-shell/issues/47))
- PATH キャッシュ問題を Fish Shell 方式でキャッシュレス化して解決 ([#51](https://github.com/tominaga-h/jarvis-shell/issues/51))

## [v1.0.1](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.0.1) - 2026-02-20

### Added

- 設定ファイル `config.toml` の読み書き対応 ([#17](https://github.com/tominaga-h/jarvis-shell/issues/17))
- Git ブランチ補完の実装 ([#36](https://github.com/tominaga-h/jarvis-shell/issues/36))
- プロンプトに Git 情報を表示 ([#38](https://github.com/tominaga-h/jarvis-shell/issues/38))
- ディレクトリ付き履歴出力 ([#39](https://github.com/tominaga-h/jarvis-shell/issues/39))
- `--debug` オプションによるローカルログ出力 ([#40](https://github.com/tominaga-h/jarvis-shell/issues/40))
- NerdFont 設定の実装 ([#42](https://github.com/tominaga-h/jarvis-shell/issues/42))
- `source` ビルトインコマンドの実装 ([#44](https://github.com/tominaga-h/jarvis-shell/issues/44))
- `alias` / `unalias` ビルトインコマンドの実装 ([#45](https://github.com/tominaga-h/jarvis-shell/issues/45))

### Changed

- Help メッセージを英語化 ([#46](https://github.com/tominaga-h/jarvis-shell/issues/46))
- README に追記・更新 ([#43](https://github.com/tominaga-h/jarvis-shell/issues/43))
- ロゴを README に追加
- CHANGELOG を作成

### Fixed

- `&&` が動かない問題を修正 ([#34](https://github.com/tominaga-h/jarvis-shell/issues/34))
- `$HOME` 使用時に補完が効かない問題を修正 ([#35](https://github.com/tominaga-h/jarvis-shell/issues/35))
- 並列テストでエラーが出る問題を修正
- テストの失敗を修正
- Clippy エラーを修正

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
