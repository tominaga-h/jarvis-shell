use super::*;

/// 必要な所有権一式。
///
/// `mark_dead_and_kill` / `shutdown_blocking` はどちらも「`alive` を
/// `false` にし、この束を取り出してから実際の kill 処理へ渡す」という
/// 同じ手順を踏む。処理そのもの（`kill_tree` → 有界 `try_wait` ポーリング
/// → 一時ファイル削除）は [`reap_bundle`] に一本化し、呼び出し元
/// （バックグラウンドスレッド or 呼び出し元スレッド自身）が同期/非同期
/// どちらの文脈で呼ぶかだけを選べるようにする。
pub(crate) struct ReapBundle {
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
pub(crate) fn reap_bundle(bundle: ReapBundle, deadline: Instant) {
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
    /// **バックグラウンドスレッドへ委譲**し、`alive` を `false` にする。
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
    pub(super) fn mark_dead_and_kill(&mut self) {
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
    /// グラウンド委譲でノンブロッキング。呼び出し元スレッドが
    /// kill/reap の完了を待つ必要がある場合（プロセス終了直前の決定的な
    /// shutdown）は [`shutdown_blocking`](Self::shutdown_blocking) を使う。
    pub(crate) fn shutdown(&mut self) {
        self.mark_dead_and_kill();
    }

    /// デーモンを終了させ、`deadline` の範囲内で kill/reap の完了を
    /// **呼び出し元スレッド上で**待つ有界同期版。
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
        // 通常経路は非ブロッキング shutdown。`ZshDaemon` を保持する
        // 側（`DaemonSlot`）は、プロセス終了直前など有界同期待ちが必要な
        // 経路では明示的に `shutdown_blocking` を先に呼んでから drop する
        // ことで、この Drop は既に `alive == false` かつ所有権移譲済みの
        // no-op として通過する。
        self.shutdown();
    }
}
///
/// # なぜ必要か
///
/// [`write_init_script`] が書き出す一時スクリプトは、デーモンの正常な
/// 終了経路（`shutdown` / `Drop` の reap）で削除される。しかし
/// `SIGKILL`・OOM killer・電源断など reap を経ない終わり方をすると
/// ファイルだけが残る。1回あたり数 KB と小さいため実害は出にくいが、
/// 放置するとブリッジディレクトリに数百個単位で堆積する
/// （実環境で 237 個の残骸を確認した）。
///
/// # 安全性: 生きているデーモンのファイルは消さない
///
/// ファイル名は `.daemon_init.<pid>.<random>.zsh`（旧形式は
/// `.daemon_init.<pid>.zsh`）で、`<pid>` は**そのファイルを作った
/// jarvish プロセス**の pid。ここでは pid を取り出し、`kill(pid, 0)` で
/// 生存を確認して、**既に存在しないプロセスのファイルだけ**を削除する。
/// これにより、複数の jarvish を同時に起動していても、他インスタンスが
/// 今まさに使っているスクリプトを消してしまう事故が起きない
/// （デーモンは init 時に一度 source するだけだが、`initialize()` 実行中に
/// 消されると spawn 自体が失敗しうる）。
///
/// 自分自身の pid のファイルも削除対象外（起動直後に自分が書いたものを
/// 消さないため — 呼び出し順に依存しない安全側の設計）。
///
/// pid として解釈できない名前のファイルは触らない。削除に失敗しても
/// 無視する（権限・競合など。掃除は best-effort であり、失敗しても
/// 補完機能自体には影響しない）。
///
/// # 既知の限界: pid の再利用
///
/// OS は終了したプロセスの pid をいずれ再利用する。そのため「残骸を
/// 作った jarvish は既に終了しているが、同じ pid を無関係のプロセスが
/// 引き継いでいる」状態が起こりうる（実環境で、残骸 237 個のうち 1 個が
/// この状態だった — pid を macOS の `followupd` が再利用していた）。
/// この場合そのファイルは「生きている」と判定されて残る。
///
/// これは意図的な割り切りである。判定を強めるには pid だけでなく
/// プロセスの実体まで確認する必要があるが、掃除し損ねたファイルは
/// 数 KB 残るだけで無害な一方、**生きているデーモンのファイルを誤って
/// 消すと spawn 失敗という実害が出る**。したがって「消してよいと確信
/// できない限り消さない」側に倒している。取りこぼしたファイルも、pid が
/// さらに再利用されて次に空くタイミングで回収される。
///
/// 戻り値は削除できたファイル数（呼び出し元のログ用）。
pub(crate) fn cleanup_stale_init_scripts(bridge_dir: &Path) -> usize {
    let Ok(entries) = fs::read_dir(bridge_dir) else {
        return 0;
    };

    let self_pid = std::process::id();
    let mut removed = 0usize;

    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };

        let Some(pid) = parse_init_script_pid(name) else {
            continue;
        };

        // 自分自身のファイルは対象外。
        if pid == self_pid {
            continue;
        }

