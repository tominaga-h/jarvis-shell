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

// ── Phase 4.3: source ビルトインのスクリプト実行統合 ──
//
// `source <path>` は拡張子で分岐する: `.toml`（大文字小文字を区別しない）は
// 既存の config reload、それ以外（拡張子なし含む）は rc スクリプトとして
// 実行される（分類器バイパス・行番号付きエラー・continue-on-error・
// ネスト深さ上限8・exit 伝播は rc.jsh と同一の意味論）。

/// (a) `--rcfile` 経由で読み込んだトップレベルスクリプトが
/// `source other.jsh` で別のスクリプトをネストして実行し、その中で
/// 登録された alias が `-c` のコマンドから見えること
/// （nested script execution のエンドツーエンド確認）。
#[test]
#[serial]
fn source_builtin_runs_nested_script_and_alias_is_visible() {
    let tmpdir = tempfile::tempdir().unwrap();
    let home = tmpdir.path();
    let other_path = home.join("other.jsh");
    std::fs::write(
        &other_path,
        "alias nested_greet='echo hello-from-nested-script'\n",
    )
    .unwrap();

    let rc_path = home.join("top_rc.jsh");
    std::fs::write(&rc_path, format!("source {}\n", other_path.display())).unwrap();

    let exe = env!("CARGO_BIN_EXE_jarvish");
    let output = std::process::Command::new(exe)
        .env("HOME", home)
        .args(["--rcfile", rc_path.to_str().unwrap(), "-c", "nested_greet"])
        .output()
        .expect("failed to spawn jarvish");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("hello-from-nested-script"),
        "alias registered by the nested `source`d script should be usable from -c: stdout={stdout}"
    );
}

/// (b) 自己 source（`self_source.jsh` が自分自身を `source` する）は
/// 無限ループに陥らず、ネスト深さ上限（8）に達した時点で
/// "source nesting too deep" エラーを出して速やかに停止すること。
/// プロセスがハングしないことを `wait_timeout` 相当の実装
/// （出力取得付き spawn + 明示的な待機）で保証する。
#[test]
#[serial]
fn self_sourcing_script_terminates_quickly_with_depth_error() {
    let tmpdir = tempfile::tempdir().unwrap();
    let home = tmpdir.path();
    let script_path = home.join("self_source.jsh");
    // 自分自身を source する1行だけのスクリプト。
    std::fs::write(&script_path, format!("source {}\n", script_path.display())).unwrap();

    let exe = env!("CARGO_BIN_EXE_jarvish");
    let child = std::process::Command::new(exe)
        .env("HOME", home)
        .args([
            "--rcfile",
            script_path.to_str().unwrap(),
            "-c",
            "echo after-self-source",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn jarvish");

    // ハングした場合にテストスイート全体を止めないよう、別スレッドで
    // 待機しつつタイムアウトを設ける。
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let output = child.wait_with_output();
        let _ = tx.send(output);
    });
    let output = rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("jarvish did not terminate within 30s — self-sourcing must not hang")
        .expect("failed to collect child output");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("source nesting too deep"),
        "self-sourcing must be stopped with a nesting-too-deep error: stderr={stderr}"
    );
    assert!(
        stdout.contains("after-self-source"),
        "the -c command must still run after the nested source chain is cut off: stdout={stdout}"
    );
}

/// (c) `source <path>.toml`（大文字小文字を問わず）は従来どおり
/// config.toml の再読み込みパスを通り、"Loaded ..." サマリーが出力される
/// こと（Phase 4.3 で拡張子ディスパッチを追加した後も .toml の挙動が
/// 変わっていないことの確認）。
#[test]
#[serial]
fn source_toml_extension_still_reloads_config() {
    let tmpdir = tempfile::tempdir().unwrap();
    let home = tmpdir.path();
    let config_dir = home.join(".config/jarvish");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_path = config_dir.join("custom_config.TOML");
    std::fs::write(
        &config_path,
        "[alias]\nfromtoml = \"echo from-toml-reload\"\n",
    )
    .unwrap();

    let rc_path = home.join("rc_sources_toml.jsh");
    std::fs::write(&rc_path, format!("source {}\n", config_path.display())).unwrap();

    let exe = env!("CARGO_BIN_EXE_jarvish");
    let output = std::process::Command::new(exe)
        .env("HOME", home)
        .args(["--rcfile", rc_path.to_str().unwrap(), "-c", "fromtoml"])
        .output()
        .expect("failed to spawn jarvish");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Loaded"),
        "a .toml source (even uppercase extension) must go through the config-reload \
         summary path: stdout={stdout}"
    );
    assert!(
        stdout.contains("from-toml-reload"),
        "the alias defined in the reloaded .toml must be usable from -c: stdout={stdout}"
    );
}

