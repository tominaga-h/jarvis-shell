# Changelog

このプロジェクトに対するすべての注目すべき変更を記録します。
フォーマットは [Keep a Changelog](https://keepachangelog.com/ja/1.1.0/) に基づいています。

## [v2.0.0] - 2026-08-09

### Added

- OpenAI、Anthropic、OpenCode Zen/Go のマルチプロバイダ AI 対応
- `[ai]` の `provider`、`max_tokens`、`base_url`、`api_key_env` 設定
- `ANTHROPIC_API_KEY` / `OPENCODE_API_KEY` 環境変数
- AI ストリーミング中の経過秒数・バイト数・チャンク数表示
- `source` によるプロバイダ設定変更時の AI クライアント再構築

## [v1.15.6](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.15.6) - 2026-08-08

### Changed

- README / docs/README_JA.md のデモ GIF を vhs で再生成し `images/demo.gif` へ差し替え（旧 `images/jarvish-demo.gif` は削除）。生成スクリプト `images/demo.tape` をリポジトリへ同梱し、以後再現可能にした
- コード全体のコメントを整理し、「何をしているか」の重複説明を削って「なぜそうしているか」を残す方針へ統一

### Internal

振る舞いを一切変えない大規模リファクタリング。CLAUDE.md のコーディング規約「各ファイルは単一かつ明確な責務を持つ」「巨大ファイルは機能単位のサブモジュールへ再編する」に沿って、1,000 行超の巨大モジュールを責務単位のディレクトリ構成へ分割した。公開 API と実行時挙動は変更していない。

- `src/cli/completer/` — `carapace.rs` (1,846 行) / `registry_provider.rs` (1,567 行) / `zsh_bridge.rs` (3,491 行) / `zsh_daemon.rs` (2,593 行) をそれぞれディレクトリ化し、`core` / `lifecycle` / `candidates` / `parsing` / `pty_io` / `request_framing` などの責務単位サブモジュールへ分割
- `src/shell/` — `mod.rs` (1,679 行) から `construction.rs` / `run.rs` / `reload.rs` / `restart.rs` / `prewarm.rs` を切り出し、`rc.rs` (1,447 行) を `rc/{resolution,file_reading,parsing,execution,options,template}.rs` へ分割
- `src/engine/` — `dispatch/mod.rs` を `{parse,builtin,external,expansion}.rs` へ、`parser/mod.rs` を `{command_list,pipeline,simple_command,redirects,ai_filter,types}.rs` へ、`builtins/update.rs` (837 行) を `update/{version,release,local_binary,homebrew,flag_file}.rs` へ分割
- `src/ai/client/` — `mod.rs` から `core.rs` / `conversation.rs` / `input_processing.rs` / `investigation.rs` / `config_update.rs` を切り出し
- `src/config/` — `mod.rs` (736 行) を `types.rs` / `loading.rs` へ分割
- `src/storage/` — `mod.rs` を `types.rs` / `database.rs` / `session.rs` へ分割
- `src/cli/highlighter/` — `mod.rs` (535 行) を `core.rs` / `token_styling.rs` / `operator_styling.rs` / `quote_styling.rs` / `env_styling.rs` へ分割
- 各モジュールのテストは対応する `tests.rs` へ移動。`make check`（fmt / check / clippy / test）は全パス

## [v1.15.5](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.15.5) - 2026-08-01

### Fixed

このリリースはテストコードのみの修正であり、jarvish 本体の挙動（補完・実行エンジン・AI 連携など）は一切変更していない。CI が赤いままでは以降の変更を安全に検証できないため、その土台を回復するのが目的。

- CI（GitHub Actions / ubuntu-latest）でテストが必ず失敗する問題を修正
  - `zsh_bridge` のゲート判定テストが、テストヘルパー経由で `ExternalCompletionSettings::resolve()` を呼んでいた。この関数は実機の `PATH` を `which::which("zsh")` で引くため、zsh が入っていない ubuntu-latest では `binary: None` に解決され、`gate()` が `None` を返してテストが必ず落ちる
  - 開発機には zsh / carapace が入っているため常に緑になり、CI でのみ再現する典型的な環境差だった。PATH から両者を除いたミラーを作ることでローカルでも同じ失敗を再現し、原因を確定させている
  - `carapace.rs` の既存テストと同じ方針で、`resolve()` を経由せず `enabled` を直接組み立てて PATH 非依存にした。実際に zsh プロセスを spawn する統合テストは従来どおり実機の有無を確認して skip するため、「zsh 不在なのに起動を試みる」ことにはならない

- ローカルで `make check` が高頻度に失敗する問題を修正
  - テスト用の使い捨て git リポジトリを作るヘルパーが `git.rs` / `mod.rs` / `carapace.rs` に 3 重コピペされており、いずれも裸の `git init` で**実行環境のグローバル git 設定を継承**していた
  - `init.defaultBranch` を `develop` 等に設定している開発者の環境では初期ブランチ名が変わり、`main` / `master` を決め打ちする assertion が落ちる
  - さらに深刻な問題として、各 git コマンドが `.output().unwrap()`（プロセス起動の成否しか見ず**終了ステータスを検査しない**）だったため、`commit.gpgsign = true` かつ GPG エージェントが使えない環境では `git commit` が失敗しても検知されず、**コミットが 1 つも無い空リポジトリ**のままテストが進んでいた。結果として一見無関係な箇所で assertion が落ち、原因が git 設定にあると気づきにくい形で表面化していた
  - `test_git.rs` に集約し、`git init --initial-branch=main` でブランチ名を固定、`GIT_CONFIG_GLOBAL` / `GIT_CONFIG_SYSTEM` を無効化してグローバル・システム設定を一切読ませない、全 git コマンドの終了ステータスを検査して失敗時は stderr 付きで即 panic する、という 3 点で環境非依存にした

- 1 件の失敗が大量の失敗に化けて原因特定を困難にする問題を修正
  - デーモンテスト用フィクスチャが `HOME` をテンポラリディレクトリへ差し替える際、復元を手書きのコードで行っていた。assertion が落ちるとアンワインドで復元処理が飛ばされ、**削除済みのディレクトリを指したままの `HOME`** がテストバイナリの残りの実行全体に漏れる
  - その結果、`~` 展開やパス解決など `HOME` に依存する無関係なテストが芋づる式に落ち、本来 1 件だった失敗が大量失敗として現れていた。`#[serial]` はテストの実行順を制御するだけで、一度漏れた環境変数は元に戻さない
  - `Drop` による RAII 化でパニック時も確実に復元されるようにし、連鎖を断ち切った。`OPENAI_API_KEY` を退避するテストも同じ理由で RAII 化している

- 並列実行時に確率的に失敗するテストを修正（リトライでは決定化できない真の競合）
  - プロセスグローバルな `RESTART_FLAG`（`AtomicBool`）を読み書きする 3 つのテストが `#[serial]` を欠いており、互いに並列実行されうる状態だった
  - 特に `register_sigusr1_handler` は内部で `RESTART_FLAG` を `false` にリセットするため、他テストの転送スレッドが `true` を観測する前にリセットされる、という形で状態を壊し合っていた
  - 同じフラグを触るテスト群として `#[serial]` で直列化した

- 高負荷環境でのみ失敗するテストを修正（実装は正しくテストだけが落ちるケース）
  - 実 zsh の `compinit` によるコールドスタートは、CPU が飽和した環境では本番のタイムアウト予算（2 秒）を容易に超える。この場合プロダクションコードは正しく `None` へ縮退しているにもかかわらず、テストが無条件に候補の存在を主張していたため落ちていた。E2E テスト用のタイムアウト定数を導入して統一し、タイムアウト挙動そのものを検証するテストは個別に短い値を明示しているため検出力は落ちていない
  - 事前ウォームアップ（`prewarm_zsh_daemon`）は spawn 予算にプロダクション定数をハードコードしており、テスト設定では上書きできない。これは UI スレッドをブロックしないための本番仕様として正しいため実装は変更せず、テスト側を「高負荷で spawn できなければ前提不成立として skip」に改めた
  - `oversized_response_is_capped_and_marks_daemon_dead` は閾値の調整では根治しないため測定手法自体を変更した。6.4MB の応答を生成する重い処理で実測値が 14.9〜28.6 秒と大きく揺れるため、経過時間では「バッファ上限で落ちたのか、タイムアウトで落ちたのか」を確率的にしか判定できない。バッファ上限超過はグレース対象外で即座に kill される一方、クリーンなタイムアウトは連続 2 回まではデーモンを生かすという差を利用し、「1 回のリクエスト後に死んでいること」を経路の決定的な証拠とする因果ベースの判定に置き換えた（経過時間に一切依存しない）

## [v1.15.4](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.15.4) - 2026-07-31

### Fixed

- 外部補完が担当するコマンドで、無関係なパス補完が表示されてから正しい候補に入れ替わる問題を修正
  - `tmuxinator <TAB>` がまず `CLAUDE.md` / `Cargo.lock` / `docs/` といったカレントディレクトリのファイル一覧を表示し、しばらくしてから本来のサブコマンド補完へ切り替わっていた。tmuxinator の引数として意味をなさない候補が出るうえ、「もう一度 Tab を押せば正しい候補が出る」ことを知る手がかりがユーザー側に無い
  - 原因は `PathProvider::provide` が無条件で `Some(...)` を返すこと。外部補完プロバイダがタイムアウト・失敗して `None` を返すと、プロバイダ連鎖（`find_map`）がそのまま `PathProvider` へ落ち、「答えられなかった」が「パス補完が正解」にすり替わっていた
  - `CompletionProvider` に `is_responsible(&ctx) -> bool`（既定 `false`）を追加し、外部補完プロバイダのみが「このコマンドを自分が担当するか」を外部プロセスを起動せずに宣言する。`provide()` が冒頭で行う安価なガードと同じ判定を共有ヘルパーに切り出して再利用するため、2 箇所で判定基準が drift しない
  - ディスパッチを `find_map` から `dispatch_providers()` へ置き換え、担当プロバイダが `None` を返した時点で以降のプロバイダを呼ばずに空候補で確定する。`PathProvider` へ落ちないため、誤った候補が表示されることも後から入れ替わることもない
  - 副次的な効果として、担当プロバイダの失敗時点で連鎖を打ち切るため、carapace と zsh ブリッジでタイムアウト予算が加算される問題（最悪 2.5 秒の UI フリーズ）も解消される
  - 設計方針は fish-shell の実装調査に基づく。fish の Tab 補完は完全に同期・ブロッキング（バックグラウンド化しているのは autosuggestion / syntax highlighting / history pager の 3 つのみ）で、補完結果のキャッシュも持たず毎回スクリプトを再実行する。`tmuxinator <TAB>` には実測 ~190ms かかっている（10 回で 2.16 秒、毎回ほぼ一定）。fish の体感の良さは速さではなく「待ってから正しい結果を一度だけ出す」一貫性に由来するため、「待つのは許容できるが、間違ったものを見せてはいけない」という方針を採った
  - `MIN_TIMEOUT_MS` / `WARM_MIN_TIMEOUT_MS` は death-loop 対策として意図的に設定された値のため変更していない

- 上記の修正により `tmuxinator <TAB>` が候補ゼロ（`NO RECORDS FOUND`）になる回帰を修正
  - carapace は内蔵 spec（653 個）を持つコマンドしか答えられず、spec が無いコマンドでは**正常終了しつつ空の出力**を返す（実測: `carapace tmuxinator export tmuxinator ''` は exit 0 かつ出力ゼロ、10〜20ms）。`provide()` はこれを `None` に畳むが、これは「タイムアウトして答えられなかった」ではなく「担当外なので次に譲る」の意味である
  - しかし `is_responsible` は carapace が有効かどうかしか見ておらず spec の有無を区別できないため、tmuxinator でも「責任者だが失敗した」と申告していた。その結果ディスパッチが連鎖を打ち切り、本来答えられる zsh ブリッジが一度も呼ばれない状態になっていた
  - 「責任者を名乗れるのは後ろに誰もいないプロバイダだけ」という原則に沿って、carapace の `is_responsible` を常に `false` にした。`external = "auto"` では carapace → zsh ブリッジの順に並ぶため、誤ったパス補完を抑止する役割は連鎖最後尾の zsh ブリッジが担う
  - carapace のみを有効化した構成（`external = "carapace"`）では従来どおり `PathProvider` へフォールバックする。carapace は spec の無いコマンドが多いため、そこでパス補完すら出さないほうが実害が大きいという判断

## [v1.15.3](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.15.3) - 2026-07-31

### Added

- 補完デーモンの残骸ファイルを起動時に自動で掃除するようにした
  - デーモンが書き出す一時スクリプト `.daemon_init.<pid>.<random>.zsh` は正常な終了経路（shutdown / `Drop` の reap）で削除されるが、`SIGKILL`・OOM killer・電源断など reap を経ない終わり方をするとファイルだけが残る。1 個あたり数 KB と小さいため実害は出にくいが、放置するとブリッジディレクトリに堆積する（実環境で 237 個の残骸を確認）
  - あわせて `compinit` のリネーム残骸（`.zcompdump.<host>.<pid>`）も掃除する。`compinit` はダンプ更新時に一時ファイルへ書き出してから本来の名前へ `mv` するため、その途中で落ちると一時ファイルだけが残る（実環境で 25 個を確認）
  - 掃除はシェル起動直後のデーモン事前ウォームアップ（バックグラウンドスレッド）で 1 回だけ実行し、UI スレッドを一切ブロックしない
  - 安全側の設計: init スクリプトはファイル名から pid を取り出し `kill(pid, 0)` で生存確認したうえで、既に存在しないプロセスのものだけを削除する。自分自身の pid・生存中の pid・命名規則に合致しないファイル（`.zshrc` 等）は対象外。「存在するが権限が無い」（`EPERM`）場合も生存扱いとする（生きているデーモンのスクリプトを誤って消すと spawn 失敗という実害が出るため、消してよいと確信できない限り消さない）
  - 完成品のキャッシュ本体（`.zcompdump`）は**削除しない**。これは残骸ではなく有効なキャッシュであり、消すと次回の `compinit` が全補完関数を読み直して起動が遅くなる。`.zcompdump_capture` 等の別プレフィックスも対象外。対象はブリッジディレクトリ配下のみで、ユーザー自身の対話 zsh が作る `~/.zcompdump` には一切触れない
  - 既知の限界として、pid が OS に再利用されている残骸は削除できない（実環境の 237 個中 1 個が該当）。取りこぼしても無害な数 KB が残るだけであり、pid が次に空くタイミングで回収される

### Fixed

- zsh ブリッジ統合テストがユーザーのローカル環境に依存して不安定になる問題を修正
  - `integration_git_checkout_prefix_suggests_branch` がブリッジディレクトリと `HOME` を隔離しないコンストラクタを使っていたため、**実ユーザーの** `~/.config/jarvish/zsh-bridge` と実 `$HOME` を使って補完デーモンを spawn していた。結果としてテストの成否が開発者のローカル環境の状態に左右され、レディマーカーが cold timeout 内に届かず `zsh daemon failed to reach ready marker within timeout` で失敗する、実 `$HOME` の `~/.zcompdump_capture` を他のデーモンと共有するため compdump 再生成のタイミング次第で warm timeout を超過する、といった形で不安定化していた
  - プロダクションコードの欠陥ではなくテストの隔離漏れ。同ファイルの他の統合テストが既に使っている、ブリッジディレクトリと `HOME` を一時ディレクトリへ隔離する版のコンストラクタに揃えて環境非依存にした

## [v1.15.2](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.15.2) - 2026-07-31

### Fixed

- 補完候補に「現在のディレクトリに存在しないファイル」が混入する不具合を修正
  - 常駐 zsh 補完デーモン（`zsh -i`）は spawn 時点の作業ディレクトリを継承したままセッション中ずっと生き続ける。jarvish 側の `cd`（`std::env::set_current_dir`）は自プロセスの cwd を変えるだけで、既に走っている子プロセスには届かないため、`_files` / `_path_files` 等の補完関数が **jarvish の起動ディレクトリ**を基準に相対パスを解決していた
  - デーモンはシェル起動直後に事前ウォームアップされる（`prewarm_zsh_daemon`）ため、実際の症状は「起動ディレクトリ（多くは `$HOME`）の中身が候補に出続ける」形で現れる。さらに `PathProvider` は補完プロバイダ連鎖の最後尾（`find_map` ディスパッチ）であり、デーモンが候補を返した時点で正しい `fs::read_dir` の結果は評価されず握り潰されていた
  - `ZshDaemon` に「デーモン側が今いると判っているディレクトリ」を保持し、補完リクエスト送出の直前に jarvish の現在の cwd と突き合わせる `sync_cwd()` を追加。食い違う場合のみデーモンへ移動指示を送る（無変化なら何も送らないため、`cd` の度に再 spawn / `compinit` やり直しとなる方式と違い常駐デーモンの利点を失わない）
  - `daemon_init.zsh` に `jarvish-set-cwd` ZLE ウィジェットを追加し `^G` にバインド。`^X` は emacs キーマップのプレフィックスキー（`^X^B` 等 16 個）であり単体では発火せずバッファへリテラル挿入されてしまうため採用しない（デーモンは `^M`/`^J` を `undefined` にしておりコマンドを実行しないため `send-break` は不要）
  - パスはシングルクォートで囲って渡し、`'\''` パターンでパス中のシングルクォートも表現する。これにより `$`・バッククォート・グロブ・空白を含むディレクトリ名もリテラルとして安全に渡り、クォート破りによる意図しない実行を防ぐ
  - 同期後に発生する ZLE の再描画出力は読み捨てる（残すと直後の補完リクエストのフレーム読み取り（NUL トグル）に混入し desync の原因になる）

## [v1.15.1](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.15.1) - 2026-07-15

### Fixed

- パイプ/コネクタの各セグメント先頭でエイリアスを展開するよう修正
  - `cat x | grep y` のようにパイプ後段のコマンドがエイリアスでも展開されなかった（`&&` `||` `;` の 2 番目以降のセグメントも同様）。原因は `expand_alias` が行全体の先頭 1 トークンのみを、パイプ/コネクタ分割より前に 1 回だけ置換していたこと
  - `quote.rs` にバイト範囲付きトークナイザ `split_quoted_spans` を新設し `split_quoted` をそれに委譲（トークナイザ実装を一本化）。`alias.rs` の `expand_alias` を `expand_aliases_in_line` へ置き換え、`|` `&&` `||` `;` で分割した各セグメント先頭にエイリアスを適用して原文へバイト splice で復元（クォート・空白・マルチバイトを保持）
  - `>` `>>` `<`（リダイレクト）や、クォート内・`$(...)` 内の `|` はセグメント境界にしない。one-shot・クォート/subst 先頭は非展開、演算子を含む alias 値は bash 準拠

### Changed

- 非対話単体実行 (`-c`) のコマンドを履歴に記録しないよう変更
  - nvim 等の外部ツールがファイル glob 展開のために `jarvish -c "..."` を非対話実行すると、その一時コマンドが `history.db`（上下矢印キーの履歴補完）に混入していた。bash/zsh と同様に非対話単体実行は履歴対象外とする
  - `Shell` 構造体に `interactive: bool` を保持し、`command_history` テーブルへ書き込む 2 経路（`record_history` / AI ツールコール実行コマンドの reedline 直接保存）を `interactive` でガード。設定項目は増やさず常に有効。rc.jsh/startup/source は元々 `record_history` を通らないため影響なし

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
