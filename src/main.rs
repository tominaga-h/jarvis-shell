use std::path::PathBuf;

use clap::{CommandFactory, FromArgMatches, Parser};
use rand::Rng;
use tracing::{info, warn};

use jarvish::shell::RcOptions;
use jarvish::{engine, logging, shell};

/// Next Generation AI Integrated Shell
#[derive(Parser)]
#[command(name = "jarvish", version)]
struct Args {
    /// デバッグモード: ログを ./var/logs に出力する
    #[arg(long)]
    debug: bool,

    /// 文字列をコマンドとして実行して終了する
    #[arg(short = 'c', allow_hyphen_values = true)]
    command: Option<String>,

    /// rc.jsh の代わりに指定したパスの起動スクリプトを読み込む（自動生成はしない）
    #[arg(long, value_name = "PATH", conflicts_with = "no_rc")]
    rcfile: Option<PathBuf>,

    /// 起動スクリプト（rc.jsh）の読み込みを完全に無効化する
    #[arg(long, conflicts_with = "rcfile")]
    no_rc: bool,
}

#[tokio::main]
async fn main() {
    // .env ファイルから環境変数を読み込む
    dotenvy::dotenv().ok();

    let args = Args::from_arg_matches(
        &Args::command()
            .disable_version_flag(true)
            .arg(
                clap::Arg::new("version")
                    .short('v')
                    .long("version")
                    .action(clap::ArgAction::Version),
            )
            .get_matches(),
    )
    .expect("failed to parse args");

    let log_dir_override = if args.debug {
        Some(PathBuf::from("./var/logs"))
    } else {
        None
    };

    // プロセス固有のセッション ID を生成
    // session_id: 履歴のセッション分離に使用する一意な整数
    // session_key: ログのプレフィックスに使用する 6 文字 hex
    let session_id: i64 = rand::rng().random_range(0..i64::MAX);
    let session_key = format!("{:06x}", (session_id as u64) & 0xFFFFFF);

    // ログシステムの初期化（_guard は main 終了まで保持する必要がある）
    let (_guard, logging_ok) = logging::init_logging(log_dir_override, &session_key);
    logging::start_cpu_monitor();

    info!(
        "\n\n==== J.A.R.V.I.S.H. STARTED at [{}] ====\n",
        session_key
    );

    let rc_options = RcOptions {
        rcfile: args.rcfile,
        no_rc: args.no_rc,
    };
    // `-c` 指定時（非対話単体実行）は Tab 補完が一切発生しないため、
    // 起動時のウォーム zsh 補完デーモン事前ウォームアップをスキップする
    // （S5 修正 — 孤児 `/bin/zsh -i` 対策の1つ目、`Shell::new` のドキュメント
    // 参照）。
    let interactive = resolve_interactive(args.command.is_some());
    let mut shell = shell::Shell::new(logging_ok, session_id, rc_options, interactive);
    let (exit_code, action) = if let Some(ref command) = args.command {
        let exit_code = shell.run_command(command).await;
        // Fix B2: `run()`（対話 REPL）は `restart_requested` を再チェックして
        // LoopAction::Restart を選ぶが、`-c` 単体実行はこれまで
        // `engine::LoopAction::Exit` を決め打ちしていたため、`--rcfile`
        // スクリプト内や `-c` の引数内で `restart` を呼んでも
        // "Restarting jarvish..." が出力されるだけで実際には exec()
        // されずサイレントに終了していた。`run()` と同じ判定
        // （`resolve_run_command_action`）に揃え、`restart_requested` が
        // 立っていれば正直に `LoopAction::Restart` を返す。
        let action = resolve_run_command_action(shell.restart_requested());
        (exit_code, action)
    } else {
        shell.run().await
    };

    info!(
        "\n\n==== [{}] J.A.R.V.I.S.H. SHUTTING DOWN ====\n\n",
        session_key
    );

    // Restart アクション: exec() でプロセスを置換
    if action == engine::LoopAction::Restart {
        // _guard を明示的にドロップしてログをフラッシュ
        drop(_guard);
        // exec_restart() 内部で exec() 直前に温存 zsh デーモンを shutdown
        // する（A1, #89）。exec() が失敗して以下に到達した場合も、
        // デーモンは既に shutdown 済みなのでここでの追加対応は不要。
        let err = shell.exec_restart();
        // exec() が失敗した場合のみここに到達
        warn!(error = %err, "exec_restart failed");
        eprintln!("jarvish: restart failed: {err}");
        std::process::exit(1);
    }

    // std::process::exit はデストラクタを一切実行しないため、温存 zsh
    // デーモンが稼働中ならここで明示的に shutdown する（A2, #89 レビュー
    // 指摘 — README の「daemon is killed automatically when Jarvish exits」
    // を実際に真にする）。デーモンが元々稼働していなければ no-op。
    shell.shutdown_zsh_daemon();

    std::process::exit(exit_code);
}

/// CLI 引数から `Shell::new` へ渡す `interactive` フラグを決める純粋な
/// 決定関数（S5 修正）。
///
/// `has_command` は `args.command.is_some()`（`-c '<command>'` が指定
/// されたか）。`-c` 指定時は Tab 補完が一切発生しない非対話単体実行のため
/// `false`（= 起動時のウォーム zsh 補完デーモン事前ウォームアップを
/// スキップする）を返す。
fn resolve_interactive(has_command: bool) -> bool {
    !has_command
}

