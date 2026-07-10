//! 温存 zsh 補完デーモン — `zsh -i` を常駐させ Tab ごとの起動コストを消す
//! （Task 2b.3, #89）
//!
//! [`super::zsh_bridge::ZshBridgeProvider`]（ワンショット版）は Tab 押下
//! ごとに `zsh --no-rcs -c <capture.zsh>` を新規 spawn する。実機計測では
//! これが 700〜1100ms かかる（zpty/PTY セットアップ + ポーリングが支配的で、
//! 補完の計算自体は数十ms）。このモジュールは同じプロトコル
//! （`compadd` オーバーライド + NUL センチネルで候補行を区切る）を、
//! **jarvish が直接 PTY 経由で spawn し常駐させる `zsh -i` 1本**に対して
//! 使い回すことで、2回目以降のリクエストを「計算のみ」（Tab ごとの
//! 再起動なし）にする。
//!
//! アーキテクチャ上の決定（固定）: デーモンは launchctl/launchd や
//! システムサービスを一切使わない、jarvish の**素の子プロセス**として
//! 100% Rust 側で管理する（spawn・監視・kill すべて jarvish 自身が行う）。
//!
//! # プロトコル
//! 1. [`ZshDaemon::spawn`] が `nix::pty::openpty`（`engine/pty.rs` と同じ
//!    クレート利用パターン。`engine::pty` はプライベートモジュールで
//!    `cli::completer` から到達できないため、同じ手順をこのファイル内で
//!    再実装している）で PTY ペアを作り、`zsh -i` を PTY slave 経由の
//!    セッションリーダーとして spawn する（`engine/exec/pty_session.rs`
//!    の `setsid()` + `TIOCSCTTY` パターンを踏襲）。
//! 2. [`assets/zsh/daemon_init.zsh`] の内容を spawn 時に一時ファイルへ
//!    書き出し、`"source <path>\n"` を PTY 経由で送って初期化する。
//!    初期化完了は末尾の `jarvish_daemon_ok` 行（レディマーカー）で判定する。
//! 3. 各補完リクエストは `^U`（kill-whole-line。バッファに残った前回の
//!    リクエスト内容を確実に破棄する）→ エスケープ済みの行 → `^I`
//!    （`jarvish-complete-word` widget、`daemon_init.zsh` 参照）の順で
//!    書き込み、2つの NUL センチネル行に挟まれた候補行ブロックを読み取る。
//!    パース自体は [`super::zsh_bridge::parse_capture_output`] を再利用する
//!    （`capture.zsh` と `daemon_init.zsh` の `compadd` オーバーライドは
//!    同一の "value -- description" 形式で出力するため、パーサを複製しない）。
//! 4. タイムアウトまたはプロトコル desync（センチネルが揃わない）を
//!    検知した場合は子プロセスとその子孫ツリー全体を kill し
//!    （[`super::external::kill_tree`] を再利用）、以後 `is_alive()` は
//!    `false` を返す（このデーモンインスタンスは使い捨てられ、
//!    呼び出し元が必要なら新しい `ZshDaemon` を spawn し直す）。
//!
//! # `compprefuncs` / `comppostfuncs` が一度きりの配列である問題
//! プロトタイピング中に実機検証で判明した重要な zsh の挙動: `_main_complete`
//! （`complete-word` widget が実際に呼ぶ補完システムの本体）は
//! `compprefuncs` / `comppostfuncs` を読み取った直後に**空配列へリセット**
//! する（`funcs=("$compprefuncs[@]"); compprefuncs=()` というコードが
//! `_main_complete` 本体に存在する — `autoload -Uz +X _main_complete` で
//! 確認可能）。`capture.zsh` はプロセスごとに1回しか補完しないためこれに
//! 気づかないが、常駐デーモンでは2回目以降の Tab でセンチネル行が
//! 一切出力されなくなり、読み取り側が容易に desync する。
//! `daemon_init.zsh` はこれを、`compprefuncs`/`comppostfuncs` を**毎回
//! 再武装するラッパー ZLE widget**（`jarvish-complete-word`）を `^I` に
//! 束縛することで解決している（詳細は同ファイルのコメント参照）。
//!
//! # このタスクのスコープ（Task 2b.3 の Task 1）
//! ここでは init スクリプトと [`ZshDaemon`] のライフサイクル（spawn /
//! request / shutdown / Drop）のみを実装する。`JarvishCompleter` の
//! provider チェーンへの実配線（`ZshBridgeProvider` からの切り替え、
//! 死亡時の自動 respawn 等）は Task 2 のスコープのため、本タスクの時点
//! では `ZshDaemon` はまだどこからも呼ばれない。`cargo clippy -D warnings`
//! の dead_code 検査に対しては、下記の `#![allow(dead_code)]` で
//! モジュール全体を一時的に許容する（各アイテムはユニットテスト・zsh
//! 実機統合テストの両方で実際に exercise 済み — 末尾の `tests` モジュール
//! 参照。Task 2 で実配線されれば通常の到達可能性で警告は自然に消える）。
#![allow(dead_code)]

