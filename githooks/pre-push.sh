#!/usr/bin/env bash
set -euo pipefail

# ── Colors ───────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BOLD='\033[1m'
RESET='\033[0m'

# ── Helpers ──────────────────────────────────────────────
step() {
    echo -e "${YELLOW}${BOLD}▶ $1${RESET}"
}

pass() {
    echo -e "${GREEN}  ✔ $1${RESET}"
}

fail() {
    echo -e "${RED}  ✖ $1${RESET}"
    echo -e "${RED}${BOLD}Push aborted.${RESET}"
    exit 1
}

# ── Pre-push checks (mirrors CI) ────────────────────────
echo ""
echo -e "${BOLD}Running pre-push checks …${RESET}"
echo ""

# 1. Format
step "cargo fmt --check"
if cargo fmt --all -- --check; then
    pass "Format OK"
else
    fail "Format check failed. Run 'cargo fmt' to fix."
fi

# 2. Check (compile)
step "cargo check"
if RUSTFLAGS="-Dwarnings" cargo check --all-targets; then
    pass "Check OK"
else
    fail "cargo check failed."
fi

# 3. Clippy (lint)
step "cargo clippy"
if cargo clippy --all-targets -- -D warnings; then
    pass "Clippy OK"
else
    fail "Clippy found issues."
fi

# 4. Test
step "cargo test"
if cargo test --all-targets; then
    pass "Tests OK"
else
    fail "Tests failed."
fi

echo ""
echo -e "${GREEN}${BOLD}All checks passed. Pushing …${RESET}"
echo ""
