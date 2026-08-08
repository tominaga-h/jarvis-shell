use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use super::super::carapace::{gate, ExternalCompletionSettings, ExternalKind};
use super::super::context::CompletionContext;
use super::super::external::run_external_capped;
use super::super::provider::{Candidate, CompletionProvider};
use super::super::zsh_daemon::ZshDaemon;
#[allow(unused_imports)]
// new_shared_daemon_slot used at lines 131/146/165 (struct field init)
use super::{
    bridge_dir, bridge_zshrc_path, compute_warm_timeout, ensure_bridge_zshrc, escape_spans,
    new_shared_daemon_slot, parse_capture_output, shutdown_shared_daemon, DaemonSlot,
    SharedDaemonSlot, CAPTURE_SCRIPT, MIN_TIMEOUT_MS,
};

/// zsh 補完ブリッジ Provider。
///
/// `ExternalCompletionSettings` を [`super::carapace::CarapaceProvider`] と
/// 同じ `Arc<RwLock<_>>` で共有する（`git_branch_commands` と同じ配管
/// パターン）。有効化判定は `settings.binary_path(ExternalKind::Zsh)` が
/// `Some` を返すかどうかに一本化されている — `[completion] external` の
/// 値（`"auto"` / `"zsh"` / 配列での明示指定など）に応じて `resolve()` が
/// このプロバイダを優先順リストに含めるかどうか・zsh バイナリを検出するか
/// どうかを決める。timeout も同じ共有設定から取得する。
///
/// # 温存デーモン配線
/// `[completion] external_zsh_daemon`（`settings.zsh_daemon_enabled`）が
/// `true` の間、[`ZshDaemon`] を使い回す。**主経路は起動時の事前ウォーム
/// アップ**（[`prewarm_zsh_daemon`]）——`Shell::new` がバックグラウンド
/// スレッドから spawn 済みにしておくため、通常は最初の `provide()` 呼び
/// 出し時点で `daemon` スロットが既に埋まっている。プリウォームが間に
/// 合わなかった場合（または zsh 未検出等でスキップされた場合）は、
/// `provide()` 自身が遅延 spawn するフォールバック経路が働く。completer は
/// reedline の UI スレッド上で同期的に呼ばれる（並行呼び出しなし）ため、
/// `daemon` フィールドの `Mutex` は `&self` からの内部可変性確保のみが
/// 目的であり、実際の競合排他は発生しない（prewarm 用バックグラウンド
/// スレッドとのレースのみ Mutex の二重チェックで防止する——
/// [`prewarm_zsh_daemon`] のドキュメント参照）。
/// - **コールド**（デーモン未 spawn、または直前のリクエストで dead 化した
///   直後の再 spawn）: `MIN_TIMEOUT_MS`（2000ms）フロアを spawn + init の
///   レディマーカー待ちのみに適用する。spawn 直後に送る最初の
///   実補完リクエスト自体はこの予算に含めず、常にウォーム側の
///   `warm_timeout` を使う——spawn+init 自体は速くても補完関数の初回呼び
///   出しが重い（`tmuxinator` 等）ケースで、初回 Tab だけコールド予算を
///   使い切って `None` になっていた不具合の修正。
/// - **ウォーム**（既に生きているデーモンへの2回目以降のリクエスト、および
///   spawn 直後の初回リクエスト）: 設定された `external_timeout_ms`
///   と [`WARM_MIN_TIMEOUT_MS`]（2000ms）の
///   大きい方を使う。
/// - **失敗時（グレースドレイン + サーキットブレーカー）**:
///   1回のクリーンなタイムアウト（遅いが正常な補完関数、例:
///   インタプリタ起動を伴う `tmuxinator` 補完）では、もはやデーモンを
///   即座に kill しない——[`ZshDaemon`] は残留フレームを次回リクエストで
///   排水するだけに留め、生存を続ける。**連続2回**のタイムアウト
///   （ドレイン失敗 + 通常失敗、または通常失敗が2回連続）で初めてハングと
///   判定し、[`ZshDaemon::request`] が内部で子プロセスをバックグラウンド
///   kill 済み・`is_alive() == false` になる。いずれの場合も `provide()`
///   はこの Tab では `None` を返す（同一キー押下内でのワンショット
///   フォールバックは行わない — 仕様どおり）。デーモンが実際に kill
///   された場合のみ、次回 Tab で `daemon` スロットが `None`（dead
///   インスタンスは捨てる）になっているため、遅延 respawn が自然に起きる。
///   応答バッファ上限超過（プロトコル desync 相当）はグレースの対象外で
///   即座に kill する。
/// - **再起動トリガ**: 毎リクエスト前にブリッジ `.zshrc` の mtime を
///   spawn 時点のものと比較する（`stat` のみで安価）。両方 `Some` かつ
///   不一致の場合のみ変化ありと判定し、ユーザーが `fpath`/`compdef` を
///   編集したとみなして既存デーモンを shutdown してから同一リクエスト内で
///   新しいデーモンを遅延 spawn する（どちらか一方でも `None`——`stat`
///   不能——の場合は「変化なし」として扱い、誤検知で毎回再起動しない
///   安全側フォールバック）。`settings.zsh_daemon_enabled` が `false` に
///   変わった場合も同様に既存デーモンを shutdown する（以後はワンショット
///   経路を使う）。
/// - **プロセス終了・設定変更での明示的 shutdown**: `provide()` からの
///   自然な respawn/kill サイクルとは別に、`Shell` はライフサイクル
///   イベント（`source` による設定変更のその場、`exec`/`exit` 直前、
///   `restart` ビルトイン）でも稼働中のデーモンを明示的に shutdown する
///   （[`shutdown_shared_daemon`] / [`shutdown_shared_daemon_blocking`]
///   のドキュメント参照）——稼働中のデーモンが Jarvish のセッションより
///   長生きすることはない。
pub(crate) struct ZshBridgeProvider {
    pub(super) settings: Arc<RwLock<ExternalCompletionSettings>>,
    /// 温存デーモン本体（`Shell` と共有する `Arc<Mutex<_>>`）。
    /// `None` は「未 spawn」または「直前のリクエストで dead 化して捨てた」、
    /// あるいは `Shell` 側がライフサイクルイベント（reload/exit/restart）で
    /// shutdown 済みであることを意味する（次回リクエストで遅延 respawn）。
    pub(super) daemon: SharedDaemonSlot,
    /// テスト用に zsh の場所を差し替えられるようにするフック。
    /// 本番は `None` で `which::which("zsh")` を都度引く。
    pub(super) zsh_override: Option<PathBuf>,
    /// テスト用にブリッジディレクトリ（`ZDOTDIR` に渡す先）を差し替える
    /// フック。本番は `None` で [`bridge_dir`]（`~/.config/jarvish/zsh-bridge/`）
    /// を使う。E2E テストではユーザーの実 `~/.config` を汚さないよう
    /// tempdir を注入する。
    pub(super) bridge_dir_override: Option<PathBuf>,
    /// テスト専用: spawn する外側 zsh に追加で渡す環境変数。本番は常に空。
    ///
    /// `capture.zsh`（vendor・改変不可）の `compinit -d ~/.zcompdump_capture`
    /// は `$ZDOTDIR` ではなく **`$HOME`** を基準に固定パスの compdump
    /// キャッシュへ読み書きする。そのため、異なる一時 fpath ディレクトリを
    /// 使う複数の E2E テストを同一の実 `$HOME` で連続実行すると、後続の
    /// テストが古い compdump を再利用してしまい新しい `#compdef` 関数を
    /// 認識できないことがある（実地検証済みの環境依存フレーク）。
    /// このフックで `HOME` をテストごとの tempdir に差し替え、compdump の
    /// 汚染・衝突を避ける。
    #[cfg(test)]
    pub(super) extra_envs: Vec<(String, String)>,
}
impl ZshBridgeProvider {
    pub(crate) fn new(
        settings: Arc<RwLock<ExternalCompletionSettings>>,
        daemon: SharedDaemonSlot,
    ) -> Self {
        Self {
            settings,
            daemon,
            zsh_override: None,
            bridge_dir_override: None,
            #[cfg(test)]
            extra_envs: Vec::new(),
        }
    }

