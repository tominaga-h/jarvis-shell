//! rc.jsh（Phase 4）のインテグレーションテスト
//!
//! rc.jsh はデフォルトでは対話起動時のみ読み込まれる（`Shell::run()`
//! 内、`[startup].commands` の直前）が、Phase 4.2 で追加された
//! `--rcfile` を明示指定した場合に限り `-c`（非対話）実行でも読み込まれる
//! （唯一のセーム）。対話起動そのものは reedline が実端末（PTY）での
//! `read_line()` を要求するため、`jarvish` バイナリをここから素朴に
//! spawn して対話モードを検証することはできない（テストランナーには
//! 制御端末がない）。
//!
//! そのため本ファイルは:
//! - `--rcfile` + `-c` のセームを実バイナリ spawn
//!   （`env!("CARGO_BIN_EXE_jarvish")`）で end-to-end に検証する
//!   （分類器バイパス優先順位、exit 検出、エラープレフィックス整形の
//!   純粋関数レベルの網羅は `src/shell/rc.rs` 側のユニットテストが担う）
//! - `--no-rc` / デフォルト `-c` が rc を一切読み込まない回帰も
//!   同じ手法で確認する
//!
//! 各テストは隔離した一時 HOME を使うが、環境変数 `HOME` を子プロセスに
//! 渡すだけで自プロセスの `env::set_var` は呼ばないため本来並行実行も
//! 安全ではあるが、他の統合テスト（`tests/self_update.rs`）が同じ
//! `#[serial]` 規約に従っているため、規約を揃えて `#[serial]` を付与する。

use std::io::Write;

use serial_test::serial;

/// (a) `--rcfile <path> -c "<command>"`: rc スクリプトが `-c` のコマンド
/// 実行前に読み込まれ、alias 登録が反映されること、コメント行が無視される
/// こと、失敗行が行番号付きエラーを吐きつつ後続行の実行を妨げないこと
/// （continue-on-error）を確認する。
#[test]
#[serial]
fn rcfile_flag_with_dash_c_loads_and_executes_rc_script() {
    let tmpdir = tempfile::tempdir().unwrap();
    let home = tmpdir.path();
    let rc_path = home.join("custom_rc.jsh");
    std::fs::write(
        &rc_path,
        "# a comment, must be skipped\n\
         alias greet='echo hello-from-rc'\n\
         export RC_SCRIPT_RAN=1\n\
         complete -c mycmd -s v -l verbose\n\
         this_command_does_not_exist_zzz\n",
    )
    .unwrap();

    let exe = env!("CARGO_BIN_EXE_jarvish");
    let output = std::process::Command::new(exe)
        .env("HOME", home)
        .args(["--rcfile", rc_path.to_str().unwrap(), "-c", "greet"])
        .output()
        .expect("failed to spawn jarvish");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stdout.contains("hello-from-rc"),
        "alias registered by rc.jsh should be usable in the -c command: stdout={stdout}"
    );
    assert!(
        stderr.contains("custom_rc.jsh:5"),
        "the failing line (line 5) should be reported with its line number: stderr={stderr}"
    );
}

/// (b) 同上の `--rcfile` セームで `complete` ビルトインの登録が反映され、
/// `-c "complete"` の一覧表示に登場すること（rc スクリプト経由で登録した
/// spec がその場の `-c` プロセス内でも生きていることの確認）。
#[test]
#[serial]
fn rcfile_flag_registers_complete_spec_visible_in_dash_c() {
    let tmpdir = tempfile::tempdir().unwrap();
    let home = tmpdir.path();
    let rc_path = home.join("custom_rc.jsh");
    std::fs::write(
        &rc_path,
        "complete -c mycmd -s v -l verbose -d 'Verbose output'\n",
    )
    .unwrap();

    let exe = env!("CARGO_BIN_EXE_jarvish");
    let output = std::process::Command::new(exe)
        .env("HOME", home)
        .args(["--rcfile", rc_path.to_str().unwrap(), "-c", "complete"])
        .output()
        .expect("failed to spawn jarvish");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("mycmd"),
        "complete spec registered by rc.jsh should be listed by `complete`: stdout={stdout}"
    );
    assert!(
        stdout.contains("verbose"),
        "the registered long flag should appear in the listing: stdout={stdout}"
    );
}

