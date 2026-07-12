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

- [x] 4.1 rc ファイルローダ（`595c5ed`。src/shell/rc.rs 新設: 純パーサ+分類器バイパス実行（classify 不使用をトレースで確認）+コメントのみテンプレ初回自動生成+[startup] より前に実行）
- [x] 4.2 `--rcfile <path>` / `--no-rc` CLI オプション（`2db6885`。conflicts_with、--rcfile 明示時は -c でも読む、欠損は警告して続行）
- [x] 4.3 `source` ビルトインのスクリプト対応（`d87f80b`。.toml=config reload / それ以外=rc 実行系、ネスト上限8、exit は bash 準拠でシェル終了）
- [ ] **Checkpoint 4**: 進行中
  - [x] 実装後 make check 全緑（1045+5+14+6）+ 検証エージェントの実バイナリ E2E pass（rc→alias/complete/ネスト source/失敗行継続/--no-rc 遮断）
  - [x] 7レンズレビュー+反証検証: 確定19・却下0
    - critical: FIFO を --rcfile/source すると起動が永久ブロック+サイズ無制限読込 / restart が -c モードの rc 内で「Restarting...」表示後に黙死（main.rs が LoopAction::Exit 固定） / rc/source 行が Black Box 非記録（→意図仕様として文書化する設計判断） / #[serial] 欠落3テスト（rc.rs×2+ai/client×1、系統的 flake の根因）
    - major: rc ブートストラップの symlink 書き抜け（dangling symlink で exists()=false → write が貫通。zsh-bridge の対策と非対称） / **同一スクリプト内で先に定義した alias が後続行で使えない**（run_rc_line に alias 展開がない） / exit N が偽の失敗行を出す / 分類器バイパスの回帰テストゼロ / 深さ上限の実テストなし ほか
  - [x] 修正ラウンド完了: Fix A `8a567df`（ガード付きリーダ regular-file+1MiB / symlink 強化）/ Fix B `9a96fd5`（同一スクリプト内 alias 展開・restart の -c 伝播・exit 偽エラー除去・Black Box 非記録の意図文書化）/ Fix C `32d83de`（#[serial]3件・分類器バイパス/深さ境界/no-rc テスト・docs・CLAUDE.md フラグ）+ 検証 pass（報告文字数超過で1回 fail → キャッシュ再開で解決）
  - [x] **数日来の間欠 flake 根治**（`dfbcaa0`）: 真犯人は rc.rs の新テストが `try_builtin("cd /tmp")` で cwd を放置（grep 不可視、実測で特定）→ #[serial]+tempdir+cwd復元。再現→10連続緑で実証。/tmp のゴミ45個はカナリアとして温存
  - [x] 最終 make check 全緑（1059+7+29+6 = 1101）※PTY枯渇はユーザーが孤児484件掃除で解消
  - [x] ドキュメント総点検 pass（`a92de24`）: CLAUDE.md の [startup]/rc.jsh 欠落と reload_config doc コメント陳腐化の2件修正。README EN/JA は377行構造ミラー完全・claims-vs-code 全一致でゼロ修正
  - [x] 全フェーズ横断 E2E: S1 計画書最終受け入れ（rc.jsh→alias/export/complete静的+動的/ネストsource→全反映）pass / S2 complete 一覧を rc に転写→バイト一致 round-trip pass / S3 テンプレ自動生成・非生成条件・実行可能性 pass / S4 レジストリ+git 共存 pass
  - [x] **S5 欠陥検出→修正完了**: `jarvish -c` のたびにウォーム zsh デーモンが孤児化（prewarm 背景スレッド vs main.rs 終端 shutdown のレース — shutdown 時点でスロット未挿入→no-op→遅れて spawn。**「デーモン zsh が溜まる」現象の root cause**）+ `cargo test --lib` 1回で孤児 ~56 本（フィクスチャ teardown 欠落）
    - 修正 `7498b38`（DaemonGate tombstone + -c は prewarm スレッド不起動 + 対話 exit は prewarm 完了を mpsc で有界待ち 2500ms）/ `e198bd4`+`76b46ff`（テスト teardown 3件）/ `6a3dadf`（二重spawn破棄経路も同期 shutdown 化）
    - 実装中に**第二のレース**も発見修正: tombstone があっても非ブロッキング shutdown の kill スレッドを process::exit が道連れにする経路 → tombstone 検知・二重spawn破棄とも同期 shutdown_blocking に統一。reload 経路（再spawn可）は非対象のまま維持
  - [x] 修正後の独立検証: make check 全緑（1068+9+29+6 = 1112）/ `jarvish -c` 10連発で孤児 0→0・.daemon_init 残骸 0 / cargo test --lib 前後も孤児増加ゼロ → **Checkpoint 4 クローズ（2026-07-12）**
  - [ ] develop マージ（ユーザー判断）。保留: make reap-orphans ターゲット追加の提案（S5 根治により新規蓄積は止まるため必要性低下 — ユーザー判断）
