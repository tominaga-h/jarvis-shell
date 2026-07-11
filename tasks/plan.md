# 実装プラン: コマンド補完網羅 + スクリプト設定ファイル (jarvish)

## Context（背景と目的）

jarvish は現在、パス補完・コマンド名補完・git ブランチ補完のみを持ち、世の中の大量の CLI コマンドに対する引数補完がない。補完スクリプトを1コマンドずつ自前実装するのは非現実的なため、**既存の補完資産（carapace の 500〜1000+ コマンド、zsh-completions エコシステム）をブリッジ方式で取り込む**。zsh スクリプトを jarvish が解釈する方式は zsh インタプリタの再実装に相当するため採らない（調査済み: compsys は zsh スクリプト + ZLE ウィジェット内でのみ動く C ビルトインのハイブリッドで、carapace-bridge / nushell も全て「本物の zsh を起動して候補を吸い出す」方式）。

副産物として、TOML より自由度の高い**スクリプト形式の設定ファイル `~/.config/jarvish/rc.jsh`** を導入する（alias / export / complete 登録などを行形式で記述）。

既存 issue との整合: **#89「completion機能」（fish 式 complete ビルトイン, milestone v2.0.0）= Phase 3**、**#88「git補完の拡充 + ライブラリ活用」= Phase 2 で実質解決**。

### 決定事項（合意済み）
- Phase 2 は **carapace 連携を先行**、zsh ブリッジは後続
- rc ファイルは **`~/.config/jarvish/rc.jsh`**。`-c` モードでは読まない。**rc ファイルを指定する CLI オプション**を新設
- **ブランチ戦略（2026-07-09 更新）**: Phase 1〜3 は補完機能の範囲としてすべて `feature/completion` ブランチで継続。develop マージは Phase 3 完了後に判断。Phase 4（rc.jsh）のみ別ブランチ（`feature/rc-script`）

## アーキテクチャ決定

1. **nushell 型 spans プロトコル**を内部補完基盤に採用: `(line, pos)` → カーソルを含むパイプライン区間の `spans: Vec<String>` + partial + reedline `Span`。nushell の既知の弱点（alias 未展開・同期ブロック・fallback なし）は対策込みで設計する。
2. **CompletionProvider trait** で補完源をプラグイン化。Phase 1 で既存3種を移植、Phase 2 以降で carapace / zsh-bridge / registry プロバイダを追加。プロバイダが候補なし → パス補完フォールバック。
3. **外部プロセス系プロバイダはタイムアウト必須**（reedline 0.45 の `Completer::complete` は UI スレッド同期実行のため）。ワーカースレッド + `recv_timeout`、失敗時はパス補完に degrade（既存バナーの offline 報告と同じ graceful degradation 文化）。
4. **共有状態は既存パターン踏襲**: `Arc<RwLock<T>>` を `build_editor` 経由で completer に注入（`git_branch_commands` の前例）。
5. **rc.jsh は既存の行実行機構を流用**: `Shell::run()` の `[startup].commands` ループ / `run_command` と同じ「1行ずつ実行」。ただし **NL 分類器はバイパス**し全行コマンド確定で実行（設定ファイルの決定性確保）。
6. バージョニング: SemVer。各フェーズ完了 = minor bump 候補（Phase 3 完了時に v2.0.0 判断はユーザー裁量）。

---

## Phase 1: 補完パイプライン基盤（branch: `feature/completion` — 現行ブランチ）

> ✅ ソース実地検証済みの確定設計。
> **検証済み事実**: reedline `Span` は**バイト単位**（`completion/base.rs` 明記、`floor_char_boundary` でクランプされるため日本語ファイル名はバイトオフセット厳守）/ ColumnarMenu は **description を描画する**（ただし1件でも description があると全幅1カラム化 → 30件超で除去するガードを入れる）/ completer は `line[..pos]` しか受け取らない（`only_buffer_difference=false`）/ builtin リストは実は **3箇所**にあり `help.rs` の `BUILTIN_COMMANDS`（説明文付き19件、pwd 欠落）が一元化の母体に最適 / completer の呼び出し元は `editor.rs:34` のみ。

