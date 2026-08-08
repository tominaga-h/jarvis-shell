//! 温存 zsh 補完デーモン — `zsh -i` を常駐させ Tab ごとの起動コストを消す
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
//! # `JarvishCompleter` への配線
//! [`super::zsh_bridge::ZshBridgeProvider`] が `Mutex<Option<ZshDaemon>>` を
//! 保持し、`[completion] external_zsh_daemon` が有効な間は**シェル起動直後に
//! バックグラウンドスレッドから事前ウォームアップ**される
//! （[`super::zsh_bridge::prewarm_zsh_daemon`]）ため、通常は最初の
//! 補完リクエストの時点で既にこのスロットが埋まっている。プリウォームが
//! 間に合わなかった場合（または zsh 未検出等でスキップされた場合）は、
//! 最初にデーモンを必要とするリクエストで遅延 spawn する経路がフォール
//! バックとして機能する。以後は同じインスタンスを使い回す。連続タイム
//! アウトで `is_alive()` が `false` になった場合や、ブリッジ `.zshrc` の
//! mtime が spawn 時から変わっていた場合は shutdown して次回リクエストで
//! 再 spawn する（`zsh_bridge.rs` のモジュールドキュメント参照）。

use std::env;
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

/// [`ZshDaemon::request`] のレスポンスバッファ上限。
///
/// ハング/バグった補完関数が延々と出力し続けるケース（無限ループの
/// `compadd`、巨大ファイルの誤 cat 等）に対して、タイムアウトまで
/// 無制限に `Vec<u8>` を伸ばし続けるとメモリを圧迫する。この上限を
/// 超えた時点でプロトコル desync 相当として扱い、即座に `mark_dead_and_kill`
/// して `None` を返す（タイムアウトを待たない）。4 MiB は通常の補完候補
/// 数千件分でも十分な余裕がある値。
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// jarvish が生成する init スクリプト本体（`assets/zsh/daemon_init.zsh`）。
const DAEMON_INIT_SCRIPT: &str = include_str!("../../../../assets/zsh/daemon_init.zsh");

/// 初期化完了を示すレディマーカー行（`daemon_init.zsh` の末尾 `echo` と対応）。
const READY_MARKER: &str = "jarvish_daemon_ok";

/// センチネル行の末尾マーカー（PTY の `\r\n` 変換後、NUL の直後に `\r` が
/// 来る — `capture.zsh` の `[[ $line == *$'\0\r' ]]` と同じ検出条件）。
const SENTINEL_BYTE: u8 = 0;
mod core;
mod lifecycle;
mod pty_io;
mod request_framing;
#[cfg(test)]
mod tests;

#[allow(unused_imports)] // MAX_CONSECUTIVE_TIMEOUTS re-exported for zsh_daemon tests
pub(crate) use core::{ZshDaemon, MAX_CONSECUTIVE_TIMEOUTS};
pub(super) use lifecycle::{cleanup_stale_compdumps, cleanup_stale_init_scripts};
use pty_io::{contains_line, create_daemon_pty, read_available, write_init_script};
use request_framing::{FramedRead, PartialRead};
