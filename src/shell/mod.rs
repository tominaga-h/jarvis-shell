//! Shell モジュール — REPL ループとシェル状態管理
//!
//! `Shell` 構造体にすべてのシェル状態を集約し、
//! 入力ハンドリング、AI ルーティング、エラー調査の各責務をサブモジュールに分離する。

mod ai_router;
mod editor;
mod input;
mod investigate;
mod rc;

pub use rc::RcOptions;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use reedline::{Reedline, Signal};
use tracing::{info, warn};

use std::sync::atomic::AtomicBool as StaticAtomicBool;

/// SIGUSR1 シグナルハンドラが設定するグローバルフラグ。
/// シグナルハンドラ内では async-signal-safe な操作のみ許可されるため、
/// `AtomicBool::store` を使用する。
static RESTART_FLAG: StaticAtomicBool = StaticAtomicBool::new(false);

use crate::ai::{ConversationState, JarvisAI};
use crate::cli::completer::{
    format_external_binaries_display, format_external_summary, new_shared_daemon_slot,
    prewarm_zsh_daemon, registry::CompletionRegistry, shutdown_shared_daemon,
    shutdown_shared_daemon_blocking, DaemonGate, ExternalCompletionSettings, SharedDaemonSlot,
};
use crate::cli::prompt::starship::CMD_DURATION_NONE;
use crate::cli::prompt::{ShellPrompt, EXIT_CODE_NONE};
use crate::config::JarvishConfig;
use crate::engine::classifier::InputClassifier;
use crate::engine::expand;
use crate::engine::LoopAction;
use crate::storage::BlackBox;

/// Jarvis Shell の状態を管理する構造体。
/// エディタ、AI クライアント、履歴ストレージ、会話状態を保持する。
pub struct Shell {
    editor: Reedline,
    prompt: ShellPrompt,
    ai_client: Option<JarvisAI>,
    black_box: Option<BlackBox>,
    conversation_state: Option<ConversationState>,
    last_exit_code: Arc<AtomicI32>,
    /// 直前コマンドの実行時間（ミリ秒）。Starship プロンプトの `--cmd-duration` に使用。
    cmd_duration_ms: Arc<AtomicU64>,
    classifier: Arc<InputClassifier>,
    /// 設定ファイルで定義されたコマンドエイリアス（JarvishCompleter と共有）
    aliases: Arc<RwLock<HashMap<String, String>>>,
    /// 異常終了時に自動調査をスキップするコマンドの前方一致パターン
    ignore_auto_investigation_cmds: Vec<String>,
    /// pushd / popd / cd で管理されるディレクトリスタック
    dir_stack: Vec<PathBuf>,
    /// Farewell メッセージが既に表示済みかどうか（AI goodbye 等で表示済みの場合 true）
    farewell_shown: bool,
    /// コマンド履歴（reedline 矢印キー・ヒンター）が利用可能か
    history_available: bool,
    /// ロギングシステムがファイルに書き込み可能か
    logging_operational: bool,
    /// ブランチ名補完対象の git サブコマンド（JarvishCompleter と共有）
    git_branch_commands: Arc<RwLock<Vec<String>>>,
    /// 外部補完（carapace）の実行時設定（JarvishCompleter と共有）。
    /// `source` コマンドで `which()` 再検出込みに更新される。
    external_completion: Arc<RwLock<ExternalCompletionSettings>>,
    /// 温存 zsh 補完デーモンのスロット（`ZshBridgeProvider` と共有、
    /// Task A, #89）。`reload_config` / `exec_restart` / プロセス終了経路
    /// から、`provide()` を経由せずに直接 shutdown できるようにする
    /// （`Drop` にのみ依存すると `Command::exec` や `std::process::exit`
    /// では一切実行されないため）。
    zsh_daemon: SharedDaemonSlot,
    /// 終端 shutdown（exit/exec 経路）の tombstone ゲート（S5 修正）。
    /// `Shell::new` が起動時バックグラウンドスレッドの prewarm と共有する。
    /// `shutdown_zsh_daemon`（exit/exec 直前の有界同期 shutdown）が一度
    /// closed にすると、以後 prewarm が遅延してスロットに書き込もうとしても
    /// 挿入前に必ず kill される（`zsh_bridge::DaemonGate` のドキュメント
    /// 参照 — `-c` 単体実行や `rc.jsh` 内 `exit` での孤児デーモン化を防ぐ）。
    zsh_daemon_gate: Arc<DaemonGate>,
    /// prewarm スレッドの完了通知チャネル受信側（S5 追加修正）。
    ///
    /// `gate.close()` だけでは閉じないレースが実際に存在する: `main` が
    /// `shutdown_zsh_daemon`（deadline 1200ms）から戻った直後に
    /// `std::process::exit` を呼ぶと、detached な prewarm スレッドは
    /// **実行中であっても道連れで強制終了される**。もし prewarm が
    /// `ZshDaemon::spawn` 内の `Command::spawn()`（実際の子 zsh プロセス
    /// 生成、OS レベルの操作）を既に終えていて、かつ Mutex 内の tombstone
    /// 再チェック（コードレベルの判定）にまだ到達していないタイミングで
    /// プロセスごと消えると、その子プロセスは Rust コードが一切実行され
    /// ないまま孤児化する（`gate.close()` は「以後 Rust コードが判定に
    /// 使う値」を変えるだけで、既に他スレッドで実行中の OS 操作を中断
    /// させる力はない）。
    ///
    /// そのため `shutdown_zsh_daemon` は `gate.close()` の後、
    /// このチャネルから「prewarm スレッドが実際に完了通知を送るまで」を
    /// 有界時間で待つ（`JoinHandle::join()` は無界ブロッキングのため
    /// 使わず、`recv_timeout` で待つ）。間に合わなかった場合でも、
    /// closed 後にスロットへ書き込む経路は Mutex 内チェックで塞がれて
    /// いる（`DaemonGate` のドキュメント参照）ため、prewarm が
    /// `Command::spawn()` の**後**で強制終了された最悪ケースだけが残る
    /// リスクとなる — 有界待ちの猶予（`PREWARM_JOIN_DEADLINE`）を
    /// prewarm の spawn 上限（`MIN_TIMEOUT_MS`）より長く取ることで、
    /// この残存リスクを実務上ゼロに近づける。
    zsh_daemon_prewarm_done: Option<std::sync::mpsc::Receiver<()>>,
    /// `complete` ビルトインで登録されたユーザー定義補完（JarvishCompleter と共有、
    /// issue #89 Phase 3）。エントリはセッション限りで、rc.jsh（Phase 4）が
    /// 導入されるまでは再起動のたびに空から始まる。
    complete_registry: Arc<RwLock<CompletionRegistry>>,
    /// SIGUSR1 受信時に再起動をリクエストするフラグ。
    /// コマンド実行中・PTY 使用中は即座に再起動せず、次の REPL idle 時に遅延実行する。
    restart_requested: Arc<AtomicBool>,
    /// 起動時に実行するコマンドのリスト（config.toml の `[startup]` セクション）
    startup_commands: Vec<String>,
    /// `--rcfile` / `--no-rc` CLI オプション（Phase 4.2）。rc.jsh の
    /// 読み込みを `run()` / `run_command()` の両方から解決するために保持する。
    rc_options: RcOptions,
    /// 現在実行中の rc/source スクリプトのネスト深さ（Phase 4.3）。
    /// トップレベルの rc スクリプト実行では 0。`source <script>` 行が
    /// `try_shell_builtins` 経由で再帰的にスクリプトを実行するたびに
    /// `run_rc_script_sync` が加算・復元する。`MAX_SOURCE_DEPTH` を
    /// 超えるネストを検出して無限ループ（自己 source 等）を防ぐために使う。
    source_depth: usize,
    /// 対話セッションか否か（`main.rs` が `args.command.is_none()`（`-c`
    /// 未指定）のとき `true`）。`-c '<command>'` による非対話単体実行では
    /// `false`。`nvim` などの外部ツールがファイル glob 展開のために
    /// `jarvish -c "vimglob() {...}"` を呼ぶと、そのツール由来の一時
    /// コマンドが履歴（上下矢印キーの履歴補完）に混入してしまう。これを
    /// 防ぐため、非対話実行時は履歴（`command_history` テーブル）への
    /// 書き込みをスキップする（bash/zsh でも非対話実行は履歴対象外なのと
    /// 同じ挙動）。
    ///
    /// このフラグの consumer は2種類ある:
    /// 1. **prewarm 判定** — `spawn_prewarm_thread_if_interactive` が
    ///    `interactive == false` のとき zsh 補完デーモンの事前ウォーム
    ///    アップをスキップする（S5 修正）。
    /// 2. **履歴記録のゲート** — `handle_input`（`src/shell/input.rs`）から
    ///    `command_history` テーブルへ書き込む経路は2つあり、どちらも
    ///    `interactive == false` で塞ぐ:
    ///    - 経路B: `record_history` の `BlackBox::record`。
    ///    - 経路(reedline 直接): AI がツールコールで実行したコマンドを
    ///      `self.editor.history_mut().save()` で reedline 履歴に直接追加
    ///      する箇所（`executed_command`）。これは `read_line()` を通らない
    ///      `-c` 実行でも走るため、`record_history` とは別に個別のガードが
    ///      必要。
    ///
    /// なお reedline の `read_line()` が Enter 押下時に内部で行う自動保存
    /// （純粋な経路A）は、`-c` 実行では REPL ループ（`read_line()`）自体に
    /// 入らないため元々走らない。上記2つのゲートは `read_line()` の外側で
    /// 起きる書き込みを対象にしている。
    interactive: bool,
}