    #[cfg(test)]
    pub(super) fn with_zsh_binary(
        settings: Arc<RwLock<ExternalCompletionSettings>>,
        zsh: PathBuf,
    ) -> Self {
        Self {
            settings,
            daemon: new_shared_daemon_slot(),
            zsh_override: Some(zsh),
            bridge_dir_override: None,
            extra_envs: Vec::new(),
        }
    }

    #[cfg(test)]
    pub(super) fn with_zsh_binary_and_bridge_dir(
        settings: Arc<RwLock<ExternalCompletionSettings>>,
        zsh: PathBuf,
        bridge_dir: PathBuf,
    ) -> Self {
        Self {
            settings,
            daemon: new_shared_daemon_slot(),
            zsh_override: Some(zsh),
            bridge_dir_override: Some(bridge_dir),
            extra_envs: Vec::new(),
        }
    }

    /// [`with_zsh_binary_and_bridge_dir`] に加え、spawn する外側 zsh へ渡す
    /// 追加の環境変数（`HOME` の compdump キャッシュ隔離など）を指定する。
    /// テスト専用（`extra_envs` フィールドのドキュメント参照）。
    #[cfg(test)]
    pub(super) fn with_zsh_binary_bridge_dir_and_envs(
        settings: Arc<RwLock<ExternalCompletionSettings>>,
        zsh: PathBuf,
        bridge_dir: PathBuf,
        extra_envs: Vec<(String, String)>,
    ) -> Self {
        Self {
            settings,
            daemon: new_shared_daemon_slot(),
            zsh_override: Some(zsh),
            bridge_dir_override: Some(bridge_dir),
            extra_envs,
        }
    }

