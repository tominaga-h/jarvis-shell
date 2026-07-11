# TODO: コマンド補完網羅 + スクリプト設定ファイル

詳細は [tasks/plan.md](plan.md) を参照。実装順: フェーズ内は依存順、フェーズ間は 1 → 2a → 2b → 3 → 4。

## Phase 1: 補完パイプライン基盤（branch: `feature/completion`）

- [x] 1.1 寛容スキャナ + CompletionContext（`e98d0d5`）
- [x] 1.2 builtin リスト一元化（`56f186a`）
- [x] 1.4 aliases の `Arc<RwLock<HashMap>>` 化（`32029a2`）
- [x] 1.3 CompletionProvider trait 化 + command/path/git 移植 + orchestrator（`bf06ab1`）
- [x] 1.5 alias 対応補完（`701ddf9`）
- [ ] **Checkpoint 1**: ✅ 5レンズレビュー（確定3件修正済 `357ed26`, `a4d5b7b`）/ ✅ make check（708+6テスト）/ ✅ カバレッジ調査 / ⬜ 手動 REPL 確認 / ⬜ develop マージ

## Phase 2a: carapace 連携（branch: `feature/completion` 継続 — 2026-07-09 決定）

- [x] 2a.1 外部プロバイダ実行基盤（`90d5f82`、グループkill修正 `5dba9e7`）
- [x] 2a.2 CarapaceProvider（`967479f`、cd防御ガード `97f9fd1`）
- [x] 2a.3 `[completion]` 設定 + ドキュメント4点セット（`416119e`、サマリ修正 `ad61cba`）
- [x] **Checkpoint 2a**: ✅ 7レンズ相当レビュー（確定8件全修正）/ ✅ make check（759+6）/ ✅ カバレッジ補強（`f39bfdf`）

## Phase 2b: zsh ブリッジ（branch: `feature/completion` 継続）

- [x] 2b.1 capture.zsh vendor + ワンショットブリッジ（`9633225`。vendor はユーザー手動 DL + Fable 検分 — 外部コード取り込みは分類器によりサブエージェント不可）
- [x] 2b.2 ブリッジ用 zshrc（`cac8a68`。実体は `~/.config/jarvish/zsh-bridge/.zshrc`、ZDOTDIR 方式 + capture.zsh の `-f` 除去パッチ）
- [x] 2b.3 ウォームデーモン化 — **完了（2026-07-11）**
  - [x] コア実装（`6333128` デーモン基盤 + `6c88d94` プロバイダ配線+設定フラグ。warm 1.9ms 実測）
  - [x] 6レンズレビュー+反証検証（確定11件・準確定3件）
  - [x] Fix A: 全終了経路での確実な shutdown（`ce53dfd`）
  - [x] Fix B: kill/reap の背景スレッド化・生存プローブ・4MiBキャップ・PTY ECHO off（`02ec55d`。timeout+250ms 内で復帰を実測）
  - [x] **Fix D（実地バグ）**: tmuxinator 死のループ修正（`69b9c30`）。①起動時バックグラウンド prewarm ②warm床 100→2000ms ③grace-drain（1回のタイムアウトでは殺さず遅延フレームを次 Tab で回収、連続2回で kill）④初回リクエストをコールド予算から分離。実装中に読み残しフレームの実バグも発見修正（PartialRead 状態を struct で持ち越し）。Ruby模擬600msの受け入れテスト（同一PIDで3連続Tab成功）込み
  - [x] Fix C: init tempfile 強化（create_new+0600+乱数名）・mtime-None カバレッジ・docs 真実性パス（`5c48055`）
  - [x] 最終検証: warm 中央値 ~2.0ms / timeout経路 500ms予算+30-50msで復帰 / 孤児ゼロ監査 / make check 全緑（917+6、独立再実行も緑）
  - [x] ユーザー実機確認（2026-07-11: 補完は成立。体感メリットは限定的との評だが受け入れ — 残存レイテンシは補完関数自身の Ruby exec 起因でネイティブ zsh と同等）
