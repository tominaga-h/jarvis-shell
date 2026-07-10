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
use std::sync::{Arc, RwLock};

use super::carapace::{gate, ExternalCompletionSettings, ExternalKind};
use super::context::CompletionContext;
use super::external::run_external_capped;
use super::provider::{Candidate, CompletionProvider};

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

/// zsh 補完ブリッジ Provider。
///
/// `ExternalCompletionSettings` を [`super::carapace::CarapaceProvider`] と
/// 同じ `Arc<RwLock<_>>` で共有する（`git_branch_commands` と同じ配管
/// パターン）。有効化判定は `settings.binary_path(ExternalKind::Zsh)` が
/// `Some` を返すかどうかに一本化されている — `[completion] external` の
/// 値（`"auto"` / `"zsh"` / 配列での明示指定など）に応じて `resolve()` が
/// このプロバイダを優先順リストに含めるかどうか・zsh バイナリを検出するか
/// どうかを決める（Task 2b.4）。timeout も同じ共有設定から取得する。
pub(super) struct ZshBridgeProvider {
    settings: Arc<RwLock<ExternalCompletionSettings>>,
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

impl ZshBridgeProvider {
    pub(super) fn new(settings: Arc<RwLock<ExternalCompletionSettings>>) -> Self {
        Self {
            settings,
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
        // carapace のみが指定されている等）場合は `gate` が `None` を返し、
        // ここで早期 return する。`MIN_TIMEOUT_MS` フロアは zsh ブリッジ
        // 専用（compinit の重さ対策 — 定数のドキュメント参照）なので
        // `Some(...)` で渡す。
        let (_gated_binary, timeout) = gate(
            &self.settings,
            ExternalKind::Zsh,
            Some(std::time::Duration::from_millis(MIN_TIMEOUT_MS)),
        )?;

        let zsh = self.resolve_zsh()?;

        let spans = ctx.spans();
        if spans.len() < 2 {
            // spans[0] (コマンド名) しかない = まだサブコマンド/引数の
            // 補完対象がない（carapace.rs と同じガード）。
            return None;
        }

        let escaped_spans = escape_spans(&spans)?;

        let mut args = vec![
            "--no-rcs".to_string(),
            "-c".to_string(),
            CAPTURE_SCRIPT.to_string(),
            "--".to_string(),
        ];
        args.extend(escaped_spans);

        // ブリッジディレクトリ + テンプレート .zshrc の存在を保証してから
        // ZDOTDIR で渡す。存在保証を spawn の直前に必ず行うことで、
        // ZDOTDIR が指すディレクトリが空だったために zsh が $HOME に
        // フォールバックし、ユーザーの実 ~/.zshrc を読んでしまう事故を防ぐ
        // （モジュール冒頭ドキュメント参照）。
        let bridge_dir = self.resolve_bridge_dir();
        if ensure_bridge_zshrc(&bridge_dir).is_err() {
            tracing::debug!("zsh bridge: failed to prepare bridge dir at {bridge_dir:?}, skipping");
            return None;
        }
        #[cfg_attr(not(test), allow(unused_mut))]
        let mut envs = vec![(
            "ZDOTDIR".to_string(),
            bridge_dir.to_string_lossy().into_owned(),
        )];
        #[cfg(test)]
        envs.extend(self.extra_envs.iter().cloned());

        let stdout = run_external_capped(&zsh, &args, &envs, timeout)?;

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
fn parse_capture_output(stdout: &str) -> Vec<Candidate> {
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
    use std::time::Duration;

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
        let provider = ZshBridgeProvider::new(settings);
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
}