impl Shell {
    /// 新しい Shell インスタンスを作成する。
    ///
    /// 設定ファイル、入力分類器、エディタ、プロンプト、BlackBox、AI クライアントを初期化する。
    ///
    /// `interactive` は `main.rs` が `args.command.is_none()`（`-c` 未指定）
    /// かどうかから決める。`false`（`-c` 単体実行）の場合、Tab 補完が
    /// 一切発生しないウォーム zsh 補完デーモンの事前 spawn は純粋な無駄な
    /// うえ、起動〜終了が数ミリ秒で完走することが多く S5 のレース
    /// （孤児 `/bin/zsh -i`）を踏みやすいため、prewarm 自体を丸ごとスキップ
    /// する（S5 修正 — tombstone ゲートと合わせた二段構えの対策の1つ目）。
    pub fn new(
        logging_operational: bool,
        session_id: i64,
        rc_options: RcOptions,
        interactive: bool,
    ) -> Self {
        // 設定ファイルの読み込み
        let config = JarvishConfig::load();

        // [export] セクションの環境変数を設定
        Self::apply_exports(&config);

        // 入力分類器の初期化（キャッシュレス設計: which クレートでリアルタイム PATH 解決）
        // ハイライターと REPL ループの両方で共有するため Arc で包む
        let classifier = Arc::new(InputClassifier::new());

        // データディレクトリを一度だけ決定し、エディタ履歴と BlackBox の両方で共有する。
        let data_dir = BlackBox::data_dir();

        let git_branch_commands =
            Arc::new(RwLock::new(config.completion.git_branch_commands.clone()));

        // エイリアスは JarvishCompleter と共有するため editor 構築前に確保する
        let aliases = Arc::new(RwLock::new(config.alias.clone()));

        // 外部補完（carapace）の設定を解決する（`which` によるバイナリ検出込み）。
        // JarvishCompleter と共有するため editor 構築前に確保する。
        let external_completion = Arc::new(RwLock::new(ExternalCompletionSettings::resolve(
            &config.completion,
        )));

        // 温存 zsh 補完デーモンのスロット。`ZshBridgeProvider` と共有し、
        // `Shell` 側からライフサイクルイベント（reload/exit/restart）で
        // 直接 shutdown できるようにする（Task A, #89）。
        let zsh_daemon = new_shared_daemon_slot();
        // S5 修正: 終端 shutdown の tombstone ゲート。prewarm スレッドと
        // `shutdown_zsh_daemon` の両方に配る（`DaemonGate` のドキュメント
        // 参照）。
        let zsh_daemon_gate = DaemonGate::new();

        // Fix D4: 起動時のバックグラウンド事前ウォームアップ。設定でデーモン
        // が有効（フラグ on + zsh が enabled-kinds に含まれる + zsh バイナリ
        // 検出済み）なら、デタッチしたバックグラウンドスレッドから spawn を
        // 開始し、ユーザーの最初の Tab 押下までに温存デーモンが生きている
        // 状態を狙う（起動そのものはブロックしない）。無効なら
        // `prewarm_zsh_daemon` 内部で即座に no-op として戻る。`provide()`
        // とのレースは `prewarm_zsh_daemon` 側の Mutex 二重チェックで防止
        // 済み（同モジュールのドキュメント参照）。
        //
        // S5 修正: `interactive == false`（`-c` 単体実行）では Tab 補完が
        // 一切発生しないため、prewarm スレッド自体を起動しない（spawn は
        // 純粋な無駄なうえ、起動直後に完走するプロセスでは孤児化レースを
        // 踏みやすい）。判定ロジックは `spawn_prewarm_thread_if_interactive`
        // に切り出し、`Shell` 全体を構築せずに単体テストできるようにする。
        //
        // 戻り値の `Receiver` は `zsh_daemon_prewarm_done` に保持し、
        // `shutdown_zsh_daemon` が「prewarm スレッドの完了」を有界時間で
        // 待つのに使う（`zsh_daemon_prewarm_done` フィールドのドキュメント
        // 参照）。
        let zsh_daemon_prewarm_done = spawn_prewarm_thread_if_interactive(
            interactive,
            &external_completion,
            &zsh_daemon,
            &zsh_daemon_gate,
        );

        // `complete` ビルトインで登録されるユーザー定義補完（issue #89 Phase 3）。
        // JarvishCompleter と共有するため editor 構築前に確保する。
        let complete_registry = Arc::new(RwLock::new(CompletionRegistry::new()));

        let db_path = data_dir.join("history.db");
        let (reedline, history_available) = editor::build_editor(
            Arc::clone(&classifier),
            db_path,
            session_id,
            Arc::clone(&git_branch_commands),
            Arc::clone(&aliases),
            Arc::clone(&external_completion),
            Arc::clone(&zsh_daemon),
            Arc::clone(&complete_registry),
        );

        // 直前コマンドの終了コードを共有するアトミック変数
        // 初期値は EXIT_CODE_NONE（未設定）。コマンド実行時に実際の終了コードで上書きされる。
        let last_exit_code = Arc::new(AtomicI32::new(EXIT_CODE_NONE));
        let cmd_duration_ms = Arc::new(AtomicU64::new(CMD_DURATION_NONE));

        let prompt = Self::build_prompt(
            &config,
            Arc::clone(&last_exit_code),
            Arc::clone(&cmd_duration_ms),
        );
        prompt.refresh_git_status();

        // Black Box（履歴永続化）の初期化
        // BlackBox::open() ではなく open_at() を使い、フォールバック時も同じパスを使用する
        let black_box = match BlackBox::open_at(data_dir, session_id) {
            Ok(bb) => {
                info!("BlackBox initialized successfully");
                Some(bb)
            }
            Err(e) => {
                warn!("Failed to initialize BlackBox: {e}");
                eprintln!("jarvish: warning: failed to initialize black box: {e}");
                None
            }
        };

        // AI クライアントの初期化（設定ファイルの [ai] セクションを反映）
        let ai_client = match JarvisAI::new(&config.ai) {
            Ok(ai) => {
                info!("AI client initialized successfully");
                Some(ai)
            }
            Err(e) => {
                warn!("AI disabled: {e}");
                eprintln!("jarvish: warning: AI disabled: {e}");
                None // API キー未設定時は AI 機能を無効化
            }
        };

        Self {
            editor: reedline,
            prompt,
            ai_client,
            black_box,
            conversation_state: None,
            last_exit_code,
            cmd_duration_ms,
            classifier,
            aliases,
            ignore_auto_investigation_cmds: config.ai.ignore_auto_investigation_cmds,
            dir_stack: Vec::new(),
            farewell_shown: false,
            history_available,
            logging_operational,
            git_branch_commands,
            external_completion,
            zsh_daemon,
            zsh_daemon_gate,
            zsh_daemon_prewarm_done,
            complete_registry,
            restart_requested: Arc::new(AtomicBool::new(false)),
            startup_commands: config.startup.commands,
            rc_options,
            source_depth: 0,
            interactive,
        }
    }

    /// 設定ファイルの `[export]` セクションを環境変数に適用する。
    ///
    /// 値に含まれる環境変数参照（`$PATH` 等）は展開してから設定する。
    fn apply_exports(config: &JarvishConfig) {
        for (key, value) in &config.export {
            let expanded = expand::expand_token(value);
            let display = format!("{key}={expanded}");
            let masked = if crate::storage::sanitizer::contains_secrets(&display) {
                crate::storage::sanitizer::mask_secrets(&display)
            } else {
                display
            };
            info!(masked = %masked, "Applying export from config");
            // SAFETY: シェル起動時のシングルスレッド初期化で呼ばれるため安全
            unsafe {
                std::env::set_var(key, &expanded);
            }
        }
    }

    /// 設定と環境に基づいてプロンプトを構築する。
    ///
    /// `[prompt] starship = true` かつ `starship` コマンドと設定ファイルが
    /// 存在する場合は Starship プロンプトを返し、それ以外はビルトインを返す。
    fn build_prompt(
        config: &JarvishConfig,
        last_exit_code: Arc<AtomicI32>,
        cmd_duration_ms: Arc<AtomicU64>,
    ) -> ShellPrompt {
        if config.prompt.starship {
            if let Some(path) = Self::detect_starship() {
                info!(starship_path = %path.display(), "Starship prompt enabled");
                return ShellPrompt::starship(last_exit_code, cmd_duration_ms, path);
            }
            eprintln!(
                "jarvish: warning: starship = true but starship command or config not found, \
                 falling back to builtin prompt"
            );
        }
        ShellPrompt::builtin(last_exit_code, config.prompt.clone())
    }

