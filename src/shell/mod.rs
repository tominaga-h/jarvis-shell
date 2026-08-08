//! Shell モジュール — REPL ループとシェル状態管理
//!
//! `Shell` 構造体にすべてのシェル状態を集約し、
//! 入力ハンドリング、AI ルーティング、エラー調査の各責務をサブモジュールに分離する。
//!
//! ## サブモジュール構成
//!
//! - [`ai_router`] — AI コマンドのルーティング
//! - [`construction`] — `Shell::new` と関連ヘルパー
//! - [`editor`] — reedline エディタの構築
//! - [`input`] — 入力ハンドリング（handle_input）
//! - [`investigate`] — 失敗時のエラー調査
//! - [`prewarm`] — 起動時の zsh 補完デーモン prewarm
//! - [`rc`] — rc.jsh / `source` ビルトイン
//! - [`reload`] — `source` からの設定再読み込み
//! - [`restart`] — 再起動系メソッド・SIGUSR1 ハンドラ
//! - [`run`] — REPL ループ本体 (`run` / `run_command`)

mod ai_router;
mod construction;
mod editor;
mod input;
mod investigate;
mod prewarm;
mod rc;
mod reload;
mod restart;
mod run;

pub use rc::RcOptions;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64};
use std::sync::{Arc, RwLock};

use reedline::Reedline;

use crate::ai::{ConversationState, JarvisAI};
use crate::cli::completer::{
    registry::CompletionRegistry, DaemonGate, ExternalCompletionSettings, SharedDaemonSlot,
};
use crate::cli::prompt::ShellPrompt;
use crate::engine::classifier::InputClassifier;
use crate::storage::BlackBox;

/// Jarvis Shell の状態を管理する構造体。
/// エディタ、AI クライアント、履歴ストレージ、会話状態を保持する。
///
/// `impl Shell` ブロックは機能別に以下のサブモジュールへ分割している
/// （各 impl ブロックが互いに独立に `impl Shell {}` を追加する形）：
///
/// - 構築・初期化: [`construction`]
/// - 設定再読み込み: [`reload`]
/// - 再起動・SIGUSR1: [`restart`]
/// - REPL 実行: [`run`]
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
    /// 終端 shutdown（exit/exec 経路）の tombstone ゲート。
    /// `Shell::new` が起動時バックグラウンドスレッドの prewarm と共有する。
    /// `shutdown_zsh_daemon`（exit/exec 直前の有界同期 shutdown）が一度
    /// closed にすると、以後 prewarm が遅延してスロットに書き込もうとしても
    /// 挿入前に必ず kill される（`zsh_bridge::DaemonGate` のドキュメント
    /// 参照 — `-c` 単体実行や `rc.jsh` 内 `exit` での孤児デーモン化を防ぐ）。
    zsh_daemon_gate: Arc<DaemonGate>,
    /// prewarm スレッドの完了通知チャネル受信側。
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
    /// `--rcfile` / `--no-rc` CLI オプション。rc.jsh の
    /// 読み込みを `run()` / `run_command()` の両方から解決するために保持する。
    rc_options: RcOptions,
    /// 現在実行中の rc/source スクリプトのネスト深さ。
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
    ///    アップをスキップする。
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

#[cfg(test)]
mod tests;