use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use nix::pty::openpty;

use super::external::kill_tree;

/// jarvish が生成する init スクリプト本体（`assets/zsh/daemon_init.zsh`）。
const DAEMON_INIT_SCRIPT: &str = include_str!("../../../assets/zsh/daemon_init.zsh");

/// 初期化完了を示すレディマーカー行（`daemon_init.zsh` の末尾 `echo` と対応）。
const READY_MARKER: &str = "jarvish_daemon_ok";

/// センチネル行の末尾マーカー（PTY の `\r\n` 変換後、NUL の直後に `\r` が
/// 来る — `capture.zsh` の `[[ $line == *$'\0\r' ]]` と同じ検出条件）。
const SENTINEL_BYTE: u8 = 0;

/// 温存 zsh 補完デーモン。
///
/// jarvish プロセスの子として `zsh -i` を1本だけ spawn し、複数回の
/// [`request`](Self::request) 呼び出しにわたって使い回す。プロトコル
/// desync やタイムアウトが起きると内部的に「死亡」状態へ遷移し
/// （[`is_alive`](Self::is_alive) が `false` を返す）、以後の `request` は
/// 常に `None` を返す（呼び出し元が新しい `ZshDaemon` を spawn し直す
/// 設計 — このタスクではライフサイクルのみを扱い、provider 側からの
/// 自動再spawn 配線は Task 2 のスコープ）。
pub(crate) struct ZshDaemon {
    child: Child,
    master: fs::File,
    /// PTY slave 側の fd。子プロセスの生存中は親側で保持しておく必要は
    /// ないが、`spawn` 完了まで（`command.spawn()` 呼び出しの直前まで）
    /// 生かしておく必要があるため一時変数として使う（構造体には残さない）。
    alive: bool,
    /// init スクリプトを書き出した一時ファイル（`ZshDaemon` が生きている
    /// 間だけ存在すればよい — `TempPath` 相当を手動管理: Drop で削除）。
    init_script_path: PathBuf,
}