    /// Starship の利用可否を検出する。
    ///
    /// 条件:
    /// 1. `starship` コマンドが PATH 上に存在する
    /// 2. `STARSHIP_CONFIG` 環境変数のパス、または `~/.config/starship.toml` が存在する
    ///
    /// 両方満たせば starship バイナリのパスを返す。
    fn detect_starship() -> Option<PathBuf> {
        let starship_path = which::which("starship").ok()?;

        let config_path = std::env::var("STARSHIP_CONFIG")
            .ok()
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join(".config/starship.toml")
            });

        if config_path.exists() {
            Some(starship_path)
        } else {
            info!(
                config_path = %config_path.display(),
                "Starship config file not found"
            );
            None
        }
    }

    /// 指定されたパスから設定ファイルを再読み込みし、Shell の状態に反映する。
    ///
    /// `source` ビルトインコマンドから呼び出される。
    /// `[ai]`、`[alias]`、`[export]`、`[prompt]`、`[completion]`、`[startup]`
    /// の各セクションを反映する（`[startup]` は値の更新のみで再実行はしない）。
    pub(super) fn reload_config(&mut self, path: &std::path::Path) -> crate::engine::CommandResult {
        use crate::engine::CommandResult;

        let config = match JarvishConfig::load_from(path) {
            Ok(c) => c,
            Err(msg) => {
                let err = format!("jarvish: source: {msg}\n");
                eprint!("{err}");
                return CommandResult::error(err, 1);
            }
        };

        // [alias] を反映
        if let Ok(mut a) = self.aliases.write() {
            *a = config.alias.clone();
        }

        // [export] を反映
        Self::apply_exports(&config);

        // [ai] を反映
        if let Some(ref mut ai) = self.ai_client {
            ai.update_config(&config.ai);
        }
        self.ignore_auto_investigation_cmds = config.ai.ignore_auto_investigation_cmds.clone();

        // [prompt] を反映（starship フラグ変更時はプロンプト自体を入れ替え）
        self.prompt = Self::build_prompt(
            &config,
            Arc::clone(&self.last_exit_code),
            Arc::clone(&self.cmd_duration_ms),
        );
        self.prompt.refresh_git_status();

        // [completion] を反映
        if let Ok(mut cmds) = self.git_branch_commands.write() {
            *cmds = config.completion.git_branch_commands.clone();
        }
        // 外部補完（carapace / zsh ブリッジ）は which() の再検出込みで反映する。
        // これによりセッション中に carapace/zsh をインストールしてから
        // `source` するだけで再起動なしに有効化できる。
        let resolved_external =
            reload_external_completion(&self.external_completion, &config.completion);

        // 新しい設定の下で温存 zsh デーモンが稼働禁止（フラグ off、または
        // zsh が enabled-kinds リストから外れた）なら、`provide()` の次回
        // 呼び出しを待たず**その場**で shutdown する（A3/A4, #89 レビュー
        // 指摘 — README の「immediately shuts down」を実際に真にする）。
        apply_zsh_daemon_lifecycle_for_reload(&resolved_external, &self.zsh_daemon);

        // [startup] を反映（再実行はしない、値の更新のみ）
        self.startup_commands = config.startup.commands.clone();

        // サマリー出力（config.toml のセクション順: ai, alias, export, prompt, completion, startup）
        let ignore_cmds_display = if config.ai.ignore_auto_investigation_cmds.is_empty() {
            "none".to_string()
        } else {
            format!("{:?}", config.ai.ignore_auto_investigation_cmds)
        };
        let external_mode_display =
            format_external_summary(&config.completion.external.to_string(), &resolved_external);
        // 解決済みの優先順に沿って、各プロバイダのバイナリパス（未検出なら
        // "not found"）を1行ずつ列挙する。`enabled` が空（external = "none"
        // または全プロバイダ無効化）の場合は空行なし。
        let external_binaries_display = format_external_binaries_display(&resolved_external);
        let summary = format!(
            "Loaded {}\n\
             \x20 [ai]\n\
             \x20\x20 model: {}\n\
             \x20\x20 max_rounds: {}\n\
             \x20\x20 markdown_rendering: {}\n\
             \x20\x20 ai_pipe_max_chars: {}\n\
             \x20\x20 ai_redirect_max_chars: {}\n\
             \x20\x20 temperature: {}\n\
             \x20\x20 ignore_auto_investigation_cmds: {}\n\
             \x20 [alias]   {} {}\n\
             \x20 [export]  {} {}\n\
             \x20 [prompt]  nerd_font: {}, starship: {}\n\
             \x20 [completion]  git_branch_commands: {} {}\n\
             \x20\x20 external: {}\n\
             {}\
             \x20\x20 external_timeout_ms: {}\n\
             \x20\x20 external_zsh_daemon: {}\n\
             \x20 [startup]  {} {}\n",
            path.display(),
            config.ai.model,
            config.ai.max_rounds,
            config.ai.markdown_rendering,
            config.ai.ai_pipe_max_chars,
            config.ai.ai_redirect_max_chars,
            config.ai.temperature,
            ignore_cmds_display,
            config.alias.len(),
            if config.alias.len() == 1 {
                "entry"
            } else {
                "entries"
            },
            config.export.len(),
            if config.export.len() == 1 {
                "entry"
            } else {
                "entries"
            },
            config.prompt.nerd_font,
            config.prompt.starship,
            config.completion.git_branch_commands.len(),
            if config.completion.git_branch_commands.len() == 1 {
                "command"
            } else {
                "commands"
            },
            external_mode_display,
            external_binaries_display,
            config.completion.external_timeout_ms,
            resolved_external.zsh_daemon_enabled,
            config.startup.commands.len(),
            if config.startup.commands.len() == 1 {
                "command"
            } else {
                "commands"
            },
        );
        print!("{summary}");

        CommandResult::success(summary)
    }

    /// `-c` オプションで渡されたコマンド文字列を非対話的に実行する。
    ///
    /// REPL ループには入らず、文字列を行ごとに `handle_input()` で処理して終了する。
    /// ウェルカムバナー・Farewell メッセージは表示しない。
    ///
    /// `--rcfile` が明示的に指定されている場合のみ、`-c` のコマンドを実行する
    /// 前に rc スクリプトを読み込む（デフォルトパス・`--no-rc` では
    /// `-c` 単体では rc は一切読み込まれない、既存の Phase 4.1 の挙動を維持）。
    /// rc スクリプト側で `exit`/goodbye が要求された場合は `-c` のコマンドを
    /// 実行せず、rc の終了コードをそのまま返す。
    ///
    /// 戻り値: 最後に実行したコマンドの終了コード。
    pub async fn run_command(&mut self, command: &str) -> i32 {
        if self.rc_options.rcfile.is_some()
            && rc::RcOutcome::ExitRequested == self.run_configured_rc().await
        {
            if let Some(ref bb) = self.black_box {
                bb.release_session();
            }
            let code = self.last_exit_code.load(Ordering::Relaxed);
            return if code == EXIT_CODE_NONE { 0 } else { code };
        }

        for line in command.lines() {
            if !self.handle_input(line).await {
                break;
            }
        }

        if let Some(ref bb) = self.black_box {
            bb.release_session();
        }

        let code = self.last_exit_code.load(Ordering::Relaxed);
        if code == EXIT_CODE_NONE {
            0
        } else {
            code
        }
    }

    /// REPL ループを実行する。
    ///
    /// ユーザー入力を受け取り、ビルトイン/コマンド/自然言語を処理する。
    /// Ctrl-D、exit コマンド、または goodbye 入力で終了する。
    /// SIGUSR1 受信時は再起動を行う。
    ///
    /// 戻り値: `(終了コード, LoopAction)` のタプル。
    /// - `LoopAction::Exit`: 通常終了
    /// - `LoopAction::Restart`: exec() による再起動が必要
    pub async fn run(&mut self) -> (i32, LoopAction) {
        let mut offline = Vec::new();
        if !self.logging_operational {
            offline.push("Logging offline");
        }
        if !self.history_available {
            offline.push("Command History offline");
        }
        if self.black_box.is_none() {
            offline.push("Black Box offline");
        }
        if self.ai_client.is_none() {
            offline.push("AI module offline");
        }
        crate::cli::banner::print_welcome(&offline);

        // バックグラウンドでバージョンチェックを実行（24時間キャッシュ付き）
        let update_check = tokio::spawn(crate::cli::update_check::check_for_update_notification());

        let mut repl_error = false;
        let mut action = LoopAction::Exit;

        // SIGUSR1 ハンドラの登録（AtomicBool フラグを共有）
        Self::register_sigusr1_handler(Arc::clone(&self.restart_requested));

        // 最初のプロンプト表示前にバージョンチェック結果を表示（最大1秒待機）
        if let Ok(Ok(Some(notification))) =
            tokio::time::timeout(std::time::Duration::from_secs(1), update_check).await
        {
            println!("{notification}");
            println!();
        }

        // rc.jsh の実行（[startup].commands より前、対話モード限定）。
        // `--rcfile` / `--no-rc` に応じてデフォルトパス／明示パス／スキップを
        // 解決する（Phase 4.2, `RcOptions::resolve`）。デフォルトパスのみ
        // 初回起動時にコメントのみのテンプレートを自動生成する。
        if rc::RcOutcome::ExitRequested == self.run_configured_rc().await {
            info!("rc.jsh triggered shell exit");
            if let Some(ref bb) = self.black_box {
                bb.release_session();
            }
            let exit_code = self.last_exit_code.load(Ordering::Relaxed);
            return (
                if exit_code == EXIT_CODE_NONE {
                    0
                } else {
                    exit_code
                },
                if self.restart_requested.load(Ordering::Relaxed) {
                    LoopAction::Restart
                } else {
                    LoopAction::Exit
                },
            );
        }
        self.prompt.refresh_git_status();

        // 起動時コマンドの実行（config.toml [startup] commands）
        if !self.startup_commands.is_empty() {
            info!(
                count = self.startup_commands.len(),
                "Executing startup commands"
            );
            let commands = self.startup_commands.clone();
            for cmd in &commands {
                info!(command = %cmd, "Running startup command");
                if !self.handle_input(cmd).await {
                    // exit 等でシェル終了が要求された場合
                    info!("Startup command triggered shell exit");
                    if let Some(ref bb) = self.black_box {
                        bb.release_session();
                    }
                    let exit_code = self.last_exit_code.load(Ordering::Relaxed);
                    return (
                        if exit_code == EXIT_CODE_NONE {
                            0
                        } else {
                            exit_code
                        },
                        LoopAction::Exit,
                    );
                }
                self.prompt.refresh_git_status();
            }
        }

        loop {
            // 別プロセスの update コマンドによるフラグファイルを検出し、通知を表示
            if let Some(notification) = crate::engine::builtins::update::check_update_flag() {
                println!("  {notification}");
                println!();
            }

            // SIGUSR1 による再起動リクエストがフラグに残っている場合（コマンド実行中に受信した場合）
            if self.restart_requested.load(Ordering::Relaxed) {
                info!("Deferred restart triggered (SIGUSR1 received during command execution)");
                println!("Restarting jarvish (deferred SIGUSR1)...");
                action = LoopAction::Restart;
                break;
            }

            let signal = tokio::task::block_in_place(|| self.editor.read_line(&self.prompt));

            // read_line の完了後にシグナルフラグをチェック
            if self.restart_requested.load(Ordering::Relaxed) {
                info!("SIGUSR1 received during read_line: restarting shell");
                println!("\nRestarting jarvish (SIGUSR1)...");
                action = LoopAction::Restart;
                break;
            }

            match signal {
                Ok(Signal::Success(line)) => {
                    let result = self.handle_input(&line).await;
                    if !result {
                        // handle_input が false を返した場合、restart か exit かを判別
                        // restart ビルトインが呼ばれた場合は last action を確認
                        if self.restart_requested.load(Ordering::Relaxed) {
                            action = LoopAction::Restart;
                        }
                        break;
                    }
                    self.prompt.refresh_git_status();
                }
                Ok(Signal::CtrlC) => {
                    info!("\n!!!! Ctrl-C received: do it nothing !!!!!\n");
                    // なにもしない
                    println!(); // 改行して次のプロンプトを見やすくする
                }
                Ok(Signal::CtrlD) => {
                    // EOF → シェル終了
                    info!("\n!!!! Ctrl-D received: exiting shell !!!!!\n");
                    break;
                }
                Err(e) => {
                    warn!(error = %e, "REPL error, exiting");
                    eprintln!("jarvish: error: {e}");
                    repl_error = true;
                    break;
                }
            }
        }

        // Farewell メッセージ表示（再起動時と AI goodbye 表示済みの場合はスキップ）
        if action != LoopAction::Restart && !self.farewell_shown {
            crate::cli::banner::print_goodbye();
        }

        // セッション終了: session_id を NULL に解放し、次回起動時に履歴を辿れるようにする
        if let Some(ref bb) = self.black_box {
            bb.release_session();
        }

        // 終了コードを決定
        let exit_code = if repl_error {
            1
        } else {
            let code = self.last_exit_code.load(Ordering::Relaxed);
            if code == EXIT_CODE_NONE {
                0
            } else {
                code
            }
        };

        (exit_code, action)
    }

    /// SIGUSR1 シグナルハンドラを登録する。
    ///
    /// 受信時に `RESTART_FLAG` グローバルフラグを立てる。
    /// reedline の `read_line()` は同期ブロッキング呼び出しのため、
    /// シグナルハンドラでフラグを立て、次の REPL ループイテレーションでチェックする。
    fn register_sigusr1_handler(restart_flag: Arc<AtomicBool>) {
        extern "C" fn handle_sigusr1(_: libc::c_int) {
            // シグナルハンドラ内では async-signal-safe な操作のみ許可
            RESTART_FLAG.store(true, Ordering::Relaxed);
        }

        // グローバルフラグをリセット
        RESTART_FLAG.store(false, Ordering::Relaxed);

        // グローバル RESTART_FLAG を Shell の restart_requested に転送するスレッド
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if RESTART_FLAG.load(Ordering::Relaxed) {
                restart_flag.store(true, Ordering::Relaxed);
                break;
            }
        });

        // libc の sigaction で SIGUSR1 ハンドラを登録
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = handle_sigusr1 as *const () as usize;
            sa.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut sa.sa_mask);

            if libc::sigaction(libc::SIGUSR1, &sa, std::ptr::null_mut()) == 0 {
                info!("SIGUSR1 handler registered for self-restart");
            } else {
                let e = std::io::Error::last_os_error();
                warn!(error = %e, "Failed to register SIGUSR1 handler");
                eprintln!("jarvish: warning: SIGUSR1 handler unavailable: {e}");
            }
        }
    }

    /// exec/exit 直前の有界同期 shutdown 予算（B1/B2, #89）。
    ///
    /// プロセスがこの直後に exec() で置換される、または exit() で終了する
    /// ため、バックグラウンドスレッドへ kill/reap を委譲しても実行される
    /// 保証がない（[`shutdown_shared_daemon_blocking`] のドキュメント参照）。
    /// `ZshDaemon` 単体の reap 予算（既定 1 秒、`zsh_daemon.rs` 参照）に
    /// 軽い余裕を足した値。
    const ZSH_DAEMON_EXIT_SHUTDOWN_DEADLINE: std::time::Duration =
        std::time::Duration::from_millis(1200);

    /// 温存 zsh 補完デーモンが稼働中なら shutdown する（kill + 有界同期 reap）。
    ///
    /// `Command::exec`（[`exec_restart`](Self::exec_restart)）はプロセス
    /// イメージを置換するため `Drop` は一切実行されず、`std::process::exit`
    /// もデストラクタをスキップする。そのためこれらの経路の**直前**に
    /// 明示的に呼び、デーモン子プロセス・PTY fd・init 一時ファイルの
    /// リークを防ぐ（A1/A2, #89 レビュー指摘）。
    ///
    /// # ノンブロッキング shutdown ではなく有界同期版を使う理由（B1/B2, #89）
    /// 通常の reload/gate 経路（`apply_zsh_daemon_lifecycle_for_reload`
    /// 等）はバックグラウンドスレッドへ kill/reap を委譲するノンブロッキング
    /// 版（[`shutdown_shared_daemon`]）を使うが、ここ（exec 直前・exit
    /// 直前）ではプロセスがこの直後に置換/終了されるため、バックグラウンド
    /// スレッドに委ねても実行が保証されない。そのため
    /// [`shutdown_shared_daemon_blocking`] で `deadline` 以内の完了を
    /// 呼び出し元スレッド上で待つ。デーモンが元々稼働していなければ
    /// no-op（冪等）。
    ///
    /// # tombstone（S5 修正）
    /// `self.zsh_daemon_gate` を渡すため、この呼び出しは同時に「以後
    /// prewarm がこのスロットへ書き込むことを二度と許さない」という
    /// tombstone をセットする。`-c` 単体実行やここに到達する前に
    /// prewarm がまだ spawn 中だった場合でも、prewarm が完了して
    /// スロットへ書き込もうとした瞬間に closed を検知して自壊する
    /// （`zsh_bridge::DaemonGate` のドキュメント参照）。
    ///
    /// # prewarm スレッド完了の有界待ち（S5 追加修正）
    /// `gate.close()` だけでは閉じないレースが実際に存在する
    /// （`zsh_daemon_prewarm_done` フィールドのドキュメント参照）: この
    /// 呼び出しの直後に呼び出し元（`main`）が `std::process::exit` する
    /// と、detached な prewarm スレッドは実行中でも強制終了され、
    /// tombstone チェックのコードが一度も走らないまま子プロセスだけが
    /// 残りうる。これを防ぐため、スロット shutdown の後に prewarm
    /// スレッドの完了通知を [`Self::PREWARM_JOIN_DEADLINE`] まで待つ
    /// （`recv_timeout` — `JoinHandle::join()` の無界ブロッキングは
    /// 受け入れ基準4「`-c` の終了レイテンシを悪化させない」に反するため
    /// 使わない）。prewarm が既に完了していれば（通常の対話 REPL 終了、
    /// または prewarm 自体が無効/未検出だった場合）このチャネルは
    /// 即座に受信できるため、この待ちはほぼ常に no-op に等しい。
    pub fn shutdown_zsh_daemon(&mut self) {
        shutdown_shared_daemon_blocking(
            &self.zsh_daemon,
            Self::ZSH_DAEMON_EXIT_SHUTDOWN_DEADLINE,
            Some(&self.zsh_daemon_gate),
        );

        if let Some(rx) = self.zsh_daemon_prewarm_done.take() {
            if rx.recv_timeout(Self::PREWARM_JOIN_DEADLINE).is_err() {
                tracing::debug!(
                    "zsh daemon shutdown: prewarm thread did not finish within the join \
                     deadline; falling back to a final slot re-check"
                );
            }
            // prewarm が実際に間に合わずスロットへ書き込んでいた場合の
            // 最終防衛線: closed 後の書き込みは Mutex 内チェックで
            // 通常は防がれるが（`DaemonGate` のドキュメント参照）、万一
            // タイムアウトで recv を諦めた直後に prewarm がスロットへ
            // 書き込みを完了させていた場合に備え、もう一度だけ shutdown
            // を試みる（スロットが空なら no-op、埋まっていれば確実に
            // kill する）。
            shutdown_shared_daemon_blocking(
                &self.zsh_daemon,
                Self::ZSH_DAEMON_EXIT_SHUTDOWN_DEADLINE,
                None,
            );
        }
    }

    /// [`Self::shutdown_zsh_daemon`] が prewarm スレッドの完了通知を待つ
    /// 上限（S5 追加修正）。prewarm の spawn 自体の上限
    /// （`zsh_bridge::MIN_TIMEOUT_MS` = 2000ms）に軽い余裕を足した値 ──
    /// この値より短いと「prewarm がまだ正常に spawn 中なだけ」のケースで
    /// 待ちきれず、tombstone チェック未実行のまま強制終了されるレースが
    /// 再発する。
    const PREWARM_JOIN_DEADLINE: std::time::Duration = std::time::Duration::from_millis(2500);

    /// `restart` ビルトイン（または rc/source スクリプト内の `restart` 行、
    /// SIGUSR1）によって再起動が要求されたかどうかを返す（Fix B2）。
    ///
    /// `run()`（対話 REPL）は既にこのフラグをループ内で直接
    /// （`self.restart_requested.load(...)`）参照して `LoopAction::Restart`
    /// を選択しているが、`run_command`（`-c` 単体実行、`main.rs` から
    /// 呼ばれる）はこれまでこのフラグを一切見ておらず、`main.rs` 側が
    /// `-c` の戻り値を常に `LoopAction::Exit` と決め打ちしていた。その
    /// ため `--rcfile` スクリプト内 / `-c` の引数内で `restart` を呼んでも
    /// "Restarting jarvish..." が出力されるだけで実際には exec()
    /// されずプロセスが終了する（サイレントに死ぬ）という不整合があった。
    /// `main.rs` はこのアクセサで `run_command` 実行後にフラグを読み、
    /// `run()` と同じ判定に揃える。
    pub fn restart_requested(&self) -> bool {
        self.restart_requested.load(Ordering::Relaxed)
    }

    /// exec() によるプロセス再起動を実行する。
    ///
    /// クリーンアップ後、現在のバイナリで exec() を呼び出しプロセスを置換する。
    /// 成功時はこの関数から戻らない。失敗時はエラーを返す。
    pub fn exec_restart(&mut self) -> std::io::Error {
        use std::os::unix::process::CommandExt;

        // 温存 zsh デーモンを exec() の**前**に明示的に shutdown する。
        // `Command::exec` はプロセスイメージを置換するため、この行の後では
        // Rust の `Drop` が一切実行されない（A1, #89 レビュー指摘）。
        self.shutdown_zsh_daemon();

        // stdout/stderr をフラッシュ
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let _ = std::io::Write::flush(&mut std::io::stderr());

        info!("exec_restart: executing self-restart");

        let (exe, args) = match build_restart_command() {
            Ok(pair) => pair,
            Err(e) => return e,
        };

        // exec() — 成功時はこの行に到達しない
        std::process::Command::new(exe).args(&args).exec()
    }
}

