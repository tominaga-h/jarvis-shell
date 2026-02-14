//! PTY ヘルパーと Alternate Screen 検出
//!
//! PTY ペアの作成、ターミナルサイズの取得、
//! Alternate Screen Buffer の検出を管理する。

use std::fs::File;
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::process::Stdio;

use nix::pty::openpty;
use nix::sys::termios::{self, OutputFlags, SetArg};
use tracing::debug;

// ── Alternate Screen 検出 ──

/// Alternate Screen Buffer 有効化シーケンス: ESC [ ? 1 0 4 9 h
const ALT_SCREEN_ENABLE: &[u8] = b"\x1b[?1049h";

/// バイトスライス内に Alternate Screen 有効化シーケンスが含まれているかチェックする。
pub(super) fn contains_alt_screen_seq(data: &[u8]) -> bool {
    data.windows(ALT_SCREEN_ENABLE.len())
        .any(|w| w == ALT_SCREEN_ENABLE)
}

// ── PTY ヘルパー ──

/// 現在のターミナルサイズを取得する。
/// 取得に失敗した場合はデフォルト値 (80x24) を返す。
pub(super) fn get_terminal_winsize() -> libc::winsize {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let ret = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) };
    if ret == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
        ws
    } else {
        libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }
    }
}

/// PTY slave の OPOST フラグを無効にし、出力時の `\n` → `\r\n` 変換を抑制する。
/// レガシーモード（tee キャプチャ）専用。
fn disable_opost(slave_fd: &OwnedFd) {
    let fd = slave_fd.as_fd();
    if let Ok(mut attrs) = termios::tcgetattr(fd) {
        attrs.output_flags.remove(OutputFlags::OPOST);
        let _ = termios::tcsetattr(fd, SetArg::TCSANOW, &attrs);
    }
}

/// セッション PTY ペアを作成し、(master File, slave OwnedFd) を返す。
/// フル PTY セッション用: OPOST は有効のまま（line discipline が \n→\r\n 変換を行う）。
pub(super) fn create_session_pty() -> io::Result<(File, OwnedFd)> {
    let ws = get_terminal_winsize();
    let pty = openpty(Some(&ws), None).map_err(|e| io::Error::other(e.to_string()))?;

    let master_file = File::from(pty.master);
    Ok((master_file, pty.slave))
}

/// レガシー PTY ペアを作成し、(master File, slave OwnedFd) を返す。
/// tee キャプチャ用: OPOST を無効化する。
fn create_legacy_pty() -> io::Result<(File, OwnedFd)> {
    let ws = get_terminal_winsize();
    let pty = openpty(Some(&ws), None).map_err(|e| io::Error::other(e.to_string()))?;

    disable_opost(&pty.slave);

    let master_file = File::from(pty.master);
    Ok((master_file, pty.slave))
}

/// stdout/stderr キャプチャ用の (reader, writer Stdio) ペアを作成する。
/// PTY を優先して使用し、子プロセスが `isatty()=true` と判定するようにする。
/// PTY 作成に失敗した場合は os_pipe にフォールバック。
pub(super) fn create_capture_pair() -> io::Result<(Box<dyn std::io::Read + Send>, Stdio)> {
    match create_legacy_pty() {
        Ok((master, slave)) => Ok((Box::new(master), slave.into())),
        Err(e) => {
            debug!("PTY creation failed, falling back to pipe: {e}");
            let (read, write) = os_pipe::pipe()?;
            Ok((Box::new(read), write.into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alt_screen_detection() {
        assert!(contains_alt_screen_seq(b"before\x1b[?1049hafter"));
        assert!(contains_alt_screen_seq(b"\x1b[?1049h"));
        assert!(!contains_alt_screen_seq(b"no alt screen here"));
        assert!(!contains_alt_screen_seq(b"\x1b[?1049")); // incomplete
    }
}
