//! rc.jsh（Phase 4）のインテグレーションテスト
//!
//! rc.jsh はシェルの**対話起動時のみ**読み込まれる（`Shell::run()`
//! 内、`[startup].commands` の直前）。対話起動は reedline が実端末
//! （PTY）での `read_line()` を要求するため、`jarvish` バイナリを
//! ここから素朴に spawn して検証することはできない
//! （テストランナーには制御端末がない）。
//!
//! そのため本ファイルでは、実行器の判断ロジック（分類器バイパスの
//! 優先順位、exit 検出、エラープレフィックス整形）を
//! `src/shell/rc.rs` 側のユニットテストで純粋関数レベルに分解して
//! 網羅している（`parse_rc_lines` のテーブル駆動テスト、
//! `TEMPLATE` がゼロ実行行になることの確認、`rc_path`/`ensure_default_rc`
//! の HOME 依存・作成一回性のテスト、`is_goodbye_pattern`/`try_builtin`/
//! `execute` の各判断分岐の直接呼び出しテスト）。
//!
//! 本ファイルには、バイナリを実際に spawn して rc.jsh の存在を
//! エンドツーエンドで確認する `#[ignore]` テストを1本だけ用意している。
//! これは Phase 4.2 で `--rcfile` + `-c` の組み合わせ（非対話でも
//! rc スクリプトを読み込む唯一のセーム）が入った時点で `#[ignore]` を
//! 外して有効化する（TODO）。それまでは `cargo test -- --ignored` でのみ
//! 手動実行できる。

use std::io::Write;

/// TODO(Phase 4.2): `--rcfile <path> -c "<command>"` が実装されたら
/// `#[ignore]` を外す。現状は `-c` 単体では rc.jsh を読み込まない
/// （対話モード限定）ため、このテストは意図した形では動作しない。
///
/// 有効化後の想定コントラクト:
/// 1. 隔離 HOME 配下に rc.jsh を書く（alias 登録 + comment + 失敗行）
/// 2. `jarvish --rcfile <path> -c "alias-that-was-registered"` を実行
/// 3. rc.jsh の alias 登録が反映されていること、コメント行が無視される
///    こと、失敗行が exit code 1 とその行番号付きエラーを吐きつつ後続の
///    行の実行を妨げないことを stdout/stderr から確認する
#[test]
#[ignore = "Phase 4.2 で --rcfile + -c が実装されるまで駆動不可（対話専用の rc.jsh 読み込みを -c 経路から確認する手段がまだ無い）"]
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

/// 対話モードを経由しない範囲での回帰チェック: `-c` 単体（`--rcfile` なし）
/// では rc.jsh がロードされないこと（設計契約: 「rc runs BEFORE
/// [startup].commands, interactive mode only — except an EXPLICIT
/// --rcfile also loads in -c mode」）。デフォルトの rc.jsh がホームに
/// 存在しても、-c 単体の実行結果には一切影響しないことを確認する。
#[test]
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

/// stdin を消費させず即座に落ちないためのヘルパー（将来 PTY 駆動テストを
/// 足す場合の下地）。現状は未使用だが、Phase 4.2 実装時に対話モードの
/// child プロセスへ書き込む際の参考として残す。
#[allow(dead_code)]
fn write_and_close_stdin(child: &mut std::process::Child, data: &str) {
    if let Some(stdin) = child.stdin.as_mut() {
        let _ = stdin.write_all(data.as_bytes());
    }
}
