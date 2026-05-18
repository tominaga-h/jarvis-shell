//! cdj: 履歴ディレクトリから fzf で 1 件選んで cd するビルトイン
//!
//! 候補ソースは `cdhist` と同じ `recent_unique_dirs`。
//! - 候補 1 件: fzf を起動せず即 cd
//! - 候補複数: fzf で選択させて cd
//! - 候補 0 件: "no matching directories" + exit 1
//! - fzf キャンセル: cwd 不変で exit 130
//!
//! cwd / `dir_stack` を更新するため、ディスパッチ経由ではなく
//! `Shell::try_shell_builtins` 経由でしか正しく動作しない。

use std::path::PathBuf;

use clap::Parser;

use crate::engine::builtins::cd;
use crate::engine::builtins::cdhist;
use crate::engine::builtins::wrapper::fzf::Fzf;
use crate::engine::CommandResult;

/// `cdhist` から取得する候補の上限。
///
/// fzf に流すには十分大きく、起動コストは候補数に対して線形なので
/// `cdhist` の既定 200 より多めに 1000 を採用する。
const CANDIDATE_LIMIT: usize = 1000;

/// cdj: 履歴ディレクトリから fzf で選んで cd する。
#[derive(Parser)]
#[command(name = "cdj", about = "Jump to a directory from cd history via fzf")]
struct CdjArgs {
    /// Filter candidates by case-insensitive substring
    pattern: Option<String>,
}

/// dispatch_builtin 経由で呼ばれた際のスタブ。
///
/// `cdj` は `dir_stack` の更新が必要なため、対話シェル経由でしか正しく動作しない。
pub(crate) fn execute_stub(args: &[&str]) -> CommandResult {
    // --help だけは clap が処理して即終了する
    if let Err(result) = super::parse_args::<CdjArgs>("cdj", args) {
        return result;
    }
    let msg = "jarvish: cdj: requires interactive shell\n".to_string();
    eprint!("{msg}");
    CommandResult::error(msg, 1)
}

/// `Shell::try_shell_builtins` 経由で呼ばれる本体。
pub(crate) fn execute(args: &[&str], dir_stack: &mut Vec<PathBuf>) -> CommandResult {
    let parsed = match super::parse_args::<CdjArgs>("cdj", args) {
        Ok(a) => a,
        Err(result) => return result,
    };

    let candidates = match cdhist::collect_candidates(CANDIDATE_LIMIT) {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("jarvish: cdj: {e}\n");
            eprint!("{msg}");
            return CommandResult::error(msg, 1);
        }
    };

    let filtered = filter_candidates(&candidates, parsed.pattern.as_deref());

    pick_and_cd(filtered, dir_stack)
}

/// 候補リストを pattern で case-insensitive substring 絞り込みする。
///
/// `pattern` が `None` または空文字なら全件返す。
fn filter_candidates(candidates: &[String], pattern: Option<&str>) -> Vec<String> {
    match pattern {
        None | Some("") => candidates.to_vec(),
        Some(p) => {
            let needle = p.to_lowercase();
            candidates
                .iter()
                .filter(|c| c.to_lowercase().contains(&needle))
                .cloned()
                .collect()
        }
    }
}

