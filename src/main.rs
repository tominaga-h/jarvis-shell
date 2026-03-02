mod ai;
mod cli;
mod config;
mod engine;
mod logging;
mod shell;
mod storage;

use std::path::PathBuf;

use clap::{CommandFactory, FromArgMatches, Parser};
use tracing::info;

/// Next Generation AI Integrated Shell
#[derive(Parser)]
#[command(name = "jarvish", version)]
struct Args {
    /// デバッグモード: ログを ./var/logs に出力する
    #[arg(long)]
    debug: bool,
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

    // ログシステムの初期化（_guard は main 終了まで保持する必要がある）
    let (_guard, logging_ok) = logging::init_logging(log_dir_override);
    logging::start_cpu_monitor();

    info!("\n\n==== J.A.R.V.I.S.H. STARTED ====\n");

    let mut shell = shell::Shell::new(logging_ok);
    let exit_code = shell.run().await;

    info!("\n\n==== J.A.R.V.I.S.H. SHUTTING DOWN ====\n\n");

    std::process::exit(exit_code);
}
