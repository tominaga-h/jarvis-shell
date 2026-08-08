use std::io;
use std::path::{Path, PathBuf};

use super::options::RcOptions;

/// 実行すべき rc スクリプトの解決結果。
#[derive(Debug)]
pub(in crate::shell) enum ResolvedRc {
    /// デフォルトパス。存在しなければテンプレートを自動生成してよい。
    Default(PathBuf),
    /// `--rcfile` で明示指定されたパス。自動生成は行わない。
    Explicit(PathBuf),
    /// `--no-rc` 指定、または明示パスが見つからず読み込むものがない。
    None,
}

impl RcOptions {
    /// CLI オプションから実行すべき rc スクリプトを解決する。
    ///
    /// - `no_rc` が真なら常に [`ResolvedRc::None`]（`rcfile` が同時指定されて
    ///   いてもここには来ない — clap の `conflicts_with` が先に弾く）。
    /// - `rcfile` が `Some` ならそれを [`ResolvedRc::Explicit`] として返す
    ///   （存在確認は呼び出し側が行う）。
    /// - どちらも未指定ならデフォルトパスを [`ResolvedRc::Default`] として返す。
    pub(in crate::shell) fn resolve(&self) -> ResolvedRc {
        if self.no_rc {
            return ResolvedRc::None;
        }
        if let Some(ref path) = self.rcfile {
            return ResolvedRc::Explicit(path.clone());
        }
        ResolvedRc::Default(rc_path())
    }
}

/// [`Shell::run_configured_rc`] が実際に行うべきことを表す純粋な計画
/// （`Shell` を一切参照しない、`ResolvedRc` から直接導出できる決定）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::shell) enum RcBootstrapPlan {
    /// 何もしない — テンプレート自動生成も含め、rc スクリプトには一切
    /// 触れない（`--no-rc` 指定時）。
    Skip,
    /// このパスに対して [`ensure_default_rc`] を呼んでから実行する
    /// （デフォルトパス、初回起動時のテンプレート自動生成込み）。
    BootstrapAndRun {
        path: PathBuf,
        display_name: &'static str,
    },
    /// このパスをそのまま実行する。自動生成は行わない
    /// （`--rcfile` で明示指定されたパスが存在する場合）。
    RunExplicit { path: PathBuf, display_name: String },
    /// 明示 `--rcfile` パスが存在しない — 警告のみで実行はしない。
    ExplicitMissing { path: PathBuf },
}

/// [`ResolvedRc`] と実際のファイル存在確認（`--rcfile` のみ）から
/// [`RcBootstrapPlan`] を導出する。
pub(in crate::shell) fn plan_rc_bootstrap(resolved: ResolvedRc) -> RcBootstrapPlan {
    match resolved {
        ResolvedRc::None => RcBootstrapPlan::Skip,
        ResolvedRc::Default(path) => RcBootstrapPlan::BootstrapAndRun {
            path,
            display_name: "rc.jsh",
        },
        ResolvedRc::Explicit(path) => {
            if !path.exists() {
                return RcBootstrapPlan::ExplicitMissing { path };
            }
            let display_name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            RcBootstrapPlan::RunExplicit { path, display_name }
        }
    }
}

/// rc.jsh のデフォルトパスを解決する。
pub(in crate::shell) fn rc_path() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".config/jarvish/rc.jsh")
}

/// `path` がシンボリックリンクかどうかを判定する。
pub(in crate::shell::rc) fn is_symlink(path: &Path) -> io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => Ok(meta.file_type().is_symlink()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// `source <path>` の拡張子が `.toml`（大文字小文字を区別しない）かどうかを
/// 判定する。
pub(in crate::shell) fn is_toml_source_path(path_str: &str) -> bool {
    Path::new(path_str)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"))
}