/// 絞り込み結果に応じて cd を実行する。
///
/// - 0 件 → exit 1
/// - 1 件 → 即 cd
/// - 複数 → fzf で選択 → cd（キャンセル時 exit 130）
fn pick_and_cd(filtered: Vec<String>, dir_stack: &mut Vec<PathBuf>) -> CommandResult {
    match filtered.len() {
        0 => {
            let msg = "jarvish: cdj: no matching directories\n".to_string();
            eprint!("{msg}");
            CommandResult::error(msg, 1)
        }
        1 => {
            let target = filtered.into_iter().next().expect("len == 1");
            cd::execute(&[target.as_str()], dir_stack)
        }
        _ => {
            let fzf = Fzf::new();
            let child = match fzf.spawn() {
                Ok(c) => c,
                Err(e) => {
                    let msg = format!("jarvish: cdj: {e}\n");
                    eprint!("{msg}");
                    return CommandResult::error(msg, 127);
                }
            };

            match child.run(&filtered) {
                Ok(Some(selected)) => cd::execute(&[selected.as_str()], dir_stack),
                Ok(None) => {
                    // キャンセル / no-match — 静かに終了 (cwd 不変、exit 130)
                    CommandResult::error(String::new(), 130)
                }
                Err(e) => {
                    let msg = format!("jarvish: cdj: {e}\n");
                    eprint!("{msg}");
                    CommandResult::error(msg, 1)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::builtins::cwd::test_helpers::CwdGuard;
    use rusqlite::{params, Connection};
    use serial_test::serial;
    use std::env;
    use tempfile::TempDir;

    fn setup_db_with_dirs(dirs: &[&str]) -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("history.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS command_history (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                command     TEXT    NOT NULL,
                cwd         TEXT    NOT NULL,
                exit_code   INTEGER NOT NULL,
                stdout_hash TEXT,
                stderr_hash TEXT,
                created_at  TEXT    NOT NULL,
                session_id  INTEGER
            );",
        )
        .unwrap();
        for d in dirs {
            conn.execute(
                "INSERT INTO command_history (command, cwd, exit_code, created_at) \
                 VALUES ('cmd', ?1, 0, datetime('now'))",
                params![d],
            )
            .unwrap();
        }
        (tmp, db_path)
    }

    #[test]
    fn cdj_help_returns_success() {
        let result = execute_stub(&["--help"]);
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("cdj"));
    }

    #[test]
    fn cdj_clap_parses_pattern() {
        let args = CdjArgs::try_parse_from(["cdj", "myproj"]).unwrap();
        assert_eq!(args.pattern.as_deref(), Some("myproj"));

        let args = CdjArgs::try_parse_from(["cdj"]).unwrap();
        assert!(args.pattern.is_none());
    }

    #[test]
    fn cdj_filter_candidates_case_insensitive_substring() {
        let candidates = vec![
            "/home/user/Projects/jarvis".to_string(),
            "/tmp/foo".to_string(),
            "/var/log/PROJ".to_string(),
        ];

        let all = filter_candidates(&candidates, None);
        assert_eq!(all.len(), 3);

        let empty = filter_candidates(&candidates, Some(""));
        assert_eq!(empty.len(), 3);

        let hit = filter_candidates(&candidates, Some("proj"));
        assert_eq!(
            hit,
            vec![
                "/home/user/Projects/jarvis".to_string(),
                "/var/log/PROJ".to_string(),
            ]
        );

        let none = filter_candidates(&candidates, Some("xyzzy"));
        assert!(none.is_empty());
    }

    #[test]
    #[serial]
    fn cdj_single_match_cds_without_fzf() {
        let _guard = CwdGuard::new();
        let dir = TempDir::new().unwrap();
        let dir_path = dir.path().to_path_buf();

        // 候補に該当ディレクトリを 1 つ入れた状態を作る
        let candidates = vec![dir_path.to_string_lossy().into_owned()];
        let mut dir_stack = Vec::new();
        let result = pick_and_cd(candidates, &mut dir_stack);
        assert_eq!(result.exit_code, 0, "single-match should cd successfully");

        let new_cwd = env::current_dir().unwrap();
        assert_eq!(
            new_cwd.canonicalize().unwrap(),
            dir_path.canonicalize().unwrap()
        );
    }

    #[test]
    #[serial]
    fn cdj_no_match_returns_error() {
        let _guard = CwdGuard::new();
        let before = env::current_dir().unwrap();

        let mut dir_stack = Vec::new();
        let result = pick_and_cd(Vec::new(), &mut dir_stack);
        assert_eq!(result.exit_code, 1);
        assert!(result.stderr.contains("no matching directories"));

        // cwd が変わらないこと
        let after = env::current_dir().unwrap();
        assert_eq!(
            after.canonicalize().unwrap(),
            before.canonicalize().unwrap()
        );
    }

    #[test]
    fn cdj_stub_without_help_reports_interactive_requirement() {
        let result = execute_stub(&[]);
        assert_eq!(result.exit_code, 1);
        assert!(result.stderr.contains("requires interactive shell"));
    }

    #[test]
    fn cdj_collect_candidates_with_db_works() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();
        let a = dir_a.path().to_str().unwrap();
        let b = dir_b.path().to_str().unwrap();

        let (_tmp, db) = setup_db_with_dirs(&[a, b]);

        // exclude = None なので両方含む。
        let candidates = cdhist::collect_candidates_with_db(&db, 10, None).unwrap();
        assert_eq!(candidates.len(), 2);
        assert!(candidates.contains(&a.to_string()));
        assert!(candidates.contains(&b.to_string()));
    }
}
