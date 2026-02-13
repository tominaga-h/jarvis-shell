use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::OwnedFd;
use std::process::{Command, Stdio};
use std::thread;

use nix::pty::openpty;
use nix::sys::termios::{self, OutputFlags, SetArg};
use tracing::debug;

use super::parser::{Pipeline, Redirect, SimpleCommand};
use super::CommandResult;
use crate::cli::jarvis::jarvis_talk;

// ── PTY ヘルパー ──

/// 現在のターミナルサイズを取得する。
/// 取得に失敗した場合はデフォルト値 (80x24) を返す。
fn get_terminal_winsize() -> libc::winsize {
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
fn disable_opost(slave_fd: &OwnedFd) {
    use std::os::fd::AsFd;
    let fd = slave_fd.as_fd();
    if let Ok(mut attrs) = termios::tcgetattr(fd) {
        attrs.output_flags.remove(OutputFlags::OPOST);
        let _ = termios::tcsetattr(fd, SetArg::TCSANOW, &attrs);
    }
}

/// PTY ペアを作成し、(master File, slave OwnedFd) を返す。
/// ターミナルサイズを伝播し、OPOST を無効化する。
fn create_pty() -> io::Result<(File, OwnedFd)> {
    let ws = get_terminal_winsize();
    let pty = openpty(Some(&ws), None)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

    disable_opost(&pty.slave);

    let master_file = File::from(pty.master);
    Ok((master_file, pty.slave))
}

/// stdout/stderr キャプチャ用の (reader, writer Stdio) ペアを作成する。
/// PTY を優先して使用し、子プロセスが `isatty()=true` と判定するようにする。
/// PTY 作成に失敗した場合は os_pipe にフォールバック。
fn create_capture_pair() -> io::Result<(Box<dyn Read + Send>, Stdio)> {
    match create_pty() {
        Ok((master, slave)) => Ok((Box::new(master), slave.into())),
        Err(e) => {
            debug!("PTY creation failed, falling back to pipe: {e}");
            let (read, write) = os_pipe::pipe()?;
            Ok((Box::new(read), write.into()))
        }
    }
}

/// パイプラインを実行する。
///
/// - 単一コマンド: stdout/stderr を tee でキャプチャしつつターミナルに表示
/// - 複数コマンド: 前段の stdout を次段の stdin にパイプで接続し、
///   最終段の stdout/stderr のみ tee でキャプチャ
/// - リダイレクト: `>`, `>>`, `<` を処理
pub fn run_pipeline(pipeline: &Pipeline) -> CommandResult {
    let n = pipeline.commands.len();
    debug!(
        pipeline_length = n,
        "Running pipeline"
    );

    if n == 1 {
        // 単一コマンド: tee キャプチャ付きで実行
        return run_single_command(&pipeline.commands[0]);
    }

    // 複数コマンドのパイプライン
    run_piped_commands(&pipeline.commands)
}

/// 単一コマンドをリダイレクト付きで実行（tee キャプチャあり）。
/// PTY を使用して子プロセスの色出力を保持する。
fn run_single_command(simple: &SimpleCommand) -> CommandResult {
    let cmd = &simple.cmd;
    let args: Vec<&str> = simple.args.iter().map(|s| s.as_str()).collect();

    debug!(command = %cmd, args = ?args, "Spawning external command");

    // stdout キャプチャ: PTY (色出力保持) / pipe (フォールバック)
    let (stdout_reader, stdout_writer) = match create_capture_pair() {
        Ok(pair) => pair,
        Err(e) => {
            let msg = format!("jarvish: pipe error: {e}\n");
            eprint!("{msg}");
            return CommandResult::error(msg, 1);
        }
    };

    // stderr キャプチャ: PTY (色出力保持) / pipe (フォールバック)
    let (stderr_reader, stderr_writer) = match create_capture_pair() {
        Ok(pair) => pair,
        Err(e) => {
            let msg = format!("jarvish: pipe error: {e}\n");
            eprint!("{msg}");
            return CommandResult::error(msg, 1);
        }
    };

    // リダイレクト: stdin
    let final_stdin: Stdio = match find_stdin_redirect(&simple.redirects) {
        Ok(Some(file)) => file.into(),
        Ok(None) => Stdio::inherit(),
        Err(e) => return e,
    };

    // リダイレクト: stdout
    // stdout_writer は 1 箇所でのみ消費する必要がある
    let has_stdout_redirect = simple
        .redirects
        .iter()
        .any(|r| matches!(r, Redirect::StdoutOverwrite(_) | Redirect::StdoutAppend(_)));

    let final_stdout: Stdio = if has_stdout_redirect {
        // ファイルへリダイレクト。writer を閉じて reader が EOF を受け取れるようにする。
        let file = find_stdout_redirect(&simple.redirects).expect("redirect checked above");
        drop(stdout_writer);
        file.into()
    } else {
        // リダイレクトなし: PTY/pipe 経由で出力をキャプチャ
        stdout_writer
    };

    // 子プロセスを起動。spawn 後に command をドロップして
    // PTY slave / パイプ書き込み端を親プロセス側で閉じる必要がある。
    let mut child = {
        let mut command = Command::new(cmd);
        command
            .args(&args)
            .stdin(final_stdin)
            .stdout(final_stdout)
            .stderr(stderr_writer);

        match command.spawn() {
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
        }
    }; // command がここでドロップ → PTY slave / パイプ書き込み端が閉じる

    // stdout tee スレッド
    let stdout_handle = thread::spawn(move || tee_to_terminal(stdout_reader, false));

    // stderr tee スレッド
    let stderr_handle = thread::spawn(move || tee_to_terminal(stderr_reader, true));

    let exit_code = match child.wait() {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("jarvish: wait error: {e}");
            1
        }
    };

    let stdout_bytes = stdout_handle.join().unwrap_or_default();
    let stderr_bytes = stderr_handle.join().unwrap_or_default();

    debug!(
        command = %cmd,
        exit_code = exit_code,
        stdout_size = stdout_bytes.len(),
        stderr_size = stderr_bytes.len(),
        "External command completed"
    );

    CommandResult {
        stdout: String::from_utf8_lossy(&stdout_bytes).to_string(),
        stderr: String::from_utf8_lossy(&stderr_bytes).to_string(),
        exit_code,
        action: super::LoopAction::Continue,
    }
}