impl ZshDaemon {
    /// `zsh -i` を spawn し、`ZDOTDIR=<bridge_dir>` を設定したうえで
    /// [`DAEMON_INIT_SCRIPT`] を source し、レディマーカーを待つ。
    ///
    /// `extra_envs` はテスト専用フック（`HOME` の compdump キャッシュ隔離等、
    /// [`super::zsh_bridge::ZshBridgeProvider`] の `extra_envs` と同じ用途）
    /// として本番コードからも呼べる形にしてある（`zsh_override` に相当する
    /// バイナリパス差し替えも `zsh_path` 引数で行う）。
    pub(crate) fn spawn(
        zsh_path: &Path,
        bridge_dir: &Path,
        extra_envs: &[(String, String)],
        init_timeout: Duration,
    ) -> io::Result<Self> {
        let init_script_path = write_init_script(bridge_dir)?;

        let (master, slave) = create_daemon_pty()?;
        let slave_raw_fd = slave.as_raw_fd();
        let stdin_fd = unsafe { libc::dup(slave_raw_fd) };
        let stdout_fd = unsafe { libc::dup(slave_raw_fd) };
        let stderr_fd = unsafe { libc::dup(slave_raw_fd) };
        if stdin_fd < 0 || stdout_fd < 0 || stderr_fd < 0 {
            let _ = fs::remove_file(&init_script_path);
            return Err(io::Error::last_os_error());
        }

        let mut command = Command::new(zsh_path);
        command
            .arg("-i")
            .env("ZDOTDIR", bridge_dir)
            .env("TERM", "dumb")
            .envs(extra_envs.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(unsafe { Stdio::from_raw_fd(stdin_fd) })
            .stdout(unsafe { Stdio::from_raw_fd(stdout_fd) })
            .stderr(unsafe { Stdio::from_raw_fd(stderr_fd) });

        // engine/exec/pty_session.rs と同じパターン: 新しいセッションを
        // 作り、PTY を制御端末に設定する。
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as libc::c_ulong, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let child = match command.spawn() {
            Ok(child) => child,
            Err(err) => {
                let _ = fs::remove_file(&init_script_path);
                return Err(err);
            }
        };

        // 親側の PTY slave fd を閉じる（子プロセスに複製済み）。
        drop(slave);

        let mut daemon = Self {
            child,
            master,
            alive: true,
            init_script_path,
        };

        if !daemon.initialize(init_timeout) {
            daemon.shutdown();
            return Err(io::Error::other(
                "zsh daemon failed to reach ready marker within timeout",
            ));
        }

        Ok(daemon)
    }

    /// init スクリプトを source し、レディマーカーを待つ。
    fn initialize(&mut self, timeout: Duration) -> bool {
        let cmd = format!("source {}\n", self.init_script_path.display());
        if self.master.write_all(cmd.as_bytes()).is_err() {
            return false;
        }

        let deadline = Instant::now() + timeout;
        let mut buf = Vec::new();
        while Instant::now() < deadline {
            match read_available(&mut self.master, Duration::from_millis(200)) {
                Some(chunk) => {
                    buf.extend_from_slice(&chunk);
                    if contains_line(&buf, READY_MARKER) {
                        return true;
                    }
                }
                None => continue,
            }
        }
        false
    }

    /// このデーモンが生きている（spawn 済みかつタイムアウト/desync で
    /// killed されていない）かどうか。
    pub(crate) fn is_alive(&self) -> bool {
        self.alive
    }

    /// 補完リクエストを1回実行する。
    ///
    /// `escaped_line`（呼び出し元がすでに `zsh_bridge::escape_spans` 相当の
    /// エスケープを済ませたスペース結合済みの1行）を送り、センチネルで
    /// 挟まれた候補行ブロックの生テキストを返す。
    /// [`super::zsh_bridge::parse_capture_output`] にそのまま渡せる形式
    /// （PTY 由来の `\r\n` 区切り、ANSI・バックスラッシュ未処理）。
    ///
    /// タイムアウトまたはセンチネルが正しく揃わない場合（プロトコル
    /// desync）は子プロセスとその子孫ツリーを kill して `alive = false`
    /// に遷移し、`None` を返す。
    pub(crate) fn request(&mut self, line: &str, timeout: Duration) -> Option<String> {
        if !self.alive {
            return None;
        }

        // ^U (kill-whole-line) で前回リクエストの残留を破棄してから、
        // 新しい行 + ^I (jarvish-complete-word) を送る。
        let payload = format!("\x15{line}\t");
        if self.master.write_all(payload.as_bytes()).is_err() {
            self.mark_dead_and_kill();
            return None;
        }

        let deadline = Instant::now() + timeout;
        let mut buf: Vec<u8> = Vec::new();
        let mut toggles = 0u8;
        let mut frame_start: Option<usize> = None;
        let mut frame_end: Option<usize> = None;

        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let step = remaining.min(Duration::from_millis(200));
            match read_available(&mut self.master, step) {
                Some(chunk) if !chunk.is_empty() => {
                    let base = buf.len();
                    buf.extend_from_slice(&chunk);
                    // 新しく読めたバイト範囲内で NUL をスキャンする。
                    let mut idx = base;
                    while idx < buf.len() {
                        if buf[idx] == SENTINEL_BYTE {
                            toggles += 1;
                            if toggles == 1 {
                                frame_start = Some(idx + 1);
                            } else if toggles == 2 {
                                frame_end = Some(idx);
                                break;
                            }
                        }
                        idx += 1;
                    }
                    if toggles >= 2 {
                        break;
                    }
                }
                _ => continue,
            }
        }

        match (frame_start, frame_end) {
            (Some(start), Some(end)) if start <= end => {
                Some(String::from_utf8_lossy(&buf[start..end]).into_owned())
            }
            _ => {
                // タイムアウト or desync（センチネルが2個揃わなかった）。
                self.mark_dead_and_kill();
                None
            }
        }
    }

