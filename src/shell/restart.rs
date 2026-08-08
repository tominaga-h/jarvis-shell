//! 再起動系のメソッド・static・ヘルパーを切り出したサブモジュール。
//!
//! - `RESTART_FLAG` グローバルフラグ（SIGUSR1 ハンドラ用）
//! - `Shell::register_sigusr1_handler` / `Shell::shutdown_zsh_daemon` /
//!   `Shell::restart_requested` / `Shell::exec_restart`
//! - exec_restart 用の free 関数 `build_restart_command`
//!
//! 振る舞いは元の `src/shell/mod.rs` から一切変更していない。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use std::sync::atomic::AtomicBool as StaticAtomicBool;
use tracing::{info, warn};

use crate::cli::completer::shutdown_shared_daemon_blocking;

use super::Shell;

/// SIGUSR1 シグナルハンドラが設定するグローバルフラグ。
/// シグナルハンドラ内では async-signal-safe な操作のみ許可されるため、
/// `AtomicBool::store` を使用する。
pub(super) static RESTART_FLAG: StaticAtomicBool = StaticAtomicBool::new(false);

impl Shell {
    /// exec/exit 直前の有界同期 shutdown 予算。
    ///
    /// プロセスがこの直後に exec() で置換される、または exit() で終了する
    /// ため、バックグラウンドスレッドへ kill/reap を委譲しても実行される
    /// 保証がない（[`shutdown_shared_daemon_blocking`] のドキュメント参照）。
    /// `ZshDaemon` 単体の reap 予算（既定 1 秒、`zsh_daemon.rs` 参照）に
    /// 軽い余裕を足した値。
    pub(super) const ZSH_DAEMON_EXIT_SHUTDOWN_DEADLINE: std::time::Duration =
        std::time::Duration::from_millis(1200);

    /// [`Self::shutdown_zsh_daemon`] が prewarm スレッドの完了通知を待つ
    /// 上限。prewarm の spawn 自体の上限
    /// （`zsh_bridge::MIN_TIMEOUT_MS` = 2000ms）に軽い余裕を足した値 ──
    /// この値より短いと「prewarm がまだ正常に spawn 中なだけ」のケースで
    /// 待ちきれず、tombstone チェック未実行のまま強制終了されるレースが
    /// 再発する。
    pub(super) const PREWARM_JOIN_DEADLINE: std::time::Duration =
        std::time::Duration::from_millis(2500);

    /// SIGUSR1 シグナルハンドラを登録する。
    ///
    /// 受信時に `RESTART_FLAG` グローバルフラグを立てる。
    /// reedline の `read_line()` は同期ブロッキング呼び出しのため、
    /// シグナルハンドラでフラグを立て、次の REPL ループイテレーションでチェックする。
    pub(super) fn register_sigusr1_handler(restart_flag: std::sync::Arc<AtomicBool>) {
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

    /// 温存 zsh 補完デーモンが稼働中なら shutdown する（kill + 有界同期 reap）。
    ///
    /// `Command::exec`（[`exec_restart`](Self::exec_restart)）はプロセス
    /// イメージを置換するため `Drop` は一切実行されず、`std::process::exit`
    /// もデストラクタをスキップする。そのためこれらの経路の**直前**に
    /// 明示的に呼び、デーモン子プロセス・PTY fd・init 一時ファイルの
    /// リークを防ぐ。
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

    /// `restart` ビルトイン（または rc/source スクリプト内の `restart` 行、
    /// SIGUSR1）によって再起動が要求されたかどうかを返す。
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

/// exec_restart 用のコマンド情報を構築する。
///
/// 現在のバイナリパスと引数を取得する。テスト可能な純粋関数として分離。
pub(super) fn build_restart_command() -> Result<(PathBuf, Vec<String>), std::io::Error> {
    let exe = std::env::current_exe().map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("failed to get current exe path: {e}"),
        )
    })?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    Ok((exe, args))
}
