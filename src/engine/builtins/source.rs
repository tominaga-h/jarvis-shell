use clap::Parser;

use crate::engine::CommandResult;

/// source: 設定ファイル(TOML)、または rc スクリプトを読み込む。
///
/// この struct はコマンドラインの引数パースのみを担当する。実際の
/// ディスパッチ（`.toml` → config 再読み込み / それ以外 → rc スクリプト
/// 実行）は `Shell::dispatch_source`（`src/shell/rc.rs`, Phase 4.3）が
/// 行う — `try_shell_builtins` の `"source"` 分岐がこの `parse` で得た
/// パス文字列を渡す。
#[derive(Parser)]
#[command(
    name = "source",
    about = "Load a configuration file (.toml) or run a script (any other/no extension)"
)]
struct SourceArgs {
    /// Path to the file to load. A `.toml` path (case-insensitive) reloads
    /// the configuration; any other extension (or none) is executed as an
    /// rc-style script — same semantics as `rc.jsh` (classifier bypass,
    /// line-numbered errors, continue-on-error, max nesting depth 8).
    #[arg(required = true)]
    path: String,
}

/// source の引数をパースし、ファイルパスを返す。
///
/// パース成功 → `Ok(path)` を返す。
/// `--help` やエラー → `Err(CommandResult)` を返す（呼び出し元がそのまま返せる）。
pub(crate) fn parse(args: &[&str]) -> Result<String, CommandResult> {
    let parsed = super::parse_args::<SourceArgs>("source", args)?;
    Ok(parsed.path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_no_args_returns_error() {
        let result = parse(&[]);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_ne!(err.exit_code, 0);
    }

    #[test]
    fn source_with_path_returns_ok() {
        let result = parse(&["~/.config/jarvish/config.toml"]);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "~/.config/jarvish/config.toml");
    }

    #[test]
    fn source_help_returns_success() {
        let result = parse(&["--help"]);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.exit_code, 0);
        assert!(err.stdout.contains("source"));
    }
}