### 主要設計判断
- **トークナイズ**: `split_quoted` は拡張**しない**。実行系は未閉クォートをエラーにすべき（`input.rs:231-238` が依存）で、補完系は寛容であるべき — 要求が正反対のため、`src/cli/completer/context.rs` に補完専用の寛容スキャナを新設。**乖離防止策**: 演算子テーブルを `quote.rs` の `operator_prefix_len()` として共有 + 整形式コーパスで `split_quoted` と値が一致するパリティテストを常設。
- **クォート途中の partial**: `echo "fo<Tab>` → partial は `fo`（クォート剥がし）、Span は `"` から。確定時は orchestrator が `escape_for_insert`（空白・記号をバックスラッシュエスケープ。`~` 先頭は温存 = チルダ展開維持）で再エスケープして挿入 — 空白入りファイル名の潜在バグも同時に修正。
- **セグメント切断**: `| && || ;` で切断、リダイレクト `> >> <` は切断しない（コマンド位置を変えないため演算子トークンとして残す）。`ls | <Tab>` → first-token（コマンド補完）。未閉 `$(` はその内側で再帰抽出（`echo $(git checkout fo<Tab>` が効く）。
- **Provider trait**: `provide(&self, ctx) -> Option<Vec<Candidate>>`。`None`=対象外（次へ）/ `Some(vec![])`=担当したが候補なし（フォールバックしない）。Span は Candidate に持たせず orchestrator が `ctx.span` から一括組立（現行 path.rs の「値再構築」方式を維持）。
- **alias Arc 化**: builtins のシグネチャは**不変**（Shell がロックガードの `&mut *guard` を渡す）。completer は `complete()` ごとに short-lived read + clone。デッドロックなし（completer は read_line と同スレッド、alias 書込は read_line 復帰後）。
- **alias 展開の単純化**: alias 値に演算子を含む場合は展開スキップ（`lg='ls | grep'` は実効コマンド再計算が必要になるため見送り、挙動は現状維持のパス補完）。実行系の `expand_alias` も first-token 単純置換なのでエンジンより弱くならない。

### Task 1.1: 寛容スキャナ + CompletionContext（`context.rs` 新設）
- **内容**: `LexToken{value(剥がし済), start, end(バイト), is_operator, quoted}` / `CompletionContext{tokens, partial, span, is_first_token, expanded_head}` / `extract_context(line, pos)`。`quote.rs` に `operator_prefix_len()` 追加（挙動変更なし）。
- **受け入れ基準**: 未閉クォート・dangling backslash・未閉 `$(` でエラーにならない / マルチバイトで正確なバイト範囲 / パイプライン切断・trailing-space・エスケープ空白が仕様表どおり
- **検証**: テーブル駆動テスト（パイプ切断・クォート各種・UTF-8 バイト厳密比較・`$(` 再帰・空行）+ `split_quoted` パリティテスト
- **依存**: なし / **規模**: M

### Task 1.2: builtin リスト一元化（説明文付き）
- **内容**: `help.rs` の説明文付きテーブルを `engine/builtins/mod.rs` の `pub(crate) const BUILTIN_COMMANDS: &[(&str, &str)]` に移設（**pwd 行を追加**して20件に）。`is_builtin` / `help` / completer が同一テーブルを参照。
- **受け入れ基準**: `is_builtin` が従来と同じ20コマンドを受理（旧リスト列挙テスト）/ help に pwd が載る
- **検証**: ユニットテスト
- **依存**: なし（1.1 と並行可） / **規模**: S

