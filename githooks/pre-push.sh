#!/usr/bin/env bash
set -uo pipefail

# ── Colors ───────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BOLD='\033[1m'
RESET='\033[0m'

# ── Options ───────────────────────────────────────────────
NO_HOOK=false
FIX=false
for arg in "$@"; do
    case "$arg" in
        --no-hook) NO_HOOK=true ;;
        --fix)     FIX=true ;;
    esac
done

# ── Result tracking ───────────────────────────────────────
CHECK_NAMES=()
CHECK_RESULTS=()
FAIL_COUNT=0
CURRENT_STEP=""

# ── Helpers ──────────────────────────────────────────────
step() {
    CURRENT_STEP="$1"
    echo -e "${YELLOW}${BOLD}▶ $1${RESET}"
}

pass() {
    echo -e "${GREEN}  ✔ $1${RESET}"
    CHECK_NAMES+=("$CURRENT_STEP")
    CHECK_RESULTS+=("pass")
}

fail() {
    echo -e "${RED}  ✖ $1${RESET}"
    CHECK_NAMES+=("$CURRENT_STEP")
    CHECK_RESULTS+=("fail")
    FAIL_COUNT=$((FAIL_COUNT + 1))
}

# ── Pre-push checks (mirrors CI) ────────────────────────
echo ""
if $NO_HOOK; then
    echo -e "${BOLD}Running local checks${RESET}"
else
    echo -e "${BOLD}Running pre-push checks${RESET}"
fi
echo ""

# 1. Format
if $FIX; then
    step "cargo fmt (auto-fix)"
    if cargo fmt --all; then
        pass "Format applied"
    else
        fail "cargo fmt failed."
    fi
else
    step "cargo fmt --check"
    if cargo fmt --all -- --check; then
        pass "Format OK"
    else
        fail "Format check failed. Run 'cargo fmt' to fix."
    fi
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

# ── Summary ──────────────────────────────────────────────
echo ""
if [ "$FAIL_COUNT" -eq 0 ]; then
    echo -e "${GREEN}${BOLD}Summary: All checks passed${RESET}"
else
    echo -e "${RED}${BOLD}Summary: ${FAIL_COUNT} check(s) failed${RESET}"
fi
echo ""
for i in "${!CHECK_NAMES[@]}"; do
    if [ "${CHECK_RESULTS[$i]}" = "pass" ]; then
        echo -e "  ${CHECK_NAMES[$i]} : ${GREEN}✔ done${RESET}"
    else
        echo -e "  ${CHECK_NAMES[$i]} : ${RED}✖ error${RESET}"
    fi
done
echo ""

if [ "$FAIL_COUNT" -gt 0 ]; then
    if $NO_HOOK; then
        echo -e "${RED}${BOLD}Check failed.${RESET}"
    else
        echo -e "${RED}${BOLD}Push aborted.${RESET}"
    fi
    exit 1
fi

if ! $NO_HOOK; then
    echo -e "${GREEN}${BOLD}Ready to push!${RESET}"
    echo ""
fi
