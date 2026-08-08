use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime};

use super::super::carapace::ExternalCompletionSettings;
use super::super::zsh_daemon::{cleanup_stale_compdumps, cleanup_stale_init_scripts, ZshDaemon};
use super::capture_script::BRIDGE_ZSHRC_TEMPLATE;

/// ブリッジディレクトリ名（`~/.config/jarvish/` 配下）。
pub(crate) const BRIDGE_DIR_NAME: &str = "zsh-bridge";

/// ブリッジディレクトリの `.zshrc` ファイル名。
pub(crate) const BRIDGE_ZSHRC_NAME: &str = ".zshrc";

/// ブリッジディレクトリのパスを返す（`~/.config/jarvish/zsh-bridge/`）。
///
/// `HOME` 未設定環境（テスト等）では `.` 起点にフォールバックする —
/// `config::config_path` と同じ方針。
pub(crate) fn bridge_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".config/jarvish")
        .join(BRIDGE_DIR_NAME)
}

/// ブリッジディレクトリ直下の `.zshrc` パス。
pub(crate) fn bridge_zshrc_path(dir: &Path) -> PathBuf {
    dir.join(BRIDGE_ZSHRC_NAME)
}

/// ブリッジディレクトリと `.zshrc` の存在を保証する。
///
/// ディレクトリが無ければ作成し、`.zshrc` が無ければテンプレートを書き込む。
/// **既存の `.zshrc` は絶対に上書きしない**（ユーザーが書いた fpath/compdef
/// を保護するため）。`provide()` はこれを spawn 直前に毎回呼ぶ — `ZDOTDIR`
/// を設定してもディレクトリ自体が無ければ zsh は `$HOME` にフォールバック
/// しうる（zsh の `ZDOTDIR` 挙動）ため、常に先に存在を保証することで
/// 「ユーザーの実 `~/.zshrc` が意図せず読まれる」事故を防ぐ。
///
/// # シンボリックリンク防御（TOCTOU/symlink 攻撃対策）
/// 攻撃者が `~/.config/jarvish/zsh-bridge` を事前に自分が制御するディレクトリ
/// へのシンボリックリンクとして作成しておくと、`create_dir_all` はそれを
/// 素通りし、以後 Tab を押すたびに攻撃者のディレクトリ配下の `.zshrc`
/// （攻撃者の任意コード）が `ZDOTDIR` 経由で内側 zsh に source されてしまう。
/// これを防ぐため、`create_dir_all` の後に **ブリッジディレクトリ本体と
/// `.zshrc` パスの両方**を `fs::symlink_metadata`（シンボリックリンクを
/// たどらない lstat 相当）で検査し、どちらか一方でもシンボリックリンクで
/// あれば書き込み・利用を一切行わず `Err` を返す（`provide()` はこれを
/// 受けて補完をあきらめ `None` に縮退する）。通常時（シンボリックリンクが
/// 一切絡まないケース）の挙動は従来と完全に同一。
///
/// I/O 失敗（権限等）は `Err` を返し、呼び出し側は補完をあきらめて
/// フォールバックする（既存の graceful degradation 方針と同じ）。
pub(crate) fn ensure_bridge_zshrc(dir: &Path) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;

    if is_symlink(dir)? {
        tracing::warn!(
            "zsh bridge: refusing to use bridge dir {dir:?} because it is a symlink \
             (possible symlink attack) — completion will be skipped"
        );
        return Err(io::Error::other(format!(
            "zsh bridge dir {dir:?} is a symlink, refusing to use it"
        )));
    }

    let zshrc = bridge_zshrc_path(dir);

    if is_symlink(&zshrc)? {
        tracing::warn!(
            "zsh bridge: refusing to use bridge zshrc {zshrc:?} because it is a symlink \
             (possible symlink attack) — completion will be skipped"
        );
        return Err(io::Error::other(format!(
            "zsh bridge zshrc {zshrc:?} is a symlink, refusing to use it"
        )));
    }

    if !zshrc.exists() {
        fs::write(&zshrc, BRIDGE_ZSHRC_TEMPLATE)?;
    }
    Ok(zshrc)
}

