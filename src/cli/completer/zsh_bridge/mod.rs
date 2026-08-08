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
//! # ユーザー拡張点（zsh-bridge ブリッジディレクトリ）
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

mod capture_script;
mod core;
mod lifecycle;
mod output_parse;
#[cfg(test)]
mod tests;

pub(super) use capture_script::CAPTURE_SCRIPT;
pub(super) use core::ZshBridgeProvider;
pub(super) use lifecycle::{
    bridge_dir, bridge_zshrc_path, compute_warm_timeout, ensure_bridge_zshrc, DaemonSlot,
    MIN_TIMEOUT_MS,
};
pub use lifecycle::{
    new_shared_daemon_slot, prewarm_zsh_daemon, shutdown_shared_daemon,
    shutdown_shared_daemon_blocking, DaemonGate, SharedDaemonSlot,
};
pub(super) use output_parse::{escape_spans, parse_capture_output};