/// (d) 存在しないファイルを `source` すると exit code 1 になり、
/// "no such file" を含むエラーが stderr に出ること。
#[test]
#[serial]
fn source_missing_file_errors_with_no_such_file() {
    let tmpdir = tempfile::tempdir().unwrap();
    let home = tmpdir.path();
    let missing = home.join("does-not-exist.jsh");
    assert!(!missing.exists());

    let rc_path = home.join("rc_sources_missing.jsh");
    std::fs::write(&rc_path, format!("source {}\n", missing.display())).unwrap();

    let exe = env!("CARGO_BIN_EXE_jarvish");
    let output = std::process::Command::new(exe)
        .env("HOME", home)
        .args(["--rcfile", rc_path.to_str().unwrap(), "-c", "true"])
        .output()
        .expect("failed to spawn jarvish");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no such file"),
        "sourcing a missing file must report \"no such file\": stderr={stderr}"
    );
    // rc.jsh の continue-on-error により、このエラー行の後で
    // "command exited with status 1" のサマリーも rc_sources_missing.jsh
    // 側から報告される（source 自体が exit code 1 を返すため）。
    assert!(
        stderr.contains("rc_sources_missing.jsh:1"),
        "the failing `source` line inside the rc script should be reported with its \
         line number: stderr={stderr}"
    );
}

/// (e) スクリプトの途中行が失敗しても、そのスクリプトを `source` した
/// 側でも後続行が実行されること（ネストした source 経由の
/// continue-on-error を、通常の rc.jsh 直接実行とは別に確認する）。
#[test]
#[serial]
fn source_script_with_failing_middle_line_still_runs_later_lines() {
    let tmpdir = tempfile::tempdir().unwrap();
    let home = tmpdir.path();
    let inner_path = home.join("inner_with_bad_line.jsh");
    std::fs::write(
        &inner_path,
        "alias before_bad='echo before-bad-line'\n\
         this_command_does_not_exist_zzz\n\
         alias after_bad='echo after-bad-line-in-nested-script'\n",
    )
    .unwrap();

    let rc_path = home.join("outer_rc.jsh");
    std::fs::write(&rc_path, format!("source {}\n", inner_path.display())).unwrap();

    // `-c` の本体は改行区切りで複数コマンドを実行できる（`Shell::run_command`
    // が `command.lines()` でループする）。`;` はこのシェルではコマンド区切り
    // として解釈されない（外部コマンドへの1トークンとして渡ってしまう）ため、
    // 改行区切りを使う。
    let exe = env!("CARGO_BIN_EXE_jarvish");
    let output = std::process::Command::new(exe)
        .env("HOME", home)
        .args([
            "--rcfile",
            rc_path.to_str().unwrap(),
            "-c",
            "before_bad\nafter_bad",
        ])
        .output()
        .expect("failed to spawn jarvish");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stdout.contains("before-bad-line"),
        "the alias registered before the failing line must take effect: stdout={stdout}"
    );
    assert!(
        stdout.contains("after-bad-line-in-nested-script"),
        "the alias registered after the failing line inside the nested script must \
         still take effect (continue-on-error survives across the source boundary): \
         stdout={stdout}"
    );
    assert!(
        stderr.contains("inner_with_bad_line.jsh:2"),
        "the failing line inside the nested script should be reported with the \
         nested script's own display name and line number: stderr={stderr}"
    );
}

// ── Fix A: --rcfile / source のファイル安全性ガード ─────────────────
//
// A1: FIFO・巨大ファイル・ディレクトリを --rcfile / source に渡した場合、
//     ハング・OOM・分かりにくいエラーにならず、行番号付きの明確なエラーで
//     即座に継続すること。
// A2: デフォルト rc.jsh のブートストラップ（ensure_default_rc）が
//     ダングリングシンボリックリンク・シンボリックリンクされた親ディレクトリを
//     突破口にした書き込みを拒否すること。

