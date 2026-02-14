//! I/O 転送・キャプチャ
//!
//! stdin → PTY master への転送、PTY master からの出力キャプチャ、
//! tee パターンによるターミナル表示とバッファ蓄積を提供する。

use std::fs::File;
use std::io::{self, BufRead, Read, Write};
use std::os::fd::{AsRawFd, BorrowedFd, RawFd};

use nix::poll::{poll, PollFd, PollFlags, PollTimeout};

use super::pty::{contains_alt_screen_seq, get_terminal_winsize};

// ── stdin 転送 ──

/// 実 stdin → PTY master へのキーストローク転送。
/// poll ベースで停止パイプとウィンドウサイズ変更を監視する。
pub(super) fn forward_stdin(
    mut master_write: File,
    shutdown_read: os_pipe::PipeReader,
    pty_master_fd: RawFd,
) {
    let stdin_fd = io::stdin().as_raw_fd();
    let shutdown_fd = shutdown_read.as_raw_fd();
    let mut last_ws = get_terminal_winsize();
    let mut read_buf = [0u8; 4096];

    loop {
        let mut fds = [
            PollFd::new(
                unsafe { BorrowedFd::borrow_raw(stdin_fd) },
                PollFlags::POLLIN,
            ),
            PollFd::new(
                unsafe { BorrowedFd::borrow_raw(shutdown_fd) },
                PollFlags::POLLIN,
            ),
        ];

        // 100ms タイムアウト: ウィンドウサイズ変更を定期チェック
        match poll(&mut fds, PollTimeout::from(100u16)) {
            Ok(_) => {}
            Err(nix::errno::Errno::EINTR) => continue,
            Err(_) => break,
        }

        // 停止シグナルをチェック
        if let Some(revents) = fds[1].revents() {
            if revents.intersects(PollFlags::POLLIN | PollFlags::POLLHUP) {
                break;
            }
        }

        // ウィンドウサイズ変更を検出し、PTY に伝播 (SIGWINCH 相当)
        let current_ws = get_terminal_winsize();
        if current_ws.ws_row != last_ws.ws_row || current_ws.ws_col != last_ws.ws_col {
            unsafe {
                libc::ioctl(pty_master_fd, libc::TIOCSWINSZ, &current_ws);
            }
            last_ws = current_ws;
        }

        // stdin からキーストロークを読み取り、PTY master に転送
        if let Some(revents) = fds[0].revents() {
            if revents.contains(PollFlags::POLLIN) {
                let n = unsafe {
                    libc::read(
                        stdin_fd,
                        read_buf.as_mut_ptr() as *mut libc::c_void,
                        read_buf.len(),
                    )
                };
                if n <= 0 {
                    break;
                }
                let _ = master_write.write_all(&read_buf[..n as usize]);
            }
            // stdin 側が EOF/HUP した場合も終了
            if revents.contains(PollFlags::POLLHUP) {
                break;
            }
        }
    }
}

// ── 出力キャプチャ ──

/// PTY master から読み取った出力の結果。
#[derive(Default)]
pub(super) struct CaptureResult {
    pub bytes: Vec<u8>,
    pub used_alt_screen: bool,
}

/// PTY master から読み取った出力をターミナルに表示しつつキャプチャする。
/// Alternate Screen の使用を検出し、使用された場合はキャプチャを停止する。
pub(super) fn capture_pty_output(mut master: File) -> CaptureResult {
    let mut result = CaptureResult::default();
    let mut read_buf = [0u8; 4096];

    loop {
        match master.read(&mut read_buf) {
            Ok(0) => break,
            Ok(n) => {
                let chunk = &read_buf[..n];

                // Alternate Screen 検出
                if !result.used_alt_screen && contains_alt_screen_seq(chunk) {
                    result.used_alt_screen = true;
                }

                // ターミナルに表示 (常に行う)
                let mut out = io::stdout().lock();
                let _ = out.write_all(chunk);
                let _ = out.flush();

                // キャプチャバッファに蓄積 (alt screen 未使用時のみ)
                if !result.used_alt_screen {
                    result.bytes.extend_from_slice(chunk);
                }
            }
            Err(e) => {
                // EIO = PTY slave が閉じた (子プロセス終了)
                if e.raw_os_error() == Some(libc::EIO) {
                    break;
                }
                break;
            }
        }
    }

    result
}

// ── tee ヘルパー ──

/// 読み取りソースからデータを読み、ターミナルに表示しつつバッファに蓄積する（tee パターン）。
/// レガシーモードおよびパイプライン用。
pub(super) fn tee_to_terminal<R: Read>(read: R, is_stderr: bool) -> Vec<u8> {
    let mut buf = Vec::new();
    let reader = io::BufReader::new(read);

    for line in reader.split(b'\n') {
        match line {
            Ok(mut bytes) => {
                // バッファには \n のみ保存（キャプチャ用）
                bytes.push(b'\n');
                buf.extend_from_slice(&bytes);

                // ターミナル出力時は \r\n で行頭復帰させる
                // （OPOST 無効の PTY から読み取るため \n → \r\n 変換が行われない）
                if is_stderr {
                    let mut err = io::stderr().lock();
                    let _ = err.write_all(&bytes[..bytes.len() - 1]); // 内容（\n なし）
                    let _ = err.write_all(b"\r\n"); // \r\n で終端
                    let _ = err.flush();
                } else {
                    let mut out = io::stdout().lock();
                    let _ = out.write_all(&bytes[..bytes.len() - 1]); // 内容（\n なし）
                    let _ = out.write_all(b"\r\n"); // \r\n で終端
                    let _ = out.flush();
                }
            }
            Err(_) => break,
        }
    }
    buf
}

/// stderr パイプからデータを読み取り、ターミナルに表示しつつバッファに蓄積する。
/// PTY セッション用。
pub(super) fn tee_stderr(read: os_pipe::PipeReader) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut reader = io::BufReader::new(read);
    let mut read_buf = [0u8; 4096];

    loop {
        match reader.read(&mut read_buf) {
            Ok(0) => break,
            Ok(n) => {
                let chunk = &read_buf[..n];
                let mut err = io::stderr().lock();
                let _ = err.write_all(chunk);
                let _ = err.flush();
                buf.extend_from_slice(chunk);
            }
            Err(_) => break,
        }
    }
    buf
}
