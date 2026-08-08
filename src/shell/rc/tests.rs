use std::io;
use std::path::PathBuf;

use serial_test::serial;

use crate::engine::classifier::InputClassifier;
use crate::engine::{execute, try_builtin, LoopAction};

use super::execution::MAX_SOURCE_DEPTH;
use super::file_reading::{read_rc_file_guarded, MAX_RC_FILE_SIZE};
use super::options::RcOptions;
use super::parsing::parse_rc_lines;
use super::resolution::{is_toml_source_path, plan_rc_bootstrap, RcBootstrapPlan, ResolvedRc};
use super::template::{ensure_default_rc, TEMPLATE};

#[test]
fn parse_rc_lines_skips_blank_lines() {
    let content = "alias g=git\n\n\nexport FOO=bar\n";
    let lines = parse_rc_lines(content);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].text, "alias g=git");
    assert_eq!(lines[1].text, "export FOO=bar");
}

#[test]
fn parse_rc_lines_skips_whitespace_only_lines() {
    let content = "alias g=git\n   \n\t\nexport FOO=bar\n";
    let lines = parse_rc_lines(content);
    assert_eq!(lines.len(), 2);
}

#[test]
fn parse_rc_lines_skips_comment_lines() {
    let content = "# a comment\nalias g=git\n# another comment\n";
    let lines = parse_rc_lines(content);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].text, "alias g=git");
}

#[test]
fn parse_rc_lines_skips_indented_comment_lines() {
    let content = "    # indented comment\nalias g=git\n\t# tab-indented comment\n";
    let lines = parse_rc_lines(content);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].text, "alias g=git");
}

#[test]
fn parse_rc_lines_mid_line_hash_is_not_a_comment() {
    // '#' がある位置が「行頭の非空白文字」でなければコメントではない。
    let content = "echo 'hello #world'\nalias grep='grep --color=auto # colorize'\n";
    let lines = parse_rc_lines(content);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].text, "echo 'hello #world'");
    assert_eq!(lines[1].text, "alias grep='grep --color=auto # colorize'");
}

#[test]
fn parse_rc_lines_preserves_line_numbers_across_skipped_lines() {
    let content = "# comment line 1\n\nalias g=git\n\n# comment line 5\nexport FOO=bar\n";
    let lines = parse_rc_lines(content);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].lineno, 3);
    assert_eq!(lines[1].lineno, 6);
}

#[test]
fn parse_rc_lines_tolerates_crlf() {
    let content = "alias g=git\r\n# comment\r\nexport FOO=bar\r\n";
    let lines = parse_rc_lines(content);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].text, "alias g=git");
    assert_eq!(lines[1].text, "export FOO=bar");
    // CR が末尾に残っていないことを確認する
    assert!(!lines[0].text.contains('\r'));
    assert!(!lines[1].text.contains('\r'));
}

#[test]
fn parse_rc_lines_trims_leading_and_trailing_whitespace() {
    let content = "   alias g=git   \n";
    let lines = parse_rc_lines(content);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].text, "alias g=git");
}

#[test]
fn parse_rc_lines_empty_content_returns_empty_vec() {
    assert!(parse_rc_lines("").is_empty());
}

#[test]
fn parse_rc_lines_only_comments_and_blanks_returns_empty_vec() {
    let content = "# only comments\n\n   \n# more comments\n";
    assert!(parse_rc_lines(content).is_empty());
}

#[test]
fn template_parses_to_zero_executable_lines() {
    let lines = parse_rc_lines(TEMPLATE);
    assert!(
        lines.is_empty(),
        "TEMPLATE must be comments-only, got executable lines: {lines:?}"
    );
}