    /// テスト専用: 既存の共有スロットを注入する版（reload/exit 経路の
    /// 統合テストで `Shell` 側と同じ `Arc` を共有する必要がある場合に使う）。
    #[cfg(test)]
    pub(super) fn with_shared_daemon_slot_for_test(
        settings: Arc<RwLock<ExternalCompletionSettings>>,
        zsh: PathBuf,
        bridge_dir: PathBuf,
        extra_envs: Vec<(String, String)>,
        daemon: SharedDaemonSlot,
    ) -> Self {
        Self {
            settings,
            daemon,
            zsh_override: Some(zsh),
            bridge_dir_override: Some(bridge_dir),
            extra_envs,
        }
    }

    pub(super) fn resolve_zsh(&self) -> Option<PathBuf> {
        if let Some(path) = &self.zsh_override {
            return Some(path.clone());
        }
        which::which("zsh").ok()
    }

    fn resolve_bridge_dir(&self) -> PathBuf {
        self.bridge_dir_override.clone().unwrap_or_else(bridge_dir)
    }

    /// 現在のテスト用 `extra_envs`（本番ビルドでは常に空）を返す。
    #[cfg(test)]
    fn extra_envs(&self) -> Vec<(String, String)> {
        self.extra_envs.clone()
    }

    #[cfg(not(test))]
    fn extra_envs(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    /// デーモン経路でのリクエストを試みる。
    ///
    /// `zsh` / `bridge_dir` / `escaped_spans` は呼び出し元（`provide()`）で
    /// 解決済みの値をそのまま受け取る（ワンショット経路と共有するため）。
    /// 戻り値は [`parse_capture_output`] にそのまま渡せる生テキスト
    /// （`None` はデーモン経路自体が使えなかった/失敗したことを示し、
    /// 呼び出し元はワンショットへフォールバックしない仕様 —
    /// 型ドキュメント参照）。
    pub(super) fn request_via_daemon(
        &self,
        zsh: &Path,
        bridge_dir: &Path,
        escaped_spans: &[String],
        cold_timeout: Duration,
        warm_timeout: Duration,
    ) -> Option<String> {
        let zshrc_path = bridge_zshrc_path(bridge_dir);
        let current_mtime = fs::metadata(&zshrc_path).and_then(|m| m.modified()).ok();

        let mut slot_guard = self.daemon.lock().ok()?;

        // 再起動トリガ: 既存デーモンがあり、spawn 時点の mtime と現在の
        // mtime が食い違う（両方 Some で不一致）場合は shutdown する。
        // どちらかが None（stat 不能）の場合は「変化なし」として扱い、
        // 誤検知で毎回再起動しない安全側に倒す。
        if let Some(slot) = slot_guard.as_ref() {
            let mtime_changed = matches!(
                (slot.zshrc_mtime_at_spawn, current_mtime),
                (Some(a), Some(b)) if a != b
            );
            if mtime_changed {
                tracing::debug!("zsh daemon: bridge .zshrc changed since spawn, restarting daemon");
                *slot_guard = None;
            }
        }

        if slot_guard.is_none() {
            // `cold_timeout`（[`MIN_TIMEOUT_MS`]）は spawn + init
            // レディマーカー待ちのみを賄う予算であり、その直後に送る最初の
            // 実補完リクエストはこの中に含めない（`ZshDaemon::spawn` 内部の
            // `initialize()` が既に「レディマーカーを待つだけ」の実装に
            // なっているため、ここでの変更は「初回リクエストのタイムアウト
            // として cold_timeout ではなく warm_timeout を使う」ことだけで
            // 完成する）。以前は初回リクエストも `cold_timeout` を使い回して
            // いたため、spawn+init 自体は速くても実測 460〜910ms かかる
            // 重い補完関数（tmuxinator 等）の初回リクエストが cold budget を
            // 使い切ってしまい、初回 Tab だけ `None`（PathProvider
            // フォールバック）になっていた（実機報告）。
            let extra_envs = self.extra_envs();
            match ZshDaemon::spawn(zsh, bridge_dir, &extra_envs, cold_timeout) {
                Ok(daemon) => {
                    *slot_guard = Some(DaemonSlot {
                        daemon,
                        zshrc_mtime_at_spawn: current_mtime,
                    });
                }
                Err(err) => {
                    tracing::debug!("zsh daemon: failed to spawn: {err}");
                    return None;
                }
            }
        }

        let line = escaped_spans.join(" ");
        let slot = slot_guard.as_mut()?;
        // spawn 直後の初回リクエストも含め、常に warm_timeout を
        // 使う（cold_timeout は spawn()/initialize() 内部の準備段階専用）。
        let result = slot.daemon.request(&line, warm_timeout);

        if !slot.daemon.is_alive() {
            // request() 内部で timeout/desync により kill 済み。次回リクエスト
            // で遅延 respawn できるようスロットを空にする（「デーモンは kill され、この Tab は None、次の Tab で遅延
            // respawn」という仕様どおり）。
            *slot_guard = None;
        }

        result
    }

    /// `[completion] external_zsh_daemon` が `false`（初期設定 or `source`
    /// による reload で off にされた）場合に呼ぶ。生きているデーモンが
    /// 残っていれば明示的に shutdown してスロットを空にする（`ZshDaemon`
    /// の `Drop` に任せず、設定変更の**その場**で確実に子プロセスを畳む —
    /// タスク指示: "turning it off shuts the daemon down"）。デーモンが
    /// 元々無ければ no-op。[`shutdown_shared_daemon`] への薄い委譲
    /// （`Shell::reload_config` / exit / restart 経路と同じ shutdown 経路を
    /// 使うことで実装を1箇所に保つ）。
    fn shutdown_daemon_if_running(&self) {
        shutdown_shared_daemon(&self.daemon);
    }
}

impl ZshBridgeProvider {
    /// `ctx` が zsh ブリッジの対象（先頭トークンでない・zsh が優先順リストに
    /// 有効化されている・spans 十分・span 内容がエスケープ可能）かどうかを、
    /// 実際に zsh プロセスを起動せず安価に判定する。
    ///
    /// [`CompletionProvider::is_responsible`] と `provide()` 本体の両方から
    /// 呼ばれる判定基準の単一の情報源（perf/completion-latency）。
    /// `provide()` が `gate()` の後に行う副作用（`shutdown_daemon_if_running`
    /// の呼び出し、ブリッジディレクトリへのファイル書き込み）はここには
    /// 含めない — `is_responsible` は「対象かどうか」の純粋に近い判定に
    /// 留め、実際の実行（と、その過程で起きる副作用）は `provide()` 側の
    /// 責務のままにする。
    fn is_target_of_zsh_bridge(&self, ctx: &CompletionContext) -> bool {
        if ctx.is_first_token {
            // コマンド名自体の補完は CommandProvider の担当。
            return false;
        }

        // `MIN_TIMEOUT_MS` フロアの有無は「対象かどうか」の判定には無関係
        // （timeout 値そのものは使わない）が、`gate()` を呼ぶこと自体が
        // 「zsh が enabled かつバイナリ検出済みか」を確認する唯一の経路
        // なので `provide()` と同じ呼び方をする。
        if gate(
            &self.settings,
            ExternalKind::Zsh,
            Some(Duration::from_millis(MIN_TIMEOUT_MS)),
        )
        .is_none()
        {
            return false;
        }

        if self.resolve_zsh().is_none() {
            return false;
        }

        if ctx.spans().len() < 2 {
            // spans[0] (コマンド名) しかない = まだサブコマンド/引数の
            // 補完対象がない（carapace.rs と同じガード）。
            return false;
        }

        // 制御文字を含む span は `escape_spans` が None を返す（安全に
        // 表現できず補完を諦める既存仕様）。この場合も「zsh ブリッジが
        // 対象だが実行できない」ではなく「そもそも対象外」寄りの性質が
        // 強いが、`provide()` 側もこの場合 None に縮退するため、
        // is_responsible を true にすると「誤って PathProvider を抑止して
        // 空候補になる」だけで済み（安全側）、逆に false にすると通常の
        // 制御文字混入ケースで PathProvider に静かにフォールスルーする
        // （挙動が変わる）。ここでは後者（既存挙動維持）を優先し、
        // escape 不能なら「対象外」として扱う。
        escape_spans(&ctx.spans()).is_some()
    }
}

impl CompletionProvider for ZshBridgeProvider {
    fn is_responsible(&self, ctx: &CompletionContext) -> bool {
        self.is_target_of_zsh_bridge(ctx)
    }

