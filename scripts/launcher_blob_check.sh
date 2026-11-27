#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
# Kind: CI
# bash-floor: 3.2
#
# Runs one committed launcher blob natively: lays out a minimal vendored directory by hand, starts the
# blob from it and checks exit status and output, for every record kind: `direct` (starts a child, bakes
# arguments and environment), the home kinds `package`, `deferred` and `toolchain` (a stub `ocx` prints
# the argv it was started with) and the refusals (no record, an old `.direct` sidecar, a malformed record).
# `launcher-blobs.yml` runs it on each blob's own OS; `task rust:shim:blob:check` runs it on the host's.
# Usage: launcher_blob_check.sh BLOB TARGET
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
STUB=
USER_ARG=
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

# A fresh vendored directory holding the blob as its launcher; prints the launcher path.
vendor() {
    local root="$WORK/$1/vendor"
    mkdir -p "$root/$(dirname "$SELF_PATH")"
    cp -f "$BLOB" "$root/$SELF_PATH$EXE"
    printf '%s\n' "$root/$SELF_PATH$EXE"
}

# Runs the command line, recording stdout, stderr and the status in $STATUS; never stops the script.
# The working directory is a scratch one: env falls back to sh on a mangled image and sh then writes
# files named after the image's bytes.
run() {
    STATUS=0
    (cd "$WORK/cwd" && "$@") >"$WORK/out" 2>"$WORK/err" || STATUS=$?
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

# The record the generator writes for kind `direct`, with the fields every case shares; the program path
# is per case.
sidecar() {
    printf '{"kind":"direct","schema_version":1,"vendor_id":"%s","self":"%s","program":"@VENDOR_DIR@/%s","args":%s,"unset":[],"env":%s}' \
        "$VENDOR_ID" "$SELF_PATH" "$1" "$2" "$3"
}

# A path as the OS under test spells it (Windows: C:\...), and as a JSON string body.
native() {
    if [ -n "$EXE" ]; then cygpath -w "$1"; else printf '%s' "$1"; fi
}

jstr() {
    native "$1" | sed 's/\\/\\\\/g'
}

# The record file sits beside the launcher, under the launcher's name minus .exe.
record_of() {
    printf '%s.launch' "${1%"$EXE"}"
}

# Starts a home-kind launcher with the stub `ocx` reachable only through the given PATH value, and with
# the variables that would change the program or skip the containment check removed.
start_home() {
    local path=$1 launcher=$2
    shift 2
    run env -u OCX_HOME -u OCX_BINARY_PIN -u __OCX_LAUNCHER_DEPTH PATH="$path" "$launcher" "$@"
}

# The stub prints one argument per line then the depth; Windows' echo cannot print the environment, so
# there the argv is checked space-joined and the depth is not checked.
stub_output_is() {
    local got want
    got=$(cat "$WORK/out")
    if [ -n "$EXE" ]; then
        want="$*"
    else
        want=$(printf '%s\n' "$@" depth=1)
    fi
    [ "$got" = "$want" ]
}

home_expect() {
    local name=$1
    shift
    if [ "$STATUS" -ne 0 ]; then
        fail "$name" "exit $STATUS, want 0"
    elif ! stub_output_is "$@"; then
        fail "$name" "stdout is not the stub's argv: $*"
    else
        pass "$name"
    fi
}

make_stub() {
    STUB=$WORK/stub
    mkdir -p "$STUB"
    if [ -n "$EXE" ]; then
        # No PATHEXT lookup: the launcher wants ocx.exe. Git for Windows' echo.exe prints its argv.
        cp -f /usr/bin/echo.exe "$STUB/ocx.exe"
    else
        # shellcheck disable=SC2016 # the stub expands $@ and the depth itself, not this shell
        printf '#!/bin/sh\nprintf "%%s\\n" "$@"\nprintf "depth=%%s\\n" "${__OCX_LAUNCHER_DEPTH-}"\n' >"$STUB/ocx"
        chmod +x "$STUB/ocx"
    fi
}

write_package_record() {
    printf '{"kind":"package","schema_version":1,"package_root":"%s"}' "$(jstr "$2")" >"$(record_of "$1")"
}

case_no_record() {
    local launcher
    launcher=$(vendor no-record)
    run "$launcher"
    expect no-record 78 "tool.launch"
}

# A first-run `tool.direct` sidecar is no record: the refusal names the file the launcher wants.
case_old_sidecar() {
    local launcher
    launcher=$(vendor old-sidecar)
    sidecar "$SELF_PATH" '[]' '[]' >"${launcher%"$EXE"}.direct"
    run "$launcher"
    expect old-sidecar 78 "tool.launch"
}

case_malformed_record() {
    local launcher
    launcher=$(vendor malformed)
    printf '{' >"$(record_of "$launcher")"
    run "$launcher"
    expect malformed-record 78 "malformed"
}

# The child is a second copy of the blob with no record: its refusal names its own path, which the
# parent's never does, so exit 78 with that name proves the parent found its root and started the child.
case_direct_starts_child() {
    local launcher root child=packages/p/content/bin/probe-child$EXE
    launcher=$(vendor direct-child)
    root=$WORK/direct-child/vendor
    mkdir -p "$root/$(dirname "$child")"
    cp -f "$BLOB" "$root/$child"
    sidecar "$child" '[]' '[]' >"$(record_of "$launcher")"
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
        >"$(record_of "$launcher")"
    run "$launcher" "caller arg"
    if [ "$STATUS" -ne 7 ]; then
        fail direct-args-env-exit "exit $STATUS, want 7"
    elif [ "$(cat "$WORK/out")" != "$(printf '%s\n' "$root/data" --baked "caller arg")" ]; then
        fail direct-args-env-exit "stdout is not the expanded value, the baked and the caller argument"
    else
        pass direct-args-env-exit
    fi
}

case_package() {
    local launcher root
    launcher=$(vendor package)
    root=$WORK/package/pkgroot
    mkdir -p "$root"
    write_package_record "$launcher" "$root"
    start_home "$STUB:$PATH" "$launcher" "$USER_ARG"
    home_expect package launcher exec "$(native "$root")" -- tool "$USER_ARG"
}

case_deferred() {
    local launcher id=ocx.sh/kitware/cmake:3.31.6@sha256:1f2e3d4c5b6a79881f2e3d4c5b6a79881f2e3d4c5b6a79881f2e3d4c5b6a7988
    launcher=$(vendor deferred)
    printf '{"kind":"deferred","schema_version":1,"identifier":"%s"}' "$id" >"$(record_of "$launcher")"
    start_home "$STUB:$PATH" "$launcher" "$USER_ARG"
    home_expect deferred launcher shim "$id" -- tool "$USER_ARG"
}

# Rung 2: the record names the stub; the stub's directory is not on PATH.
case_toolchain_project() {
    local launcher root
    launcher=$(vendor toolchain-project)
    root=$WORK/toolchain-project/app
    mkdir -p "$root"
    printf '{"kind":"toolchain","schema_version":1,"tier":"project","project_root":"%s","ocx":"%s"}' \
        "$(jstr "$root")" "$(jstr "$STUB/ocx$EXE")" >"$(record_of "$launcher")"
    start_home "$PATH" "$launcher" "$USER_ARG"
    home_expect toolchain-project --project "$(native "$root")" exec -- tool "$USER_ARG"
}

# Rung 3: no `ocx` in the record, so the launcher walks PATH and finds the stub.
case_toolchain_global() {
    local launcher
    launcher=$(vendor toolchain-global)
    printf '{"kind":"toolchain","schema_version":1,"tier":"global"}' >"$(record_of "$launcher")"
    start_home "$STUB:$PATH" "$launcher" "$USER_ARG"
    home_expect toolchain-global --global exec -- tool "$USER_ARG"
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

# V-L36: three copies of the signed launcher each start and verify; the quarantine start is reported only
# and counts toward no floor (it fails only when the attribute cannot be set), since Gatekeeper's verdict
# per inode is a claim.
case_signed_copies_start() {
    local i launcher root
    for i in 1 2 3; do
        launcher=$(vendor "signed-$i")
        root=$WORK/signed-$i/pkgroot
        mkdir -p "$root"
        write_package_record "$launcher" "$root"
        start_home "$STUB:$PATH" "$launcher" "$USER_ARG"
        if ! codesign --verify --strict "$launcher" 2>"$WORK/err"; then
            fail signed-copies-start "copy $i fails codesign --verify"
            return
        fi
        if [ "$STATUS" -ne 0 ] || ! stub_output_is launcher exec "$root" -- tool "$USER_ARG"; then
            fail signed-copies-start "copy $i did not start the stub (exit $STATUS)"
            return
        fi
    done
    pass signed-copies-start
}

case_quarantine_start() {
    local launcher root
    launcher=$(vendor quarantine)
    root=$WORK/quarantine/pkgroot
    mkdir -p "$root"
    write_package_record "$launcher" "$root"
    : >"$WORK/out"
    if ! xattr -w com.apple.quarantine '0081;00000000;ocx;' "$launcher" 2>"$WORK/err" ||
        ! xattr -w com.apple.quarantine '0081;00000000;ocx;' "$(dirname "$launcher")" 2>"$WORK/err"; then
        fail quarantine-start "xattr cannot set the quarantine attribute"
        return
    fi
    start_home "$STUB:$PATH" "$launcher" "$USER_ARG"
    # Reported, not counted: nothing here can fail on Gatekeeper's verdict, so it owes no case to the floor.
    printf 'info quarantine-start: exit %s\n' "$STATUS"
}

main() {
    [ "$#" -eq 2 ] || usage
    BLOB=$1
    local target=$2 owed=8
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
    mkdir "$WORK/cwd"

    USER_ARG='caller arg'
    if [ -n "$EXE" ]; then USER_ARG=uarg; fi
    make_stub

    case_no_record
    case_old_sidecar
    case_malformed_record
    case_direct_starts_child
    if [ -z "$EXE" ]; then
        owed=$((owed + 1))
        case_direct_args_env_exit
    fi
    case_package
    case_deferred
    case_toolchain_project
    case_toolchain_global
    if [ "$target" = aarch64-apple-darwin ]; then
        owed=$((owed + 3))
        case_signature
        case_signed_copies_start
        case_quarantine_start
    fi

    # The floor: a case list that shrank or a function that returned early reads as a smaller green.
    if [ "$FAILED" -ne 0 ] || [ "$RAN" -ne "$owed" ]; then
        printf 'launcher_blob_check: %s: %d of %d cases ran, %d failed\n' "$target" "$RAN" "$owed" "$FAILED" >&2
        exit 1
    fi
    printf 'launcher_blob_check: %s: %d cases passed\n' "$target" "$RAN"
}

main "$@"
