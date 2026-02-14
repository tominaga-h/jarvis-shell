//! ログ初期化モジュール
//!
//! `tracing` + `tracing-subscriber` を使用して、デバッグログを外部ファイルに出力する。
//! ログファイルは `var/logs/` ディレクトリに日次ローテーション（JST基準）で保存される。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{FixedOffset, Utc};
use tracing_subscriber::fmt::time::FormatTime;
use tracing_subscriber::{fmt, EnvFilter};

/// JST (UTC+09:00) のオフセット（秒）
const JST_OFFSET_SECS: i32 = 9 * 3600;

/// JST タイムゾーンを返すヘルパー
fn jst() -> FixedOffset {
    FixedOffset::east_opt(JST_OFFSET_SECS).expect("invalid JST offset")
}

// ---------------------------------------------------------------------------
// JST タイマー
// ---------------------------------------------------------------------------

/// ログ行のタイムスタンプを JST で出力するタイマー
struct JstTimer;

impl FormatTime for JstTimer {
    fn format_time(&self, w: &mut tracing_subscriber::fmt::format::Writer<'_>) -> std::fmt::Result {
        let now = Utc::now().with_timezone(&jst());
        write!(w, "{}", now.format("%Y-%m-%dT%H:%M:%S%.3f+09:00"))
    }
}

// ---------------------------------------------------------------------------
// JST ベースのローリングファイルアペンダー
// ---------------------------------------------------------------------------

/// JST 基準で日次ローテーションするファイルアペンダー。
///
/// 書き込み時に現在の JST 日付を確認し、日付が変わっていれば新しいファイルを開く。
/// `tracing_appender::non_blocking` と組み合わせて使用する。
struct JstRollingAppender {
    dir: PathBuf,
    prefix: String,
    current_date: chrono::NaiveDate,
    file: File,
}

impl JstRollingAppender {
    /// 新しい JST ローリングアペンダーを作成する。
    fn new(dir: PathBuf, prefix: &str) -> std::io::Result<Self> {
        let today = Utc::now().with_timezone(&jst()).date_naive();
        let file = Self::open_log_file(&dir, prefix, today)?;
        Ok(Self {
            dir,
            prefix: prefix.to_string(),
            current_date: today,
            file,
        })
    }

    /// 指定した日付のログファイルを開く（なければ作成）。
    fn open_log_file(dir: &Path, prefix: &str, date: chrono::NaiveDate) -> std::io::Result<File> {
        let filename = format!("{}.{}", prefix, date.format("%Y-%m-%d"));
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(filename))
    }
}

impl Write for JstRollingAppender {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let today = Utc::now().with_timezone(&jst()).date_naive();
        if today != self.current_date {
            self.file = Self::open_log_file(&self.dir, &self.prefix, today)?;
            self.current_date = today;
        }
        self.file.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

// ---------------------------------------------------------------------------
// ログ初期化
// ---------------------------------------------------------------------------

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
/// - ログファイルは `var/logs/jarvish.log.YYYY-MM-DD` に日次ローテーション（JST基準）で出力
/// - タイムスタンプは JST (UTC+09:00) で記録
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

    // JST ベースの日次ローテーションアペンダーを作成
    let file_appender =
        JstRollingAppender::new(log_dir.clone(), "jarvish.log").unwrap_or_else(|e| {
            panic!(
                "jarvish: failed to create log file in {}: {e}",
                log_dir.display()
            )
        });

    // 非ブロッキング書き込み用のワーカーを作成
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    // JARVISH_LOG 環境変数でログレベルを制御（デフォルト: debug）
    let env_filter =
        EnvFilter::try_from_env("JARVISH_LOG").unwrap_or_else(|_| EnvFilter::new("debug"));

    // サブスクライバーを構成して設定
    fmt()
        .with_env_filter(env_filter)
        .with_writer(non_blocking)
        .with_timer(JstTimer) // タイムスタンプを JST で出力
        .with_ansi(false) // ファイル出力には ANSI カラーコードを含めない
        .with_target(true) // ターゲット（モジュールパス）を表示
        .with_thread_ids(false)
        .with_line_number(true) // 行番号を表示（デバッグ用）
        .with_file(true) // ファイル名を表示（デバッグ用）
        .init();

    guard
}
