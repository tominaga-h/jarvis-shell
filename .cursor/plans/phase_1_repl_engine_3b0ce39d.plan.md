---
name: Phase 1 REPL Engine
overview: Jarvis Shell (jarvish) の Phase 1 として、reedline ベースの REPL ループ、ビルトインコマンド (cd, cwd, exit)、os_pipe による I/O キャプチャ付き外部コマンド実行エンジンを構築する。
todos:
  - id: cargo-setup
    content: Cargo.toml 作成（reedline, os_pipe, tokio, shell-words, anyhow, git2）
    status: completed
  - id: project-structure
    content: src/ ディレクトリ構成とモジュール定義（engine/mod.rs, CommandResult 型等）
    status: completed
  - id: prompt
    content: Jarvis 風カスタムプロンプト実装（src/prompt.rs）
    status: completed
  - id: builtin
    content: ビルトインコマンド実装（cd, cwd, exit） - src/engine/builtin.rs
    status: completed
  - id: exec-tee
    content: 外部コマンド実行 + I/O Tee キャプチャ - src/engine/exec.rs
    status: completed
  - id: main-repl
    content: REPL メインループ統合 - src/main.rs
    status: completed
  - id: unit-tests
    content: 各モジュールにユニットテスト追加（builtin.rs, exec.rs, prompt.rs）
    status: completed
  - id: build-test
    content: ビルド確認と cargo test 実行
    status: completed
isProject: false
---

# Phase 1: The Basic REPL & Execution Engine

## ゴール

`ls` が動き、`cd` で移動でき、`exit` で終了できるシェル。外部コマンドの stdout/stderr を画面表示しつつメモリバッファに保持する（Phase 2 の永続化に備えた tee 機構）。

## ディレクトリ構成

```
jarvis-shell/
  Cargo.toml
  src/
    main.rs              # エントリーポイント、REPLループ
    engine/
      mod.rs             # engine モジュール定義 + CommandResult 型
      builtin.rs         # ビルトインコマンド (cd, cwd, exit)
      exec.rs            # 外部コマンド実行 + I/O tee キャプチャ
    prompt.rs            # Jarvis 風カスタムプロンプト
```

## 依存クレート ([Cargo.toml](Cargo.toml))

- **reedline** `0.44`: Fish ライクな REPL（行エディタ）
- **os_pipe** `1.2`: 子プロセスの stdout/stderr パイプ作成（tee 用）
- **tokio** `1` (features: full): 非同期ランタイム（Phase 2 以降で本格利用、Phase 1 では `main` に `#[tokio::main]` を付与して準備）
- **shell-words** `1`: クォート付き引数の正しいパース（`"hello world"` を1引数として扱う）
- **anyhow** `1`: エラーハンドリング簡素化
- **git2** `0.19`: Git リポジトリのブランチ名をネイティブに取得（プロンプト表示用）

## 実装詳細

### 1. Cargo.toml セットアップ

プロジェクト名 `jarvish`、edition 2021 で上記依存を定義。バイナリ名は `jarvish`。

### 2. REPL メインループ ([src/main.rs](src/main.rs))

```rust
// 疑似コード
#[tokio::main]
async fn main() {
    let mut editor = Reedline::create();
    let prompt = JarvisPrompt::new();
    loop {
        match editor.read_line(&prompt) {
            Ok(Signal::Success(line)) => {
                let result = execute(&line);
                match result.action {
                    LoopAction::Continue => { /* Phase 2: ここで result を Black Box に保存 */ }
                    LoopAction::Exit => break,
                }
            }
            Ok(Signal::CtrlC) => { /* 行をクリアして続行 */ }
            Ok(Signal::CtrlD) => break,
            Err(e) => { eprintln!("Error: {e}"); break; }
        }
    }
}
```

- `execute()` は入力を `shell_words::split()` でトークン化し、ビルトイン or 外部コマンドに振り分ける
- 戻り値 `CommandResult` に `stdout`, `stderr`, `exit_code`, `action` を含める

### 3. ビルトインコマンド ([src/engine/builtin.rs](src/engine/builtin.rs))


| コマンド        | 実装                                                  |
| ----------- | --------------------------------------------------- |
| `cd [path]` | `std::env::set_current_dir(path)` (引数なしは `$HOME` へ) |
| `cwd`       | `std::env::current_dir()` の結果を stdout に出力           |
| `exit`      | `LoopAction::Exit` を返す                              |


- ビルトインかどうかの判定は、コマンド名の match で行う
- 将来的にビルトインを追加しやすいよう、`dispatch_builtin(cmd, args) -> Option<CommandResult>` の設計にする