/// (f) `--rcfile` にディレクトリを渡すと "is a directory" エラーを報告し、
/// `-c` のコマンドは実行を継続すること。
#[test]
#[serial]
fn rcfile_directory_reports_is_a_directory_and_continues() {
    let tmpdir = tempfile::tempdir().unwrap();
    let home = tmpdir.path();
    let dir_as_rcfile = home.join("a_directory");
    std::fs::create_dir_all(&dir_as_rcfile).unwrap();

    let exe = env!("CARGO_BIN_EXE_jarvish");
    let output = std::process::Command::new(exe)
        .env("HOME", home)
        .args([
            "--rcfile",
            dir_as_rcfile.to_str().unwrap(),
            "-c",
            "echo still-alive",
        ])
        .output()
        .expect("failed to spawn jarvish");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("is a directory"),
        "a directory passed as --rcfile must be reported as such: stderr={stderr}"
    );
    assert!(
        stdout.contains("still-alive"),
        "the -c command must still run after the rcfile guard rejects the directory: \
         stdout={stdout}"
    );
}

/// (g) `--rcfile` に 1MiB を超えるファイルを渡すと "too large" エラーを
/// 報告し、`-c` のコマンドは実行を継続すること（OOM ガード）。
#[test]
#[serial]
fn rcfile_oversized_file_reports_too_large_and_continues() {
    let tmpdir = tempfile::tempdir().unwrap();
    let home = tmpdir.path();
    let huge_rc = home.join("huge.jsh");
    // 1 MiB + 1 バイトのファイル（内容は全てコメント文字で埋める—— パース
    // されるかどうかの前にサイズガードで弾かれるはずなので内容は無関係）。
    let oversized = vec![b'#'; 1024 * 1024 + 1];
    std::fs::write(&huge_rc, &oversized).unwrap();

    let exe = env!("CARGO_BIN_EXE_jarvish");
    let output = std::process::Command::new(exe)
        .env("HOME", home)
        .args([
            "--rcfile",
            huge_rc.to_str().unwrap(),
            "-c",
            "echo still-alive",
        ])
        .output()
        .expect("failed to spawn jarvish");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("too large"),
        "an oversized --rcfile must be reported as too large: stderr={stderr}"
    );
    assert!(
        stdout.contains("still-alive"),
        "the -c command must still run after the rcfile guard rejects the oversized file: \
         stdout={stdout}"
    );
}

/// (h) `--rcfile` に FIFO（名前付きパイプ）を渡しても、ライタが現れるのを
/// 待ってハングすることなく、"not a regular file" エラーで即座に継続する
/// こと（A1 の中核リグレッション: FIFO は決して `open` しない）。
///
/// タイムアウトを設けて、万一ハングする実装に戻ってもテストスイート全体を
/// 止めないようにする。
#[test]
#[serial]
#[cfg(unix)]
fn rcfile_fifo_does_not_hang_and_reports_not_a_regular_file() {
    let tmpdir = tempfile::tempdir().unwrap();
    let home = tmpdir.path();
    let fifo_path = home.join("a_fifo.jsh");
    nix::unistd::mkfifo(&fifo_path, nix::sys::stat::Mode::S_IRWXU)
        .expect("failed to create test FIFO");

    let exe = env!("CARGO_BIN_EXE_jarvish");
    let child = std::process::Command::new(exe)
        .env("HOME", home)
        .args([
            "--rcfile",
            fifo_path.to_str().unwrap(),
            "-c",
            "echo still-alive",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn jarvish");

    // このテストの本来の目的は「ハングしない」ことの証明そのもの ——
    // 別スレッドで待ちつつタイムアウトを設け、万一リグレッションで
    // ハングする実装に戻ってもテストスイート全体を止めない。
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let output = child.wait_with_output();
        let _ = tx.send(output);
    });
    let output = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect(
            "jarvish did not terminate within 10s — a FIFO passed as --rcfile must not \
             block shell startup (A1 regression)",
        )
        .expect("failed to collect child output");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("not a regular file"),
        "a FIFO passed as --rcfile must be reported as not a regular file: stderr={stderr}"
    );
    assert!(
        stdout.contains("still-alive"),
        "the -c command must still run after the rcfile guard rejects the FIFO: stdout={stdout}"
    );
}

