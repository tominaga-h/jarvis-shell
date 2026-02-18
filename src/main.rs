mod ai;
mod cli;
mod config;
mod engine;
mod logging;
mod shell;
mod storage;

use std::path::PathBuf;

use clap::Parser;
use tracing::info;

/// Next Generation AI Integrated Shell
#[derive(Parser)]
#[command(name = "jarvish")]
struct Args {
    /// デバッグモード: ログを ./var/logs に出力する
    #[arg(long)]
    debug: bool,
}

#[tokio::main]
async fn main() {
    // .env ファイルから環境変数を読み込む
    dotenvy::dotenv().ok();

    let args = Args::parse();

    let log_dir_override = if args.debug {
        Some(PathBuf::from("./var/logs"))
    } else {
        None
    };

    // ログシステムの初期化（_guard は main 終了まで保持する必要がある）
    let _guard = logging::init_logging(log_dir_override);

    info!("\n\n==== J.A.R.V.I.S.H. STARTED ====\n");

    let mut shell = shell::Shell::new();
    let exit_code = shell.run().await;

    info!("\n\n==== J.A.R.V.I.S.H. SHUTTING DOWN ====\n\n");

    std::process::exit(exit_code);
}
