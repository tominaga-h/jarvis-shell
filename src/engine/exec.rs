use std::io::{self, BufRead, BufReader, Write};
use std::process::Command;
use std::thread;

use super::CommandResult;
use crate::cli::jarvis::jarvis_talk;

/// 外部コマンドを実行し、stdout/stderr をリアルタイムで画面に表示しつつバッファにキャプチャする。
///
/// os_pipe を使用して子プロセスの出力をパイプ経由で取得し、
/// 別スレッドで「ターミナルに表示」+「バッファに蓄積」を同時に行う（tee パターン）。
pub fn run_external(cmd: &str, args: &[&str]) -> CommandResult {
    // stdout 用パイプ
    let (stdout_read, stdout_write) = match os_pipe::pipe() {
        Ok(pair) => pair,
        Err(e) => {
            let msg = format!("jarvish: pipe error: {e}\n");
            eprint!("{msg}");
            return CommandResult::error(msg, 1);
        }
    };

    // stderr 用パイプ
    let (stderr_read, stderr_write) = match os_pipe::pipe() {
        Ok(pair) => pair,
        Err(e) => {
            let msg = format!("jarvish: pipe error: {e}\n");
            eprint!("{msg}");
            return CommandResult::error(msg, 1);
        }
    };

    // 子プロセスを起動
    let mut child = match Command::new(cmd)
        .args(args)
        .stdout(stdout_write)
        .stderr(stderr_write)
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            let reason = match e.kind() {
                io::ErrorKind::NotFound => "command not found".to_string(),
                io::ErrorKind::PermissionDenied => "permission denied".to_string(),
                _ => format!("{e}"),
            };
            let msg = format!("{cmd}: {reason}. Something wrong, sir?");
            jarvis_talk(&msg);
            return CommandResult::error(msg, 127);
        }
    };

    // パイプの書き込み端は子プロセスに渡した後、親側では閉じる必要がある。
    // stdout_write, stderr_write は Command に move されているのでここでは drop 不要。

    // stdout tee スレッド
    let stdout_handle = thread::spawn(move || -> Vec<u8> {
        let mut buf = Vec::new();
        let reader = BufReader::new(stdout_read);
        let stdout = io::stdout();

        for line in reader.split(b'\n') {
            match line {
                Ok(mut bytes) => {
                    bytes.push(b'\n');
                    // ターミナルに表示
                    let mut out = stdout.lock();
                    let _ = out.write_all(&bytes);
                    let _ = out.flush();
                    // バッファに蓄積
                    buf.extend_from_slice(&bytes);
                }
                Err(_) => break,
            }
        }
        buf
    });

    // stderr tee スレッド
    let stderr_handle = thread::spawn(move || -> Vec<u8> {
        let mut buf = Vec::new();
        let reader = BufReader::new(stderr_read);
        let stderr = io::stderr();

        for line in reader.split(b'\n') {
            match line {
                Ok(mut bytes) => {
                    bytes.push(b'\n');
                    // ターミナルに表示
                    let mut err = stderr.lock();
                    let _ = err.write_all(&bytes);
                    let _ = err.flush();
                    // バッファに蓄積
                    buf.extend_from_slice(&bytes);
                }
                Err(_) => break,
            }
        }
        buf
    });

    // 子プロセスの終了を待機
    let exit_code = match child.wait() {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("jarvish: wait error: {e}");
            1
        }
    };

    // tee スレッドの完了を待ち、バッファを回収
    let stdout_bytes = stdout_handle.join().unwrap_or_default();
    let stderr_bytes = stderr_handle.join().unwrap_or_default();

    CommandResult {
        stdout: String::from_utf8_lossy(&stdout_bytes).to_string(),
        stderr: String::from_utf8_lossy(&stderr_bytes).to_string(),
        exit_code,
        action: super::LoopAction::Continue,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_stdout_capture() {
        let result = run_external("echo", &["hello"]);
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "hello");
    }

    #[test]
    fn exit_code_success() {
        let result = run_external("true", &[]);
        assert_eq!(result.exit_code, 0);
    }

    #[test]
    fn exit_code_failure() {
        let result = run_external("false", &[]);
        assert_eq!(result.exit_code, 1);
    }

    #[test]
    fn stderr_capture() {
        // sh -c を使って stderr に出力するコマンドを実行
        let result = run_external("sh", &["-c", "echo err >&2"]);
        assert_eq!(result.stderr.trim(), "err");
    }

    #[test]
    fn nonexistent_command_returns_error() {
        let result = run_external("__jarvish_nonexistent_command__", &[]);
        assert_ne!(result.exit_code, 0);
        assert!(!result.stderr.is_empty());
    }
}
