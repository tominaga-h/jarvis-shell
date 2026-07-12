use std::path::PathBuf;

use clap::{CommandFactory, FromArgMatches, Parser};
use rand::Rng;
use tracing::{info, warn};

use jarvish::shell::RcOptions;
use jarvish::{engine, logging, shell};

/// Next Generation AI Integrated Shell
#[derive(Parser)]
#[command(name = "jarvish", version)]
struct Args {
    /// デバッグモード: ログを ./var/logs に出力する
    #[arg(long)]
    debug: bool,

    /// 文字列をコマンドとして実行して終了する
    #[arg(short = 'c', allow_hyphen_values = true)]
    command: Option<String>,

    /// rc.jsh の代わりに指定したパスの起動スクリプトを読み込む（自動生成はしない）
    #[arg(long, value_name = "PATH", conflicts_with = "no_rc")]
    rcfile: Option<PathBuf>,

    /// 起動スクリプト（rc.jsh）の読み込みを完全に無効化する
    #[arg(long, conflicts_with = "rcfile")]
    no_rc: bool,
}

#[tokio::main]
async fn main() {
    // .env ファイルから環境変数を読み込む
    dotenvy::dotenv().ok();

    let args = Args::from_arg_matches(
        &Args::command()
            .disable_version_flag(true)
            .arg(
                clap::Arg::new("version")
                    .short('v')
                    .long("version")
                    .action(clap::ArgAction::Version),
            )
            .get_matches(),
    )
    .expect("failed to parse args");

    let log_dir_override = if args.debug {
        Some(PathBuf::from("./var/logs"))
    } else {
        None
    };

    // プロセス固有のセッション ID を生成
    // session_id: 履歴のセッション分離に使用する一意な整数
    // session_key: ログのプレフィックスに使用する 6 文字 hex
    let session_id: i64 = rand::rng().random_range(0..i64::MAX);
    let session_key = format!("{:06x}", (session_id as u64) & 0xFFFFFF);

    // ログシステムの初期化（_guard は main 終了まで保持する必要がある）
    let (_guard, logging_ok) = logging::init_logging(log_dir_override, &session_key);
    logging::start_cpu_monitor();

    info!(
        "\n\n==== J.A.R.V.I.S.H. STARTED at [{}] ====\n",
        session_key
    );

    let rc_options = RcOptions {
        rcfile: args.rcfile,
        no_rc: args.no_rc,
    };
    let mut shell = shell::Shell::new(logging_ok, session_id, rc_options);
    let (exit_code, action) = if let Some(ref command) = args.command {
        (shell.run_command(command).await, engine::LoopAction::Exit)
    } else {
        shell.run().await
    };

    info!(
        "\n\n==== [{}] J.A.R.V.I.S.H. SHUTTING DOWN ====\n\n",
        session_key
    );

    // Restart アクション: exec() でプロセスを置換
    if action == engine::LoopAction::Restart {
        // _guard を明示的にドロップしてログをフラッシュ
        drop(_guard);
        // exec_restart() 内部で exec() 直前に温存 zsh デーモンを shutdown
        // する（A1, #89）。exec() が失敗して以下に到達した場合も、
        // デーモンは既に shutdown 済みなのでここでの追加対応は不要。
        let err = shell.exec_restart();
        // exec() が失敗した場合のみここに到達
        warn!(error = %err, "exec_restart failed");
        eprintln!("jarvish: restart failed: {err}");
        std::process::exit(1);
    }

    // std::process::exit はデストラクタを一切実行しないため、温存 zsh
    // デーモンが稼働中ならここで明示的に shutdown する（A2, #89 レビュー
    // 指摘 — README の「daemon is killed automatically when Jarvish exits」
    // を実際に真にする）。デーモンが元々稼働していなければ no-op。
    shell.shutdown_zsh_daemon();

    std::process::exit(exit_code);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `--rcfile` と `--no-rc` は clap の `conflicts_with` により
    /// 同時指定を拒否される（Phase 4.2 の DESIGN CONTRACT どおり）。
    #[test]
    fn rcfile_and_no_rc_conflict_is_rejected() {
        let result = Args::command().try_get_matches_from([
            "jarvish",
            "--rcfile",
            "/tmp/some_rc.jsh",
            "--no-rc",
        ]);
        assert!(
            result.is_err(),
            "--rcfile and --no-rc must be mutually exclusive"
        );
    }

    /// `--rcfile <PATH>` 単体は問題なくパースでき、値が保持されること。
    #[test]
    fn rcfile_alone_parses_value() {
        let matches = Args::command()
            .try_get_matches_from(["jarvish", "--rcfile", "/tmp/some_rc.jsh"])
            .expect("--rcfile alone must parse");
        let args = Args::from_arg_matches(&matches).expect("must convert to Args");
        assert_eq!(args.rcfile, Some(PathBuf::from("/tmp/some_rc.jsh")));
        assert!(!args.no_rc);
    }

    /// `--no-rc` 単体は問題なくパースでき、フラグが立つこと。
    #[test]
    fn no_rc_alone_parses_flag() {
        let matches = Args::command()
            .try_get_matches_from(["jarvish", "--no-rc"])
            .expect("--no-rc alone must parse");
        let args = Args::from_arg_matches(&matches).expect("must convert to Args");
        assert!(args.no_rc);
        assert_eq!(args.rcfile, None);
    }

    /// どちらも未指定の場合はデフォルト（両方 unset）になること。
    #[test]
    fn neither_flag_defaults_to_unset() {
        let matches = Args::command()
            .try_get_matches_from(["jarvish"])
            .expect("no flags must parse");
        let args = Args::from_arg_matches(&matches).expect("must convert to Args");
        assert!(!args.no_rc);
        assert_eq!(args.rcfile, None);
    }

    /// `--rcfile` は `-c` と併用できる（本 Phase の主要な組み合わせ）。
    #[test]
    fn rcfile_combines_with_dash_c() {
        let matches = Args::command()
            .try_get_matches_from(["jarvish", "--rcfile", "/tmp/some_rc.jsh", "-c", "echo hi"])
            .expect("--rcfile + -c must parse together");
        let args = Args::from_arg_matches(&matches).expect("must convert to Args");
        assert_eq!(args.rcfile, Some(PathBuf::from("/tmp/some_rc.jsh")));
        assert_eq!(args.command, Some("echo hi".to_string()));
    }
}