/// `path` がシンボリックリンクかどうかを判定する。
///
/// `fs::symlink_metadata`（`lstat` 相当、リンクをたどらない）を使うため、
/// リンク先の実体を経由せずリンクそのものの種別を判定できる。パスが
/// そもそも存在しない場合は `false`（シンボリックリンクではない = 通常の
/// 「まだ何もない」ケースとして扱う）。それ以外の I/O エラーは呼び出し元へ
/// 伝播する。
pub(super) fn is_symlink(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) => Ok(meta.file_type().is_symlink()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

/// `zsh --no-rcs -c <script>` のハードタイムアウト予算。
///
/// 実機計測（warm 状態、zpty 経由の内側 zsh 起動 + compinit + PTY
/// ポーリングを含むワンショット呼び出し全体）では、通常の補完（例:
/// `git <subcommand>` 補完）でも 700〜1100ms かかることが確認されている
/// （zpty/PTY のポーリングオーバーヘッドが支配的）。デフォルト設定
/// （`external_timeout_ms = 400`）をそのまま使うと、この現実的なコストを
/// 大きく下回るタイムアウトになり、ありふれた補完（git サブコマンド
/// 補完等）でも静かにタイムアウトしてしまう。そのため、計測値 700〜1100ms に
/// 余裕（headroom）を持たせた 2000ms を下限値として設定し、共有設定の
/// timeout とこの下限値の大きい方を使う。
///
/// **この下限値は zsh ブリッジ専用**であり、carapace（[`CarapaceProvider`]）
/// には適用しない — carapace は起動コストが低く、設定された
/// `external_timeout_ms` をそのまま使っても実用上問題ない
/// （[`crate::cli::completer::carapace::gate`] のドキュメント参照）。
pub(crate) const MIN_TIMEOUT_MS: u64 = 2000;

/// 温存デーモン（[`ZshDaemon`]）を使ったウォームリクエストの実効タイムアウトに
/// 適用する下限値。
///
/// **以前は 100ms だった。** これは死のループを引き起こしていた:
/// 実機計測で、ありふれた補完関数（例: `_tmuxinator` は `tmuxinator
/// commands zsh` を毎回 exec する）が内部で Ruby 等のインタプリタ起動を
/// 伴う場合、460〜910ms かかることが確認されている。ウォームフロアが
/// この現実的なコストを下回っていると、そうした補完関数を持つコマンドは
/// **デフォルト設定（`external_timeout_ms = 400`）のもとで毎回タイムアウト
/// する** → 旧実装ではタイムアウト = ハング扱いでデーモンを
/// 即 kill → 次の Tab はコールド再 spawn となり、コールド予算
/// （[`MIN_TIMEOUT_MS`] = spawn + init + 初回リクエストの合計）
/// もこの重い初回リクエストを賄いきれず `None` → `PathProvider` フォール
/// バック（「最初の Tab がパス補完になる」症状）。以後の Tab もこの
/// キル/再spawnループを繰り返す。
///
/// そのため、[`MIN_TIMEOUT_MS`] と同じ計測値（460〜910ms）に余裕を
/// 持たせた 2000ms をウォームフロアにも適用する。（グレースドレイン）
/// と組み合わせることで、遅いが正常な補完関数はタイムアウトしても
/// デーモンを即座に殺さなくなるため、このフロア自体は「あからさまに
/// ハングした補完を検知するまでの猶予」としての役割になる。
///
/// **この下限値は温存 zsh デーモン専用**であり、carapace や zsh の
/// ワンショット経路には適用しない（それぞれ [`gate`] 呼び出し時の
/// `min_timeout` 引数を参照）。
pub(crate) const WARM_MIN_TIMEOUT_MS: u64 = 2000;

/// ウォームリクエストの実効タイムアウトを計算する（ロジックを
/// 独立した純粋関数として切り出したもの — ユニットテストで
/// `raw_timeout_ms` → 実効タイムアウトの対応を直接検証するため）。
///
/// 設定された `external_timeout_ms` と [`WARM_MIN_TIMEOUT_MS`] の大きい方を
/// 返す。
pub(crate) fn compute_warm_timeout(raw_timeout: Duration) -> Duration {
    raw_timeout.max(Duration::from_millis(WARM_MIN_TIMEOUT_MS))
}
/// spawn 済みの [`ZshDaemon`] と、その spawn 時点でのブリッジ `.zshrc` の
/// mtime を対にして保持する（mtime 再起動トリガの比較基準）。
///
/// `.zshrc` が存在しなかった／`stat` に失敗した場合は `None` を保持し、
/// 以後の比較で「常に変化なし」として扱う（mtime を取得できない環境で
/// 誤って毎回再起動しないための安全側フォールバック）。
pub struct DaemonSlot {
    pub(super) daemon: ZshDaemon,
    pub(super) zshrc_mtime_at_spawn: Option<SystemTime>,
}

impl DaemonSlot {
    /// このスロットが保持するデーモン子プロセスの pid を返す（テスト専用:
    /// `shell::mod` 等、他モジュールの統合テストが「本当に spawn された
    /// 子プロセスが shutdown 後に死んでいるか」を ESRCH ポーリングで直接
    /// 証明するためのアクセサ。[`ZshDaemon::child_pid_for_test`] への薄い
    /// 委譲）。
    #[cfg(test)]
    pub(crate) fn daemon_pid_for_test(&self) -> u32 {
        self.daemon.child_pid_for_test()
    }
}

/// 温存デーモンスロットの共有ハンドル。
///
/// `ExternalCompletionSettings` と同じ「`Shell::new` で構築し
/// `Arc` として `Shell` / `ZshBridgeProvider` の両方に配る」パターン
/// （`git_branch_commands` / `external_completion` と同じ配管方針）。
/// `Shell` はこのハンドルを経由して、`reload_config`（設定変更の**その場**）
/// や exit / restart 経路など、`provide()` が次に呼ばれるとは限らない
/// ライフサイクルイベント上でもデーモンを確実に shutdown できる
/// （`Drop` にのみ依存すると `Command::exec`
/// や `std::process::exit` では一切実行されないため）。
pub type SharedDaemonSlot = Arc<Mutex<Option<DaemonSlot>>>;

/// 終端 shutdown（exit/exec 経路）が起きたことを示す tombstone フラグ。
///
/// # 背景: `-c` 単体実行での孤児 zsh デーモン
/// `Shell::new` は起動直後に**デタッチしたバックグラウンドスレッド**から
/// [`prewarm_zsh_daemon`] を起動する（[`prewarm_zsh_daemon`] のドキュメント
/// 参照）。`jarvish -c '<command>'` のような非対話実行は数ミリ秒で完走し、
/// `main.rs` は完走直後に [`shutdown_shared_daemon_blocking`] を呼ぶ。この
/// 2つのスレッドの間には本質的なレースがある:
///
/// 1. prewarm スレッドが `ZshDaemon::spawn`（PTY + プロセス起動 + レディ
///    マーカー待ち、数百ms かかりうる）を実行している最中に
/// 2. メインスレッドが `-c` を完走し `shutdown_shared_daemon_blocking` を
///    呼ぶ → その時点でスロットはまだ空（prewarm がまだ書き込んでいない）
///    ため no-op で即座に戻る → `main` は `std::process::exit` する
/// 3. その後 prewarm スレッドの spawn が完了し、共有 `Mutex` を取って
///    「スロットが空だから」と自分の `ZshDaemon` を書き込む
/// 4. 誰もこのデーモンを kill しない — 親プロセス（jarvish 本体）は既に
///    exit 済みのため、子は PID 1 に re-parent されて無期限に生存する
///
/// 実機 E2E で `jarvish -c 'echo hi'` を複数回実行するたびに孤児
/// `/bin/zsh -i`（ppid=1）が1本ずつ増えることを確認済み（このフラグ導入
/// 前）。プリウォームを `-c` モードでは起動時点でスキップする対策
/// （`Shell::new` の `interactive` 引数）だけでは閉じない経路が別にある
/// ため注意: 対話起動でも `rc.jsh` に `exit` が書かれていれば REPL に入る
/// 前に `shutdown_zsh_daemon` → プロセス終了という同じ順序を踏み、同じ
/// レースが成立しうる。
///
/// # 解として選んだ不変条件
/// 「終端 shutdown が一度でも起きたら、その後 prewarm が遅れてスロットに
/// 挿入しようとしても、挿入前に必ず kill される」という不変条件を
/// [`DaemonGate`] に持たせる。[`shutdown_shared_daemon_blocking`]（exit/exec
/// 専用）はこのフラグを `true` にセットしてから shutdown する。
/// [`prewarm_zsh_daemon_with`] は spawn 完了後、共有 `Mutex` の中で
/// 「スロットが空」に加えて「closed でない」ことも確認し、closed なら
/// 今 spawn したデーモンをスロットに書き込まず即座に shutdown する。
///
/// # reload（`source` による設定変更）とは区別する
/// `source` でデーモンを無効化 → 再度有効化、というホットリロード経路
/// （[`shutdown_shared_daemon`]、非ブロッキング版）は再 spawn 可能なまま
/// でなければならない（設計上の要求）。そのため [`shutdown_shared_daemon`]
/// はこのフラグを一切触らない — tombstone は
/// [`shutdown_shared_daemon_blocking`]（exit/exec 専用の有界同期版）のみが
/// セットする。
#[derive(Debug, Default)]
pub struct DaemonGate {
    closed: AtomicBool,
}

impl DaemonGate {
    /// 新しい（open な）ゲートを作る。`Shell::new` から `Arc` として
    /// `SharedDaemonSlot` と対で配る。
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            closed: AtomicBool::new(false),
        })
    }

    /// 終端 shutdown が起きたことを記録する（一度 closed になったら二度と
    /// open に戻らない — jarvish プロセスの残り寿命の間ずっと有効な
    /// tombstone）。
    pub(super) fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }

    /// 終端 shutdown が既に起きているかどうか。
    pub(super) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

