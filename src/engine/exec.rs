use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::thread;

use nix::poll::{poll, PollFd, PollFlags, PollTimeout};
use nix::pty::openpty;
use nix::sys::termios::{self, OutputFlags, SetArg, Termios};
use tracing::debug;

use super::parser::{Pipeline, Redirect, SimpleCommand};
use super::CommandResult;
use crate::cli::jarvis::jarvis_talk;

// ── Alternate Screen 検出 ──

/// Alternate Screen Buffer 有効化シーケンス: ESC [ ? 1 0 4 9 h
const ALT_SCREEN_ENABLE: &[u8] = b"\x1b[?1049h";

/// バイトスライス内に Alternate Screen 有効化シーケンスが含まれているかチェックする。
fn contains_alt_screen_seq(data: &[u8]) -> bool {
    data.windows(ALT_SCREEN_ENABLE.len())
        .any(|w| w == ALT_SCREEN_ENABLE)
}

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
fn create_session_pty() -> io::Result<(File, OwnedFd)> {
    let ws = get_terminal_winsize();
    let pty = openpty(Some(&ws), None)
        .map_err(|e| io::Error::other(e.to_string()))?;

    let master_file = File::from(pty.master);
    Ok((master_file, pty.slave))
}

/// レガシー PTY ペアを作成し、(master File, slave OwnedFd) を返す。
/// tee キャプチャ用: OPOST を無効化する。
fn create_legacy_pty() -> io::Result<(File, OwnedFd)> {
    let ws = get_terminal_winsize();
    let pty = openpty(Some(&ws), None)
        .map_err(|e| io::Error::other(e.to_string()))?;

    disable_opost(&pty.slave);

    let master_file = File::from(pty.master);
    Ok((master_file, pty.slave))
}

/// stdout/stderr キャプチャ用の (reader, writer Stdio) ペアを作成する。
/// PTY を優先して使用し、子プロセスが `isatty()=true` と判定するようにする。
/// PTY 作成に失敗した場合は os_pipe にフォールバック。
fn create_capture_pair() -> io::Result<(Box<dyn Read + Send>, Stdio)> {
    match create_legacy_pty() {
        Ok((master, slave)) => Ok((Box::new(master), slave.into())),
        Err(e) => {
            debug!("PTY creation failed, falling back to pipe: {e}");
            let (read, write) = os_pipe::pipe()?;
            Ok((Box::new(read), write.into()))
        }
    }
}

// ── ターミナル状態管理 ──

/// 現在のターミナル属性を保存する。
/// 非ターミナル環境（テスト等）では Err を返す。
fn save_terminal_state() -> io::Result<Termios> {
    termios::tcgetattr(io::stdin().as_fd())
        .map_err(|e| io::Error::other(e.to_string()))
}

/// 保存しておいたターミナル属性を復元する。
fn restore_terminal_state(saved: &Termios) {
    let _ = termios::tcsetattr(io::stdin().as_fd(), SetArg::TCSANOW, saved);
}

/// RAII ガード: スコープを抜けるときに自動的にターミナル状態を復元する。
/// エラー発生時や早期リターン、パニック時も確実に復元される。
struct TerminalStateGuard {
    saved: Termios,
    active: bool,
}

impl TerminalStateGuard {
    /// ターミナル状態を保存してガードを作成する。
    fn new() -> io::Result<Self> {
        let saved = save_terminal_state()?;
        Ok(Self { saved, active: false })
    }