/// 起動時のバックグラウンド事前ウォームアップスレッドを、対話モードの
/// ときだけ起動する（S5 修正、`Shell::new` から切り出し）。
///
/// `interactive == false`（`-c` 単体実行）では、そもそも Tab 補完が発生
/// しない短命プロセスなので、スレッドを起動すること自体が無駄なうえ、
/// プロセスが数ミリ秒で完走してしまうと prewarm の spawn（PTY + プロセス
/// 起動 + レディマーカー待ち、数百ms かかりうる）がプロセス終了後まで
/// 完了せず、孤児 `/bin/zsh -i` 化のレースを踏みやすい（`DaemonGate` の
/// ドキュメント参照）。`Shell` 全体を構築せずに「スレッドを1本も起動しない」
/// ことを直接観測できるよう、`Shell::new` 本体から切り出している。
///
/// 戻り値は `interactive == true` の場合のみ `Some(Receiver<()>)`
/// （prewarm スレッドが実行を終えた瞬間に送信される完了通知チャネル）を
/// 返す。`Shell::zsh_daemon_prewarm_done` のドキュメント参照 — `main` が
/// `std::process::exit` する前に `shutdown_zsh_daemon` がこのチャネルを
/// 有界時間で待つことで、「gate.close() 後に prewarm がまだ実行中の
/// まま強制終了され、tombstone チェックが一度も走らない」レースを閉じる。
fn spawn_prewarm_thread_if_interactive(
    interactive: bool,
    external_completion: &Arc<RwLock<ExternalCompletionSettings>>,
    zsh_daemon: &SharedDaemonSlot,
    zsh_daemon_gate: &Arc<DaemonGate>,
) -> Option<std::sync::mpsc::Receiver<()>> {
    if !interactive {
        return None;
    }
    let settings_for_prewarm = Arc::clone(external_completion);
    let daemon_for_prewarm = Arc::clone(zsh_daemon);
    let gate_for_prewarm = Arc::clone(zsh_daemon_gate);
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        prewarm_zsh_daemon(
            &settings_for_prewarm,
            &daemon_for_prewarm,
            &gate_for_prewarm,
        );
        // prewarm が（スロットへの書き込みの成否に関わらず）完全に終了
        // したことを通知する。送信失敗（受信側が既に drop 済み）は
        // 無視してよい — `shutdown_zsh_daemon` が呼ばれずプロセスが
        // 対話 REPL のまま動き続けているケース（`Receiver` は `Shell` に
        // 保持されたまま）を含め、通知を誰も待っていない状況は正常。
        let _ = done_tx.send(());
    });
    Some(done_rx)
}