/// 共有デーモンスロットが埋まっていれば shutdown してスロットを空にする。
///
/// スロットが既に空なら no-op（冪等）。`Mutex` の poison（他スレッドの
/// panic 経由）はロック取得失敗として扱い、安全側に倒して何もしない
/// （poison 状態から shutdown を試みても panic を伝播させるだけで
/// 状況が改善しないため）。
///
/// # ノンブロッキング
/// kill/reap は [`ZshDaemon::shutdown`] がバックグラウンドスレッドへ
/// 委譲するため、この関数は「スロットの所有権を取り出して手放す」以上の
/// 待ちを一切行わずすぐ戻る。reedline の completer 呼び出し元（UI スレッド）
/// から呼ばれうる経路 — `Shell::reload_config`（設定変更の**その場**での
/// shutdown）、`ZshBridgeProvider::provide()` の `gate()`-None 早期
/// パス、mtime トリガによるデーモン再起動（`request_via_daemon`）—
/// はすべてこちらを使う。プロセスが直後に exec/exit で消える経路
/// （`Shell::exec_restart` 手前、`main.rs` の正常終了経路手前）は、
/// バックグラウンドスレッドに reap を委ねても実行される保証がないため
/// 代わりに [`shutdown_shared_daemon_blocking`] を使う。
pub fn shutdown_shared_daemon(slot: &SharedDaemonSlot) {
    let Ok(mut guard) = slot.lock() else {
        return;
    };
    if let Some(mut slot) = guard.take() {
        slot.daemon.shutdown();
    }
}