### Task 1.3: Provider trait 化 + 既存3補完の移植 + orchestrator
- **内容**: `provider.rs` 新設（trait / `Candidate{value, description, append_whitespace}` / `escape_for_insert`）。CommandProvider（PATH + 説明付き builtins）・GitProvider（`git_branch_commands` と git-alias キャッシュを内包）・PathProvider（終端フォールバック、`cd` は dirs_only）に再配置。orchestrator は extract_context → プロバイダ順走査 → Suggestion 一括組立 + `DESCRIPTION_LIMIT=30` 超で description 除去。
- **受け入れ基準**: 既存の trait レベルテスト全 green（回帰網）/ `ls | git checkout test-<Tab>` でブランチ補完 / `echo "fo<Tab>` でファイル補完 / 空白入りファイル名がエスケープ挿入される
- **検証**: `make check` + 手動 REPL
- **依存**: 1.1, 1.2 / **規模**: M

### Task 1.4: aliases の `Arc<RwLock<HashMap>>` 化（配管のみ）
- **内容**: `shell/mod.rs:48,145,247` / `input.rs:43,285-286,300-301` / `editor.rs` / completer コンストラクタ（保持のみ、利用は 1.5）。builtins のシグネチャ・テストは不変。
- **受け入れ基準**: `make check` clean / alias/unalias/which/type/source の既存挙動不変 / ロックガードを await 越しに保持しない
- **検証**: 既存テスト + コンパイル
- **依存**: なし（1.1〜1.3 と並行可、1.5 の前提） / **規模**: S

### Task 1.5: alias 対応補完
- **内容**: `apply_shell_alias`（tokens[0] が alias なら値を `split_quoted` し、演算子を含まなければ `expanded_head` に格納）。GitProvider が `g checkout <Tab>`（alias g=git）でブランチ補完。CommandProvider が first-token に alias 名を候補追加（description = alias 値）。
- **受け入れ基準**: `g checkout <Tab>` / `ls | g co test-<Tab>`（alias→git→git-alias 連鎖）が効く / 演算子入り alias はパス補完に降格 / **セッション中に `alias` で定義した直後の Tab に反映**（共有 Arc の主要 UX 効果）/ `gco="git checkout"` → `gco <Tab>` でブランチ補完
- **検証**: シード済み Arc を使った E2E テスト（実 Shell 不要）
- **依存**: 1.3, 1.4 / **規模**: M

**実装順**: 1.1 ∥ 1.2 ∥ 1.4 → 1.3 → 1.5

### ✅ Checkpoint 1
- `make check` 全パス / カバレッジ確認（未テストの重要パスなし）
- 手動確認: パイプ後補完・クォートパス・alias 補完・日本語ファイル名
- レビュー（品質検証）→ コミット → PR → develop マージ

---

## Phase 2a: carapace 連携（branch: `feature/completion` 継続）

### Task 2a.1: 外部プロバイダ実行基盤（タイムアウト + フォールバック）
- **内容**: ワーカースレッド + `mpsc::recv_timeout` で外部コマンド実行するランナー。タイムアウト時は候補なし扱い→パス補完へ。タイムアウト値は `[completion]` に設定可能（default 400ms 目安）。
- **受け入れ基準**: ハングするダミーコマンドでも Tab がタイムアウト内に返る / タイムアウト後のゾンビプロセスなし（kill）
- **検証**: ユニットテスト（sleep コマンド利用）
- **依存**: Phase 1 / **規模**: S

### Task 2a.2: CarapaceProvider
- **内容**: PATH 上の carapace を起動時 detect（キャッシュ）。`carapace <cmd> export <spans...>` を実行し JSON（serde_json）→ Candidate{value, description}。エラー/空→フォールスルー。
- **受け入れ基準**: carapace 有りで `git chec<Tab>` `docker ru<Tab>` 等が候補+説明付きで補完 / carapace 無し環境で無害（起動時1回の detect のみ）
- **検証**: `which carapace` で実行時 skip する統合テスト + JSON パースの固定文字列ユニットテスト
- **依存**: 2a.1 / **規模**: M

### Task 2a.3: 設定・ドキュメント反映
- **内容**: `[completion] external = "auto"|"carapace"|"none"`（default auto）+ timeout_ms。CLAUDE.md 4点セット反映（ソースコメント / README.md / docs/README_JA.md / source ビルトイン出力）+ defaults.rs テンプレート + reload_config 対応。
- **受け入れ基準**: `source` で hot-reload される / ドキュメント4点更新
- **検証**: config ユニットテスト + `make check`
- **依存**: 2a.2 / **規模**: S

