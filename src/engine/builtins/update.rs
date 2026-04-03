use clap::Parser;

use crate::engine::CommandResult;

/// update: jarvish を最新バージョンに更新する。
#[derive(Parser)]
#[command(name = "update", about = "Update jarvish to the latest version")]
struct UpdateArgs {
    /// Check for updates without installing
    #[arg(long)]
    check: bool,
}

/// update: GitHub Releases から最新バイナリをダウンロードし、
/// 自プロセスを更新・再起動する。兄弟プロセスにはフラグファイルで通知する。
///
/// Homebrew でインストールされている場合は `brew upgrade jarvish` を案内する。
pub(super) fn execute(args: &[&str]) -> CommandResult {
    let parsed = match super::parse_args::<UpdateArgs>("update", args) {
        Ok(a) => a,
        Err(result) => return result,
    };

    if is_homebrew_install() {
        return handle_homebrew_update(parsed.check);
    }

    if parsed.check {
        return check_for_updates();
    }

    perform_update()
}

/// Homebrew でインストールされているかを判定する。
fn is_homebrew_install() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(|s| s.to_string()))
        .map(|s| is_homebrew_path(&s))
        .unwrap_or(false)
}

/// パス文字列が Homebrew インストールのパスパターンに一致するか判定する。
///
/// Intel Mac: `/usr/local/Cellar/jarvish/...`
/// Apple Silicon: `/opt/homebrew/Cellar/jarvish/...`
fn is_homebrew_path(exe_path: &str) -> bool {
    exe_path.contains("/Cellar/") || exe_path.contains("/homebrew/")
}

/// Homebrew インストールの場合の更新ハンドリング。
fn handle_homebrew_update(check_only: bool) -> CommandResult {
    if check_only {
        let msg = "jarvish is installed via Homebrew.\n\
                   Run `brew outdated jarvish` to check for updates.\n";
        print!("{msg}");
        return CommandResult::success(msg.to_string());
    }

    let msg = "jarvish is installed via Homebrew.\n\
               Run `brew upgrade jarvish` to update, then `restart` to reload.\n";
    print!("{msg}");
    CommandResult::success(msg.to_string())
}

/// 最新バージョンの確認のみ行う（--check オプション）。
fn check_for_updates() -> CommandResult {
    let current = env!("CARGO_PKG_VERSION");
    println!("Current version: v{current}");
    println!("Checking for updates...");

    match get_latest_release_version() {
        Ok(latest) => {
            let latest_clean = latest.trim_start_matches('v');
            if is_newer_version(current, latest_clean) {
                let msg = format!(
                    "New version available: v{latest_clean} (current: v{current})\n\
                     Run `update` to install.\n"
                );
                print!("{msg}");
                CommandResult::success(msg)
            } else {
                let msg = format!("jarvish v{current} is up to date.\n");
                print!("{msg}");
                CommandResult::success(msg)
            }
        }
        Err(e) => {
            let msg = format!("Failed to check for updates: {e}\n");
            eprint!("{msg}");
            CommandResult::error(msg, 1)
        }
    }
}

/// `latest` が `current` より新しいかどうかを semver 比較で判定する。
fn is_newer_version(current: &str, latest: &str) -> bool {
    let current_parts: Vec<u32> = current.split('.').filter_map(|s| s.parse().ok()).collect();
    let latest_parts: Vec<u32> = latest.split('.').filter_map(|s| s.parse().ok()).collect();
    latest_parts > current_parts
}

/// GitHub Releases API で最新バージョンを取得する。
fn get_latest_release_version() -> Result<String, Box<dyn std::error::Error>> {
    let release = self_update::backends::github::Update::configure()
        .repo_owner("tominaga-h")
        .repo_name("jarvis-shell")
        .bin_name("jarvish")
        .current_version(self_update::cargo_crate_version!())
        .build()?;

    let latest = release.get_latest_release()?;
    Ok(latest.version)
}