/// 複数コマンドをパイプで接続して実行する。
/// 最終段の stdout/stderr を tee でキャプチャする。
fn run_piped_commands(commands: &[SimpleCommand]) -> CommandResult {
    let n = commands.len();
    let mut children = Vec::new();
    // 前段の stdout 読み取り端を保持する
    let mut prev_stdout: Option<os_pipe::PipeReader> = None;

    for (i, simple) in commands.iter().enumerate() {
        let is_last = i == n - 1;
        let cmd = &simple.cmd;
        let args: Vec<&str> = simple.args.iter().map(|s| s.as_str()).collect();

        debug!(
            command = %cmd,
            args = ?args,
            stage = i + 1,
            total = n,
            "Pipeline stage"
        );

        // stdin: 最初のコマンドは inherit (またはリダイレクト)、それ以降は前段の stdout
        let stdin_cfg: Stdio = if let Some(prev) = prev_stdout.take() {
            prev.into()
        } else {
            match find_stdin_redirect(&simple.redirects) {
                Ok(Some(file)) => file.into(),
                Ok(None) => Stdio::inherit(),
                Err(e) => return e,
            }
        };

        if is_last {
            // 最終段: PTY + tee キャプチャ付きで実行（色出力保持）
            let (stdout_reader, stdout_writer) = match create_capture_pair() {
                Ok(pair) => pair,
                Err(e) => {
                    let msg = format!("jarvish: pipe error: {e}\n");
                    eprint!("{msg}");
                    return CommandResult::error(msg, 1);
                }
            };

            let (stderr_reader, stderr_writer) = match create_capture_pair() {
                Ok(pair) => pair,
                Err(e) => {
                    let msg = format!("jarvish: pipe error: {e}\n");
                    eprint!("{msg}");
                    return CommandResult::error(msg, 1);
                }
            };

            // 最終段の stdout リダイレクト
            let has_stdout_redirect = simple
                .redirects
                .iter()
                .any(|r| matches!(r, Redirect::StdoutOverwrite(_) | Redirect::StdoutAppend(_)));

            let final_stdout: Stdio = if has_stdout_redirect {
                let file =
                    find_stdout_redirect(&simple.redirects).expect("redirect checked above");
                drop(stdout_writer);
                file.into()
            } else {
                stdout_writer
            };

            let mut child = {
                let mut command = Command::new(cmd);
                command
                    .args(&args)
                    .stdin(stdin_cfg)
                    .stdout(final_stdout)
                    .stderr(stderr_writer);

                match command.spawn() {
                    Ok(child) => child,
                    Err(e) => {
                        for mut c in children {
                            let _ = kill_and_wait(&mut c);
                        }
                        return spawn_error(cmd, e);
                    }
                }
            }; // command がドロップ → PTY slave / パイプ書き込み端が閉じる

            // tee スレッド
            let stdout_handle = thread::spawn(move || tee_to_terminal(stdout_reader, false));
            let stderr_handle = thread::spawn(move || tee_to_terminal(stderr_reader, true));

            let exit_code = match child.wait() {
                Ok(status) => status.code().unwrap_or(1),
                Err(e) => {
                    eprintln!("jarvish: wait error: {e}");
                    1
                }
            };

            // 前段プロセスの完了を待つ
            for mut c in children {
                let _ = c.wait();
            }

            let stdout_bytes = stdout_handle.join().unwrap_or_default();
            let stderr_bytes = stderr_handle.join().unwrap_or_default();

            debug!(
                command = %cmd,
                exit_code = exit_code,
                stdout_size = stdout_bytes.len(),
                stderr_size = stderr_bytes.len(),
                "Pipeline final stage completed"
            );

            return CommandResult {
                stdout: String::from_utf8_lossy(&stdout_bytes).to_string(),
                stderr: String::from_utf8_lossy(&stderr_bytes).to_string(),
                exit_code,
                action: super::LoopAction::Continue,
            };
        }

        // 中間段: stdout をパイプで次段に渡す
        let (pipe_read, pipe_write) = match os_pipe::pipe() {
            Ok(pair) => pair,
            Err(e) => {
                let msg = format!("jarvish: pipe error: {e}\n");
                eprint!("{msg}");
                return CommandResult::error(msg, 1);
            }
        };

        // 中間段: command をブロックスコープで spawn して即ドロップ
        let child = {
            let mut command = Command::new(cmd);
            command
                .args(&args)
                .stdin(stdin_cfg)
                .stdout(pipe_write)
                .stderr(Stdio::inherit());

            match command.spawn() {
                Ok(child) => child,
                Err(e) => {
                    for mut c in children {
                        let _ = kill_and_wait(&mut c);
                    }
                    return spawn_error(cmd, e);
                }
            }
        }; // command がドロップ → pipe_write が閉じる
        children.push(child);
        prev_stdout = Some(pipe_read);
    }

    // ここには到達しないはず
    CommandResult::error("jarvish: internal error: empty pipeline".to_string(), 1)
}