/// (c) `--no-rc` はデフォルトパスに rc.jsh が存在していても読み込まず、
/// かつテンプレートの自動生成も行わないこと。
#[test]
#[serial]
fn no_rc_flag_skips_existing_default_rc_and_never_bootstraps() {
    let tmpdir = tempfile::tempdir().unwrap();
    let home = tmpdir.path();
    let rc_dir = home.join(".config/jarvish");
    std::fs::create_dir_all(&rc_dir).unwrap();
    let default_rc = rc_dir.join("rc.jsh");
    std::fs::write(&default_rc, "alias should_not_leak='echo LEAKED'\n").unwrap();
    let original_contents = std::fs::read_to_string(&default_rc).unwrap();

    let exe = env!("CARGO_BIN_EXE_jarvish");
    let output = std::process::Command::new(exe)
        .env("HOME", home)
        .args(["--no-rc", "-c", "should_not_leak"])
        .output()
        .expect("failed to spawn jarvish");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("LEAKED"),
        "--no-rc must not load the default rc.jsh even when -c is also given: stdout={stdout}"
    );

    // 既存ファイルは変更されず（上書き/テンプレ化されず）そのまま残る
    let contents_after = std::fs::read_to_string(&default_rc).unwrap();
    assert_eq!(
        contents_after, original_contents,
        "--no-rc must not touch (bootstrap/overwrite) the existing default rc.jsh"
    );
}

/// (c-2) `--no-rc` はデフォルトパスに rc.jsh が存在しない場合でも
/// テンプレートを自動生成しないこと。
#[test]
#[serial]
fn no_rc_flag_does_not_bootstrap_default_rc_when_absent() {
    let tmpdir = tempfile::tempdir().unwrap();
    let home = tmpdir.path();

    let exe = env!("CARGO_BIN_EXE_jarvish");
    let status = std::process::Command::new(exe)
        .env("HOME", home)
        .args(["--no-rc", "-c", "true"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("failed to spawn jarvish");
    assert!(status.success());

    let rc_path = home.join(".config/jarvish/rc.jsh");
    assert!(
        !rc_path.exists(),
        "--no-rc must never bootstrap the default rc.jsh template"
    );
}

/// (d) `--rcfile` に存在しないパスを渡した場合、stderr に警告を出しつつ
/// `-c` のコマンドは通常どおり実行され、終了コードは 0 のままであること。
#[test]
#[serial]
fn rcfile_missing_path_warns_but_dash_c_command_still_runs() {
    let tmpdir = tempfile::tempdir().unwrap();
    let home = tmpdir.path();
    let missing_rc = home.join("does-not-exist.jsh");
    assert!(!missing_rc.exists());

    let exe = env!("CARGO_BIN_EXE_jarvish");
    let output = std::process::Command::new(exe)
        .env("HOME", home)
        .args([
            "--rcfile",
            missing_rc.to_str().unwrap(),
            "-c",
            "echo still-ran",
        ])
        .output()
        .expect("failed to spawn jarvish");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("rcfile not found"),
        "missing --rcfile path should warn on stderr: stderr={stderr}"
    );
    assert!(
        stdout.contains("still-ran"),
        "the -c command must still run despite the missing rcfile: stdout={stdout}"
    );
    assert!(
        output.status.success(),
        "exit code must be 0 when the -c command itself succeeds: status={:?}",
        output.status
    );

    // 存在しないパスは自動生成もされない（明示パスは常に auto-generation 対象外）
    assert!(!missing_rc.exists());
}

/// (e) 対話モードを経由しない範囲での回帰チェック: `-c` 単体
/// （`--rcfile` なし、`--no-rc` もなし）では rc.jsh がロードされないこと
/// （設計契約: 「rc runs BEFORE [startup].commands, interactive mode only —
/// except an EXPLICIT --rcfile also loads in -c mode」）。デフォルトの
/// rc.jsh がホームに存在しても、-c 単体の実行結果には一切影響しないことを
/// 確認する。
#[test]
#[serial]
fn plain_dash_c_does_not_load_default_rc_jsh() {
    let tmpdir = tempfile::tempdir().unwrap();
    let home = tmpdir.path();
    let rc_dir = home.join(".config/jarvish");
    std::fs::create_dir_all(&rc_dir).unwrap();
    std::fs::write(
        rc_dir.join("rc.jsh"),
        "alias should_not_leak='echo LEAKED'\n",
    )
    .unwrap();

    let exe = env!("CARGO_BIN_EXE_jarvish");
    let output = std::process::Command::new(exe)
        .env("HOME", home)
        .args(["-c", "should_not_leak"])
        .output()
        .expect("failed to spawn jarvish");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("LEAKED"),
        "plain -c must not load the default rc.jsh: stdout={stdout}"
    );
}

