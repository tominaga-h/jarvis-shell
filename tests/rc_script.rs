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
use std::path::PathBuf;

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

/// (f-2) 分類器バイパス（classifier-bypass）の回帰テスト:
/// AI に自然言語としてルーティングされるはずの行（例:
/// `please explain this error to me` — 先頭トークン `please` は PATH 上に
/// 実在しないバイナリであることを確認済み。macOS には `/usr/bin/what` の
/// ような紛らわしい実在バイナリがあるため、先頭語の選定には注意が必要）
/// が rc スクリプトに書かれていても、`try_shell_builtins` →
/// `try_builtin` → `execute` の純粋なコマンド実行パスに落ちて
/// 「command not found」（exit code 127）として失敗するだけであり、AI
/// 呼び出しは一切発生しないこと。`OPENAI_API_KEY` を明示的に unset した
/// 状態で実行し、（仮に AI 経路に迷い込んだ場合に出るはずの）AI 関連の
/// エラー文言や応答が出力に一切現れないことも合わせて確認する。
#[test]
#[serial]
fn natural_language_line_in_rcfile_is_run_as_command_not_routed_to_ai() {
    let tmpdir = tempfile::tempdir().unwrap();
    let home = tmpdir.path();
    let rc_path = home.join("rc_with_nl_line.jsh");
    std::fs::write(
        &rc_path,
        "please explain this error to me\n\
         alias after_nl='echo after-nl-line-still-works'\n",
    )
    .unwrap();

    let exe = env!("CARGO_BIN_EXE_jarvish");
    let output = std::process::Command::new(exe)
        .env("HOME", home)
        .env_remove("OPENAI_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .args(["--rcfile", rc_path.to_str().unwrap(), "-c", "after_nl"])
        .output()
        .expect("failed to spawn jarvish");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // 分類器を経由していれば自然言語と判定されるはずの行が、rc.jsh 経由では
    // ただの「未知のコマンド」として command not found (exit 127) で
    // 失敗すること。「command not found」自体は spawn_error() →
    // jarvis_talk() が println! で stdout に出す文言（jarvish 全体の
    // 既存挙動）であり、行番号付きの継続サマリー
    // （"jarvish: <file>:<lineno>: command exited with status <code>"）は
    // rc.rs 側が eprintln! で stderr に出す別の文言である —— 両方を
    // それぞれの出力先で確認する。
    assert!(
        stdout.contains("command not found"),
        "a natural-language-looking line must fail as an unknown command, not be routed to AI: \
         stdout={stdout}"
    );
    assert!(
        stderr.contains("rc_with_nl_line.jsh:1: command exited with status 127"),
        "the failing line should be reported with the file:lineno prefix and exit code 127: \
         stderr={stderr}"
    );
    // AI には一切送信されていないことの追加確認: AI クライアントが実際に
    // 呼び出された場合に特有の文言（API リクエスト失敗・`investigate` 応答
    // 等）が出力に混入していないこと。
    //
    // 注意: `OPENAI_API_KEY` という文字列そのものは「AI 呼び出しの証拠」に
    // ならない。API キーを unset した状態で jarvish を起動すると、AI 経路に
    // 迷い込んだかどうかとは無関係に、起動時に必ず
    // `jarvish: warning: AI disabled: OPENAI_API_KEY is not set.` という
    // 警告が stderr に出るためである（CI のクリーン環境で顕在化）。
    // したがって単純な substring 一致ではなく、「AI がキー欠如を理由に
    // *呼び出しに失敗した* こと」を示す文言のみを検査対象とする。
    let ai_invocation_markers = ["AI request failed", "API request", "investigate", "OpenAI"];
    for marker in ai_invocation_markers {
        assert!(
            !stdout.contains(marker) && !stderr.contains(marker),
            "no AI-invocation evidence ({marker:?}) should appear — the line must never reach \
             the AI client: stdout={stdout} stderr={stderr}"
        );
    }
    // continue-on-error により、NL 行の後のエイリアス定義は生きている。
    assert!(
        stdout.contains("after-nl-line-still-works"),
        "the alias registered on the line after the NL-looking line must still take effect: \
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

/// `n_files` 個の連鎖する `source` チェーンを `dir` の下に書き出す。
/// `chain_1.jsh` がトップレベル（`--rcfile` の深さ0）で、
/// `chain_N.jsh`（`N < n_files`）は `alias chain_marker_N=...` を定義した
/// 直後に `chain_{N+1}.jsh` を `source` する（このホップは
/// `dispatch_source` の `next_depth = self.source_depth + 1` を
/// `N`（1始まり）にする）。一番最後のファイル（`chain_{n_files}.jsh`）は
/// 次を `source` せずに止まる。戻り値はトップレベル
/// （`--rcfile` に渡す）`chain_1.jsh` のパス。
///
/// 深さ算術: `--rcfile`（`chain_1.jsh`）は `source_depth == 0` で走る。
/// `chain_K.jsh`（`K < n_files`）から `chain_{K+1}.jsh` への `source` ホップ
/// は `next_depth == K` を要求する（`K <= MAX_SOURCE_DEPTH` なら許可、
/// 超えれば拒否）。したがって `n_files` 個のファイル・`n_files - 1` 回の
/// ホップからなるチェーンがすべて実行されるための条件は
/// `n_files - 1 <= MAX_SOURCE_DEPTH`、すなわち
/// `n_files <= MAX_SOURCE_DEPTH + 1`。
fn write_source_chain(dir: &std::path::Path, n_files: usize) -> PathBuf {
    assert!(n_files >= 1, "chain must have at least 1 file");
    for n in 1..=n_files {
        let path = dir.join(format!("chain_{n}.jsh"));
        let mut content = format!("alias chain_marker_{n}='echo chain-marker-{n}-reached'\n");
        if n < n_files {
            let next = dir.join(format!("chain_{}.jsh", n + 1));
            content.push_str(&format!("source {}\n", next.display()));
        }
        std::fs::write(&path, content).unwrap();
    }
    dir.join("chain_1.jsh")
}

/// (b-2) `MAX_SOURCE_DEPTH`（8）ちょうどの実チェーン: `--rcfile`（深さ0、
/// `chain_1.jsh`）から `source` を8回辿って `chain_9.jsh`（9番目のファイル、
/// 最後の `source` ホップの `next_depth == 8 == MAX_SOURCE_DEPTH`）まで
/// 全行が実行され、一番深いファイルで定義した alias が `-c` から見える
/// こと。単なる整数比較の unit test ではなく、実際に9個の別ファイルを
/// `source` で辿らせて末端の副作用（alias 登録）を観測する。
#[test]
#[serial]
fn source_chain_at_exactly_max_depth_fully_executes_deepest_alias() {
    let tmpdir = tempfile::tempdir().unwrap();
    let home = tmpdir.path();
    let chain_dir = home.join("chain");
    std::fs::create_dir_all(&chain_dir).unwrap();

    // MAX_SOURCE_DEPTH と実チェーンの本数が食い違ったら壊れるように、
    // ハードコードではなく定数由来の値でチェーンを組み立てる。
    // rc.rs の MAX_SOURCE_DEPTH は private のため、README/DESIGN CONTRACT
    // で固定されている値 8 をここでは直接使う（`max_source_depth_is_eight`
    // ユニットテストが定数側の回帰を別途保証する）。
    const MAX_SOURCE_DEPTH: usize = 8;
    // ちょうど限界のチェーン: ファイル数 = MAX_SOURCE_DEPTH + 1（ホップ数
    // = MAX_SOURCE_DEPTH）。
    let n_files = MAX_SOURCE_DEPTH + 1;
    let top = write_source_chain(&chain_dir, n_files);

    let exe = env!("CARGO_BIN_EXE_jarvish");
    let child = std::process::Command::new(exe)
        .env("HOME", home)
        .args([
            "--rcfile",
            top.to_str().unwrap(),
            "-c",
            &format!("chain_marker_{n_files}"),
        ])
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
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("jarvish did not terminate within 30s at the max-depth boundary")
        .expect("failed to collect child output");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !stderr.contains("source nesting too deep"),
        "a chain of exactly MAX_SOURCE_DEPTH source hops must NOT be rejected: stderr={stderr}"
    );
    assert!(
        stdout.contains(&format!("chain-marker-{n_files}-reached")),
        "the alias defined at the deepest file (file #{n_files}, the {MAX_SOURCE_DEPTH}th \
         source hop) of an at-the-limit chain must be visible and runnable from -c: \
         stdout={stdout}"
    );
}

/// (b-3) `MAX_SOURCE_DEPTH` を1超える実チェーン（9ホップ、10ファイル）は、
/// 10番目のファイルに到達する手前（9ホップ目、`next_depth == 9`）で
/// "source nesting too deep" を報告して打ち切ること。加えて、そのエラーが
/// 実際に到達を試みたファイル（10番目のパス）を名指ししていることを
/// 確認する。
#[test]
#[serial]
fn source_chain_one_beyond_max_depth_errors_at_the_right_file() {
    let tmpdir = tempfile::tempdir().unwrap();
    let home = tmpdir.path();
    let chain_dir = home.join("chain");
    std::fs::create_dir_all(&chain_dir).unwrap();

    const MAX_SOURCE_DEPTH: usize = 8;
    // 限界を1つ超えるチェーン: ファイル数 = MAX_SOURCE_DEPTH + 2
    // （ホップ数 = MAX_SOURCE_DEPTH + 1、最後のホップが拒否される）。
    let n_files = MAX_SOURCE_DEPTH + 2;
    let top = write_source_chain(&chain_dir, n_files);
    let unreached_path = chain_dir.join(format!("chain_{n_files}.jsh"));

    let exe = env!("CARGO_BIN_EXE_jarvish");
    let child = std::process::Command::new(exe)
        .env("HOME", home)
        .args(["--rcfile", top.to_str().unwrap(), "-c", "echo chain-done"])
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
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("jarvish did not terminate within 30s one level beyond the max-depth boundary")
        .expect("failed to collect child output");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("source nesting too deep"),
        "a chain one hop beyond MAX_SOURCE_DEPTH must be rejected: stderr={stderr}"
    );
    assert!(
        stderr.contains(unreached_path.to_str().unwrap()),
        "the nesting-too-deep error must name the file it failed to reach (file #{n_files}): \
         stderr={stderr}"
    );
    // マーカーの alias（ファイル1..={MAX_SOURCE_DEPTH}+1、8ホップ目まで）は
    // 登録済みだが、最後のファイルには到達しないためそのマーカーは
    // 定義されない。-c 側は継続する。
    assert!(
        stdout.contains("chain-done"),
        "the -c command must still run after the too-deep chain is cut off: stdout={stdout}"
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

/// (d-2) Fix C5: `.toml` 側の「ファイルが存在しない」エラーはスクリプト側
/// （`source_missing_file_errors_with_no_such_file`、"no such file"）とは
/// **異なる文言**であることを固定する回帰テスト。`.toml` パスは
/// `reload_config` → `JarvishConfig::load_from` → `fs::read_to_string` の
/// 生の I/O エラーをそのまま `jarvish: source: failed to read <path>: ...`
/// として報告する（README/README_JA の「両分岐で文言が異なる」という
/// 記述を裏付ける）。
#[test]
#[serial]
fn source_missing_toml_file_errors_with_failed_to_read_not_no_such_file() {
    let tmpdir = tempfile::tempdir().unwrap();
    let home = tmpdir.path();
    let missing_toml = home.join("does-not-exist.toml");
    assert!(!missing_toml.exists());

    let rc_path = home.join("rc_sources_missing_toml.jsh");
    std::fs::write(&rc_path, format!("source {}\n", missing_toml.display())).unwrap();

    let exe = env!("CARGO_BIN_EXE_jarvish");
    let output = std::process::Command::new(exe)
        .env("HOME", home)
        .args(["--rcfile", rc_path.to_str().unwrap(), "-c", "true"])
        .output()
        .expect("failed to spawn jarvish");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("failed to read"),
        ".toml source on a missing path must report \"failed to read\" (via the config \
         loader's raw I/O error), not the script branch's \"no such file\": stderr={stderr}"
    );
    assert!(
        !stderr.contains("no such file"),
        "the .toml branch's message must NOT match the script branch's wording — they are \
         genuinely different code paths with different text: stderr={stderr}"
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

/// (B5 / 非対話単体実行の履歴非記録) rc/source 経由で実行された行、および
/// `jarvish -c "<command>"`（非対話単体実行）で渡された行は、いずれも
/// Black Box（history.db）に **記録されない** こと。
///
/// これは DESIGN CONTRACT（意図的な仕様）である:
/// - rc スクリプト行は「設定の再生」であって対話的に打ったコマンドでは
///   ないため、起動のたびに history をスパムしない（bash の `source` と
///   同じ方針）。
/// - `-c` 単体実行は `Shell { interactive: false, .. }` となり、
///   `record_history`（`src/shell/input.rs`）の冒頭 `if !self.interactive
///   { return; }` により記録がスキップされる。これは `nvim` 等の外部
///   ツールがファイル glob 展開のために `jarvish -c "vimglob() {...}"`
///   を呼んだ際、その一時コマンドが対話履歴（上下矢印キー補完）に混入
///   するのを防ぐための変更（`interactive` フィールドは `src/shell/mod.rs`、
///   `resolve_interactive` は `src/main.rs` 参照）。
///
/// 旧仕様では「rc.jsh 行は非記録／-c 行は記録される」という対比だったが、
/// 本変更により -c 行も非記録になったため、両方が非記録であることを
/// 検証する形に書き換えた（旧テスト名:
/// `rc_script_lines_are_not_recorded_to_history_but_dash_c_lines_are`）。
///
/// 対話入力（reedline 経由）が記録されることまでは本テストでは検証
/// できない。reedline は実端末（TTY, raw mode の termios）を要求し、
/// `std::process::Command` の `Stdio::piped()` で stdin にコマンドを
/// 流し込む方式では PTY が割り当てられず `Device not configured
/// (os error 6)` で即エラー終了することを実験で確認済み（PTY 未割当の
/// パイプ stdin では reedline の `read_line()` に到達すらしない）。
/// そのため対話経路の記録確認は本テストの対象外とし、「history 機能
/// 自体は生きている」ことの担保は `src/storage/mod.rs` の
/// `record_stores_command_metadata`（`BlackBox::record` のユニット
/// テスト）に委ねる。
#[test]
#[serial]
fn dash_c_lines_are_not_recorded_to_history() {
    let tmpdir = tempfile::tempdir().unwrap();
    let home = tmpdir.path();
    let rc_path = home.join("rc_defines_marker_alias.jsh");
    std::fs::write(&rc_path, "alias rc_marker_cmd='echo from-rc-marker'\n").unwrap();

    let exe = env!("CARGO_BIN_EXE_jarvish");
    // rc.jsh 側では alias 定義だけ行い、実際にそれを「実行」するのは
    // -c 側の1行目にする —— これにより、rc.jsh の行自体
    // （alias 定義行）と、-c 側の行（echo コマンド）の両方が非記録で
    // あることを、同一プロセスの起動で確認できる。
    //
    // -c 側は `echo <marker>` の形にする（他のテストと同じパターン）:
    // 単語一つだけの未知コマンド名（例: "dash_c_marker_line" 単体）は
    // 分類器が InputType::NaturalLanguage と判定して AI にルーティング
    // してしまい、Command 経路（record_history が呼ばれる経路）を
    // 通らないため、確実に InputType::Command になる `echo` 呼び出しに
    // する。history 側の判定マーカーは echo の**出力文字列**とは別の
    // 語（`DASH_C_HISTORY_MARKER`）にし、record が働いていれば
    // `history -n 50` の一覧にコマンド文字列として出現するはずのその
    // 語を素直に contains で検査する（echo 自身の出力と紛れない）。
    let output = std::process::Command::new(exe)
        .env("HOME", home)
        .args([
            "--rcfile",
            rc_path.to_str().unwrap(),
            "-c",
            "echo DASH_C_HISTORY_MARKER\nhistory -n 50",
        ])
        .output()
        .expect("failed to spawn jarvish");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // echo 自身の出力として、マーカー文字列は1回は stdout に現れる。
    assert!(
        stdout.contains("DASH_C_HISTORY_MARKER"),
        "sanity check: the echo command itself must have run and printed the \
         marker: stdout={stdout} stderr={stderr}"
    );
    // history 一覧にコマンド全文（"echo DASH_C_HISTORY_MARKER"）が
    // 記録されていれば、その文字列が stdout 中に2回目以降として出現する
    // はず。新仕様（-c 単体実行は非記録）では、echo の出力による1回の
    // 出現のみで、history 一覧側には出現しない。
    let occurrences = stdout.matches("DASH_C_HISTORY_MARKER").count();
    assert_eq!(
        occurrences, 1,
        "a line passed via -c (non-interactive single execution) must NOT be \
         recorded to history: expected the marker to appear exactly once \
         (from echo's own stdout) but it appeared {occurrences} times \
         (a 2nd+ occurrence would indicate it also showed up in the \
         `history -n 50` listing): stdout={stdout} stderr={stderr}"
    );
    // rc.jsh 側の行（"alias rc_marker_cmd=..."）は rc/source 経由なので
    // 引き続き記録されてはならない。
    assert!(
        !stdout.contains("rc_marker_cmd"),
        "a line executed via the rc script must NOT be recorded to history \
         (Fix B5 / DESIGN CONTRACT): stdout={stdout}"
    );
}

/// `dash_c_lines_are_not_recorded_to_history` の単純化版: `--rcfile` を
/// 一切絡めず、`jarvish -c "<command>"` 単体だけで
/// 「-c で渡した行は履歴に記録されない」ことを確認する。上のテストは
/// rc.jsh との相互作用込みの回帰確認を兼ねるため、こちらは `-c` 単体の
/// 非記録契約だけを狙い撃ちで検証する最小構成のテストとして分離した
/// （`--no-rc` で rc.jsh 読み込み自体を無効化し、変数を完全に排除）。
#[test]
#[serial]
fn dash_c_single_command_is_not_recorded_to_history() {
    let tmpdir = tempfile::tempdir().unwrap();
    let home = tmpdir.path();

    let exe = env!("CARGO_BIN_EXE_jarvish");
    // マーカーは `echo` 経由で出力する（単語単体の未知コマンド名は
    // InputType::NaturalLanguage に分類されて AI にルーティングされて
    // しまい、Command 経路 = record_history の対象経路を通らないため。
    // 詳細は `dash_c_lines_are_not_recorded_to_history` のコメント参照）。
    let output = std::process::Command::new(exe)
        .env("HOME", home)
        .args([
            "--no-rc",
            "-c",
            "echo some_unique_marker_cmd\nhistory -n 50",
        ])
        .output()
        .expect("failed to spawn jarvish");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stdout.contains("some_unique_marker_cmd"),
        "sanity check: the echo command itself must have run and printed the \
         marker: stdout={stdout} stderr={stderr}"
    );
    // echo 自身の出力として1回だけ現れるはず。history 一覧
    // （`history -n 50` の出力）に記録されていれば2回目以降が出現する。
    let occurrences = stdout.matches("some_unique_marker_cmd").count();
    assert_eq!(
        occurrences, 1,
        "a line passed via `jarvish -c` must NOT be recorded to history.db \
         (non-interactive single execution, interactive=false): expected \
         the marker to appear exactly once (from echo's own stdout) but it \
         appeared {occurrences} times: stdout={stdout} stderr={stderr}"
    );
}