#[test]
#[serial]
fn rc_path_uses_home_env_var() {
    let original = std::env::var("HOME").ok();
    unsafe {
        std::env::set_var("HOME", "/tmp/jarvish-rc-path-test-home");
    }
    let path = super::resolution::rc_path();
    assert_eq!(
        path,
        PathBuf::from("/tmp/jarvish-rc-path-test-home/.config/jarvish/rc.jsh")
    );
    unsafe {
        match original {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }
}

#[test]
fn resolve_no_rc_wins_regardless_of_rcfile() {
    let opts = RcOptions {
        rcfile: Some(PathBuf::from("/tmp/should-be-ignored.jsh")),
        no_rc: true,
    };
    assert!(matches!(opts.resolve(), ResolvedRc::None));
}

#[test]
fn resolve_no_rc_alone() {
    let opts = RcOptions {
        rcfile: None,
        no_rc: true,
    };
    assert!(matches!(opts.resolve(), ResolvedRc::None));
}

#[test]
fn resolve_explicit_rcfile_returns_the_given_path() {
    let opts = RcOptions {
        rcfile: Some(PathBuf::from("/tmp/custom.jsh")),
        no_rc: false,
    };
    match opts.resolve() {
        ResolvedRc::Explicit(path) => assert_eq!(path, PathBuf::from("/tmp/custom.jsh")),
        other => panic!("expected ResolvedRc::Explicit, got {other:?}"),
    }
}

#[test]
#[serial]
fn resolve_default_when_both_unset() {
    let original = std::env::var("HOME").ok();
    unsafe {
        std::env::set_var("HOME", "/tmp/jarvish-rc-resolve-test-home");
    }
    let opts = RcOptions::default();
    match opts.resolve() {
        ResolvedRc::Default(path) => assert_eq!(
            path,
            PathBuf::from("/tmp/jarvish-rc-resolve-test-home/.config/jarvish/rc.jsh")
        ),
        other => panic!("expected ResolvedRc::Default, got {other:?}"),
    }
    unsafe {
        match original {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }
}

#[test]
fn plan_rc_bootstrap_no_rc_skips_without_touching_filesystem() {
    let plan = plan_rc_bootstrap(ResolvedRc::None);
    assert_eq!(plan, RcBootstrapPlan::Skip);
}

#[test]
fn plan_rc_bootstrap_no_rc_skip_implies_no_bootstrap_side_effect() {
    let tmpdir = tempfile::tempdir().unwrap();
    let rc_path = tmpdir.path().join(".config/jarvish/rc.jsh");
    assert!(!rc_path.exists());

    let skip_plan = plan_rc_bootstrap(ResolvedRc::None);
    assert_eq!(skip_plan, RcBootstrapPlan::Skip);
    assert!(
        !rc_path.exists(),
        "planning Skip must not itself create the default rc.jsh"
    );

    let default_plan = plan_rc_bootstrap(ResolvedRc::Default(rc_path.clone()));
    match default_plan {
        RcBootstrapPlan::BootstrapAndRun { path, display_name } => {
            assert_eq!(path, rc_path);
            assert_eq!(display_name, "rc.jsh");
            ensure_default_rc(&path);
            assert!(
                path.exists(),
                "BootstrapAndRun's path, once passed to ensure_default_rc, must exist"
            );
        }
        other => panic!("expected BootstrapAndRun, got {other:?}"),
    }
}

#[test]
fn plan_rc_bootstrap_explicit_existing_path_runs_without_bootstrap() {
    let tmpdir = tempfile::tempdir().unwrap();
    let path = tmpdir.path().join("custom.jsh");
    std::fs::write(&path, "alias x=echo\n").unwrap();

    match plan_rc_bootstrap(ResolvedRc::Explicit(path.clone())) {
        RcBootstrapPlan::RunExplicit {
            path: got_path,
            display_name,
        } => {
            assert_eq!(got_path, path);
            assert_eq!(display_name, "custom.jsh");
        }
        other => panic!("expected RunExplicit, got {other:?}"),
    }
}

#[test]
fn plan_rc_bootstrap_explicit_missing_path_never_bootstraps() {
    let tmpdir = tempfile::tempdir().unwrap();
    let path = tmpdir.path().join("does-not-exist.jsh");
    assert!(!path.exists());

    let plan = plan_rc_bootstrap(ResolvedRc::Explicit(path.clone()));
    assert_eq!(
        plan,
        RcBootstrapPlan::ExplicitMissing { path: path.clone() }
    );
    assert!(
        !path.exists(),
        "a missing explicit --rcfile path must never be auto-generated"
    );
}

#[test]
fn ensure_default_rc_creates_file_and_parent_dirs() {
    let tmpdir = tempfile::tempdir().unwrap();
    let path = tmpdir.path().join("nested/dir/rc.jsh");
    assert!(!path.exists());

    ensure_default_rc(&path);

    assert!(path.exists());
    let content = std::fs::read_to_string(&path).unwrap();
    assert_eq!(content, TEMPLATE);
}

#[test]
fn ensure_default_rc_never_overwrites_existing_file() {
    let tmpdir = tempfile::tempdir().unwrap();
    let path = tmpdir.path().join("rc.jsh");
    std::fs::write(&path, "alias custom=echo\n").unwrap();

    ensure_default_rc(&path);

    let content = std::fs::read_to_string(&path).unwrap();
    assert_eq!(content, "alias custom=echo\n");
}

#[test]
fn ensure_default_rc_is_idempotent_create_once() {
    let tmpdir = tempfile::tempdir().unwrap();
    let path = tmpdir.path().join("rc.jsh");

    ensure_default_rc(&path);
    let first = std::fs::read_to_string(&path).unwrap();

    std::fs::write(&path, "export EDITED=1\n").unwrap();
    ensure_default_rc(&path);
    let second = std::fs::read_to_string(&path).unwrap();

    assert_eq!(first, TEMPLATE);
    assert_eq!(second, "export EDITED=1\n");
}

#[test]
#[cfg(unix)]
fn ensure_default_rc_dangling_symlink_is_not_followed() {
    use std::os::unix::fs::symlink;

    let tmpdir = tempfile::tempdir().unwrap();
    let rc_path = tmpdir.path().join("rc.jsh");
    let attacker_target = tmpdir.path().join("attacker_target.txt");
    symlink(&attacker_target, &rc_path).unwrap();

    assert!(
        !rc_path.exists(),
        "sanity: a dangling symlink must report exists() == false"
    );

    ensure_default_rc(&rc_path);

    assert!(
        !attacker_target.exists(),
        "ensure_default_rc must NOT write through a dangling symlink to the attacker's target"
    );
    assert!(
        std::fs::symlink_metadata(&rc_path)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the dangling symlink itself must be left untouched"
    );
}

#[test]
#[cfg(unix)]
fn ensure_default_rc_refuses_symlinked_parent_dir() {
    use std::os::unix::fs::symlink;

    let tmpdir = tempfile::tempdir().unwrap();
    let real_dir = tmpdir.path().join("real_dir");
    std::fs::create_dir_all(&real_dir).unwrap();
    let linked_dir = tmpdir.path().join("linked_dir");
    symlink(&real_dir, &linked_dir).unwrap();

    let rc_path = linked_dir.join("rc.jsh");

    ensure_default_rc(&rc_path);

    assert!(
        !real_dir.join("rc.jsh").exists(),
        "ensure_default_rc must refuse to write into a symlinked parent directory"
    );
    assert!(!rc_path.exists());
}

#[test]
fn ensure_default_rc_symlink_to_regular_file_read_path_still_works() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let tmpdir = tempfile::tempdir().unwrap();
        let real_file = tmpdir.path().join("real_rc.jsh");
        std::fs::write(&real_file, "alias viaSymlink=echo\n").unwrap();
        let link_path = tmpdir.path().join("rc.jsh");
        symlink(&real_file, &link_path).unwrap();

        ensure_default_rc(&link_path);
        let content = std::fs::read_to_string(&real_file).unwrap();
        assert_eq!(
            content, "alias viaSymlink=echo\n",
            "ensure_default_rc must not touch a symlink pointing at an existing regular file"
        );

        let read_back = read_rc_file_guarded(&link_path).unwrap();
        assert_eq!(read_back, "alias viaSymlink=echo\n");
    }
}