/// 読み取りソースからデータを読み、ターミナルに表示しつつバッファに蓄積する（tee パターン）。
/// PTY master (`File`) と os_pipe (`PipeReader`) の両方を受け取れるようジェネリック。
fn tee_to_terminal<R: Read>(read: R, is_stderr: bool) -> Vec<u8> {
    let mut buf = Vec::new();
    let reader = BufReader::new(read);

    for line in reader.split(b'\n') {
        match line {
            Ok(mut bytes) => {
                bytes.push(b'\n');
                if is_stderr {
                    let mut err = io::stderr().lock();
                    let _ = err.write_all(&bytes);
                    let _ = err.flush();
                } else {
                    let mut out = io::stdout().lock();
                    let _ = out.write_all(&bytes);
                    let _ = out.flush();
                }
                buf.extend_from_slice(&bytes);
            }
            Err(_) => break,
        }
    }
    buf
}

/// リダイレクトリストから stdout リダイレクト先ファイルを開く。
/// 複数指定されている場合は最後のものが有効。
fn find_stdout_redirect(redirects: &[Redirect]) -> Option<File> {
    let mut result = None;
    for r in redirects {
        match r {
            Redirect::StdoutOverwrite(path) => {
                result = File::create(path).ok();
            }
            Redirect::StdoutAppend(path) => {
                result = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .ok();
            }
            _ => {}
        }
    }
    result
}

/// リダイレクトリストから stdin リダイレクト元ファイルを開く。
fn find_stdin_redirect(redirects: &[Redirect]) -> Result<Option<File>, CommandResult> {
    for r in redirects {
        if let Redirect::StdinFrom(path) = r {
            return match File::open(path) {
                Ok(f) => Ok(Some(f)),
                Err(e) => {
                    let msg = format!("jarvish: {path}: {e}\n");
                    eprint!("{msg}");
                    Err(CommandResult::error(msg, 1))
                }
            };
        }
    }
    Ok(None)
}

/// プロセス起動エラーを CommandResult として返すヘルパー。
fn spawn_error(cmd: &str, e: io::Error) -> CommandResult {
    let reason = match e.kind() {
        io::ErrorKind::NotFound => "command not found".to_string(),
        io::ErrorKind::PermissionDenied => "permission denied".to_string(),
        _ => format!("{e}"),
    };
    let msg = format!("{cmd}: {reason}. Something wrong, sir?");
    jarvis_talk(&msg);
    CommandResult::error(msg, 127)
}

