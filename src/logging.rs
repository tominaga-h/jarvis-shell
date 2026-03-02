//! ログ初期化モジュール
//!
//! `tracing` + `tracing-subscriber` を使用して、デバッグログを外部ファイルに出力する。
//! ログファイルは XDG_DATA_HOME 準拠のシステムディレクトリ
//! (`~/.local/share/jarvish/logs/`) に日次ローテーション（JST基準）で保存される。

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
        let filename = format!("{}_{}.log", prefix, date.format("%Y-%m-%d"));
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
///
/// `BlackBox::data_dir()` で決定されたデータディレクトリ配下の `logs/` を返す。
fn log_dir() -> PathBuf {
    crate::storage::BlackBox::data_dir().join("logs")
}

/// ログシステムを初期化する。
///
/// - ログレベルは `JARVISH_LOG` 環境変数で制御（デフォルト: `debug`）
/// - ログファイルは日次ローテーション（JST基準）で出力
/// - `log_dir_override` が `Some` の場合はそのパスに、`None` の場合は
///   `XDG_DATA_HOME/jarvish/logs/` に出力
/// - タイムスタンプは JST (UTC+09:00) で記録
/// - フォーマット: タイムスタンプ + レベル + ターゲット + メッセージ
///
/// # Returns
/// `(WorkerGuard, bool)` を返す。
/// - `WorkerGuard` は `main()` で保持し続ける必要がある（ドロップするとログ出力が停止する）。
/// - `bool` はログファイルへの書き込みが正常に開始できたかどうか。
///   `false` の場合、ログは sink（破棄）にフォールバックしている。
pub fn init_logging(
    log_dir_override: Option<PathBuf>,
) -> (tracing_appender::non_blocking::WorkerGuard, bool) {
    let log_dir = log_dir_override.unwrap_or_else(log_dir);

    // ログディレクトリが存在しない場合は作成
    if let Err(e) = std::fs::create_dir_all(&log_dir) {
        eprintln!(
            "jarvish: warning: failed to create log directory {}: {e}",
            log_dir.display()
        );
    }

    // JST ベースの日次ローテーションアペンダーを作成。
    // ログファイル作成に失敗した場合は stderr に警告を出力し、sink にフォールバック。
    let (writer, operational): (Box<dyn Write + Send>, bool) =
        match JstRollingAppender::new(log_dir.clone(), "jarvish") {
            Ok(appender) => (Box::new(appender), true),
            Err(e) => {
                eprintln!(
                    "jarvish: warning: failed to create log file in {}: {e}",
                    log_dir.display()
                );
                (Box::new(std::io::sink()), false)
            }
        };

    // 非ブロッキング書き込み用のワーカーを作成
    let (non_blocking, guard) = tracing_appender::non_blocking(writer);

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

    (guard, operational)
}

// ---------------------------------------------------------------------------
// CPU 使用率モニター
// ---------------------------------------------------------------------------

/// 自プロセスのCPU使用率を定期監視するバックグラウンドスレッドを起動する。
///
/// 2秒間隔でCPU使用率を計測し、閾値（10.0%）を超えた場合に warn レベルで
/// ログ出力する。閾値未満の場合は debug レベルで記録する。
/// スレッドはデーモンスレッドとして動作し、プロセス終了時に自動で停止する。
pub fn start_cpu_monitor() {
    use std::time::Duration;

    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate};

    const INTERVAL: Duration = Duration::from_secs(2);
    const CPU_THRESHOLD: f32 = 10.0;

    std::thread::spawn(move || {
        let pid = match sysinfo::get_current_pid() {
            Ok(pid) => pid,
            Err(e) => {
                tracing::warn!("[CPU Monitor] Failed to get current PID: {e}");
                return;
            }
        };

        let refresh_kind = ProcessRefreshKind::nothing().with_cpu();
        let pids = [pid];
        let target = ProcessesToUpdate::Some(&pids);
        let mut sys = sysinfo::System::new();

        // ベースライン測定（sysinfo はCPU使用率算出に前回値が必要）
        sys.refresh_processes_specifics(target, false, refresh_kind);
        std::thread::sleep(INTERVAL);

        loop {
            sys.refresh_processes_specifics(target, false, refresh_kind);

            if let Some(process) = sys.process(pid) {
                let usage = process.cpu_usage();
                if usage >= CPU_THRESHOLD {
                    tracing::warn!("[CPU Monitor] High CPU usage detected: {usage:.1}%");
                } else {
                    tracing::debug!("[CPU Monitor] CPU usage: {usage:.1}%");
                }
            }

            std::thread::sleep(INTERVAL);
        }
    });
}
