use std::path::{Path, PathBuf};

use crate::engine::CommandResult;

use super::flag_file::write_update_flag;
use super::version::{is_newer_version, parse_version_from_output};

/// ローカルバイナリのデフォルトパス
pub(super) const DEFAULT_LOCAL_BINARY: &str = "target/release/jarvish";

/// ローカルバイナリのパスを解決する。
///
/// 引数が指定されていればそのまま使い、未指定の場合はデフォルトパスを返す。
pub(super) fn resolve_local_binary_path(specified: Option<&str>) -> PathBuf {
    match specified {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(DEFAULT_LOCAL_BINARY),
    }
}

/// ローカルバイナリのバージョンを `--version` 実行で取得する。
///
/// 出力例: `jarvish 1.8.0` → `"1.8.0"` を返す。
pub(super) fn get_local_binary_version(binary_path: &Path) -> Result<String, String> {
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

/// ローカルバイナリのバージョンを確認する（`--check --local`）。
pub(super) fn check_for_local_updates(binary_path: &Path) -> CommandResult {
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
pub(super) fn perform_local_update(binary_path: &Path) -> CommandResult {
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
pub(super) fn perform_local_update_to(binary_path: &Path, dest: &Path) -> CommandResult {
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
pub(super) fn replace_binary(source: &Path, dest: &Path) -> Result<(), String> {
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