/// [`shutdown_shared_daemon`] の有界同期版。
///
/// `deadline` の範囲内で kill/reap の完了を**呼び出し元スレッド上で**
/// 待つ。`Command::exec` 直前・`std::process::exit` 直前など、この行の
/// 後にプロセスが置換/終了されるためバックグラウンドスレッドに reap を
/// 委ねても実行される保証がない経路（Fix A, ce53dfd が landed させた
/// exit/exec shutdown 経路）専用 — reedline の completer 呼び出し元
/// （UI スレッド）から通常のリクエスト処理中に呼んではならない
/// （その場合は必ず [`shutdown_shared_daemon`] を使うこと）。
///
/// # tombstone（[`DaemonGate`] 参照）
/// `gate` を渡した場合、実際の shutdown 処理の**前に** `gate.close()` を
/// 呼ぶ。以後 [`prewarm_zsh_daemon_with`] がこのタイミングより後にスロット
/// へ書き込もうとしても、closed を検知して即座に shutdown する（プロセス
/// 終了直前の遅延 prewarm 挿入による孤児化を防ぐ）。`gate` が `None` の
/// 呼び出し元（`reload_config` 等、tombstone を意図しない経路）は従来どおり
/// フラグに触れない。
pub fn shutdown_shared_daemon_blocking(
    slot: &SharedDaemonSlot,
    deadline: Duration,
    gate: Option<&Arc<DaemonGate>>,
) {
    if let Some(gate) = gate {
        gate.close();
    }
    let Ok(mut guard) = slot.lock() else {
        return;
    };
    if let Some(mut slot) = guard.take() {
        slot.daemon.shutdown_blocking(deadline);
    }
}

