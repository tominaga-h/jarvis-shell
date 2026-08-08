use super::*;

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
    /// が呼ばれた後）のみ。`alive == true` の間は常に `Some`。
    pub(super) child: Option<Child>,
    pub(super) master: fs::File,
    /// PTY slave 側の fd。子プロセスの生存中は親側で保持しておく必要は
    /// ないが、`spawn` 完了まで（`command.spawn()` 呼び出しの直前まで）
    /// 生かしておく必要があるため一時変数として使う（構造体には残さない）。
    pub(super) alive: bool,
    /// init スクリプトを書き出した一時ファイル（`ZshDaemon` が生きている
    /// 間だけ存在すればよい — `TempPath` 相当を手動管理）。kill/reap の
    /// 所有権譲渡と同時にこのパスも移譲する（`None` になったら既に
    /// バックグラウンドスレッド or 呼び出し元が削除責任を持つ）。
    pub(super) init_script_path: Option<PathBuf>,
    /// 直前のリクエストがタイムアウトし、
    /// まだ完了していない応答フレームが PTY 側に残っている可能性がある
    /// （zsh 側は補完関数の実行を継続しており、いずれセンチネルに挟まれた
    /// 出力を送ってくる）ことを示すフラグ。`true` の間、次の `request()`
    /// 呼び出しは新しい行を送る**前**にこの残留フレームを読み飛ばす
    /// （[`drain_pending_frame`](Self::drain_pending_frame)）。
    pub(super) pending_frame: bool,
    /// [`read_framed_response`](Self::read_framed_response) の呼び出しを
    /// またいで持ち越す部分読み取り状態。
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
    pub(super) partial_read: PartialRead,
    /// 直近の「成功したフレーム読み取り」
    /// 以降に連続したタイムアウト回数。ドレイン自体のタイムアウトも
    /// 1回とカウントする。2 に達した時点でデーモンをハングとみなし
    /// kill する（[`mark_dead_and_kill`](Self::mark_dead_and_kill)）。
    /// 成功したフレーム読み取りが1回でもあればこのカウンタは 0 に戻る。
    pub(super) consecutive_timeouts: u8,
    /// デーモン（子プロセス `zsh -i`）が現在いる作業ディレクトリ。
    ///
    /// 子プロセスは spawn 時点の cwd を継承し、以後 jarvish 側の
    /// `std::env::set_current_dir`（`cd` ビルトイン）は**一切届かない**
    /// （プロセスごとに独立した cwd を持つため）。そのため補完関数
    /// （`_files` / `_path_files` 等）が相対パスを解決すると、ユーザーが
    /// 実際にいるディレクトリではなく jarvish の**起動時**ディレクトリの
    /// 中身を返してしまう。
    ///
    /// これを防ぐため、[`sync_cwd`](Self::sync_cwd) が各リクエストの直前に
    /// jarvish の現在の cwd と突き合わせ、食い違っていればデーモンのバッファ
    /// へ `cd` 行を送って追従させる。ここにはその「デーモン側が今いると
    /// 判っているディレクトリ」を記録する（spawn 直後は継承した cwd）。
    pub(super) cwd: Option<PathBuf>,
}

/// [`ZshDaemon`] がハングと判定してデーモンを kill するまでに許容する
/// 連続タイムアウト回数。
///
/// 「1回のタイムアウト」は遅いが正常な補完関数（例: インタプリタ起動を
/// 伴う `tmuxinator` 補完）でも普通に起こりうるため即座には殺さない。
/// 2回連続（= ドレインした上でなお次のリクエストもタイムアウトする、
/// または2回連続で素の要求がタイムアウトする）で初めて「本当にハングして
/// いる」とみなす。
pub(crate) const MAX_CONSECUTIVE_TIMEOUTS: u8 = 2;

/// [`ZshDaemon::sync_cwd`] 後に ZLE の再描画出力を読み捨てる予算。
///
/// `cd` の適用自体はローカルな `chdir(2)` で、補完関数の実行のような
/// 重い処理は伴わない（実測でミリ秒未満）。この予算は「再描画バイトが
/// PTY を通って戻ってくるまで」を賄えれば十分であり、長く取ると
/// ディレクトリ移動直後の Tab が無駄に待たされる。読めるデータが尽きた
/// 時点で早期に戻るため、通常はこの上限には達しない。
const CWD_SYNC_DRAIN: Duration = Duration::from_millis(120);

