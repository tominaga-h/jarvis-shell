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
//! # `JarvishCompleter` への配線（Task 2b.3 の Task 2、Fix D4 で更新）
//! [`super::zsh_bridge::ZshBridgeProvider`] が `Mutex<Option<ZshDaemon>>` を
//! 保持し、`[completion] external_zsh_daemon` が有効な間は**シェル起動直後に
//! バックグラウンドスレッドから事前ウォームアップ**される
//! （[`super::zsh_bridge::prewarm_zsh_daemon`]、Fix D4）ため、通常は最初の
//! 補完リクエストの時点で既にこのスロットが埋まっている。プリウォームが
//! 間に合わなかった場合（または zsh 未検出等でスキップされた場合）は、
//! 最初にデーモンを必要とするリクエストで遅延 spawn する経路がフォール
//! バックとして機能する。以後は同じインスタンスを使い回す。連続タイム
//! アウトで `is_alive()` が `false` になった場合や、ブリッジ `.zshrc` の
//! mtime が spawn 時から変わっていた場合は shutdown して次回リクエストで
//! 再 spawn する（`zsh_bridge.rs` のモジュールドキュメント参照）。

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use nix::pty::openpty;
use nix::sys::termios::{self, LocalFlags, SetArg};

use super::external::kill_tree;

/// [`ZshDaemon::request`] のレスポンスバッファ上限（B4, #89）。
///
/// ハング/バグった補完関数が延々と出力し続けるケース（無限ループの
/// `compadd`、巨大ファイルの誤 cat 等）に対して、タイムアウトまで
/// 無制限に `Vec<u8>` を伸ばし続けるとメモリを圧迫する。この上限を
/// 超えた時点でプロトコル desync 相当として扱い、即座に `mark_dead_and_kill`
/// して `None` を返す（タイムアウトを待たない）。4 MiB は通常の補完候補
/// 数千件分でも十分な余裕がある値。
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

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
    /// `None` になるのは kill/reap の所有権をバックグラウンドスレッドへ
    /// 渡した後（[`mark_dead_and_kill`](Self::mark_dead_and_kill) 系メソッド
    /// が呼ばれた後）のみ（B1/B2, #89）。`alive == true` の間は常に `Some`。
    child: Option<Child>,
    master: fs::File,
    /// PTY slave 側の fd。子プロセスの生存中は親側で保持しておく必要は
    /// ないが、`spawn` 完了まで（`command.spawn()` 呼び出しの直前まで）
    /// 生かしておく必要があるため一時変数として使う（構造体には残さない）。
    alive: bool,
    /// init スクリプトを書き出した一時ファイル（`ZshDaemon` が生きている
    /// 間だけ存在すればよい — `TempPath` 相当を手動管理）。kill/reap の
    /// 所有権譲渡と同時にこのパスも移譲する（`None` になったら既に
    /// バックグラウンドスレッド or 呼び出し元が削除責任を持つ）。
    init_script_path: Option<PathBuf>,
    /// Fix D2（グレースドレイン）: 直前のリクエストがタイムアウトし、
    /// まだ完了していない応答フレームが PTY 側に残っている可能性がある
    /// （zsh 側は補完関数の実行を継続しており、いずれセンチネルに挟まれた
    /// 出力を送ってくる）ことを示すフラグ。`true` の間、次の `request()`
    /// 呼び出しは新しい行を送る**前**にこの残留フレームを読み飛ばす
    /// （[`drain_pending_frame`](Self::drain_pending_frame)）。
    pending_frame: bool,
    /// [`read_framed_response`](Self::read_framed_response) の呼び出しを
    /// またいで持ち越す部分読み取り状態（Fix D2）。
    ///
    /// 1つの論理フレームの**開始センチネル**は `_main_complete` が
    /// `compprefuncs` を呼んだ直後（補完関数本体が実行される**前**）に
    /// 出力される（`null-line` を先に呼ぶ `compprefuncs` の仕組み——
    /// `daemon_init.zsh` 参照）。そのため、遅い補完関数の場合「開始
    /// センチネルは元のリクエストのタイムアウト以内に届くが、終了
    /// センチネルは補完関数が完了する後続のドレイン呼び出しでようやく届く」
    /// という状況が普通に起こる。`read_framed_response` を呼び出しごとに
    /// 独立したローカル状態（`toggles`/`buf` をその場でリセット）で実装
    /// すると、ドレイン側の呼び出しは「終了センチネル1個だけ」を見て
    /// `toggles == 1`（2に届かない）と誤判定し、実際にはフレームが完成して
    /// いるにも関わらずタイムアウト扱いになってしまう（実機検証で発覚 —
    /// 実装当初のバグ）。これを避けるため、読み取りバッファとトグル状態を
    /// `ZshDaemon` インスタンスに持たせ、`request()` の呼び出しをまたいで
    /// 引き継ぐ。
    partial_read: PartialRead,
    /// Fix D2（サーキットブレーカー）: 直近の「成功したフレーム読み取り」
    /// 以降に連続したタイムアウト回数。ドレイン自体のタイムアウトも
    /// 1回とカウントする。2 に達した時点でデーモンをハングとみなし
    /// kill する（[`mark_dead_and_kill`](Self::mark_dead_and_kill)）。
    /// 成功したフレーム読み取りが1回でもあればこのカウンタは 0 に戻る。
    consecutive_timeouts: u8,
}

/// [`ZshDaemon`] がハングと判定してデーモンを kill するまでに許容する
/// 連続タイムアウト回数（Fix D2 サーキットブレーカー）。
///
/// 「1回のタイムアウト」は遅いが正常な補完関数（例: インタプリタ起動を
/// 伴う `tmuxinator` 補完）でも普通に起こりうるため即座には殺さない。
/// 2回連続（= ドレインした上でなお次のリクエストもタイムアウトする、
/// または2回連続で素の要求がタイムアウトする）で初めて「本当にハングして
/// いる」とみなす。
const MAX_CONSECUTIVE_TIMEOUTS: u8 = 2;

/// [`ZshDaemon::read_framed_response`] の結果。
enum FramedRead {
    /// 2つのセンチネルに挟まれたフレームを正常に読み取れた。
    Frame(String),
    /// `deadline` までにセンチネルが2個揃わなかった（純粋なタイムアウト、
    /// またはセンチネルが1個も来ないまま `deadline` に達した場合を含む）。
    Timeout,
    /// 応答バッファが [`MAX_RESPONSE_BYTES`] を超えた（B4）。プロトコル
    /// desync 相当として扱う——グレースの対象外。
    BufferOverflow,
}

/// [`ZshDaemon::read_framed_response`] が `request()`/`drain_pending_frame`
/// の呼び出しをまたいで持ち越す部分読み取り状態（Fix D2）。
///
/// `ZshDaemon::partial_read` フィールドのドキュメント参照——1つの論理
/// フレームの開始・終了センチネルが異なる呼び出し（元のリクエストと、
/// それに続くドレイン呼び出し）にまたがって届くケースに対応するため、
/// バッファとトグルカウントを呼び出し間で保持する。
#[derive(Default)]
struct PartialRead {
    /// これまでに読み取った生バイト列（フレームが完成する、または
    /// バッファ上限超過/desync でリセットされるまで蓄積し続ける）。
    buf: Vec<u8>,
    /// これまでに検出したセンチネルバイトの個数（0, 1, または 2）。
    toggles: u8,
    /// 最初のセンチネル直後のオフセット（`buf` 内、フレーム本体の開始位置）。
    frame_start: Option<usize>,
}