### 4. 外部コマンド実行 + I/O Tee ([src/engine/exec.rs](src/engine/exec.rs))

Phase 1 の最重要実装。子プロセスの出力をリアルタイムで画面に表示しつつ、バッファにも保持する。

```
          ┌─────────────┐
          │ Child Proc  │
          │ (ls, git等)  │
          └──┬───┬──────┘
     stdout  │   │  stderr
             ▼   ▼
        ┌────────────┐
        │  os_pipe   │
        └──┬───┬─────┘
           │   │
    ┌──────┘   └──────┐
    ▼                  ▼
 Thread 1           Thread 2
 (stdout tee)       (stderr tee)
    │                  │
    ├─→ Terminal       ├─→ Terminal
    └─→ Buffer         └─→ Buffer
```

- `os_pipe::pipe()` で読み書きペアを作成
- `Command::new(cmd).args(args).stdout(pipe_write).stderr(pipe_write)` で子プロセス起動
- **別スレッド** (`std::thread::spawn`) でパイプの読み取り端から BufReader でループ読み取り
  - 読み取ったデータを `io::stdout().write_all()` で画面出力
  - 同時に `Vec<u8>` バッファに `extend_from_slice()` で蓄積
- `child.wait()` で終了を待ち、exit code を取得
- スレッドを `join()` してバッファを回収
- `CommandResult { stdout: String, stderr: String, exit_code, action: Continue }` を返す

### 5. カスタムプロンプト ([src/prompt.rs](src/prompt.rs))

reedline の `Prompt` トレイトを実装したカスタムプロンプト:

```
⚡jarvish in ~/dev/project on  main
 ❯
```

- 1行目: `⚡jarvish` ブランド + `in` + カレントディレクトリ（`~` でホーム短縮）+ Git ブランチ情報
- 2行目: `❯` シェブロンで入力開始
- Git ブランチ: `git2` クレートで `Repository::discover(cwd)` → `head().shorthand()` で取得
- Git リポジトリ外のディレクトリでは `on  branch` 部分を**非表示**にする
- 将来的にはユーザーがカスタマイズ可能にする予定

## ユニットテスト (`#[cfg(test)] mod tests`)

各モジュール内にインラインでユニットテストを記述する。

### builtin.rs のテスト

- **cd で指定ディレクトリに移動できる**: 一時ディレクトリ (`tempdir`) を作成し、`cd` 実行後に `current_dir()` が一致することを検証
- **cd 引数なしでホームへ移動**: `cd` を引数なしで実行し、`$HOME` に移動することを検証
- **cd 存在しないパスでエラー**: 存在しないパスを指定した場合、exit_code が非0であることを検証
- **cwd が現在のディレクトリを返す**: `cwd` 実行結果の stdout が `current_dir()` と一致することを検証
- **exit が LoopAction::Exit を返す**: `exit` 実行後の action が `Exit` であることを検証
- **未知のコマンドは None を返す**: `dispatch_builtin("ls", &[])` が `None` を返すことを検証

### exec.rs のテスト

- **echo コマンドの stdout キャプチャ**: `echo hello` を実行し、stdout バッファに `"hello\n"` が含まれることを検証
- **exit code の取得**: `true` (exit 0) と `false` (exit 1) を実行し、exit_code を検証
- **stderr キャプチャ**: stderr に出力するコマンド（例: シェル経由で `echo err >&2`）を実行し、stderr バッファにキャプチャされることを検証
- **存在しないコマンドのエラーハンドリング**: 存在しないコマンドを実行した場合、適切にエラーが返ることを検証

### prompt.rs のテスト

- **ホームディレクトリが `~` に短縮される**: ホームパスを渡し、`~` に変換されることを検証
- **ホーム配下のパスが `~/...` に短縮される**: `$HOME/dev/project` が `~/dev/project` になることを検証
- **ホーム外のパスはそのまま**: `/tmp` 等はそのまま `/tmp` であることを検証

### 注意事項

- `cd` テストはプロセスのカレントディレクトリを変更するため、**テスト間の干渉を避ける**よう各テストで一時ディレクトリを使用し、テスト後に元に戻す
- `tempfile` クレートを `dev-dependencies` に追加（一時ディレクトリ生成用）

## 手動確認項目

Phase 1 完了時に手動で確認:

- `jarvish` を起動し、プロンプトが表示される
- `ls` を実行して結果が表示される
- `cd /tmp` → `cwd` で `/tmp` が表示される
- `cd` (引数なし) でホームディレクトリに移動
- `exit` または Ctrl+D で正常終了
- 存在しないコマンド実行時にエラーメッセージが出る
- `echo "hello world"` でクォート付き引数が正しく処理される

