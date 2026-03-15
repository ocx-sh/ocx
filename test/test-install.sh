#!/usr/bin/env bash
# test-install.sh — Test the OCX install script across Linux distributions
#
# Runs the install script in various Docker containers to verify it works
# on different Linux environments. Requires Docker.
#
# Usage:
#   ./test/test-install.sh
#   ./test/test-install.sh ubuntu       # run only one image
#   ./test/test-install.sh --local      # test local install.sh instead of remote

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BOLD='\033[1m'
NC='\033[0m'

PASSED=0
FAILED=0
SKIPPED=0
FAILURES=()

# Default: fetch from https://ocx.sh/install.sh
USE_LOCAL=false
FILTER=""

for arg in "$@"; do
    case "$arg" in
        --local) USE_LOCAL=true ;;
        --help|-h)
            echo "Usage: $0 [--local] [image-filter]"
            echo "  --local    Mount local website/src/public/install.sh instead of fetching from ocx.sh"
            echo "  <filter>   Only run images whose name contains this string"
            exit 0
            ;;
        *) FILTER="$arg" ;;
    esac
done

# --- Test runner ---

run_test() {
    local name="$1" image="$2" setup="$3" shell_cmd="$4"
    local docker_args=()

    if [[ -n "$FILTER" ]] && [[ "$name" != *"$FILTER"* ]]; then
        printf '%bSKIP%b %s\n' "$YELLOW" "$NC" "$name"
        ((SKIPPED++))
        return
    fi

    printf '%bTEST%b %-40s ' "$BOLD" "$NC" "$name"

    # Mount local install.sh if requested
    if $USE_LOCAL; then
        docker_args+=(-v "$REPO_ROOT/website/src/public/install.sh:/tmp/install.sh:ro")
    fi

    # Build the test script to run inside the container
    local test_script
    test_script=$(cat <<INNEREOF
set -e

# Setup phase (install deps)
$setup

# Fetch install script
if [ -f /tmp/install.sh ]; then
    cp /tmp/install.sh /tmp/ocx-install.sh
else
    $shell_cmd -c '
        if command -v curl >/dev/null 2>&1; then
            curl -fsSL https://ocx.sh/install.sh -o /tmp/ocx-install.sh
        elif command -v wget >/dev/null 2>&1; then
            wget -qO /tmp/ocx-install.sh https://ocx.sh/install.sh
        else
            echo "ERROR: neither curl nor wget available"
            exit 1
        fi
    '
fi

chmod +x /tmp/ocx-install.sh

# Run the install script
echo "=== Running install.sh ==="
$shell_cmd /tmp/ocx-install.sh
install_exit=\$?

if [ \$install_exit -ne 0 ]; then
    echo "FAIL: install.sh exited with code \$install_exit"
    exit 1
fi

# Verify installation
echo "=== Verifying installation ==="

# Check env file exists
if [ ! -f "\$HOME/.ocx/env" ]; then
    echo "FAIL: ~/.ocx/env not created"
    exit 1
fi
echo "OK: ~/.ocx/env exists"

# Check binary is accessible via the current symlink
OCX_BIN="\$HOME/.ocx/installs/ocx.sh/ocx/current/bin/ocx"
if [ ! -x "\$OCX_BIN" ]; then
    # Check if candidate exists but current doesn't
    if ls "\$HOME/.ocx/installs/ocx.sh/ocx/candidates/" 2>/dev/null; then
        echo "FAIL: candidates exist but current symlink is missing"
    else
        echo "FAIL: no installation found at \$OCX_BIN"
    fi
    # Debug: show what's actually there
    echo "DEBUG: OCX home contents:"
    find "\$HOME/.ocx" -type f -o -type l 2>/dev/null | head -20 || true
    exit 1
fi
echo "OK: binary exists at \$OCX_BIN"

# Check version output
VERSION=\$("\$OCX_BIN" version 2>&1) || true
echo "OK: ocx version = \$VERSION"

# Check candidate symlink
if ls "\$HOME/.ocx/installs/ocx.sh/ocx/candidates/" >/dev/null 2>&1; then
    echo "OK: candidate symlinks exist:"
    ls -la "\$HOME/.ocx/installs/ocx.sh/ocx/candidates/"
else
    echo "WARN: no candidate symlinks found"
fi

# Check current symlink
if [ -L "\$HOME/.ocx/installs/ocx.sh/ocx/current" ]; then
    echo "OK: current symlink -> \$(readlink "\$HOME/.ocx/installs/ocx.sh/ocx/current")"
else
    echo "WARN: current is not a symlink"
fi

echo "=== All checks passed ==="
INNEREOF
)

    # Run in Docker
    local output
    if output=$(docker run --rm \
        --platform linux/amd64 \
        "${docker_args[@]}" \
        "$image" \
        /bin/sh -c "$test_script" 2>&1); then
        printf '%bPASS%b\n' "$GREEN" "$NC"
        ((PASSED++))
    else
        printf '%bFAIL%b\n' "$RED" "$NC"
        FAILURES+=("$name")
        ((FAILED++))
        # Print output indented
        printf '%s\n' "$output" | sed 's/^/    /'
        echo ""
    fi
}

