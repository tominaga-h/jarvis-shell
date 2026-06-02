//! ジョブ制御（プロセスグループ + 端末フォアグラウンド管理）
//!
//! 外部コマンドを独立したプロセスグループに分離し、対話端末の
//! フォアグラウンドプロセスグループを子へ一時的に委譲することで、
//! 実行中コマンドへの Ctrl+C（SIGINT）が jarvish 本体ではなく
//! 子プロセスグループにのみ届くようにする。
//!
//! # 設計判断: SIGINT を SIG_IGN にしない
//!
//! tcsetpgrp で前面プロセスグループを子へ渡せば、端末ドライバが生成する
//! SIGINT は子プロセスグループにのみ配送される。jarvish 自身はバックグラウンド
//! プロセスグループとなり Ctrl+C を受け取らない。したがって起動時の
//! SIGINT 無視（SIG_IGN）は不要である。
//!
//! さらに、AI ストリーム中断（`src/ai/stream.rs`）は
//! `tokio::signal::unix::signal(SignalKind::interrupt())` で SIGINT を
//! 受信して動作するため、起動時に SIGINT を SIG_IGN にすると
//! AI ストリーム中断・プロンプト中の中断挙動を壊すおそれがある。
//! そのため本実装では SIGINT 無視は採用せず、ジョブ制御（tcsetpgrp）のみで
//! 「子コマンドだけが Ctrl+C で停止し jarvish は生存する」挙動を実現する。

use std::io::{self, IsTerminal};

use libc::pid_t;

/// jarvish 自身のプロセスグループ ID を取得する。
///
/// 取得に失敗した場合は `None` を返す。
pub(crate) fn shell_pgid() -> Option<pid_t> {
    // SAFETY: getpgrp は引数を取らず、呼び出しプロセスの pgid を返すだけで
    // メモリ安全性に影響しない。
    let pgid = unsafe { libc::getpgrp() };
    if pgid < 0 {
        None
    } else {
        Some(pgid)
    }
}

/// ジョブ制御を有効化すべきかを判定する純粋関数。
///
/// - `is_test_build` が `true`（cfg!(test)）の場合は常に `false`。
///   テスト並列実行で tcsetpgrp / 端末状態を触らないため。
/// - `is_tty` が `false`（非対話 / パイプ実行）の場合は `false`。
///   制御端末がない環境ではフォアグラウンド委譲が無意味かつ有害なため。
/// - 両者を満たす（対話端末かつ非テストビルド）ときのみ `true`。
pub(crate) fn should_enable_job_control(is_tty: bool, is_test_build: bool) -> bool {
    if is_test_build {
        return false;
    }
    if !is_tty {
        return false;
    }
    true
}

/// 実際にジョブ制御を有効化すべきかを判定する薄いラッパ。
///
/// stdin が端末かどうかを見て [`should_enable_job_control`] に委譲する。
/// `cfg!(test)` をテストビルド判定として渡す。
pub(crate) fn job_control_enabled() -> bool {
    should_enable_job_control(io::stdin().is_terminal(), cfg!(test))
}

/// パイプライン全段に割り当てるジョブのプロセスグループ ID を決定する。
///
/// 慣習に従い、パイプライン先頭プロセスの pid をジョブの pgid とし、
/// 後続の全プロセスを同じ pgid に join させる。
#[inline]
pub(crate) fn pipeline_pgid(first_pid: pid_t) -> pid_t {
    first_pid
}

/// `Command::pre_exec` クロージャ内で呼び出し、子プロセスをジョブ制御用に
/// セットアップする。
///
/// - `setpgid(0, pgid)` で子を指定プロセスグループへ移す。
///   `pgid == 0` の場合は子自身の pid を pgid とする新規グループを作る
///   （パイプライン先頭プロセス用）。後続段は先頭の pid を渡す。
///   setpgid はベストエフォートとし、失敗（EACCES/ESRCH 等のレース）しても
///   エラーを返さない。ジョブ制御のセットアップを諦めるだけで、コマンド
///   自体は通常どおり実行させるべきだからである。たとえばパイプライン先頭の
///   子が即終了して reap 済みの場合、後続段が先頭 pid の pgid へ join しよう
///   とすると失敗するが、それでコマンド実行ごと失敗させてはならない。
/// - jarvish 本体が（将来 SIG_IGN にした場合も含め）変更したジョブ制御系
///   シグナルの扱いを子へ継承させないため、SIGINT/SIGQUIT/SIGTSTP/
///   SIGTTIN/SIGTTOU を SIG_DFL に戻す。この復元は setpgid の成否に関わらず
///   常に行う（子がシグナルのデフォルト挙動を得るのは必須のため）。
///
/// # Safety
/// `pre_exec` の規約に従い、async-signal-safe な syscall のみを使用する。
/// メモリ確保やロック取得は行わない。
pub(crate) fn pre_exec_setpgid(pgid: pid_t) -> io::Result<()> {
    // SAFETY: setpgid / signal は async-signal-safe。fork 後 exec 前の
    // 子プロセス内でのみ呼ばれ、メモリ確保やロックを行わない。
    unsafe {
        // setpgid の失敗は握りつぶす（ベストエフォート）。失敗時はジョブ制御を
        // 諦めるだけで、エラーを返して Command::spawn ごと失敗させない。
        let _ = libc::setpgid(0, pgid);
        // jarvish が無視/変更したジョブ制御系シグナルを子に継承させない。
        // setpgid の成否に関わらず常に実行する。
        for sig in [
            libc::SIGINT,
            libc::SIGQUIT,
            libc::SIGTSTP,
            libc::SIGTTIN,
            libc::SIGTTOU,
        ] {
            libc::signal(sig, libc::SIG_DFL);
        }
    }
    Ok(())
}