    /// raw mode を有効にする。有効化後、ガードがドロップされるまで復元される。
    fn activate_raw_mode(&mut self) -> io::Result<()> {
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

// ── stdin 転送 ──

/// 実 stdin → PTY master へのキーストローク転送。
/// poll ベースで停止パイプとウィンドウサイズ変更を監視する。
fn forward_stdin(mut master_write: File, shutdown_read: os_pipe::PipeReader, pty_master_fd: RawFd) {
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
struct CaptureResult {
    bytes: Vec<u8>,
    used_alt_screen: bool,
}

/// PTY master から読み取った出力をターミナルに表示しつつキャプチャする。
/// Alternate Screen の使用を検出し、使用された場合はキャプチャを停止する。
fn capture_pty_output(mut master: File) -> CaptureResult {
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

// ── パイプライン実行 ──

/// パイプラインを実行する。
///
/// - 単一コマンド: フル PTY セッションで実行（vim/less/bat 等の対話コマンド対応）
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
        return run_single_command(&pipeline.commands[0]);
    }

    // 複数コマンドのパイプライン
    run_piped_commands(&pipeline.commands)
}

/// 単一コマンドの実行エントリポイント。
/// リダイレクトがある場合はレガシー（pipe + tee）方式にフォールバック。
/// リダイレクトがない場合はフル PTY セッションで実行する。
fn run_single_command(simple: &SimpleCommand) -> CommandResult {
    let has_redirect = !simple.redirects.is_empty();

    if has_redirect {
        return run_single_command_legacy(simple);
    }

    // フル PTY セッションを試行。ターミナル取得に失敗した場合はレガシーにフォールバック。
    match run_single_command_pty_session(simple) {
        Ok(result) => result,
        Err(e) => {
            debug!("PTY session failed ({e}), falling back to legacy mode");
            run_single_command_legacy(simple)
        }
    }
}

/// フル PTY セッション方式で単一コマンドを実行する。
/// 子プロセスをセッションリーダーとして起動し、PTY を制御端末として割り当てる。
/// stdin は PTY 経由で転送し、stdout は PTY 経由でキャプチャする。
fn run_single_command_pty_session(simple: &SimpleCommand) -> io::Result<CommandResult> {
    // テストビルドでは PTY セッションモードを使用しない。
    // PTY セッションは親ターミナルを raw mode（OPOST 無効）に変更するため、
    // 複数テストが並列実行されるとターミナル状態のレースコンディションが発生し、
    // 出力が斜めになる（\n → \r\n 変換が失われる）問題を引き起こす。
    if cfg!(test) || !io::stdout().is_terminal() {
        return Err(io::Error::other("PTY session not available"));
    }

    let cmd = &simple.cmd;
    let args: Vec<&str> = simple.args.iter().map(|s| s.as_str()).collect();

    debug!(command = %cmd, args = ?args, "Spawning external command (PTY session)");

    // 1. セッション PTY ペアを作成 (stdin + stdout 共用)
    let (master, slave) = create_session_pty()?;
    let master_raw_fd = master.as_raw_fd();

    // 2. stderr 用パイプを作成
    let (stderr_read, stderr_write) = os_pipe::pipe()?;

    // 3. ターミナル状態ガードを作成（RAII で確実に復元）
    let mut terminal_guard = TerminalStateGuard::new()?;

    // 4. PTY slave fd を複製して stdin / stdout に割り当てる
    let slave_raw_fd = slave.as_raw_fd();
    let stdin_fd = unsafe { libc::dup(slave_raw_fd) };
    let stdout_fd = unsafe { libc::dup(slave_raw_fd) };
    if stdin_fd < 0 || stdout_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // slave は spawn 前にドロップ不要: dup 済みの fd は独立
    // spawn 後に slave をドロップする（親側のコピーを閉じる）

    // 5. 子プロセスを起動
    let mut child = {
        let mut command = Command::new(cmd);
        command
            .args(&args)
            .stdin(unsafe { Stdio::from_raw_fd(stdin_fd) })
            .stdout(unsafe { Stdio::from_raw_fd(stdout_fd) })
            .stderr(Stdio::from(stderr_write));

        // 新しいセッションを作成し、PTY を制御端末に設定
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                // fd 0 (stdin) は PTY slave → 制御端末として設定
                if libc::ioctl(0, libc::TIOCSCTTY as libc::c_ulong, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }

        match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                // spawn 失敗: dup した fd の残りをクリーンアップ
                // (Command が stdin_fd, stdout_fd を所有しているので通常は不要だが安全のため)
                return Err(e);
            }
        }
    }; // command ドロップ → stdin_fd, stdout_fd, stderr_write が閉じる

    // 6. 親側の PTY slave fd を閉じる
    drop(slave);

    // 7. 親ターミナルを raw mode に設定（ガードが自動復元を保証）
    if let Err(e) = terminal_guard.activate_raw_mode() {
        debug!("Failed to set raw mode: {e}");
        // raw mode 設定に失敗しても続行（非対話コマンドは動作する）
    }

    // 8. stdin 転送スレッドを起動 (停止パイプ付き)
    let (shutdown_read, shutdown_write) = os_pipe::pipe()?;
    let master_for_stdin = master.try_clone()?;
    let stdin_handle = thread::spawn(move || {
        forward_stdin(master_for_stdin, shutdown_read, master_raw_fd);
    });

    // 9. 出力キャプチャスレッドを起動 (Alternate Screen 検出付き)
    let output_handle = thread::spawn(move || capture_pty_output(master));

