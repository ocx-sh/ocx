#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
# Kind: TOOL
# bash-floor: 3.2
#
# Run the acceptance suite, or the Rust unit-test bundle, on the Windows host
# from WSL: `task test:windows-local MODE=accept|unit`.
#
#   windows_local.sh --mode accept|unit [--shard-count N --shard-index I] [--exe PATH] [-- ARGS...]
#
# accept: ARGS go to pytest through `task test:accept:native`; --exe replaces the
#         cross-built ocx.exe (a release build, say) and its siblings
#         ocx-shim.exe and schema/ocx_schema.exe are taken from beside it.
# unit:   ARGS is at most one substring that selects the bundle's targets by label.
#
# Exit: the child's status; 64 usage; 66 --exe, or ocx-shim.exe / schema/ocx_schema.exe beside it, missing;
# 69 not WSL with a cmd.exe; 77 a staging directory that is a link or resolves outside ocx-wintest;
# 2 a shard request accept_shard.py refuses.
# WINDOWS_LOCAL_CMD_EXE names a different cmd.exe (the proof that 69 is reachable); WINDOWS_LOCAL_DRIVE_ROOT the
# directory it starts in (default /mnt/c). Both exist for scripts/tests/test_windows_local.py's stubs.

set -euo pipefail

TRIPLE=x86_64-pc-windows-gnullvm
# The suite starts its own zot registries on free ports. The registry:2 mirror is Docker
# only, and WSL forwards its own localhost:5001 to Windows, so the default port would
# reach WSL's compose stack; a dedicated one is reachable by nothing.
PORT_MIRROR=15001
GIT_USR_BIN='C:\Program Files\Git\usr\bin'

die() {
    local status=$1
    shift
    printf 'windows_local: %s\n' "$*" >&2
    exit "$status"
}

# Sets QUOTED to "$1" for a line of a batch file. cmd.exe has no escape for a `"` inside quotes and a line break
# ends the line, so either is refused; `%` expands even inside quotes, so it is doubled.
batch_quote() {
    case $1 in
        *\"* | *$'\n'* | *$'\r'*) die 64 "'$1' holds a double quote or a line break, which cmd.exe cannot carry" ;;
    esac
    QUOTED=\"${1//\%/%%}\"
}

mode='' shard_count='' shard_index='' exe=''
while [ $# -gt 0 ]; do
    case $1 in
        --mode | --shard-count | --shard-index | --exe)
            [ $# -ge 2 ] || die 64 "$1 needs a value"
            case $1 in
                --mode) mode=$2 ;;
                --shard-count) shard_count=$2 ;;
                --shard-index) shard_index=$2 ;;
                --exe) exe=$2 ;;
            esac
            shift 2
            ;;
        --)
            shift
            break
            ;;
        *) die 64 "unknown argument '$1'" ;;
    esac
done
args=''
for a in "$@"; do
    batch_quote "$a"
    args="$args $QUOTED"
done

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)

case $mode in
    accept | unit) ;;
    *) die 64 "--mode is accept or unit, got '$mode'" ;;
esac
if [ "$mode" = unit ]; then
    [ -z "$exe" ] || die 64 "--exe applies to --mode accept only"
    [ $# -le 1 ] || die 64 "--mode unit takes at most one filter substring"
    # A test name holds none of these, and refusing them keeps shell syntax out of every layer the filter crosses.
    case ${1:-} in
        *'$'* | *'`'* | *\\*) die 64 "the filter '$1' holds a \$, a backtick or a backslash; use a plain substring of the test name" ;;
    esac
    [ -z "$shard_count$shard_index" ] || die 64 "--shard-count and --shard-index apply to --mode accept only"
fi
# The selector is the authority on what a shard request means (exit 2), and it answers
# before any build is paid for.
if [ -n "$shard_count$shard_index" ]; then
    select_flags=()
    [ -z "$shard_count" ] || select_flags+=(--shard-count "$shard_count")
    [ -z "$shard_index" ] || select_flags+=(--shard-index "$shard_index")
    status=0
    python3 "$ROOT/scripts/accept_shard.py" --format stems "${select_flags[@]}" >/dev/null || status=$?
    [ "$status" -eq 0 ] || exit "$status"
fi
# The set comes from one build: a missing sibling would leave the previous run's copy beside the new ocx.exe.
require_siblings() {
    local f
    for f in ocx-shim.exe schema/ocx_schema.exe; do
        [ -f "$1/$f" ] || die 66 "$1/$f is missing; stage the whole binary set from one build"
    done
}
[ -z "$exe" ] || [ -f "$exe" ] || die 66 "--exe '$exe' is not a file"
[ -z "$exe" ] || require_siblings "$(dirname "$exe")"

CMD_EXE=${WINDOWS_LOCAL_CMD_EXE:-$(command -v cmd.exe || echo /mnt/c/Windows/System32/cmd.exe)}
{ [ -n "${WSL_DISTRO_NAME:-}" ] || grep -qi microsoft /proc/version 2>/dev/null; } && [ -x "$CMD_EXE" ] ||
    die 69 "needs WSL with a Windows cmd.exe (looked for $CMD_EXE)"