    fn provide(&self, ctx: &CompletionContext) -> Option<Vec<Candidate>> {
        if ctx.is_first_token {
            // コマンド名自体の補完は CommandProvider の担当。
            return None;
        }

        // 短命な read ロック（`gate` 内部で取得・即座に drop する —
        // `carapace.rs` / `mod.rs` の aliases スナップショットと同じ方針）。
        // zsh が優先順リストに含まれていない（無効化されている、または
        // carapace のみが指定されている等）場合は `gate` が `None` を返す。
        // その場合でも、直前まで zsh が有効だった名残で温存デーモンが
        // 生きたまま残っている可能性があるため（例: `external` を配列で
        // `["carapace"]` に変更して `source` した直後）、`None` で早期 return する前に必ず shutdown しておく
        // （既に空なら no-op、冪等）。`MIN_TIMEOUT_MS` フロアはワンショット
        // 経路/デーモンのコールド経路専用（compinit の重さ対策 — 定数の
        // ドキュメント参照）なので `Some(...)` で渡す。
        let Some((_gated_binary, cold_timeout)) = gate(
            &self.settings,
            ExternalKind::Zsh,
            Some(Duration::from_millis(MIN_TIMEOUT_MS)),
        ) else {
            self.shutdown_daemon_if_running();
            return None;
        };

        // ウォーム経路用の実効タイムアウト（設定値 + 小さな床）。
        // `gate` はコールド用フロアしか計算しないため、ここでは共有設定の
        // 生 timeout を別途読み直す（`Arc<RwLock<_>>` への短命な read ロック
        // — 他の共有設定アクセスと同じ方針）。
        let (daemon_enabled, raw_timeout) = match self.settings.read() {
            Ok(guard) => (guard.zsh_daemon_enabled, guard.timeout),
            Err(_) => (false, cold_timeout),
        };
        let warm_timeout = compute_warm_timeout(raw_timeout);

        let zsh = self.resolve_zsh()?;

        let spans = ctx.spans();
        if spans.len() < 2 {
            // spans[0] (コマンド名) しかない = まだサブコマンド/引数の
            // 補完対象がない（carapace.rs と同じガード）。
            return None;
        }

        let escaped_spans = escape_spans(&spans)?;

        // ブリッジディレクトリ + テンプレート .zshrc の存在を保証してから
        // 使う。存在保証を spawn/リクエストの直前に必ず行うことで、
        // ZDOTDIR が指すディレクトリが空だったために zsh が $HOME に
        // フォールバックし、ユーザーの実 ~/.zshrc を読んでしまう事故を防ぐ
        // （モジュール冒頭ドキュメント参照）。
        let bridge_dir = self.resolve_bridge_dir();
        if ensure_bridge_zshrc(&bridge_dir).is_err() {
            tracing::debug!("zsh bridge: failed to prepare bridge dir at {bridge_dir:?}, skipping");
            return None;
        }

        if daemon_enabled {
            let stdout = self.request_via_daemon(
                &zsh,
                &bridge_dir,
                &escaped_spans,
                cold_timeout,
                warm_timeout,
            )?;
            let candidates = parse_capture_output(&stdout);
            if candidates.is_empty() {
                return None;
            }
            return Some(candidates);
        } else {
            // 設定でデーモンが無効化された（または reload で off にされた）。
            // 生きているデーモンが残っていれば shutdown し、以後はワン
            // ショット経路のみを使う。
            self.shutdown_daemon_if_running();
        }

        let mut args = vec![
            "--no-rcs".to_string(),
            "-c".to_string(),
            CAPTURE_SCRIPT.to_string(),
            "--".to_string(),
        ];
        args.extend(escaped_spans);

        #[cfg_attr(not(test), allow(unused_mut))]
        let mut envs = vec![(
            "ZDOTDIR".to_string(),
            bridge_dir.to_string_lossy().into_owned(),
        )];
        #[cfg(test)]
        envs.extend(self.extra_envs.iter().cloned());

        let stdout = run_external_capped(&zsh, &args, &envs, cold_timeout)?;

        let candidates = parse_capture_output(&stdout);
        if candidates.is_empty() {
            return None;
        }
        Some(candidates)
    }
}