impl PartialRead {
    /// フレームが完成した（`toggles == 2`）ときに、次のリクエストに
    /// 備えて状態を空にリセットする。
    fn reset(&mut self) {
        self.buf.clear();
        self.toggles = 0;
        self.frame_start = None;
    }
}

/// kill/reap をバックグラウンドスレッドまたは有界同期待ちへ委譲するために
/// 必要な所有権一式（B1/B2, #89）。
///
/// `mark_dead_and_kill` / `shutdown_blocking` はどちらも「`alive` を
/// `false` にし、この束を取り出してから実際の kill 処理へ渡す」という
/// 同じ手順を踏む。処理そのもの（`kill_tree` → 有界 `try_wait` ポーリング
/// → 一時ファイル削除）は [`reap_bundle`] に一本化し、呼び出し元
/// （バックグラウンドスレッド or 呼び出し元スレッド自身）が同期/非同期
/// どちらの文脈で呼ぶかだけを選べるようにする。
struct ReapBundle {
    child: Child,
    init_script_path: PathBuf,
}

/// 実際の kill + 有界 reap + 一時ファイル削除処理そのもの。
///
/// `deadline` に達するまで `try_wait()` を 25ms 間隔でポーリングする
/// （デフォルトの 40 回 × 25ms = 最大 1000ms という既存の待ち時間予算を
/// `deadline` という形に一般化しただけで、呼び出し元の待ち方針
/// （バックグラウンドスレッドで無視して良いか、呼び出し元が有界に
/// 待ちたいか）には関知しない）。
fn reap_bundle(bundle: ReapBundle, deadline: Instant) {
    let ReapBundle {
        mut child,
        init_script_path,
    } = bundle;
    kill_tree(child.id());
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(_) => break,
        }
    }
    let _ = fs::remove_file(&init_script_path);
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
            child: Some(child),
            master,
            alive: true,
            init_script_path: Some(init_script_path),
            pending_frame: false,
            partial_read: PartialRead::default(),
            consecutive_timeouts: 0,
        };

        if !daemon.initialize(init_timeout) {
            // spawn() 自体は失敗として呼び出し元へ Err を返すため、ここでは
            // 即座に確定させたい（呼び出し元がすぐ再試行/別経路へ切り替える
            // 可能性がある）。バックグラウンド化はせず、既存どおり有界に
            // 同期 reap してから Err を返す。
            daemon.shutdown_blocking(Duration::from_millis(1000));
            return Err(io::Error::other(
                "zsh daemon failed to reach ready marker within timeout",
            ));
        }

        Ok(daemon)
    }

    /// init スクリプトを source し、レディマーカーを待つ。
    ///
    /// `spawn()` から `alive = true` になった直後にのみ呼ばれるため、
    /// `init_script_path` は常に `Some`（kill/reap への所有権移譲は
    /// まだ起きていない）。
    fn initialize(&mut self, timeout: Duration) -> bool {
        let Some(init_script_path) = self.init_script_path.as_ref() else {
            return false;
        };
        let cmd = format!("source {}\n", init_script_path.display());
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

    /// 子プロセスの pid を返す（テスト専用: [`super::zsh_bridge`] の
    /// 「同じ子プロセスが複数リクエストにわたって使い回されているか」
    /// 「mtime 変化で実際に respawn（新しい pid）されたか」を実機の pid
    /// 比較で直接証明するためのアクセサ）。
    #[cfg(test)]
    pub(crate) fn child_pid_for_test(&self) -> u32 {
        self.child.as_ref().map(Child::id).unwrap_or(0)
    }

    /// 補完リクエストを1回実行する。
    ///
    /// `escaped_line`（呼び出し元がすでに `zsh_bridge::escape_spans` 相当の
    /// エスケープを済ませたスペース結合済みの1行）を送り、センチネルで
    /// 挟まれた候補行ブロックの生テキストを返す。
    /// [`super::zsh_bridge::parse_capture_output`] にそのまま渡せる形式
    /// （PTY 由来の `\r\n` 区切り、ANSI・バックスラッシュ未処理）。
    ///
    /// # Fix D2: グレースドレイン + サーキットブレーカー
    ///
    /// **クリーンなタイムアウト**（センチネルが1個も来ない、または1個しか
    /// 来ないまま `timeout` に達した場合）はもはや即座にデーモンを kill
    /// しない。代わりに [`pending_frame`](Self::pending_frame) を立てて
    /// `None` を返すだけに留める（この Tab の UI 応答性を確保しつつ、
    /// 遅いだけで正常な補完関数の結果を捨てない）。次の `request()` 呼び
    /// 出しは、新しい行を送る**前**にまずこの残留フレームを排水する
    /// （[`drain_pending_frame`](Self::drain_pending_frame)、予算は
    /// `timeout` を再利用）。ドレイン自体がタイムアウトした場合、または
    /// 「連続2回」のクリーンタイムアウト（ドレイン失敗 + 通常リクエスト
    /// 失敗、または通常リクエストの failure が2回連続、のいずれか）が
    /// 起きた場合にのみ、デーモンをハングと判定して kill する
    /// （[`MAX_CONSECUTIVE_TIMEOUTS`]）。フレーム読み取りが1回でも成功
    /// すればこのカウンタは 0 にリセットされる。
    ///
    /// プロトコル desync（センチネルが完全に壊れた並びで来る等、
    /// [`read_framed_response`](Self::read_framed_response) が
    /// `FramedRead::Timeout` 以外の失敗として扱うことはない——現行の
    /// フレーミング実装はタイムアウトと desync を区別しないため、
    /// **desync も「クリーンなタイムアウト」と同じグレース経路を通る**）
    /// と応答バッファ上限超過（[`FramedRead::BufferOverflow`]、B4）は
    /// 従来どおり**グレースの対象外**——即座に子プロセスとその子孫ツリーの
    /// kill/reap を**バックグラウンドスレッドへ委譲**し（B1、呼び出し元
    /// スレッドはブロックしない）、`alive = false` に遷移して `None` を
    /// 返す。
    pub(crate) fn request(&mut self, line: &str, timeout: Duration) -> Option<String> {
        if !self.alive {
            return None;
        }

        // B3: 書き込み前の安価な生存確認。外部要因（OOM killer、手動
        // kill 等）で子プロセスが既に死んでいる場合、フルタイムアウトを
        // 待たずに即座に None を返す（次の Tab での遅延 respawn に任せる
        // — ここでインラインに respawn はしない、タスク指示どおり）。
        // `try_wait()` はノンブロッキングなので UI スレッドを一切止めない。
        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(Some(_)) => {
                    // 既に終了済み（reap 待ちの zombie）。kill_tree 自体は
                    // 冪等・無害だが、資源解放（PTY fd 等)の一貫した経路を
                    // 保つためバックグラウンド委譲に統一する。
                    self.mark_dead_and_kill();
                    return None;
                }
                Ok(None) => {}
                Err(_) => {
                    // try_wait 自体のエラーは「判定不能」であり、通常運転を
                    // 妨げない（従来どおり通常のリクエストへ進む）。
                }
            }
        }

        // Fix D2: 前回リクエストがタイムアウトして残留フレームがある場合、
        // 新しい行を送る前にまずそれを排水する。ドレイン自体が失敗した
        // 場合はサーキットブレーカーのカウンタを進めたうえで即座に
        // `None` を返す（新しいリクエストは送らない——2つのリクエストの
        // 応答が PTY 上で混ざる desync を避けるため）。
        if self.pending_frame && !self.drain_pending_frame(timeout) {
            return None;
        }

        // ^U (kill-whole-line) で前回リクエストの残留を破棄してから、
        // 新しい行 + ^I (jarvish-complete-word) を送る。
        let payload = format!("\x15{line}\t");
        if self.master.write_all(payload.as_bytes()).is_err() {
            self.mark_dead_and_kill();
            return None;
        }

        match self.read_framed_response(timeout) {
            FramedRead::Frame(frame) => {
                self.consecutive_timeouts = 0;
                Some(frame)
            }
            FramedRead::BufferOverflow => {
                tracing::debug!(
                    "zsh daemon: response buffer exceeded {MAX_RESPONSE_BYTES} bytes, treating as desync"
                );
                self.mark_dead_and_kill();
                None
            }
            FramedRead::Timeout => {
                self.register_timeout_and_maybe_kill();
                None
            }
        }
    }

    /// [`request`](Self::request) 冒頭で前回タイムアウト分の残留フレームを
    /// 排水する（Fix D2）。`timeout` 予算内でセンチネル2個の対を読み切れ
    /// れば `pending_frame` を降ろして `true` を返す（読み取った内容自体は
    /// 破棄する——この Tab のリクエストに対応する応答ではないため）。
    /// 読み切れなければ（タイムアウト/オーバーフローいずれも）サーキット
    /// ブレーカーのカウンタを進め、`pending_frame` は立てたままにして
    /// （まだ排水できていないため）`false` を返す。
    fn drain_pending_frame(&mut self, timeout: Duration) -> bool {
        match self.read_framed_response(timeout) {
            FramedRead::Frame(_) => {
                self.pending_frame = false;
                // 排水成功はハング検知の観点では「フレームが取れた」ことに
                // 変わりないため、カウンタをリセットする（このフレーム自体は
                // 直前のリクエストに対する遅延応答であり、デーモン自体は
                // 生きて正常に動いている証拠のため）。
                self.consecutive_timeouts = 0;
                true
            }
            FramedRead::BufferOverflow => {
                tracing::debug!(
                    "zsh daemon: response buffer exceeded {MAX_RESPONSE_BYTES} bytes while \
                     draining a pending frame, treating as desync"
                );
                self.mark_dead_and_kill();
                false
            }
            FramedRead::Timeout => {
                self.register_timeout_and_maybe_kill();
                false
            }
        }
    }

    /// クリーンなタイムアウト（ドレイン中・通常リクエスト中いずれも）を
    /// 1回記録し、[`MAX_CONSECUTIVE_TIMEOUTS`] に達していればデーモンを
    /// ハングと判定して kill する（Fix D2 サーキットブレーカー）。
    /// 達していなければ `pending_frame` を立てて次回に排水を持ち越す。
    fn register_timeout_and_maybe_kill(&mut self) {
        self.consecutive_timeouts = self.consecutive_timeouts.saturating_add(1);
        if self.consecutive_timeouts >= MAX_CONSECUTIVE_TIMEOUTS {
            tracing::debug!(
                "zsh daemon: {} consecutive timeouts, treating as hung and killing",
                self.consecutive_timeouts
            );
            self.mark_dead_and_kill();
        } else {
            self.pending_frame = true;
        }
    }

    /// PTY master から `timeout` 予算内でセンチネル2個に挟まれた1フレーム分
    /// を読み取る（[`request`](Self::request) / [`drain_pending_frame`]
    /// 共通のフレーミングロジック、Fix D2 で切り出し）。バッファ上限
    /// （[`MAX_RESPONSE_BYTES`]、B4）超過はタイムアウトより優先して検知する。
    ///
    /// # 呼び出しをまたぐ状態の持ち越し（Fix D2、[`PartialRead`] 参照）
    /// 読み取り状態（`buf`/`toggles`/`frame_start`）は `self.partial_read`
    /// に保持し、呼び出しごとにリセットしない。開始センチネルが前回の
    /// 呼び出し（元のリクエスト）内で既に届いていた場合、この呼び出しは
    /// 「終了センチネルだけを待てばよい」状態から再開する——`toggles == 1`
    /// のまま呼ばれることも正常なケースであり、その場合でも新しく読めた
    /// バイト内で NUL が1個見つかれば `toggles` が2に達してフレーム完成と
    /// 判定できる。フレームが完成した時点（`toggles == 2`）で
    /// [`PartialRead::reset`] を呼び、次の論理フレーム用にまっさらな状態へ
    /// 戻す（バッファ上限超過時も同様——desync 扱いで kill されるため
    /// どのみち次のフレームは存在しないが、防御的にリセットしておく）。
    fn read_framed_response(&mut self, timeout: Duration) -> FramedRead {
        let deadline = Instant::now() + timeout;

        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let step = remaining.min(Duration::from_millis(200));
            match read_available(&mut self.master, step) {
                Some(chunk) if !chunk.is_empty() => {
                    let base = self.partial_read.buf.len();
                    self.partial_read.buf.extend_from_slice(&chunk);
                    // 新しく読めたバイト範囲内で NUL をスキャンする。
                    let mut idx = base;
                    while idx < self.partial_read.buf.len() {
                        if self.partial_read.buf[idx] == SENTINEL_BYTE {
                            self.partial_read.toggles += 1;
                            if self.partial_read.toggles == 1 {
                                self.partial_read.frame_start = Some(idx + 1);
                            } else if self.partial_read.toggles == 2 {
                                let start = self
                                    .partial_read
                                    .frame_start
                                    .expect("toggles==2 implies frame_start was set at toggle 1");
                                let frame =
                                    String::from_utf8_lossy(&self.partial_read.buf[start..idx])
                                        .into_owned();
                                self.partial_read.reset();
                                return FramedRead::Frame(frame);
                            }
                        }
                        idx += 1;
                    }
                    // B4: 上限超過はプロトコル desync 相当として扱い、
                    // タイムアウトを待たず即座に打ち切る。
                    if self.partial_read.buf.len() > MAX_RESPONSE_BYTES {
                        self.partial_read.reset();
                        return FramedRead::BufferOverflow;
                    }
                }
                _ => continue,
            }
        }

        FramedRead::Timeout
    }

    /// 子プロセスとその子孫ツリーの kill + reap + 一時ファイル削除を
    /// **バックグラウンドスレッドへ委譲**し、`alive` を `false` にする
    /// （B1/B2, #89）。
    ///
    /// 以前の実装は `kill_tree` 呼び出し後、`try_wait()` を最大 40 回
    /// （25ms 間隔 = 最大 1000ms）呼び出し元スレッド上でポーリングして
    /// おり、`request()` のタイムアウト/desync 直後にこの処理が挟まると
    /// UI スレッド（reedline の completer 呼び出し元）が最大 1 秒近く
    /// 追加でフリーズしていた（実測: 500ms タイムアウト設定に対し合計
    /// 2.86 秒）。この関数は代わりに `child` と `init_script_path` の
    /// 所有権を [`ReapBundle`] として切り出し、`std::thread::spawn` で
    /// 起こした detached なバックグラウンドスレッドに丸ごと渡す。
    /// 呼び出し元スレッドは所有権移譲のコストのみを払い、即座に戻る。
    ///
    /// 子孫プロセスは「バックグラウンドスレッドがいずれ確実に reap する」
    /// ことが保証されればよく（テストでは ESRCH ポーリングで検証する）、
    /// 呼び出し元がそれを待つ必要はない、というのが本 Fix の核心。
    fn mark_dead_and_kill(&mut self) {
        if !self.alive {
            return;
        }
        self.alive = false;
        let (Some(child), Some(init_script_path)) =
            (self.child.take(), self.init_script_path.take())
        else {
            // 既に所有権が移譲済み（二重 shutdown 等）。alive は既に false
            // だったはずなので通常はここに来ないが、安全側の no-op とする。
            return;
        };
        let bundle = ReapBundle {
            child,
            init_script_path,
        };
        std::thread::spawn(move || {
            reap_bundle(bundle, Instant::now() + Duration::from_secs(1));
        });
    }

    /// デーモンを明示的に終了させる（`Drop` から呼ばれる既定の冪等操作）。
    ///
    /// [`mark_dead_and_kill`](Self::mark_dead_and_kill) と同じくバック
    /// グラウンド委譲でノンブロッキング（B1/B2）。呼び出し元スレッドが
    /// kill/reap の完了を待つ必要がある場合（プロセス終了直前の決定的な
    /// shutdown）は [`shutdown_blocking`](Self::shutdown_blocking) を使う。
    pub(crate) fn shutdown(&mut self) {
        self.mark_dead_and_kill();
    }

    /// デーモンを終了させ、`deadline` の範囲内で kill/reap の完了を
    /// **呼び出し元スレッド上で**待つ有界同期版（B1/B2, #89）。
    ///
    /// UI スレッド（reedline の completer 呼び出し元）から呼んではならない
    /// — 通常経路は常に非ブロッキングな [`shutdown`](Self::shutdown) を
    /// 使うこと。この変種は「プロセスがまもなく終了/置換される」ため
    /// バックグラウンドスレッドに委ねても reap される保証がない経路
    /// （`Command::exec` 直前・`std::process::exit` 直前 — Fix A, ce53dfd
    /// が landed させた exit/exec shutdown 経路）専用。
    pub(crate) fn shutdown_blocking(&mut self, deadline: Duration) {
        if !self.alive {
            return;
        }
        self.alive = false;
        let (Some(child), Some(init_script_path)) =
            (self.child.take(), self.init_script_path.take())
        else {
            return;
        };
        let bundle = ReapBundle {
            child,
            init_script_path,
        };
        reap_bundle(bundle, Instant::now() + deadline);
    }
}