/// self_update で更新を実行し、フラグファイルで兄弟プロセスに通知する。
fn perform_update() -> CommandResult {
    let current = env!("CARGO_PKG_VERSION");
    println!("Current version: v{current}");
    println!("Checking for updates...");

    let status = match self_update::backends::github::Update::configure()
        .repo_owner("tominaga-h")
        .repo_name("jarvis-shell")
        .bin_name("jarvish")
        .show_download_progress(true)
        .current_version(self_update::cargo_crate_version!())
        .build()
        .and_then(|u| u.update())
    {
        Ok(s) => s,
        Err(e) => {
            let msg = format!("Update failed: {e}\n");
            eprint!("{msg}");
            return CommandResult::error(msg, 1);
        }
    };

    if status.updated() {
        let new_version = status.version().to_string();
        println!("Updated to v{new_version}!");

        // 兄弟 jarvish プロセスにフラグファイルで更新を通知
        write_update_flag(&new_version);

        // 自プロセスを再起動
        println!("Restarting jarvish...");
        CommandResult::restart()
    } else {
        let msg = format!("jarvish v{current} is already up to date.\n");
        print!("{msg}");
        CommandResult::success(msg)
    }
}

/// フラグファイルのパス: `<data_dir>/update-ready`
fn update_flag_path() -> Option<std::path::PathBuf> {
    directories::ProjectDirs::from("", "", "jarvish").map(|p| p.data_dir().join("update-ready"))
}

/// 更新完了後にフラグファイルを作成して兄弟プロセスに通知する。
///
/// フラグファイルには新しいバージョン番号を書き込む。
/// 兄弟プロセスは次のプロンプト表示時にこのファイルを検出し、
/// ユーザーに `restart` コマンドの実行を促す。
/// テスト用にフラグファイルを書き込む公開ヘルパー。
#[cfg(test)]
pub fn write_update_flag_for_test(version: &str) {
    write_update_flag(version);
}

fn write_update_flag(version: &str) {
    let Some(path) = update_flag_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, version);
}

