use clap::{Parser, Subcommand};
use rusqlite::Connection;

use crate::engine::CommandResult;
use crate::storage::BlackBox;

/// history: コマンド履歴を表示・管理する。
#[derive(Parser)]
#[command(name = "history", about = "コマンド履歴を表示・管理する")]
struct HistoryArgs {
    #[command(subcommand)]
    command: Option<HistoryCommand>,

    /// 表示する件数 (デフォルト: 50)
    #[arg(short = 'n', long, default_value = "50")]
    count: usize,

    /// ディレクトリ付きで表示する
    #[arg(short = 'd', long = "dirs")]
    dirs: bool,
}

#[derive(Subcommand)]
enum HistoryCommand {
    /// 全履歴をクリアする
    Clear,
}

/// history: コマンド履歴を表示・管理する。
/// - `history` → 直近 50 件を表示
/// - `history -n 100` → 直近 100 件を表示
/// - `history clear` → 全履歴をクリア
pub(super) fn execute(args: &[&str]) -> CommandResult {
    let parsed = match super::parse_args::<HistoryArgs>("history", args) {
        Ok(a) => a,
        Err(result) => return result,
    };

    match parsed.command {
        Some(HistoryCommand::Clear) => clear_history(),
        None => list_history(parsed.count, parsed.dirs),
    }
}

/// 直近 N 件の履歴を表示する。
/// `dirs` が true の場合、各エントリに実行ディレクトリを付加する。
fn list_history(count: usize, dirs: bool) -> CommandResult {
    let conn = match open_history_db() {
        Ok(c) => c,
        Err(result) => return result,
    };

    let sql = if dirs {
        "SELECT id, command, cwd FROM command_history ORDER BY id DESC LIMIT ?1"
    } else {
        "SELECT id, command FROM command_history ORDER BY id DESC LIMIT ?1"
    };

    let mut stmt = match conn.prepare(sql) {
        Ok(s) => s,
        Err(e) => {
            let msg = format!("jarvish: history: failed to query: {e}\n");
            eprint!("{msg}");
            return CommandResult::error(msg, 1);
        }
    };

    if dirs {
        list_history_with_dirs(&mut stmt, count)
    } else {
        list_history_simple(&mut stmt, count)
    }
}

/// ディレクトリなしの通常表示
fn list_history_simple(stmt: &mut rusqlite::Statement, count: usize) -> CommandResult {
    let rows = match stmt.query_map(rusqlite::params![count as i64], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    }) {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("jarvish: history: failed to query: {e}\n");
            eprint!("{msg}");
            return CommandResult::error(msg, 1);
        }
    };

    let mut entries: Vec<(i64, String)> = Vec::new();
    for row in rows {
        match row {
            Ok(entry) => entries.push(entry),
            Err(e) => {
                let msg = format!("jarvish: history: failed to read row: {e}\n");
                eprint!("{msg}");
                return CommandResult::error(msg, 1);
            }
        }
    }

    entries.reverse();

    let mut output = String::new();
    for (id, command) in &entries {
        let line = format!("{id:>6}  {command}\n");
        output.push_str(&line);
    }
    print!("{output}");

    CommandResult::success(output)
}

/// ディレクトリ付きの表示
fn list_history_with_dirs(stmt: &mut rusqlite::Statement, count: usize) -> CommandResult {
    let rows = match stmt.query_map(rusqlite::params![count as i64], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    }) {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("jarvish: history: failed to query: {e}\n");
            eprint!("{msg}");
            return CommandResult::error(msg, 1);
        }
    };

    let mut entries: Vec<(i64, String, String)> = Vec::new();
    for row in rows {
        match row {
            Ok(entry) => entries.push(entry),
            Err(e) => {
                let msg = format!("jarvish: history: failed to read row: {e}\n");
                eprint!("{msg}");
                return CommandResult::error(msg, 1);
            }
        }
    }

    entries.reverse();

    let mut output = String::new();
    for (id, command, cwd) in &entries {
        let line = format!("{id:>6}  {cwd}  {command}\n");
        output.push_str(&line);
    }
    print!("{output}");

    CommandResult::success(output)
}