    // 10. stderr tee スレッドを起動
    let stderr_handle = thread::spawn(move || tee_stderr(stderr_read));

    // 11. 子プロセスの終了を待機
    let exit_code = match child.wait() {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("jarvish: wait error: {e}");
            1
        }
    };

    // 12. stdin 転送スレッドを停止
    drop(shutdown_write);

    // 13. ターミナル状態は terminal_guard の Drop で自動復元される

    // 14. スレッドを join
    let _ = stdin_handle.join();
    let capture = output_handle.join().unwrap_or_default();
    let stderr_bytes = stderr_handle.join().unwrap_or_default();

    debug!(
        command = %cmd,
        exit_code = exit_code,
        stdout_size = capture.bytes.len(),
        stderr_size = stderr_bytes.len(),
        used_alt_screen = capture.used_alt_screen,
        "External command completed (PTY session)"
    );

    Ok(CommandResult {
        stdout: String::from_utf8_lossy(&capture.bytes).to_string(),
        stderr: String::from_utf8_lossy(&stderr_bytes).to_string(),
        exit_code,
        action: super::LoopAction::Continue,
        used_alt_screen: capture.used_alt_screen,
    })
}

/// レガシー方式で単一コマンドを実行する（リダイレクト対応、PTY セッションのフォールバック）。
/// 旧来の PTY + tee キャプチャ方式。stdin は inherit。
fn run_single_command_legacy(simple: &SimpleCommand) -> CommandResult {
    let cmd = &simple.cmd;
    let args: Vec<&str> = simple.args.iter().map(|s| s.as_str()).collect();

    debug!(command = %cmd, args = ?args, "Spawning external command (legacy mode)");

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
    let has_stdout_redirect = simple
        .redirects
        .iter()
        .any(|r| matches!(r, Redirect::StdoutOverwrite(_) | Redirect::StdoutAppend(_)));

    let final_stdout: Stdio = if has_stdout_redirect {
        let file = find_stdout_redirect(&simple.redirects).expect("redirect checked above");
        drop(stdout_writer);
        file.into()
    } else {
        stdout_writer
    };

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
    };

    let stdout_handle = thread::spawn(move || tee_to_terminal(stdout_reader, false));
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
        "External command completed (legacy)"
    );

    CommandResult {
        stdout: String::from_utf8_lossy(&stdout_bytes).to_string(),
        stderr: String::from_utf8_lossy(&stderr_bytes).to_string(),
        exit_code,
        action: super::LoopAction::Continue,
        used_alt_screen: false,
    }
}

/// 複数コマンドをパイプで接続して実行する。
/// 全ステージの stdout/stderr を tee でキャプチャする。
fn run_piped_commands(commands: &[SimpleCommand]) -> CommandResult {
    let n = commands.len();
    let mut children = Vec::new();
    let mut prev_stdout: Option<os_pipe::PipeReader> = None;

    // 中間ステージの stderr を共有パイプでキャプチャする。
    // Option でラップし、is_last ブロックで take() → drop して EOF を伝播させる。
    let (mid_stderr_reader, mid_stderr_writer) = match os_pipe::pipe() {
        Ok(pair) => pair,
        Err(e) => {
            let msg = format!("jarvish: pipe error: {e}\n");
            eprint!("{msg}");
            return CommandResult::error(msg, 1);
        }
    };
    let mut mid_stderr_writer = Some(mid_stderr_writer);

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
            // 全中間ステージのクローン作成後、オリジナルを drop して EOF を伝播
            mid_stderr_writer.take();

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
                            kill_and_wait(&mut c);
                        }
                        return spawn_error(cmd, e);
                    }
                }
            };

            let stdout_handle = thread::spawn(move || tee_to_terminal(stdout_reader, false));
            let stderr_handle = thread::spawn(move || tee_to_terminal(stderr_reader, true));
            let mid_stderr_handle =
                thread::spawn(move || tee_to_terminal(mid_stderr_reader, true));

            let exit_code = match child.wait() {
                Ok(status) => status.code().unwrap_or(1),
                Err(e) => {
                    eprintln!("jarvish: wait error: {e}");
                    1
                }
            };

            for mut c in children {
                let _ = c.wait();
            }

            let stdout_bytes = stdout_handle.join().unwrap_or_default();
            let stderr_bytes = stderr_handle.join().unwrap_or_default();
            let mid_stderr_bytes = mid_stderr_handle.join().unwrap_or_default();

            // 中間ステージ + 最終ステージの stderr を結合
            let mut combined_stderr = mid_stderr_bytes;
            combined_stderr.extend_from_slice(&stderr_bytes);

            debug!(
                command = %cmd,
                exit_code = exit_code,
                stdout_size = stdout_bytes.len(),
                stderr_size = combined_stderr.len(),
                "Pipeline final stage completed"
            );

            return CommandResult {
                stdout: String::from_utf8_lossy(&stdout_bytes).to_string(),
                stderr: String::from_utf8_lossy(&combined_stderr).to_string(),
                exit_code,
                action: super::LoopAction::Continue,
                used_alt_screen: false,
            };
        }

        // 中間段
        let (pipe_read, pipe_write) = match os_pipe::pipe() {
            Ok(pair) => pair,
            Err(e) => {
                let msg = format!("jarvish: pipe error: {e}\n");
                eprint!("{msg}");
                return CommandResult::error(msg, 1);
            }
        };

        // 中間ステージの stderr を共有パイプに流してキャプチャする
        let mid_stderr: Stdio = mid_stderr_writer
            .as_ref()
            .and_then(|w| w.try_clone().ok())
            .map(|w| -> Stdio { w.into() })
            .unwrap_or_else(Stdio::inherit);

        let child = {
            let mut command = Command::new(cmd);
            command
                .args(&args)
                .stdin(stdin_cfg)
                .stdout(pipe_write)
                .stderr(mid_stderr);

            match command.spawn() {
                Ok(child) => child,
                Err(e) => {
                    for mut c in children {
                        kill_and_wait(&mut c);
                    }
                    return spawn_error(cmd, e);
                }
            }
        };
        children.push(child);
        prev_stdout = Some(pipe_read);
    }

    CommandResult::error("jarvish: internal error: empty pipeline".to_string(), 1)
}