/// `-c` 実行では標準の `.config/jarvish/rc.jsh` テンプレートも
/// 自動生成されないこと（自動生成はデフォルトパスの対話起動時のみ）。
#[test]
#[serial]
fn plain_dash_c_does_not_bootstrap_default_rc_jsh() {
    let tmpdir = tempfile::tempdir().unwrap();
    let home = tmpdir.path();

    let exe = env!("CARGO_BIN_EXE_jarvish");
    let status = std::process::Command::new(exe)
        .env("HOME", home)
        .args(["-c", "true"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("failed to spawn jarvish");
    assert!(status.success());

    let rc_path = home.join(".config/jarvish/rc.jsh");
    assert!(
        !rc_path.exists(),
        "plain -c must not bootstrap the default rc.jsh template"
    );
}

/// (f) rc スクリプト内の失敗行が `jarvish: <display_name>:<lineno>:` の
/// プレフィックスで報告されつつ、その後続にある正常な alias 登録行が
/// きちんと反映されること（continue-on-error のエンドツーエンド確認、
/// (a) を行番号・alias 反映の両面から明示的に再確認する）。
#[test]
#[serial]
fn rc_line_failure_reports_lineno_prefix_and_later_lines_still_take_effect() {
    let tmpdir = tempfile::tempdir().unwrap();
    let home = tmpdir.path();
    let rc_path = home.join("rc_with_bad_line.jsh");
    std::fs::write(
        &rc_path,
        "this_command_does_not_exist_zzz\n\
         alias after_bad='echo after-bad-line-still-works'\n",
    )
    .unwrap();

    let exe = env!("CARGO_BIN_EXE_jarvish");
    let output = std::process::Command::new(exe)
        .env("HOME", home)
        .args(["--rcfile", rc_path.to_str().unwrap(), "-c", "after_bad"])
        .output()
        .expect("failed to spawn jarvish");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("rc_with_bad_line.jsh:1"),
        "the failing first line should be reported with the file display name and line number: \
         stderr={stderr}"
    );
    assert!(
        stdout.contains("after-bad-line-still-works"),
        "the alias registered on the line after the failure must still take effect: \
         stdout={stdout}"
    );
}

/// (g) rc スクリプト内に `exit`（あるいは goodbye 相当）の行があると、
/// `--rcfile` + `-c` 経路では `-c` のコマンドが一切実行されないこと。
#[test]
#[serial]
fn exit_inside_rcfile_aborts_before_running_dash_c_command() {
    let tmpdir = tempfile::tempdir().unwrap();
    let home = tmpdir.path();
    let rc_path = home.join("rc_with_exit.jsh");
    std::fs::write(
        &rc_path,
        "export RC_RAN_UP_TO_EXIT=1\n\
         exit\n\
         alias unreachable='echo should-never-run'\n",
    )
    .unwrap();

    let exe = env!("CARGO_BIN_EXE_jarvish");
    let output = std::process::Command::new(exe)
        .env("HOME", home)
        .args([
            "--rcfile",
            rc_path.to_str().unwrap(),
            "-c",
            "echo dash-c-should-not-run",
        ])
        .output()
        .expect("failed to spawn jarvish");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("dash-c-should-not-run"),
        "exit inside the rc script must abort before the -c command runs: stdout={stdout}"
    );
}

/// stdin を消費させず即座に落ちないためのヘルパー（将来 PTY 駆動テストを
/// 足す場合の下地）。現状は未使用だが、対話モードの child プロセスへ
/// 書き込む際の参考として残す。
#[allow(dead_code)]
fn write_and_close_stdin(child: &mut std::process::Child, data: &str) {
    if let Some(stdin) = child.stdin.as_mut() {
        let _ = stdin.write_all(data.as_bytes());
    }
}
