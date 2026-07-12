# 🤵 Jarvish — The AI-Native Shell

[![status](https://img.shields.io/github/actions/workflow/status/tominaga-h/jarvis-shell/ci.yml)](https://github.com/tominaga-h/jarvis-shell/actions)
[![version](https://img.shields.io/badge/version-1.15.0-blue)](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.15.0)

> 🌐 [日本語版 README はこちら](docs/README_JA.md)

## 💡 About

> _"I want J.A.R.V.I.S. as my companion — but inside my terminal."_

**Jarvish** is a **Next Generation AI Integrated Shell** written in Rust, inspired by **J.A.R.V.I.S.** from Marvel's Iron Man.

It is not just a wrapper around existing shells (Bash, Zsh) or an external tool. Jarvish deeply integrates AI into your terminal workflow itself, delivering an unprecedented experience where **you can seamlessly switch between regular commands and natural language** as naturally as breathing.

The days of copy-pasting errors into a browser to ask AI are over. Just ask Jarvish.

[![jarvish-demo](images/jarvish-demo.gif)](https://asciinema.org/a/806755)

## 📑 Table of Contents

- [About](#-about)
- [Core Experience](#-core-experience)
  - [Your Personal Assistant, Living in the Terminal](#1-your-personal-assistant-living-in-the-terminal)
  - [AI Pipe & AI Redirect (The Ultimate Text Processor)](#2-ai-pipe--ai-redirect-the-ultimate-text-processor)
  - ["The Black Box" (Total Recall Storage)](#3-the-black-box-total-recall-storage)
  - [Uncompromising "Blazing Fast" Shell UX](#4-uncompromising-blazing-fast-shell-ux)
- [Install](#-install)
- [Updating](#-updating)
- [Setup and Configuration](#️-setup-and-configuration)
  - [Starship Prompt Integration](#starship-prompt-integration)
  - [External Completion (carapace)](#external-completion-carapace)
  - [zsh Completion Bridge](#zsh-completion-bridge)
  - [Custom Completions (`complete` builtin)](#custom-completions-complete-builtin)
  - [Startup script (`rc.jsh`)](#-startup-script-rcjsh)
- [Architecture](#️-architecture)
- [Development](#-development)

## ✨ Core Experience

### 1. Your Personal Assistant, Living in the Terminal

- **Natural Language Execution**: Just type "show me the list of active ports" at the prompt, and Jarvish translates it into the optimal command and executes it.
- **Smart Error Handling**: When a command fails, Jarvish reads the `stdout`/`stderr` context and automatically analyzes the cause and suggests solutions.
- **Autonomous Agent**: More than just a chatbot — Jarvish can read/write files and re-execute commands on its own (Tool Calls).

### 2. AI Pipe & AI Redirect (The Ultimate Text Processor)

No more struggling to remember complex `awk`, `sed`, or `jq` syntax.

- **AI Pipe (`| ai "..."`)**: Filter and transform command output directly using natural language.
  ```bash
  ls -la | ai "what is the most heavy file?"
  docker ps | ai "output the container IDs and image names as JSON"
  ```
- **AI Redirect (`> ai "..."`)**: Send command output to Jarvish's context for interactive analysis.
  ```bash
  git log --oneline -10 > ai "summarize the intent of recent commits"
  eza --help > ai "what options can be used with --tree?"
  ```

### 3. "The Black Box" (Total Recall Storage)

Jarvish remembers everything that happens in your terminal.

- **Git-like History Storage**: Every command, timestamp, directory, exit code, and full `stdout`/`stderr` output is persisted in a content-addressable blob storage (SHA-256 + zstd compression).
- **Time-Traveling Context**: Even after restarting the shell, you can ask Jarvish "what caused that error yesterday?"
- **Security**: Sensitive information such as API keys or tokens (e.g., those in `.bashrc`) is automatically **masked** before being saved.

### 4. Uncompromising "Blazing Fast" Shell UX

Despite deep AI integration, Jarvish leverages Rust's strengths to deliver outstanding performance as an infrastructure tool.

- **Async Background Prompt**: Git status scanning runs in a separate thread (using the Stale-While-Revalidate pattern), achieving **zero UI jitter** regardless of repository size.
- **Fish-like Autocomplete**: Real-time syntax highlighting with powerful auto-completion for PATH binaries and file paths, plus optional [carapace](#external-completion-carapace) integration for argument/flag completion across hundreds of CLI tools.
- **Full PTY Support**: Interactive programs like `vim` and `top` work natively.
- **Job-control Ctrl+C**: Pressing `Ctrl+C` while a command runs interrupts only that command — the Jarvish shell itself keeps running. External commands are spawned into their own process group and given the terminal foreground, so the terminal-generated `SIGINT` reaches the child group only.
- **Starship Integration**: Native support for [Starship](https://starship.rs/) prompt — use your existing Starship configuration as-is.
- **Glob & Brace Expansion**: Bash/zsh-compatible filename expansion:
  - Glob: `ls *.toml`, `cat Cargo.???`, `rm [Cc]argo.lock`
  - Brace: `echo {a,b,c}`, `echo {1..5}`, `mkdir -p src/{api,cli}/v{1..3}`
  - Combined: `cp *.{txt,md} backup/`
  - `zsh`-compatible: errors on no-match (`jarvish: no matches found: <pattern>`)
  - Quotes / escapes are honored: `'*'`, `"{a,b}"`, `\*` stay literal.
- **`cdhist` / `cdj` directory jumping**: Recall and jump back to recently visited directories without leaving the shell:
  - `cdhist [--limit N]` — print recently visited directories in LRU order (one per line, deduplicated, current cwd excluded)
  - `cdj [pattern]` — fuzzy-pick a directory via `fzf` (requires `fzf` in `PATH`); `pattern` filters candidates by case-insensitive substring; a single match `cd`s immediately. The fzf preview pane shows `ls -Cp` of the highlighted directory (UNIX only).
  - Source of truth is the existing `command_history.cwd` column — no schema migration

## 🚀 Install

### Prerequisites

- **OpenAI API Key**
- **NerdFont** (recommended for prompt icons)

### Install via Homebrew (macOS)

```bash
brew tap tominaga-h/tap
brew install tominaga-h/tap/jarvish
```

### Install via Cargo

```bash
cargo install jarvish
```

### Build from Source

```bash
git clone https://github.com/tominaga-h/jarvis-shell.git
cd jarvis-shell
cargo install --path .
```

## 🔄 Updating

Jarvish has a built-in `update` command that updates itself to the latest version.

```bash
update            # Update to the latest version from GitHub Releases
update --check    # Check if a newer version is available (without installing)
```

If jarvish was installed via Homebrew, the command will detect this and guide you to use `brew upgrade jarvish` instead.

### Updating from a Local Binary

For developers who build from source, you can update from a locally compiled binary:

```bash
update --local                    # Use default path (target/release/jarvish)
update --local /path/to/jarvish   # Use a custom binary path
update --check --local            # Check the local binary version without installing
```

After a successful update, jarvish automatically restarts to apply the new version.

## ⚙️ Setup and Configuration

Set your OpenAI API key as an environment variable:

```bash
export OPENAI_API_KEY="sk-..."
```

> You can also configure this in the `[export]` section of `~/.config/jarvish/config.toml` for automatic setup.

### Configuration File (`config.toml`)

A default config file is automatically generated at `~/.config/jarvish/config.toml` on first launch.

```toml
[ai]
model = "gpt-4o"              # AI model to use
max_rounds = 10               # Max agent loop rounds
markdown_rendering = true     # Render AI responses as Markdown
ai_pipe_max_chars = 50000     # Max characters for AI Pipe input (fail-fast on overflow)
ai_redirect_max_chars = 50000 # Max characters for AI Redirect input (fail-fast on overflow)
temperature = 0.5             # Response randomness
ignore_auto_investigation_cmds = ["git log", "git diff"]  # Skip auto-investigation for these commands

[alias]
g = "git"                     # Command aliases (also manageable via builtins)
ll = "eza --icons -la"

[export]
PATH = "/usr/local/bin:$PATH" # Environment variables expanded on startup
# ⚠️ Caution: Setting SHELL = "/usr/local/bin/jarvish" causes external tools
# (Cursor, VS Code, etc.) to use jarvish as their subshell, which may trigger
# mass AI auto-investigations on tool hook failures.
# Keep SHELL set to bash/zsh if you only use jarvish as an interactive shell.

[prompt]
nerd_font = true              # Set to false if NerdFont is not installed
starship = false              # Set to true to use Starship prompt (requires: starship command + ~/.config/starship.toml)

[completion]
git_branch_commands = ["checkout", "switch", "merge", "rebase", "branch", "diff", "log", "cherry-pick", "reset", "push", "fetch"]
external = "auto"             # "auto" | "carapace" | "zsh" | "none" | ["carapace", "zsh"] — external completion policy (string or array)
external_timeout_ms = 400     # Timeout for the external completion process (milliseconds)
external_zsh_daemon = true    # Keep the zsh bridge warm in a persistent daemon (see "zsh Completion Bridge" below)

[startup]
commands = [                      # Commands to run on shell startup (skipped with -c option)
    "echo 'Welcome to jarvish!'",
    "export JAVA_HOME=/usr/lib/jvm/default",
]
```

> **Tip**: After changing settings, you can apply them without restarting using the `source` command:
>
> ```bash
> source ~/.config/jarvish/config.toml
> ```

### Starship Prompt Integration

Jarvish natively supports [Starship](https://starship.rs/) as an alternative prompt. When enabled, Jarvish calls `starship prompt` directly — no init scripts needed.

**Prerequisites:**

1. The `starship` command is installed and available in your PATH
2. A Starship config file exists at `~/.config/starship.toml` (or the path specified by the `STARSHIP_CONFIG` environment variable)

**Setup:**

```toml
# ~/.config/jarvish/config.toml
[prompt]
starship = true
```

Jarvish passes `--status`, `--cmd-duration`, and `--terminal-width` to `starship prompt`, so modules like `character`, `cmd_duration`, and `status` work as expected.

If `starship = true` is set but the prerequisites are not met, Jarvish falls back to the built-in prompt with a warning.

### External Completion (carapace)

Jarvish's Tab completion can bridge to [carapace](https://github.com/carapace-sh/carapace-bin), a multi-shell completion engine that ships completions for 500+ CLI tools (git, docker, kubectl, etc.). Install it with `brew install carapace`.

- **`[completion] external` accepts a string or an array**:
  - `"auto"` (the default) tries each provider in priority order (carapace, then the [zsh bridge](#zsh-completion-bridge)) and enables whichever binaries are found on `PATH` — no further configuration needed.
  - `"none"` disables external completion entirely.
  - `"carapace"` / `"zsh"` enables only that one provider (a warning is printed if its binary is missing).
  - An array such as `["zsh", "carapace"]` explicitly sets the priority order — providers are tried left to right, and each is enabled only if its binary is found. Unrecognized array entries are skipped with a warning; the rest of the array still applies.
- **Timeout + fallback**: Each external completion invocation is capped by `external_timeout_ms` (default 400ms). If a provider hangs, errors, or returns no candidates, Jarvish silently falls through to the next provider (and ultimately to its built-in path completion) — Tab never blocks waiting on an external process.
- **Hot-reload**: `external` and `external_timeout_ms` are re-read by the `source` builtin, and every configured provider's binary is re-detected (via `which`) on every reload. This means you can `brew install carapace` mid-session and run `source ~/.config/jarvish/config.toml` to enable it immediately, without restarting Jarvish. (Note: changing the *order* of an array — e.g. swapping `["carapace", "zsh"]` to `["zsh", "carapace"]` — takes effect on the next Jarvish restart, not on `source`; enabling/disabling a provider and re-detecting its binary does apply immediately.)
- **Widening coverage**: carapace also supports bridging to real shell completion functions (e.g. zsh's `compsys`). Export `CARAPACE_BRIDGES` (e.g. `CARAPACE_BRIDGES = "zsh"`) in the `[export]` section of `config.toml` to pull in completions that carapace doesn't natively ship.

### zsh Completion Bridge

If [carapace](#external-completion-carapace) doesn't have candidates for a command (or isn't enabled), Jarvish falls back to a built-in zsh bridge: it spawns a real zsh in the background and asks its native completion system (`compsys`, the `_*` functions) for suggestions. This means any completion function that works in zsh — including ones from third-party packages — can work in Jarvish too, without carapace support. Like carapace, it is controlled by the same `[completion] external` setting described above (e.g. `external = "zsh"` to use only the zsh bridge, or `external = ["zsh", "carapace"]` to prefer it over carapace).

- **Bridge zshrc**: The bridge zsh sources `~/.config/jarvish/zsh-bridge/.zshrc` instead of your real `~/.zshrc`, so it stays isolated from your interactive shell setup. Jarvish auto-generates this file (with commented examples) the first time the bridge runs, if it doesn't already exist — it is never overwritten afterward, so your edits are safe.
- **Adding completions**: Write ordinary zsh syntax in the bridge zshrc. For example, to pull in the [`zsh-completions`](https://github.com/zsh-users/zsh-completions) project installed via Homebrew:
  ```sh
  # ~/.config/jarvish/zsh-bridge/.zshrc
  fpath=(/opt/homebrew/share/zsh-completions $fpath)
  ```
  You can also add `compdef` lines to bind a completion function to a specific command, just as you would in a normal `~/.zshrc`.
- **Timeout + fallback**: Like carapace, every bridge invocation is capped by a timeout (shared with `external_timeout_ms`, with a higher floor to accommodate zsh's `compinit` startup cost). If the bridge hangs, errors, or returns nothing, Jarvish falls back to built-in path completion — Tab never blocks the UI.
- **Warm daemon (`external_zsh_daemon`)**: A one-shot zsh invocation (spawn a fresh `zsh`, run `compinit`, complete, exit) typically costs 700-1100ms per Tab press — mostly process/PTY startup, not the completion itself. When `external_zsh_daemon = true` (the default), Jarvish instead spawns a single `zsh -i` **as a plain child process of Jarvish** and keeps reusing it for every Tab press. This is not a system service and involves no `launchd`/`launchctl` — it is a per-session child process that lives only as long as your Jarvish shell does. Jarvish warms this daemon up **in the background as soon as the shell starts**, so your first Tab press is usually already warm; if the prewarm hasn't finished yet (or was skipped, e.g. `zsh` wasn't found at the time), the daemon is instead spawned lazily on whichever Tab press needs it first. Once warm, requests only pay for the completion computation itself — typically a couple of milliseconds. Completions that shell out to a slow interpreter (e.g. `tmuxinator`'s Ruby-based completion) are tolerated: the warm request timeout is floored at 2000ms, and a single slow/timed-out completion does **not** kill the daemon — Jarvish drains the late response on your next Tab press instead. Only two *consecutive* timeouts are treated as a real hang, at which point the daemon is killed in the background and the next Tab press lazily spawns a fresh one. Editing the bridge zshrc (see below) is detected automatically (by its file modification time) and transparently restarts the daemon on your next Tab press, so you never need to restart Jarvish after tweaking `fpath`/`compdef` entries. Set `external_zsh_daemon = false` to always use the one-shot invocation instead (this also serves as a manual escape hatch when troubleshooting the bridge); hot-reloadable via `source` — flipping it off shuts down any running daemon immediately, at `source` time, and flipping it back on spawns a new one lazily on the next zsh completion request. A running daemon is also always shut down before Jarvish exits or restarts (including via the `restart` builtin) — it never outlives the Jarvish session that spawned it.

**Troubleshooting: bridge completions suddenly return nothing after editing `fpath`.** If you add a directory to `fpath` in the bridge zshrc (as in the example above) and the zsh bridge stops returning candidates for *every* command, the cause is almost always zsh's `compinit` security check. `compinit` runs `compaudit`, which inspects not just the directories you added to `fpath` but also their parent directories, and refuses to proceed if any of them are group-writable — instead it prints an interactive `Ignore insecure directories and continue [ny]?` prompt. Since the bridge zsh runs inside an invisible `zpty` session, nothing can answer that prompt, so `compinit` hangs and completions silently fail across the board. This is common on Intel Macs, where Homebrew's `/usr/local/share` is group-writable by default (Apple Silicon's `/opt/homebrew` is much less likely to hit this). Run `compaudit` to list the offending directories, then fix it the same way Homebrew recommends: `chmod g-w /usr/local/share`.

### Custom Completions (`complete` builtin)

For commands not covered by carapace or the zsh bridge (your own scripts, an internal CLI, etc.), Jarvish provides a fish-style `complete` builtin for defining ad-hoc completions directly at the prompt — no external tool required.

- **Register**: `complete -c CMD [-s X]... [-l LONG]... [-a 'WORDS'] [-d DESC] [-n COND]` adds one completion spec for `CMD`. `-c/--command` is required. `-s` takes a single-character short flag (e.g. `-s v` for `-v`) — it must be a single ASCII graphic character and cannot be a quote (`'`/`"`) or backslash; `-l/--long-option` takes a long flag name (e.g. `-l verbose` for `--verbose`) — it must be non-empty and cannot contain whitespace or backslash. Both may be repeated to register several flags in one call, or accumulated by calling `complete` again for the same command. `-a/--arguments` supplies either a space-separated (optionally quoted) list of static candidate words, or a single dynamic source `"$(command)"` (see below). `-d/--description` sets the fallback text shown in the completion menu. `-n/--condition` restricts the spec to a subset of built-in conditions (see below); specs with an unrecognized condition are still registered and listed, but never offer completions. `-c`, `-a`, `-d`, and `-n` values must not contain a newline, carriage return, or NUL byte — such values would corrupt `complete`'s round-trippable listing and are rejected at registration (exit code 2).
- **List**: `complete` with no arguments prints every registered spec, one per line, in the same `complete -c ...` syntax you'd use to register it — so the output is directly re-runnable (round-trippable) once re-tokenized by Jarvish's own shell parser. Values containing anything outside a conservative safe set of characters (letters, digits, and `_ . / : = + , @ % ^ -`) are automatically wrapped in single quotes, including values containing backslashes — this avoids the backslash being reinterpreted as an escape character on re-parse.
- **Erase**: `complete -e -c CMD` removes every spec registered for `CMD`. `-e` without `-c` is an error.
- **Standalone only**: `complete` must be run as its own simple command (no `;`, `&&`, `||`, or pipe) — e.g. `complete -c foo -a bar` on its own line, or as `jarvish -c "complete -c foo -a bar"`. Running it inside a pipeline, a `cmd1 ; complete ...` list, or piped into `ai` has no access to the shell's live registry and fails with `complete: can only be used as a standalone command` (exit code 1) rather than silently discarding the registration; `complete --help` keeps working everywhere.

Example:

```sh
complete -c mycmd -s v -l verbose -d 'Verbose output'
complete -c mycmd -a 'start stop restart' -d 'Subcommand'
complete            # list everything you've registered so far
complete -e -c mycmd  # forget mycmd's completions
```

Once registered, pressing Tab after `mycmd ` (or `mycmd -`) offers the matching flags or argument words alongside Jarvish's other completion sources. Prefix matching (both for flags and for `-a` argument words) is **case-sensitive** — typing `mycmd B` will not match a candidate registered as `build`.

**Dynamic candidates (`-a "$(...)"`)**: if `-a`'s value is (once trimmed) exactly of the form `$(command)`, Jarvish treats it as a *dynamic* source instead of a static word list — `command` is run through `/bin/sh -c` on every Tab press and its stdout supplies the candidates. Each line of output is parsed as `value<TAB>description` (the tab and description are optional — a bare `value` line is fine and falls back to the spec's `-d`); blank lines are skipped and a trailing `\r` is stripped. The command is capped by `[completion] external_timeout_ms` (floored at 200ms); a timeout, non-zero exit, or spawn failure is treated as "zero candidates from this spec" rather than an error — other specs for the same command still apply, and Jarvish falls through to its other completion sources if nothing matches overall. Mixing static words and `$(...)` in one `-a` string is **not** supported — a spec's `-a` is either a static word list or a single `$(...)`, never both.

**Conditions (`-n`)**: only two condition forms are evaluated, and both run without spawning a subprocess:
- `__fish_use_subcommand` — true as long as no non-flag word has appeared yet after the command name (so `mycmd -v <Tab>` still counts as "no subcommand seen").
- `__fish_seen_subcommand_from w1 w2 ...` — true once any of the listed words has appeared after the command name.

A spec whose `-n` is anything else is **inactive for completion** (it never contributes candidates) but is still registered and shown by `complete`'s listing — this is a known limitation of this phase, not a bug.

Worked example — a `mycmd` with two subcommands, the second of which takes a dynamically-listed argument:

```sh
complete -c mycmd -n '__fish_use_subcommand' -a 'start stop'
complete -c mycmd -n '__fish_seen_subcommand_from start' -a "$(mycmd --list-targets)"
```

Pressing Tab right after `mycmd ` offers `start`/`stop`; after `mycmd start `, it instead runs `mycmd --list-targets` and offers its output as candidates.

**Persisting across restarts**: specs registered via `complete` at the prompt live only in memory and are lost when Jarvish exits. To make them (and other setup) survive restarts, put the same commands in [`rc.jsh`](#-startup-script-rcjsh) below.

### 🏁 Startup script (`rc.jsh`)

`~/.config/jarvish/rc.jsh` is a plain-text startup script that Jarvish runs once, every time it starts **interactively** — before the `[startup].commands` section of `config.toml`, and before the first prompt is shown. It exists to solve exactly the "session-only" problem above: put your `alias`/`export`/`complete` calls (or any other builtin) in `rc.jsh` and they persist across every restart, no shell alias or copy-paste required.

- **Location**: `~/.config/jarvish/rc.jsh` (mirrors `config.toml`'s location convention). A commented-only template is auto-generated here on first interactive launch if the file doesn't already exist — it is never overwritten afterward, so your edits are safe. (An explicit `--rcfile` path, below, is never auto-generated.)
- **CLI options**:
  - `--rcfile <PATH>` — load `<PATH>` instead of the default `~/.config/jarvish/rc.jsh`. Never auto-generated, even if missing: a missing explicit path prints `jarvish: rcfile not found: <PATH>` on stderr and Jarvish continues without an rc script. Unlike the default path, an explicit `--rcfile` is also honored in `-c` mode — it loads (and can run/`exit`) before the `-c` command executes; plain `-c` alone never touches rc.jsh at all.
  - `--no-rc` — skip rc script loading entirely, including the default-path template auto-generation.
  - `--rcfile` and `--no-rc` conflict and cannot be combined.
- **Format**: one command per line. Blank lines are skipped. A line whose first non-whitespace character is `#` is treated as a full-line comment and skipped — `#` appearing mid-line (e.g. inside a quoted string) does **not** start a comment. There is no line-continuation syntax; keep each command on a single line.
- **Classifier bypass guarantee**: every line runs through the same builtin dispatch path as typing it at the prompt (alias expansion first, then `alias`, `export`, `complete`, `cd`, `source`, and ordinary commands all work exactly as they do interactively) — but it **never** goes through the AI natural-language classifier. A line that looks like a question or a request to the AI assistant is not routed anywhere special; it's simply run as a command and fails as "not found" if it isn't one. `rc.jsh` is for deterministic setup, not conversation. Because alias expansion runs on every line, an `alias` defined earlier in the script is usable by any later line of that same script (or a script it `source`s, and vice versa).
- **Execution order**: `rc.jsh` → `[startup].commands` (`config.toml`) → first prompt.
- **Error handling**: a failing line prints its own error (from the command itself) plus a summary line `jarvish: rc.jsh:<lineno>: command exited with status <code>` — then execution continues with the next line. `rc.jsh` never aborts partway through because of one bad line. An `exit <code>` (or `restart`) line is a deliberate action, not a failing command, so it never prints this summary line, even when `<code>` is non-zero.
- **Exiting from a script**: an `exit` line (or a goodbye phrase such as `bye`/`さようなら`) stops the script and exits Jarvish immediately, the same way it would at the interactive prompt. A `restart` line behaves the same way in every mode, including `-c`: Jarvish re-execs itself instead of just printing "Restarting jarvish..." and quitting.
- **Not recorded to history**: lines executed via `rc.jsh` or `source` are never written to the Black Box (`history` builtin / `history.db`), even though every interactive or `-c` line is. This is intentional, not an oversight — script lines are configuration replay, not something a user typed, and recording them would spam `history` with the same `alias`/`export`/`complete` calls on every single launch (this mirrors how bash's `source`/`.` doesn't add to `history` either).

Example:

```sh
# ~/.config/jarvish/rc.jsh

# Persist a couple of aliases
alias gs="git status"
alias ll="eza --icons -la"

# Register a completion for an internal tool (see "Custom Completions" above)
complete -c mycmd -s v -l verbose -d 'Verbose output'
complete -c mycmd -a 'start stop restart' -d 'Subcommand'
```

#### `source`: reload config, or run a script

`source <path>` dispatches on the file's extension:

- **`.toml`** (case-insensitive) — reloads `config.toml` and re-applies `[ai]`/`[alias]`/`[export]`/`[prompt]`/`[completion]` in place, exactly as before. This is unchanged: `source ~/.config/jarvish/config.toml` still prints the familiar `Loaded ...` summary (see the Tip in "Configuration File" above).
- **any other extension, or none** — runs the file as an rc-style script, using the exact same executor as `rc.jsh` itself: classifier bypass, `#`-comment/blank-line handling, line-numbered `jarvish: <file>:<lineno>: ...` errors, continue-on-error, and `exit`/goodbye propagation all apply identically. This lets you factor a large `rc.jsh` into smaller files and `source` them, or load an ad-hoc script from the prompt (`source ./setup.jsh`).
- **Nesting**: a sourced script can itself `source` another script, up to a maximum depth of 8. Exceeding it (including a script that sources itself) stops with `jarvish: <file>: source nesting too deep` instead of hanging. This depth guard only bounds how deeply scripts can nest via `source` — it does **not** bound the total amount of work a script (or its whole `source` tree) can do at any single depth level. A script with a large number of `source` calls at the same level, or one that runs thousands of commands per line, is not stopped by this guard; keeping wide-fan-out or expensive scripts reasonable is the user's own responsibility.
- **Exit code**: a scripted `source` returns 0 if every line succeeded, 1 if any line failed, or exits the shell if the script itself contained an `exit`/goodbye line.
- **Missing file**: the exact wording differs by branch (they go through different code paths — the `.toml` branch delegates to the same config loader `source ~/.config/jarvish/config.toml` has always used, which reports the raw I/O error verbatim), but both branches exit 1: a missing **script** path (any extension other than `.toml`, or none) reports `jarvish: source: no such file: <path>`; a missing **`.toml`** path reports `jarvish: source: failed to read <path>: <os error>` (e.g. `No such file or directory (os error 2)`).
- **A directory named `*.toml`**: sourcing a path that merely looks like a config file by extension but is actually a directory reports `jarvish: source: <path> is a directory` and exits 1 — it never reaches the config-reload code path.

## 🏗️ Architecture

Jarvish is composed of four highly modular core components:

```mermaid
graph TB
    User(["User"]) --> A["Line Editor (reedline)"]
    A --> B["Execution Engine"]
    B --> B1["Builtin Commands (cd, exit, alias...)"]
    B --> B2["External Commands (PTY + I/O Capture)"]
    B --> D["AI Brain (OpenAI API / Tools)"]
    B2 --> C["Black Box"]
    D --> C
    C --> C1[("history.db (SQLite)")]
    C --> C2[("blobs/ (SHA-256 + zstd)")]
```

| Component            | Description                                                                                        |
| :------------------- | :------------------------------------------------------------------------------------------------- |
| **Line Editor**      | `reedline`-based REPL with async Git prompt, syntax highlighting, and history suggestions.         |
| **Execution Engine** | Parses and dispatches commands with reliable I/O capture via PTY sessions.                         |
| **Black Box**        | Storage engine for all terminal memory. Hybrid architecture of SQLite and compressed blob storage. |
| **AI Brain**         | Classifies intent (natural language vs. command) and drives a context-aware autonomous agent loop. |

## 👩‍💻 Development

### Git Hooks

For safe development, we provide pre-push hooks.

```bash
make install-hooks   # Install hooks
make uninstall-hooks # Remove hooks
```

### Code Verification (Local CI)

```bash
make check  # Run format, clippy, check, and test in one go
```

### CI Pipeline (GitHub Actions)

The following CI runs on every push and PR to `main`:

- `cargo check --all-targets`
- `cargo test --all-targets`
- `cargo fmt --all -- --check`
- `cargo clippy --all-targets -- -D warnings`