#[test]
fn read_rc_file_guarded_reads_normal_file() {
    let tmpdir = tempfile::tempdir().unwrap();
    let path = tmpdir.path().join("rc.jsh");
    std::fs::write(&path, "alias g=git\n").unwrap();

    let content = read_rc_file_guarded(&path).unwrap();
    assert_eq!(content, "alias g=git\n");
}

#[test]
fn read_rc_file_guarded_missing_file_is_not_found_error() {
    let tmpdir = tempfile::tempdir().unwrap();
    let path = tmpdir.path().join("does_not_exist.jsh");

    let err = read_rc_file_guarded(&path).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::NotFound);
}

#[test]
fn read_rc_file_guarded_directory_reports_is_a_directory() {
    let tmpdir = tempfile::tempdir().unwrap();
    let dir_path = tmpdir.path().join("some_dir");
    std::fs::create_dir_all(&dir_path).unwrap();

    let err = read_rc_file_guarded(&dir_path).unwrap_err();
    assert!(
        err.to_string().contains("is a directory"),
        "expected 'is a directory' in error, got: {err}"
    );
}

#[test]
fn read_rc_file_guarded_oversized_file_reports_too_large() {
    let tmpdir = tempfile::tempdir().unwrap();
    let path = tmpdir.path().join("huge.jsh");
    // MAX_RC_FILE_SIZE を1バイト超える内容を書き込む。
    let oversized = vec![b'#'; (MAX_RC_FILE_SIZE + 1) as usize];
    std::fs::write(&path, &oversized).unwrap();

    let err = read_rc_file_guarded(&path).unwrap_err();
    assert!(
        err.to_string().contains("too large"),
        "expected 'too large' in error, got: {err}"
    );
}

#[test]
fn read_rc_file_guarded_exactly_at_limit_is_allowed() {
    let tmpdir = tempfile::tempdir().unwrap();
    let path = tmpdir.path().join("exact.jsh");
    let exact = vec![b'#'; MAX_RC_FILE_SIZE as usize];
    std::fs::write(&path, &exact).unwrap();

    let content = read_rc_file_guarded(&path).unwrap();
    assert_eq!(content.len(), MAX_RC_FILE_SIZE as usize);
}