TASK=${TASK_EXE:-task}
NAME=$(basename "$ROOT")

# cmd.exe starts in a drive directory: from a \\wsl$ cwd it prints a warning and
# falls back to the Windows directory.
win_profile=$(cd "${WINDOWS_LOCAL_DRIVE_ROOT:-/mnt/c}" && "$CMD_EXE" /c 'echo %USERPROFILE%' 2>/dev/null | tr -d '\r') ||
    die 69 "cmd.exe did not answer"
[ -n "$win_profile" ] || die 69 "cmd.exe reported no %USERPROFILE%"
base_wsl="$(wslpath -u "$win_profile")/ocx-wintest"
stage_wsl="$base_wsl/$NAME"
stage_win="$win_profile\\ocx-wintest\\$NAME"
# Owned by this script: the python3 venv, the env file, pytest's scratch. Never in the
# stage (the sweep below deletes whatever git does not list) and never beside another
# tool's content under ocx-wintest. Short names on purpose: the index paths under pytest's
# basetemp reach MAX_PATH, and ocx.exe fails there with "cannot find the path" (os error 3).
own_wsl="$base_wsl/_wl"
own_win="$win_profile\\ocx-wintest\\_wl"
env_win="$own_win\\$NAME-env.cmd"

# Runs the command line $1 under cmd.exe from the stage, CRs stripped, the child's status kept
# (`task -x` hands pytest's own exit code through; plain `task` turns any failure into 201).
# Through a batch file: WSL interop escapes every double quote in an argument with a backslash,
# so a quoted path in `cmd.exe /c "..."` arrives as \"path\" and cmd.exe cannot find it.
win() {
    {
        printf '%s\r\n' '@echo off' "call \"$env_win\"" "cd /d \"$stage_win\"" "$1" 'exit /b %ERRORLEVEL%'
    } >"$own_wsl/$NAME-step.cmd"
    (cd "$stage_wsl" && "$CMD_EXE" /c "$own_win\\$NAME-step.cmd" 2>&1 </dev/null | tr -d '\r')
}

# 1. The binaries (or the bundle) come from the existing cross tasks.
bin_src=$ROOT/target/cross/$TRIPLE/test-bin
if [ "$mode" = accept ]; then
    if [ -n "$exe" ]; then
        bin_src=$(dirname "$exe")
    else
        (cd "$ROOT" && "$TASK" cross:test:bin TARGET="$TRIPLE")
        require_siblings "$bin_src"
    fi
    # Several modules read the generated metadata schema straight off disk.
    (cd "$ROOT" && "$TASK" schema:generate)
else
    (cd "$ROOT" && "$TASK" cross:test:bundle TARGET="$TRIPLE")
fi

# 2. The working tree on C:. Windows processes cannot use a \\wsl$ cwd. git decides what
# the tree is; rsync cannot both take a file list and delete, so files git no longer
# lists are swept first. Kept across runs: the venv, caches, test/bin, target/.
mkdir -p "$stage_wsl" "$own_wsl"
# The sweep below deletes whatever git does not list, so a stage that resolves anywhere but
# <profile>/ocx-wintest/<name> (a link, a junction WSL reports as one) would delete from where it points.
profile_real=$(cd "$(wslpath -u "$win_profile")" && pwd -P)
for dir in "$stage_wsl" "$own_wsl"; do
    [ "$(cd "$dir" && pwd -P)" = "$profile_real/ocx-wintest/$(basename "$dir")" ] ||
        die 77 "$dir is a link or resolves outside $profile_real/ocx-wintest; remove it"
done
list=$(mktemp "${TMPDIR:-/tmp}/windows-local.XXXXXX")
have=$(mktemp "${TMPDIR:-/tmp}/windows-local.XXXXXX")
gone=$(mktemp "${TMPDIR:-/tmp}/windows-local.XXXXXX")
trap 'rm -f "$list" "$have" "$gone"' EXIT
# external/ is the three submodule checkouts; .agents/ holds other tasks' worktrees. `ls-files -c` still names a
# tracked file deleted from the working tree, which rsync exits 23 on, so the deleted ones are subtracted. Each git
# call is the head of its own pipeline: a failing git must stop the script, not empty $list and sweep the stage.
(cd "$ROOT" && git ls-files -d -z) | sort -z >"$gone"
(cd "$ROOT" && git ls-files -co --exclude-standard -z) | sort -zu | comm -z -23 - "$gone" |
    { grep -zv -e '^external/' -e '^\.agents/' || true; } >"$list"
if [ "$mode" = accept ] && [ -d "$ROOT/website/src/public/schemas" ]; then
    # Generated and gitignored, so git does not list it.
    (cd "$ROOT" && find website/src/public/schemas -type f -print0) >>"$list"
fi
(cd "$stage_wsl" && find . \( -name .venv -o -name __pycache__ -o -name .pytest_cache -o -name .ruff_cache \) -prune \
    -o -path ./test/bin -prune -o -path ./target -prune -o -path ./test/results -prune -o -type f -print0) |
    sed -z 's|^\./||' >"$have"