### ✅ Checkpoint 2a
- carapace 環境での手動 E2E（git/docker/kubectl 等 5 コマンド）+ 非搭載環境の劣化確認 → レビュー → develop マージ

---

## Phase 2b: zsh ブリッジ（branch: `feature/completion` 継続）

### Task 2b.1: capture.zsh vendor + ワンショットブリッジ
- **内容**: capture.zsh（MIT, Valodim/zsh-capture-completion — carapace-bridge も vendor している実績）を assets 同梱。ZshBridgeProvider: `zsh` spawn → バッファ+Tab 送信 → NUL センチネル区切りで `候補 -- 説明` をパース（ANSI 除去）。2a.1 のタイムアウトランナー利用。
- **受け入れ基準**: zsh 有り環境で fpath 上の `_*` 補完が効く / zsh 無しで無害 / ハング補完関数でタイムアウト
- **検証**: 実行時 skip 付き統合テスト + パーサのユニットテスト
- **依存**: Phase 2a / **規模**: M

### Task 2b.2: ブリッジ用 zshrc（ユーザー拡張点）
- **内容**: `~/.config/jarvish/zsh-bridge.zshrc` をブリッジ zsh が source（初回テンプレート自動生成、carapace の `~/.config/carapace/bridge/zsh` と同じ設計）。ここに fpath 追加や compdef を **zsh 構文で** 書ける = 「zsh の設定方法をカバー」の実体。
- **受け入れ基準**: zsh-completions を fpath 追加すると jarvish で補完される（E2E）
- **検証**: 手動 E2E + ドキュメント
- **依存**: 2b.1 / **規模**: S

### Task 2b.3: ウォームデーモン化（性能）
- **内容**: 常駐 zsh を PTY 下に1本維持（`src/engine/pty.rs` 基盤流用、zsh-autosuggestions と同じパターン）。Tab ごとに書込/読出。死活監視 + ハング時 kill&respawn。
- **受け入れ基準**: ウォーム時レイテンシ実測 50ms 以下目安 / respawn 動作 / シェル終了時にデーモン残留なし
- **検証**: レイテンシ計測テスト + プロセスリークテスト
- **依存**: 2b.1 / **規模**: M–L（ワンショットで十分速ければ延期可）

### Task 2b.4: プロバイダ優先順の設定
- **内容**: `[completion]` にプロバイダ順/コマンド単位オーバーライド（例: `external = ["carapace", "zsh"]`, `overrides.git = "zsh"`）。ドキュメント4点セット。
- **依存**: 2b.1 / **規模**: S

### ✅ Checkpoint 2b → レビュー → develop マージ

---

## Phase 3: fish 式 `complete` ビルトイン（branch: `feature/completion` 継続, issue #89）

### Task 3.1: CompletionRegistry + `complete` ビルトイン（登録・一覧・削除）
- **内容**: `complete -c cmd -s x -l long -a '...' -d '説明' -n '条件'`（clap derive、既存 parse_args 慣行）。`Arc<RwLock<CompletionRegistry>>` を completer と共有。**4箇所の登録**: engine/builtins/mod.rs is_builtin + dispatch_builtin / help.rs（→1.2 で一元化済みのテーブル） / input.rs stateful intercept。
- **受け入れ基準**: 登録→一覧→削除が REPL で動く / help に載る
- **依存**: Phase 1 / **規模**: M

### Task 3.2: RegistryProvider（静的候補 + フラグ補完）
- **内容**: 登録済みスペックから short/long フラグ + 静的引数候補を description 付きで補完。
- **受け入れ基準**: `complete -c mycmd -l verbose -d 'verbose output'` 登録後 `mycmd --v<Tab>` が効く
- **依存**: 3.1 / **規模**: S

### Task 3.3: 動的候補 + 条件
- **内容**: `-a '$(git branch ...)'` を `run_pipeline_captured` で実行（`候補\t説明` 行プロトコル）。`-n` は最小セット（サブコマンド既出判定など fish の頻出条件の組み込み版）から開始。
- **受け入れ基準**: 動的候補が補完に出る / 条件で出し分け
- **依存**: 3.2 / **規模**: M