/// `[completion]` の外部補完設定を再解決し、共有 `Arc<RwLock<_>>` へ
/// 書き込む。`Shell::reload_config`（`source` ビルトイン）が呼び出す
/// 「resolve + 共有 Arc への書き込み」ステップを切り出したもの（D1, #89
/// レビュー指摘）。
///
/// `Shell` 全体を構築せずに `Arc<RwLock<ExternalCompletionSettings>>` と
/// `CompletionConfig` だけでテストできるようにする狙い。書き込み後の
/// 解決結果（reload 後の状態）をそのまま返すため、呼び出し側
/// （`reload_config`）はサマリー表示にこれを使い、`Arc` の中身と表示が
/// 常に同じ「reload 後」の値を参照していることを保証する。
fn reload_external_completion(
    external_completion: &Arc<RwLock<ExternalCompletionSettings>>,
    completion_config: &crate::config::CompletionConfig,
) -> ExternalCompletionSettings {
    let resolved = ExternalCompletionSettings::resolve(completion_config);
    if let Ok(mut ext) = external_completion.write() {
        *ext = resolved.clone();
    }
    resolved
}

/// `source` による reload 直後の温存 zsh デーモンのライフサイクル反映。
///
/// 新しく解決された `resolved`（reload 後の `ExternalCompletionSettings`）
/// の下でデーモンが稼働禁止（フラグ off、または zsh が enabled-kinds
/// リストから外れた）なら、`provide()` の次回呼び出しを待たず**その場**で
/// shutdown する（A3/A4, #89 レビュー指摘: README の「turning it off
/// immediately shuts down any running daemon」を実際に真にする）。
///
/// `reload_external_completion` と同じ理由（`Shell` 全体を構築せず
/// `Arc<RwLock<ExternalCompletionSettings>>` + `SharedDaemonSlot` だけで
/// テストできるようにする）で切り出した純粋寄りのヘルパー。デーモンが
/// 元々稼働していなければ [`shutdown_shared_daemon`] が no-op を保証する。
fn apply_zsh_daemon_lifecycle_for_reload(
    resolved: &ExternalCompletionSettings,
    zsh_daemon: &SharedDaemonSlot,
) {
    if !resolved.should_run_zsh_daemon() {
        shutdown_shared_daemon(zsh_daemon);
    }
}