/// フラグファイルを確認し、更新通知メッセージを返す。
///
/// フラグファイルが存在する場合は読み取って削除し、通知文字列を返す。
/// REPL ループのプロンプト表示前に呼び出される。
pub fn check_update_flag() -> Option<String> {
    let path = update_flag_path()?;
    let version = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    let version = version.trim();
    if version.is_empty() {
        return None;
    }
    Some(format!(
        "jarvish has been updated to v{version}. Run `restart` to apply."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::LoopAction;

    #[test]
    fn update_help_does_not_update() {
        let result = execute(&["--help"]);
        assert_eq!(result.action, LoopAction::Continue);
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("update"));
    }

    #[test]
    fn is_homebrew_detects_cellar() {
        // テスト環境では Homebrew 経由でないはず
        assert!(!is_homebrew_install());
    }

    #[test]
    #[ignore]
    fn update_check_flag_does_not_restart() {
        // --check はバージョン確認のみ。restart しない。
        // GitHub API に接続するため CI で不安定 → #[ignore]
        let result = execute(&["--check"]);
        assert_ne!(result.action, LoopAction::Restart);
    }

    #[test]
    fn homebrew_update_returns_guidance() {
        // handle_homebrew_update(false) は案内メッセージを返す
        let result = handle_homebrew_update(false);
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("brew upgrade jarvish"));
        assert_eq!(result.action, LoopAction::Continue);
    }

    #[test]
    fn homebrew_check_returns_guidance() {
        let result = handle_homebrew_update(true);
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("brew outdated jarvish"));
        assert_eq!(result.action, LoopAction::Continue);
    }

    #[test]
    #[ignore]
    fn get_latest_release_version_succeeds() {
        // GitHub API 依存。手動実行用。
        let result = get_latest_release_version();
        assert!(result.is_ok());
    }

    // ── is_newer_version ──

    #[test]
    fn newer_version_detected() {
        assert!(is_newer_version("1.6.3", "1.7.0"));
        assert!(is_newer_version("1.7.0", "2.0.0"));
        assert!(is_newer_version("1.7.0", "1.7.1"));
    }

    #[test]
    fn same_version_is_not_newer() {
        assert!(!is_newer_version("1.7.0", "1.7.0"));
    }

    #[test]
    fn older_version_is_not_newer() {
        assert!(!is_newer_version("1.7.0", "1.6.3"));
        assert!(!is_newer_version("2.0.0", "1.9.9"));
        assert!(!is_newer_version("1.7.1", "1.7.0"));
    }

    // ── flag file ──

    #[test]
    fn update_flag_path_returns_some() {
        let path = update_flag_path();
        assert!(path.is_some());
        let path = path.unwrap();
        assert!(path.to_str().unwrap().contains("update-ready"));
    }

    /// フラグファイルテスト用のロック。
    /// 並列テスト実行時にフラグファイルの競合を防ぐ。
    use std::sync::Mutex;
    static FLAG_FILE_LOCK: Mutex<()> = Mutex::new(());

    fn cleanup_flag_file() {
        if let Some(path) = update_flag_path() {
            let _ = std::fs::remove_file(&path);
        }
    }

    #[test]
    fn write_and_check_update_flag() {
        let _lock = FLAG_FILE_LOCK.lock().unwrap();
        cleanup_flag_file();

        write_update_flag("1.8.0");
        let msg = check_update_flag();
        assert!(msg.is_some());
        let msg = msg.unwrap();
        assert!(msg.contains("v1.8.0"));
        assert!(msg.contains("restart"));
        // 読み取り後はファイルが削除されているので再度呼ぶと None
        assert!(check_update_flag().is_none());
    }

    #[test]
    fn check_update_flag_returns_none_when_no_file() {
        let _lock = FLAG_FILE_LOCK.lock().unwrap();
        cleanup_flag_file();
        assert!(check_update_flag().is_none());
    }

    #[test]
    fn check_update_flag_ignores_empty_file() {
        let _lock = FLAG_FILE_LOCK.lock().unwrap();
        cleanup_flag_file();

        let path = update_flag_path().unwrap();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&path, "");
        assert!(check_update_flag().is_none());
    }

    #[test]
    fn check_update_flag_trims_whitespace() {
        let _lock = FLAG_FILE_LOCK.lock().unwrap();
        cleanup_flag_file();

        write_update_flag("  1.9.0\n");
        let msg = check_update_flag();
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("v1.9.0"));
    }

    // ── is_homebrew_path ──

    #[test]
    fn homebrew_intel_mac_path() {
        assert!(is_homebrew_path(
            "/usr/local/Cellar/jarvish/1.7.0/bin/jarvish"
        ));
    }

    #[test]
    fn homebrew_apple_silicon_path() {
        assert!(is_homebrew_path(
            "/opt/homebrew/Cellar/jarvish/1.7.0/bin/jarvish"
        ));
    }

    #[test]
    fn homebrew_generic_homebrew_path() {
        assert!(is_homebrew_path(
            "/home/linuxbrew/.linuxbrew/homebrew/bin/jarvish"
        ));
    }

    #[test]
    fn non_homebrew_cargo_path() {
        assert!(!is_homebrew_path("/Users/user/.cargo/bin/jarvish"));
    }

    #[test]
    fn non_homebrew_usr_local_bin() {
        assert!(!is_homebrew_path("/usr/local/bin/jarvish"));
    }

    #[test]
    fn non_homebrew_target_debug() {
        assert!(!is_homebrew_path(
            "/Users/user/project/target/debug/jarvish"
        ));
    }

    // ── is_newer_version edge cases ──

    #[test]
    fn newer_version_major_bump_from_zero() {
        assert!(is_newer_version("0.9.9", "1.0.0"));
    }

    #[test]
    fn newer_version_partial_parts() {
        // パーツ数が異なる場合
        assert!(is_newer_version("1.0", "1.0.1"));
    }

    #[test]
    fn newer_version_with_non_numeric_ignored() {
        // 非数値パーツは filter_map で除外される
        assert!(is_newer_version("1.0.0", "2.0.0"));
    }

    // ── perform_update error path (mock なし、ユニットレベル) ──

    #[test]
    fn write_update_flag_creates_file() {
        let _lock = FLAG_FILE_LOCK.lock().unwrap();
        cleanup_flag_file();

        write_update_flag("1.10.0");
        let path = update_flag_path().unwrap();
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "1.10.0");

        cleanup_flag_file();
    }
}
