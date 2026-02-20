use clap::Parser;

use crate::engine::CommandResult;

/// source: 設定ファイル(TOML)を読み込む。
#[derive(Parser)]
#[command(name = "source", about = "Load a configuration file (TOML)")]
struct SourceArgs {
    /// Path to the TOML file to load
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
