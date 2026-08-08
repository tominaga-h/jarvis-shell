use super::BlackBox;
use crate::engine::{CommandResult, LoopAction};
use tempfile::TempDir;

fn make_result(stdout: &str, stderr: &str, exit_code: i32) -> CommandResult {
    CommandResult {
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
        exit_code,
        action: LoopAction::Continue,
        used_alt_screen: false,
    }
}

#[test]
fn open_creates_database() {
    let tmp = TempDir::new().unwrap();
    let bb = BlackBox::open_at(tmp.path().to_path_buf(), 1).unwrap();

    let count: i32 = bb
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='command_history'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn record_stores_command_metadata() {
    let tmp = TempDir::new().unwrap();
    let bb = BlackBox::open_at(tmp.path().to_path_buf(), 1).unwrap();

    let result = make_result("hello world\n", "", 0);
    bb.record("echo hello world", &result).unwrap();

    let (cmd, exit_code): (String, i32) = bb
        .conn
        .query_row(
            "SELECT command, exit_code FROM command_history WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(cmd, "echo hello world");
    assert_eq!(exit_code, 0);
}

#[test]
fn record_stores_and_retrieves_blobs() {
    let tmp = TempDir::new().unwrap();
    let bb = BlackBox::open_at(tmp.path().to_path_buf(), 1).unwrap();

    let stdout_content = "output line 1\noutput line 2\n";
    let stderr_content = "error: something went wrong\n";
    let result = make_result(stdout_content, stderr_content, 1);
    bb.record("failing-command", &result).unwrap();

    let (stdout_hash, stderr_hash): (Option<String>, Option<String>) = bb
        .conn
        .query_row(
            "SELECT stdout_hash, stderr_hash FROM command_history WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();

    let loaded_stdout = bb.blob_store.load(&stdout_hash.unwrap()).unwrap();
    assert_eq!(loaded_stdout, stdout_content);

    let loaded_stderr = bb.blob_store.load(&stderr_hash.unwrap()).unwrap();
    assert_eq!(loaded_stderr, stderr_content);
}

#[test]
fn record_with_empty_output_stores_null_hashes() {
    let tmp = TempDir::new().unwrap();
    let bb = BlackBox::open_at(tmp.path().to_path_buf(), 1).unwrap();

    let result = make_result("", "", 0);
    bb.record("cd /tmp", &result).unwrap();

    let (stdout_hash, stderr_hash): (Option<String>, Option<String>) = bb
        .conn
        .query_row(
            "SELECT stdout_hash, stderr_hash FROM command_history WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();

    assert!(stdout_hash.is_none());
    assert!(stderr_hash.is_none());
}

#[test]
fn get_recent_context_returns_formatted_history() {
    let tmp = TempDir::new().unwrap();
    let bb = BlackBox::open_at(tmp.path().to_path_buf(), 1).unwrap();

    bb.record("echo hello", &make_result("hello\n", "", 0))
        .unwrap();
    bb.record("bad-cmd", &make_result("", "error: not found\n", 1))
        .unwrap();

    let ctx = bb.get_recent_context(5).unwrap();
    assert!(ctx.contains("echo hello"));
    assert!(ctx.contains("bad-cmd"));
    assert!(ctx.contains("error: not found"));
    assert!(ctx.contains("hello"));
}

#[test]
fn get_recent_context_empty_when_no_history() {
    let tmp = TempDir::new().unwrap();
    let bb = BlackBox::open_at(tmp.path().to_path_buf(), 1).unwrap();

    let ctx = bb.get_recent_context(5).unwrap();
    assert!(ctx.is_empty());
}

#[test]
fn multiple_records_increment_id() {
    let tmp = TempDir::new().unwrap();
    let bb = BlackBox::open_at(tmp.path().to_path_buf(), 1).unwrap();

    bb.record("cmd1", &make_result("out1", "", 0)).unwrap();
    bb.record("cmd2", &make_result("out2", "", 0)).unwrap();
    bb.record("cmd3", &make_result("out3", "", 0)).unwrap();

    let count: i32 = bb
        .conn
        .query_row("SELECT COUNT(*) FROM command_history", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 3);
}