    /// 子プロセスとその子孫ツリーを kill し、`alive` を `false` にする。
    ///
    /// SIGKILL 送信から実際にプロセスが終了しカーネルが zombie 化する
    /// までは非同期のレースがあるため、`try_wait()` を短時間・有界回数
    /// ポーリングして reap を試みる（1回きりの `try_wait()` だと直後の
    /// 呼び出し元が `kill(pid, 0)` で生存確認した際に「reap されていない
    /// zombie はまだ ESRCH にならず生存扱いに見える」という false
    /// negative を生みうる — 呼び出し元が `request()` のタイムアウト直後に
    /// 「子プロセスが本当に死んだか」を確認できることが本タスクの要件の
    /// 一つのため、ここで確実に reap してから返す）。`Drop` 側にも同様の
    /// ポーリングがあるが、そちらは `shutdown()`/`mark_dead_and_kill()` が
    /// 既に reap 済みなら即座に抜けるだけの冪等な保険。
    fn mark_dead_and_kill(&mut self) {
        if !self.alive {
            return;
        }
        self.alive = false;
        kill_tree(self.child.id());
        for _ in 0..40 {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => std::thread::sleep(Duration::from_millis(25)),
                Err(_) => break,
            }
        }
    }

    /// デーモンを明示的に終了させる（`Drop` からも呼ばれる冪等操作）。
    pub(crate) fn shutdown(&mut self) {
        self.mark_dead_and_kill();
    }
}

impl Drop for ZshDaemon {
    fn drop(&mut self) {
        self.shutdown();
        // 子孫まで含めた kill 後、直接の子プロセスが reap されるまで
        // 短時間ポーリングする（zombie を残さない。external.rs のテストの
        // ESRCH ポーリングパターンと同じ考え方だが、ここでは `Child::wait`
        // 経由で reap する — `libc::kill(pid, 0)` による生存確認は
        // `wait()` していない子プロセスに対しても ESRCH を返しうる
        // （まだ親が reap していないだけで実体はゾンビとして残る）ため、
        // 確実な reap には `try_wait` の方が適切）。
        for _ in 0..40 {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => std::thread::sleep(Duration::from_millis(25)),
                Err(_) => break,
            }
        }
        let _ = fs::remove_file(&self.init_script_path);
    }
}

/// [`ZshDaemon::spawn`] 用の PTY ペアを作る。
///
/// `engine/pty.rs::create_session_pty` と同じ手順（`nix::pty::openpty`、
/// OPOST は有効のまま）だが、`engine::pty` はプライベートモジュールで
/// `cli::completer` から到達できないため、ここで同じパターンを再実装する
/// （タスク指示: "portable-pty crate ... same as engine/pty.rs" —
/// 実際には `engine/pty.rs` 自体が `nix::pty::openpty` を直接使っており
/// `portable-pty` クレートには依存していないため、既存のクレート利用
/// パターンに揃えている）。
fn create_daemon_pty() -> io::Result<(fs::File, OwnedFd)> {
    let ws = libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let pty = openpty(Some(&ws), None).map_err(|e| io::Error::other(e.to_string()))?;
    let master_file = fs::File::from(pty.master);
    Ok((master_file, pty.slave))
}

/// [`DAEMON_INIT_SCRIPT`] を `bridge_dir` 配下の専用一時ファイルへ書き出す。
///
/// プロセス pid を混ぜたファイル名にすることで、同一ホストで複数の
/// jarvish セッションが同時にデーモンを spawn しても衝突しない。
fn write_init_script(bridge_dir: &Path) -> io::Result<PathBuf> {
    fs::create_dir_all(bridge_dir)?;
    let path = bridge_dir.join(format!(".daemon_init.{}.zsh", std::process::id()));
    fs::write(&path, DAEMON_INIT_SCRIPT)?;
    Ok(path)
}