/// 文字列を zsh のシングルクォート文字列としてクォートする。
///
/// [`ZshDaemon::sync_cwd`] がディレクトリパスをデーモンのバッファへ送る際に
/// 使う。シングルクォート内では zsh は**一切の展開を行わない**（`$`、`` ` ``、
/// `~`、グロブ、バックスラッシュエスケープすべて無効）ため、含まれうる
/// 特殊文字を個別にエスケープする必要がない。唯一の例外がシングルクォート
/// 自身で、これは一度クォートを閉じ、バックスラッシュでエスケープした
/// `'` を置き、再びクォートを開く定番の `'\''` パターンで表現する
/// （POSIX sh / zsh 共通のイディオム）。
///
/// これにより、`It's a dir` や `$HOME`、`*` を名前に含むディレクトリでも
/// リテラルとして安全に渡せる（クォートを破って任意コマンドが実行される
/// 事故を防ぐ）。受け取り側の `jarvish-set-cwd` ウィジェットは `${(Q)BUFFER}`
/// でこのクォートを外す。
pub(crate) fn single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
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
            // 子プロセスは spawn 時点の jarvish の cwd をそのまま継承する。
            // 取得できなかった場合は `None` とし、最初の `sync_cwd` で
            // 必ず `cd` を送る（安全側 — 不明なまま放置しない）。
            cwd: env::current_dir().ok(),
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
    /// # グレースドレイン + サーキットブレーカー
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
    /// と応答バッファ上限超過（[`FramedRead::BufferOverflow`]）は
    /// 従来どおり**グレースの対象外**——即座に子プロセスとその子孫ツリーの
    /// kill/reap を**バックグラウンドスレッドへ委譲**し（呼び出し元
    /// スレッドはブロックしない）、`alive = false` に遷移して `None` を
    /// 返す。
    pub(crate) fn request(&mut self, line: &str, timeout: Duration) -> Option<String> {
        if !self.alive {
            return None;
        }

        // 書き込み前の安価な生存確認。外部要因（OOM killer、手動
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

        // 前回リクエストがタイムアウトして残留フレームがある場合、
        // 新しい行を送る前にまずそれを排水する。ドレイン自体が失敗した
        // 場合はサーキットブレーカーのカウンタを進めたうえで即座に
        // `None` を返す（新しいリクエストは送らない——2つのリクエストの
        // 応答が PTY 上で混ざる desync を避けるため）。
        if self.pending_frame && !self.drain_pending_frame(timeout) {
            return None;
        }

        // デーモンの cwd を jarvish の現在の cwd に追従させる（#cwd bug）。
        // 補完リクエストを送る**前**に行う必要がある — `_files` 等が相対
        // パスを解決するのはリクエスト処理中のため。
        self.sync_cwd();

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

    /// デーモン（子プロセス）の作業ディレクトリを jarvish の現在の cwd に
    /// 追従させる。
    ///
    /// # なぜ必要か
    ///
    /// デーモンは spawn 時の cwd を継承したまま**セッション中ずっと生き続ける**。
    /// jarvish 側の `cd`（`std::env::set_current_dir`）は自プロセスの cwd を
    /// 変えるだけで、既に走っている子プロセスには届かない。この同期が無いと
    /// `_files` / `_path_files` 等の補完関数が **jarvish の起動ディレクトリ**を
    /// 基準に相対パスを解決し、「今いるディレクトリに存在しないファイル」が
    /// 候補に出る（しかも `PathProvider` は provider チェーンの最後尾なので、
    /// デーモンが `Some` を返した時点で正しい `fs::read_dir` の結果は
    /// 握り潰される — `mod.rs` の `find_map` ディスパッチ参照）。
    ///
    /// # コスト
    ///
    /// cwd が前回と同じなら**何も送らない**（実際に `cd` するのはユーザーが
    /// ディレクトリを移動した直後の1リクエストだけ）。送る場合も応答を待つ
    /// フレームは無く、`^X` ウィジェットは同期的にバッファを消費するため、
    /// 直後の `^U` + 補完リクエストと衝突しない。デーモンを再 spawn する案
    /// （`cd` の度に compinit やり直し）と違い warm の利点を失わない。
    ///
    /// 書き込みに失敗した場合はデーモンを死亡扱いにする（以降の
    /// リクエストは `None`、次の Tab で遅延 respawn される）。
    /// cwd が取得できない場合は同期をスキップする（`cd` すべき先が
    /// 判らないため、誤ったディレクトリへ移動させるより現状維持が安全）。
    pub(super) fn sync_cwd(&mut self) {
        let Ok(current) = env::current_dir() else {
            return;
        };

        if self.cwd.as_deref() == Some(current.as_path()) {
            return;
        }

        // `^U` で残留バッファを消し、クォート済みパスを置いて `^G`
        // （`jarvish-set-cwd` ウィジェット）で適用する。`^X` ではなく `^G`
        // を使う理由は `daemon_init.zsh` のウィジェット定義のコメント参照
        // （`^X` は emacs キーマップのプレフィックスキーで、単体では発火しない）。
        let payload = format!("\x15{}\x07", single_quote(&current.to_string_lossy()));
        if self.master.write_all(payload.as_bytes()).is_err() {
            self.mark_dead_and_kill();
            return;
        }

        // ZLE はバッファを再描画するため、送った行が PTY 上にエコーバック
        // される（ECHO は termios で切ってあるが、これは端末エコーではなく
        // ZLE 自身の描画出力）。この残骸を読み捨てておかないと、直後の
        // 補完リクエストのフレーム読み取り（NUL トグル）に混入して
        // desync の原因になる。センチネルを伴わない出力なので、短い予算で
        // 「読めるだけ読む」だけでよい（フレーム待ちはしない）。
        self.drain_echo(CWD_SYNC_DRAIN);

        self.cwd = Some(current);
    }

    /// [`sync_cwd`](Self::sync_cwd) 後の ZLE 再描画出力を読み捨てる。
    ///
    /// フレーム（センチネル対）を待つ [`read_framed_response`] とは異なり、
    /// 「`budget` の間に届いたものを捨てる」だけ。読めるデータが尽きたら
    /// 早期に戻る。ここでのエラーやタイムアウトはデーモンの死とはみなさない
    /// （再描画が来ないこと自体は異常ではない）。
    fn drain_echo(&mut self, budget: Duration) {
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match read_available(&mut self.master, remaining) {
                // 何か読めた場合は捨てて、まだ続きがあるかもう一度試す。
                Some(chunk) if !chunk.is_empty() => {}
                // データ無し（予算切れ） / EOF / エラー: これ以上待たない。
                _ => return,
            }
        }
    }

    /// [`request`](Self::request) 冒頭で前回タイムアウト分の残留フレームを
    /// 排水する。`timeout` 予算内でセンチネル2個の対を読み切れ
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
    /// ハングと判定して kill する。
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
}
