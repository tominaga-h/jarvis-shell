mod ai;
mod cli;
mod config;
mod engine;
mod logging;
mod shell;
mod storage;

use std::path::PathBuf;

use clap::{CommandFactory, FromArgMatches, Parser};
use rand::Rng;
use tracing::info;

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

    let mut shell = shell::Shell::new(logging_ok, session_id);
    let exit_code = if let Some(ref command) = args.command {
        shell.run_command(command).await
    } else {
        shell.run().await
    };

    info!(
        "\n\n==== [{}] J.A.R.V.I.S.H. SHUTTING DOWN ====\n\n",
        session_key
    );

    std::process::exit(exit_code);
}