/// PTY master から利用可能なバイト列を読み取る（最大 `timeout` 待つ）。
///
/// ノンブロッキング切り替えはせず、`read` がタイムアウト付きで返る保証は
/// ないため、[`nix`] の `poll` で先に readable を確認してから読む。
/// タイムアウトで readable にならなかった場合は `None`（呼び出し元は
/// ループ継続 or デッドライン判定）。EOF（子プロセス死亡等で `read` が
/// `0` バイトを返す）の場合も `None` を返す。
fn read_available(master: &mut fs::File, timeout: Duration) -> Option<Vec<u8>> {
    use nix::poll::{poll, PollFd, PollFlags, PollTimeout};

    let fd = master.as_fd();
    let mut fds = [PollFd::new(fd, PollFlags::POLLIN)];
    let poll_timeout: PollTimeout = timeout.as_millis().try_into().unwrap_or(PollTimeout::MAX);
    match poll(&mut fds, poll_timeout) {
        Ok(0) => None,
        Ok(_) => {
            let mut buf = [0u8; 65536];
            match master.read(&mut buf) {
                Ok(0) => None,
                Ok(n) => Some(buf[..n].to_vec()),
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => None,
                Err(_) => None,
            }
        }
        Err(_) => None,
    }
}

/// `haystack`（複数行、`\r\n` または `\n` 区切りを想定）の中に `needle` と
/// 完全一致する行が含まれるかどうかを判定する。
///
/// [`ZshDaemon::initialize`] のレディマーカー検出専用ヘルパー。ANSI
/// エスケープや先頭・末尾の空白除去は行わない単純な行完全一致（実機検証:
/// `echo jarvish_daemon_ok` はプロンプト無効化済み・エコーバック無効な
/// PTY 経由でも装飾なしにそのまま出力される）。
fn contains_line(haystack: &[u8], needle: &str) -> bool {
    let text = String::from_utf8_lossy(haystack);
    text.lines().any(|line| line.trim_end() == needle)
}

#[cfg(test)]
mod tests {
    use super::super::zsh_bridge::parse_capture_output;
    use super::*;
    use serial_test::serial;
    use std::fs;

    // ── ユニットテスト: センチネルフレーミング（zsh 不要） ──

    #[test]
    fn contains_line_finds_exact_match_among_multiple_lines() {
        let haystack = b"source /tmp/foo.zsh\r\nsome noise\r\njarvish_daemon_ok\r\n";
        assert!(contains_line(haystack, READY_MARKER));
    }

    #[test]
    fn contains_line_does_not_match_substring_only() {
        let haystack = b"not jarvish_daemon_ok exactly\r\n";
        // 行全体が完全一致しない限り false（部分文字列一致では拾わない）。
        assert!(!contains_line(haystack, READY_MARKER));
    }

    #[test]
    fn contains_line_absent_returns_false() {
        let haystack = b"still initializing...\r\n";
        assert!(!contains_line(haystack, READY_MARKER));
    }

    #[test]
    fn contains_line_trims_trailing_carriage_return() {
        // PTY 由来の \r\n 行末で trim_end が \r も落とすことを確認する。
        let haystack = b"jarvish_daemon_ok\r\n";
        assert!(contains_line(haystack, READY_MARKER));
    }

    /// センチネル2個に挟まれたテキストを抽出するロジックを、
    /// `request()` 本体から切り出さずに直接文字列操作で再現して検証する
    /// （`request()` は PTY 越しの非同期読み取りを含むため、フレーミング
    /// だけを純粋にテストする目的でロジックを模した最小実装を使う）。
    fn extract_frame(buf: &[u8]) -> Option<String> {
        let mut toggles = 0u8;
        let mut frame_start = None;
        let mut frame_end = None;
        for (idx, &byte) in buf.iter().enumerate() {
            if byte == SENTINEL_BYTE {
                toggles += 1;
                if toggles == 1 {
                    frame_start = Some(idx + 1);
                } else if toggles == 2 {
                    frame_end = Some(idx);
                    break;
                }
            }
        }
        match (frame_start, frame_end) {
            (Some(s), Some(e)) if s <= e => Some(String::from_utf8_lossy(&buf[s..e]).into_owned()),
            _ => None,
        }
    }