# --- Test matrix ---

echo ""
echo "============================================"
echo "  OCX Install Script Test Suite"
echo "============================================"
echo ""
if $USE_LOCAL; then
    echo "Mode: LOCAL (mounting website/src/public/install.sh)"
else
    echo "Mode: REMOTE (fetching from https://ocx.sh/install.sh)"
fi
echo ""

# Ubuntu 24.04 (glibc, bash, curl pre-installed)
run_test "ubuntu-24.04" "ubuntu:24.04" \
    "apt-get update -qq && apt-get install -y -qq curl xz-utils >/dev/null 2>&1" \
    "sh"

# Ubuntu 22.04 (older glibc)
run_test "ubuntu-22.04" "ubuntu:22.04" \
    "apt-get update -qq && apt-get install -y -qq curl xz-utils >/dev/null 2>&1" \
    "sh"

# Debian 12 (stable)
run_test "debian-12" "debian:12" \
    "apt-get update -qq && apt-get install -y -qq curl xz-utils >/dev/null 2>&1" \
    "sh"

# Debian 12 with wget only (no curl)
run_test "debian-12-wget" "debian:12" \
    "apt-get update -qq && apt-get install -y -qq wget xz-utils >/dev/null 2>&1" \
    "sh"

# Alpine (musl libc)
run_test "alpine-3.20" "alpine:3.20" \
    "apk add --no-cache curl xz >/dev/null 2>&1" \
    "sh"

# Alpine with wget only
run_test "alpine-3.20-wget" "alpine:3.20" \
    "apk add --no-cache wget xz >/dev/null 2>&1" \
    "sh"

# Fedora (glibc, dnf)
run_test "fedora-41" "fedora:41" \
    "dnf install -y -q curl xz tar >/dev/null 2>&1" \
    "sh"

# Arch Linux
run_test "archlinux" "archlinux:latest" \
    "pacman -Sy --noconfirm curl xz tar >/dev/null 2>&1" \
    "sh"

# Amazon Linux 2023 (common CI environment)
run_test "amazonlinux-2023" "amazonlinux:2023" \
    "dnf install -y -q curl xz tar >/dev/null 2>&1" \
    "sh"

# Rocky Linux 9 (RHEL clone)
run_test "rocky-9" "rockylinux:9" \
    "dnf install -y -q curl xz tar >/dev/null 2>&1" \
    "sh"

# Minimal: Ubuntu with no extras (test that error messages are helpful)
run_test "ubuntu-minimal" "ubuntu:24.04" \
    "apt-get update -qq && apt-get install -y -qq curl >/dev/null 2>&1" \
    "sh"

# Test --no-modify-path flag
run_test "ubuntu-no-modify-path" "ubuntu:24.04" \
    "apt-get update -qq && apt-get install -y -qq curl xz-utils >/dev/null 2>&1" \
    "sh -s -- --no-modify-path"

# --- Summary ---

echo ""
echo "============================================"
echo "  Results"
echo "============================================"
printf '  %bPassed:%b  %d\n' "$GREEN" "$NC" "$PASSED"
printf '  %bFailed:%b  %d\n' "$RED" "$NC" "$FAILED"
printf '  %bSkipped:%b %d\n' "$YELLOW" "$NC" "$SKIPPED"
echo ""

if [ ${#FAILURES[@]} -gt 0 ]; then
    echo "Failed tests:"
    for f in "${FAILURES[@]}"; do
        printf '  %b✗%b %s\n' "$RED" "$NC" "$f"
    done
    echo ""
    exit 1
fi

echo "All tests passed!"
