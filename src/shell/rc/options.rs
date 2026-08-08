use std::path::PathBuf;

/// CLI から渡される rc スクリプトの読み込みオプション。
///
/// `--rcfile <PATH>` と `--no-rc` は clap 側で `conflicts_with` により
/// 同時指定を拒否されるため、ここでは両方 unset（デフォルト）/
/// `rcfile` のみ / `no_rc` のみ、の3状態のみを想定する。
#[derive(Debug, Clone, Default)]
pub struct RcOptions {
    /// 明示的に指定された rc スクリプトのパス。デフォルトパス
    /// （[`rc_path`]）の代わりに使用し、存在しなくても自動生成しない。
    pub rcfile: Option<PathBuf>,
    /// rc スクリプトの読み込みを完全に無効化する（テンプレート生成も含む）。
    pub no_rc: bool,
}

/// rc スクリプト実行後の制御結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::shell) enum RcOutcome {
    /// 全行を実行し終え、REPL ループを継続してよい。
    /// `had_failure`: 1行以上が非ゼロ終了コードで終わっていれば `true`
    /// （`source` ビルトインが exit code 0/1 を決めるために使う）。
    Continue { had_failure: bool },
    /// `exit` / goodbye 相当の行によりシェル終了が要求された
    ExitRequested,
}