/// 新しい（空の）共有デーモンスロットを作る。`Shell::new` / `build_editor`
/// から呼び、`Shell` と [`ZshBridgeProvider::new`] の両方に配る。
pub fn new_shared_daemon_slot() -> SharedDaemonSlot {
    Arc::new(Mutex::new(None))
}

/// Shell 起動時のバックグラウンド事前ウォームアップ。
///
/// `Shell::new` がこの関数を**デタッチしたバックグラウンドスレッド**から
/// 呼ぶことで、ユーザーの最初の Tab 押下時に温存デーモンが既に spawn 済み
/// （= コールドスタートではなくウォームリクエスト）であることを狙う。
/// ここでの spawn 失敗（zsh 未検出、init タイムアウト等）は単に「事前
/// ウォームアップできなかった」だけであり、`provide()` 側の通常の遅延
/// spawn 経路がフォールバックとして機能するため、戻り値は返さずログのみ
/// に留める。
///
/// # 呼び出し前提（`settings` は呼び出し時点のスナップショット）
/// `Shell::new` は設定解決直後（`ExternalCompletionSettings::resolve` 完了
/// 後）にこの関数を**別スレッドへ**ディスパッチする。呼び出し元は
/// `Arc<RwLock<ExternalCompletionSettings>>` を渡すため、prewarm 実行時点
/// までに `reload_config`（`source` 実行）でホットリロードされていたとして
/// も、この関数は呼び出し直前の最新状態を読み直す（`should_run_zsh_daemon`
/// の読み取り自体がその場で行われるため）。
///
/// # `provide()` とのレース回避（プロセス二重 spawn 防止）
/// ユーザーが起動直後に即座に Tab を押すと、`provide()`（reedline の
/// UI スレッド）とこの prewarm スレッドが同時にデーモン未 spawn 状態を
/// 見て、両方が spawn しようとする可能性がある。これを避けるため:
/// 1. `zsh` バイナリの検出・`ZshDaemon::spawn`（重い処理: init スクリプト
///    書き出し + PTY + プロセス spawn + レディマーカー待ち）は**共有
///    `Mutex` の外**で行う（`provide()` 側の UI スレッドをこの重い処理で
///    ブロックしないため——`Mutex` を握ったまま spawn すると、
///    `provide()` 側が `daemon.lock()` で prewarm の完了をブロッキング
///    待ちすることになり、事前ウォームアップの意味がなくなる）。
/// 2. spawn 完了後、共有スロットの `Mutex` を取ってから**もう一度**
///    「スロットが空であること」を確認し、空の場合のみ書き込む。
///    既に埋まっていれば（`provide()` 側が先に spawn していた場合）、
///    このスレッドが今 spawn したデーモンは不要なので即座に shutdown
///    する（二重デーモン防止——Fix A の exit-time shutdown semantics は
///    どちらの経路で spawn されたデーモンにも同様に適用される。このスレッド
///    が捨てるデーモンも通常の `ZshDaemon::shutdown`/`Drop` 経路で確実に
///    kill/reap される）。
///
/// # tombstone チェック（[`DaemonGate`] 参照）
/// `gate` が既に closed（= 終端 shutdown が既に起きた）の場合は、そもそも
/// 重い spawn 処理に入る前に即座に諦める。呼び出し元は `-c` 単体実行の
/// ように起動直後に完走してしまうケースを想定しており、この早期リターンは
/// 「まだ間に合っていない」だけであり、実際にレースを閉じているのは
/// spawn 完了後の**2度目**のチェック（下記）である。
pub fn prewarm_zsh_daemon(
    settings: &Arc<RwLock<ExternalCompletionSettings>>,
    daemon: &SharedDaemonSlot,
    gate: &Arc<DaemonGate>,
) {
    if gate.is_closed() {
        tracing::debug!("zsh daemon prewarm: gate already closed, skipping");
        return;
    }
    let Some(zsh) = which::which("zsh").ok() else {
        tracing::debug!("zsh daemon prewarm: zsh binary not found, skipping");
        return;
    };
    prewarm_zsh_daemon_with(settings, daemon, gate, &zsh, &bridge_dir(), &[]);
}