/// (i) `source` ビルトインで FIFO を指定しても、ハングせずエラー行を報告し
/// 後続処理を継続すること（`dispatch_source` → `run_rc_script_sync` →
/// `read_rc_file_guarded` の経路でも A1 の保護が効くことの確認、`--rcfile`
/// 経路とは独立に検証する）。
#[test]
#[serial]
#[cfg(unix)]
fn source_fifo_does_not_hang_and_reports_error_line() {
    let tmpdir = tempfile::tempdir().unwrap();
    let home = tmpdir.path();
    let fifo_path = home.join("sourced_fifo");
    nix::unistd::mkfifo(&fifo_path, nix::sys::stat::Mode::S_IRWXU)
        .expect("failed to create test FIFO");

    let rc_path = home.join("rc_sources_fifo.jsh");
    std::fs::write(
        &rc_path,
        format!(
            "source {}\nalias after_fifo='echo after-fifo-source'\n",
            fifo_path.display()
        ),
    )
    .unwrap();

    let exe = env!("CARGO_BIN_EXE_jarvish");
    let child = std::process::Command::new(exe)
        .env("HOME", home)
        .args(["--rcfile", rc_path.to_str().unwrap(), "-c", "after_fifo"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn jarvish");

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let output = child.wait_with_output();
        let _ = tx.send(output);
    });
    let output = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect(
            "jarvish did not terminate within 10s — `source`ing a FIFO must not block \
             the shell (A1 regression)",
        )
        .expect("failed to collect child output");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("not a regular file"),
        "sourcing a FIFO must report a not-a-regular-file error line: stderr={stderr}"
    );
    assert!(
        stderr.contains("rc_sources_fifo.jsh:1"),
        "the failing `source` line should be reported with its line number: stderr={stderr}"
    );
    assert!(
        stdout.contains("after-fifo-source"),
        "the alias registered after the failing `source` line must still take effect \
         (continue-on-error): stdout={stdout}"
    );
}

// (j)/(k) デフォルト rc.jsh パス（ダングリングシンボリックリンク防御・
// 既存ファイルのバイト不変性）は `ensure_default_rc` 経由でしか到達
// しない。この関数は `Shell::run()`（対話モード）からのみ呼ばれ、
// `run_command`（`-c`）は `rc_options.rcfile.is_some()` の場合のみ
// rc を読み込む（`ResolvedRc::Explicit` は自動生成しない —— 本ファイル
// 冒頭のドキュメント参照）ため、`-c` 経由の実バイナリ spawn では
// `ensure_default_rc` に到達できない。対話モードは reedline が実端末を
// 要求するためテストランナーから素朴に spawn できない（同上）。
// したがってこの2ケース（ダングリングシンボリックリンクの拒否・既存
// ファイルの不変性）は `src/shell/rc.rs` 側の `ensure_default_rc` 直接
// 呼び出しユニットテスト
// （`ensure_default_rc_dangling_symlink_is_not_followed`,
// `ensure_default_rc_refuses_symlinked_parent_dir`,
// `ensure_default_rc_never_overwrites_existing_file`,
// `ensure_default_rc_is_idempotent_create_once`）が担う —— 対象関数を
// `Shell` の構築なしに直接呼べるため、こちらの方が対話モードを模擬する
// より確実な検証になる。

/// stdin を消費させず即座に落ちないためのヘルパー（将来 PTY 駆動テストを
/// 足す場合の下地）。現状は未使用だが、対話モードの child プロセスへ
/// 書き込む際の参考として残す。
#[allow(dead_code)]
fn write_and_close_stdin(child: &mut std::process::Child, data: &str) {
    if let Some(stdin) = child.stdin.as_mut() {
        let _ = stdin.write_all(data.as_bytes());
    }
}

// ── Fix B: 実行器のセマンティクス（レビュー確認済み4件 + 方針決定1件）──
//
// B1: rc/source スクリプト内で早い行に定義した alias が、同じスクリプトの
//     後の行から使えること（`run_rc_line` に `handle_input` と同じ
//     エイリアス展開を追加）。
// B2: `--rcfile` スクリプト内 / `-c` 引数内の `restart` が `-c` モードでも
//     実際に exec() による再起動を行うこと（サイレントに死なない）。
// B3: `exit N`（N != 0）はスクリプトの「失敗行」として
//     `command exited with status N` を出力してはならない。
// B4: `foo.toml` という名前の**ディレクトリ**を source すると
//     "is a directory" エラーになり、reload_config には到達しないこと。
// B5: rc/source 行は Black Box（history.db）に記録されない
//     （DESIGN CONTRACT: これは意図的な仕様であり、バグではない）。

