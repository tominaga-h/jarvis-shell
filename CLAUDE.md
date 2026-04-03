# CLAUDE.md

## Project Overview

**jarvish** — Next Generation AI Integrated Shell written in Rust, inspired by J.A.R.V.I.S. from Iron Man.
A standalone interactive shell (not a wrapper around bash/zsh) that natively integrates AI into the terminal workflow.

## Development Commands

```bash
make check          # fmt (auto-fix) + check + clippy + test — run before every commit
make install-hooks  # Install pre-push git hook (mirrors CI)
cargo build --release
```

CI mirrors `make check`: `cargo fmt --check`, `cargo check --all-targets`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all-targets`.

## Git Flow

`feature/*` → `develop` → `main` (PRs target `main`)

## Architecture

Four core components — keep them well-separated:

| Component | Module | Role |
|---|---|---|
| Line Editor | `src/cli/` | reedline REPL, prompt, syntax highlighting, autocomplete |
| Execution Engine | `src/engine/` | builtins, external commands via PTY, I/O capture (tee), AI dispatch |
| Black Box | `src/storage/` | SQLite (`history.db`) + SHA-256/zstd blob store (`~/.local/share/jarvish/`) |
| AI Brain | `src/ai/` + `src/shell/` | OpenAI API, NL classifier, autonomous agent loop with tool calls |

### Source Layout

```
src/
├── main.rs           # Entry point (clap args, session_id, logging init)
├── logging.rs        # tracing setup, debug log dir: ./var/logs
├── ai/               # client/, tools/, prompts.rs, stream.rs, types.rs
├── engine/           # builtins/, classifier/, dispatch/, exec/, parser/
│                     # pty.rs, redirect.rs, terminal.rs, expand.rs, io.rs
├── shell/            # mod.rs (Shell struct), ai_router.rs, input.rs,
│                     # editor.rs, investigate.rs
├── storage/          # history.rs, blob.rs, context.rs, record.rs, sanitizer.rs
├── config/           # mod.rs, defaults.rs
└── cli/              # prompt/, completer/, highlighter/, banner.rs, jarvis.rs
```

## Key Design Decisions

- **Builtins must run in-process**: `cd`, `exit`, `export`, `alias` etc. use `std::env::set_current_dir` — never spawn a subprocess for these.
- **I/O Capture (tee)**: External command stdout/stderr is simultaneously forwarded to the terminal AND captured into memory buffers, then persisted to the Black Box after execution.
- **PTY for interactive programs**: `vim`, `top`, etc. run via PTY (`src/engine/pty.rs`) so they work natively.
- **Secret masking**: `src/storage/sanitizer.rs` strips API keys/tokens before persisting to the Black Box.
- **Async Git prompt**: Git status scanning uses Stale-While-Revalidate (background thread) — zero UI jitter.
- **Session isolation**: Each shell process gets a random `session_id: i64` and `session_key: 6-char hex` used for history DB grouping and log file prefixing.

## Configuration

User config: `~/.config/jarvish/config.toml` (auto-generated on first launch).
Sections: `[ai]`, `[alias]`, `[export]`, `[prompt]`, `[completion]`.

Debug mode: `jarvish --debug` writes logs to `./var/logs/`.

## CLI Flags

- `jarvish --debug` — enable tracing logs to `./var/logs/`
- `jarvish -c "<command>"` — run a single command and exit
- `jarvish -v` / `--version` — print version

## Rules

### Critical

- Always read `docs/OVERVIEW.md` at the start of each session to understand the project overview.
- Never run `cargo build`, `cargo test`, or `cargo check` in a sandboxed environment.
- When changing a config value, always reflect the change in: source code comments, `README.md`, `docs/README_JA.md`, and the output of the `source` builtin command.

### Coding Conventions

- Each file (module) must have a single, clear responsibility (a data structure with its behavior, or a specific algorithm).
- Large files (e.g. `mod.rs`, `client.rs`, `exec.rs`) must be reorganized into directories with submodules, grouped by feature.

### Development Cycle (実装 → 完了の必須フロー)

新機能の実装・バグ修正は以下の順序で進めること。ステップを飛ばしてはならない。

1. **実装**: コードを実装し、ユニットテスト・インテグレーションテストを同時に追加する
2. **品質検証**: レビュアーによる実装の品質検証（コード品質、設計適合性）
3. **テストコード追加**: 検証で不足が指摘されたテストを追加する
4. **テストチェック**: `make check`（fmt + clippy + test）を全パスさせる
5. **テストカバレッジ調査**: 追加したテストのカバレッジを調査し、未テストの重要ロジックパスがないか確認する。不足があればステップ3に戻る
6. **完了**: コミット → develop マージ → タグ → push

- テストカバレッジ調査で不足が見つかった場合、テスト追加 → 再調査のサイクルを回すこと
- `make check` が通らない限り、マージに進んではならない

### Miscellaneous

- Markdown filenames under the `docs/` folder must always be ALL UPPERCASE (e.g. `OVERVIEW.md`, `CHANGELOG.md`).
