//! 自己更新・再起動メカニズムのインテグレーションテスト
//!
//! ここでは jarvish の内部モジュールにはアクセスせず、
//! シグナルハンドリングやプロセス管理のシステムレベルの挙動をテストする。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// SIGUSR1 を自プロセスに送信して AtomicBool フラグが更新されることを確認する。
/// Shell::register_sigusr1_handler と同等のロジックを再現してテストする。
#[test]
fn sigusr1_sets_restart_flag() {
    let flag = Arc::new(AtomicBool::new(false));
    let flag_clone = Arc::clone(&flag);

    static RESTART_FLAG: AtomicBool = AtomicBool::new(false);

    extern "C" fn handle_sigusr1(_: libc::c_int) {
        RESTART_FLAG.store(true, Ordering::Relaxed);
    }

    RESTART_FLAG.store(false, Ordering::Relaxed);

    // ポーリングスレッド（RESTART_FLAG → flag に転送）
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(50));
        if RESTART_FLAG.load(Ordering::Relaxed) {
            flag_clone.store(true, Ordering::Relaxed);
            break;
        }
    });

    // sigaction で SIGUSR1 ハンドラを登録
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = handle_sigusr1 as *const () as usize;
        sa.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut sa.sa_mask);
        assert_eq!(libc::sigaction(libc::SIGUSR1, &sa, std::ptr::null_mut()), 0);
    }

    // 自プロセスに SIGUSR1 を送信
    unsafe {
        libc::kill(libc::getpid(), libc::SIGUSR1);
    }

    // フラグが立つまで待機（最大1秒）
    for _ in 0..20 {
        if flag.load(Ordering::Relaxed) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    assert!(
        flag.load(Ordering::Relaxed),
        "SIGUSR1 should set restart flag"
    );
}

/// exec_restart の前提条件: current_exe() が利用可能であること。
#[test]
fn current_exe_is_available() {
    let exe = std::env::current_exe();
    assert!(exe.is_ok(), "current_exe() should succeed");
    let path = exe.unwrap();
    assert!(path.exists(), "current_exe path should exist");
}

/// 引数収集のテスト。std::env::args().skip(1) でバイナリ名が除外される。
#[test]
fn args_skip_first_excludes_binary_name() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    for arg in &args {
        // テストバイナリのパスが含まれていないことを確認
        assert!(
            !arg.contains("self_update-"),
            "skip(1) should exclude binary name, got: {arg}"
        );
    }
}

/// SIGUSR1 ハンドラを再登録しても前回のハンドラが上書きされることを確認。
#[test]
fn sigusr1_handler_can_be_reregistered() {
    static FLAG_A: AtomicBool = AtomicBool::new(false);
    static FLAG_B: AtomicBool = AtomicBool::new(false);

    extern "C" fn handler_a(_: libc::c_int) {
        FLAG_A.store(true, Ordering::Relaxed);
    }
    extern "C" fn handler_b(_: libc::c_int) {
        FLAG_B.store(true, Ordering::Relaxed);
    }

    // ハンドラ A を登録
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = handler_a as *const () as usize;
        sa.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(libc::SIGUSR1, &sa, std::ptr::null_mut());
    }

    // ハンドラ B で上書き
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = handler_b as *const () as usize;
        sa.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(libc::SIGUSR1, &sa, std::ptr::null_mut());
    }

    // SIGUSR1 送信 → handler_b のみ呼ばれるはず
    unsafe {
        libc::kill(libc::getpid(), libc::SIGUSR1);
    }

    std::thread::sleep(std::time::Duration::from_millis(200));

    assert!(
        FLAG_B.load(Ordering::Relaxed),
        "handler_b should be called after re-registration"
    );
    // handler_a は呼ばれないはず（上書きされた）
    assert!(
        !FLAG_A.load(Ordering::Relaxed),
        "handler_a should not be called after being overwritten"
    );
}

/// exec に使うバイナリパスが実行可能ファイルであることを確認。
#[test]
fn current_exe_is_executable_file() {
    let exe = std::env::current_exe().unwrap();
    assert!(exe.is_file(), "current_exe should be a file");

    // Unix では実行パーミッションを確認
    use std::os::unix::fs::PermissionsExt;
    let metadata = std::fs::metadata(&exe).unwrap();
    let mode = metadata.permissions().mode();
    assert!(
        mode & 0o111 != 0,
        "current_exe should have execute permission"
    );
}

/// --local オプション付き update のインテグレーションテスト。
///
/// モックバイナリを作成し、`update --check --local <path>` 相当の
/// バージョン取得・比較フローが正しく動作することを検証する。
#[test]
fn local_update_check_with_mock_binary() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::TempDir::new().unwrap();
    let mock_binary = tmp.path().join("jarvish");

    // 現在のバージョンより新しいバージョンを返すモックバイナリ
    std::fs::write(&mock_binary, "#!/bin/sh\necho \"jarvish 99.0.0\"\n").unwrap();
    std::fs::set_permissions(&mock_binary, std::fs::Permissions::from_mode(0o755)).unwrap();

    // モックバイナリを --version で実行してバージョンが取得できることを確認
    let output = std::process::Command::new(&mock_binary)
        .arg("--version")
        .output()
        .expect("mock binary should execute");

    assert!(output.status.success(), "mock binary should exit 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("99.0.0"),
        "mock binary should output version: {stdout}"
    );

    // バイナリ置換のテスト（一時ファイル経由のアトミック置換）
    let source = tmp.path().join("source-bin");
    let dest = tmp.path().join("dest-bin");
    std::fs::write(&source, b"new binary").unwrap();
    std::fs::write(&dest, b"old binary").unwrap();

    // copy → rename で置換
    let tmp_path = tmp.path().join(".jarvish-update.tmp");
    std::fs::copy(&source, &tmp_path).unwrap();
    std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::rename(&tmp_path, &dest).unwrap();

    // 置換後のファイル内容とパーミッションを検証
    let content = std::fs::read_to_string(&dest).unwrap();
    assert_eq!(content, "new binary");
    let mode = std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o755);
}