    #[test]
    fn extract_frame_between_two_sentinels() {
        let buf = b"jarvishtestcmd \x00\r\nalpha\r\nbeta\r\ngamma\r\n\x00\r\n\x07";
        let frame = extract_frame(buf).expect("should find a frame");
        assert_eq!(frame, "\r\nalpha\r\nbeta\r\ngamma\r\n");
    }

    #[test]
    fn extract_frame_missing_second_sentinel_returns_none() {
        let buf = b"jarvishtestcmd \x00\r\nalpha\r\nbeta\r\n";
        assert_eq!(extract_frame(buf), None);
    }

    #[test]
    fn extract_frame_no_sentinel_at_all_returns_none() {
        let buf = b"alpha\r\nbeta\r\ngamma\r\n";
        assert_eq!(extract_frame(buf), None);
    }

    #[test]
    fn extract_frame_empty_frame_between_adjacent_sentinels() {
        let buf = b"\x00\x00";
        let frame = extract_frame(buf).expect("adjacent sentinels should still frame (empty)");
        assert_eq!(frame, "");
    }

    #[test]
    fn extracted_frame_feeds_into_parse_capture_output() {
        // フレーム抽出 → 既存の zsh_bridge パーサへ、という Task 2 の
        // 実配線を模した end-to-end 相当のユニットテスト（zsh 不要）。
        let buf = b"jarvishtestcmd \x00\r\nalpha\r\nbeta -- desc\r\n\x00\r\n\x07";
        let frame = extract_frame(buf).expect("should find a frame");
        let candidates = parse_capture_output(&frame);
        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert_eq!(values, vec!["alpha", "beta"]);
        assert_eq!(
            candidates
                .iter()
                .find(|c| c.value == "beta")
                .unwrap()
                .description,
            Some("desc".to_string())
        );
    }

    // ── zsh 実機統合テスト（zsh 不在なら runtime skip、#[serial]） ──

    fn zsh_binary() -> Option<PathBuf> {
        which::which("zsh").ok()
    }

    /// テスト用の隔離された ZDOTDIR + fpath ディレクトリ + カスタム補完
    /// フィクスチャ（固定ワードリスト）+ 隔離 HOME を作る。
    ///
    /// `zsh_bridge.rs` の E2E テストと同じ理由（`compinit -d
    /// ~/.zcompdump_capture` は `$HOME` 基準の固定パスにキャッシュを
    /// 読み書きするため、テストごとに `$HOME` も隔離しないと compdump が
    /// 衝突する）で `HOME` も隔離する。
    struct TestFixture {
        _tmpdir: tempfile::TempDir,
        zdotdir: PathBuf,
        home: PathBuf,
    }

    fn setup_fixture(completions: &[(&str, &str)]) -> TestFixture {
        let tmpdir = tempfile::tempdir().unwrap();
        let zdotdir = tmpdir.path().join("zdotdir");
        let fpath_dir = tmpdir.path().join("completions");
        let home = tmpdir.path().join("home");
        fs::create_dir_all(&zdotdir).unwrap();
        fs::create_dir_all(&fpath_dir).unwrap();
        fs::create_dir_all(&home).unwrap();

        for (name, body) in completions {
            fs::write(fpath_dir.join(name), body).unwrap();
        }
        fs::write(
            zdotdir.join(".zshrc"),
            format!("fpath=({} $fpath)\n", fpath_dir.display()),
        )
        .unwrap();

        TestFixture {
            _tmpdir: tmpdir,
            zdotdir,
            home,
        }
    }

    fn extra_envs_for(fixture: &TestFixture) -> Vec<(String, String)> {
        vec![(
            "HOME".to_string(),
            fixture.home.to_string_lossy().into_owned(),
        )]
    }

    #[test]
    #[serial]
    fn spawn_reaches_ready_marker() {
        let Some(zsh) = zsh_binary() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        let fixture = setup_fixture(&[(
            "_jarvishtestcmd",
            "#compdef jarvishtestcmd\ncompadd -- alpha beta gamma\n",
        )]);

        let daemon = ZshDaemon::spawn(
            &zsh,
            &fixture.zdotdir,
            &extra_envs_for(&fixture),
            Duration::from_secs(10),
        );
        let mut daemon = daemon.expect("daemon should spawn and reach ready marker");
        assert!(daemon.is_alive());
        daemon.shutdown();
        assert!(!daemon.is_alive());
    }

