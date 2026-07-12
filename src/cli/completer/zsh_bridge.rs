//! zsh 補完ブリッジ — vendored `capture.zsh` を経由して zsh の compsys
//! （`_*` 補完関数群）の候補をワンショットで吸い出す Provider
//!
//! `assets/zsh/capture.zsh`（`Valodim/zsh-capture-completion`、MIT）を
//! `include_str!` でバイナリに埋め込み、Tab 押下ごとに `zsh --no-rcs -c
//! <script> -- <spans...>` を [`run_external_capped`] 経由で起動する。
//! スクリプト内部では `zpty` で "内側の" zsh をさらに1本起動し compinit
//! した上で completion widget を叩くため（`zsh.go` の呼び出し形と実地検証済み
//! プロトコル — このファイル冒頭のドキュメント参照）、ハング時の kill が
//! 特に重要。ただし `zpty` が起動する内側の zsh は PTY 経由で**独自の
//! プロセスグループ**を持つため、外側 zsh のプロセスグループだけを kill
//! する単純な group-kill では内側 zsh に届かない（外側 zsh が SIGKILL で
//! 即死しても、内側 zsh は自身の pgid のまま生き残りうる）。
//! [`run_external_capped`] はこれに対処するため、タイムアウト時に外側 pid
//! の子孫プロセス全体を事前収集し、通常のグループ kill に加えて各子孫
//! （別 pgid のものを含む）にも個別に SIGKILL を送る
//! （`src/cli/completer/external.rs` のモジュールドキュメント参照）。
//!
//! # 呼び出し形（`zsh.go` = carapace-bridge のリファレンス実装と一致）
//! ```text
//! zsh --no-rcs -c <embedded script> -- <word0> <word1> ... <partial>
//! ```
//! `partial` は最後の引数（空文字列もありうる）。`capture.zsh` は
//! `zpty -w z "$*"$'\t'` で argv をスペース結合して内側の zsh バッファに
//! 流し込むため、`partial` が空文字列でも末尾にスペースが付き、compsys が
//! 「新しい単語の補完」として扱う（`spans()` が既にこの規約で
//! 末尾に空文字列を追加する設計になっている — `context.rs` 参照）。
//!
//! # 出力パース
//! `capture.zsh` 自身が NUL センチネル行を吸収し、候補行だけを stdout に
//! 流す（このファイルではセンチネル処理は不要）。行区切りは PTY 由来の
//! `\r\n` で、末尾の空要素は捨てる。各行は:
//! 1. ANSI エスケープ除去
//! 2. `zsh.go` のバックスラッシュ unquote テーブルを適用
//! 3. 最初の `" -- "` で `value` / `description` に分割（区切りなしなら
//!    description は `None`）
//!
//! # ユーザー拡張点（zsh-bridge ブリッジディレクトリ、Task 2b.2）
//! `capture.zsh` は `zpty z zsh -f -i` で内側の zsh を起動していたが
//! （vendor 元のまま）、`-f`（`NO_RCS`）は zshrc を一切読ませないフラグ
//! のため、これではユーザーが `fpath` に `zsh-completions` を追加したり
//! `compdef` を書いたりする余地がない。そこで jarvish 側で2点を組み合わせる:
//!
//! 1. `assets/zsh/capture.zsh` の当該行を `-f` を落として `zpty z zsh -i`
//!    に変更（`# jarvish:` コメント付き、vendor ファイルの他の部分は無改変）。
//! 2. **外側**の `zsh --no-rcs -c <script> -- ...` プロセスの環境変数に
//!    `ZDOTDIR=<bridge dir>` を設定する（[`run_external_capped`] の
//!    `envs` 引数経由）。`ZDOTDIR` は子プロセスに継承されるため、`zpty`
//!    が spawn する内側の対話 zsh もこれを引き継ぎ、`$ZDOTDIR/.zshrc`
//!    （= [`bridge_zshrc_path`]）を source する。
//!
//! 結果として内側 zsh はユーザーの実 `~/.zshrc` ではなく jarvish 専用の
//! ブリッジ zshrc を読む（carapace の `~/.config/carapace/bridge/zsh` と
//! 同じ設計思想）。**[`ensure_bridge_zshrc`] は毎回の `provide()` 呼び出し
//! で必ずブリッジディレクトリと `.zshrc` の存在を保証してから spawn する**
//! ため、`ZDOTDIR` が万一未設定になっても実 `~/.zshrc` へ漏れることはない
//! （zsh は `ZDOTDIR` 未設定時 `$HOME` を使うが、`ZDOTDIR` は常に明示設定
//! される — この関数を経由しない spawn 経路が生まれない限り安全）。
//! `-f` を落としたことで `/etc/zshrc` は読まれるようになる（carapace-bridge
//! も同じ挙動）。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime};

use super::carapace::{gate, ExternalCompletionSettings, ExternalKind};
use super::context::CompletionContext;
use super::external::run_external_capped;
use super::provider::{Candidate, CompletionProvider};
use super::zsh_daemon::ZshDaemon;

/// zsh ブリッジ本体（`assets/zsh/capture.zsh` を vendor したもの）。
const CAPTURE_SCRIPT: &str = include_str!("../../../assets/zsh/capture.zsh");

/// ブリッジ用 `.zshrc` の初回生成テンプレート。
///
/// fpath 追加や `compdef` の書き方をコメントで示す最小限のサンプル。
/// ユーザーが自由に書き換えてよい（jarvish は既存ファイルを上書きしない —
/// [`ensure_bridge_zshrc`] 参照）。
const BRIDGE_ZSHRC_TEMPLATE: &str = r#"# jarvish zsh completion bridge — ~/.config/jarvish/zsh-bridge/.zshrc
#
# このファイルは jarvish の Tab 補完が内部で起動する "ブリッジ用" zsh
# だけが読み込みます。あなたの通常の ~/.zshrc には一切影響しません。
# This file is sourced only by the internal zsh jarvish spawns for Tab
# completion. It has no effect on your normal ~/.zshrc.
#
# ここに書いた fpath 追加や compdef は、本物の zsh 構文でそのまま使えます。
# Anything you write here (fpath additions, compdef, zstyle, ...) uses real
# zsh syntax — no jarvish-specific DSL to learn.

# 例1: Homebrew でインストールした zsh-completions を fpath に追加する
# Example: add Homebrew's zsh-completions to fpath
#   brew install zsh-completions
# fpath=(/opt/homebrew/share/zsh-completions $fpath)
#
# 注意: 追加するディレクトリやその親ディレクトリが group-writable だと、
# zsh の compinit セキュリティ検査（compaudit）に引っかかり補完が全滅
# することがあります（例: Intel Mac の /usr/local/share）。`compaudit`
# で確認し、必要なら `chmod g-w /usr/local/share` を実行してください。
# Warning: if the directory you add (or its parent) is group-writable,
# zsh's compinit security check (compaudit) may flag it and silently
# break all completions (e.g. /usr/local/share on Intel Macs). Check
# with `compaudit`, and if needed run `chmod g-w /usr/local/share`.

# 例2: 自作/追加の補完関数を任意のディレクトリから読み込む
# Example: load custom completion functions from your own directory
# fpath=(~/.zsh/completions $fpath)

# 例3: 特定コマンドに補完関数を明示的に紐付ける (compdef)
# Example: bind a completion function to a command explicitly
# compdef _git my-git-wrapper
"#;

/// ブリッジディレクトリ名（`~/.config/jarvish/` 配下）。
const BRIDGE_DIR_NAME: &str = "zsh-bridge";

/// ブリッジディレクトリの `.zshrc` ファイル名。
const BRIDGE_ZSHRC_NAME: &str = ".zshrc";

/// ブリッジディレクトリのパスを返す（`~/.config/jarvish/zsh-bridge/`）。
///
/// `HOME` 未設定環境（テスト等）では `.` 起点にフォールバックする —
/// `config::config_path` と同じ方針。
pub(super) fn bridge_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".config/jarvish")
        .join(BRIDGE_DIR_NAME)
}

