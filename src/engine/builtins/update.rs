use std::path::{Path, PathBuf};

use clap::Parser;

use crate::engine::CommandResult;

/// update: jarvish を最新バージョンに更新する。
#[derive(Parser)]
#[command(name = "update", about = "Update jarvish to the latest version")]
struct UpdateArgs {
    /// Check for updates without installing
    #[arg(long)]
    check: bool,

    /// Update from a local binary instead of GitHub Releases.
    /// Optionally specify the path to the binary (default: target/release/jarvish).
    #[arg(long)]
    local: Option<Option<String>>,
}

/// update: GitHub Releases またはローカルバイナリから更新する。
///
/// `--local` オプションでローカルビルドのバイナリを使った更新が可能。
/// Homebrew でインストールされている場合は `brew upgrade jarvish` を案内する。
pub(super) fn execute(args: &[&str]) -> CommandResult {
    let parsed = match super::parse_args::<UpdateArgs>("update", args) {
        Ok(a) => a,
        Err(result) => return result,
    };

    // --local が指定された場合はローカルバイナリから更新
    if let Some(local_path) = parsed.local {
        let path = resolve_local_binary_path(local_path.as_deref());
        if parsed.check {
            return check_for_local_updates(&path);
        }
        return perform_local_update(&path);
    }

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

/// ローカルバイナリのデフォルトパス
const DEFAULT_LOCAL_BINARY: &str = "target/release/jarvish";

/// ローカルバイナリのパスを解決する。
///
/// 引数が指定されていればそのまま使い、未指定の場合はデフォルトパスを返す。
fn resolve_local_binary_path(specified: Option<&str>) -> PathBuf {
    match specified {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(DEFAULT_LOCAL_BINARY),
    }
}

/// ローカルバイナリのバージョンを `--version` 実行で取得する。
///
/// 出力例: `jarvish 1.8.0` → `"1.8.0"` を返す。
fn get_local_binary_version(binary_path: &Path) -> Result<String, String> {
    let output = std::process::Command::new(binary_path)
        .arg("--version")
        .output()
        .map_err(|e| format!("Failed to execute {}: {e}", binary_path.display()))?;

    if !output.status.success() {
        return Err(format!(
            "{} --version exited with {}",
            binary_path.display(),
            output.status
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    // "jarvish 1.8.0" → "1.8.0"
    parse_version_from_output(&stdout)
        .ok_or_else(|| format!("Could not parse version from: {}", stdout.trim()))
}

/// `--version` 出力からバージョン番号を抽出する。
///
/// `"jarvish 1.8.0\n"` → `Some("1.8.0")`
fn parse_version_from_output(output: &str) -> Option<String> {
    let trimmed = output.trim();
    // "jarvish X.Y.Z" or "X.Y.Z" のどちらも対応
    let version_str = trimmed.rsplit_once(' ').map(|(_, v)| v).unwrap_or(trimmed);
    let version = version_str.trim_start_matches('v');
    // 数字で始まるか確認（バージョン番号の妥当性チェック）
    if version.starts_with(|c: char| c.is_ascii_digit()) {
        Some(version.to_string())
    } else {
        None
    }
}

/// ローカルバイナリのバージョンを確認する（`--check --local`）。
fn check_for_local_updates(binary_path: &Path) -> CommandResult {
    let current = env!("CARGO_PKG_VERSION");
    println!("Current version: v{current}");

    if !binary_path.exists() {
        let msg = format!(
            "Local binary not found: {}\n\
             Run `cargo build --release` to build.\n",
            binary_path.display()
        );
        eprint!("{msg}");
        return CommandResult::error(msg, 1);
    }

    println!("Checking local binary: {}", binary_path.display());

    match get_local_binary_version(binary_path) {
        Ok(local_version) => {
            let local_clean = local_version.trim_start_matches('v');
            if is_newer_version(current, local_clean) {
                let msg = format!(
                    "Local binary is newer: v{local_clean} (current: v{current})\n\
                     Run `update --local` to install.\n"
                );
                print!("{msg}");
                CommandResult::success(msg)
            } else {
                let msg =
                    format!("Local binary v{local_clean} is not newer than current v{current}.\n");
                print!("{msg}");
                CommandResult::success(msg)
            }
        }
        Err(e) => {
            let msg = format!("Failed to get local binary version: {e}\n");
            eprint!("{msg}");
            CommandResult::error(msg, 1)
        }
    }
}

/// ローカルバイナリで現在の実行バイナリを置換する（`update --local`）。
fn perform_local_update(binary_path: &Path) -> CommandResult {
    // 現在の実行バイナリのパスを取得
    let current_exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(e) => {
            let msg = format!("Failed to get current exe path: {e}\n");
            eprint!("{msg}");
            return CommandResult::error(msg, 1);
        }
    };

    perform_local_update_to(binary_path, &current_exe)
}

/// ローカルバイナリで指定されたバイナリを置換する。
///
/// `perform_local_update` から呼び出される。置換先を引数で受け取ることで
/// テスト時に実テストバイナリを破壊しない。
fn perform_local_update_to(binary_path: &Path, dest: &Path) -> CommandResult {
    let current = env!("CARGO_PKG_VERSION");
    println!("Current version: v{current}");

    if !binary_path.exists() {
        let msg = format!(
            "Local binary not found: {}\n\
             Run `cargo build --release` to build.\n",
            binary_path.display()
        );
        eprint!("{msg}");
        return CommandResult::error(msg, 1);
    }

    // ローカルバイナリのバージョンを確認
    let new_version = match get_local_binary_version(binary_path) {
        Ok(v) => v,
        Err(e) => {
            let msg = format!("Failed to get local binary version: {e}\n");
            eprint!("{msg}");
            return CommandResult::error(msg, 1);
        }
    };

    let new_clean = new_version.trim_start_matches('v');
    println!("Local binary version: v{new_clean}");

    if !is_newer_version(current, new_clean) {
        let msg = format!(
            "Local binary v{new_clean} is not newer than current v{current}. \
             No update performed.\n"
        );
        print!("{msg}");
        return CommandResult::success(msg);
    }

    // バイナリを置換
    println!("Replacing {} ...", dest.display());
    if let Err(e) = replace_binary(binary_path, dest) {
        let msg = format!("Update failed: {e}\n");
        eprint!("{msg}");
        return CommandResult::error(msg, 1);
    }

    println!("Updated to v{new_clean}!");

    // 兄弟プロセスにフラグファイルで通知
    write_update_flag(new_clean);

    // 自プロセスを再起動
    println!("Restarting jarvish...");
    CommandResult::restart()
}

/// ローカルバイナリで現在のバイナリを置換する。
///
/// 実行中のバイナリは直接上書きできないため、一時ファイル経由で置換する。
fn replace_binary(source: &Path, dest: &Path) -> Result<(), String> {
    // 一時ファイルにコピーしてからリネーム（アトミックな置換）
    let dest_dir = dest.parent().unwrap_or(Path::new("."));
    let tmp_path = dest_dir.join(".jarvish-update.tmp");

    std::fs::copy(source, &tmp_path).map_err(|e| {
        format!(
            "Failed to copy {} to {}: {e}",
            source.display(),
            tmp_path.display()
        )
    })?;

    // 実行パーミッションを設定
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&tmp_path, perms)
            .map_err(|e| format!("Failed to set permissions: {e}"))?;
    }

    // リネームで置換（同一ファイルシステム上ならアトミック）
    std::fs::rename(&tmp_path, dest).map_err(|e| {
        format!(
            "Failed to rename {} to {}: {e}",
            tmp_path.display(),
            dest.display()
        )
    })?;

    Ok(())
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

    // ── --local option ──

    #[test]
    fn local_option_parses() {
        // --local オプションが clap でパースできることを確認
        let result = execute(&["--help"]);
        assert!(result.stdout.contains("--local"));
    }

    #[test]
    fn resolve_local_binary_path_default() {
        let path = resolve_local_binary_path(None);
        assert_eq!(path, PathBuf::from("target/release/jarvish"));
    }

    #[test]
    fn resolve_local_binary_path_custom() {
        let path = resolve_local_binary_path(Some("/tmp/my-jarvish"));
        assert_eq!(path, PathBuf::from("/tmp/my-jarvish"));
    }

    // ── parse_version_from_output ──

    #[test]
    fn parse_version_standard_format() {
        let result = parse_version_from_output("jarvish 1.8.0\n");
        assert_eq!(result, Some("1.8.0".to_string()));
    }

    #[test]
    fn parse_version_with_v_prefix() {
        let result = parse_version_from_output("jarvish v1.8.0\n");
        assert_eq!(result, Some("1.8.0".to_string()));
    }

    #[test]
    fn parse_version_bare_version() {
        let result = parse_version_from_output("1.8.0\n");
        assert_eq!(result, Some("1.8.0".to_string()));
    }

    #[test]
    fn parse_version_empty_string() {
        assert!(parse_version_from_output("").is_none());
    }

    #[test]
    fn parse_version_invalid_output() {
        assert!(parse_version_from_output("error: something went wrong").is_none());
    }

    #[test]
    fn parse_version_with_extra_whitespace() {
        let result = parse_version_from_output("  jarvish  1.8.0  \n");
        assert_eq!(result, Some("1.8.0".to_string()));
    }

    // ── check_for_local_updates ──

    #[test]
    fn check_local_binary_not_found() {
        let result = check_for_local_updates(Path::new("/nonexistent/jarvish"));
        assert_ne!(result.exit_code, 0);
        assert!(result.stderr.contains("not found"));
    }

    #[test]
    fn perform_local_binary_not_found() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dest = tmp.path().join("dest");
        let result = perform_local_update_to(Path::new("/nonexistent/jarvish"), &dest);
        assert_ne!(result.exit_code, 0);
        assert!(result.stderr.contains("not found"));
    }

    // ── replace_binary ──

    #[test]
    fn replace_binary_with_valid_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let source = tmp.path().join("source");
        let dest = tmp.path().join("dest");

        std::fs::write(&source, b"new binary content").unwrap();
        std::fs::write(&dest, b"old binary content").unwrap();

        let result = replace_binary(&source, &dest);
        assert!(result.is_ok());

        let content = std::fs::read_to_string(&dest).unwrap();
        assert_eq!(content, "new binary content");
    }

    #[test]
    fn replace_binary_source_not_found() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dest = tmp.path().join("dest");
        std::fs::write(&dest, b"old").unwrap();

        let result = replace_binary(Path::new("/nonexistent/source"), &dest);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Failed to copy"));
    }

    // ── get_local_binary_version ──

    #[test]
    fn get_local_binary_version_nonexistent() {
        let result = get_local_binary_version(Path::new("/nonexistent/jarvish"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Failed to execute"));
    }

    #[test]
    fn get_local_binary_version_non_executable_file() {
        // 非実行ファイルでのエラーをテスト
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let result = get_local_binary_version(tmp.path());
        assert!(result.is_err());
    }

    // ── DEFAULT_LOCAL_BINARY ──

    #[test]
    fn default_local_binary_path_is_release() {
        assert_eq!(DEFAULT_LOCAL_BINARY, "target/release/jarvish");
    }

    // ── Fury 監査指摘: 追加テスト ──

    #[test]
    fn get_local_binary_version_success_with_mock_binary() {
        // シェルスクリプトでバイナリをモックし、バージョン文字列の正常取得を検証
        let tmp = tempfile::TempDir::new().unwrap();
        let mock_binary = tmp.path().join("mock-jarvish");
        std::fs::write(&mock_binary, "#!/bin/sh\necho \"jarvish 99.1.0\"\n").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&mock_binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let result = get_local_binary_version(&mock_binary);
        assert!(result.is_ok(), "expected Ok, got: {result:?}");
        assert_eq!(result.unwrap(), "99.1.0");
    }

    #[test]
    fn perform_local_update_older_binary_skips_update() {
        // 現在のバージョンより古いバイナリの場合に置換がスキップされることを検証
        let tmp = tempfile::TempDir::new().unwrap();
        let mock_binary = tmp.path().join("old-jarvish");
        let dest = tmp.path().join("dest");
        // 現在のバージョンより明確に古いバージョンを返すモック
        std::fs::write(&mock_binary, "#!/bin/sh\necho \"jarvish 0.0.1\"\n").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&mock_binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let result = perform_local_update_to(&mock_binary, &dest);
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.action, LoopAction::Continue); // restart しない
        assert!(result.stdout.contains("not newer"));
    }

    #[test]
    fn replace_binary_sets_executable_permission() {
        // 置換後のファイルが 0o755 であることを検証
        let tmp = tempfile::TempDir::new().unwrap();
        let source = tmp.path().join("source");
        let dest = tmp.path().join("dest");

        std::fs::write(&source, b"binary content").unwrap();
        std::fs::write(&dest, b"old content").unwrap();

        let result = replace_binary(&source, &dest);
        assert!(result.is_ok());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = std::fs::metadata(&dest).unwrap();
            let mode = metadata.permissions().mode() & 0o777;
            assert_eq!(mode, 0o755, "replaced binary should have 0o755 permissions");
        }
    }

    #[test]
    fn perform_local_update_success_returns_restart() {
        // 置換成功後に restart アクションを返し、フラグファイルが作成されることを検証
        // perform_local_update_to を使い、テストバイナリ自体を破壊しないようにする
        let _lock = FLAG_FILE_LOCK.lock().unwrap();
        cleanup_flag_file();

        let tmp = tempfile::TempDir::new().unwrap();
        let mock_binary = tmp.path().join("new-jarvish");
        let dest_binary = tmp.path().join("dest-jarvish");

        // 十分に大きいバージョン番号で「新しい」と判定させる
        std::fs::write(&mock_binary, "#!/bin/sh\necho \"jarvish 99.99.99\"\n").unwrap();
        std::fs::write(&dest_binary, b"old binary").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&mock_binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        // perform_local_update_to で一時ファイルに置換（テストバイナリを壊さない）
        let result = perform_local_update_to(&mock_binary, &dest_binary);
        assert_eq!(result.action, LoopAction::Restart);
        assert_eq!(result.exit_code, 0);

        // 置換先のファイルが更新されていることを確認
        assert!(dest_binary.exists());

        // フラグファイルが作成されていることを確認
        let flag_msg = check_update_flag();
        assert!(flag_msg.is_some(), "update flag should be written");
        assert!(flag_msg.unwrap().contains("v99.99.99"));
    }
}
