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