/// (B1-a) `--rcfile` スクリプト内の早い行で定義した alias を、
/// 同じスクリプトの後の行から呼び出せること（rc.jsh 直接実行のケース）。
#[test]
#[serial]
fn alias_defined_earlier_in_rcfile_is_usable_on_a_later_line_of_the_same_script() {
    let tmpdir = tempfile::tempdir().unwrap();
    let home = tmpdir.path();
    let rc_path = home.join("rc_alias_reuse.jsh");
    std::fs::write(
        &rc_path,
        "alias gs='echo alias-defined-earlier-works'\n\
         gs\n",
    )
    .unwrap();

    let exe = env!("CARGO_BIN_EXE_jarvish");
    let output = std::process::Command::new(exe)
        .env("HOME", home)
        .args(["--rcfile", rc_path.to_str().unwrap(), "-c", "true"])
        .output()
        .expect("failed to spawn jarvish");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("alias-defined-earlier-works"),
        "an alias defined on an earlier line of the SAME rc script must be usable on a \
         later line (Fix B1): stdout={stdout} stderr={stderr}"
    );
    assert!(
        !stderr.contains("command not found") && !stderr.contains("command exited"),
        "the later line must not fail as \"command not found\": stderr={stderr}"
    );
}

/// (B1-b) 同じ確認を `source` 経由のスクリプトで行う
/// （`--rcfile` 本体からネストして `source` した先のスクリプト内で、
/// 早い行の alias を後の行から使えること）。
#[test]
#[serial]
fn alias_defined_earlier_in_sourced_script_is_usable_on_a_later_line() {
    let tmpdir = tempfile::tempdir().unwrap();
    let home = tmpdir.path();
    let sourced_path = home.join("sourced_alias_reuse.jsh");
    std::fs::write(
        &sourced_path,
        "alias gs='echo alias-in-sourced-script-works'\n\
         gs\n",
    )
    .unwrap();

    let rc_path = home.join("top_rc.jsh");
    std::fs::write(&rc_path, format!("source {}\n", sourced_path.display())).unwrap();

    let exe = env!("CARGO_BIN_EXE_jarvish");
    let output = std::process::Command::new(exe)
        .env("HOME", home)
        .args(["--rcfile", rc_path.to_str().unwrap(), "-c", "true"])
        .output()
        .expect("failed to spawn jarvish");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("alias-in-sourced-script-works"),
        "an alias defined earlier in a `source`d script must be usable on a later line of \
         that same script (Fix B1): stdout={stdout}"
    );
}

/// (B1-c) alias 定義がスクリプト境界をまたいで双方向に見えること:
/// rc.jsh で定義した alias が後で `source` した別スクリプトの行から使え、
/// かつその `source` されたスクリプトで定義した alias が rc.jsh 側の
/// 後続行（source の後）からも使えること。
#[test]
#[serial]
fn alias_is_usable_across_source_boundary_in_both_directions() {
    let tmpdir = tempfile::tempdir().unwrap();
    let home = tmpdir.path();
    let sourced_path = home.join("nested_defines_alias.jsh");
    std::fs::write(
        &sourced_path,
        "rc_alias\n\
         alias nested_alias='echo nested-alias-works'\n",
    )
    .unwrap();

    let rc_path = home.join("top_rc.jsh");
    std::fs::write(
        &rc_path,
        format!(
            "alias rc_alias='echo rc-alias-works'\n\
             source {}\n\
             nested_alias\n",
            sourced_path.display()
        ),
    )
    .unwrap();

    let exe = env!("CARGO_BIN_EXE_jarvish");
    let output = std::process::Command::new(exe)
        .env("HOME", home)
        .args(["--rcfile", rc_path.to_str().unwrap(), "-c", "true"])
        .output()
        .expect("failed to spawn jarvish");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("rc-alias-works"),
        "an alias defined in rc.jsh must be usable from a LATER `source`d script's line: \
         stdout={stdout}"
    );
    assert!(
        stdout.contains("nested-alias-works"),
        "an alias defined inside the `source`d script must be usable from rc.jsh's line \
         after the `source` call: stdout={stdout}"
    );
}

