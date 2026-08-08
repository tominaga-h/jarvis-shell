use std::fs::OpenOptions;
use std::io::{self, Write as _};
use std::path::Path;

use tracing::{debug, info, warn};

use super::resolution::is_symlink;

/// rc.jsh の初回自動生成テンプレート。
pub(in crate::shell) const TEMPLATE: &str = r#"# jarvish rc.jsh — startup script
#
# This file runs once, every time jarvish starts interactively — before
# the [startup].commands section of config.toml, and before the first
# prompt is shown. One command per line; blank lines and lines whose
# first non-whitespace character is '#' are skipped. There is no line
# continuation syntax — keep each command on a single line.
#
# IMPORTANT: every line here is executed through the same builtin path
# as typing it at the prompt (alias / export / complete / cd / source /
# ...), but it NEVER goes through the AI natural-language classifier.
# A line that would normally be routed to the AI assistant is instead
# run as a plain command and will simply fail as "command not found" —
# this file is for deterministic setup, not conversation.
#
# ── alias: define a shorthand for a command ──────────────────────────
# alias gs="git status"
#
# ── export: set an environment variable (expands $VARS) ─────────────
# export EDITOR="nvim"
#
# ── complete: register a fish-style completion for your own command ──
# (see the "Custom Completions" section of the README for the full
# flag reference: -c/-s/-l/-a/-d/-n)
# complete -c mycmd -s v -l verbose -d 'Verbose output'
#
# A failing line prints its error and line number but does NOT stop the
# rest of the script — every remaining line still runs.
"#;

/// rc.jsh が存在しなければコメントのみのテンプレートを生成する。
pub(in crate::shell) fn ensure_default_rc(path: &Path) {
    if path.exists() {
        return;
    }

    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            warn!(path = %parent.display(), error = %e, "Failed to create rc.jsh directory");
            eprintln!("jarvish: warning: failed to create rc.jsh directory: {e}");
            return;
        }

        match is_symlink(parent) {
            Ok(true) => {
                warn!(
                    path = %parent.display(),
                    "rc.jsh: refusing to bootstrap because the parent directory is a symlink \
                     (possible symlink attack) — startup continues without rc.jsh"
                );
                return;
            }
            Ok(false) => {}
            Err(e) => {
                warn!(path = %parent.display(), error = %e, "Failed to stat rc.jsh parent directory");
                eprintln!("jarvish: warning: failed to stat rc.jsh parent directory: {e}");
                return;
            }
        }
    }

    match is_symlink(path) {
        Ok(true) => {
            warn!(
                path = %path.display(),
                "rc.jsh: refusing to bootstrap because the path is a symlink \
                 (possible dangling-symlink attack) — startup continues without rc.jsh"
            );
            return;
        }
        Ok(false) => {}
        Err(e) => {
            warn!(path = %path.display(), error = %e, "Failed to stat rc.jsh path");
            eprintln!("jarvish: warning: failed to stat rc.jsh path: {e}");
            return;
        }
    }

    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .and_then(|mut f| f.write_all(TEMPLATE.as_bytes()))
    {
        Ok(()) => {
            info!(path = %path.display(), "Created default rc.jsh file");
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            debug!(path = %path.display(), "rc.jsh already exists at write time, skipping bootstrap");
        }
        Err(e) => {
            warn!(path = %path.display(), error = %e, "Failed to create default rc.jsh file");
            eprintln!("jarvish: warning: failed to create rc.jsh file: {e}");
        }
    }
}