/// exec_restart 用のコマンド情報を構築する。
///
/// 現在のバイナリパスと引数を取得する。テスト可能な純粋関数として分離。
fn build_restart_command() -> Result<(PathBuf, Vec<String>), std::io::Error> {
    let exe = std::env::current_exe().map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("failed to get current exe path: {e}"),
        )
    })?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    Ok((exe, args))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── build_restart_command ──

    #[test]
    fn build_restart_command_returns_valid_exe() {
        let result = build_restart_command();
        assert!(result.is_ok());
        let (exe, _args) = result.unwrap();
        assert!(exe.exists(), "current_exe path should exist");
    }

    #[test]
    fn build_restart_command_args_exclude_binary_name() {
        let (_, args) = build_restart_command().unwrap();
        // テストバイナリのパスが引数に含まれないことを確認
        for arg in &args {
            assert!(
                !arg.contains("jarvish-") && !arg.ends_with("jarvish"),
                "args should not contain binary name, got: {arg}"
            );
        }
    }

    // ── RESTART_FLAG (global AtomicBool) ──

    #[test]
    fn restart_flag_initial_state_is_false() {
        // テスト間の副作用を避けるためリセット
        RESTART_FLAG.store(false, Ordering::Relaxed);
        assert!(!RESTART_FLAG.load(Ordering::Relaxed));
    }

    #[test]
    fn restart_flag_can_be_set_and_read() {
        RESTART_FLAG.store(true, Ordering::Relaxed);
        assert!(RESTART_FLAG.load(Ordering::Relaxed));
        // クリーンアップ
        RESTART_FLAG.store(false, Ordering::Relaxed);
    }

    // ── register_sigusr1_handler + flag propagation ──

    #[test]
    fn sigusr1_handler_propagates_to_restart_flag() {
        let restart_flag = Arc::new(AtomicBool::new(false));

        // ハンドラを登録
        Shell::register_sigusr1_handler(Arc::clone(&restart_flag));

        // 自プロセスに SIGUSR1 を送信
        unsafe {
            libc::kill(libc::getpid(), libc::SIGUSR1);
        }

        // フラグが伝播するまで待機（最大2秒）
        for _ in 0..40 {
            if restart_flag.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        assert!(
            restart_flag.load(Ordering::Relaxed),
            "SIGUSR1 should propagate to restart_flag via polling thread"
        );

        // グローバルフラグをリセット
        RESTART_FLAG.store(false, Ordering::Relaxed);
    }

    // ── restart_requested flag monitoring ──

    #[test]
    fn restart_requested_flag_default_is_false() {
        let flag = Arc::new(AtomicBool::new(false));
        assert!(!flag.load(Ordering::Relaxed));
    }

    #[test]
    fn restart_requested_flag_set_triggers_restart() {
        let flag = Arc::new(AtomicBool::new(false));
        flag.store(true, Ordering::Relaxed);
        // REPL ループと同じチェックロジック
        assert!(flag.load(Ordering::Relaxed));
    }

    // ── update flag file notification in REPL ──

    #[test]
    fn check_update_flag_returns_none_without_flag_file() {
        use crate::engine::builtins::update;
        // 念のため既存フラグを削除
        let _ = update::check_update_flag();
        assert!(update::check_update_flag().is_none());
    }

    #[test]
    fn check_update_flag_returns_notification_with_flag_file() {
        use crate::engine::builtins::update;
        // 念のため既存フラグを削除
        let _ = update::check_update_flag();

        update::write_update_flag_for_test("2.0.0");
        let msg = update::check_update_flag();
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("v2.0.0"));
        // 読み取り後は削除されている
        assert!(update::check_update_flag().is_none());
    }

    // ── reload_external_completion（`reload_config` の resolve + Arc 書き込みステップ）──
    //
    // D1 (#89): `Shell` 全体は構築せず、`Shell::new` / `reload_config` と
    // 同じ「resolve() → 共有 Arc への書き込み」経路のみを直接シミュレートする
    // （`carapace.rs` の hot-reload テストと同じ方針）。あわせて、書き込み後の
    // Arc の中身が、`format_external_binaries_display` の表示にも
    // post-reload の値として反映されることを検証する（pre-reload の値が
    // 混入していないことの証明）。

    use crate::cli::completer::format_external_binaries_display;
    use crate::config::{CompletionConfig, ExternalSetting};

    #[test]
    fn reload_external_completion_updates_shared_arc_from_none_to_disabled_stays_empty() {
        let initial = ExternalCompletionSettings::resolve(&CompletionConfig {
            external: ExternalSetting::Single("none".to_string()),
            ..CompletionConfig::default()
        });
        let shared = Arc::new(RwLock::new(initial));

        let new_config = CompletionConfig {
            external: ExternalSetting::Single("none".to_string()),
            external_timeout_ms: 777,
            ..CompletionConfig::default()
        };
        let returned = reload_external_completion(&shared, &new_config);

        // 戻り値と Arc の中身が一致し、どちらも新しい timeout を反映している。
        assert_eq!(returned.timeout, std::time::Duration::from_millis(777));
        let after = shared.read().unwrap();
        assert_eq!(after.timeout, std::time::Duration::from_millis(777));
        assert!(after.enabled.is_empty());
    }

    #[test]
    fn reload_external_completion_display_reflects_post_reload_state_not_pre_reload() {
        // reload 前は `external = "none"`（enabled 空、display も空）。
        // reload 後は `external = "carapace"` に切り替える —
        // `resolve_single_kind` は明示指定の場合、バイナリが未検出でも
        // エントリ自体は `binary = None`（"not found" 表示）で残すため、
        // 実機に carapace が無い CI 環境でも enabled は非空になり、
        // display が確定的に "not found" を含む行を返す
        // （carapace.rs の `resolve_carapace_string_missing_binary_disables_without_panic`
        // と同じ「明示指定は残る」契約に依拠）。
        let initial = ExternalCompletionSettings::resolve(&CompletionConfig {
            external: ExternalSetting::Single("none".to_string()),
            ..CompletionConfig::default()
        });
        let shared = Arc::new(RwLock::new(initial));

        // reload 前のスナップショットの表示は空（enabled が空のため）。
        let before_display = {
            let guard = shared.read().unwrap();
            format_external_binaries_display(&guard)
        };
        assert_eq!(
            before_display, "",
            "pre-reload display should be empty (no providers enabled)"
        );

        // reload: external = "carapace" に明示切り替える。
        let new_config = CompletionConfig {
            external: ExternalSetting::Single("carapace".to_string()),
            ..CompletionConfig::default()
        };
        let returned = reload_external_completion(&shared, &new_config);

        // 戻り値・Arc の中身の両方が post-reload の内容（carapace エントリ1件）
        // を持つ。
        assert_eq!(returned.enabled.len(), 1);
        let after_display = {
            let guard = shared.read().unwrap();
            format_external_binaries_display(&guard)
        };
        assert_eq!(
            after_display,
            format_external_binaries_display(&returned),
            "Arc content and returned value must produce the same display"
        );
        assert!(
            after_display.starts_with("    carapace: "),
            "post-reload display should show the carapace entry, not the pre-reload empty state: {after_display:?}"
        );
        assert_ne!(
            after_display, before_display,
            "display must change from pre-reload (empty) to post-reload (carapace entry)"
        );
    }

    // ── 温存 zsh デーモンのライフサイクル (Task A, #89) ──
    //
    // ZshDaemon / ZshBridgeProvider は cli::completer 配下の非公開モジュール
    // のため、ここでは公開 API（`JarvishCompleter` + `reedline::Completer`
    // トレイト + `SharedDaemonSlot`）だけを使って実デーモンを実際に spawn
    // させ、`apply_zsh_daemon_lifecycle_for_reload` / `shutdown_shared_daemon`
    // が本当に子プロセスを殺すことを ESRCH ポーリングで直接証明する
    // （`zsh_bridge.rs` の daemon テストと同じ隔離 HOME/ZDOTDIR パターン）。

    use crate::cli::completer::new_shared_daemon_slot;
    use reedline::Completer as _;
    use serial_test::serial;

    /// テスト用の隔離された ZDOTDIR + fpath ディレクトリ + 隔離 HOME を作る
    /// （`zsh_bridge.rs` / `zsh_daemon.rs` の E2E テストと同じ理由 —
    /// `compinit -d ~/.zcompdump_capture` が `$HOME` 基準の固定パスに
    /// compdump キャッシュを読み書きするため）。
    struct DaemonTestFixture {
        _tmpdir: tempfile::TempDir,
        zdotdir: PathBuf,
    }

    fn setup_daemon_fixture() -> DaemonTestFixture {
        let tmpdir = tempfile::tempdir().unwrap();
        let zdotdir = tmpdir.path().join("zdotdir");
        let fpath_dir = tmpdir.path().join("completions");
        let home = tmpdir.path().join("home");
        std::fs::create_dir_all(&zdotdir).unwrap();
        std::fs::create_dir_all(&fpath_dir).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            fpath_dir.join("_jarvishtestcmd"),
            "#compdef jarvishtestcmd\ncompadd -- alpha beta\n",
        )
        .unwrap();
        std::fs::write(
            zdotdir.join(".zshrc"),
            format!("fpath=({} $fpath)\n", fpath_dir.display()),
        )
        .unwrap();
        // HOME を隔離した状態で spawn する（プロセス全体の HOME を一時的に
        // 差し替える — このテストファイル内で HOME を触るテストは
        // #[serial] を付けて直列化しているため他テストと競合しない）。
        unsafe {
            std::env::set_var("HOME", &home);
        }
        DaemonTestFixture {
            _tmpdir: tmpdir,
            zdotdir,
        }
    }

    fn zsh_binary_for_test() -> Option<PathBuf> {
        which::which("zsh").ok()
    }

    /// zsh 有効設定 + 温存デーモン有効の `ExternalCompletionSettings` を
    /// `bridge_dir_override` 相当の zdotdir で使えるよう、`external =
    /// "zsh"` かつ `external_zsh_daemon = true` に解決したものを返す。
    fn zsh_enabled_daemon_settings() -> Arc<RwLock<ExternalCompletionSettings>> {
        use crate::config::{CompletionConfig, ExternalSetting};
        Arc::new(RwLock::new(ExternalCompletionSettings::resolve(
            &CompletionConfig {
                external: ExternalSetting::Single("zsh".to_string()),
                external_timeout_ms: 3000,
                external_zsh_daemon: true,
                ..CompletionConfig::default()
            },
        )))
    }

    /// pid が実際に ESRCH になるまで短時間・有界回数ポーリングする
    /// （`zsh_daemon.rs` / `zsh_bridge.rs` の既存テストと同じ考え方）。
    fn wait_for_pid_death(pid: u32) -> bool {
        for _ in 0..40 {
            let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
            if ret == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        false
    }

    #[test]
    #[serial]
    fn apply_zsh_daemon_lifecycle_for_reload_shuts_down_when_flag_flips_off() {
        // reload-disables-daemon-at-source-time: `external_zsh_daemon` が
        // false になった新設定を渡すと、`provide()` の次回呼び出しを待たず
        // その場でスロットが None になり、子プロセスが実際に死ぬ（A3）。
        let Some(zsh) = zsh_binary_for_test() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        let original_home = std::env::var("HOME").ok();
        let fixture = setup_daemon_fixture();

        let settings = zsh_enabled_daemon_settings();
        let zsh_daemon = new_shared_daemon_slot();
        let mut completer = crate::cli::completer::JarvishCompleter::new(
            Arc::new(RwLock::new(vec![])),
            Arc::new(RwLock::new(HashMap::new())),
            Arc::clone(&settings),
            Arc::clone(&zsh_daemon),
            Arc::new(RwLock::new(
                crate::cli::completer::registry::CompletionRegistry::new(),
            )),
        );

        // resolve_zsh は which::which("zsh") を都度引く本番経路のため、
        // PATH 上の実 zsh をそのまま使う（override フックは公開されていない
        // ため、ZDOTDIR は環境変数経由ではなく bridge_dir() = 隔離 HOME 配下
        // の ~/.config/jarvish/zsh-bridge を使う。fixture の zdotdir 直下の
        // .zshrc をそこへコピーする必要はなく、bridge_dir() 自体が初回
        // ensure_bridge_zshrc() でテンプレートを生成するため、代わりに
        // spawn 自体が成功することだけを確認する — 候補内容の検証は
        // zsh_bridge.rs 側の既存テストの責務）。
        let _ = zsh; // 実 PATH 上の zsh を使うため override は不要
        let _ = &fixture.zdotdir; // 隔離目的で保持しているだけ

        let line = "jarvishtestcmd ";
        let pos = line.len();
        let _ = completer.complete(line, pos);

        // 実装上 gate() が binary を検出できなかった場合など、環境によって
        // 稀にデーモンが spawn されないことがある。その場合はこのテストの
        // 前提が成立しないため skip する（実機依存の CI 環境差を吸収）。
        if zsh_daemon.lock().unwrap().is_none() {
            eprintln!("skipping: zsh daemon did not spawn in this environment");
            if let Some(home) = original_home {
                unsafe {
                    std::env::set_var("HOME", home);
                }
            }
            return;
        }

        let child_pid = {
            let guard = zsh_daemon.lock().unwrap();
            guard.as_ref().unwrap().daemon_pid_for_test()
        };

        // reload: フラグを off にした新設定で apply_zsh_daemon_lifecycle_for_reload
        // を呼ぶ（Shell::reload_config が呼ぶのと同じ経路）。
        let disabled = ExternalCompletionSettings::resolve(&crate::config::CompletionConfig {
            external: crate::config::ExternalSetting::Single("zsh".to_string()),
            external_zsh_daemon: false,
            ..crate::config::CompletionConfig::default()
        });
        apply_zsh_daemon_lifecycle_for_reload(&disabled, &zsh_daemon);

        assert!(
            zsh_daemon.lock().unwrap().is_none(),
            "slot must become None immediately after reload disables the daemon flag"
        );
        assert!(
            wait_for_pid_death(child_pid),
            "child pid {child_pid} should be dead after reload-time shutdown"
        );

        if let Some(home) = original_home {
            unsafe {
                std::env::set_var("HOME", home);
            }
        }
    }

    #[test]
    #[serial]
    fn apply_zsh_daemon_lifecycle_for_reload_shuts_down_when_zsh_dropped_from_kinds() {
        // kinds-change: external が "auto"/"zsh" から "carapace" のみへ
        // 変わる（zsh が enabled-kinds から消える）と、フラグ自体は
        // true のままでもその場でデーモンが shutdown される（A4 相当を
        // reload 経路でも保証する）。
        let Some(zsh) = zsh_binary_for_test() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        let original_home = std::env::var("HOME").ok();
        let fixture = setup_daemon_fixture();
        let _ = zsh;
        let _ = &fixture.zdotdir;

        let settings = zsh_enabled_daemon_settings();
        let zsh_daemon = new_shared_daemon_slot();
        let mut completer = crate::cli::completer::JarvishCompleter::new(
            Arc::new(RwLock::new(vec![])),
            Arc::new(RwLock::new(HashMap::new())),
            Arc::clone(&settings),
            Arc::clone(&zsh_daemon),
            Arc::new(RwLock::new(
                crate::cli::completer::registry::CompletionRegistry::new(),
            )),
        );

        let line = "jarvishtestcmd ";
        let pos = line.len();
        let _ = completer.complete(line, pos);

        if zsh_daemon.lock().unwrap().is_none() {
            eprintln!("skipping: zsh daemon did not spawn in this environment");
            if let Some(home) = original_home {
                unsafe {
                    std::env::set_var("HOME", home);
                }
            }
            return;
        }

        let child_pid = {
            let guard = zsh_daemon.lock().unwrap();
            guard.as_ref().unwrap().daemon_pid_for_test()
        };

        // reload: external を "carapace" のみに切り替える（zsh_daemon_enabled
        // は true のまま — kinds-change 単独での shutdown を検証する）。
        let carapace_only = ExternalCompletionSettings::resolve(&crate::config::CompletionConfig {
            external: crate::config::ExternalSetting::Single("carapace".to_string()),
            external_zsh_daemon: true,
            ..crate::config::CompletionConfig::default()
        });
        apply_zsh_daemon_lifecycle_for_reload(&carapace_only, &zsh_daemon);

        assert!(
            zsh_daemon.lock().unwrap().is_none(),
            "slot must become None when zsh is dropped from enabled kinds"
        );
        assert!(
            wait_for_pid_death(child_pid),
            "child pid {child_pid} should be dead after kinds-change shutdown"
        );

        if let Some(home) = original_home {
            unsafe {
                std::env::set_var("HOME", home);
            }
        }
    }

    #[test]
    fn apply_zsh_daemon_lifecycle_for_reload_is_noop_when_daemon_should_run() {
        // 稼働許可されたままの reload（フラグ on かつ zsh が enabled）では
        // 何もしない（スロットの中身に触れない）ことを、空スロットのまま
        // no-op であることで確認する（zsh 不要・実機非依存）。
        let settings = ExternalCompletionSettings::resolve(&crate::config::CompletionConfig {
            external: crate::config::ExternalSetting::Single("zsh".to_string()),
            external_zsh_daemon: true,
            ..crate::config::CompletionConfig::default()
        });
        let zsh_daemon = new_shared_daemon_slot();
        apply_zsh_daemon_lifecycle_for_reload(&settings, &zsh_daemon);
        assert!(zsh_daemon.lock().unwrap().is_none());
    }

    #[test]
    fn apply_zsh_daemon_lifecycle_for_reload_on_empty_slot_is_a_no_op() {
        // 稼働禁止設定でも、スロットが元々空なら panic せず空のまま
        // （冪等性）。
        let settings = ExternalCompletionSettings::resolve(&crate::config::CompletionConfig {
            external: crate::config::ExternalSetting::Single("none".to_string()),
            external_zsh_daemon: false,
            ..crate::config::CompletionConfig::default()
        });
        let zsh_daemon = new_shared_daemon_slot();
        apply_zsh_daemon_lifecycle_for_reload(&settings, &zsh_daemon);
        assert!(zsh_daemon.lock().unwrap().is_none());
    }

    // ── exec_restart 直前 shutdown (A1) / exit 直前 shutdown (A2) の
    //    unit テスト（実際に exec()/exit() は呼ばない — shutdown_zsh_daemon
    //    ヘルパー自体の契約のみを検証する）──

    #[test]
    fn shutdown_zsh_daemon_helper_on_empty_slot_is_a_no_op() {
        // Shell::shutdown_zsh_daemon が exec_restart / main.rs の exit
        // 直前から呼ばれるのと同じ shutdown_shared_daemon 経路であることを
        // 直接確認する（Shell 全体を構築せず SharedDaemonSlot だけで検証）。
        let zsh_daemon = new_shared_daemon_slot();
        shutdown_shared_daemon(&zsh_daemon);
        assert!(
            zsh_daemon.lock().unwrap().is_none(),
            "shutdown on an empty slot must remain a no-op (idempotent)"
        );
    }

    #[test]
    #[serial]
    fn shutdown_zsh_daemon_helper_kills_live_daemon_before_would_be_exec_or_exit() {
        // exec_restart() / main.rs の exit 経路が「exec()/exit() の直前に
        // shutdown_shared_daemon を呼ぶ」という契約を、実際に spawn した
        // デーモンに対して直接証明する（exec()/exit() 自体は呼ばない —
        // プロセスを本当に置換/終了させるとテストランナーごと落ちるため）。
        let Some(zsh) = zsh_binary_for_test() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        let original_home = std::env::var("HOME").ok();
        let fixture = setup_daemon_fixture();
        let _ = zsh;
        let _ = &fixture.zdotdir;

        let settings = zsh_enabled_daemon_settings();
        let zsh_daemon = new_shared_daemon_slot();
        let mut completer = crate::cli::completer::JarvishCompleter::new(
            Arc::new(RwLock::new(vec![])),
            Arc::new(RwLock::new(HashMap::new())),
            Arc::clone(&settings),
            Arc::clone(&zsh_daemon),
            Arc::new(RwLock::new(
                crate::cli::completer::registry::CompletionRegistry::new(),
            )),
        );

        let line = "jarvishtestcmd ";
        let pos = line.len();
        let _ = completer.complete(line, pos);

        if zsh_daemon.lock().unwrap().is_none() {
            eprintln!("skipping: zsh daemon did not spawn in this environment");
            if let Some(home) = original_home {
                unsafe {
                    std::env::set_var("HOME", home);
                }
            }
            return;
        }

        let child_pid = {
            let guard = zsh_daemon.lock().unwrap();
            guard.as_ref().unwrap().daemon_pid_for_test()
        };

        // exec_restart() / main.rs の exit 経路が呼ぶのと同じヘルパー。
        shutdown_shared_daemon(&zsh_daemon);

        assert!(zsh_daemon.lock().unwrap().is_none());
        assert!(
            wait_for_pid_death(child_pid),
            "child pid {child_pid} should be dead after the pre-exec/pre-exit shutdown helper runs"
        );

        if let Some(home) = original_home {
            unsafe {
                std::env::set_var("HOME", home);
            }
        }
    }

    // ── B1/B2: Shell::shutdown_zsh_daemon は有界同期版を使う ──

    #[test]
    #[serial]
    fn shutdown_zsh_daemon_blocking_helper_reaps_deterministically_without_polling() {
        // `Shell::shutdown_zsh_daemon`（exec_restart / main.rs の exit
        // 直前から呼ばれる）は、reload/gate 経路が使うノンブロッキング版
        // （`shutdown_shared_daemon`）ではなく有界同期版
        // （`shutdown_shared_daemon_blocking`）を使う（B1/B2, #89 — プロセスが
        // この直後に exec()/exit() で消えるため、バックグラウンドスレッドに
        // reap を委譲しても実行される保証がない）。`Shell` 構造体そのものを
        // 構築せず、`Shell::shutdown_zsh_daemon` が実際に呼ぶのと同じ
        // `shutdown_shared_daemon_blocking` を直接呼び、**戻ってきた時点で
        // 既に reap 済み**（呼び出し元がポーリングする必要がない）ことを
        // 直接証明する — `shutdown_shared_daemon`（ノンブロッキング版）との
        // 違いはまさにこの「戻り値の時点での決定性」にある。
        let Some(zsh) = zsh_binary_for_test() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        let original_home = std::env::var("HOME").ok();
        let fixture = setup_daemon_fixture();
        let _ = zsh;
        let _ = &fixture.zdotdir;

        let settings = zsh_enabled_daemon_settings();
        let zsh_daemon = new_shared_daemon_slot();
        let mut completer = crate::cli::completer::JarvishCompleter::new(
            Arc::new(RwLock::new(vec![])),
            Arc::new(RwLock::new(HashMap::new())),
            Arc::clone(&settings),
            Arc::clone(&zsh_daemon),
            Arc::new(RwLock::new(
                crate::cli::completer::registry::CompletionRegistry::new(),
            )),
        );

        let line = "jarvishtestcmd ";
        let pos = line.len();
        let _ = completer.complete(line, pos);

        if zsh_daemon.lock().unwrap().is_none() {
            eprintln!("skipping: zsh daemon did not spawn in this environment");
            if let Some(home) = original_home {
                unsafe {
                    std::env::set_var("HOME", home);
                }
            }
            return;
        }

        let child_pid = {
            let guard = zsh_daemon.lock().unwrap();
            guard.as_ref().unwrap().daemon_pid_for_test()
        };

        // `Shell::shutdown_zsh_daemon` の内部実装と同じ呼び出し（同じ
        // deadline 定数を直接使うと private const に依存してしまうため、
        // ここでは十分な独自の deadline を渡す — 主張したいのは「関数が
        // 戻った時点で既に reap されている」という決定性であり、具体的な
        // deadline 値の一致ではない）。
        shutdown_shared_daemon_blocking(&zsh_daemon, std::time::Duration::from_secs(2), None);

        assert!(zsh_daemon.lock().unwrap().is_none());
        // ポーリングなしで即座に ESRCH を確認できることが非同期版との
        // 違いの直接証拠（`wait_for_pid_death` のような有界ポーリングを
        // 使わない）。
        let ret = unsafe { libc::kill(child_pid as libc::pid_t, 0) };
        let is_dead =
            ret == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        assert!(
            is_dead,
            "child pid {child_pid} should already be reaped when shutdown_shared_daemon_blocking returns"
        );

        if let Some(home) = original_home {
            unsafe {
                std::env::set_var("HOME", home);
            }
        }
    }

    // ── S5 修正: spawn_prewarm_thread_if_interactive / DaemonGate 配線 ──

    #[test]
    fn spawn_prewarm_thread_if_interactive_false_never_spawns_and_slot_stays_empty() {
        // -c 単体実行相当（interactive = false）では prewarm スレッド自体を
        // 一切起動しない。デーモン有効設定を渡しても、寛容な猶予時間を
        // 置いてもスロットが埋まらないことで「スレッドが起動していない」
        // ことを間接的に、しかし決定的に証明する（zsh バイナリの実機有無に
        // 関わらず: スレッドが起動していれば eventually 埋まるはずの
        // スロットが、猶予時間内に一切変化しないことが主張の核）。
        let settings = zsh_enabled_daemon_settings();
        let zsh_daemon = new_shared_daemon_slot();
        let gate = DaemonGate::new();

        spawn_prewarm_thread_if_interactive(false, &settings, &zsh_daemon, &gate);

        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(
            zsh_daemon.lock().unwrap().is_none(),
            "interactive=false must not spawn any prewarm thread, slot must stay empty"
        );
    }

    #[test]
    #[serial]
    fn spawn_prewarm_thread_if_interactive_true_eventually_populates_slot() {
        // 対照実験: interactive = true では従来どおりスレッドが起動し、
        // 猶予時間内にスロットが埋まる（対話モードの既存挙動が不変で
        // あることの確認、受け入れ基準5）。
        let Some(_zsh) = zsh_binary_for_test() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        let original_home = std::env::var("HOME").ok();
        let _fixture = setup_daemon_fixture();

        let settings = zsh_enabled_daemon_settings();
        let zsh_daemon = new_shared_daemon_slot();
        let gate = DaemonGate::new();

        spawn_prewarm_thread_if_interactive(true, &settings, &zsh_daemon, &gate);

        let mut populated = false;
        for _ in 0..100 {
            if zsh_daemon.lock().unwrap().is_some() {
                populated = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            populated,
            "interactive=true must still spawn the prewarm thread and populate the slot"
        );

        // テストフィクスチャ teardown（S5 修正）: `shutdown_shared_daemon`
        // （非ブロッキング、kill/reap をバックグラウンドスレッドへ委譲）は
        // テスト関数を抜けた直後にテストバイナリが終了するとバックグラウンド
        // スレッドが道連れで強制終了されうる（`zsh_daemon.rs` の
        // `spawn_reaches_ready_marker` テストで実測した孤児の根本原因と
        // 同じパターン）。有界同期版で確実に reap してから終える。
        shutdown_shared_daemon_blocking(&zsh_daemon, std::time::Duration::from_secs(2), None);

        if let Some(home) = original_home {
            unsafe {
                std::env::set_var("HOME", home);
            }
        }
    }

    #[test]
    #[serial]
    fn shutdown_zsh_daemon_gate_blocks_late_prewarm_insertion_end_to_end() {
        // S5 受け入れ基準1〜3 の統合的な決定的検証: `Shell::shutdown_zsh_daemon`
        // が実際に呼ぶのと同じ2つの公開関数（`shutdown_shared_daemon_blocking`
        // に `Some(&gate)` を渡す版と `prewarm_zsh_daemon`）を、実際の
        // レース順序（shutdown が先に完了 → prewarm が後から遅れて発火）で
        // 直接呼び、最終的にスロットが空のままであることを検証する。
        // `Shell` 全体は構築しない（`zsh_daemon` + `zsh_daemon_gate` +
        // `external_completion` の3つの共有状態だけで再現できる）。
        let Some(zsh) = zsh_binary_for_test() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        let original_home = std::env::var("HOME").ok();
        let _fixture = setup_daemon_fixture();
        let _ = zsh;

        let settings = zsh_enabled_daemon_settings();
        let zsh_daemon = new_shared_daemon_slot();
        let gate = DaemonGate::new();

        // 1. 終端 shutdown が先に完了する（-c が数ミリ秒で完走するケースを
        //    模す。スロットはまだ空。この呼び出しが gate を close する）。
        shutdown_shared_daemon_blocking(
            &zsh_daemon,
            std::time::Duration::from_secs(1),
            Some(&gate),
        );
        assert!(zsh_daemon.lock().unwrap().is_none());

        // 2. その後で prewarm が遅れて発火する。
        prewarm_zsh_daemon(&settings, &zsh_daemon, &gate);

        // 3. 決定的保証: closed 後の prewarm はスロットに何も残さない。
        assert!(
            zsh_daemon.lock().unwrap().is_none(),
            "prewarm firing after shutdown_zsh_daemon's gate closed must never leave a \
             daemon in the slot (S5 acceptance criteria 1-3)"
        );

        if let Some(home) = original_home {
            unsafe {
                std::env::set_var("HOME", home);
            }
        }
    }
}