impl Drop for ZshDaemon {
    fn drop(&mut self) {
        // 通常経路は非ブロッキング shutdown（B1/B2）。`ZshDaemon` を保持する
        // 側（`DaemonSlot`）は、プロセス終了直前など有界同期待ちが必要な
        // 経路では明示的に `shutdown_blocking` を先に呼んでから drop する
        // ことで、この Drop は既に `alive == false` かつ所有権移譲済みの
        // no-op として通過する。
        self.shutdown();
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
    disable_echo(&pty.slave);
    let master_file = fs::File::from(pty.master);
    Ok((master_file, pty.slave))
}

/// PTY slave の line discipline から ECHO を無効化する（B5, #89）。
///
/// デフォルトでは PTY の line discipline が slave 側への書き込みをそのまま
/// 読み取り側へもエコーバックする。[`ZshDaemon::request`] が `^U` + 補完行 +
/// `^I` を書き込むと、この設定のままではセンチネル探索前にエコーされた
/// 送信ペイロード自体が読み取りストリームに混入する（実機検証済み）。
/// これまでフレーミングは最初のセンチネルバイト以降だけを対象にするため
/// 実害は出ていなかったが、送信内容が応答ストリームへ紛れ込むこと自体が
/// プロトコルとして脆い（センチネルより前に偶然 NUL 相当のバイト列が
/// 来る等の将来的な desync リスク）ため、`engine/pty.rs::disable_opost` と
/// 同じ `nix::sys::termios` 経由のパターンで ECHO を明示的に切る。
///
/// `tcgetattr`/`tcsetattr` が失敗した場合（一部のプラットフォーム/権限
/// 制約）はベストエフォートで諦め、従来どおりエコー有効のまま動作を続ける
/// （`engine/pty.rs::disable_opost` と同じ縮退方針 — フレーミングは
/// センチネル起点のため機能的には壊れない）。
fn disable_echo(slave_fd: &OwnedFd) {
    let fd = slave_fd.as_fd();
    if let Ok(mut attrs) = termios::tcgetattr(fd) {
        attrs.local_flags.remove(LocalFlags::ECHO);
        // ECHOE/ECHOK/ECHONL は ECHO 前提の派生エコー（消去・改行時の
        // 見た目調整）のため、ECHO 自体を切るなら道連れで無効化しておく
        // （残しても ECHO なしでは実害が出ないが、意図を明示するため）。
        attrs.local_flags.remove(LocalFlags::ECHOE);
        attrs.local_flags.remove(LocalFlags::ECHOK);
        attrs.local_flags.remove(LocalFlags::ECHONL);
        let _ = termios::tcsetattr(fd, SetArg::TCSANOW, &attrs);
    }
}

/// [`DAEMON_INIT_SCRIPT`] を `bridge_dir` 配下の専用一時ファイルへ書き出す。
///
/// プロセス pid + ランダムな 64bit 値を混ぜたファイル名にすることで、
/// 同一ホストで複数の jarvish セッションが同時にデーモンを spawn しても
/// 衝突しない（pid だけでは "確実に予測できるファイル名" になってしまい
/// C1 の症状そのものになるため、ランダム成分が本質的に必要）。
///
/// # シンボリックリンク防御（C1, #89）
/// 以前の実装は `fs::write` を使っており、パスが予測可能（`bridge_dir` は
/// 固定パス `~/.config/jarvish/zsh-bridge/`、ファイル名は pid のみで決まる）
/// なうえ `fs::write` はシンボリックリンクをそのままたどって書き込む
/// （`O_CREAT` のみで `O_EXCL` を指定しない標準の `open` 相当）。攻撃者が
/// 対象 pid を予測（または広く先回りして複数 pid 分）してこのパスへの
/// シンボリックリンクを事前に仕込んでおくと、jarvish が生成する init
/// スクリプト（zsh 実行内容）がリンク先の任意ファイルへ書き込まれてしまう
/// （`ensure_bridge_zshrc` に対する既存の symlink 攻撃対策と同種の脅威）。
///
/// これを防ぐため、`OpenOptions::create_new(true)`（`O_CREAT | O_EXCL`
/// 相当 — 既存のファイル/シンボリックリンクが対象パスに**何であれ**存在
/// する場合は常にエラーになり、シンボリックリンクを一切たどらない）で
/// 開き、パーミッションも `0o600`（所有者のみ読み書き可）に絞る。
/// ランダム成分によりファイル名衝突自体がほぼ起こらないため、
/// `create_new` が失敗するのは実質的に「攻撃者が事前に何かを仕込んでいた」
/// ケースのみであり、その場合は即座に `Err` を返して呼び出し元
/// （[`ZshDaemon::spawn`]）に degrade（このタブでは補完デーモン利用不可）
/// させる。
fn write_init_script(bridge_dir: &Path) -> io::Result<PathBuf> {
    fs::create_dir_all(bridge_dir)?;
    let path = bridge_dir.join(format!(
        ".daemon_init.{}.{:016x}.zsh",
        std::process::id(),
        rand::random::<u64>()
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    file.write_all(DAEMON_INIT_SCRIPT.as_bytes())?;
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

    /// pid が実際に ESRCH になる（プロセスが死んでいる）まで短時間・有界
    /// 回数ポーリングする（S5 修正: テストフィクスチャ teardown 用共通
    /// ヘルパー、`zsh_bridge.rs` / `shell/mod.rs` の同名パターンと同じ
    /// 考え方）。
    ///
    /// `mark_dead_and_kill`（バッファ超過・サーキットブレーカー等の内部
    /// 経路）や `shutdown()`（非ブロッキング）は kill/reap をバックグラウンド
    /// スレッドへ委譲するため、テスト関数がこれらを呼んだ直後に単に
    /// スコープを抜けると、まだ子プロセスが生きたまま `cargo test` の
    /// テストバイナリプロセスが終了しうる（単体実行で観測される孤児
    /// `/bin/zsh -i` の根本原因）。テストは必ずこのヘルパーで実際の死亡を
    /// 確認してから終わること。
    fn wait_for_pid_death_for_test(pid: u32) -> bool {
        for _ in 0..80 {
            let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
            if ret == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
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
        // テストフィクスチャ teardown（S5 修正）: `shutdown()`（非ブロッキング、
        // kill/reap をバックグラウンドスレッドへ委譲）はテストプロセスの
        // 終了と競合しうる ── 単体テスト実行（1テストのみ）だとテスト
        // 関数を抜けた直後にテストバイナリごと終了し、バックグラウンド
        // スレッドの `kill_tree` が実行される前にプロセスが道連れで消える
        // ことがある（実機計測で確認: `cargo test --lib` 1回の実行で
        // ppid=1 の孤児 `/bin/zsh -i` が複数残った根本原因）。テストでは
        // 常に有界同期版 `shutdown_blocking` を使い、関数が戻った時点で
        // 子プロセスの kill/reap が完了していることを保証する。
        daemon.shutdown_blocking(Duration::from_secs(2));
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

        let child_pid_before = daemon.child_pid_for_test();

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
            daemon.child_pid_for_test(),
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

        // テストフィクスチャ teardown（S5 修正）: Drop に任せず明示的に
        // 有界同期 shutdown する（`spawn_reaches_ready_marker` のコメント
        // 参照）。
        daemon.shutdown_blocking(Duration::from_secs(2));
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

        // テストフィクスチャ teardown（S5 修正、`spawn_reaches_ready_marker`
        // のコメント参照）。
        daemon.shutdown_blocking(Duration::from_secs(2));
    }

    #[test]
    #[serial]
    fn hung_completion_first_timeout_stays_alive_second_kills_descendants() {
        // Fix D2 サーキットブレーカー: `sleep 30` の完全ハング補完関数に
        // 対して、1回目のタイムアウトではまだデーモンを殺さず（グレース）、
        // 同じハング状態が続く2回目のリクエスト（= 1回目の残留フレームの
        // ドレインがタイムアウトし、それ自体が「2回連続」の2回目としてカウント
        // される）で初めてハングと判定してデーモンを kill することを検証する。
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

        let child_pid = daemon.child_pid_for_test();

        let request_timeout = Duration::from_millis(500);

        // 1回目: タイムアウトするが、グレースによりまだ生きている。
        let start1 = Instant::now();
        let result1 = daemon.request("jarvishtesthang ", request_timeout);
        let elapsed1 = start1.elapsed();
        assert_eq!(result1, None, "hung completion should time out to None");
        assert!(
            elapsed1 < request_timeout + Duration::from_millis(250),
            "request() must return within timeout + small epsilon, took {elapsed1:?}"
        );
        assert!(
            daemon.is_alive(),
            "daemon must stay alive after a single (first) timeout (Fix D2 grace)"
        );

        // 2回目: 残留フレームのドレインがタイムアウトし、これが「2回連続」の
        // 2回目としてカウントされ、サーキットブレーカーが作動する。
        let start2 = Instant::now();
        let result2 = daemon.request("jarvishtesthang ", request_timeout);
        let elapsed2 = start2.elapsed();
        assert_eq!(result2, None);
        // B1: kill_tree + reap は request() のタイムアウト/desync 経路から
        // バックグラウンドスレッドへ委譲されるようになったため、
        // request() 自体は「タイムアウト値 + 小さな epsilon」以内に戻る
        // はず（以前は kill_tree + 40x25ms 有界ポーリングがこの呼び出し元
        // スレッド上でインラインに走り、最大 ~1 秒余計にブロックしていた
        // — 実測 2.86 秒 vs 500ms タイムアウト。この下限を厳しくすること
        // 自体が「reap を呼び出し元スレッドから追い出せた」ことの直接証拠）。
        assert!(
            elapsed2 < request_timeout + Duration::from_millis(250),
            "request() must return within timeout + small epsilon (reap must not block \
             the caller thread), timeout={request_timeout:?}, took {elapsed2:?}"
        );
        assert!(
            !daemon.is_alive(),
            "daemon must be marked dead after 2 consecutive timeouts (circuit breaker)"
        );

        // 子プロセス（と、可能なら子孫）は request() が戻った後も
        // バックグラウンドスレッドによっていずれ確実に reap される
        // ことを、寛容な時間幅の ESRCH ポーリングで確認する
        // （external.rs のテストと同じ考え方 — ただし今回は呼び出し元
        // スレッドをブロックしないことが主張の核心なので、ポーリング自体は
        // request() が返った**後**に行う）。
        let mut alive = true;
        for _ in 0..80 {
            let ret = unsafe { libc::kill(child_pid as libc::pid_t, 0) };
            if ret == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                alive = false;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            !alive,
            "child pid {child_pid} should eventually be dead after background reap"
        );
    }

    #[test]
    #[serial]
    fn grace_drain_recovers_slow_first_call_and_serves_second_request_correctly() {
        // Fix D2 の核心保証: 1回目の呼び出しだけ遅く（テスト用タイムアウトを
        // 超えて）、2回目以降は速く応答する補完関数フィクスチャで、
        // 1回目は None（グレースで daemon は生存継続）、2回目は 1回目の
        // 残留フレームをドレインしたうえで、正しい候補（2回目のリクエスト
        // に対応するもの）を返すことを検証する。
        //
        // フィクスチャ: マーカーファイルの有無で初回/以降を判別する
        // （プロセスをまたいで状態を持たせる必要があるため、zsh の変数では
        // なくファイルシステムを使う——1回目の呼び出しで sleep してから
        // マーカーを作り、2回目以降はマーカーがあるので即座に応答する）。
        let Some(zsh) = zsh_binary() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        let tmpdir = tempfile::tempdir().unwrap();
        let marker = tmpdir.path().join("first-call-done");
        let fixture = setup_fixture(&[(
            "_jarvishtestslow",
            &format!(
                "#compdef jarvishtestslow\n\
                 if [[ ! -f {marker} ]]; then\n\
                 touch {marker}\n\
                 sleep 2\n\
                 fi\n\
                 compadd -- fastcandidate\n",
                marker = marker.display()
            ),
        )]);

        let mut daemon = ZshDaemon::spawn(
            &zsh,
            &fixture.zdotdir,
            &extra_envs_for(&fixture),
            Duration::from_secs(10),
        )
        .expect("daemon should spawn");
        let pid_before = daemon.child_pid_for_test();

        // テスト用の短いウォームタイムアウト（1回目の sleep 2 秒より短い）。
        let short_timeout = Duration::from_millis(400);

        let first = daemon.request("jarvishtestslow ", short_timeout);
        assert_eq!(
            first, None,
            "first (slow) call should yield None under the short test timeout"
        );
        assert!(
            daemon.is_alive(),
            "daemon must stay alive after the first timeout (grace)"
        );
        assert_eq!(
            daemon.child_pid_for_test(),
            pid_before,
            "grace must not respawn the daemon"
        );

        // 2回目: 残留フレームをドレインしたうえで、今回のリクエストの
        // 応答（fastcandidate）を正しく返す。ドレイン + 新リクエストの
        // 両方を賄うのに十分な予算を与える。
        let second = daemon.request("jarvishtestslow ", Duration::from_secs(5));
        let candidates = second.expect("second request should recover and return candidates");
        let values = parse_capture_output(&candidates);
        assert!(
            values.iter().any(|c| c.value == "fastcandidate"),
            "second request should see its own response, got {values:?}"
        );
        assert!(
            daemon.is_alive(),
            "daemon must still be alive after a successful drain + request"
        );
        assert_eq!(
            daemon.child_pid_for_test(),
            pid_before,
            "the same daemon process must still be serving requests (no respawn)"
        );

        // テストフィクスチャ teardown（S5 修正、`spawn_reaches_ready_marker`
        // のコメント参照）。
        daemon.shutdown_blocking(Duration::from_secs(2));
    }

    #[test]
    #[serial]
    fn success_between_timeouts_resets_consecutive_counter() {
        // Fix D2: timeout → success → timeout という並びでは、途中の success
        // がカウンタをリセットするため、2回目の timeout だけではサーキット
        // ブレーカーは作動せず、デーモンは生きたままである。
        let Some(zsh) = zsh_binary() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        let fixture = setup_fixture(&[
            (
                "_jarvishtesthang",
                "#compdef jarvishtesthang\nsleep 30\ncompadd -- neverseen\n",
            ),
            (
                "_jarvishtestcmd",
                "#compdef jarvishtestcmd\ncompadd -- alpha beta gamma\n",
            ),
        ]);

        let mut daemon = ZshDaemon::spawn(
            &zsh,
            &fixture.zdotdir,
            &extra_envs_for(&fixture),
            Duration::from_secs(10),
        )
        .expect("daemon should spawn");
        let pid_before = daemon.child_pid_for_test();

        let request_timeout = Duration::from_millis(500);

        // 1回目: timeout (グレース、まだ生存)。
        let r1 = daemon.request("jarvishtesthang ", request_timeout);
        assert_eq!(r1, None);
        assert!(daemon.is_alive());

        // 2回目: 別の（ハングしない）コマンドへのリクエスト。まず1回目の
        // 残留フレームをドレインする必要があるが、`sleep 30` はまだ動作中
        // なのでこのドレイン自体もタイムアウトしうる——このテストの主張は
        // 「ドレインが失敗してもリクエスト自体は諦めて None を返すこと」
        // ではなく、**ドレインさえ間に合えば**カウンタがリセットされる
        // ことなので、ドレイン予算に十分な余裕を与える（`sleep 30` の残り
        // 時間を上回るテスト用タイムアウト）ことで確実にドレインを成功させる。
        let r2 = daemon.request("jarvishtestcmd ", Duration::from_secs(35));
        let candidates =
            r2.expect("second request should succeed once the stale hang frame is drained");
        let values = parse_capture_output(&candidates);
        assert!(values.iter().any(|c| c.value == "alpha"));
        assert!(
            daemon.is_alive(),
            "daemon must be alive after a successful request"
        );

        // 3回目: 再びハングするコマンド。timeout が起きるが、直前の success
        // がカウンタをリセットしているため、これは「連続1回目」に過ぎず、
        // まだ kill されない。
        let r3 = daemon.request("jarvishtesthang ", request_timeout);
        assert_eq!(r3, None);
        assert!(
            daemon.is_alive(),
            "daemon must stay alive: the counter was reset by the success in between"
        );
        assert_eq!(
            daemon.child_pid_for_test(),
            pid_before,
            "no respawn should have happened throughout"
        );

        // テストフィクスチャ teardown（S5 修正、`spawn_reaches_ready_marker`
        // のコメント参照）。デーモンはまだ生存中（pending_frame が残った
        // 状態）のため、明示的な有界同期 shutdown が必須。
        daemon.shutdown_blocking(Duration::from_secs(2));
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
        let child_pid = daemon.child_pid_for_test();

        // Drop 自体はバックグラウンド委譲でノンブロッキング（B1/B2）になった
        // ため、`drop()` 呼び出し自体の所要時間ではなく、その後の
        // バックグラウンドスレッドがいずれ確実に reap することを証明する
        // （elapsed の主張は不要 — 「drop() が速く戻ること」は
        // hung_completion_times_out_and_kills_descendants で別途担保済み）。
        drop(daemon);

        let mut alive = true;
        for _ in 0..80 {
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
        let script_path = daemon
            .init_script_path
            .clone()
            .expect("init_script_path should be Some while alive");
        assert!(script_path.exists());

        drop(daemon);

        // 一時ファイル削除もバックグラウンドスレッド側の reap_bundle が
        // 行うようになったため（B1/B2）、`drop()` が戻った直後の同期確認
        // ではなく短時間ポーリングで確認する。
        let mut removed = false;
        for _ in 0..80 {
            if !script_path.exists() {
                removed = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            removed,
            "init script temp file should eventually be removed after Drop"
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

        // テストフィクスチャ teardown（S5 修正）: このテストの主張（shutdown
        // 後の request() が即座に None を返すこと）自体は shutdown の
        // ブロッキング/非ブロッキングに依存しないため、有界同期版に置き換えて
        // 子プロセスの確実な reap を保証する（`spawn_reaches_ready_marker`
        // のコメント参照）。
        daemon.shutdown_blocking(Duration::from_secs(2));
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

    // ── B3: 書き込み前の安価な生存確認（外部要因での kill を高速検知） ──

    #[test]
    #[serial]
    fn request_after_external_sigkill_returns_none_fast_without_full_timeout() {
        // OOM killer や手動 `kill -9` のような外部要因でデーモン子プロセスが
        // 死んでいるケースを模す: テストから直接 SIGKILL を送ってから
        // request() を呼び、フルタイムアウトを待たず高速に None が返る
        // ことを確認する（B3 の核心保証）。
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

        let child_pid = daemon.child_pid_for_test();
        unsafe {
            libc::kill(child_pid as libc::pid_t, libc::SIGKILL);
        }
        // カーネルが実際に終了処理するまでの短い猶予（このポーリング自体は
        // テストのセットアップであり、request() の高速性の主張には数えない）。
        for _ in 0..40 {
            let ret = unsafe { libc::kill(child_pid as libc::pid_t, 0) };
            if ret == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }

        let start = Instant::now();
        let result = daemon.request("jarvishtestcmd ", Duration::from_secs(5));
        let elapsed = start.elapsed();

        assert_eq!(
            result, None,
            "request on an externally-killed daemon should yield None"
        );
        assert!(
            elapsed < Duration::from_millis(150),
            "liveness probe should detect external kill fast (< 150ms), took {elapsed:?} \
             (a full-timeout wait would indicate the try_wait() probe is missing)"
        );
        assert!(
            !daemon.is_alive(),
            "daemon must be marked dead after detecting the external kill"
        );
    }

    // ── B4: 応答バッファの上限（暴走補完でのメモリ増大対策） ──

    #[test]
    #[serial]
    fn oversized_response_is_capped_and_marks_daemon_dead() {
        // フィクスチャ: MAX_RESPONSE_BYTES を超える出力を吐き続ける（センチネル
        // を出さない）補完関数。実運用のバグ/悪意ある補完関数を模す。
        let Some(zsh) = zsh_binary() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        // シェル内で大量出力させる: 十分に長い1行を大量回数 echo する
        // （compadd を経由せず、daemon_init.zsh の compadd オーバーライドが
        // 介在しない生の PTY 出力で MAX_RESPONSE_BYTES 超過を直接再現する）。
        let fixture = setup_fixture(&[(
            "_jarvishtestflood",
            "#compdef jarvishtestflood\n\
             for i in {1..200000}; do print -n 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA'; done\n\
             compadd -- neverseen\n",
        )]);

        let mut daemon = ZshDaemon::spawn(
            &zsh,
            &fixture.zdotdir,
            &extra_envs_for(&fixture),
            Duration::from_secs(10),
        )
        .expect("daemon should spawn");
        let child_pid = daemon.child_pid_for_test();

        let start = Instant::now();
        // 上限判定はタイムアウトより先に効くはずなので、タイムアウト自体は
        // 余裕を持たせて「上限超過検知が先に効いた」ことを立証する。
        let result = daemon.request("jarvishtestflood ", Duration::from_secs(10));
        let elapsed = start.elapsed();

        assert_eq!(
            result, None,
            "oversized response must be treated as desync and yield None"
        );
        assert!(
            elapsed < Duration::from_secs(10),
            "buffer cap should trip well before the full 10s timeout, took {elapsed:?}"
        );
        assert!(
            !daemon.is_alive(),
            "daemon must be marked dead after exceeding the response buffer cap"
        );

        // テストフィクスチャ teardown（S5 修正）: バッファ上限超過は
        // `request()` 内部で `mark_dead_and_kill`（非ブロッキング、kill/reap
        // をバックグラウンドスレッドへ委譲）を経由するため、テスト関数を
        // 抜けた時点では reap が完了している保証がない（`spawn_reaches_
        // ready_marker` のコメント参照）。有界ポーリングで実際に子プロセスが
        // 死ぬまで待ってからテストを終える。
        assert!(
            wait_for_pid_death_for_test(child_pid),
            "child pid {child_pid} should eventually be reaped by the background thread"
        );
    }

    // ── B5: PTY ECHO が無効化されていること ──

    #[test]
    #[serial]
    fn echo_is_disabled_on_daemon_pty_slave() {
        // termios レベルで直接、ECHO が実際にオフになっていることを検証する
        // （B5 の核心保証）。`daemon_init.zsh` の `zsh -i` は ZLE
        // （zsh のインタラクティブ行編集システム）を使っており、ZLE は
        // 端末の ECHO フラグとは独立に、入力バッファの再描画を自前で
        // 常に行う（実機検証済み: `tcsetattr` で ECHO を明示的に消しても
        // `zle complete-word` を経由した書き込みは ZLE 自身の再描画により
        // 引き続き画面に現れる — これは zsh の仕様であり termios では
        // 抑止できない）。そのため「応答ストリームに送信行が一切現れない」
        // ことをこのテストの主張にはできない（別テスト
        // `echo_off_reduces_duplicate_marker_occurrences` が、ECHO を切る
        // ことで**カーネル側の生エコーによる重複**が消えることを検証する）。
        // ここでは fix 自体が適用されていること — `disable_echo` が
        // slave fd の termios ECHO ビットを実際に落としていること — を
        // 直接 `tcgetattr` で確認する。
        use nix::sys::termios::LocalFlags;

        let Some(zsh) = zsh_binary() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        let fixture = setup_fixture(&[(
            "_jarvishtestcmd",
            "#compdef jarvishtestcmd\ncompadd -- alpha\n",
        )]);

        let (master, slave) = create_daemon_pty().expect("daemon pty should be created");
        let attrs = termios::tcgetattr(slave.as_fd()).expect("tcgetattr should succeed");
        assert!(
            !attrs.local_flags.contains(LocalFlags::ECHO),
            "ECHO must be cleared on the daemon PTY slave immediately after create_daemon_pty()"
        );
        drop(master);
        drop(slave);

        // 実際に spawn() 経由で組み立てたデーモンでも同じ保証が効くことを
        // 一応 end-to-end で確認しておく（daemon が生きて壊れていないこと
        // 自体の回帰チェックも兼ねる）。
        let mut daemon = ZshDaemon::spawn(
            &zsh,
            &fixture.zdotdir,
            &extra_envs_for(&fixture),
            Duration::from_secs(10),
        )
        .expect("daemon should spawn");
        let result = daemon.request("jarvishtestcmd ", Duration::from_secs(3));
        assert!(result.is_some(), "daemon should still serve completions");
        // テストフィクスチャ teardown（S5 修正、`spawn_reaches_ready_marker`
        // のコメント参照）。
        daemon.shutdown_blocking(Duration::from_secs(2));
    }

    #[test]
    #[serial]
    fn echo_off_reduces_duplicate_marker_occurrences_vs_echo_on() {
        // B5 の実測可能な保証: ECHO を切ると、送信ペイロードのカーネル側
        // 生エコー（tty line discipline による即時反響）は消える。ZLE 自身
        // の再描画は ECHO 設定に関わらず残る（上のテストのコメント参照）
        // ため、"0 回" を主張することはできないが、"ECHO オフ時の出現回数は
        // ECHO オン時より厳密に少ない" ことは実機で決定的に検証できる
        // （実機検証: ECHO オンで2回、オフで1回 — カーネル生エコー分だけ
        // 減る）。ここでは `create_daemon_pty`（ECHO オフ適用済み）と、
        // ECHO をあえて有効化し直した比較用 PTY の両方で同じマーカーを
        // 送り込み、出現回数を比較する。
        let Some(zsh) = zsh_binary() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        let fixture = setup_fixture(&[(
            "_jarvishtestcmd",
            "#compdef jarvishtestcmd\ncompadd -- alpha\n",
        )]);

        let echo_off_count = capture_marker_occurrences(&zsh, &fixture, false);
        let echo_on_count = capture_marker_occurrences(&zsh, &fixture, true);

        eprintln!(
            "echo_off marker occurrences: {echo_off_count}, echo_on marker occurrences: {echo_on_count}"
        );
        assert!(
            echo_off_count < echo_on_count,
            "disabling ECHO must strictly reduce marker duplication in the raw PTY stream: \
             echo_off={echo_off_count}, echo_on={echo_on_count}"
        );
    }

    /// [`echo_off_reduces_duplicate_marker_occurrences_vs_echo_on`] 専用の
    /// ヘルパー: `zsh -i` を直接 spawn し（`ZshDaemon::spawn` は常に ECHO を
    /// オフにするため使えない）、`force_echo_on` の指示に応じて PTY slave
    /// の ECHO を制御したうえでユニークマーカーを含む行を送り込み、応答
    /// ストリーム中のマーカー出現回数を返す。
    fn capture_marker_occurrences(zsh: &Path, fixture: &TestFixture, force_echo_on: bool) -> usize {
        use nix::sys::termios::{LocalFlags, SetArg};

        let (mut master, slave) = create_daemon_pty().expect("daemon pty should be created");
        if force_echo_on {
            let mut attrs = termios::tcgetattr(slave.as_fd()).expect("tcgetattr should succeed");
            attrs.local_flags.insert(LocalFlags::ECHO);
            termios::tcsetattr(slave.as_fd(), SetArg::TCSANOW, &attrs)
                .expect("tcsetattr should succeed");
        }

        let slave_raw_fd = slave.as_raw_fd();
        let stdin_fd = unsafe { libc::dup(slave_raw_fd) };
        let stdout_fd = unsafe { libc::dup(slave_raw_fd) };
        let stderr_fd = unsafe { libc::dup(slave_raw_fd) };

        let mut command = Command::new(zsh);
        command
            .arg("-i")
            .env("ZDOTDIR", &fixture.zdotdir)
            .env("TERM", "dumb")
            .envs(
                extra_envs_for(fixture)
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_str())),
            )
            .stdin(unsafe { Stdio::from_raw_fd(stdin_fd) })
            .stdout(unsafe { Stdio::from_raw_fd(stdout_fd) })
            .stderr(unsafe { Stdio::from_raw_fd(stderr_fd) });
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
        let mut child = command.spawn().expect("zsh -i should spawn");
        drop(slave);

        // シェルが起動しプロンプトを出すまで少し待つ（正確な待ち方をせず
        // 固定 sleep なのは、このヘルパーが比較専用の低リスクなテスト
        // ユーティリティであり、多少の余裕時間で十分なため）。
        std::thread::sleep(Duration::from_millis(800));
        let payload = b"\x15echo uniqechomarkerXYZ\t";
        master
            .write_all(payload)
            .expect("write to master should succeed");
        std::thread::sleep(Duration::from_millis(800));

        let mut raw = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            match read_available(&mut master, Duration::from_millis(200)) {
                Some(chunk) if !chunk.is_empty() => raw.extend_from_slice(&chunk),
                _ => break,
            }
        }

        let _ = child.kill();
        let _ = child.wait();

        let raw_text = String::from_utf8_lossy(&raw);
        raw_text.matches("uniqechomarkerXYZ").count()
    }

    // ── shutdown_blocking: exit/exec 経路専用の有界同期 shutdown ──

    #[test]
    #[serial]
    fn shutdown_blocking_reaps_within_deadline_and_kills_child() {
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
        let child_pid = daemon.child_pid_for_test();

        daemon.shutdown_blocking(Duration::from_secs(2));

        assert!(!daemon.is_alive());
        // 有界同期版なので、戻ってきた時点で reap が完了している（または
        // deadline に達している）はず — ポーリングなしで即座に ESRCH を
        // 確認できることが非同期版との違いの直接証拠。
        let ret = unsafe { libc::kill(child_pid as libc::pid_t, 0) };
        let is_dead = ret == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        assert!(
            is_dead,
            "child pid {child_pid} should already be reaped when shutdown_blocking returns"
        );
    }

    // ── write_init_script: シンボリックリンク防御 (C1, #89) ──
    //
    // 実 zsh を一切必要としない純粋なファイルシステムテスト。ランダムな
    // ファイル名成分のおかげでテスト同士が衝突しないため #[serial] も不要。

    #[test]
    fn write_init_script_refuses_preexisting_symlink_at_target_pattern() {
        // 攻撃者が「pid + 総当たりのランダム値」のパターンへ事前にシンボ
        // リックリンクを仕込んでおいたケースを直接は再現できない
        // （ランダム値は予測不能なため）が、`create_new` が「対象パスに
        // 何であれ既に存在する（シンボリックリンクを含む）場合は常に拒否
        // する」という契約そのものを検証する: 実際に write_init_script が
        // 生成するのと同じ命名規則のパスへ事前にシンボリックリンクを
        // 置いておき、その特定のパスへの書き込みを試みても symlink を
        // たどらず失敗することを、write_init_script が内部で使う
        // OpenOptions::create_new のセマンティクスとして直接確認する。
        let tmpdir = tempfile::tempdir().unwrap();
        let bridge_dir = tmpdir.path().join("zsh-bridge");
        fs::create_dir_all(&bridge_dir).unwrap();

        // write_init_script と同じ命名パターンで、攻撃者が用意しうる
        // シンボリックリンクを再現する。
        let evil_target = tmpdir.path().join("evil-target.zsh");
        let victim_path = bridge_dir.join(format!(
            ".daemon_init.{}.{:016x}.zsh",
            std::process::id(),
            0u64
        ));
        std::os::unix::fs::symlink(&evil_target, &victim_path).unwrap();

        // create_new(true) は「対象パスに既に何か（シンボリックリンクを
        // 含む）が存在する」場合は常にエラーになる契約を直接検証する。
        let result = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&victim_path);
        assert!(
            result.is_err(),
            "create_new must refuse to open a path that is already a symlink"
        );
        assert!(
            !evil_target.exists(),
            "the symlink target must never be written through"
        );

        // write_init_script 自体は毎回ランダムなファイル名を生成するため、
        // 事前に置かれたこのシンボリックリンクとは衝突せず正常に成功する
        // （= 予測できないファイル名であること自体が防御の一部）。
        let path = write_init_script(&bridge_dir).expect("write_init_script should succeed");
        assert_ne!(
            path, victim_path,
            "write_init_script must not reuse the attacker-planted path"
        );
        assert!(!fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn write_init_script_refuses_preexisting_regular_file_then_succeeds_with_new_random_name() {
        let tmpdir = tempfile::tempdir().unwrap();
        let bridge_dir = tmpdir.path().join("zsh-bridge");
        fs::create_dir_all(&bridge_dir).unwrap();

        // 事前に通常ファイルを、write_init_script が今後生成しうる
        // ファイル名パターンへ置いておく。create_new(true) はシンボリック
        // リンクだけでなく既存の通常ファイルも同様に拒否する
        // （O_CREAT | O_EXCL のセマンティクス）ことを直接確認する。
        let preexisting = bridge_dir.join(format!(
            ".daemon_init.{}.{:016x}.zsh",
            std::process::id(),
            0u64
        ));
        fs::write(&preexisting, "pre-existing content").unwrap();

        let result = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&preexisting);
        assert!(
            result.is_err(),
            "create_new must refuse an already-existing regular file"
        );
        assert_eq!(
            fs::read_to_string(&preexisting).unwrap(),
            "pre-existing content",
            "the pre-existing file's content must not be overwritten"
        );

        // write_init_script 自体は、ランダム成分のおかげで上記の
        // 事前配置ファイルとは別名になり正常に成功する。
        let path = write_init_script(&bridge_dir).expect("write_init_script should succeed");
        assert_ne!(path, preexisting);
        assert_eq!(fs::read_to_string(&path).unwrap(), DAEMON_INIT_SCRIPT);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn write_init_script_creates_file_with_0600_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let tmpdir = tempfile::tempdir().unwrap();
        let bridge_dir = tmpdir.path().join("zsh-bridge");

        let path = write_init_script(&bridge_dir).expect("write_init_script should succeed");
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        // 下位 9 ビット（パーミッションビットのみ）を比較する。
        assert_eq!(
            mode & 0o777,
            0o600,
            "init script must be owner-read/write only (0600), got {:o}",
            mode & 0o777
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn write_init_script_two_calls_yield_distinct_paths_and_both_contain_the_script() {
        // 同一 pid 内で複数回呼んでも（ランダム成分により）パスが衝突
        // しないことを確認する（デーモン再 spawn のたびに呼ばれるため
        // 重要な性質）。
        let tmpdir = tempfile::tempdir().unwrap();
        let bridge_dir = tmpdir.path().join("zsh-bridge");

        let path1 = write_init_script(&bridge_dir).expect("first call should succeed");
        let path2 = write_init_script(&bridge_dir).expect("second call should succeed");
        assert_ne!(
            path1, path2,
            "two calls must not collide on the same filename"
        );
        assert_eq!(fs::read_to_string(&path1).unwrap(), DAEMON_INIT_SCRIPT);
        assert_eq!(fs::read_to_string(&path2).unwrap(), DAEMON_INIT_SCRIPT);
        let _ = fs::remove_file(&path1);
        let _ = fs::remove_file(&path2);
    }
}