    #[test]
    #[serial]
    fn two_sequential_requests_reuse_same_daemon_and_return_words() {
        let Some(zsh) = zsh_binary() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        let fixture = setup_fixture(&[(
            "_jarvishtestcmd",
            "#compdef jarvishtestcmd\ncompadd -- alpha beta gamma\n",
        )]);

        let mut daemon = ZshDaemon::spawn(
            &zsh,
            &fixture.zdotdir,
            &extra_envs_for(&fixture),
            Duration::from_secs(10),
        )
        .expect("daemon should spawn");

        let child_pid_before = daemon.child.id();

        let first = daemon
            .request("jarvishtestcmd ", Duration::from_secs(3))
            .expect("first request should succeed");
        assert!(
            daemon.is_alive(),
            "daemon must still be alive after request 1"
        );

        let start = Instant::now();
        let second = daemon
            .request("jarvishtestcmd ", Duration::from_secs(3))
            .expect("second request should succeed");
        let elapsed = start.elapsed();

        assert!(
            daemon.is_alive(),
            "daemon must still be alive after request 2"
        );
        assert_eq!(
            daemon.child.id(),
            child_pid_before,
            "the same child process must serve both requests (no respawn)"
        );

        let candidates1 = parse_capture_output(&first);
        let candidates2 = parse_capture_output(&second);
        let values1: Vec<&str> = candidates1.iter().map(|c| c.value.as_str()).collect();
        let values2: Vec<&str> = candidates2.iter().map(|c| c.value.as_str()).collect();
        assert!(
            values1.contains(&"alpha") && values1.contains(&"beta") && values1.contains(&"gamma")
        );
        assert!(
            values2.contains(&"alpha") && values2.contains(&"beta") && values2.contains(&"gamma")
        );

        eprintln!("warm second-request latency: {elapsed:?}");
        assert!(
            elapsed < Duration::from_millis(500),
            "warm second request should be well under 500ms (compute-only), took {elapsed:?}"
        );
    }

    #[test]
    #[serial]
    fn no_state_bleed_between_different_requests() {
        let Some(zsh) = zsh_binary() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        let fixture = setup_fixture(&[
            (
                "_jarvishtestcmd",
                "#compdef jarvishtestcmd\ncompadd -- alphaone alphatwo\n",
            ),
            (
                "_jarvishtestcmd2",
                "#compdef jarvishtestcmd2\ncompadd -- betaone betatwo\n",
            ),
        ]);

        let mut daemon = ZshDaemon::spawn(
            &zsh,
            &fixture.zdotdir,
            &extra_envs_for(&fixture),
            Duration::from_secs(10),
        )
        .expect("daemon should spawn");

        // request A: long/different line.
        let out_a = daemon
            .request("jarvishtestcmd al", Duration::from_secs(3))
            .expect("request A should succeed");
        let candidates_a = parse_capture_output(&out_a);
        let values_a: Vec<&str> = candidates_a.iter().map(|c| c.value.as_str()).collect();
        assert!(values_a.contains(&"alphaone") && values_a.contains(&"alphatwo"));

        // request B: a DIFFERENT command entirely -- must reflect only B.
        let out_b = daemon
            .request("jarvishtestcmd2 ", Duration::from_secs(3))
            .expect("request B should succeed");
        let candidates_b = parse_capture_output(&out_b);
        let values_b: Vec<&str> = candidates_b.iter().map(|c| c.value.as_str()).collect();
        assert!(values_b.contains(&"betaone") && values_b.contains(&"betatwo"));
        assert!(
            !values_b.iter().any(|v| v.starts_with("alpha")),
            "request B must not bleed candidates from request A: {values_b:?}"
        );
    }