/// (B2) `--rcfile` スクリプト内の `restart` が `-c` モードでもサイレントに
/// 死なず、実際に exec() による再起動を行うこと。
///
/// 無限再起動ループを避けるため、rc スクリプトは自分自身を `rm` した
/// **直後**に `restart` する。1回目の起動: `rm` で rcfile 自体を消してから
/// `restart` → `exec_restart()` が同じ引数（同じ `--rcfile <path>`）で
/// 自己 exec する。2回目の起動: rcfile が既に存在しないため
/// `jarvish: rcfile not found: ...` を出して rc 読み込みをスキップし、
/// `-c` のコマンドを普通に実行して終了する —— したがって
/// 「`-c` のコマンドの出力が観測できる」こと自体が、`restart` が
/// 単に print-and-die したのではなく実際に exec() されたことの証拠になる
/// （修正前は "Restarting jarvish..." の直後にプロセスが終了し、
/// 2回目の起動が一切発生しないため `-c` の出力は絶対に現れない）。
#[test]
#[serial]
fn restart_inside_rcfile_in_dash_c_mode_actually_re_execs_instead_of_dying_silently() {
    let tmpdir = tempfile::tempdir().unwrap();
    let home = tmpdir.path();
    let rc_path = home.join("rc_with_restart.jsh");
    std::fs::write(&rc_path, format!("rm {}\nrestart\n", rc_path.display())).unwrap();

    let exe = env!("CARGO_BIN_EXE_jarvish");
    let child = std::process::Command::new(exe)
        .env("HOME", home)
        .args([
            "--rcfile",
            rc_path.to_str().unwrap(),
            "-c",
            "echo restart-actually-happened",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn jarvish");

    // exec() による自己再起動を挟むため、通常の単発実行より僅かに時間が
    // かかりうる。万一 fix 前の状態（サイレント終了）や予期しないハングに
    // 戻っても、テストスイート全体を止めないようタイムアウトで有界にする。
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let output = child.wait_with_output();
        let _ = tx.send(output);
    });
    let output = rx
        .recv_timeout(std::time::Duration::from_secs(20))
        .expect("jarvish did not terminate within 20s after `restart` inside --rcfile in -c mode")
        .expect("failed to collect child output");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stdout.contains("restart-actually-happened"),
        "the -c command must run on the re-exec'd process, proving `restart` inside a \
         --rcfile script in -c mode actually re-execs instead of silently dying \
         (Fix B2): stdout={stdout} stderr={stderr}"
    );
    // 1回目の起動で必ず "Restarting jarvish..." が出ているはず
    // （restart ビルトイン自体の既存メッセージ、B2 で変更していない）。
    assert!(
        stderr.contains("Restarting jarvish") || stdout.contains("Restarting jarvish"),
        "the restart builtin's own message should still be printed on the first launch: \
         stdout={stdout} stderr={stderr}"
    );
    assert!(
        output.status.success(),
        "the re-exec'd process must exit with the -c command's own (successful) status: \
         status={:?}",
        output.status
    );
}

/// (B3) `exit 3` を含むスクリプト行は、それ自体が「失敗したコマンド」
/// として `jarvish: <file>:<lineno>: command exited with status 3` の
/// 失敗プレフィックスを stderr に出してはならない（exit は意図的な
/// アクションであり、失敗したコマンドではない）。
/// 一方で終了コード 3 自体はプロセスの終了コードとして保持されること。
#[test]
#[serial]
fn exit_with_nonzero_code_does_not_print_spurious_failure_prefix() {
    let tmpdir = tempfile::tempdir().unwrap();
    let home = tmpdir.path();
    let rc_path = home.join("rc_with_exit_code.jsh");
    std::fs::write(&rc_path, "exit 3\n").unwrap();

    let exe = env!("CARGO_BIN_EXE_jarvish");
    let output = std::process::Command::new(exe)
        .env("HOME", home)
        .args([
            "--rcfile",
            rc_path.to_str().unwrap(),
            "-c",
            "echo should-not-run",
        ])
        .output()
        .expect("failed to spawn jarvish");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !stderr.contains("command exited with status"),
        "`exit 3` must NOT be reported as a failing command (Fix B3): stderr={stderr}"
    );
    assert!(
        !stdout.contains("should-not-run"),
        "the -c command must not run after the rc script's `exit`: stdout={stdout}"
    );
    assert_eq!(
        output.status.code(),
        Some(3),
        "the exit code from `exit 3` inside the rc script must be preserved as the \
         process's own exit code: status={:?}",
        output.status
    );
}

