//! テスト専用: 補完系テストが使う「使い捨て git リポジトリ」の生成ヘルパー。
//!
//! `git.rs` / `mod.rs` / `carapace.rs` の 3 箇所に同一の
//! `create_test_git_repo()` がコピペされており、いずれも
//! **実行環境のグローバル git 設定を継承してしまう**問題があった:
//!
//! - `init.defaultBranch` を `develop` 等に設定している開発者の環境では、
//!   初期ブランチ名が `main` / `master` 以外になり
//!   `complete_git_branch_current_branch_comes_first` が落ちる。
//! - `commit.gpgsign = true` かつ GPG エージェントが使えない環境では
//!   `git commit --allow-empty` が失敗するが、旧実装は `.output().unwrap()`
//!   （= プロセス起動の成否しか見ず、終了ステータスを検査しない）だったため
//!   **コミットが無い空リポジトリのまま**テストが進み、後続の assertion が
//!   一見無関係な形で落ちていた。
//! - `core.hooksPath` をグローバル設定している環境では、このリポジトリ自身の
//!   `githooks/` が使い捨てリポジトリにも適用されうる。
//!
//! ここでは以下で環境非依存にする:
//! - `git init --initial-branch=main` で初期ブランチ名を固定する。
//! - `GIT_CONFIG_GLOBAL` / `GIT_CONFIG_SYSTEM` を `/dev/null` に向け、
//!   グローバル・システム設定を一切読ませない。
//! - `commit.gpgsign=false` / `core.hooksPath=` を明示的に上書きする。
//! - 各 git コマンドの終了ステータスを検査し、失敗したら stderr 付きで
//!   即座に panic する（沈黙して壊れたリポジトリを作らない）。

use std::path::Path;
use std::process::Command;

/// 環境設定を完全に無効化した `git` コマンドを組み立てる。
fn isolated_git(dir: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(dir)
        // グローバル/システム設定を読ませない（`/dev/null` は空ファイル
        // として扱われる）。ユーザ環境の `init.defaultBranch` /
        // `commit.gpgsign` / `core.hooksPath` などの影響を遮断する。
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        // 万一 GPG 署名が有効になっても署名を試みないようにする保険。
        .env("GIT_CONFIG_NOSYSTEM", "1");
    command
}

/// `args` を隔離済み git として実行し、失敗したら stderr 付きで panic する。
fn run_git(dir: &Path, args: &[&str]) {
    let output = isolated_git(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn `git {}`: {e}", args.join(" ")));
    assert!(
        output.status.success(),
        "`git {}` failed (status {:?}): {}",
        args.join(" "),
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// 初期ブランチ `main`・空コミット 1 個・追加ブランチ `test-feature` を持つ
/// リポジトリを `dir` に作る。
///
/// ブランチ名を固定するため、テスト側は「カレントブランチは `main`」と
/// 決め打ちで assert してよい。
pub(super) fn init_repo_with_test_feature_branch(dir: &Path) {
    // `--initial-branch` は git 2.28+ が必要。それ未満では `init` 自体が
    // 失敗するため、フォールバックとして `init` + `branch -m main` を使う。
    let init = isolated_git(dir)
        .args(["init", "--initial-branch=main"])
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn `git init`: {e}"));
    if !init.status.success() {
        run_git(dir, &["init"]);
    }

    run_git(dir, &["config", "user.email", "test@test.com"]);
    run_git(dir, &["config", "user.name", "Test"]);
    // グローバル設定を遮断済みでも、明示的に無効化しておく（多重防御）。
    run_git(dir, &["config", "commit.gpgsign", "false"]);
    run_git(dir, &["config", "core.hooksPath", ""]);

    run_git(dir, &["commit", "--allow-empty", "-m", "init"]);

    // `--initial-branch` が使えなかった場合に備え、コミット後に改名する
    // （コミットが無い状態では古い git の `branch -m` が失敗するため
    // コミット後に実施する）。既に `main` なら no-op 相当。
    run_git(dir, &["branch", "-M", "main"]);

    run_git(dir, &["branch", "test-feature"]);
}

/// [`init_repo_with_test_feature_branch`] に加え、テスト用の git エイリアス
/// （`co` = `checkout`, `nb` = `checkout -b`）をローカル設定として登録する。
pub(super) fn init_repo_with_aliases(dir: &Path) {
    init_repo_with_test_feature_branch(dir);
    run_git(dir, &["config", "alias.co", "checkout"]);
    run_git(dir, &["config", "alias.nb", "checkout -b"]);
}
