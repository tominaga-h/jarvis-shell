use crate::engine::CommandResult;

use super::flag_file::write_update_flag;
use super::version::is_newer_version;

/// 最新バージョンの確認のみ行う（--check オプション）。
pub(super) fn check_for_updates() -> CommandResult {
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

/// GitHub Releases API で最新バージョンを取得する。
pub(super) fn get_latest_release_version() -> Result<String, Box<dyn std::error::Error>> {
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
pub(super) fn perform_update() -> CommandResult {
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
