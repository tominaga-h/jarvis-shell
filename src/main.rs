mod ai;
mod cli;
mod engine;
mod logging;
mod shell;
mod storage;

use tracing::info;

#[tokio::main]
async fn main() {
    // .env ファイルから環境変数を読み込む
    dotenvy::dotenv().ok();

    // ログシステムの初期化（_guard は main 終了まで保持する必要がある）
    let _guard = logging::init_logging();

    info!("\n\n==== J.A.R.V.I.S.H. STARTED ====\n");

    let mut shell = shell::Shell::new();
    let exit_code = shell.run().await;

    info!("\n\n==== J.A.R.V.I.S.H. SHUTTING DOWN ====\n\n");

    std::process::exit(exit_code);
}