/// 全履歴をクリアする。
fn clear_history() -> CommandResult {
    let conn = match open_history_db() {
        Ok(c) => c,
        Err(result) => return result,
    };

    match conn.execute("DELETE FROM command_history", []) {
        Ok(_) => {
            let msg = "history cleared\n".to_string();
            print!("{msg}");
            CommandResult::success(msg)
        }
        Err(e) => {
            let msg = format!("jarvish: history: failed to clear: {e}\n");
            eprint!("{msg}");
            CommandResult::error(msg, 1)
        }
    }
}

/// BlackBox の history.db への接続を開く。
fn open_history_db() -> Result<Connection, CommandResult> {
    let db_path = BlackBox::data_dir().join("history.db");

    let conn = Connection::open(&db_path).map_err(|e| {
        let msg = format!("jarvish: history: failed to open database: {e}\n");
        eprint!("{msg}");
        CommandResult::error(msg, 1)
    })?;

    // WAL モードを有効にして並行アクセスを安全にする
    let _ = conn.execute_batch("PRAGMA journal_mode=WAL;");

    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::BlackBox;
    use tempfile::TempDir;

    /// テスト用に一時ディレクトリに BlackBox を作成し、履歴を挿入するヘルパー。
    fn setup_test_db(commands: &[&str]) -> TempDir {
        let tmp = TempDir::new().unwrap();
        let bb = BlackBox::open_at(tmp.path().to_path_buf()).unwrap();

        for cmd in commands {
            let result = crate::engine::CommandResult {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 0,
                action: crate::engine::LoopAction::Continue,
                used_alt_screen: false,
            };
            bb.record(cmd, &result).unwrap();
        }

        tmp
    }

    #[test]
    fn history_list_shows_entries() {
        let tmp = setup_test_db(&["echo hello", "ls -la", "git status"]);
        let db_path = tmp.path().join("history.db");

        // 直接 DB を使ってテスト（open_history_db は実際の data_dir を使うため）
        let conn = Connection::open(&db_path).unwrap();
        let mut stmt = conn
            .prepare("SELECT id, command FROM command_history ORDER BY id DESC LIMIT 50")
            .unwrap();
        let rows: Vec<(i64, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        assert_eq!(rows.len(), 3);
        // DESC なので最新が先頭
        assert_eq!(rows[0].1, "git status");
        assert_eq!(rows[1].1, "ls -la");
        assert_eq!(rows[2].1, "echo hello");
    }

    #[test]
    fn history_clear_removes_all() {
        let tmp = setup_test_db(&["cmd1", "cmd2", "cmd3"]);
        let db_path = tmp.path().join("history.db");

        let conn = Connection::open(&db_path).unwrap();

        // クリア前
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM command_history", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 3);

        // クリア
        conn.execute("DELETE FROM command_history", []).unwrap();

        // クリア後
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM command_history", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn history_help_returns_success() {
        let result = execute(&["--help"]);
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("history"));
    }

    #[test]
    fn history_clap_parses_count() {
        let args = HistoryArgs::try_parse_from(["history", "-n", "100"]).unwrap();
        assert_eq!(args.count, 100);
        assert!(!args.dirs);
        assert!(args.command.is_none());
    }

    #[test]
    fn history_clap_parses_dirs() {
        let args = HistoryArgs::try_parse_from(["history", "--dirs"]).unwrap();
        assert!(args.dirs);
        assert_eq!(args.count, 50);

        let args = HistoryArgs::try_parse_from(["history", "-d", "-n", "20"]).unwrap();
        assert!(args.dirs);
        assert_eq!(args.count, 20);
    }

    #[test]
    fn history_clap_parses_clear() {
        let args = HistoryArgs::try_parse_from(["history", "clear"]).unwrap();
        assert!(matches!(args.command, Some(HistoryCommand::Clear)));
    }
}