/// (B4) `foo.toml` という名前の**ディレクトリ**を `source` すると
/// "is a directory" エラーになり、`reload_config`（config.toml
/// 再読み込みパス）には到達しないこと。到達していれば `JarvishConfig`
/// の "Loaded ..." サマリーや `toml`固有のパースエラーメッセージが出る
/// はずだが、それらは一切出ないことも合わせて確認する。
#[test]
#[serial]
fn sourcing_a_directory_named_dot_toml_reports_is_a_directory_and_skips_reload_config() {
    let tmpdir = tempfile::tempdir().unwrap();
    let home = tmpdir.path();
    let dir_as_toml = home.join("looks_like_config.toml");
    std::fs::create_dir_all(&dir_as_toml).unwrap();

    let rc_path = home.join("rc_sources_toml_dir.jsh");
    std::fs::write(
        &rc_path,
        format!(
            "source {}\nalias after_dir='echo after-toml-dir-source'\n",
            dir_as_toml.display()
        ),
    )
    .unwrap();

    let exe = env!("CARGO_BIN_EXE_jarvish");
    let output = std::process::Command::new(exe)
        .env("HOME", home)
        .args(["--rcfile", rc_path.to_str().unwrap(), "-c", "after_dir"])
        .output()
        .expect("failed to spawn jarvish");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("is a directory"),
        "sourcing a directory named *.toml must report \"is a directory\" (Fix B4): \
         stderr={stderr}"
    );
    assert!(
        !stdout.contains("Loaded"),
        "reload_config must NOT be reached (no \"Loaded ...\" summary) when the .toml \
         path is actually a directory: stdout={stdout}"
    );
    assert!(
        stdout.contains("after-toml-dir-source"),
        "continue-on-error must still let the later alias line take effect: stdout={stdout}"
    );
}

/// (B5) rc/source 経由で実行された行は Black Box（history.db）に
/// **記録されない**こと。これは DESIGN CONTRACT（意図的な仕様）であり、
/// スクリプト行は「設定の再生」であって対話的に打ったコマンドではない
/// ため、起動のたびに history をスパムしない（bash の `source` と同じ
/// 方針）。対照として、`-c` の引数自体として渡された行は通常どおり
/// 記録されることも合わせて確認し、「rc 経由は記録されない」ことが
/// 「history 機能そのものが働いていない」わけではないことを示す。
#[test]
#[serial]
fn rc_script_lines_are_not_recorded_to_history_but_dash_c_lines_are() {
    let tmpdir = tempfile::tempdir().unwrap();
    let home = tmpdir.path();
    let rc_path = home.join("rc_defines_marker_alias.jsh");
    std::fs::write(&rc_path, "alias rc_marker_cmd='echo from-rc-marker'\n").unwrap();

    let exe = env!("CARGO_BIN_EXE_jarvish");
    // rc.jsh 側では alias 定義だけ行い、実際にそれを「実行」するのは
    // -c 側の1行目にする —— これにより、rc.jsh の行自体
    // （alias 定義行）が記録されていないことと、-c 側の行
    // （dash_c_marker_line、対話/非対話コマンド実行の通常経路）が
    // 記録されていることの両方を、同一プロセスの起動で確認できる。
    let output = std::process::Command::new(exe)
        .env("HOME", home)
        .args([
            "--rcfile",
            rc_path.to_str().unwrap(),
            "-c",
            "dash_c_marker_line\nhistory -n 50",
        ])
        .output()
        .expect("failed to spawn jarvish");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // -c の1行目自体（"dash_c_marker_line"、存在しないコマンド）は
    // 通常の -c 実行経路を通るため history に記録されるはず。
    assert!(
        stdout.contains("dash_c_marker_line"),
        "a line passed via -c must be recorded to history (contrast case): \
         stdout={stdout} stderr={stderr}"
    );
    // rc.jsh 側の行（"alias rc_marker_cmd=..."）は rc/source 経由なので
    // 記録されてはならない。
    assert!(
        !stdout.contains("rc_marker_cmd"),
        "a line executed via the rc script must NOT be recorded to history \
         (Fix B5 / DESIGN CONTRACT): stdout={stdout}"
    );
}
