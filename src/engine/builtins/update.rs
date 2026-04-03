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
/// 自プロセスを更新・再起動する。兄弟プロセスにも SIGUSR1 で再起動を通知する。
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
        .map(|s| s.contains("/Cellar/") || s.contains("/homebrew/"))
        .unwrap_or(false)
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
            if latest_clean == current {
                let msg = format!("jarvish v{current} is up to date.\n");
                print!("{msg}");
                CommandResult::success(msg)
            } else {
                let msg = format!(
                    "New version available: v{latest_clean} (current: v{current})\n\
                     Run `update` to install.\n"
                );
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

/// self_update で更新を実行し、兄弟プロセスに SIGUSR1 を送信する。
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
        println!("Updated to v{}!", status.version());

        // 兄弟 jarvish プロセスに SIGUSR1 を送信して再起動を通知
        notify_sibling_processes();

        // 自プロセスも再起動
        println!("Restarting jarvish...");
        CommandResult::restart()
    } else {
        let msg = format!("jarvish v{current} is already up to date.\n");
        print!("{msg}");
        CommandResult::success(msg)
    }
}

/// 兄弟の jarvish プロセスに SIGUSR1 を送信する。
fn notify_sibling_processes() {
    use sysinfo::System;

    let mut sys = System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    let self_pid = std::process::id();

    let mut notified = 0u32;
    for (pid, process) in sys.processes() {
        if pid.as_u32() == self_pid {
            continue;
        }
        if process.name().to_str() == Some("jarvish") {
            unsafe {
                libc::kill(pid.as_u32() as i32, libc::SIGUSR1);
            }
            notified += 1;
        }
    }

    if notified > 0 {
        println!(
            "Sent restart signal to {notified} sibling jarvish {}.",
            if notified == 1 {
                "process"
            } else {
                "processes"
            }
        );
    }
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
        // 現在のバイナリが /Cellar/ を含まなければ false
        // （テスト環境では Homebrew 経由でないはず）
        assert!(!is_homebrew_install());
    }
}