- [x] 2b.4 プロバイダ優先順の設定（`4ca402b`。`external` が文字列|配列両対応、順序変更は再起動要・有効化/検出は hot-reload）
- [x] **Checkpoint 2b**: ✅ 7レンズレビュー+反証検証（所見13→確定8・全修正: `868ef6b` 孤児kill / `eb0a8f9` spanエスケープ+symlinkガード / `26bc7e2` タイムアウト下限2000ms+ゲート共通化 / `19a2e8f` reload・fallthroughテスト）/ ✅ make check（849+6）/ ✅ zsh-completions 手動 E2E ユーザー確認済み（insecure dirs 問題は `chmod g-w /usr/local/share` で解決、トラブルシューティングを docs 追記 `96e5d5a`）（develop マージは Phase 3 完了後）

## Phase 3: `complete` ビルトイン（branch: `feature/completion` 継続, issue #89）

- [x] 3.1 CompletionRegistry + `complete` ビルトイン（登録・一覧・削除）（`81cc4a5`。registry は src/cli/completer/registry.rs、ビルトインは builtins/complete.rs、配線5点 + README EN/JA）
- [x] 3.2 RegistryProvider（フラグ + 静的候補）（`2fe7d2a`。CommandProvider 直後=ユーザー登録が git/外部より優先、候補ゼロは None フォールスルー）
- [x] 3.3 動的候補 `$(...)` + `-n` 条件（`c945be5`。sh -c + run_external_capped タイムアウト実行、`__fish_use_subcommand`/`__fish_seen_subcommand_from` の純評価、未知条件は inactive-but-listed）
- [x] **Checkpoint 3**: 完了（2026-07-12）
  - [x] 7レンズレビュー+反証検証（2票制）: 確定13・準確定3・却下0 → 統合10クラスタ
    - critical: dispatch_builtin スタブ経由（パイプ・`;`・`&&`・ai_pipe）の complete が使い捨てレジストリに黙って書き捨て
    - major: 一覧のバックスラッシュ/-s 値エスケープ不備でラウンドトリップ破壊 / 動的 $() 出力の無サニタイズ / 動的スペック N 個でタイムアウト N 倍直列加算 / 条件評価がリダイレクト先を語と誤認 / 候補の重複排除なし
    - minor: フラグ+引数併記スペックの片側隠蔽 / 括弧スキャンのクォート盲 / -s に空白・引用符が通る / input.rs アーム無カバレッジ ほか
  - [x] 修正ラウンド完了: Fix A `2f11bb5`（スタブ経路をエラー化+round-trip プロパティテスト+改行/NUL登録拒否+-s/-l検証）/ Fix B `6c2a088`（dedup・'-'枝マージ・ANSI/制御文字サニタイズ・動的予算一本化・クォート対応括弧スキャン・リダイレクト対応条件評価）/ Fix C `b2722f3`（input.rsアーム4テスト・条件×フラグ枝テスト・docs・carapace #[serial]確認）
  - [x] 最終検証 pass + 独立 make check 全緑（1015+6）。E2E: バックスラッシュ+空白入り記述のラウンドトリップをバイト一致で確認、`complete -c x -a y; ls` はエラー表面化を確認
  - [x] バージョン決定: **v1.14.0**（v2.0.0 は見送り、ユーザー判断 2026-07-12）
  - [ ] develop マージ（ユーザー指示待ち）
  - 残メモ: ①mod.rs apply_shell_alias の expanded_head 構築にもリダイレクト先混入あり（RegistryProvider 側でローカル回避済み、根本修正は別途）②shell_words が未使用依存化（Cargo.toml 整理候補）③フルスイートがまれに負荷依存で落ちる（単発再実行は常に緑、次回から tee でログ捕捉）④complete の永続化は当面 config.toml [startup].commands で代替可（本命は Phase 4 rc.jsh）

## Phase 4: rc.jsh スクリプト設定（branch: `feature/rc-script`）

- [ ] 4.1 rc ファイルローダ（`~/.config/jarvish/rc.jsh`、分類器バイパス、-c では読まない）
- [ ] 4.2 `--rcfile <path>` / `--no-rc` CLI オプション
- [ ] 4.3 `source` ビルトインのスクリプト対応（拡張子分岐）
- [ ] **Checkpoint 4**: 全フェーズ E2E + ドキュメント総点検