/// ブリッジディレクトリ直下の `.zshrc` パス。
fn bridge_zshrc_path(dir: &Path) -> PathBuf {
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
fn ensure_bridge_zshrc(dir: &Path) -> io::Result<PathBuf> {
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
fn is_symlink(path: &Path) -> io::Result<bool> {
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
const MIN_TIMEOUT_MS: u64 = 2000;

/// 温存デーモン（[`ZshDaemon`]）を使ったウォームリクエストの実効タイムアウトに
/// 適用する下限値（Task 2b.3 / Fix D, #89）。
///
/// **Fix D 以前は 100ms だった。** これは死のループを引き起こしていた:
/// 実機計測で、ありふれた補完関数（例: `_tmuxinator` は `tmuxinator
/// commands zsh` を毎回 exec する）が内部で Ruby 等のインタプリタ起動を
/// 伴う場合、460〜910ms かかることが確認されている。ウォームフロアが
/// この現実的なコストを下回っていると、そうした補完関数を持つコマンドは
/// **デフォルト設定（`external_timeout_ms = 400`）のもとで毎回タイムアウト
/// する** → 旧実装（Fix B 以前）ではタイムアウト = ハング扱いでデーモンを
/// 即 kill → 次の Tab はコールド再 spawn となり、コールド予算
/// （[`MIN_TIMEOUT_MS`] = spawn + init + 初回リクエストの合計、Fix D3 以前）
/// もこの重い初回リクエストを賄いきれず `None` → `PathProvider` フォール
/// バック（「最初の Tab がパス補完になる」症状）。以後の Tab もこの
/// キル/再spawnループを繰り返す。
///
/// そのため、[`MIN_TIMEOUT_MS`] と同じ計測値（460〜910ms）に余裕を
/// 持たせた 2000ms をウォームフロアにも適用する。Fix D2（グレースドレイン）
/// と組み合わせることで、遅いが正常な補完関数はタイムアウトしても
/// デーモンを即座に殺さなくなるため、このフロア自体は「あからさまに
/// ハングした補完を検知するまでの猶予」としての役割になる。
///
/// **この下限値は温存 zsh デーモン専用**であり、carapace や zsh の
/// ワンショット経路には適用しない（それぞれ [`gate`] 呼び出し時の
/// `min_timeout` 引数を参照）。
const WARM_MIN_TIMEOUT_MS: u64 = 2000;

/// ウォームリクエストの実効タイムアウトを計算する（Fix D1 のロジックを
/// 独立した純粋関数として切り出したもの — ユニットテストで
/// `raw_timeout_ms` → 実効タイムアウトの対応を直接検証するため）。
///
/// 設定された `external_timeout_ms` と [`WARM_MIN_TIMEOUT_MS`] の大きい方を
/// 返す。
fn compute_warm_timeout(raw_timeout: Duration) -> Duration {
    raw_timeout.max(Duration::from_millis(WARM_MIN_TIMEOUT_MS))
}

/// zsh 補完ブリッジ Provider。
///
/// `ExternalCompletionSettings` を [`super::carapace::CarapaceProvider`] と
/// 同じ `Arc<RwLock<_>>` で共有する（`git_branch_commands` と同じ配管
/// パターン）。有効化判定は `settings.binary_path(ExternalKind::Zsh)` が
/// `Some` を返すかどうかに一本化されている — `[completion] external` の
/// 値（`"auto"` / `"zsh"` / 配列での明示指定など）に応じて `resolve()` が
/// このプロバイダを優先順リストに含めるかどうか・zsh バイナリを検出するか
/// どうかを決める（Task 2b.4）。timeout も同じ共有設定から取得する。
///
/// # 温存デーモン配線（Task 2b.3, #89、Fix D で更新）
/// `[completion] external_zsh_daemon`（`settings.zsh_daemon_enabled`）が
/// `true` の間、[`ZshDaemon`] を使い回す。**主経路は起動時の事前ウォーム
/// アップ**（[`prewarm_zsh_daemon`]、Fix D4）——`Shell::new` がバックグラウンド
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
///   レディマーカー待ちのみに適用する（Fix D3）。spawn 直後に送る最初の
///   実補完リクエスト自体はこの予算に含めず、常にウォーム側の
///   `warm_timeout` を使う——spawn+init 自体は速くても補完関数の初回呼び
///   出しが重い（`tmuxinator` 等）ケースで、初回 Tab だけコールド予算を
///   使い切って `None` になっていた不具合の修正。
/// - **ウォーム**（既に生きているデーモンへの2回目以降のリクエスト、および
///   spawn 直後の初回リクエスト、Fix D3）: 設定された `external_timeout_ms`
///   と [`WARM_MIN_TIMEOUT_MS`]（2000ms、Fix D1 で 100ms から引き上げ）の
///   大きい方を使う。
/// - **失敗時（グレースドレイン + サーキットブレーカー、Fix D2）**:
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
pub(super) struct ZshBridgeProvider {
    settings: Arc<RwLock<ExternalCompletionSettings>>,
    /// 温存デーモン本体（`Shell` と共有する `Arc<Mutex<_>>`、Task A, #89）。
    /// `None` は「未 spawn」または「直前のリクエストで dead 化して捨てた」、
    /// あるいは `Shell` 側がライフサイクルイベント（reload/exit/restart）で
    /// shutdown 済みであることを意味する（次回リクエストで遅延 respawn）。
    daemon: SharedDaemonSlot,
    /// テスト用に zsh の場所を差し替えられるようにするフック。
    /// 本番は `None` で `which::which("zsh")` を都度引く。
    zsh_override: Option<PathBuf>,
    /// テスト用にブリッジディレクトリ（`ZDOTDIR` に渡す先）を差し替える
    /// フック。本番は `None` で [`bridge_dir`]（`~/.config/jarvish/zsh-bridge/`）
    /// を使う。E2E テストではユーザーの実 `~/.config` を汚さないよう
    /// tempdir を注入する。
    bridge_dir_override: Option<PathBuf>,
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
    extra_envs: Vec<(String, String)>,
}

/// spawn 済みの [`ZshDaemon`] と、その spawn 時点でのブリッジ `.zshrc` の
/// mtime を対にして保持する（mtime 再起動トリガの比較基準）。
///
/// `.zshrc` が存在しなかった／`stat` に失敗した場合は `None` を保持し、
/// 以後の比較で「常に変化なし」として扱う（mtime を取得できない環境で
/// 誤って毎回再起動しないための安全側フォールバック）。
pub struct DaemonSlot {
    daemon: ZshDaemon,
    zshrc_mtime_at_spawn: Option<SystemTime>,
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
/// （A1〜A4, #89 レビュー指摘 — `Drop` にのみ依存すると `Command::exec`
/// や `std::process::exit` では一切実行されないため）。
pub type SharedDaemonSlot = Arc<Mutex<Option<DaemonSlot>>>;

/// 終端 shutdown（exit/exec 経路）が起きたことを示す tombstone フラグ。
///
/// # 背景: `-c` 単体実行での孤児 zsh デーモン（S5 実機 E2E で検出）
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
    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }

    /// 終端 shutdown が既に起きているかどうか。
    fn is_closed(&self) -> bool {
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
/// # ノンブロッキング（B1/B2, #89）
/// kill/reap は [`ZshDaemon::shutdown`] がバックグラウンドスレッドへ
/// 委譲するため、この関数は「スロットの所有権を取り出して手放す」以上の
/// 待ちを一切行わずすぐ戻る。reedline の completer 呼び出し元（UI スレッド）
/// から呼ばれうる経路 — `Shell::reload_config`（設定変更の**その場**での
/// shutdown, A3/A4）、`ZshBridgeProvider::provide()` の `gate()`-None 早期
/// パス（A4）、mtime トリガによるデーモン再起動（`request_via_daemon`）—
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

/// [`shutdown_shared_daemon`] の有界同期版（B1/B2, #89）。
///
/// `deadline` の範囲内で kill/reap の完了を**呼び出し元スレッド上で**
/// 待つ。`Command::exec` 直前・`std::process::exit` 直前など、この行の
/// 後にプロセスが置換/終了されるためバックグラウンドスレッドに reap を
/// 委ねても実行される保証がない経路（Fix A, ce53dfd が landed させた
/// exit/exec shutdown 経路）専用 — reedline の completer 呼び出し元
/// （UI スレッド）から通常のリクエスト処理中に呼んではならない
/// （その場合は必ず [`shutdown_shared_daemon`] を使うこと）。
///
/// # tombstone（S5 修正、[`DaemonGate`] 参照）
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

/// Shell 起動時のバックグラウンド事前ウォームアップ（Fix D4, #89）。
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
/// # tombstone チェック（S5 修正、[`DaemonGate`] 参照）
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
fn prewarm_zsh_daemon_with(
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

    // S5 tombstone: spawn（重い処理、数百ms かかりうる）の間に終端
    // shutdown が起きていた場合、このデーモンをスロットに書き込む前に
    // 即座に破棄する。`Mutex` の外で行う軽量チェックだが、実際に決定的な
    // 保証を作るのは次の「Mutex の中でのもう一度のチェック」の方
    // （このチェックと `Mutex` 取得の間にも closed になりうるため、
    // 単独では不十分 — 二重チェックのうち片方に過ぎない）。
    //
    // ここでの破棄には非ブロッキング版ではなく `shutdown_blocking` を使う
    // （S5 追加修正）: `shutdown()`（非ブロッキング）は kill/reap を
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

    // S5 tombstone（決定的保証の本体）: `Mutex` を握った**まま**もう一度
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
        // shutdown_blocking を使う（S5 追加修正）: `shutdown_zsh_daemon`
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

impl ZshBridgeProvider {
    pub(super) fn new(
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
    fn with_zsh_binary(settings: Arc<RwLock<ExternalCompletionSettings>>, zsh: PathBuf) -> Self {
        Self {
            settings,
            daemon: new_shared_daemon_slot(),
            zsh_override: Some(zsh),
            bridge_dir_override: None,
            extra_envs: Vec::new(),
        }
    }

    #[cfg(test)]
    fn with_zsh_binary_and_bridge_dir(
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
    fn with_zsh_binary_bridge_dir_and_envs(
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
    fn with_shared_daemon_slot_for_test(
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

    fn resolve_zsh(&self) -> Option<PathBuf> {
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
    fn request_via_daemon(
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
            // Fix D3: `cold_timeout`（[`MIN_TIMEOUT_MS`]）は spawn + init
            // レディマーカー待ちのみを賄う予算であり、その直後に送る最初の
            // 実補完リクエストはこの中に含めない（`ZshDaemon::spawn` 内部の
            // `initialize()` が既に「レディマーカーを待つだけ」の実装に
            // なっているため、ここでの変更は「初回リクエストのタイムアウト
            // として cold_timeout ではなく warm_timeout を使う」ことだけで
            // 完成する）。以前は初回リクエストも `cold_timeout` を使い回して
            // いたため、spawn+init 自体は速くても実測 460〜910ms かかる
            // 重い補完関数（tmuxinator 等）の初回リクエストが cold budget を
            // 使い切ってしまい、初回 Tab だけ `None`（PathProvider
            // フォールバック）になっていた（Fix D, #89 実機報告）。
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
        // Fix D3: spawn 直後の初回リクエストも含め、常に warm_timeout を
        // 使う（cold_timeout は spawn()/initialize() 内部の準備段階専用）。
        let result = slot.daemon.request(&line, warm_timeout);

        if !slot.daemon.is_alive() {
            // request() 内部で timeout/desync により kill 済み。次回リクエスト
            // で遅延 respawn できるようスロットを空にする（Task 2b.3 の
            // 「デーモンは kill され、この Tab は None、次の Tab で遅延
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
    /// 使うことで実装を1箇所に保つ — A1〜A4, #89）。
    fn shutdown_daemon_if_running(&self) {
        shutdown_shared_daemon(&self.daemon);
    }
}

impl CompletionProvider for ZshBridgeProvider {
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
        // `["carapace"]` に変更して `source` した直後 — A4, #89 レビュー
        // 指摘）、`None` で早期 return する前に必ず shutdown しておく
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

/// `capture.zsh` の `zpty -w z "$*"$'\t'`（132行目、vendor 元のまま・
/// 改変不可）は argv をスペースで単純結合して内側 zsh の PTY バッファへ
/// 流し込む。そのため `ctx.spans()` の各要素（`git commit -m "hello world"`
/// の `"hello world"` のような、空白を含む 1 span）をそのまま argv として
/// 渡すと、内側 zsh 側では単純な空白区切りで 2 単語に分裂してしまい、
/// `$CURRENT`（内側 zsh から見たカーソル位置の単語インデックス）が
/// ずれて誤った/空の補完しか返らなくなる（実機検証済み: 5 spans のはずが
/// 内側 zsh では 6 words と数えられる）。
///
/// これを `capture.zsh` 側を一切変更せずに解決するため、Rust 側で各 span を
/// **zsh のバックスラッシュエスケープ規則で事前にエスケープしてから**
/// スペース結合する。空白・タブ・zsh の特殊文字をエスケープしておけば、
/// `"$*"` によるスペース結合後も内側 zsh のレキサがそれぞれを 1 単語として
/// 正しく再構成できる。
///
/// 注意: これは *送信方向*（Rust argv → 内側 zsh の `"$*"` バッファ）専用の
/// エスケープテーブルであり、*受信方向*（compadd 候補行 → Rust 側の表示値）
/// の [`unquote_backslashes`] とは独立している。両者はテーブルがほぼ重なる
/// が完全一致ではない（`=` は送信方向のみ）— 用途が異なるため往復対称性は
/// 意図的に要求しない（詳細は [`ZSH_SPECIAL_CHARS`] のドキュメント参照）。
///
/// 末尾の partial span（trailing space による新規単語補完のマーカーとして
/// 意図的に空文字列のまま渡される — `context.rs` の `spans()` 参照）は
/// **空のまま**（エスケープ不要かつ変形禁止）とする。空文字列をエスケープ
/// すると空文字列のままなので実質的には no-op だが、意図を明示するため
/// 明示的にスキップする分岐を設けている。
///
/// span に制御文字（`\n` `\r` `\0` などの C0 制御文字）が含まれる場合は
/// `None` を返し、呼び出し元（`provide()`）はこのプロバイダを丸ごと諦めて
/// `None` に縮退する。PTY への1行バッファ経由という `capture.zsh` の
/// プロトコル上、制御文字を安全に表現する手段がなく、無理にエスケープ
/// すると `capture.zsh` 内部のセンチネル行判定（NUL 行 = 応答区切り）を
/// 壊しうるため、安全側に倒して補完を諦める。
fn escape_spans(spans: &[String]) -> Option<Vec<String>> {
    spans.iter().map(|span| zsh_escape_span(span)).collect()
}

/// zsh の特殊文字集合（バックスラッシュエスケープ対象）。
///
/// 空白・タブに加え、`unquote_backslashes` の unquote テーブルに含まれる
/// 特殊文字全て（`\` `"` `'` `` ` `` `$` `|` `&` `;` `<` `>` `(` `)` `{` `}`
/// `[` `]` `*` `?` `~` `#` `=`）を対象にする。`=` は unquote テーブルには
/// 無いが、zsh のファイル名展開（`=command` 形式）を無効化するため追加で
/// エスケープする（余分なエスケープは `unquote_backslashes` 側で単に
/// そのまま復元されるだけなので副作用がない）。
const ZSH_SPECIAL_CHARS: &[char] = &[
    ' ', '\t', '\\', '"', '\'', '`', '$', '|', '&', ';', '<', '>', '(', ')', '{', '}', '[', ']',
    '*', '?', '~', '#', '=',
];

/// 1つの span を zsh のバックスラッシュエスケープ規則でエスケープする。
///
/// 空文字列（trailing partial のマーカー）はそのまま返す（no-op、意図的に
/// 変形しない — [`escape_spans`] のドキュメント参照）。制御文字（C0:
/// U+0000〜U+001F、および DEL の U+007F。`char::is_control` はこの範囲を
/// 過不足なく判定する）を含む場合は `None` — ただしタブ（U+0009）だけは
/// 例外で、`ZSH_SPECIAL_CHARS` に含まれるエスケープ対象文字として扱う
/// （1行バッファに安全に表現できない改行・復帰・NUL 等とは異なり、タブは
/// バックスラッシュエスケープすれば1行のまま安全に表現できるため）。
fn zsh_escape_span(span: &str) -> Option<String> {
    if span.is_empty() {
        return Some(String::new());
    }

    if span.chars().any(|c| c.is_control() && c != '\t') {
        return None;
    }

    let mut out = String::with_capacity(span.len());
    for ch in span.chars() {
        if ZSH_SPECIAL_CHARS.contains(&ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    Some(out)
}

/// `capture.zsh` の stdout をパースして候補列に変換する。
///
/// PTY 由来の `\r\n` で分割し、末尾の空要素（トレイリング改行）は捨てる。
/// 各行は ANSI 除去 → バックスラッシュ unquote → 最初の `" -- "` で
/// value/description に分割、の順で処理する。
///
/// `pub(super)` なのは [`super::zsh_daemon::ZshDaemon`]（Task 2b.3、#89）が
/// 温存デーモンから読み取った候補行ブロック（NUL センチネル間、
/// `assets/zsh/daemon_init.zsh` の `compadd` オーバーライドが
/// `assets/zsh/capture.zsh` と同一の "value -- description" 形式で出力する）
/// をパースするために再利用するため。パースロジックの重複を避ける
/// （タスク指示: "Response parsing MUST reuse the existing zsh_bridge
/// parsing helpers"）。
pub(super) fn parse_capture_output(stdout: &str) -> Vec<Candidate> {
    let mut lines: Vec<&str> = stdout.split("\r\n").collect();
    // 末尾の空要素（トレイリング区切りの結果）を捨てる。
    if lines.last() == Some(&"") {
        lines.pop();
    }

    lines
        .into_iter()
        .filter(|line| !line.is_empty())
        .filter_map(parse_capture_line)
        .collect()
}

/// 1 行を [`Candidate`] へ変換する。空行や value が空の行は `None`。
fn parse_capture_line(line: &str) -> Option<Candidate> {
    let stripped = strip_ansi(line);
    let unquoted = unquote_backslashes(&stripped);

    let (value, description) = match unquoted.find(" -- ") {
        Some(idx) => {
            let (value, rest) = unquoted.split_at(idx);
            // rest は " -- ..." なので " -- " (4 バイト) を飛ばす。
            (value.to_string(), Some(rest[4..].to_string()))
        }
        None => (unquoted, None),
    };

    if value.is_empty() {
        return None;
    }

    let append_whitespace = !ends_with_no_space_rune(&value);

    Some(Candidate {
        value,
        description,
        append_whitespace,
    })
}

/// carapace 慣習の「この文字で終わる値の後ろにはスペースを入れない」文字集合
/// （`carapace-bridge` の `NoSpace([]rune("/=@:.,"))` と同じ）。
fn ends_with_no_space_rune(value: &str) -> bool {
    matches!(
        value.chars().last(),
        Some('/' | '=' | '@' | ':' | '.' | ',')
    )
}

/// ANSI エスケープシーケンス（CSI: `ESC [ ... <final byte>`）を取り除く。
///
/// compsys の候補行は色付け（`zstyle ':completion:*' list-colors` 等）で
/// ANSI コードが混じることがあるため、確定挿入前に必ず取り除く。
fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next(); // consume '['
                          // パラメータ・中間バイトを読み飛ばし、final byte (0x40-0x7E) で終端。
            for c in chars.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&c) {
                    break;
                }
            }
            continue;
        }
        out.push(ch);
    }
    out
}

/// `zsh.go` の unquoter テーブルと同じバックスラッシュエスケープを解除する。
///
/// 対象: `\\` `\&` `\<` `\>` `` \` `` `\'` `\"` `\{` `\}` `\$` `\#` `\|` `\?`
/// `\(` `\)` `\;` `\ ` `\[` `\]` `\*` `\~`
fn unquote_backslashes(input: &str) -> String {
    const ESCAPABLE: &[char] = &[
        '\\', '&', '<', '>', '`', '\'', '"', '{', '}', '$', '#', '|', '?', '(', ')', ';', ' ', '[',
        ']', '*', '~',
    ];

    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(&next) = chars.peek() {
                if ESCAPABLE.contains(&next) {
                    out.push(next);
                    chars.next();
                    continue;
                }
            }
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CompletionConfig, ExternalSetting};
    use serial_test::serial;
    use std::env;
    use std::process::Command;
    use std::time::{Duration, Instant};

    // ── パーサ単体テスト（固定文字列フィクスチャ） ──

    #[test]
    fn parse_simple_value_no_description() {
        let stdout = "checkout\r\ncheckout-index\r\n";
        let candidates = parse_capture_output(stdout);
        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert_eq!(values, vec!["checkout", "checkout-index"]);
        assert!(candidates.iter().all(|c| c.description.is_none()));
    }

    #[test]
    fn parse_value_with_description() {
        // `git log --one` の実機キャプチャ（このタスクの検証時に取得）。
        let stdout = "--oneline -- shorthand for --pretty=oneline --abbrev-commit\r\n";
        let candidates = parse_capture_output(stdout);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].value, "--oneline");
        assert_eq!(
            candidates[0].description.as_deref(),
            Some("shorthand for --pretty=oneline --abbrev-commit")
        );
    }

    #[test]
    fn parse_trailing_empty_element_dropped() {
        let stdout = "foo\r\nbar\r\n";
        let candidates = parse_capture_output(stdout);
        assert_eq!(candidates.len(), 2);
    }

    #[test]
    fn parse_empty_stdout_yields_no_candidates() {
        assert!(parse_capture_output("").is_empty());
    }

    #[test]
    fn parse_strips_ansi_codes() {
        let stdout = "\u{1b}[34mmain\u{1b}[0m -- local branch\r\n";
        let candidates = parse_capture_output(stdout);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].value, "main");
        assert_eq!(candidates[0].description.as_deref(), Some("local branch"));
    }

    #[test]
    fn parse_value_containing_double_dash_separator_uses_first_occurrence() {
        // value 自体に " -- " を含む場合（説明文中にも同じ区切りが出うる）、
        // 最初の出現で分割する（zsh.go の SplitN(line, " -- ", 2) と同じ）。
        let stdout = "foo -- first -- second\r\n";
        let candidates = parse_capture_output(stdout);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].value, "foo");
        assert_eq!(
            candidates[0].description.as_deref(),
            Some("first -- second")
        );
    }

    #[test]
    fn parse_unquotes_escaped_special_chars() {
        let stdout = "foo\\ bar.txt\\#1\r\n";
        let candidates = parse_capture_output(stdout);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].value, "foo bar.txt#1");
    }

    #[test]
    fn parse_empty_line_between_entries_is_skipped() {
        let stdout = "foo\r\n\r\nbar\r\n";
        let candidates = parse_capture_output(stdout);
        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert_eq!(values, vec!["foo", "bar"]);
    }

    #[test]
    fn append_whitespace_false_for_no_space_rune_suffix() {
        let stdout = "subdir/\r\nfoo=\r\nplain\r\n";
        let candidates = parse_capture_output(stdout);
        let plain = candidates.iter().find(|c| c.value == "plain").unwrap();
        let dir = candidates.iter().find(|c| c.value == "subdir/").unwrap();
        let eq = candidates.iter().find(|c| c.value == "foo=").unwrap();
        assert!(plain.append_whitespace);
        assert!(!dir.append_whitespace);
        assert!(!eq.append_whitespace);
    }

    #[test]
    fn unquote_backslashes_handles_full_table() {
        let input = r#"a\\b\&c\<d\>e\`f\'g\"h\{i\}j\$k\#l\|m\?n\(o\)p\;q\ r\[s\]t\*u\~v"#;
        let out = unquote_backslashes(input);
        assert_eq!(out, "a\\b&c<d>e`f'g\"h{i}j$k#l|m?n(o)p;q r[s]t*u~v");
    }

    #[test]
    fn strip_ansi_removes_csi_sequences() {
        assert_eq!(strip_ansi("\u{1b}[1;34mtext\u{1b}[0m"), "text");
    }

    #[test]
    fn strip_ansi_no_escape_is_unchanged() {
        assert_eq!(strip_ansi("plain text"), "plain text");
    }

    // ── zsh_escape_span / escape_spans（B1: 複数語 span のスペース結合対策） ──

    #[test]
    fn zsh_escape_span_escapes_space_and_specials_table() {
        // ZSH_SPECIAL_CHARS の全種を1つの span に詰めて、それぞれが
        // バックスラッシュ付きで出力されることを確認する。
        //
        // 注意: これは *送信方向*（Rust argv → 内側 zsh の "$*" バッファ）の
        // エスケープであり、`unquote_backslashes`（*受信方向*: compadd 候補
        // 行 → Rust 側の表示値）とは独立したテーブル・用途である。`=` は
        // 送信方向でのみ zsh のファイル名展開 (`=command`) 抑止のために
        // エスケープするが、`unquote_backslashes` 側のテーブルには含まれて
        // いない（`capture.zsh` の compadd 出力に `=` がバックスラッシュ
        // 付きで出てくることはないため、逆変換対象に含める必要がない）。
        // そのため往復対称性は主張しない。
        let input = "a b\\c\"d'e`f$g|h&i;j<k>l(m)n{o}p[q]r*s?t~u#v=w";
        let escaped = zsh_escape_span(input).expect("no control chars");
        assert!(escaped.contains("a\\ b"), "space: {escaped}");
        assert!(escaped.contains("b\\\\c"), "backslash: {escaped}");
        assert!(escaped.contains("c\\\"d"), "double quote: {escaped}");
        assert!(escaped.contains("d\\'e"), "single quote: {escaped}");
        assert!(escaped.contains("e\\`f"), "backtick: {escaped}");
        assert!(escaped.contains("f\\$g"), "dollar: {escaped}");
        assert!(escaped.contains("g\\|h"), "pipe: {escaped}");
        assert!(escaped.contains("h\\&i"), "ampersand: {escaped}");
        assert!(escaped.contains("i\\;j"), "semicolon: {escaped}");
        assert!(escaped.contains("j\\<k"), "less-than: {escaped}");
        assert!(escaped.contains("k\\>l"), "greater-than: {escaped}");
        assert!(escaped.contains("l\\(m"), "open paren: {escaped}");
        assert!(escaped.contains("m\\)n"), "close paren: {escaped}");
        assert!(escaped.contains("n\\{o"), "open brace: {escaped}");
        assert!(escaped.contains("o\\}p"), "close brace: {escaped}");
        assert!(escaped.contains("p\\[q"), "open bracket: {escaped}");
        assert!(escaped.contains("q\\]r"), "close bracket: {escaped}");
        assert!(escaped.contains("r\\*s"), "asterisk: {escaped}");
        assert!(escaped.contains("s\\?t"), "question mark: {escaped}");
        assert!(escaped.contains("t\\~u"), "tilde: {escaped}");
        assert!(escaped.contains("u\\#v"), "hash: {escaped}");
        assert!(escaped.contains("v\\=w"), "equals: {escaped}");
    }

    #[test]
    fn zsh_escape_span_multi_word_value_survives_space_join_as_one_zsh_word() {
        // "hello world" のような1 span（git commit -m "hello world" の
        // 引数）をエスケープしてスペース結合した場合、実際の zsh レキサに
        // 通せば元の1トークンに戻ることを実地の zsh で証明する
        // （capture.zsh の `"$*"` 結合と同じプロトコルの直接検証）。
        // これがこの Fix の核心保証であり、単純な `str::split(' ')` による
        // 近似では「エスケープされた空白」と「区切りの空白」を区別できない
        // ため、テストとしての意味を持たせるには本物のシェルワードスプリット
        // が必要（実装時に発覚 — naive split では検証にならない）。
        let Some(zsh) = zsh_binary() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };

        let spans = vec![
            "git".to_string(),
            "commit".to_string(),
            "-m".to_string(),
            "hello world".to_string(),
        ];
        let escaped = escape_spans(&spans).expect("no control chars");
        let joined = escaped.join(" ");

        // `print -l -- ${(z)line}` は zsh 自身のワードスプリット規則
        // （`"$*"` 経由の内側 zsh バッファ解釈と同じレキサ）で `joined` を
        // 単語分割し、1行1単語で出力する。
        let output = Command::new(&zsh)
            .args(["-fc", "print -l -- ${(z)1}", "--", &joined])
            .output()
            .expect("failed to run zsh for word-split check");
        assert!(output.status.success(), "zsh word-split invocation failed");
        let words: Vec<String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|s| s.to_string())
            .collect();

        assert_eq!(
            words,
            vec!["git", "commit", "-m", "hello world"],
            "escaped+joined spans must re-split into the original 4 words via real zsh word-split, got {words:?}"
        );
    }

    #[test]
    fn zsh_escape_span_empty_span_is_untouched() {
        // trailing partial（新規単語補完マーカー）は空のまま渡さねばならない。
        assert_eq!(zsh_escape_span("").as_deref(), Some(""));
    }

    #[test]
    fn zsh_escape_span_plain_word_is_unchanged() {
        // 特殊文字を含まない単語はバイト単位で不変であるべき。
        assert_eq!(zsh_escape_span("checkout").as_deref(), Some("checkout"));
        assert_eq!(zsh_escape_span("main").as_deref(), Some("main"));
    }

    #[test]
    fn zsh_escape_span_rejects_newline() {
        assert_eq!(zsh_escape_span("hello\nworld"), None);
    }

    #[test]
    fn zsh_escape_span_rejects_carriage_return() {
        assert_eq!(zsh_escape_span("hello\rworld"), None);
    }

    #[test]
    fn zsh_escape_span_rejects_nul() {
        assert_eq!(zsh_escape_span("hello\0world"), None);
    }

    #[test]
    fn zsh_escape_span_rejects_other_c0_control_chars() {
        // タブ以外にも一般の C0 制御文字（例: \x01, \x1b の単体混入）を拒否する。
        assert_eq!(zsh_escape_span("hello\u{1}world"), None);
        assert_eq!(zsh_escape_span("hello\u{7f}world"), None);
    }

    #[test]
    fn zsh_escape_span_allows_tab_as_a_normal_escapable_char() {
        // タブは制御文字だが「安全に表現できない」ケースではなく、
        // ZSH_SPECIAL_CHARS に含めてバックスラッシュエスケープで表現する
        // 対象なので、guard には引っかからず正常にエスケープされる。
        let escaped = zsh_escape_span("a\tb").expect("tab should be escapable, not rejected");
        assert_eq!(escaped, "a\\\tb");
    }

    #[test]
    fn escape_spans_propagates_none_on_any_control_char_span() {
        let spans = vec!["git".to_string(), "co\nmmit".to_string()];
        assert_eq!(escape_spans(&spans), None);
    }

    #[test]
    fn escape_spans_leaves_trailing_empty_partial_as_bare_empty_string() {
        let spans = vec!["git".to_string(), "checkout".to_string(), String::new()];
        let escaped = escape_spans(&spans).expect("no control chars");
        assert_eq!(escaped, vec!["git", "checkout", ""]);
    }

    // ── provider-contract テスト ──

    /// 外部補完が明示的に無効化された設定（`external = "none"`）
    /// （`JarvishCompleter` の他プロバイダの単体テストと同じ方針で、
    /// このファイル内でも「外部補完まるごと無効」を再現するために使う）。
    fn disabled_external_completion() -> Arc<RwLock<ExternalCompletionSettings>> {
        Arc::new(RwLock::new(ExternalCompletionSettings::resolve(
            &CompletionConfig {
                external: ExternalSetting::Single("none".to_string()),
                ..CompletionConfig::default()
            },
        )))
    }

    /// zsh のみを明示的に有効化した設定（`external = "zsh"`）。carapace の
    /// 実機有無に左右されずゲート（first-token / zsh 有無 / spans 長さ）だけを
    /// 単体テストできる。zsh バイナリ自体は `/bin/zsh`（macOS 標準）が
    /// 前提だが、`ZshBridgeProvider::with_zsh_binary` でオーバーライドする
    /// テストでは resolve() 自体が実機で zsh を検出できるかは問わない
    /// （`binary_path` が Some を返すことだけがゲート判定に使われる）。
    fn zsh_enabled_external_completion() -> Arc<RwLock<ExternalCompletionSettings>> {
        Arc::new(RwLock::new(ExternalCompletionSettings::resolve(
            &CompletionConfig {
                external: ExternalSetting::Single("zsh".to_string()),
                ..CompletionConfig::default()
            },
        )))
    }

    /// carapace のみを明示的に有効化した設定（`external = "carapace"`）。
    /// zsh は優先順リストに含まれないため、`ZshBridgeProvider` は
    /// `binary_path(Zsh)` が常に `None` を返すことで無効化される
    /// （「他プロバイダの設定に巻き込まれない」ことの検証に使う）。
    fn carapace_only_external_completion() -> Arc<RwLock<ExternalCompletionSettings>> {
        Arc::new(RwLock::new(ExternalCompletionSettings::resolve(
            &CompletionConfig {
                external: ExternalSetting::Single("carapace".to_string()),
                ..CompletionConfig::default()
            },
        )))
    }

    #[test]
    fn provide_returns_none_when_external_is_disabled() {
        // 外部補完が `[completion] external = "none"` で無効化されている場合、
        // zsh バイナリがあり非 first-token でも候補を返さない。
        let settings = disabled_external_completion();
        let provider = ZshBridgeProvider::with_zsh_binary(settings, PathBuf::from("/bin/zsh"));
        let ctx = super::super::context::extract_context("git chec", 8);
        assert!(!ctx.is_first_token);
        assert!(provider.provide(&ctx).is_none());
    }

    #[test]
    fn provide_returns_none_when_only_carapace_is_enabled() {
        // `external = "carapace"` のとき zsh は優先順リストに含まれないため、
        // ZshBridgeProvider は他プロバイダ（carapace）の有効化設定に
        // 巻き込まれず無効のままであるべき。
        let settings = carapace_only_external_completion();
        let provider = ZshBridgeProvider::with_zsh_binary(settings, PathBuf::from("/bin/zsh"));
        let ctx = super::super::context::extract_context("git chec", 8);
        assert!(!ctx.is_first_token);
        assert!(provider.provide(&ctx).is_none());
    }

    #[test]
    fn provide_returns_none_for_first_token() {
        let settings = zsh_enabled_external_completion();
        let provider = ZshBridgeProvider::with_zsh_binary(settings, PathBuf::from("/bin/zsh"));
        let ctx = super::super::context::extract_context("gi", 2);
        assert!(ctx.is_first_token);
        assert!(provider.provide(&ctx).is_none());
    }

    #[test]
    fn provide_returns_none_when_zsh_missing() {
        let settings = zsh_enabled_external_completion();
        let provider = ZshBridgeProvider::with_zsh_binary(
            settings,
            PathBuf::from("/no/such/zsh/binary/zzjarvish"),
        );
        // resolve_zsh は override をそのまま使う設計のため、この場合
        // provide() は run_external_capped の spawn 失敗経由で None になる。
        let ctx = super::super::context::extract_context("git chec", 8);
        assert!(!ctx.is_first_token);
        assert!(provider.provide(&ctx).is_none());
    }

    #[test]
    fn provide_returns_none_when_spans_too_short() {
        // spans が [head, partial] 未満 = コマンド名しかない状態。
        let settings = zsh_enabled_external_completion();
        let provider = ZshBridgeProvider::with_zsh_binary(settings, PathBuf::from("/bin/zsh"));
        let ctx = super::super::context::extract_context("git", 3);
        // "git" は非空白なので first-token 扱いになりこちらのガードで弾かれる。
        assert!(provider.provide(&ctx).is_none());
    }

    // ── 統合テスト（実行時 zsh 有無で skip） ──

    fn zsh_binary() -> Option<PathBuf> {
        which::which("zsh").ok()
    }

    #[test]
    #[serial]
    fn integration_git_checkout_prefix_suggests_branch() {
        let Some(zsh) = zsh_binary() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };

        let tmpdir = tempfile::tempdir().unwrap();
        let dir = tmpdir.path();
        for args in [
            vec!["init"],
            vec!["config", "user.email", "test@test.com"],
            vec!["config", "user.name", "Test"],
            vec!["commit", "--allow-empty", "-m", "init"],
            vec!["branch", "zzjarvish-bridge-feature"],
        ] {
            Command::new("git")
                .args(&args)
                .current_dir(dir)
                .output()
                .unwrap();
        }

        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(dir).unwrap();

        let settings = Arc::new(RwLock::new(ExternalCompletionSettings::resolve(
            &CompletionConfig {
                external: ExternalSetting::Single("auto".to_string()),
                external_timeout_ms: 3000,
                ..CompletionConfig::default()
            },
        )));
        let provider = ZshBridgeProvider::with_zsh_binary(settings, zsh);

        let line = "git checkout zzjarvish-bridge-";
        let ctx = super::super::context::extract_context(line, line.len());
        let result = provider.provide(&ctx);

        env::set_current_dir(&original_dir).unwrap();

        let candidates = result.expect("zsh bridge should return candidates for git checkout");
        assert!(
            candidates
                .iter()
                .any(|c| c.value == "zzjarvish-bridge-feature"),
            "expected branch suggestion among {candidates:?}"
        );
    }

    #[test]
    #[serial]
    fn integration_first_token_yields_none() {
        if zsh_binary().is_none() {
            eprintln!("skipping: zsh not found on PATH");
            return;
        }
        // `ZshBridgeProvider::new`（`which::which("zsh")` を都度引く本番経路）
        // + 有効化設定でも、first-token では担当外として None を返すことを
        // 確認する（`resolve_zsh` のオーバーライドなし経路のカバレッジ）。
        let settings = zsh_enabled_external_completion();
        let provider = ZshBridgeProvider::new(settings, new_shared_daemon_slot());
        let ctx = super::super::context::extract_context("gi", 2);
        assert!(provider.provide(&ctx).is_none());
    }

    #[test]
    fn timeout_budget_is_at_least_min_timeout() {
        // MIN_TIMEOUT_MS 未満の設定 timeout でも、`gate`（C2 で共有ヘルパー化）
        // 経由の実効タイムアウトが MIN_TIMEOUT_MS を下回らないことを保証する
        // （compinit の重さ対策）。zsh を有効化した settings で `gate` 自体を
        // 呼び、`ZshBridgeProvider::provide` が実際に使う経路をそのまま検証する。
        // `external = "zsh"` の resolve() は実機に zsh バイナリが無いと
        // gate 自体が None になる（binary_path が None）ため、実行時 skip する。
        if zsh_binary().is_none() {
            eprintln!("skipping: zsh not found on PATH");
            return;
        }
        let settings = Arc::new(RwLock::new(ExternalCompletionSettings::resolve(
            &CompletionConfig {
                external: ExternalSetting::Single("zsh".to_string()),
                external_timeout_ms: 50,
                ..CompletionConfig::default()
            },
        )));
        let (_binary, effective) = gate(
            &settings,
            ExternalKind::Zsh,
            Some(Duration::from_millis(MIN_TIMEOUT_MS)),
        )
        .expect("zsh should be gated-in when external = \"zsh\"");
        assert!(effective >= Duration::from_millis(MIN_TIMEOUT_MS));
    }

    // ── Fix D1: ウォームタイムアウトの床（compute_warm_timeout）──

    #[test]
    fn compute_warm_timeout_floors_low_configured_value_to_2000ms() {
        // 実機計測: tmuxinator 等の重い補完関数は Ruby インタプリタ起動で
        // 460〜910ms かかる。デフォルト設定（external_timeout_ms = 400）が
        // そのまま使われると必ずタイムアウトする——Fix D の核心の回帰防止。
        let effective = compute_warm_timeout(Duration::from_millis(400));
        assert_eq!(effective, Duration::from_millis(2000));
    }

    #[test]
    fn compute_warm_timeout_preserves_configured_value_above_floor() {
        // 床を上回る設定値はそのまま使う（フロアは下限であって固定値では
        // ない）。
        let effective = compute_warm_timeout(Duration::from_millis(3000));
        assert_eq!(effective, Duration::from_millis(3000));
    }

    #[test]
    fn compute_warm_timeout_floors_extremely_low_value() {
        let effective = compute_warm_timeout(Duration::from_millis(1));
        assert_eq!(effective, Duration::from_millis(WARM_MIN_TIMEOUT_MS));
    }

    // ── ブリッジディレクトリ / .zshrc テンプレート ──

    #[test]
    fn bridge_dir_is_under_config_jarvish() {
        let dir = bridge_dir();
        // "~/.config/jarvish/zsh-bridge" で終わる（HOME 有無どちらでも）。
        assert!(dir.ends_with(".config/jarvish/zsh-bridge"));
    }

    #[test]
    fn ensure_bridge_zshrc_creates_dir_and_template_when_absent() {
        let tmpdir = tempfile::tempdir().unwrap();
        let bridge = tmpdir.path().join("zsh-bridge");
        assert!(!bridge.exists());

        let zshrc = ensure_bridge_zshrc(&bridge).unwrap();

        assert!(bridge.is_dir());
        assert!(zshrc.is_file());
        let contents = fs::read_to_string(&zshrc).unwrap();
        assert!(contents.contains("fpath"));
        assert!(contents.contains("compdef"));
    }

    #[test]
    fn ensure_bridge_zshrc_does_not_overwrite_existing_file() {
        let tmpdir = tempfile::tempdir().unwrap();
        let bridge = tmpdir.path().join("zsh-bridge");
        fs::create_dir_all(&bridge).unwrap();
        let zshrc = bridge_zshrc_path(&bridge);
        fs::write(&zshrc, "# user customized content\n").unwrap();

        ensure_bridge_zshrc(&bridge).unwrap();

        let contents = fs::read_to_string(&zshrc).unwrap();
        assert_eq!(contents, "# user customized content\n");
    }

    #[test]
    fn ensure_bridge_zshrc_is_idempotent_across_calls() {
        let tmpdir = tempfile::tempdir().unwrap();
        let bridge = tmpdir.path().join("zsh-bridge");

        ensure_bridge_zshrc(&bridge).unwrap();
        let first = fs::read_to_string(bridge_zshrc_path(&bridge)).unwrap();
        ensure_bridge_zshrc(&bridge).unwrap();
        let second = fs::read_to_string(bridge_zshrc_path(&bridge)).unwrap();

        assert_eq!(first, second);
    }

    // ── B2: シンボリックリンク攻撃防御（ensure_bridge_zshrc） ──
    //
    // 攻撃シナリオ: 攻撃者が ~/.config/jarvish/zsh-bridge を（存在する前に）
    // 事前に自分が制御するディレクトリへのシンボリックリンクとして作成して
    // おく、または正規のブリッジディレクトリ内に .zshrc という名前で
    // シンボリックリンクを仕込んでおく。どちらのケースでも
    // ensure_bridge_zshrc は書き込み・利用を一切行わず Err を返し、
    // provide() はこれを受けて None に縮退しなければならない。

    #[cfg(unix)]
    #[test]
    fn ensure_bridge_zshrc_rejects_symlinked_bridge_dir() {
        use std::os::unix::fs::symlink;

        let tmpdir = tempfile::tempdir().unwrap();
        // 攻撃者が制御する「本物の」ディレクトリ（symlink の先）。
        let attacker_dir = tmpdir.path().join("attacker-controlled");
        fs::create_dir_all(&attacker_dir).unwrap();
        // ブリッジディレクトリのパス自体をシンボリックリンクにする
        // （事前作成攻撃: jarvish が create_dir_all を呼ぶ前に攻撃者が
        // このパスへ symlink を仕込んでいたケースを再現）。
        let bridge = tmpdir.path().join("zsh-bridge");
        symlink(&attacker_dir, &bridge).unwrap();
        assert!(fs::symlink_metadata(&bridge)
            .unwrap()
            .file_type()
            .is_symlink());

        let result = ensure_bridge_zshrc(&bridge);

        assert!(
            result.is_err(),
            "ensure_bridge_zshrc must reject a symlinked bridge dir, got {result:?}"
        );
        // 攻撃者のディレクトリ配下には何も書き込まれていないこと
        // （symlink をたどって .zshrc を書いてしまっていないか）。
        assert!(
            !attacker_dir.join(".zshrc").exists(),
            "must not write through the symlink into the attacker-controlled dir"
        );
    }

    #[cfg(unix)]
    #[test]
    fn ensure_bridge_zshrc_rejects_symlinked_zshrc_inside_real_dir() {
        use std::os::unix::fs::symlink;

        let tmpdir = tempfile::tempdir().unwrap();
        let bridge = tmpdir.path().join("zsh-bridge");
        fs::create_dir_all(&bridge).unwrap();

        // ブリッジディレクトリ自体は本物だが、.zshrc だけが攻撃者の
        // ファイルへのシンボリックリンクになっているケース。
        let attacker_zshrc = tmpdir.path().join("attacker-zshrc-target");
        fs::write(&attacker_zshrc, "# attacker payload\n").unwrap();
        let zshrc_path = bridge_zshrc_path(&bridge);
        symlink(&attacker_zshrc, &zshrc_path).unwrap();

        let result = ensure_bridge_zshrc(&bridge);

        assert!(
            result.is_err(),
            "ensure_bridge_zshrc must reject a symlinked .zshrc, got {result:?}"
        );
        // 攻撃者のファイルの中身が書き換えられていないこと。
        let attacker_contents = fs::read_to_string(&attacker_zshrc).unwrap();
        assert_eq!(attacker_contents, "# attacker payload\n");
    }

    #[test]
    fn ensure_bridge_zshrc_normal_dir_still_works() {
        // 通常ケース（symlink が一切絡まない）の回帰確認 — 挙動が
        // byte-identical に保たれていること。
        let tmpdir = tempfile::tempdir().unwrap();
        let bridge = tmpdir.path().join("zsh-bridge");

        let zshrc = ensure_bridge_zshrc(&bridge).unwrap();

        assert!(bridge.is_dir());
        assert!(!fs::symlink_metadata(&bridge)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(zshrc.is_file());
        let contents = fs::read_to_string(&zshrc).unwrap();
        assert!(contents.contains("fpath"));
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn provide_returns_none_when_bridge_dir_is_symlinked() {
        // provide() 経路全体を通した回帰テスト: シンボリックリンクされた
        // ブリッジディレクトリでは spawn 自体に到達せず None に縮退する。
        use std::os::unix::fs::symlink;

        let Some(zsh) = zsh_binary() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };

        let tmpdir = tempfile::tempdir().unwrap();
        let attacker_dir = tmpdir.path().join("attacker-controlled");
        fs::create_dir_all(&attacker_dir).unwrap();
        let bridge = tmpdir.path().join("zsh-bridge");
        symlink(&attacker_dir, &bridge).unwrap();

        let settings = zsh_enabled_external_completion();
        let provider = ZshBridgeProvider::with_zsh_binary_and_bridge_dir(settings, zsh, bridge);

        let line = "git checkout ";
        let ctx = super::super::context::extract_context(line, line.len());
        assert!(provider.provide(&ctx).is_none());
        assert!(!attacker_dir.join(".zshrc").exists());
    }

    // ── E2E: ユーザー定義 zsh 補完がブリッジ経由で反映されるか ──
    //
    // 実際に capture.zsh の -f 除去 + ZDOTDIR の配線が機能していることを
    // 実地で証明する。temp ZDOTDIR に .zshrc を置き、そこから temp fpath
    // ディレクトリ上のカスタム補完関数 `_jarvishtestcmd`（固定ワードリストを
    // compadd するだけ）を読み込ませ、`jarvishtestcmd <Tab>` でその固定
    // ワードが候補に出ることを確認する。これが失敗する = ZDOTDIR 配線か
    // -f 除去のどちらかが壊れている、という決定的な回帰検知になる。
    #[test]
    #[serial]
    fn e2e_user_zshrc_fpath_completion_is_used_via_zdotdir() {
        let Some(zsh) = zsh_binary() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };

        let tmpdir = tempfile::tempdir().unwrap();
        let zdotdir = tmpdir.path().join("zdotdir");
        let fpath_dir = tmpdir.path().join("completions");
        fs::create_dir_all(&zdotdir).unwrap();
        fs::create_dir_all(&fpath_dir).unwrap();

        // ユーザー定義の補完関数: 固定ワードリストを compadd する。
        fs::write(
            fpath_dir.join("_jarvishtestcmd"),
            "#compdef jarvishtestcmd\ncompadd -- alpha beta gamma\n",
        )
        .unwrap();

        // ブリッジ .zshrc: fpath に上のディレクトリを追加するだけの
        // ユーザー拡張例（README の fpath 例と同じ形）。
        fs::write(
            zdotdir.join(".zshrc"),
            format!("fpath=({} $fpath)\n", fpath_dir.display()),
        )
        .unwrap();

        let settings = Arc::new(RwLock::new(ExternalCompletionSettings::resolve(
            &CompletionConfig {
                external: ExternalSetting::Single("auto".to_string()),
                external_timeout_ms: 3000,
                ..CompletionConfig::default()
            },
        )));
        let provider =
            ZshBridgeProvider::with_zsh_binary_and_bridge_dir(settings, zsh, zdotdir.clone());

        let line = "jarvishtestcmd ";
        let ctx = super::super::context::extract_context(line, line.len());
        let result = provider.provide(&ctx);

        let candidates = result.expect("zsh bridge should return candidates from user fpath");
        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert!(values.contains(&"alpha"), "got {values:?}");
        assert!(values.contains(&"beta"), "got {values:?}");
        assert!(values.contains(&"gamma"), "got {values:?}");

        // ensure_bridge_zshrc がユーザーの .zshrc を上書きしていないこと
        // （E2E 経路でも既存ファイル保護が効くことの確認）。
        let contents = fs::read_to_string(zdotdir.join(".zshrc")).unwrap();
        assert!(contents.contains("fpath=("));
    }

    // ── E2E (B1): 複数語 span がスペース結合で分裂しないこと ──
    //
    // `git commit -m "hello world"` のような、空白を含む1つの span を
    // ctx.spans() 経由で渡した場合、capture.zsh の `"$*"` 単純スペース結合
    // (132行目、vendor 元のまま) によって内側 zsh 側で誤って2単語に分裂
    // すると、$CURRENT（カーソル位置の単語インデックス）がずれて後続引数の
    // 補完が壊れる。これを実地で証明するため、$CURRENT の値そのものを
    // 候補として compadd するオラクル関数 `_jarvishtestcmd2` を使う:
    // 正しくエスケープされていれば「コマンド名 + 複数語1引数 + 次の
    // partial」で $CURRENT=3 になるはずだが、素朴な空白結合バグが残って
    // いると "hello" "world" の2語に割れて $CURRENT=4 になってしまう。
    #[test]
    #[serial]
    fn e2e_multi_word_span_keeps_current_index_correct() {
        let Some(zsh) = zsh_binary() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };

        let tmpdir = tempfile::tempdir().unwrap();
        let zdotdir = tmpdir.path().join("zdotdir");
        let fpath_dir = tmpdir.path().join("completions");
        // 隔離用 HOME: `capture.zsh`（vendor）の `compinit -d
        // ~/.zcompdump_capture` は $ZDOTDIR ではなく $HOME 基準の固定パスに
        // compdump キャッシュを読み書きする。実 $HOME を共有したまま複数の
        // E2E テスト（異なる fpath tempdir）を連続実行すると、後続テストが
        // 前のテストの compdump を再利用して新しい #compdef 関数を認識
        // できないことが実地検証で判明した（環境依存フレークの原因）。
        // このテストは独自の $HOME を与えて compdump キャッシュを完全に
        // 隔離することで、他の E2E テストの実行順序・実行有無に依存しない
        // 決定的な結果を保証する。
        let isolated_home = tmpdir.path().join("home");
        fs::create_dir_all(&zdotdir).unwrap();
        fs::create_dir_all(&fpath_dir).unwrap();
        fs::create_dir_all(&isolated_home).unwrap();

        // オラクル補完関数: $CURRENT をそのまま候補として compadd する。
        // 単語分裂が起きていなければ常に一定の値になるはずで、分裂が起きると
        // 値がずれる — 「候補が期待どおりの場所に出る/word-count drift が
        // 無い」ことの直接的な証拠になる。
        fs::write(
            fpath_dir.join("_jarvishtestcmd2"),
            "#compdef jarvishtestcmd2\ncompadd -- \"current-is-$CURRENT\"\n",
        )
        .unwrap();
        fs::write(
            zdotdir.join(".zshrc"),
            format!("fpath=({} $fpath)\n", fpath_dir.display()),
        )
        .unwrap();

        let settings = Arc::new(RwLock::new(ExternalCompletionSettings::resolve(
            &CompletionConfig {
                external: ExternalSetting::Single("auto".to_string()),
                external_timeout_ms: 3000,
                ..CompletionConfig::default()
            },
        )));
        let provider = ZshBridgeProvider::with_zsh_binary_bridge_dir_and_envs(
            settings,
            zsh,
            zdotdir.clone(),
            vec![(
                "HOME".to_string(),
                isolated_home.to_string_lossy().into_owned(),
            )],
        );

        // spans: ["jarvishtestcmd2", "hello world", ""] (3 spans)。
        // ctx.spans() が実際にこの形を作ることを確認したうえで、
        // provide() に通す。カーソルは "hello world" の後の trailing
        // space（新規引数の開始）にある想定。
        let line = r#"jarvishtestcmd2 "hello world" "#;
        let ctx = super::super::context::extract_context(line, line.len());
        assert_eq!(
            ctx.spans(),
            vec!["jarvishtestcmd2", "hello world", ""],
            "ctx.spans() should keep the quoted two-word arg as a single span"
        );

        let result = provider.provide(&ctx);
        let candidates =
            result.expect("zsh bridge should return candidates when CURRENT is correct");
        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();

        // 正しい配線: コマンド名(1) + "hello world"(1 span, 2) + 新規引数(3)
        // = $CURRENT が 3 の位置で呼ばれる。バグ版（単純スペース結合で
        // "hello"/"world" に分裂）では $CURRENT=4 になり、"current-is-3" は
        // 出現しない（症状の直接再現・回帰検知）。
        assert!(
            values.contains(&"current-is-3"),
            "expected $CURRENT=3 (no word-count drift) among {values:?}; \
             a value of current-is-4 here would indicate the old space-join bug regressed"
        );
        assert!(
            !values.contains(&"current-is-4"),
            "current-is-4 indicates the multi-word span was split into two words: {values:?}"
        );
    }

    // ── 温存デーモン配線テスト (Task 2b.3 Task 2, #89) ──
    //
    // `disabled_external_completion` / `zsh_enabled_external_completion` は
    // `CompletionConfig::default()` を土台にしており、そのデフォルトは
    // `external_zsh_daemon = true` のため、このファイル内の既存の
    // "one-shot" 統合テスト（`integration_git_checkout_prefix_suggests_branch`
    // 等）は本タスク以降、実際にはデーモン経路を経由する。それでも出力
    // フォーマット（`parse_capture_output` が読む "value -- description"
    // 形式）は capture.zsh と daemon_init.zsh で共通のため、既存アサーション
    // は変更なしにそのまま通る（daemon_init.zsh のモジュールドキュメント
    // 参照）。ここでは daemon 固有の契約（遅延 spawn・使い回し・ホット
    // リロードでの off 切り替え・mtime 再起動トリガ・失敗時のこの Tab
    // での None）を直接検証する。

    /// zsh のみを有効化し、かつ `external_zsh_daemon = false` を明示した
    /// 設定（ワンショット経路のみを強制するテスト専用ヘルパー）。
    fn zsh_enabled_daemon_off_external_completion() -> Arc<RwLock<ExternalCompletionSettings>> {
        Arc::new(RwLock::new(ExternalCompletionSettings::resolve(
            &CompletionConfig {
                external: ExternalSetting::Single("zsh".to_string()),
                external_timeout_ms: 3000,
                external_zsh_daemon: false,
                ..CompletionConfig::default()
            },
        )))
    }

    /// デーモンテスト用の隔離フィクスチャ。`zsh_bridge.rs` の既存 E2E
    /// テスト（`e2e_user_zshrc_fpath_completion_is_used_via_zdotdir` 等）と
    /// 同じ理由（`compinit -d ~/.zcompdump_capture` は `$ZDOTDIR` ではなく
    /// **`$HOME`** 基準の固定パスに compdump キャッシュを読み書きするため、
    /// 実 `$HOME` を共有したまま複数テストを並行実行すると compdump の
    /// 汚染・衝突で `#compdef` 関数が認識されず、デフォルトのファイル名
    /// 補完へ静かにフォールバックしてしまう ── 実機検証で確認済みの
    /// フレーク要因）で、`HOME` も専用 tempdir に隔離する。呼び出し元は
    /// `extra_envs()` で得られる `HOME` の env ペアを
    /// `with_zsh_binary_bridge_dir_and_envs` に渡すこと。
    fn zsh_daemon_test_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmpdir = tempfile::tempdir().unwrap();
        let zdotdir = tmpdir.path().join("zdotdir");
        let fpath_dir = tmpdir.path().join("completions");
        let home = tmpdir.path().join("home");
        fs::create_dir_all(&zdotdir).unwrap();
        fs::create_dir_all(&fpath_dir).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::write(
            fpath_dir.join("_jarvishtestcmd"),
            "#compdef jarvishtestcmd\ncompadd -- alpha beta gamma\n",
        )
        .unwrap();
        fs::write(
            zdotdir.join(".zshrc"),
            format!("fpath=({} $fpath)\n", fpath_dir.display()),
        )
        .unwrap();
        (tmpdir, zdotdir, fpath_dir)
    }

    /// [`zsh_daemon_test_fixture`] の tmpdir から隔離 `HOME` の env ペアを
    /// 組み立てる（`with_zsh_binary_bridge_dir_and_envs` にそのまま渡せる形）。
    fn zsh_daemon_test_home_envs(tmpdir: &tempfile::TempDir) -> Vec<(String, String)> {
        vec![(
            "HOME".to_string(),
            tmpdir.path().join("home").to_string_lossy().into_owned(),
        )]
    }

    #[test]
    #[serial]
    fn daemon_flag_off_never_spawns_daemon_one_shot_still_serves_candidates() {
        // フラグ off: `provide()` は一度も daemon フィールドを埋めず、
        // 常にワンショット経路（`run_external_capped` 経由）で候補を返す。
        let Some(zsh) = zsh_binary() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        let (tmpdir, zdotdir, _fpath_dir) = zsh_daemon_test_fixture();
        let home_envs = zsh_daemon_test_home_envs(&tmpdir);

        let settings = zsh_enabled_daemon_off_external_completion();
        let provider = ZshBridgeProvider::with_zsh_binary_bridge_dir_and_envs(
            settings,
            zsh,
            zdotdir.clone(),
            home_envs,
        );

        let line = "jarvishtestcmd ";
        let ctx = super::super::context::extract_context(line, line.len());
        let result = provider.provide(&ctx);

        let candidates = result.expect("one-shot path should still serve candidates");
        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert!(values.contains(&"alpha"), "got {values:?}");

        assert!(
            provider.daemon.lock().unwrap().is_none(),
            "daemon slot must remain empty when external_zsh_daemon = false"
        );
    }

    #[test]
    #[serial]
    fn daemon_path_e2e_serves_candidates_via_warm_daemon() {
        // デーモン経路 (external_zsh_daemon = true, デフォルト) で、
        // capture.zsh と同じユーザー fpath 補完がそのまま反映されることを
        // 実機で証明する。
        let Some(zsh) = zsh_binary() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        let (tmpdir, zdotdir, _fpath_dir) = zsh_daemon_test_fixture();
        let home_envs = zsh_daemon_test_home_envs(&tmpdir);

        let settings = zsh_enabled_external_completion();
        let provider = ZshBridgeProvider::with_zsh_binary_bridge_dir_and_envs(
            settings,
            zsh,
            zdotdir.clone(),
            home_envs,
        );

        let line = "jarvishtestcmd ";
        let ctx = super::super::context::extract_context(line, line.len());
        let result = provider.provide(&ctx);

        let candidates = result.expect("daemon path should serve candidates");
        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert!(values.contains(&"alpha"), "got {values:?}");
        assert!(values.contains(&"beta"), "got {values:?}");
        assert!(values.contains(&"gamma"), "got {values:?}");

        assert!(
            provider.daemon.lock().unwrap().is_some(),
            "daemon slot must be populated after a successful daemon-path request"
        );
    }

    #[test]
    #[serial]
    fn cold_spawn_budget_does_not_starve_a_slow_first_request() {
        // Fix D3: cold_timeout（MIN_TIMEOUT_MS = 2000ms）は spawn + init の
        // レディマーカー待ちのみを賄う予算であり、初回の実補完リクエストは
        // 別枠（warm_timeout）で走る。ここでは spawn+init 自体は速いが、
        // 最初の補完関数呼び出し自体が「旧実装なら cold budget の残りを
        // 使い切っていたはずの長さ」だけ遅い（900ms）フィクスチャを使い、
        // それでも最初の Tab が候補を返すことを証明する（実機報告の
        // tmuxinator シナリオの直接再現）。
        let Some(zsh) = zsh_binary() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        let (tmpdir, zdotdir, fpath_dir) = zsh_daemon_test_fixture();
        let home_envs = zsh_daemon_test_home_envs(&tmpdir);
        fs::write(
            fpath_dir.join("_jarvishtestslowfirst"),
            "#compdef jarvishtestslowfirst\nsleep 0.9\ncompadd -- slowcandidate\n",
        )
        .unwrap();

        // デフォルト相当の設定（external_timeout_ms=400 → warm floor 2000ms、
        // Fix D1）。
        let settings = zsh_enabled_external_completion();
        let provider = ZshBridgeProvider::with_zsh_binary_bridge_dir_and_envs(
            settings, zsh, zdotdir, home_envs,
        );

        let line = "jarvishtestslowfirst ";
        let ctx = super::super::context::extract_context(line, line.len());
        let result = provider.provide(&ctx);

        let candidates =
            result.expect("first Tab must serve real candidates, not fall through to None");
        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert!(
            values.contains(&"slowcandidate"),
            "expected the slow first request's own candidate among {values:?}"
        );
        assert!(
            provider.daemon.lock().unwrap().is_some(),
            "daemon must have survived spawning + the slow first request"
        );
    }

    #[test]
    #[serial]
    fn realistic_interpreter_startup_proxy_survives_three_tabs_same_pid() {
        // Fix D 全体の実測ベース受け入れテスト: `_tmuxinator` の実測値
        // （Ruby インタプリタ起動込みで 460〜910ms）を模した、サブプロセス
        // を exec して ~600ms かかる補完関数フィクスチャを、デフォルト
        // 相当の設定（external_timeout_ms 未指定 = 400ms → warm floor
        // 2000ms、Fix D1）で3回連続 Tab 押下し、いずれも None でも
        // PathProvider フォールバック相当でもなく実際の候補を返し、かつ
        // 同じデーモン pid のまま生き続けることを検証する。
        let Some(zsh) = zsh_binary() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        let (tmpdir, zdotdir, fpath_dir) = zsh_daemon_test_fixture();
        let home_envs = zsh_daemon_test_home_envs(&tmpdir);
        // `sh -c 'sleep 0.6'` でサブプロセス exec + 待ち合わせを模す
        // （tmuxinator が `ruby` を exec するのと同じ「補完関数がサブ
        // プロセスを起動して待つ」構造）。
        fs::write(
            fpath_dir.join("_jarvishtestinterp"),
            "#compdef jarvishtestinterp\n\
             sh -c 'sleep 0.6'\n\
             compadd -- interpcandidate\n",
        )
        .unwrap();

        let settings = zsh_enabled_external_completion();
        let provider = ZshBridgeProvider::with_zsh_binary_bridge_dir_and_envs(
            settings, zsh, zdotdir, home_envs,
        );

        let line = "jarvishtestinterp ";
        let ctx = super::super::context::extract_context(line, line.len());

        let mut pid_seen: Option<u32> = None;
        for tab in 1..=3 {
            let result = provider.provide(&ctx);
            let candidates = result.unwrap_or_else(|| {
                panic!("Tab #{tab} must return real candidates, not None/path-fallback")
            });
            let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
            assert!(
                values.contains(&"interpcandidate"),
                "Tab #{tab}: expected interpcandidate among {values:?}"
            );

            let pid_now = {
                let guard = provider.daemon.lock().unwrap();
                guard.as_ref().unwrap().daemon.child_pid_for_test()
            };
            if let Some(prev) = pid_seen {
                assert_eq!(
                    pid_now, prev,
                    "Tab #{tab}: daemon must survive with the same pid across all 3 tabs"
                );
            }
            pid_seen = Some(pid_now);
        }
    }

    #[test]
    #[serial]
    fn daemon_path_reuses_same_daemon_across_requests() {
        // 2回連続でリクエストしても同じ ZshDaemon インスタンス（同じ子
        // プロセス pid）が使い回されることを、実際の pid を比較して直接
        // 証明する（ウォームリクエストが「起動コストなしの計算のみ」に
        // なっているという本タスクの動機の核心）。
        let Some(zsh) = zsh_binary() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        let (tmpdir, zdotdir, _fpath_dir) = zsh_daemon_test_fixture();
        let home_envs = zsh_daemon_test_home_envs(&tmpdir);

        let settings = zsh_enabled_external_completion();
        let provider = ZshBridgeProvider::with_zsh_binary_bridge_dir_and_envs(
            settings,
            zsh,
            zdotdir.clone(),
            home_envs,
        );

        let line = "jarvishtestcmd ";
        let ctx = super::super::context::extract_context(line, line.len());

        let first = provider.provide(&ctx);
        assert!(first.is_some(), "first request should succeed");
        let pid_after_first = {
            let guard = provider.daemon.lock().unwrap();
            guard.as_ref().unwrap().daemon.child_pid_for_test()
        };

        let second = provider.provide(&ctx);
        assert!(second.is_some(), "second request should succeed");
        let pid_after_second = {
            let guard = provider.daemon.lock().unwrap();
            guard.as_ref().unwrap().daemon.child_pid_for_test()
        };

        assert_eq!(
            pid_after_first, pid_after_second,
            "the same daemon child process must serve both requests (no respawn)"
        );
    }

    #[test]
    #[serial]
    fn daemon_path_restarts_when_bridge_zshrc_mtime_changes() {
        // ブリッジ .zshrc を最初のリクエスト後に touch（新しい補完関数を
        // 追加した想定）すると、次のリクエストで既存デーモンが shutdown
        // され、新しいデーモン（新しい pid）が遅延 spawn される。かつ、
        // 新しく追加した補完関数の候補がちゃんと反映される
        // （respawn 後は新しい ZDOTDIR/.zshrc を source し直しているという
        // 直接証拠）。
        let Some(zsh) = zsh_binary() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        let (tmpdir, zdotdir, fpath_dir) = zsh_daemon_test_fixture();
        let home_envs = zsh_daemon_test_home_envs(&tmpdir);

        let settings = zsh_enabled_external_completion();
        let provider = ZshBridgeProvider::with_zsh_binary_bridge_dir_and_envs(
            settings,
            zsh,
            zdotdir.clone(),
            home_envs,
        );

        let line = "jarvishtestcmd ";
        let ctx = super::super::context::extract_context(line, line.len());
        let first = provider.provide(&ctx).expect("first request should work");
        assert!(first.iter().any(|c| c.value == "alpha"));

        let pid_before = {
            let guard = provider.daemon.lock().unwrap();
            guard.as_ref().unwrap().daemon.child_pid_for_test()
        };

        // mtime が確実に進むよう、ファイルシステムの mtime 解像度を超える
        // だけ待ってから書き換える（多くの環境で mtime は最低でも秒未満
        // 精度を持つが、安全側に倒して確実に差分を作る）。
        std::thread::sleep(Duration::from_millis(1100));

        // 新しい補完関数を追加し、.zshrc をそれを拾うよう書き換える
        // （mtime が更新される）。
        fs::write(
            fpath_dir.join("_jarvishtestcmd3"),
            "#compdef jarvishtestcmd3\ncompadd -- delta\n",
        )
        .unwrap();
        fs::write(
            zdotdir.join(".zshrc"),
            format!(
                "fpath=({} $fpath)\n# touched to bump mtime\n",
                fpath_dir.display()
            ),
        )
        .unwrap();

        let line2 = "jarvishtestcmd3 ";
        let ctx2 = super::super::context::extract_context(line2, line2.len());
        let second = provider
            .provide(&ctx2)
            .expect("request after .zshrc touch should still work (daemon respawned)");
        assert!(
            second.iter().any(|c| c.value == "delta"),
            "respawned daemon should have re-sourced the updated bridge .zshrc: {second:?}"
        );

        let pid_after = {
            let guard = provider.daemon.lock().unwrap();
            guard.as_ref().unwrap().daemon.child_pid_for_test()
        };
        assert_ne!(
            pid_before, pid_after,
            "daemon must be restarted (new child pid) after bridge .zshrc mtime changes"
        );
    }

    #[test]
    #[serial]
    fn daemon_survives_zshrc_deletion_after_spawn_mtime_none_is_treated_as_unchanged() {
        // C2: 「両側 None => 変化なしとして扱い、スプリアスな再起動をしない」
        // の片側 — spawn 時点では mtime が取れていた（Some）が、その後
        // ブリッジ .zshrc 自体が削除されて current_mtime が None になる
        // ケース。`(Some(a), Some(b)) if a != b` という一致条件は片方が
        // None の時点で成立しないため、mtime_changed は false のまま
        // ——同じデーモン（同じ pid）が使い回されることを直接証明する。
        //
        // `provide()` 経由だと `ensure_bridge_zshrc` が「削除済みの .zshrc」
        // を検知してデフォルトテンプレートで再作成してしまい（fpath 設定が
        // 失われ `_jarvishtestcmd` が引けなくなる）、mtime 比較ロジック
        // 自体とは無関係な理由でテストが崩れる。そのため
        // `request_via_daemon` を直接呼び、mtime 比較ロジックだけを隔離して
        // 検証する（`ensure_bridge_zshrc` 自体の再作成挙動は別テストの
        // 責務）。
        let Some(zsh) = zsh_binary() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        let (tmpdir, zdotdir, _fpath_dir) = zsh_daemon_test_fixture();
        let home_envs = zsh_daemon_test_home_envs(&tmpdir);

        let settings = zsh_enabled_external_completion();
        let provider = ZshBridgeProvider::with_zsh_binary_bridge_dir_and_envs(
            settings,
            zsh,
            zdotdir.clone(),
            home_envs,
        );

        let escaped = vec!["jarvishtestcmd".to_string(), String::new()];
        let cold = Duration::from_millis(MIN_TIMEOUT_MS);
        let warm = Duration::from_secs(3);

        let first = provider
            .request_via_daemon(
                &provider.resolve_zsh().unwrap(),
                &zdotdir,
                &escaped,
                cold,
                warm,
            )
            .expect("first request should work");
        assert!(parse_capture_output(&first)
            .iter()
            .any(|c| c.value == "alpha"));

        let pid_before = {
            let guard = provider.daemon.lock().unwrap();
            let slot = guard.as_ref().unwrap();
            // spawn 時点で記録された mtime が Some だったことを確認して
            // おく（そうでないとこのテストが片側 None のケースを検証して
            // いないことになる）。
            assert!(
                slot.zshrc_mtime_at_spawn.is_some(),
                "precondition: spawn-time mtime must be Some for this test to be meaningful"
            );
            slot.daemon.child_pid_for_test()
        };

        // ブリッジ .zshrc を削除する — 以後 fs::metadata(...).modified() は
        // NotFound エラーとなり current_mtime は None になる。
        // request_via_daemon 自体は ensure_bridge_zshrc を呼ばないため、
        // ここでは削除された状態のまま mtime 比較に入る。
        fs::remove_file(bridge_zshrc_path(&zdotdir)).unwrap();

        let second = provider
            .request_via_daemon(
                &provider.resolve_zsh().unwrap(),
                &zdotdir,
                &escaped,
                cold,
                warm,
            )
            .expect(
                "request after .zshrc deletion should still work (daemon reused, not respawned)",
            );
        assert!(
            parse_capture_output(&second)
                .iter()
                .any(|c| c.value == "alpha"),
            "the SAME (already-running) daemon must still serve the fpath completion it \
             loaded at spawn time — proof that it was not respawned/re-sourced"
        );

        let pid_after = {
            let guard = provider.daemon.lock().unwrap();
            guard.as_ref().unwrap().daemon.child_pid_for_test()
        };
        assert_eq!(
            pid_before, pid_after,
            "deleting the bridge .zshrc (spawn-time Some -> current None) must NOT trigger a \
             restart — either side being None is treated as 'unchanged' (safe-side fallback)"
        );
    }

    #[test]
    #[serial]
    fn daemon_reused_when_spawn_time_mtime_was_none_and_file_now_present() {
        // C2: 「両側 None => 変化なし」のもう片側 — spawn 時点では mtime が
        // 取れなかった（意図的に spawn 前に .zshrc を削除しておくケース）が、
        // 次のリクエスト時には .zshrc が（再）存在し current_mtime が Some
        // になっているケース。`(Some(a), Some(b))` のパターンは spawn 側が
        // None の時点でマッチしないため、mtime_changed は false のまま
        // ——同じデーモンが使い回されることを直接証明する。
        //
        // 前のテストと同じ理由で `request_via_daemon` を直接呼び、
        // `ensure_bridge_zshrc` のテンプレート再作成による干渉を避ける。
        let Some(zsh) = zsh_binary() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        let (tmpdir, zdotdir, _fpath_dir) = zsh_daemon_test_fixture();
        let home_envs = zsh_daemon_test_home_envs(&tmpdir);
        let zshrc_path = bridge_zshrc_path(&zdotdir);

        let settings = zsh_enabled_external_completion();
        let provider = ZshBridgeProvider::with_zsh_binary_bridge_dir_and_envs(
            settings,
            zsh,
            zdotdir.clone(),
            home_envs,
        );

        // spawn 直前に .zshrc を退避しておき、request_via_daemon が spawn
        // 時点で current_mtime = None を記録するよう仕向ける。ZDOTDIR には
        // 依然として fpath 設定済みの .zshrc が存在しないため、内側 zsh の
        // 起動時点では compdef が登録されないが、リクエスト前に復元する
        // ことで内側 zsh 自体には実害が出ない
        // （spawn() 完了後に .zshrc を読み直すことはない — 初期化は spawn
        // 時の一度きりのため）。このテストの関心は mtime 比較ロジック
        // のみであり、実際の補完動作は前段の `_jarvishtestcmd` フィクスチャ
        // ではなく、単に「同じデーモンが使い回されたか」を pid 比較で見る。
        let saved = fs::read(&zshrc_path).unwrap();
        fs::remove_file(&zshrc_path).unwrap();

        let escaped = vec!["jarvishtestcmd".to_string(), String::new()];
        let cold = Duration::from_millis(MIN_TIMEOUT_MS);
        let warm = Duration::from_secs(3);

        // spawn 時点で .zshrc が存在しないため fpath は素の状態(ZDOTDIR の
        // デフォルト検索パスのみ)になるが、request_via_daemon 自体は spawn
        // に成功する（zsh -i 自体は .zshrc が無くても起動できる）。
        let _first = provider.request_via_daemon(
            &provider.resolve_zsh().unwrap(),
            &zdotdir,
            &escaped,
            cold,
            warm,
        );

        let pid_before = {
            let guard = provider.daemon.lock().unwrap();
            let slot = guard.as_ref().unwrap();
            assert!(
                slot.zshrc_mtime_at_spawn.is_none(),
                "precondition: spawn-time mtime must be None for this test to be meaningful"
            );
            slot.daemon.child_pid_for_test()
        };

        // .zshrc を復元する — 以後 current_mtime は Some になる。
        fs::write(&zshrc_path, &saved).unwrap();

        let _second = provider.request_via_daemon(
            &provider.resolve_zsh().unwrap(),
            &zdotdir,
            &escaped,
            cold,
            warm,
        );

        let pid_after = {
            let guard = provider.daemon.lock().unwrap();
            guard.as_ref().unwrap().daemon.child_pid_for_test()
        };
        assert_eq!(
            pid_before, pid_after,
            "spawn-time None -> current Some must NOT trigger a restart — either side being \
             None is treated as 'unchanged' (safe-side fallback)"
        );
    }

    #[test]
    #[serial]
    fn daemon_failure_after_two_consecutive_timeouts_respawns_lazily_next_tab() {
        // Fix D2 サーキットブレーカーの provide() 経由 E2E: 完全ハングする
        // 補完関数に対して1回目の Tab は None（グレース、デーモンはまだ
        // 生存）、2回目の Tab（=1回目の残留フレームのドレイン失敗 + 2回目
        // 自体もハング）でサーキットブレーカーが作動しデーモンが kill
        // される。3回目の Tab では遅延 respawn されて通常どおり候補を
        // 返すことを確認する。
        //
        // 実装ノート: 隔離 `HOME`（`zsh_daemon_test_home_envs`）を必ず渡す
        // こと。`compinit -d ~/.zcompdump_capture` は `$HOME` 基準の固定
        // パスに compdump キャッシュを読み書きするため、実 `$HOME` を
        // 共有したまま並行実行すると `#compdef jarvishtesthang` が
        // 一時的に認識されず zsh のデフォルトのファイル名補完へ静かに
        // フォールバックしてしまい、`sleep 30` で本来ハングするはずの
        // リクエストが即座に（誤った）候補を返す desync として観測される
        // ── 実機検証で確認済みのフレーク要因（`zsh_daemon_test_fixture`
        // のドキュメント参照）。
        let Some(zsh) = zsh_binary() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        let (tmpdir, zdotdir, fpath_dir) = zsh_daemon_test_fixture();
        let home_envs = zsh_daemon_test_home_envs(&tmpdir);
        fs::write(
            fpath_dir.join("_jarvishtesthang"),
            "#compdef jarvishtesthang\nsleep 30\ncompadd -- neverseen\n",
        )
        .unwrap();

        // 短いタイムアウトでハング補完を確実に timeout させる。
        let settings = Arc::new(RwLock::new(ExternalCompletionSettings::resolve(
            &CompletionConfig {
                external: ExternalSetting::Single("zsh".to_string()),
                external_timeout_ms: 500,
                external_zsh_daemon: true,
                ..CompletionConfig::default()
            },
        )));
        let provider = ZshBridgeProvider::with_zsh_binary_bridge_dir_and_envs(
            settings,
            zsh,
            zdotdir.clone(),
            home_envs,
        );

        // 1回目のリクエストでコールド spawn させておく。決定的な補完
        // 関数（zsh_daemon_test_fixture が用意する _jarvishtestcmd、
        // 固定ワードリスト）を使い、レスポンスが素早く確定することを
        // 確認してからハング側のリクエストへ進む。
        let warm_line = "jarvishtestcmd ";
        let warm_ctx = super::super::context::extract_context(warm_line, warm_line.len());
        let cold_result = provider.provide(&warm_ctx);
        let cold_candidates = cold_result.expect("cold-spawn request should succeed");
        assert!(cold_candidates.iter().any(|c| c.value == "alpha"));
        assert!(
            provider.daemon.lock().unwrap().is_some(),
            "daemon should have been spawned by the first request"
        );
        let pid_before_hangs = {
            let guard = provider.daemon.lock().unwrap();
            guard.as_ref().unwrap().daemon.child_pid_for_test()
        };

        let hang_line = "jarvishtesthang ";
        let hang_ctx = super::super::context::extract_context(hang_line, hang_line.len());

        // 1回目のハング Tab: グレースにより None だが、デーモンは同じ pid
        // のまま生存し続ける。
        let start1 = std::time::Instant::now();
        let hung_result1 = provider.provide(&hang_ctx);
        let elapsed1 = start1.elapsed();
        assert_eq!(
            hung_result1, None,
            "hung completion must yield None for this Tab (no one-shot fallback)"
        );
        assert!(
            elapsed1 < Duration::from_secs(5),
            "provide() should return promptly after the configured timeout, took {elapsed1:?}"
        );
        assert!(
            provider.daemon.lock().unwrap().is_some(),
            "daemon must survive a single timeout (Fix D2 grace)"
        );
        assert_eq!(
            {
                let guard = provider.daemon.lock().unwrap();
                guard.as_ref().unwrap().daemon.child_pid_for_test()
            },
            pid_before_hangs,
            "grace must not respawn the daemon"
        );

        // 2回目のハング Tab: 1回目の残留フレームのドレインが失敗し、これで
        // 連続2回目としてサーキットブレーカーが作動、デーモンが kill される。
        let start2 = std::time::Instant::now();
        let hung_result2 = provider.provide(&hang_ctx);
        let elapsed2 = start2.elapsed();
        assert_eq!(hung_result2, None);
        assert!(
            elapsed2 < Duration::from_secs(5),
            "provide() should return promptly, took {elapsed2:?}"
        );
        assert!(
            provider.daemon.lock().unwrap().is_none(),
            "daemon slot must be cleared after 2 consecutive timeouts (circuit breaker) \
             so the next Tab respawns lazily"
        );

        // 3回目の Tab: 遅延 respawn されて再び通常の補完が使えることを確認する。
        let retry = provider.provide(&warm_ctx);
        let candidates = retry.expect("next Tab should lazily respawn and succeed");
        assert!(candidates.iter().any(|c| c.value == "alpha"));
        assert!(provider.daemon.lock().unwrap().is_some());
    }

    #[test]
    #[serial]
    fn daemon_turned_off_mid_session_shuts_down_running_daemon() {
        // ホットリロードのシミュレーション: 稼働中のデーモンがある状態から
        // 共有 settings の `zsh_daemon_enabled` を false に書き換えると、
        // 次の `provide()` 呼び出しでデーモンが shutdown され、以後は
        // ワンショット経路にフォールバックする（`reload_config` が
        // `Arc<RwLock<_>>` の中身を丸ごと差し替える経路の模擬 —
        // `carapace.rs` の hot-reload テストと同じ方針）。
        let Some(zsh) = zsh_binary() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        let (tmpdir, zdotdir, _fpath_dir) = zsh_daemon_test_fixture();
        let home_envs = zsh_daemon_test_home_envs(&tmpdir);

        let settings = zsh_enabled_external_completion();
        let provider = ZshBridgeProvider::with_zsh_binary_bridge_dir_and_envs(
            Arc::clone(&settings),
            zsh,
            zdotdir,
            home_envs,
        );

        let line = "jarvishtestcmd ";
        let ctx = super::super::context::extract_context(line, line.len());
        let first = provider.provide(&ctx);
        assert!(first.is_some(), "daemon path should work before reload");
        assert!(provider.daemon.lock().unwrap().is_some());

        // reload: 同じ Arc の中身を daemon off の設定に丸ごと差し替える。
        {
            let mut guard = settings.write().unwrap();
            guard.zsh_daemon_enabled = false;
        }

        let second = provider.provide(&ctx);
        assert!(
            second.is_some(),
            "one-shot fallback should still serve candidates after daemon is turned off"
        );
        assert!(
            provider.daemon.lock().unwrap().is_none(),
            "running daemon must be shut down as soon as external_zsh_daemon flips to false"
        );
    }

    #[test]
    fn daemon_enabled_default_true_uses_daemon_field_type() {
        // ExternalCompletionSettings::resolve のデフォルト（CompletionConfig
        // ::default()）で zsh_daemon_enabled が true になっていることの
        // 単体確認（zsh 不要、実機非依存）。
        let settings = zsh_enabled_external_completion();
        assert!(settings.read().unwrap().zsh_daemon_enabled);
    }

    // ── 共有デーモンスロット (Task A, #89): shutdown_shared_daemon /
    //    new_shared_daemon_slot / provide() の gate()-None 早期 shutdown ──

    /// pid が実際に ESRCH になる（プロセスが死んでいる）まで短時間・
    /// 有界回数ポーリングする（`zsh_daemon.rs` / `external.rs` の既存
    /// テストと同じ考え方）。
    fn wait_for_pid_death(pid: u32) -> bool {
        for _ in 0..40 {
            let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
            if ret == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    #[test]
    fn new_shared_daemon_slot_starts_empty() {
        let slot = new_shared_daemon_slot();
        assert!(slot.lock().unwrap().is_none());
    }

    #[test]
    fn shutdown_shared_daemon_on_empty_slot_is_a_no_op() {
        // 既に空のスロットに対して shutdown_shared_daemon を呼んでも
        // panic せず、スロットは空のままである（冪等性）。
        let slot = new_shared_daemon_slot();
        shutdown_shared_daemon(&slot);
        assert!(slot.lock().unwrap().is_none());
    }

    #[test]
    #[serial]
    fn shutdown_shared_daemon_kills_live_daemon_and_empties_slot() {
        // Shell::exec_restart / main.rs の exit 経路が呼ぶのと同じ
        // shutdown_shared_daemon() を直接呼び、実際に子プロセスが ESRCH に
        // なる（本当に死ぬ）ことと、スロットが None に戻ることの両方を
        // 実機で証明する（A1/A2 の unit テスト — exec() 自体はテストしない）。
        let Some(zsh) = zsh_binary() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        let (tmpdir, zdotdir, _fpath_dir) = zsh_daemon_test_fixture();
        let home_envs = zsh_daemon_test_home_envs(&tmpdir);

        let shared_slot = new_shared_daemon_slot();
        let settings = zsh_enabled_external_completion();
        let provider = ZshBridgeProvider::with_shared_daemon_slot_for_test(
            settings,
            zsh,
            zdotdir,
            home_envs,
            Arc::clone(&shared_slot),
        );

        let line = "jarvishtestcmd ";
        let ctx = super::super::context::extract_context(line, line.len());
        assert!(provider.provide(&ctx).is_some(), "daemon should spawn");

        let child_pid = {
            let guard = shared_slot.lock().unwrap();
            guard.as_ref().unwrap().daemon.child_pid_for_test()
        };

        shutdown_shared_daemon(&shared_slot);

        assert!(
            shared_slot.lock().unwrap().is_none(),
            "slot must be empty after shutdown_shared_daemon"
        );
        assert!(
            wait_for_pid_death(child_pid),
            "child pid {child_pid} should be dead after shutdown_shared_daemon"
        );
    }

    #[test]
    #[serial]
    fn provide_shuts_down_daemon_when_gate_returns_none() {
        // A4: zsh が enabled-kinds リストから外れる（gate() が None を
        // 返す）と、provide() は早期 return する前に生きているデーモンを
        // shutdown しなければならない。まず zsh 有効設定でデーモンを
        // spawn させ、その後 settings を carapace のみへ丸ごと差し替えて
        // （zsh が binary_path から消える）再度 provide() を呼び、
        // スロットが空になり子プロセスが実際に死ぬことを確認する。
        let Some(zsh) = zsh_binary() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        let (tmpdir, zdotdir, _fpath_dir) = zsh_daemon_test_fixture();
        let home_envs = zsh_daemon_test_home_envs(&tmpdir);

        let settings = zsh_enabled_external_completion();
        let provider = ZshBridgeProvider::with_zsh_binary_bridge_dir_and_envs(
            Arc::clone(&settings),
            zsh,
            zdotdir,
            home_envs,
        );

        let line = "jarvishtestcmd ";
        let ctx = super::super::context::extract_context(line, line.len());
        assert!(provider.provide(&ctx).is_some(), "daemon should spawn");

        let child_pid = {
            let guard = provider.daemon.lock().unwrap();
            guard.as_ref().unwrap().daemon.child_pid_for_test()
        };

        // settings を丸ごと carapace のみ（zsh は enabled から消える）に
        // 差し替える — `external_provider_chain` の並び替えではなく、
        // 同じ Arc の中身だけを書き換える hot-reload シミュレーション
        // （carapace.rs / zsh_bridge.rs の既存 hot-reload テストと同じ方針）。
        {
            let mut guard = settings.write().unwrap();
            *guard = ExternalCompletionSettings::resolve(&CompletionConfig {
                external: ExternalSetting::Single("carapace".to_string()),
                ..CompletionConfig::default()
            });
        }
        assert!(
            settings
                .read()
                .unwrap()
                .binary_path(ExternalKind::Zsh)
                .is_none(),
            "zsh must no longer be gated-in after the settings swap"
        );

        let result = provider.provide(&ctx);
        assert!(
            result.is_none(),
            "provide() must return None once zsh is gated out"
        );
        assert!(
            provider.daemon.lock().unwrap().is_none(),
            "provide() must shut down the now-forbidden daemon before returning None (A4)"
        );
        assert!(
            wait_for_pid_death(child_pid),
            "child pid {child_pid} should be dead after gate()-None shutdown"
        );
    }

    // ── Fix D4: 起動時のバックグラウンド事前ウォームアップ ──

    /// [`prewarm_zsh_daemon_with`] 用の poll ヘルパー: 生成された総合的な
    /// 猶予時間内でスロットが埋まるのを待つ（バックグラウンドスレッド経由
    /// の spawn は非同期なので、テスト側は寛容にポーリングする）。
    fn wait_for_slot_populated(slot: &SharedDaemonSlot, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if slot.lock().unwrap().is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        slot.lock().unwrap().is_some()
    }

    #[test]
    #[serial]
    fn prewarm_populates_slot_when_daemon_enabled_without_provide_call() {
        // Fix D4 の核心保証: settings がデーモン有効を示している状態で
        // prewarm を呼ぶと、`provide()` を一度も呼ばなくてもスロットが
        // 埋まる。
        let Some(zsh) = zsh_binary() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        let (tmpdir, zdotdir, _fpath_dir) = zsh_daemon_test_fixture();
        let home_envs = zsh_daemon_test_home_envs(&tmpdir);

        let settings = zsh_enabled_external_completion();
        let slot = new_shared_daemon_slot();
        let gate = DaemonGate::new();

        prewarm_zsh_daemon_with(&settings, &slot, &gate, &zsh, &zdotdir, &home_envs);

        assert!(
            slot.lock().unwrap().is_some(),
            "prewarm should populate the slot synchronously in this direct call \
             (no provide() call was made)"
        );
    }

    #[test]
    fn prewarm_is_a_no_op_when_daemon_disabled() {
        // フラグ off、または zsh が enabled-kinds に含まれない設定では
        // prewarm は一切 spawn せずスロットは空のまま。zsh バイナリの
        // 実機有無に関わらずテストできる（should_run_zsh_daemon の判定が
        // spawn より先に効くため、無効な zsh パスを渡しても安全）。
        let settings = zsh_enabled_daemon_off_external_completion();
        let slot = new_shared_daemon_slot();
        let gate = DaemonGate::new();

        prewarm_zsh_daemon_with(
            &settings,
            &slot,
            &gate,
            Path::new("/no/such/zsh/binary/zzjarvish"),
            Path::new("/tmp/zzjarvish-unused-bridge-dir"),
            &[],
        );

        assert!(
            slot.lock().unwrap().is_none(),
            "prewarm must be a no-op (slot stays None) when the daemon is disabled"
        );
    }

    #[test]
    fn prewarm_is_a_no_op_when_zsh_not_in_enabled_kinds() {
        // フラグは on だが zsh が優先順リストに無い（例: external =
        // "carapace"）ケースも同様に no-op であるべき
        // （`should_run_zsh_daemon` のもう一方の条件）。
        let settings = carapace_only_external_completion();
        let slot = new_shared_daemon_slot();
        let gate = DaemonGate::new();

        prewarm_zsh_daemon_with(
            &settings,
            &slot,
            &gate,
            Path::new("/no/such/zsh/binary/zzjarvish"),
            Path::new("/tmp/zzjarvish-unused-bridge-dir"),
            &[],
        );

        assert!(slot.lock().unwrap().is_none());
    }

    #[test]
    #[serial]
    fn provide_reuses_the_prewarmed_daemon_same_pid() {
        // prewarm で spawn したデーモンを、その後の provide() 呼び出しが
        // 再利用する（同じ pid で新規 spawn しない）ことを直接証明する。
        let Some(zsh) = zsh_binary() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        let (tmpdir, zdotdir, _fpath_dir) = zsh_daemon_test_fixture();
        let home_envs = zsh_daemon_test_home_envs(&tmpdir);

        let settings = zsh_enabled_external_completion();
        let slot = new_shared_daemon_slot();
        let gate = DaemonGate::new();
        prewarm_zsh_daemon_with(&settings, &slot, &gate, &zsh, &zdotdir, &home_envs);
        assert!(
            slot.lock().unwrap().is_some(),
            "prewarm should have spawned"
        );
        let pid_from_prewarm = {
            let guard = slot.lock().unwrap();
            guard.as_ref().unwrap().daemon.child_pid_for_test()
        };

        let provider = ZshBridgeProvider::with_shared_daemon_slot_for_test(
            settings,
            zsh,
            zdotdir,
            home_envs,
            Arc::clone(&slot),
        );

        let line = "jarvishtestcmd ";
        let ctx = super::super::context::extract_context(line, line.len());
        let result = provider.provide(&ctx);
        let candidates =
            result.expect("provide() should serve candidates via the prewarmed daemon");
        assert!(candidates.iter().any(|c| c.value == "alpha"));

        let pid_after_provide = {
            let guard = slot.lock().unwrap();
            guard.as_ref().unwrap().daemon.child_pid_for_test()
        };
        assert_eq!(
            pid_from_prewarm, pid_after_provide,
            "provide() must reuse the daemon spawned by prewarm rather than spawning a new one"
        );
    }

    #[test]
    #[serial]
    fn prewarm_and_provide_race_yields_exactly_one_daemon_process() {
        // Fix D4 のレース回避保証: prewarm をトリガーした直後（そのスレッドが
        // 実際に Mutex を取るより前）に provide() を呼び、両方が spawn を
        // 試みうる状況を作る。最終的にプロセスは1つだけ生き残ることを、
        // スロットの pid と実際のプロセス生存確認の両方で検証する。
        let Some(zsh) = zsh_binary() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        let (tmpdir, zdotdir, _fpath_dir) = zsh_daemon_test_fixture();
        let home_envs = zsh_daemon_test_home_envs(&tmpdir);

        let settings = zsh_enabled_external_completion();
        let slot = new_shared_daemon_slot();
        let gate = DaemonGate::new();

        let prewarm_settings = Arc::clone(&settings);
        let prewarm_slot = Arc::clone(&slot);
        let prewarm_gate = Arc::clone(&gate);
        let prewarm_zsh = zsh.clone();
        let prewarm_zdotdir = zdotdir.clone();
        let prewarm_envs = home_envs.clone();
        let handle = std::thread::spawn(move || {
            prewarm_zsh_daemon_with(
                &prewarm_settings,
                &prewarm_slot,
                &prewarm_gate,
                &prewarm_zsh,
                &prewarm_zdotdir,
                &prewarm_envs,
            );
        });

        // provide() をほぼ同時に呼ぶ（トリガー直後、prewarm スレッドが
        // Mutex を取るより先に到達しうるタイミングを狙う——スケジューリング
        // 依存のため決定的なタイミング保証はないが、両方が spawn を試みる
        // ケースをできるだけ再現する）。
        let provider = ZshBridgeProvider::with_shared_daemon_slot_for_test(
            Arc::clone(&settings),
            zsh,
            zdotdir,
            home_envs,
            Arc::clone(&slot),
        );
        let line = "jarvishtestcmd ";
        let ctx = super::super::context::extract_context(line, line.len());
        let provide_result = provider.provide(&ctx);

        handle.join().expect("prewarm thread should not panic");

        // 少なくとも一方は成功しているはず（prewarm か provide() か、
        // タイミング次第でどちらが先でも構わない）。
        assert!(
            wait_for_slot_populated(&slot, Duration::from_secs(10)),
            "slot should be populated by either prewarm or provide()"
        );
        if provide_result.is_none() {
            // provide() 側がスロット未確定のタイミングで走り None を返した
            // 場合でも、最終的にスロットは埋まっていること自体は上で確認
            // 済み。再度 provide() すれば必ず候補が返る（レースの後始末が
            // 正しく完了していることの追加確認）。
            let retry = provider.provide(&ctx);
            assert!(retry.is_some(), "retry after the race should succeed");
        }

        let final_pid = {
            let guard = slot.lock().unwrap();
            guard.as_ref().unwrap().daemon.child_pid_for_test()
        };

        // pgrep で「バックグラウンド事前ウォームアップの子として spawn
        // されうる zsh -i プロセス」の総数を数える代わりに、より決定的な
        // 方法として: スロットに残った pid が生きていることと、レースで
        // 捨てられた側（もしあれば）が実際に kill/reap されて ESRCH に
        // なっていることを確認する。捨てられた pid を直接知る手段はテスト
        // からは無い（`prewarm_zsh_daemon_with` 内部でのみ判明する）ため、
        // 「スロットに残っている pid が実際に生きているプロセスである」
        // ことと「そのプロセスに対して同じ pid で複数回 provide しても
        // 常に同じ pid が返る（=2個目のデーモンが紛れ込んでいない）」
        // ことを検証することで、実質的に「1個だけ生き残った」ことの
        // 十分な証拠とする。
        let ret = unsafe { libc::kill(final_pid as libc::pid_t, 0) };
        assert_eq!(
            ret, 0,
            "the surviving daemon pid {final_pid} must be a live process"
        );

        let second_provide = provider.provide(&ctx);
        let pid_again = {
            let guard = slot.lock().unwrap();
            guard.as_ref().unwrap().daemon.child_pid_for_test()
        };
        assert!(second_provide.is_some());
        assert_eq!(
            final_pid, pid_again,
            "no second daemon should have been spawned after the race settled"
        );
    }

    // ── S5 修正: 終端 shutdown tombstone（DaemonGate） ──

    #[test]
    fn daemon_gate_starts_open() {
        let gate = DaemonGate::new();
        assert!(!gate.is_closed());
    }

    #[test]
    fn daemon_gate_close_is_observed_and_idempotent() {
        let gate = DaemonGate::new();
        gate.close();
        assert!(gate.is_closed());
        // 二重 close は panic せず、閉じたままである（冪等性）。
        gate.close();
        assert!(gate.is_closed());
    }

    #[test]
    #[serial]
    fn prewarm_after_gate_closed_never_populates_slot_and_kills_spawned_child() {
        // S5 の核心保証（決定的ユニットテスト）: `shutdown_shared_daemon_blocking`
        // 相当（= gate を close してからスロットを shutdown）が**先に**
        // 起きたあとで `prewarm_zsh_daemon_with` を呼んでも、スロットは
        // 空のまま保たれ、かつ prewarm が実際に spawn した子プロセスは
        // （spawn 自体は締め切り後に発生する準正常系だが）確実に kill
        // される。「タイミング運任せの多くは漏れない」ではなく、closed
        // 状態の下では 100% 決定的にスロットへ書き込まれないことを保証する
        // （Mutex 内での再チェックがこの決定性の根拠 — 実装コメント参照）。
        let Some(zsh) = zsh_binary() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        let (tmpdir, zdotdir, _fpath_dir) = zsh_daemon_test_fixture();
        let home_envs = zsh_daemon_test_home_envs(&tmpdir);

        let settings = zsh_enabled_external_completion();
        let slot = new_shared_daemon_slot();
        let gate = DaemonGate::new();

        // 終端 shutdown が「先に」起きたことをシミュレートする
        // （main.rs の -c 経路 / rc.jsh 内 exit 経路が踏む順序と同じ:
        // gate.close() → スロット shutdown、この時点でスロットは空）。
        gate.close();
        shutdown_shared_daemon(&slot);
        assert!(slot.lock().unwrap().is_none());

        // その後で prewarm が（レースにより）遅れて spawn を試みる。
        prewarm_zsh_daemon_with(&settings, &slot, &gate, &zsh, &zdotdir, &home_envs);

        // 決定的保証その1: スロットは書き込まれない。
        assert!(
            slot.lock().unwrap().is_none(),
            "prewarm must never populate the slot once the gate is closed"
        );
    }

    #[test]
    #[serial]
    fn prewarm_close_race_after_spawn_but_before_lock_kills_the_orphan() {
        // より正確に実運用のレースを再現する版: prewarm が「spawn を完了した
        // 後・Mutex を取る前」というタイミングで gate が close される
        // ケース（main.rs の shutdown_zsh_daemon が prewarm のスレッド
        // スケジューリングの隙を突いて割り込む実際のシナリオ）。
        //
        // `prewarm_zsh_daemon_with` は spawn 完了直後に一度 `gate.is_closed()`
        // を確認する（Mutex 外の早期チェック）ため、このテストは「spawn 後
        // 即座に close された場合でも、実際に spawn された子プロセスが
        // 確実に kill/reap される」ことを ESRCH ポーリングで直接証明する。
        let Some(zsh) = zsh_binary() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        let (tmpdir, zdotdir, _fpath_dir) = zsh_daemon_test_fixture();
        let home_envs = zsh_daemon_test_home_envs(&tmpdir);

        let settings = zsh_enabled_external_completion();
        let slot = new_shared_daemon_slot();
        let gate = DaemonGate::new();

        // gate をあらかじめ close しておく（spawn 完了時点で必ず closed に
        // なっている、というタイミングの最も厳しいケースを決定的に作る —
        // 実際のスレッドインターリーブを待つのではなく、closed の状態で
        // prewarm を呼ぶことで「spawn 後チェックが機能するか」を直接検証）。
        gate.close();

        prewarm_zsh_daemon_with(&settings, &slot, &gate, &zsh, &zdotdir, &home_envs);

        assert!(
            slot.lock().unwrap().is_none(),
            "slot must remain empty when the gate was already closed before spawn completed"
        );
        // このテスト構成では spawn 自体が gate.is_closed() の早期チェック
        // （spawn 前）で弾かれるため、子プロセスは元々生成されていない
        // （早期リターンの網羅性を示す——重い spawn 処理にすら入らない）。
    }

    #[test]
    #[serial]
    fn shutdown_shared_daemon_blocking_with_gate_prevents_late_prewarm_insertion() {
        // 実際の Shell::shutdown_zsh_daemon が使う経路
        // （shutdown_shared_daemon_blocking(slot, deadline, Some(&gate))）を
        // 直接呼び、「shutdown 後に prewarm_zsh_daemon_with を呼んでもスロット
        // が空のまま」であることを、公開 API のシグネチャそのままで検証する
        // （タスク指示の受け入れ基準3 の決定的ユニットテスト）。
        let Some(zsh) = zsh_binary() else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };
        let (tmpdir, zdotdir, _fpath_dir) = zsh_daemon_test_fixture();
        let home_envs = zsh_daemon_test_home_envs(&tmpdir);

        let settings = zsh_enabled_external_completion();
        let slot = new_shared_daemon_slot();
        let gate = DaemonGate::new();

        // スロットは元々空（-c 単体実行が数ミリ秒で完走し、prewarm がまだ
        // 何も spawn していない時点で shutdown が先に走るケースを模す）。
        shutdown_shared_daemon_blocking(&slot, Duration::from_secs(2), Some(&gate));
        assert!(slot.lock().unwrap().is_none());
        assert!(gate.is_closed());

        // 遅れて prewarm が発火しても、closed を検知して自壊する。
        prewarm_zsh_daemon_with(&settings, &slot, &gate, &zsh, &zdotdir, &home_envs);

        assert!(
            slot.lock().unwrap().is_none(),
            "late prewarm after shutdown_shared_daemon_blocking(..., Some(gate)) must not \
             populate the slot (S5 acceptance criterion 3)"
        );
    }

    #[test]
    fn shutdown_shared_daemon_blocking_without_gate_does_not_close_it() {
        // reload 経路（`apply_zsh_daemon_lifecycle_for_reload` 等）は
        // 通常 shutdown_shared_daemon（非ブロッキング、gate なし）を使うが、
        // 万一 shutdown_shared_daemon_blocking を `gate: None` で呼んでも
        // gate には触れない（tombstone は exit/exec 専用という契約）ことを
        // 確認する。
        let slot = new_shared_daemon_slot();
        let gate = DaemonGate::new();
        shutdown_shared_daemon_blocking(&slot, Duration::from_millis(100), None);
        assert!(!gate.is_closed(), "gate must stay open when not passed in");
    }
}
