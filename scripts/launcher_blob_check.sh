#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
# Kind: CI
# bash-floor: 3.2
#
# Runs one committed launcher blob natively: lays out a minimal vendored directory by hand, starts the
# blob from it and checks exit status and output. `launcher-blobs.yml` runs it on each blob's own OS;
# `task rust:shim:blob:check` runs it on the host's. Usage: launcher_blob_check.sh BLOB TARGET
#
# Exit: 0 every case passed; 1 a case failed or fewer cases ran than the target owes; 64 usage;
# 66 the blob is missing, empty or not executable.

set -euo pipefail

usage() {
    printf 'usage: launcher_blob_check.sh BLOB TARGET\n' >&2
    exit 64
}

SELF_PATH=selections/default/shells/default/bin/tool
VENDOR_ID=sha256:0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a
EXE=
WORK=
RAN=0
FAILED=0

pass() {
    RAN=$((RAN + 1))
    printf 'ok   %s\n' "$1"
}

fail() {
    RAN=$((RAN + 1))
    FAILED=$((FAILED + 1))
    printf 'FAIL %s: %s\n' "$1" "$2"
    printf '  stdout: %s\n' "$(cat "$WORK/out")"
    printf '  stderr: %s\n' "$(cat "$WORK/err")"
}

# The sidecar the generator writes, with the fields every case shares; the program path is per case.
sidecar() {
    printf '{"kind":"direct","schema_version":1,"vendor_id":"%s","self":"%s","program":"@VENDOR_DIR@/%s","args":%s,"unset":[],"env":%s}' \
        "$VENDOR_ID" "$SELF_PATH" "$1" "$2" "$3"
}

# A fresh vendored directory holding the blob as its launcher; prints the launcher path.
vendor() {
    local root="$WORK/$1/vendor"
    mkdir -p "$root/$(dirname "$SELF_PATH")"
    cp -f "$BLOB" "$root/$SELF_PATH$EXE"
    printf '%s\n' "$root/$SELF_PATH$EXE"
}

# Runs the command line, recording stdout, stderr and the status in $STATUS; never stops the script.
run() {
    STATUS=0
    "$@" >"$WORK/out" 2>"$WORK/err" || STATUS=$?
}

expect() {
    local name=$1 want_status=$2 want_stderr=$3
    if [ "$STATUS" -ne "$want_status" ]; then
        fail "$name" "exit $STATUS, want $want_status"
    elif ! grep -qF -- "$want_stderr" "$WORK/err"; then
        fail "$name" "stderr does not contain \"$want_stderr\""
    else
        pass "$name"
    fi
}

case_no_sidecar() {
    local launcher
    launcher=$(vendor no-sidecar)
    run "$launcher"
    expect no-sidecar 78 "sidecar not found"
}

case_malformed_sidecar() {
    local launcher
    launcher=$(vendor malformed)
    printf '{' >"${launcher%"$EXE"}.launch"
    run "$launcher"
    expect malformed-sidecar 78 "malformed sidecar"
}

# The child is a second copy of the blob with no sidecar: its refusal names its own path, which the
# parent's never does, so exit 78 with that name proves the parent found its root and started the child.
case_direct_starts_child() {
    local launcher root child=packages/p/content/bin/probe-child$EXE
    launcher=$(vendor direct-child)
    root=$WORK/direct-child/vendor
    mkdir -p "$root/$(dirname "$child")"
    cp -f "$BLOB" "$root/$child"
    sidecar "$child" '[]' '[]' >"${launcher%"$EXE"}.launch"
    run "$launcher"
    expect direct-starts-child 78 "probe-child"
}

# POSIX only: baked arguments, caller arguments, an expanded environment value and the exit status.
case_direct_args_env_exit() {
    local launcher root program=packages/p/content/bin/probe.sh
    launcher=$(vendor direct-args)
    root=$WORK/direct-args/vendor
    mkdir -p "$root/$(dirname "$program")"
    # shellcheck disable=SC2016 # the probe script expands $PROBE_VALUE and $@ itself, not this shell
    printf '#!/bin/sh\nprintf "%%s\\n" "$PROBE_VALUE" "$@"\nexit 7\n' >"$root/$program"
    chmod +x "$root/$program"
    sidecar "$program" '["--baked"]' '[{"key":"PROBE_VALUE","value":"@VENDOR_DIR@/data","kind":"constant"}]' \
        >"${launcher%"$EXE"}.launch"
    run "$launcher" "caller arg"
    if [ "$STATUS" -ne 7 ]; then
        fail direct-args-env-exit "exit $STATUS, want 7"
    elif [ "$(cat "$WORK/out")" != "$(printf '%s\n' "$root/data" --baked "caller arg")" ]; then
        fail direct-args-env-exit "stdout is not the expanded value, the baked and the caller argument"
    else
        pass direct-args-env-exit
    fi
}

# Windows only: a classic `.shim` launcher starts the pinned ocx, here the sidecar-less child copy.
case_classic_pinned_ocx() {
    local bin=$WORK/classic/bin
    mkdir -p "$bin"
    cp -f "$BLOB" "$bin/tool.exe"
    cp -f "$BLOB" "$bin/probe-child.exe"
    printf '%s\n' "$(cygpath -w "$WORK/classic")" >"$bin/tool.shim"
    run env -u OCX_HOME OCX_BINARY_PIN="$(cygpath -w "$bin/probe-child.exe")" "$bin/tool.exe"
    expect classic-pinned-ocx 78 "probe-child"
}

# Apple silicon starts only a signed image; a changed byte must invalidate the signature.
case_signature() {
    : >"$WORK/out"
    if codesign --verify --strict "$BLOB" 2>"$WORK/err"; then
        pass signature-verifies
    else
        fail signature-verifies "codesign --verify rejects the committed blob"
    fi
    cp -f "$BLOB" "$WORK/flipped"
    printf '\377' | dd of="$WORK/flipped" bs=1 seek=4096 conv=notrunc 2>"$WORK/err"
    if codesign --verify --strict "$WORK/flipped" 2>"$WORK/err"; then
        fail signature-catches-a-changed-byte "codesign --verify accepts a copy with a changed byte"
    else
        pass signature-catches-a-changed-byte
    fi
}

main() {
    [ "$#" -eq 2 ] || usage
    BLOB=$1
    local target=$2 owed=4
    if [ ! -f "$BLOB" ] || [ ! -s "$BLOB" ] || [ ! -x "$BLOB" ]; then
        printf 'launcher_blob_check: no executable blob at %s\n' "$BLOB" >&2
        exit 66
    fi
    case "$target" in
        *-pc-windows-gnullvm) EXE=.exe ;;
        *-apple-darwin | *-unknown-linux-musl) ;;
        *) usage ;;
    esac
    WORK=$(mktemp -d)
    trap 'rm -rf -- "$WORK"' EXIT

    case_no_sidecar
    case_malformed_sidecar
    case_direct_starts_child
    if [ -n "$EXE" ]; then
        case_classic_pinned_ocx
    else
        case_direct_args_env_exit
    fi
    if [ "$target" = aarch64-apple-darwin ]; then
        owed=$((owed + 2))
        case_signature
    fi

    # The floor: a case list that shrank or a function that returned early reads as a smaller green.
    if [ "$FAILED" -ne 0 ] || [ "$RAN" -ne "$owed" ]; then
        printf 'launcher_blob_check: %s: %d of %d cases ran, %d failed\n' "$target" "$RAN" "$owed" "$FAILED" >&2
        exit 1
    fi
    printf 'launcher_blob_check: %s: %d cases passed\n' "$target" "$RAN"
}

main "$@"