/// 子プロセスを kill して wait するヘルパー。
fn kill_and_wait(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// 外部コマンドを実行し、stdout/stderr をリアルタイムで画面に表示しつつバッファにキャプチャする。
///
/// os_pipe を使用して子プロセスの出力をパイプ経由で取得し、
/// 別スレッドで「ターミナルに表示」+「バッファに蓄積」を同時に行う（tee パターン）。
///
/// NOTE: レガシー互換用。新しいコードは `run_pipeline()` を使用すること。
pub fn run_external(cmd: &str, args: &[&str]) -> CommandResult {
    debug!(command = %cmd, args = ?args, "Spawning external command (legacy)");

    let simple = SimpleCommand {
        cmd: cmd.to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
        redirects: vec![],
    };
    run_single_command(&simple)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── レガシー run_external テスト ──

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

    // ── run_pipeline テスト: パイプ ──

    #[test]
    fn pipeline_two_commands_piped() {
        // echo hello | cat → stdout に "hello" が出力される
        let pipeline = Pipeline {
            commands: vec![
                SimpleCommand {
                    cmd: "echo".into(),
                    args: vec!["hello".into()],
                    redirects: vec![],
                },
                SimpleCommand {
                    cmd: "cat".into(),
                    args: vec![],
                    redirects: vec![],
                },
            ],
        };
        let result = run_pipeline(&pipeline);
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "hello");
    }

    #[test]
    fn pipeline_three_commands_piped() {
        // echo -e "aaa\nbbb\nccc" | grep bbb | cat
        let pipeline = Pipeline {
            commands: vec![
                SimpleCommand {
                    cmd: "printf".into(),
                    args: vec!["aaa\\nbbb\\nccc\\n".into()],
                    redirects: vec![],
                },
                SimpleCommand {
                    cmd: "grep".into(),
                    args: vec!["bbb".into()],
                    redirects: vec![],
                },
                SimpleCommand {
                    cmd: "cat".into(),
                    args: vec![],
                    redirects: vec![],
                },
            ],
        };
        let result = run_pipeline(&pipeline);
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "bbb");
    }

    #[test]
    fn pipeline_exit_code_from_last_command() {
        // echo hello | false → exit code は 1（最終段のコード）
        let pipeline = Pipeline {
            commands: vec![
                SimpleCommand {
                    cmd: "echo".into(),
                    args: vec!["hello".into()],
                    redirects: vec![],
                },
                SimpleCommand {
                    cmd: "false".into(),
                    args: vec![],
                    redirects: vec![],
                },
            ],
        };
        let result = run_pipeline(&pipeline);
        assert_eq!(result.exit_code, 1);
    }

    // ── run_pipeline テスト: リダイレクト ──

    #[test]
    fn redirect_stdout_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.txt");
        let path_str = path.to_str().unwrap().to_string();

        let pipeline = Pipeline {
            commands: vec![SimpleCommand {
                cmd: "echo".into(),
                args: vec!["redirected".into()],
                redirects: vec![Redirect::StdoutOverwrite(path_str)],
            }],
        };
        let result = run_pipeline(&pipeline);
        assert_eq!(result.exit_code, 0);

        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents.trim(), "redirected");
    }

    #[test]
    fn redirect_stdout_append() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.txt");
        let path_str = path.to_str().unwrap().to_string();

        // まず上書きで書き込み
        std::fs::write(&path, "first\n").unwrap();

        // >> で追記
        let pipeline = Pipeline {
            commands: vec![SimpleCommand {
                cmd: "echo".into(),
                args: vec!["second".into()],
                redirects: vec![Redirect::StdoutAppend(path_str)],
            }],
        };
        let result = run_pipeline(&pipeline);
        assert_eq!(result.exit_code, 0);

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("first"));
        assert!(contents.contains("second"));
    }

    #[test]
    fn redirect_stdin_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input.txt");
        let path_str = path.to_str().unwrap().to_string();

        std::fs::write(&path, "from_file\n").unwrap();

        let pipeline = Pipeline {
            commands: vec![SimpleCommand {
                cmd: "cat".into(),
                args: vec![],
                redirects: vec![Redirect::StdinFrom(path_str)],
            }],
        };
        let result = run_pipeline(&pipeline);
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "from_file");
    }

    #[test]
    fn redirect_stdin_nonexistent_file_returns_error() {
        let pipeline = Pipeline {
            commands: vec![SimpleCommand {
                cmd: "cat".into(),
                args: vec![],
                redirects: vec![Redirect::StdinFrom(
                    "/tmp/__jarvish_nonexistent_input__".into(),
                )],
            }],
        };
        let result = run_pipeline(&pipeline);
        assert_ne!(result.exit_code, 0);
    }
}