    #[test]
    #[serial]
    fn hung_completion_times_out_and_kills_descendants() {
        let Some(zsh) = zsh_binary() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        // フィクスチャ: 30秒 sleep してから compadd するハング補完関数。
        let fixture = setup_fixture(&[(
            "_jarvishtesthang",
            "#compdef jarvishtesthang\nsleep 30\ncompadd -- neverseen\n",
        )]);

        let mut daemon = ZshDaemon::spawn(
            &zsh,
            &fixture.zdotdir,
            &extra_envs_for(&fixture),
            Duration::from_secs(10),
        )
        .expect("daemon should spawn");

        let child_pid = daemon.child.id();

        let start = Instant::now();
        let result = daemon.request("jarvishtesthang ", Duration::from_millis(500));
        let elapsed = start.elapsed();

        assert_eq!(result, None, "hung completion should time out to None");
        assert!(
            elapsed < Duration::from_secs(3),
            "request() should return promptly after the configured timeout, took {elapsed:?}"
        );
        assert!(
            !daemon.is_alive(),
            "daemon must be marked dead after a timeout"
        );

        // 子プロセス（と、可能なら子孫）が実際に死んでいることを ESRCH
        // ポーリングで確認する（external.rs のテストと同じ考え方）。
        let mut alive = true;
        for _ in 0..40 {
            let ret = unsafe { libc::kill(child_pid as libc::pid_t, 0) };
            if ret == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                alive = false;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            !alive,
            "child pid {child_pid} should be dead after timeout kill"
        );
    }

    #[test]
    #[serial]
    fn drop_kills_child_process() {
        let Some(zsh) = zsh_binary() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        let fixture = setup_fixture(&[(
            "_jarvishtestcmd",
            "#compdef jarvishtestcmd\ncompadd -- alpha\n",
        )]);

        let daemon = ZshDaemon::spawn(
            &zsh,
            &fixture.zdotdir,
            &extra_envs_for(&fixture),
            Duration::from_secs(10),
        )
        .expect("daemon should spawn");
        let child_pid = daemon.child.id();

        drop(daemon);

        let mut alive = true;
        for _ in 0..40 {
            let ret = unsafe { libc::kill(child_pid as libc::pid_t, 0) };
            if ret == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                alive = false;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!alive, "child pid {child_pid} should be dead after Drop");
    }

    #[test]
    #[serial]
    fn init_script_file_is_removed_on_drop() {
        let Some(zsh) = zsh_binary() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        let fixture = setup_fixture(&[(
            "_jarvishtestcmd",
            "#compdef jarvishtestcmd\ncompadd -- alpha\n",
        )]);

        let daemon = ZshDaemon::spawn(
            &zsh,
            &fixture.zdotdir,
            &extra_envs_for(&fixture),
            Duration::from_secs(10),
        )
        .expect("daemon should spawn");
        let script_path = daemon.init_script_path.clone();
        assert!(script_path.exists());

        drop(daemon);

        assert!(
            !script_path.exists(),
            "init script temp file should be removed on Drop"
        );
    }

    #[test]
    #[serial]
    fn request_on_dead_daemon_returns_none_without_hanging() {
        let Some(zsh) = zsh_binary() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        let fixture = setup_fixture(&[(
            "_jarvishtestcmd",
            "#compdef jarvishtestcmd\ncompadd -- alpha\n",
        )]);

        let mut daemon = ZshDaemon::spawn(
            &zsh,
            &fixture.zdotdir,
            &extra_envs_for(&fixture),
            Duration::from_secs(10),
        )
        .expect("daemon should spawn");

        daemon.shutdown();
        assert!(!daemon.is_alive());

        let start = Instant::now();
        let result = daemon.request("jarvishtestcmd ", Duration::from_secs(3));
        let elapsed = start.elapsed();

        assert_eq!(result, None);
        assert!(
            elapsed < Duration::from_millis(200),
            "request on a dead daemon should return immediately, took {elapsed:?}"
        );
    }

    #[test]
    #[serial]
    fn spawn_with_invalid_zsh_binary_returns_err() {
        let fixture = setup_fixture(&[]);
        let result = ZshDaemon::spawn(
            Path::new("/no/such/zsh/binary/zzjarvish"),
            &fixture.zdotdir,
            &extra_envs_for(&fixture),
            Duration::from_secs(2),
        );
        assert!(result.is_err());
    }
}