        if process_is_alive(pid) {
            continue;
        }

        if fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }

    removed
}

/// ブリッジディレクトリに溜まった compdump のリネーム残骸を掃除する。
///
/// # なぜ溜まるのか
///
/// `compinit` はダンプを書き換えるとき、まず
/// `<dump>.<host>.<pid>` という一時ファイルへ書き出してから本来の名前へ
/// `mv` する。プロセスがその間に落ちると一時ファイルだけが残る。
/// ブリッジ用 `.zshrc` が `compinit`（`-d` 指定なし）を実行する構成だと
/// ダンプは `$ZDOTDIR/.zcompdump` になるため、残骸も
/// `.zcompdump.<host>.<pid>` としてブリッジディレクトリに積み上がる
/// （実環境で 25 個の残骸を確認した）。
///
/// # 消すもの / 消さないもの
///
/// 消すのは**リネーム途中の一時ファイルだけ**（`.zcompdump.<host>.<pid>`）。
/// 完成品の `.zcompdump` 本体は**消さない** — これは残骸ではなく有効な
/// キャッシュであり、消すと次回の `compinit` が全補完関数を読み直して
/// 起動が目に見えて遅くなる（掃除の目的はゴミの除去であって、キャッシュ
/// 破棄ではない）。
///
/// 対象は引数で渡されたディレクトリ配下のみ。ユーザーの `$HOME` にある
/// `~/.zcompdump`（jarvish ではなくユーザー自身の対話 zsh が作るもの）には
/// 一切触れない。
///
/// [`cleanup_stale_init_scripts`] と違い pid 生存確認はしない。compdump の
/// 一時ファイルは `mv` 直前の一瞬しか存在しない設計であり、残っている時点で
/// 既に「落ちたプロセスの残骸」が確定しているため（そして仮に競合しても
/// `compinit` は単に作り直すだけで、実害が無い）。
///
/// 戻り値は削除できたファイル数。
pub(crate) fn cleanup_stale_compdumps(bridge_dir: &Path) -> usize {
    let Ok(entries) = fs::read_dir(bridge_dir) else {
        return 0;
    };

    let mut removed = 0usize;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };

        if !is_stale_compdump_name(name) {
            continue;
        }

        if fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }

    removed
}

/// `.zcompdump.<host>.<pid>` 形式（`compinit` のリネーム途中の一時ファイル）
/// かどうかを判定する。
///
/// 完成品の `.zcompdump` そのもの（サフィックス無し）は **false** を返す
/// （有効なキャッシュであり削除対象ではない — [`cleanup_stale_compdumps`]
/// のドキュメント参照）。`.zcompdump_capture` のような別プレフィックスの
/// ファイルも対象外（`.zcompdump.` というドット区切りを厳密に要求する）。
pub(crate) fn is_stale_compdump_name(file_name: &str) -> bool {
    let Some(rest) = file_name.strip_prefix(".zcompdump.") else {
        return false;
    };
    // `<host>.<pid>` の形。末尾が数値（pid）であることを確認する。
    // ホスト名自体にドットを含みうる（`foo.local.123`）ため、最後の
    // ドット以降だけを pid として見る。
    match rest.rsplit_once('.') {
        Some((host, pid)) => {
            !host.is_empty() && !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit())
        }
        None => false,
    }
}

/// `.daemon_init.<pid>.<random>.zsh` / `.daemon_init.<pid>.zsh` から pid を
/// 取り出す。命名規則に合致しない場合は `None`。
///
/// [`write_init_script`] の命名規則と対になっているため、片方を変更する
/// 場合は必ずもう片方も追従させること。
pub(crate) fn parse_init_script_pid(file_name: &str) -> Option<u32> {
    let rest = file_name.strip_prefix(".daemon_init.")?;
    let rest = rest.strip_suffix(".zsh")?;
    // 新形式は `<pid>.<random>`、旧形式は `<pid>` のみ。どちらも先頭が pid。
    let pid_part = rest.split('.').next()?;
    if pid_part.is_empty() {
        return None;
    }
    pid_part.parse::<u32>().ok()
}

/// `pid` のプロセスが存在するかを `kill(pid, 0)` で判定する。
///
/// シグナル 0 は「送らずに存在と権限だけ確認する」という POSIX の慣用。
/// `ESRCH`（そんなプロセスは無い）のときだけ「死んでいる」と判定し、
/// `EPERM`（存在するが自分の権限では触れない）を含むそれ以外は
/// **生きている扱い**にする（消してよいと確信できない限り消さない、
/// という安全側の倒し方）。
pub(crate) fn process_is_alive(pid: u32) -> bool {
    // pid が i32 に収まらない場合は判定不能 → 生きている扱い（消さない）。
    let Ok(raw) = i32::try_from(pid) else {
        return true;
    };
    if unsafe { libc::kill(raw, 0) } == 0 {
        return true;
    }
    io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}
