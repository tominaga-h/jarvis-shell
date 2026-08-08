use super::*;

/// [`ZshDaemon::spawn`] 用の PTY ペアを作る。
///
/// `engine/pty.rs::create_session_pty` と同じ手順（`nix::pty::openpty`、
/// OPOST は有効のまま）だが、`engine::pty` はプライベートモジュールで
/// `cli::completer` から到達できないため、ここで同じパターンを再実装する
/// （タスク指示: "portable-pty crate ... same as engine/pty.rs" —
/// 実際には `engine/pty.rs` 自体が `nix::pty::openpty` を直接使っており
/// `portable-pty` クレートには依存していないため、既存のクレート利用
/// パターンに揃えている）。
pub(crate) fn create_daemon_pty() -> io::Result<(fs::File, OwnedFd)> {
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

/// PTY slave の line discipline から ECHO を無効化する。
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
pub(crate) fn disable_echo(slave_fd: &OwnedFd) {
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
/// 攻撃対象になりうるため、ランダム成分が本質的に必要）。
///
/// # シンボリックリンク防御
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
pub(crate) fn write_init_script(bridge_dir: &Path) -> io::Result<PathBuf> {
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
pub(crate) fn read_available(master: &mut fs::File, timeout: Duration) -> Option<Vec<u8>> {
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
pub(crate) fn contains_line(haystack: &[u8], needle: &str) -> bool {
    let text = String::from_utf8_lossy(haystack);
    text.lines().any(|line| line.trim_end() == needle)
}