/// `-c`（`run_command`）実行後にどの `LoopAction` を選ぶべきかを決める
/// 純粋な決定関数（Fix B2）。
///
/// `run()`（対話 REPL）は `restart_requested` フラグを見て
/// `LoopAction::Restart` か `LoopAction::Exit` かを選んでいる
/// （`src/shell/mod.rs` の `run()` 末尾のループを参照）。`-c` 単体実行
/// もこれと同じ判定に揃える —— `restart` ビルトインは
/// `--rcfile` スクリプト内・rc.jsh 内・`-c` の引数自体のいずれの経路
/// からでも `Shell::restart_requested()`（同じ `AtomicBool`）を立てるため、
/// 呼び出し元（`main`）は経路を区別せずこのフラグだけを見ればよい。
///
/// 実際の `exec()` 呼び出し（[`shell::Shell::exec_restart`]）は副作用が
/// 大きい（プロセスイメージを置換する）ためユニットテストでは検証せず、
/// この決定ロジックだけを切り出してテストする。
fn resolve_run_command_action(restart_requested: bool) -> engine::LoopAction {
    if restart_requested {
        engine::LoopAction::Restart
    } else {
        engine::LoopAction::Exit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── resolve_interactive（S5 修正: -c 単体実行時の prewarm スキップ判定）──

    /// `-c` 未指定（対話 REPL 起動）では `interactive == true` を返し、
    /// 従来どおり起動時の zsh 補完デーモン事前ウォームアップが走ること。
    #[test]
    fn resolve_interactive_without_command_is_true() {
        assert!(resolve_interactive(false));
    }

    /// `-c '<command>'` 指定（非対話単体実行）では `interactive == false`
    /// を返し、`Shell::new` が prewarm スレッド自体を起動しないこと
    /// （S5 の孤児 `/bin/zsh -i` 対策その1）。
    #[test]
    fn resolve_interactive_with_command_is_false() {
        assert!(!resolve_interactive(true));
    }

    // ── resolve_run_command_action（Fix B2 の決定ロジック）──

    /// `restart_requested == true` のときは `LoopAction::Restart` を
    /// 選ぶこと（`restart` ビルトインが `--rcfile` スクリプト内や `-c`
    /// の引数内で呼ばれた場合に、`main` が黙って `LoopAction::Exit` を
    /// 決め打ちしてサイレントに終了する退行を防ぐ）。
    #[test]
    fn resolve_run_command_action_restart_requested_returns_restart() {
        assert_eq!(
            resolve_run_command_action(true),
            engine::LoopAction::Restart
        );
    }

    /// `restart_requested == false`（通常終了）のときは従来どおり
    /// `LoopAction::Exit` を選ぶこと（既存の `-c` 単体実行の回帰防止）。
    #[test]
    fn resolve_run_command_action_no_restart_returns_exit() {
        assert_eq!(resolve_run_command_action(false), engine::LoopAction::Exit);
    }

    /// `--rcfile` と `--no-rc` は clap の `conflicts_with` により
    /// 同時指定を拒否される（Phase 4.2 の DESIGN CONTRACT どおり）。
    #[test]
    fn rcfile_and_no_rc_conflict_is_rejected() {
        let result = Args::command().try_get_matches_from([
            "jarvish",
            "--rcfile",
            "/tmp/some_rc.jsh",
            "--no-rc",
        ]);
        assert!(
            result.is_err(),
            "--rcfile and --no-rc must be mutually exclusive"
        );
    }

    /// `--rcfile <PATH>` 単体は問題なくパースでき、値が保持されること。
    #[test]
    fn rcfile_alone_parses_value() {
        let matches = Args::command()
            .try_get_matches_from(["jarvish", "--rcfile", "/tmp/some_rc.jsh"])
            .expect("--rcfile alone must parse");
        let args = Args::from_arg_matches(&matches).expect("must convert to Args");
        assert_eq!(args.rcfile, Some(PathBuf::from("/tmp/some_rc.jsh")));
        assert!(!args.no_rc);
    }

    /// `--no-rc` 単体は問題なくパースでき、フラグが立つこと。
    #[test]
    fn no_rc_alone_parses_flag() {
        let matches = Args::command()
            .try_get_matches_from(["jarvish", "--no-rc"])
            .expect("--no-rc alone must parse");
        let args = Args::from_arg_matches(&matches).expect("must convert to Args");
        assert!(args.no_rc);
        assert_eq!(args.rcfile, None);
    }

    /// どちらも未指定の場合はデフォルト（両方 unset）になること。
    #[test]
    fn neither_flag_defaults_to_unset() {
        let matches = Args::command()
            .try_get_matches_from(["jarvish"])
            .expect("no flags must parse");
        let args = Args::from_arg_matches(&matches).expect("must convert to Args");
        assert!(!args.no_rc);
        assert_eq!(args.rcfile, None);
    }

    /// `--rcfile` は `-c` と併用できる（本 Phase の主要な組み合わせ）。
    #[test]
    fn rcfile_combines_with_dash_c() {
        let matches = Args::command()
            .try_get_matches_from(["jarvish", "--rcfile", "/tmp/some_rc.jsh", "-c", "echo hi"])
            .expect("--rcfile + -c must parse together");
        let args = Args::from_arg_matches(&matches).expect("must convert to Args");
        assert_eq!(args.rcfile, Some(PathBuf::from("/tmp/some_rc.jsh")));
        assert_eq!(args.command, Some("echo hi".to_string()));
    }
}