comm -z -23 <(sort -z "$have") <(sort -z "$list") | (cd "$stage_wsl" && xargs -0 -r rm -f --)
rsync -rt --from0 --files-from="$list" "$ROOT/" "$stage_wsl/"
if [ "$mode" = accept ]; then
    mkdir -p "$stage_wsl/test/bin/schema"
    cp -f "${exe:-$bin_src/ocx.exe}" "$stage_wsl/test/bin/ocx.exe"
    for f in ocx-shim.exe schema/ocx_schema.exe; do
        cp -f "$bin_src/$f" "$stage_wsl/test/bin/$f"
    done
fi

# 3. The environment every Windows command below runs under, as one file (CRLF).
# Git's usr\bin goes ahead of %PATH%, as acceptance.yml's $GITHUB_PATH line puts it: appended,
# a bare `bash` resolves to System32's WSL launcher instead of Git's, unlike on the CI runner.
{
    printf '%s\r\n' '@echo off'
    printf '%s\r\n' "set \"PATH=$own_win\\py\\Scripts;$GIT_USR_BIN;%PATH%\""
    printf '%s\r\n' 'if not defined HOME set "HOME=%USERPROFILE%"'
    printf '%s\r\n' 'set "PYTHONIOENCODING=utf-8"'
    # A host-global commit.gpgsign would break the fixtures' commits.
    printf '%s\r\n' 'set "GIT_CONFIG_COUNT=2"' 'set "GIT_CONFIG_KEY_0=commit.gpgsign"' 'set "GIT_CONFIG_VALUE_0=false"' \
        'set "GIT_CONFIG_KEY_1=tag.gpgsign"' 'set "GIT_CONFIG_VALUE_1=false"'
    # The Windows host has no Docker the suite can reach, as on the deep tier's Windows leg.
    printf '%s\r\n' 'set "SKIP_BUILD=true"' 'set "OCX_TEST_NO_DOCKER=1"' "set \"OCX_TEST_MIRROR_PORT=$PORT_MIRROR\""
    printf '%s\r\n' "set \"TEST_BASETEMP=$own_win\\p-$NAME\"" "set \"TEST_TMPDIR=$own_win\\t-$NAME\""
    # An EXE= that is release bytes lacks the __testing seams: the switch skips the marked tests.
    [ "${OCX_TEST_RELEASE_BINARY:-}" != 1 ] || printf '%s\r\n' 'set "OCX_TEST_RELEASE_BINARY=1"'
} >"$own_wsl/$NAME-env.cmd"

# 4. python3, date, tr, dirname and basename: what test/taskfile.yml shells out to and a
# Windows PATH lacks. Git's usr\bin has the coreutils; python3 is a copy of python.exe
# inside a venv this script owns, because a copy beside it loses the DLLs it loads and
# the one in the user's install is not ours to write. WindowsApps\python3.exe is the
# Store stub, so the venv leads the PATH.
if [ ! -f "$own_wsl/py/Scripts/python3.exe" ]; then
    win "uv venv \"$own_win\\py\" --python 3.14 --allow-existing"
    cp -f "$own_wsl/py/Scripts/python.exe" "$own_wsl/py/Scripts/python3.exe"
fi

# 5. The run. zot comes from ocx.lock through the staged ocx.exe; pytest starts it itself.
rc=0
if [ "$mode" = accept ]; then
    shard_vars=''
    [ -z "$shard_count" ] || shard_vars="$shard_vars SHARD_COUNT=$shard_count"
    [ -z "$shard_index" ] || shard_vars="$shard_vars SHARD_INDEX=$shard_index"
    win "test\\bin\\ocx.exe exec zot -- task -x test:accept:native$shard_vars -- $args" || rc=$?
    # The reports leave with the run: F2's limits are counted off them.
    mkdir -p "$ROOT/target/windows-local/junit"
    if [ -d "$stage_wsl/target/accept/junit" ]; then
        cp -f "$stage_wsl"/target/accept/junit/*.xml "$ROOT/target/windows-local/junit/" 2>/dev/null || true
    fi
else
    bundle_win="$own_win\\$NAME-unit-tests.tar.gz"
    cp -f "$ROOT/target/cross/$TRIPLE/unit-tests.tar.gz" "$own_wsl/$NAME-unit-tests.tar.gz"
    junit_win="$stage_win\\target\\windows-local\\unit-junit.xml"
    filter=''
    if [ -n "${1:-}" ]; then
        batch_quote "$1"
        filter=" FILTER=$QUOTED"
    fi
    win "task -x cross:test:run BUNDLE=\"$bundle_win\" JUNIT=\"$junit_win\"$filter" || rc=$?
    if [ -f "$stage_wsl/target/windows-local/unit-junit.xml" ]; then
        mkdir -p "$ROOT/target/windows-local"
        cp -f "$stage_wsl/target/windows-local/unit-junit.xml" "$ROOT/target/windows-local/"
    fi
fi
exit "$rc"