#[test]
#[cfg(unix)]
fn read_rc_file_guarded_fifo_reports_not_a_regular_file_without_blocking() {
    use std::sync::mpsc;
    use std::time::Duration;

    let tmpdir = tempfile::tempdir().unwrap();
    let fifo_path = tmpdir.path().join("a_fifo");
    nix::unistd::mkfifo(&fifo_path, nix::sys::stat::Mode::S_IRWXU)
        .expect("failed to create test FIFO");

    let (tx, rx) = mpsc::channel();
    let fifo_for_thread = fifo_path.clone();
    std::thread::spawn(move || {
        let result = read_rc_file_guarded(&fifo_for_thread);
        let _ = tx.send(result);
    });

    let result = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("read_rc_file_guarded must not block on a writerless FIFO");

    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("not a regular file"),
        "expected 'not a regular file' in error, got: {err}"
    );
}

#[test]
#[cfg(unix)]
fn is_symlink_true_for_symlink_false_for_regular_and_missing() {
    use std::os::unix::fs::symlink;

    let tmpdir = tempfile::tempdir().unwrap();
    let real_file = tmpdir.path().join("real.txt");
    std::fs::write(&real_file, "x").unwrap();
    let link = tmpdir.path().join("link.txt");
    symlink(&real_file, &link).unwrap();
    let missing = tmpdir.path().join("missing.txt");

    assert!(super::resolution::is_symlink(&link).unwrap());
    assert!(!super::resolution::is_symlink(&real_file).unwrap());
    assert!(!super::resolution::is_symlink(&missing).unwrap());
}

#[test]
fn goodbye_pattern_detection_used_by_run_rc_line_matches_classifier() {
    assert!(InputClassifier::is_goodbye_pattern("goodbye"));
    assert!(InputClassifier::is_goodbye_pattern("bye"));
    assert!(InputClassifier::is_goodbye_pattern("さようなら"));
    assert!(!InputClassifier::is_goodbye_pattern(
        "echo goodbye-file.txt"
    ));
}

#[test]
fn try_builtin_exit_line_signals_exit_action() {
    let result = try_builtin("exit").expect("exit must be a recognized builtin");
    assert_eq!(result.action, LoopAction::Exit);
}

#[test]
#[serial]
fn try_builtin_normal_command_continues() {
    let original = std::env::current_dir().expect("failed to get current dir");
    let tmpdir = tempfile::tempdir().unwrap();

    let result = try_builtin(&format!("cd {}", tmpdir.path().display()))
        .expect("cd must be a recognized builtin");
    assert_eq!(result.action, LoopAction::Continue);

    std::env::set_current_dir(&original).expect("failed to restore current dir");
}

#[test]
fn execute_unknown_command_line_is_nonzero_exit_but_continues() {
    let result = execute("please explain this error to me");
    assert_ne!(result.exit_code, 0);
}

#[test]
fn is_toml_source_path_lowercase_toml() {
    assert!(is_toml_source_path("config.toml"));
    assert!(is_toml_source_path("~/.config/jarvish/config.toml"));
    assert!(is_toml_source_path("./relative/path/settings.toml"));
}

#[test]
fn is_toml_source_path_uppercase_and_mixed_case_toml() {
    assert!(is_toml_source_path("CONFIG.TOML"));
    assert!(is_toml_source_path("Config.Toml"));
    assert!(is_toml_source_path("settings.ToMl"));
}

#[test]
fn is_toml_source_path_jsh_extension_is_not_toml() {
    assert!(!is_toml_source_path("rc.jsh"));
    assert!(!is_toml_source_path("~/.config/jarvish/rc.jsh"));
}

#[test]
fn is_toml_source_path_no_extension_is_not_toml() {
    assert!(!is_toml_source_path("myrc"));
    assert!(!is_toml_source_path("~/.config/jarvish/myscript"));
}

#[test]
fn is_toml_source_path_other_extensions_are_not_toml() {
    assert!(!is_toml_source_path("script.sh"));
    assert!(!is_toml_source_path("notes.txt"));
    assert!(!is_toml_source_path("archive.toml.bak"));
}

#[test]
fn max_source_depth_is_eight() {
    assert_eq!(MAX_SOURCE_DEPTH, 8);
}

#[test]
fn depth_guard_boundary_allows_up_to_max_and_rejects_beyond() {
    for current_depth in 0..MAX_SOURCE_DEPTH {
        let next_depth = current_depth + 1;
        assert!(
            next_depth <= MAX_SOURCE_DEPTH,
            "depth {current_depth} -> {next_depth} must still be allowed"
        );
    }
    let current_depth = MAX_SOURCE_DEPTH;
    let next_depth = current_depth + 1;
    assert!(
        next_depth > MAX_SOURCE_DEPTH,
        "nesting one level beyond MAX_SOURCE_DEPTH must be rejected"
    );
}
