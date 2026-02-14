//! ターミナル状態管理
//!
//! ターミナル属性の保存・復元と、RAII ガードによる確実な復元を提供する。

use std::io;
use std::os::fd::AsFd;

use nix::sys::termios::{self, SetArg, Termios};

/// 現在のターミナル属性を保存する。
/// 非ターミナル環境（テスト等）では Err を返す。
fn save_terminal_state() -> io::Result<Termios> {
    termios::tcgetattr(io::stdin().as_fd()).map_err(|e| io::Error::other(e.to_string()))
}

/// 保存しておいたターミナル属性を復元する。
fn restore_terminal_state(saved: &Termios) {
    let _ = termios::tcsetattr(io::stdin().as_fd(), SetArg::TCSANOW, saved);
}

/// RAII ガード: スコープを抜けるときに自動的にターミナル状態を復元する。
/// エラー発生時や早期リターン、パニック時も確実に復元される。
pub(super) struct TerminalStateGuard {
    saved: Termios,
    active: bool,
}

impl TerminalStateGuard {
    /// ターミナル状態を保存してガードを作成する。
    pub(super) fn new() -> io::Result<Self> {
        let saved = save_terminal_state()?;
        Ok(Self {
            saved,
            active: false,
        })
    }

    /// raw mode を有効にする。有効化後、ガードがドロップされるまで復元される。
    pub(super) fn activate_raw_mode(&mut self) -> io::Result<()> {
        let mut raw = self.saved.clone();
        termios::cfmakeraw(&mut raw);
        termios::tcsetattr(io::stdin().as_fd(), SetArg::TCSANOW, &raw)
            .map_err(|e| io::Error::other(e.to_string()))?;
        self.active = true;
        Ok(())
    }
}

impl Drop for TerminalStateGuard {
    fn drop(&mut self) {
        if self.active {
            restore_terminal_state(&self.saved);
        }
    }
}