/// 指定したプロセスグループに端末のフォアグラウンドを委譲する。
///
/// バックグラウンドプロセスグループからの `tcsetpgrp` は SIGTTOU を
/// 誘発するため、呼び出し中は SIGTTOU を一時的に SIG_IGN にする。
/// エラーは無視してフォールバックする（コマンド実行をブロックしない）。
pub(crate) fn give_terminal_to(pgid: pid_t) {
    with_sigttou_ignored(|| {
        // SAFETY: tcsetpgrp は STDIN_FILENO と pgid のみを取り、
        // メモリ安全性に影響しない。エラーは無視する。
        unsafe {
            libc::tcsetpgrp(libc::STDIN_FILENO, pgid);
        }
    });
}

/// 端末のフォアグラウンドを jarvish（`shell_pgid`）へ戻す。
///
/// `give_terminal_to` と同様に SIGTTOU を一時的に無視する。
pub(crate) fn reclaim_terminal(shell_pgid: pid_t) {
    with_sigttou_ignored(|| {
        // SAFETY: tcsetpgrp は STDIN_FILENO と pgid のみを取り、
        // メモリ安全性に影響しない。エラーは無視する。
        unsafe {
            libc::tcsetpgrp(libc::STDIN_FILENO, shell_pgid);
        }
    });
}

/// SIGTTOU を一時的に SIG_IGN にした状態でクロージャを実行し、
/// 終了後に元のハンドラを復元する。
fn with_sigttou_ignored<F: FnOnce()>(f: F) {
    // SAFETY: sigaction はシグナルハンドラの設定/復元のみを行い、
    // メモリ安全性に影響しない。SIG_IGN は POSIX 標準定数。
    unsafe {
        let mut old_action: libc::sigaction = std::mem::zeroed();
        let mut new_action: libc::sigaction = std::mem::zeroed();
        new_action.sa_sigaction = libc::SIG_IGN;
        libc::sigaction(libc::SIGTTOU, &new_action, &mut old_action);

        f();

        libc::sigaction(libc::SIGTTOU, &old_action, std::ptr::null_mut());
    }
}

/// 端末フォアグラウンドの委譲・回収を RAII で管理するガード。
///
/// 生成時に指定プロセスグループへフォアグラウンドを委譲し、
/// ドロップ時に jarvish（`shell_pgid`）へ確実に回収する。
/// 途中でエラーやパニックが発生しても端末がリークしない。
pub(crate) struct TerminalForegroundGuard {
    shell_pgid: pid_t,
}

impl TerminalForegroundGuard {
    /// `child_pgid` に端末フォアグラウンドを委譲してガードを生成する。
    ///
    /// ジョブ制御が無効（テストビルド / 非 tty）または shell_pgid が
    /// 取得できない場合は `None` を返し、端末は一切操作しない。
    pub(crate) fn new(child_pgid: pid_t) -> Option<Self> {
        if !job_control_enabled() {
            return None;
        }
        let shell_pgid = shell_pgid()?;
        give_terminal_to(child_pgid);
        Some(Self { shell_pgid })
    }
}

impl Drop for TerminalForegroundGuard {
    fn drop(&mut self) {
        reclaim_terminal(self.shell_pgid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── should_enable_job_control 真理値表 ──

    #[test]
    fn job_control_disabled_when_not_tty_and_not_test() {
        assert!(!should_enable_job_control(false, false));
    }

    #[test]
    fn job_control_enabled_when_tty_and_not_test() {
        assert!(should_enable_job_control(true, false));
    }

    #[test]
    fn job_control_disabled_when_not_tty_and_test() {
        assert!(!should_enable_job_control(false, true));
    }

    #[test]
    fn job_control_disabled_when_tty_and_test() {
        assert!(!should_enable_job_control(true, true));
    }

    // ── pipeline_pgid 純粋関数 ──

    #[test]
    fn pipeline_pgid_returns_first_pid() {
        assert_eq!(pipeline_pgid(12345), 12345);
        assert_eq!(pipeline_pgid(1), 1);
    }

    #[test]
    fn pipeline_pgid_is_stable_across_stages() {
        // 先頭プロセスの pid を一度決めたら全段で同一値になること。
        let first = pipeline_pgid(4242);
        let second_stage = pipeline_pgid(first);
        assert_eq!(first, second_stage);
    }

    // ── 実ビルドでの整合性 ──

    #[test]
    fn job_control_enabled_is_false_in_test_build() {
        // cfg!(test) が true のため、端末有無に関わらず常に無効。
        assert!(!job_control_enabled());
    }

    #[test]
    fn shell_pgid_returns_some_positive() {
        let pgid = shell_pgid().expect("getpgrp should succeed");
        assert!(pgid > 0);
    }
}
