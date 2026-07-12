# Changelog

このプロジェクトに対するすべての注目すべき変更を記録します。
フォーマットは [Keep a Changelog](https://keepachangelog.com/ja/1.1.0/) に基づいています。

## [v1.15.0](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.15.0) - 2026-07-12

### Added

- 起動時 rc スクリプト (`rc.jsh`) ローダーを実装
  - 対話起動時に `~/.config/jarvish/rc.jsh` を自動読み込み（`[startup].commands` より前に実行）。初回対話起動時にコメント付きテンプレートを自動生成
  - `--rcfile <path>`（既定の rc.jsh の代わりに指定ファイルを読み込む） / `--no-rc`（読み込みをスキップ）CLI オプションを追加
  - `source` ビルトインをスクリプトファイル (`.jsh`) の実行に対応。ネスト呼び出し・深度上限による無限ループ防止に対応
  - FIFO・巨大ファイル・symlink など不正入力からのファイル読み書き保護、実行器セマンティクスの対話モードへの統一

### Fixed

- `-c` 単体実行・rc.jsh 内 `exit` 終了時に zsh 補完デーモンが孤児プロセス化する不具合を tombstone 方式で根絶
- prewarm の二重 spawn 破棄経路を有界同期 shutdown に統一し、事前ウォームアップ由来の孤児デーモンを解消

### Changed

- 補完デーモンの shutdown を全終了経路で有界同期化し、kill/reap の漏れを解消
- 補完メガ機能のドキュメント総点検（`CLAUDE.md` 設定節・`reload_config` doc コメントの陳腐化を修正）

## [v1.14.0](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.14.0) - 2026-07-12

### Added

- Tab 補完システムを全面刷新 ([#88](https://github.com/tominaga-h/jarvis-shell/issues/88), [#89](https://github.com/tominaga-h/jarvis-shell/issues/89))
  - `complete` ビルトイン（登録 / 一覧 / 消去）を追加。静的候補・動的候補（`$(...)`）・条件評価（`-n <word>`）に対応し、`RegistryProvider` 経由で即座に Tab 補完へ反映
  - carapace 外部補完プロバイダを追加。`carapace` バイナリ検出時に自動で有効化、`cd` の dirs-only 防御フィルタ・設定ホットリロードに対応
  - zsh 補完ブリッジを追加。`zsh -i` を常駐デーモン化して起動コストを削減（既定 `external_zsh_daemon = true`）、シェル起動直後に事前ウォームアップ。ワンショット方式にも切り替え可能
  - alias 対応補完を実装。`Shell.aliases` を `Arc<RwLock<HashMap>>` 化して補完系と実行系で共有
  - `CompletionProvider` trait 化により既存 3 補完（コマンド / パス / git）を移植し、orchestrator が複数プロバイダを連鎖評価する構成へ再編
  - `[completion]` セクションに `external` / `external_timeout_ms` / `external_zsh_daemon` / `git_branch_commands` を追加

### Changed

- ビルトインコマンド一覧を単一テーブルに一元化 (`src/engine/builtins/mod.rs`)

## [v1.13.3](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.13.3) - 2026-07-05

### Fixed

- 先頭トークン（コマンド位置）に `./target/debug/` のような相対/絶対パスを入力しても、Tab 補完候補が出ず「NO RECORDS FOUND」になる不具合を修正 ([#321](https://github.com/tominaga-h/jarvis-shell/issues/321))
  - 実行可能ファイルがそのパスに存在するのに、先頭トークンでは `$PATH` 走査とビルトインのみが対象で、相対/絶対パスがまったく補完されなかった
  - 補完ディスパッチ (`src/cli/completer/mod.rs`) に純粋関数 `looks_like_path()` を追加。先頭トークンが `/` を含む、または `~` で始まる場合は `complete_path(dirs_only=false)` へ委譲し、ファイル・ディレクトリの両方を補完するよう修正（`./` `../` `/abs/` `~/` および中間に `/` を含むトークンに対応）
  - `complete_command`（PATH コマンド補完）は無変更のまま単一責務を維持

## [v1.13.2](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.13.2) - 2026-06-22

### Fixed

- 通常コマンドの出力に "farewell" 等の goodbye パターンを含むパスがあると、それを AI の別れの挨拶と誤検知してシェルが終了してしまう致命的な不具合を修正
  - 例: 未追跡ファイルに `...corporate-farewell-...WIP.md` のようなパスがあるリポジトリで `git status` を実行すると、出力末尾が goodbye とみなされ `jarvish` が終了していた
  - 原因は goodbye 検出が AI（Jarvis）の発話だけでなく人間が打った通常コマンドの stdout にまで適用されていたこと (`src/shell/input.rs`)
  - goodbye 検出を AI 応答経路（自然言語応答・AI パイプ）のみに限定。判定を純粋関数 `should_exit_on_goodbye()` に切り出し、回帰テストを追加

## [v1.13.1](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.13.1) - 2026-06-18

### Changed

- 起動時のウェルカムバナーのデザインを刷新（表示のみの変更、機能への影響なし）
  - ASCII ロゴをよりコンパクトな新デザインに差し替え、配色を赤系へ変更
  - 二重線セパレータ＋独立バージョン行を廃止し、ロゴ幅に合わせた一本の細線セパレータの右端にバージョンタグを配置するレイアウトに変更

## [v1.13.0](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.13.0) - 2026-06-16

### Added

- プロンプト内のコマンド置換 `$(...)` および backtick `` `...` `` に対応 ([#266](https://github.com/tominaga-h/jarvis-shell/issues/266))
  - コマンドの出力を別のコマンドの引数に展開（`echo $(echo hello)` → `hello`）。ネスト（`$(echo $(echo x))`）と単語途中への埋め込み（`prefix-$(echo mid)-suffix`）に対応
  - クォート無しの結果は空白で単語分割（連続空白は畳む）、ダブルクォート内は分割せず内部空白を保持、シングルクォート内はリテラル扱い
  - 置換結果の末尾改行はすべて除去。置換内コマンドの失敗（起動失敗・非ゼロ終了）は外側コマンドを中断し終了コード 1 を返す
  - ネストは深さ 32 までに制限（暴走防止）
  - `src/engine/expand/command_subst.rs` を新設し、トークナイザ（`split_quoted`）が `$(...)` / backtick span をアトミックに取り込むよう拡張。展開順序は「コマンド置換 → チルダ/環境変数 → ブレース → グロブ」

## [v1.12.0](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.12.0) - 2026-06-02

### Fixed

- コマンド実行中の `Ctrl+C` で jarvish のプロセスそのものが終了してしまう不具合を修正 ([#189](https://github.com/tominaga-h/jarvis-shell/issues/189))
  - ジョブ制御を導入し、外部コマンドを `setpgid` で独立したプロセスグループへ分離。実行中は `tcsetpgrp` で端末の前面プロセスグループを子へ委譲し、終了後に jarvish へ回収する
  - これにより `Ctrl+C` は実行中の子プロセスグループにのみ配送され、コマンドだけが中断してプロンプトへ戻る（シェルは継続）
  - `sleep 100`（PTY 経路）に加え、リダイレクト付き・パイプライン・AI パイプ前段（レガシー/captured 経路）でもシェルが落ちなくなった
  - プロンプト入力中の `Ctrl+C`（reedline）および AI 応答ストリーム中の `Ctrl+C` の挙動は従来どおり維持

## [v1.11.0](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.11.0) - 2026-05-18

### Added

- `cdhist` / `cdj` ビルトインを追加 ([#127](https://github.com/tominaga-h/jarvis-shell/issues/127))
  - `cdhist [--limit N]` — `command_history.cwd` を LRU 順で重複排除して 1 行 1 件出力。現在の cwd と存在しないパスは除外
  - `cdj [pattern]` — 履歴ディレクトリから fzf で選んで cd。`pattern` は case-insensitive substring 絞り込み、単一マッチなら fzf を起動せず即 cd、キャンセル時は cwd 不変 (exit 130)
  - ストレージはスキーマ追加なし、既存 `command_history` を読み取るのみ
  - fzf 連携部分は zoxide の `src/util.rs::Fzf` / `src/cmd/query.rs::get_fzf` パターンを踏襲
  - fzf プレビューウィンドウ対応 (UNIX のみ): 選択中ディレクトリの `ls -Cp` を下 30% に表示。macOS は色付き、Linux は `--group-directories-first` 付き

## [v1.10.0](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.10.0) - 2026-05-18

### Added

- グロブ展開とブレース展開を追加 ([#126](https://github.com/tominaga-h/jarvis-shell/issues/126))
  - グロブ: `*`, `?`, `[abc]`, `[a-z]`（`glob` クレート使用）
  - ブレース: `{a,b,c}`, `{1..5}`, `{01..03}`（ゼロパディング保持）, `{5..1}` 降順, `{1..10..2}` ステップ, `{a..e}` 文字レンジ, ネスト, エスケープ
  - 適用範囲: 外部コマンド + シェルビルトイン（`cd`, `source` 等）
  - 展開順序: チルダ/環境変数 → ブレース → グロブ
  - クォート尊重: `'*'`, `"{a,b}"`, `\*` は展開されずリテラル扱い
  - no-match 時は zsh 互換でエラー終了（`jarvish: no matches found: <pattern>`, exit code 1）

## [v1.8.3](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.8.3) - 2026-04-03

### Fixed

- `sigusr1_handler_can_be_reregistered` テストのスリープ時間を延長し CI でのフレーキーテストを解消

### Changed

- Claude コマンドファイルに frontmatter メタデータを追加
- 開発サイクル（実装→完了の必須フロー）を CLAUDE.md に明文化
- Cargo.lock の依存パッケージバージョンを同期
- 不要な `.claude/settings.json` を削除

## [v1.8.2](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.8.2) - 2026-04-03

### Fixed

- `lib.rs` 作成によりテスト実行問題を修正
- `perform_local_update` のテストを安全化 ([#19](https://github.com/tominaga-h/jarvis-shell/issues/19))

### Changed

- Cargo.lock の依存パッケージバージョンを同期

## [v1.8.1](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.8.1) - 2026-04-03

### Added

- `update --local` のテスト 5 件を追加（Fury 監査指摘対応）

## [v1.8.0](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.8.0) - 2026-04-03

### Added

- `update --local` オプションを追加: ローカルバイナリからの更新機能（デフォルトパス `target/release/jarvish` またはカスタムパス指定）

## [v1.7.3](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.7.3) - 2026-04-03

### Added

- テストカバレッジ強化: P0/P1 項目に 27 テストを追加（累計 477 テスト）

## [v1.7.2](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.7.2) - 2026-04-03

### Fixed

- `update --check` の semver 比較バグを修正 ([#31](https://github.com/tominaga-h/jarvis-shell/issues/31))

### Changed

- SIGUSR1 による自動再起動を廃止し、フラグファイル通知方式に変更（安定性向上）([#31](https://github.com/tominaga-h/jarvis-shell/issues/31))

## [v1.7.1](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.7.1) - 2026-04-03

### Added

- 自己更新・再起動メカニズムのテストを追加 ([#31](https://github.com/tominaga-h/jarvis-shell/issues/31))

## [v1.7.0](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.7.0) - 2026-04-03

### Added

- `update` ビルトインコマンドを追加: GitHub Releases からの自己更新機能 ([#31](https://github.com/tominaga-h/jarvis-shell/issues/31))
- `update --check` オプション: インストールせずに新バージョンの有無を確認
- Homebrew インストールの自動検知と `brew upgrade` への案内
- 更新完了後の自動再起動

### Changed

- release コマンドの homebrew-tap 記述を変更

## [v1.6.3](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.6.3) - 2026-04-03

### Fixed

- タイポ補正プロンプトで n (Reject) を選択すると AI が走ってしまう問題を修正
- タイポ補正・自動調査の確認プロンプトで Ctrl+C を押すとシェルプロセスが終了する問題を修正

### Changed

- `read_line_ignoring_sigint()` ヘルパーを導入し、対話プロンプトでの SIGINT ハンドリングを共通化

## [v1.6.1](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.6.1) - 2026-04-01

### Fixed

- 非対話モード（`-c` オプション）実行時に自動調査が暴走し、AI 失敗時にカスケード障害が発生する問題を修正
- Starship プロンプトのレンダリングが崩れるバグを修正

### Changed

- Starship プロンプトのキャッシュ実装によりプロンプト描画パフォーマンスを向上
- リリースコマンド実行時に Homebrew Formula の更新ステップを追加

## [v1.6.0](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.6.0) - 2026-03-26

### Added

- タイポ補正: 存在しないコマンド入力時に PATH 上の類似コマンドを提示する zsh 互換機能を追加（Damerau-Levenshtein 距離による転置検出対応）([#83](https://github.com/tominaga-h/jarvis-shell/issues/83))

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
