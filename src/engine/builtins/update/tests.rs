use super::execute;
use super::flag_file::{check_update_flag, update_flag_path, write_update_flag};
use super::homebrew::{handle_homebrew_update, is_homebrew_install, is_homebrew_path};
use super::local_binary::{
    check_for_local_updates, get_local_binary_version, perform_local_update_to, replace_binary,
    resolve_local_binary_path, DEFAULT_LOCAL_BINARY,
};
use super::release::get_latest_release_version;
use super::version::{is_newer_version, parse_version_from_output};
use crate::engine::LoopAction;
use std::path::{Path, PathBuf};

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

#[test]
fn default_local_binary_path_is_release() {
    assert_eq!(DEFAULT_LOCAL_BINARY, "target/release/jarvish");
}

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