### ✅ Checkpoint 3 → レビュー → develop マージ（v2.0.0 判断はユーザー）

---

## Phase 4: rc.jsh スクリプト設定（branch: `feature/rc-script`）

### Task 4.1: rc ファイルローダ
- **内容**: `~/.config/jarvish/rc.jsh` を起動時に1行ずつ実行（`#` コメント・空行 skip）。**分類器バイパスで全行コマンド確定実行**（AI 誤発火防止）。エラーは行番号付き報告で継続。`[startup].commands` より前に実行。`-c` モードでは読まない。
- **受け入れ基準**: alias/export/complete 登録が rc.jsh から反映 / 構文エラー行は報告して続行 / -c で読まれない
- **検証**: tempfile ベースのユニット + 手動
- **依存**: Phase 3（complete 登録を rc に書けること。alias/export だけなら Phase 1 後でも着手可） / **規模**: M

### Task 4.2: `--rcfile <path>` CLI オプション
- **内容**: clap に `--rcfile` 追加（明示指定時は default パスの代わりに使用。`-c` でも明示指定時は読む）。`--no-rc` も追加。
- **受け入れ基準**: 指定パスの rc が使われる / -c + --rcfile の組合せ動作
- **依存**: 4.1 / **規模**: S

### Task 4.3: `source` ビルトインのスクリプト対応
- **内容**: 拡張子分岐 — `.toml` → 従来の config reload / それ以外 → rc と同じスクリプト実行。help / ドキュメント更新。
- **受け入れ基準**: `source ~/.config/jarvish/rc.jsh` が動き、`source config.toml` の既存挙動不変
- **依存**: 4.1 / **規模**: S

### ✅ Checkpoint 4（最終）
- 全フェーズ E2E: rc.jsh に complete 登録 + carapace/zsh ブリッジ併用で日常コマンド補完が成立
- ドキュメント総点検（README / README_JA / CHANGELOG / source 出力）

---

## リスクと対策

| リスク | 影響 | 対策 |
|---|---|---|
| UI スレッドブロック（外部プロセス補完） | 高 | タイムアウトランナー必須（2a.1）+ パス補完フォールバック |
| 寛容トークナイザと quote.rs の二重実装 divergence | 中 | 演算子テーブル共有（`operator_prefix_len`）+ split_quoted パリティテスト常設 |
| UTF-8 Span バグが `floor_char_boundary` クランプで隠蔽される | 中 | UTF-8 テストは「panic しない」でなくバイトオフセット厳密一致を assert |
| alias Arc 化の波及バグ | 中 | 変更点を全列挙してからテスト同時実装; completer は read_line と同スレッドなのでデッドロックなし |
| carapace / zsh 不在環境 | 低 | 起動時 detect + graceful degradation（既存バナー文化） |
| zsh ブリッジのパース脆弱性 | 中 | PROMPT= 空化 + NUL センチネル + ANSI 除去（capture.zsh 実証済み手法）; Enter は unbind 済で誤実行なし |
| `source` の意味拡張の互換性 | 低 | .toml 分岐で既存契約維持 |

## 検証方針（全フェーズ共通）

- `make check`（fmt + clippy -D warnings + test）全パス必須。**cargo はサンドボックス外で実行**
- テストは実装と同時（CLAUDE.md 開発サイクル遵守: 実装→品質検証→テスト追加→make check→カバレッジ調査→完了）
- 外部ツール依存テストは `which <tool>` による実行時 skip
- 各 Checkpoint で手動 REPL E2E（`cargo build --release` → 実シェル操作）

## Open Questions

- Phase 4 の rc.jsh を Phase 1 直後に前倒しするか（alias/export 用途だけなら依存なし。complete 登録を書くには Phase 3 後）
- Phase 2b の ColumnarMenu → IdeMenu 切替（description の見せ方）はブリッジ導入時に再評価
