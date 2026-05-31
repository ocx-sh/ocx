#!/usr/bin/env bash
# run-windows-activation.sh - drive the Windows shell-activation gate from WSL.
#
# Cross-builds nothing. Expects the x86_64 Windows release binary at
# target/xwin/x86_64-pc-windows-msvc/release/ocx.exe (run `task
# rust:build:windows:x86`), then runs test/manual/test-windows-activation.ps1
# under each Windows PowerShell interpreter reachable through WSL interop:
#   - Windows PowerShell 5.1  (powershell.exe; the Win10/11 default)
#   - PowerShell 7+           (pwsh.exe; optional, skipped with a notice if absent)
#
# WSL-on-Windows convenience only: needs /mnt/c + .exe interop. On a plain
# Linux/CI host with no Windows interpreter this exits 0 as a skip (CI covers
# Windows natively in verify-deep.yml on a windows-latest runner). Override:
#   OCX_WIN_EXE  path to the built ocx.exe (default: target/xwin/.../release/ocx.exe)
#   OCX_WINPS    path to Windows PowerShell 5.1 (powershell.exe)
#   OCX_PWSH     path to PowerShell 7 (pwsh.exe)
set -euo pipefail
IFS=$'\n\t'

REPO_ROOT=$(git rev-parse --show-toplevel)
EXE_UNIX="${OCX_WIN_EXE:-$REPO_ROOT/target/xwin/x86_64-pc-windows-msvc/release/ocx.exe}"
GATE_UNIX="$REPO_ROOT/test/manual/test-windows-activation.ps1"
INSTALL_UNIX="$REPO_ROOT/website/src/public/install.ps1"

log() { printf 'run-windows-activation: %s\n' "$1" >&2; }

# Echo the first executable path from the argument list, or return non-zero.
first_existing() {
    local _candidate
    for _candidate in "$@"; do
        if [ -n "$_candidate" ] && [ -x "$_candidate" ]; then
            printf '%s' "$_candidate"
            return 0
        fi
    done
    return 1
}

# Run the gate harness under one interpreter. $1 = interpreter, $2 = label.
run_gate() {
    local _interp="$1" _label="$2" _exe_win _gate_win _install_win
    _exe_win=$(wslpath -w "$EXE_UNIX")
    _gate_win=$(wslpath -w "$GATE_UNIX")
    _install_win=$(wslpath -w "$INSTALL_UNIX")
    log "== $_label =="
    "$_interp" -NoProfile -ExecutionPolicy Bypass -File "$_gate_win" \
        -OcxBin "$_exe_win" -InstallPs1 "$_install_win"
}

main() {
    if ! command -v wslpath >/dev/null 2>&1; then
        log "wslpath not found - not running under WSL; skipping (CI covers Windows natively)."
        exit 0
    fi
    if [ ! -f "$EXE_UNIX" ]; then
        log "missing $EXE_UNIX"
        log "build it first: task rust:build:windows:x86"
        exit 1
    fi

    local _winps _pwsh _ran=0 _rc=0
    _winps=$(first_existing \
        "${OCX_WINPS:-}" \
        "/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe") || _winps=""
    _pwsh=$(first_existing \
        "${OCX_PWSH:-}" \
        "/mnt/c/Program Files/PowerShell/7/pwsh.exe" \
        "/mnt/c/Program Files/PowerShell/7-preview/pwsh.exe") || _pwsh=""

    if [ -n "$_winps" ]; then
        _ran=1
        run_gate "$_winps" "Windows PowerShell 5.1" || _rc=$?
    else
        log "Windows PowerShell 5.1 not found (set OCX_WINPS) - skipped."
    fi

    if [ -n "$_pwsh" ]; then
        _ran=1
        run_gate "$_pwsh" "PowerShell 7" || _rc=$?
    else
        log "PowerShell 7 (pwsh.exe) not installed on the Windows host (set OCX_PWSH) - skipped."
    fi

    if [ "$_ran" -eq 0 ]; then
        log "no Windows interpreter reachable via interop; nothing to run."
        exit 0
    fi
    if [ "$_rc" -ne 0 ]; then
        log "Windows activation gate FAILED (rc=$_rc)."
        exit "$_rc"
    fi
    log "Windows activation gate passed on all reachable interpreters."
}

main "$@"