/// [`prewarm_zsh_daemon`] の本体（テスト専用に `zsh` / `bridge_dir` /
/// `extra_envs` を差し替え可能にした版）。本番経路は
/// [`prewarm_zsh_daemon`] がこの関数に実値を渡すだけの薄いラッパー。
pub(crate) fn prewarm_zsh_daemon_with(
    settings: &Arc<RwLock<ExternalCompletionSettings>>,
    daemon: &SharedDaemonSlot,
    gate: &Arc<DaemonGate>,
    zsh: &Path,
    bridge_dir: &Path,
    extra_envs: &[(String, String)],
) {
    let should_run = match settings.read() {
        Ok(guard) => guard.should_run_zsh_daemon(),
        Err(_) => false,
    };
    if !should_run {
        return;
    }

    let zshrc_path = match ensure_bridge_zshrc(bridge_dir) {
        Ok(path) => path,
        Err(err) => {
            tracing::debug!("zsh daemon prewarm: failed to prepare bridge dir: {err}");
            return;
        }
    };
    let current_mtime = fs::metadata(&zshrc_path).and_then(|m| m.modified()).ok();

    // 死んだプロセスが残した init スクリプトを掃除する（起動時に1回だけ）。
    // prewarm はシェル起動直後にバックグラウンドスレッドで走るため、
    // ここに置けば UI スレッドを一切ブロックしない。生きているプロセスの
    // ファイルは消さない（`cleanup_stale_init_scripts` のドキュメント参照）。
    let removed = cleanup_stale_init_scripts(bridge_dir);
    if removed > 0 {
        tracing::debug!("zsh daemon prewarm: removed {removed} stale init script(s)");
    }

    // 同様に、compinit のリネーム途中で取り残された compdump 一時ファイル
    // （`.zcompdump.<host>.<pid>`）も掃除する。完成品の `.zcompdump` 本体は
    // 有効なキャッシュなので残す（`cleanup_stale_compdumps` のドキュメント参照）。
    let removed_dumps = cleanup_stale_compdumps(bridge_dir);
    if removed_dumps > 0 {
        tracing::debug!("zsh daemon prewarm: removed {removed_dumps} stale compdump temp file(s)");
    }

    // 重い spawn 処理は Mutex の外で行う（provide() 側の UI スレッドを
    // ブロックしないため——ドキュメント冒頭参照）。
    let spawned = ZshDaemon::spawn(
        zsh,
        bridge_dir,
        extra_envs,
        Duration::from_millis(MIN_TIMEOUT_MS),
    );

    let mut new_daemon = match spawned {
        Ok(daemon) => daemon,
        Err(err) => {
            tracing::debug!("zsh daemon prewarm: failed to spawn: {err}");
            return;
        }
    };

    // spawn（重い処理、数百ms かかりうる）の間に終端
    // shutdown が起きていた場合、このデーモンをスロットに書き込む前に
    // 即座に破棄する。`Mutex` の外で行う軽量チェックだが、実際に決定的な
    // 保証を作るのは次の「Mutex の中でのもう一度のチェック」の方
    // （このチェックと `Mutex` 取得の間にも closed になりうるため、
    // 単独では不十分 — 二重チェックのうち片方に過ぎない）。
    //
    // ここでの破棄には非ブロッキング版ではなく `shutdown_blocking` を使う
    // `shutdown()`（非ブロッキング）は kill/reap を
    // さらに別のバックグラウンドスレッドへ委譲するため、この関数が
    // return した時点では子プロセスがまだ生きている可能性がある。
    // 呼び出し元（`Shell::shutdown_zsh_daemon`）はこの関数自体の完了を
    // 「prewarm スレッドの完了通知チャネル」で有界時間待っているため、
    // その通知が届いた時点で子プロセスの kill が終わっていなければ、
    // 直後に `main` が `std::process::exit` した場合に kill 処理ごと
    // 強制終了されて孤児化する（実機 E2E で確認した回帰）。
    if gate.is_closed() {
        tracing::debug!("zsh daemon prewarm: gate closed during spawn, discarding");
        new_daemon.shutdown_blocking(Duration::from_secs(1));
        return;
    }

    let Ok(mut slot_guard) = daemon.lock() else {
        // Mutex poison: 安全側に倒し、このスレッドが spawn したデーモンを
        // 破棄する（呼び出し元スレッドに何かが起きた可能性があり、この
        // スレッドから状態を無理に書き込まない）。
        new_daemon.shutdown_blocking(Duration::from_secs(1));
        return;
    };

    // （決定的保証の本体）: `Mutex` を握った**まま**もう一度
    // closed を確認する。`shutdown_shared_daemon_blocking` は
    // `gate.close()` を必ず `slot.lock()` より**前**に呼ぶ契約になって
    // いるため、この時点で3通りのタイミングしかあり得ない。
    // (a) このスレッドが `Mutex` を取る前に既に closed済み →
    //     直前の Mutex 外チェックで既に弾かれている。
    // (b) このスレッドが `Mutex` を握っている間に shutdown 側が
    //     `gate.close()` を呼んだ（shutdown 側はその後 `slot.lock()` で
    //     ブロックされ待機する）→ この再チェックで捕捉し、書き込まずに
    //     破棄する。shutdown 側は解放されたロックを取得後スロットが
    //     空であることを見て no-op で戻る（孤児化しない）。
    // (c) shutdown 側がまだ影も形もない（closed のまま一切呼ばれていない）
    //     → 通常どおりスロットへ書き込んでよい。
    // つまり「書き込み時点で closed でなければ、以後 close() が呼ばれた
    // 時点で必ずこのスロットを shutdown 経路が発見して kill する」ことが
    // 保証される（このスロットへの書き込みと `close()` の可視性は同じ
    // `Mutex` が提供する happens-before で担保される）。
    if gate.is_closed() {
        tracing::debug!("zsh daemon prewarm: gate closed while holding the slot lock, discarding");
        drop(slot_guard);
        // shutdown_blocking を使う理由は spawn 直後のチェックと同じ
        // （上のコメント参照 — 呼び出し元の完了待ちチャネルが送信される
        // 時点で子プロセスが確実に死んでいることを保証するため）。
        new_daemon.shutdown_blocking(Duration::from_secs(1));
        return;
    }

    if slot_guard.is_some() {
        // レース: provide() 側が既に spawn 済み。このスレッドが今 spawn
        // したデーモンは不要なので破棄する（二重デーモン防止）。
        //
        // tombstone 経路ではなく通常の二重 spawn 防止だが、こちらも
        // shutdown_blocking を使う: `shutdown_zsh_daemon`
        // の完了待ちチャネルは `prewarm_zsh_daemon_with` 関数全体の
        // return（= このスレッドの終了）をもって「prewarm 完了」と
        // 判定するため、この破棄された方の子プロセスの kill/reap も
        // このスレッドの終了前に完了させておかないと、直後の
        // `std::process::exit` で道連れに強制終了され、init スクリプトの
        // 一時ファイルが削除されないまま残ることが実機計測で確認できた
        // （孤児プロセス自体は発生しない — スロットに残る方は provide()
        // 側が持つ別インスタンスであり、こちらの捨てられる方だけが対象）。
        drop(slot_guard);
        new_daemon.shutdown_blocking(Duration::from_secs(1));
        return;
    }

    *slot_guard = Some(DaemonSlot {
        daemon: new_daemon,
        zshrc_mtime_at_spawn: current_mtime,
    });
}