// ── tee ヘルパー ──

/// 読み取りソースからデータを読み、ターミナルに表示しつつバッファに蓄積する（tee パターン）。
/// レガシーモードおよびパイプライン用。
fn tee_to_terminal<R: Read>(read: R, is_stderr: bool) -> Vec<u8> {
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
                    let _ = err.write_all(b"\r\n");                   // \r\n で終端
                    let _ = err.flush();
                } else {
                    let mut out = io::stdout().lock();
                    let _ = out.write_all(&bytes[..bytes.len() - 1]); // 内容（\n なし）
                    let _ = out.write_all(b"\r\n");                   // \r\n で終端
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
fn tee_stderr(read: os_pipe::PipeReader) -> Vec<u8> {
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

// ── リダイレクト ヘルパー ──

/// リダイレクトリストから stdout リダイレクト先ファイルを開く。
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

// ── エラーヘルパー ──

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

#[cfg(test)]
mod tests {
    use super::*;

    /// ヘルパー: 単一コマンドの SimpleCommand を生成する
    fn simple(cmd: &str, args: &[&str]) -> SimpleCommand {
        SimpleCommand {
            cmd: cmd.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            redirects: vec![],
        }
    }

    // ── run_single_command テスト ──

    #[test]
    fn echo_stdout_capture() {
        let result = run_single_command(&simple("echo", &["hello"]));
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "hello");
    }

    #[test]
    fn exit_code_success() {
        let result = run_single_command(&simple("true", &[]));
        assert_eq!(result.exit_code, 0);
    }

    #[test]
    fn exit_code_failure() {
        let result = run_single_command(&simple("false", &[]));
        assert_eq!(result.exit_code, 1);
    }

    #[test]
    fn stderr_capture() {
        let result = run_single_command(&simple("sh", &["-c", "echo err >&2"]));
        assert_eq!(result.stderr.trim(), "err");
    }

    #[test]
    fn nonexistent_command_returns_error() {
        let result = run_single_command(&simple("__jarvish_nonexistent_command__", &[]));
        assert_ne!(result.exit_code, 0);
        assert!(!result.stderr.is_empty());
    }

    #[test]
    fn alt_screen_detection() {
        assert!(contains_alt_screen_seq(b"before\x1b[?1049hafter"));
        assert!(contains_alt_screen_seq(b"\x1b[?1049h"));
        assert!(!contains_alt_screen_seq(b"no alt screen here"));
        assert!(!contains_alt_screen_seq(b"\x1b[?1049")); // incomplete
    }

    // ── run_pipeline テスト: パイプ ──

    #[test]
    fn pipeline_two_commands_piped() {
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

        std::fs::write(&path, "first\n").unwrap();

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
