//! ログ初期化モジュール
//!
//! `tracing` + `tracing-subscriber` を使用して、デバッグログを外部ファイルに出力する。
//! ログファイルは `var/logs/` ディレクトリに日次ローテーションで保存される。

use std::path::PathBuf;

use tracing_appender::rolling;
use tracing_subscriber::{fmt, EnvFilter};

/// ログの出力先ディレクトリを決定する。
/// `CARGO_MANIFEST_DIR`（開発時）またはカレントディレクトリからの相対パス `var/logs/` を使用する。
fn log_dir() -> PathBuf {
    // 開発時: CARGO_MANIFEST_DIR が設定されていればそれを使用
    if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") {
        return PathBuf::from(manifest_dir).join("var").join("logs");
    }

    // 実行時: カレントディレクトリからの相対パス
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("var")
        .join("logs")
}

/// ログシステムを初期化する。
///
/// - ログレベルは `JARVISH_LOG` 環境変数で制御（デフォルト: `debug`）
/// - ログファイルは `var/logs/jarvish.YYYY-MM-DD.log` に日次ローテーションで出力
/// - フォーマット: タイムスタンプ + レベル + ターゲット + メッセージ
///
/// # Returns
/// `tracing_appender::non_blocking::WorkerGuard` を返す。
/// このガードは `main()` で保持し続ける必要がある（ドロップするとログ出力が停止する）。
pub fn init_logging() -> tracing_appender::non_blocking::WorkerGuard {
    let log_dir = log_dir();

    // ログディレクトリが存在しない場合は作成
    if let Err(e) = std::fs::create_dir_all(&log_dir) {
        eprintln!(
            "jarvish: warning: failed to create log directory {}: {e}",
            log_dir.display()
        );
    }

    // 日次ローテーションのファイルアペンダーを作成
    let file_appender = rolling::daily(&log_dir, "jarvish.log");

    // 非ブロッキング書き込み用のワーカーを作成
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    // JARVISH_LOG 環境変数でログレベルを制御（デフォルト: debug）
    let env_filter = EnvFilter::try_from_env("JARVISH_LOG")
        .unwrap_or_else(|_| EnvFilter::new("debug"));

    // サブスクライバーを構成して設定
    fmt()
        .with_env_filter(env_filter)
        .with_writer(non_blocking)
        .with_ansi(false) // ファイル出力には ANSI カラーコードを含めない
        .with_target(true) // ターゲット（モジュールパス）を表示
        .with_thread_ids(false)
        .with_line_number(true) // 行番号を表示（デバッグ用）
        .with_file(true) // ファイル名を表示（デバッグ用）
        .init();

    guard
}
