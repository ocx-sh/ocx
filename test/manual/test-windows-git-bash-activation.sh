#!/usr/bin/env bash
# test-windows-git-bash-activation.sh - bash/zsh activation gate (ocx#753).
#
# Usage: <bash|zsh> test/manual/test-windows-git-bash-activation.sh <path-to-ocx-binary>
#
# Counterpart of test-windows-activation.ps1 for POSIX shells. On Windows the
# binary is a native ocx.exe, run from Git Bash, MSYS2 or Cygwin bash/zsh; the
# stream must carry `C:/...` binary paths and POSIX PATH entries (`/c/...`, or
# `/proc/cygdrive/c/...` under Cygwin), never `C:\...`. Under Cygwin pass the
# script and the binary in mixed form (`D:/a/...`): Cygwin converts no arguments.
# Also passes on Linux/macOS, where it checks the same contract with no
# conversion in play.
#
# Written in the common subset of bash and zsh and run under both. It leaves
# off `set -e` and `pipefail` on purpose: every check runs and the script
# exits non-zero at the end if any failed. It never touches the real OCX_HOME
# or any rc file (--offline, --no-modify-path, --no-profile).

set -u

# Hermetic start: an inherited ocx session or project would redirect what the
# run reads and writes, and fail checks with no defect behind them.
for v in __OCX_ENV_STATE __OCX_ENV_PWD __OCX_ENV_STOOD_DOWN OCX_BINARY_PIN \
    OCX_CONSENT_PATHS OCX_CONSENT_NAMESPACES OCX_NO_CONSENT OCX_PROJECT OCX_NO_PROJECT \
    OCX_CEILING_PATH OCX_CONFIG OCX_NO_CONFIG OCX_MANAGED_CONFIG OCX_INDEX \
    OCX_DEFAULT_REGISTRY OCX_REMOTE OCX_OFFLINE OCX_FROZEN OCX_NO_COMPLETION \
    OCX_NO_HOOK OCX_NO_MODIFY_PATH OCX_TOOLCHAIN_ACTIVATE OCX_TOOLCHAIN_DIR \
    OCX_TOOLCHAIN_PINNED OCX_TOOLCHAIN_UPDATE OCX_PATCHES OCX_PATCH_SNAPSHOT \
    OCX_LAZY_MODE OCX_LOG_LEVEL OCX_LOG_CONSOLE; do
    unset "$v"
done

if [ $# -ne 1 ]; then
    printf 'usage: %s <path-to-ocx-binary>\n' "$0" >&2
    exit 2
fi

if [ -n "${ZSH_VERSION:-}" ]; then
    shell=zsh
elif [ -n "${BASH_VERSION:-}" ]; then
    shell=bash
else
    printf '%s\n' "run this script under bash or zsh" >&2
    exit 2
fi

case "$(uname -s)" in
    MINGW* | MSYS* | CYGWIN*)
        windows=1
        bin_name=ocx.exe
        ;;
    *)
        windows=0
        bin_name=ocx
        ;;
esac

cygwin=0
case "$(uname -s)" in
    CYGWIN*) cygwin=1 ;;
esac
# cmd.exe's switch: MSYS rewrites `//c` to `/c` for a native child, Cygwin passes it through.
cmd_c='//c'
[ "$cygwin" = 0 ] || cmd_c='/c'

# Absolute POSIX path of the binary: the cwd changes below. An argument spelled
# `D:/a/...` is only a directory to cygpath, not to cd on every runtime.
src_arg=$1
if [ "$windows" = 1 ]; then
    src_arg=$(cygpath -u "$1") || exit 2
fi
src_dir=$(cd "$(dirname -- "$src_arg")" && pwd) || exit 2
src_bin="$src_dir/$(basename -- "$src_arg")"
if [ ! -f "$src_bin" ]; then
    printf 'ocx binary not found: %s\n' "$src_bin" >&2
    exit 2
fi

tmp_base=${RUNNER_TEMP:-${TMPDIR:-/tmp}}
if [ "$windows" = 1 ]; then
    # RUNNER_TEMP is a native path; work in POSIX form.
    tmp_base=$(cygpath -u "$tmp_base")
fi
work=$(mktemp -d "$tmp_base/ocx-gitbash.XXXXXX") || exit 2
# The hook registration keeps a stamp file of its own and its EXIT trap defers to ours.
cleanup() {
    rm -rf -- "$work"
    [ -z "${__ocx_stamp-}" ] || rm -f -- "$__ocx_stamp"
}
trap cleanup EXIT

failures=0
passes=0
skips=0
pass() {
    printf '[PASS] %s\n' "$1"
    passes=$((passes + 1))
}
fail() {
    printf '[FAIL] %s\n' "$1"
    failures=$((failures + 1))
}
skip() {
    printf '[skip] %s\n' "$1"
    skips=$((skips + 1))
}

# bin_dir stays POSIX for the shell (cp, PATH checks). OCX_HOME reaches the native ocx.exe
# and cmd.exe unconverted under Cygwin, so on Windows it is the mixed form (`C:/...`), which
# all three runtimes accept as a path too.
home_dir=$work/home
export OCX_HOME=$home_dir
[ "$windows" = 0 ] || OCX_HOME=$(cygpath -m "$home_dir") || exit 2
bin_dir="$home_dir/symlinks/ocx.sh/ocx/cli/current/content/bin"
mkdir -p "$bin_dir" || exit 2
cp -f -- "$src_bin" "$bin_dir/$bin_name" || exit 2
ocx_bin="$bin_dir/$bin_name"
printf 'Running %s activation gate (uname: %s)\n' "$shell" "$(uname -s)"
printf '  ocx     : %s\n' "$ocx_bin"
printf '  OCX_HOME: %s\n' "$OCX_HOME"

"$ocx_bin" --offline self setup --no-modify-path --no-profile >"$work/setup.out" 2>&1 || {
    cat "$work/setup.out"
    printf '%s\n' "ocx self setup failed" >&2
    exit 2
}
if [ -f "$OCX_HOME/env.sh" ]; then
    printf '%s\n' "  env.sh written"
else
    fail "env.sh not written by 'ocx self setup'"
fi

# Whether PATH holds segment $1 exactly.
path_has() {
    case ":$PATH:" in
        *":$1:"*) return 0 ;;
    esac
    return 1
}

# Whether PATH holds the native directory $1 in the spelling the runtime asks
# for. Cygwin's fixed `/proc/cygdrive/<drive>/...` alias is built from the mixed
# form, so a changed cygdrive prefix in fstab cannot make the check pass or fail.
path_has_dir() {
    path_has "$(cygpath -u "$1")" && return 0
    [ "$cygwin" = 1 ] || return 1
    mixed=$(cygpath -m "$1")
    drive=$(printf '%s' "${mixed%%:*}" | tr '[:upper:]' '[:lower:]')
    path_has "/proc/cygdrive/$drive${mixed#*:}"
}

# Print every PATH segment that breaks the hygiene contract.
bad_segments() {
    rest=$PATH
    while [ -n "$rest" ]; do
        seg=${rest%%:*}
        case $rest in
            *:*) rest=${rest#*:} ;;
            *) rest= ;;
        esac
        case $seg in
            [A-Za-z] | \\*) printf '%s\n' "$seg" ;;
        esac
    done
}

# Print every segment of $1 (a PATH value) that is absent from the PATH
# recorded before activation and does not start with `/`.
non_absolute_added() {
    rest=$1
    while [ -n "$rest" ]; do
        seg=${rest%%:*}
        case $rest in
            *:*) rest=${rest#*:} ;;
            *) rest= ;;
        esac
        case ":$path_before:" in
            *":$seg:"*) continue ;;
        esac
        case $seg in
            /*) ;;
            *) printf '%s\n' "$seg" ;;
        esac
    done
}

# ---------------------------------------------------------------------------
# 1-6: activation stream, prompt hook, env.sh.
# ---------------------------------------------------------------------------
echo
echo "[activate] eval of the activation stream ..."
path_before=$PATH
# Baseline for check 8: a segment the runner already had is not ocx's to answer for.
native_before=
if [ "$windows" = 1 ] && command -v cmd.exe >/dev/null 2>&1; then
    native_before=$(cmd.exe "$cmd_c" "echo %PATH%" 2>&1 | tr -d '\r')
fi
# Flags of env.sh for an interactive shell; --interactive is what makes the stream
# register the prompt hook, so this non-interactive script gets it too.
eval "$("$ocx_bin" --offline self activate --shell="$shell" --no-completion --interactive 2>"$work/activate.err")"
if [ -s "$work/activate.err" ]; then
    fail "1 activation stream wrote to stderr:"
    cat "$work/activate.err"
else
    pass "1 activation stream wrote nothing to stderr"
fi

case "$(type ocx 2>&1)" in
    *function*) is_function=1 ;;
    *) is_function=0 ;;
esac
if [ "$is_function" = 1 ] && ocx --offline version >/dev/null 2>&1; then
    pass "2 'ocx' is a shell function and 'ocx version' exits 0"
else
    fail "2 'ocx' is not a working shell function: $(type ocx 2>&1)"
fi

added_bad=$(non_absolute_added "$PATH")
any_bad=$(bad_segments)
if [ -z "$added_bad" ] && [ -z "$any_bad" ]; then
    pass "3 PATH hygiene: added segments absolute, no drive-letter or backslash segment"
else
    fail "3 PATH hygiene; non-absolute added: [$added_bad] bad segments: [$any_bad]"
fi

if [ "$windows" = 1 ]; then
    want_dir=$(cygpath -u "$bin_dir")
else
    want_dir=$bin_dir
fi
# Both PATH checks below go through here so Linux and Windows share one spelling rule.
has_bin_dir() {
    if [ "$windows" = 1 ]; then path_has_dir "$bin_dir"; else path_has "$bin_dir"; fi
}
if has_bin_dir; then
    pass "4 seeded bin dir on PATH: $want_dir"
else
    fail "4 seeded bin dir missing from PATH: $want_dir"
fi

# The prompt hook the stream registered, run once in this session: nothing else
# executes its command word.
if typeset -f __ocx_prompt_hook >/dev/null 2>&1; then
    __ocx_prompt_hook 2>"$work/hook.err"
    hook_rc=$?
    if [ "$hook_rc" -eq 0 ] && [ ! -s "$work/hook.err" ]; then
        pass "5 prompt hook runs: status 0, stderr empty"
    else
        fail "5 prompt hook: status $hook_rc; stderr: $(cat "$work/hook.err")"
    fi
else
    fail "5 prompt hook function __ocx_prompt_hook not defined by the stream"
fi

# The shim must activate on its own. Restore the pre-activation PATH and drop the
# session state first, or the bin dir is still on PATH from the eval above and
# env.sh, which swallows its own activation errors, could not fail this check.
PATH=$path_before
unset __OCX_ENV_STATE
# shellcheck source=/dev/null
. "$OCX_HOME/env.sh"
if has_bin_dir; then
    pass "6 env.sh alone puts the bin dir on a reset PATH"
else
    fail "6 env.sh did not put the bin dir on a reset PATH: $want_dir"
fi

# ---------------------------------------------------------------------------
# 7: one reconcile apply and revert (S-045). The revert is the load-bearing
# half: an apply-only reconciler passes an apply-only assertion vacuously.
# ---------------------------------------------------------------------------
echo
echo "[S-045] reconcile apply/revert ..."
project_root="$work/reconcile"
project_dir="$project_root/alpha"
mkdir -p "$project_dir"
printf '%s\n' '[env]' 'WP18_CONST = "alpha"' >"$project_dir/ocx.toml"
# An empty tool set keeps this registry-free.
printf '%s\n' 'tool = []' '' '[metadata]' 'lock_version = 3' \
    'declaration_hash_version = 1' \
    'declaration_hash = "sha256:0000000000000000000000000000000000000000000000000000000000000000"' \
    'generated_by = "ocx acceptance fixture"' \
    'generated_at = "2026-01-01T00:00:00Z"' >"$project_dir/ocx.lock"

start_dir=$PWD
cd "$project_dir" || exit 2
# `ocx lock` stamps consent; without it the reconciler is inert by design.
if "$ocx_bin" --offline lock >/dev/null 2>"$work/lock.err"; then
    eval "$("$ocx_bin" --offline self activate --reconcile --shell="$shell" 2>"$work/apply.err")"
    if [ "${WP18_CONST-}" = alpha ] && [ ! -s "$work/apply.err" ]; then
        pass "7a reconcile apply: WP18_CONST=alpha, stderr empty"
    else
        fail "7a reconcile apply: WP18_CONST='${WP18_CONST-}'; stderr: $(cat "$work/apply.err")"
    fi
else
    fail "7a 'ocx lock' failed: $(cat "$work/lock.err")"
fi

cd "$project_root" || exit 2
eval "$("$ocx_bin" --offline self activate --reconcile --shell="$shell" 2>"$work/revert.err")"
if [ -z "${WP18_CONST+x}" ] && [ ! -s "$work/revert.err" ]; then
    pass "7b reconcile revert: WP18_CONST gone, stderr empty"
else
    fail "7b reconcile revert: WP18_CONST='${WP18_CONST-<unset>}'; stderr: $(cat "$work/revert.err")"
fi
cd "$start_dir" || exit 2

# ---------------------------------------------------------------------------
# 8: PATH as a native child sees it.
# ---------------------------------------------------------------------------
printf '\n[diag] final PATH: %s\n' "$PATH"
if [ "$windows" = 1 ] && ! { command -v cmd.exe >/dev/null 2>&1 && command -v cygpath >/dev/null 2>&1; }; then
    fail "8 native child PATH: cmd.exe or cygpath not found on a Windows runtime"
elif [ "$windows" = 1 ]; then
    native_path=$(cmd.exe "$cmd_c" "echo %PATH%" 2>&1 | tr -d '\r')
    printf '[diag] PATH seen by a native child: %s\n' "$native_path"
    want_native=$(cygpath -w "$bin_dir")
    lc_path=$(printf '%s' "$native_path" | tr '[:upper:]' '[:lower:]')
    lc_want=$(printf '%s' "$want_native" | tr '[:upper:]' '[:lower:]')
    bad_native=
    rest=${native_path%;}
    while [ -n "$rest" ]; do
        seg=${rest%%;*}
        case $rest in
            *\;*) rest=${rest#*;} ;;
            *) rest= ;;
        esac
        case $seg in
            [A-Za-z]:[\\/]* | \\\\*) continue ;;
        esac
        case ";$native_before;" in
            *";$seg;"*) ;;
            *) bad_native="$bad_native [$seg]" ;;
        esac
    done
    case $lc_path in
        *"$lc_want"*) native_has=1 ;;
        *) native_has=0 ;;
    esac
    if [ "$native_has" = 1 ] && [ -z "$bad_native" ]; then
        pass "8 native child PATH has $want_native and no new rootless segment"
    else
        fail "8 native child PATH: has bin dir=$native_has (want $want_native); bad segments:$bad_native"
    fi
else
    skip "8 native child PATH: needs cmd.exe and cygpath (Windows only)"
fi

# ---------------------------------------------------------------------------
# 9 (CI only): the profile `ocx self setup` writes finds the store by itself. OCX_HOME is
# unset and HOME holds no store, so on Windows the fence must reach USERPROFILE's .ocx through
# cygpath. Setup edits the user's registry PATH and profiles, so a developer machine skips it.
# ---------------------------------------------------------------------------
if [ -z "${CI:-}" ]; then
    skip "9 profile fence: set CI to run it (setup edits the user PATH and profiles)"
else
    echo
    echo "[profile] fence finds the store with OCX_HOME unset ..."
    prof=$work/userprofile
    hm=$work/shellhome
    mkdir -p "$prof" "$hm" || exit 2
    if [ "$windows" = 1 ]; then
        store=$prof/.ocx
        prof_native=$(cygpath -m "$prof") || exit 2
    else
        store=$hm/.ocx
        prof_native=$prof
    fi
    pbin=$store/symlinks/ocx.sh/ocx/cli/current/content/bin
    mkdir -p "$pbin" && cp -f -- "$src_bin" "$pbin/$bin_name" || exit 2
    # HOME is POSIX here on purpose: the runtime converts it for ocx.exe, as it does for a user.
    # PATH goes back to the pre-activation value and the ocx session variables are dropped.
    fresh() {
        env -u OCX_HOME -u ZDOTDIR -u SHELL -u XDG_CONFIG_HOME -u XDG_DATA_HOME \
            -u __OCX_ENV_STATE -u __OCX_ENV_PWD -u __OCX_ENV_STOOD_DOWN \
            HOME="$hm" USERPROFILE="$prof_native" PATH="$path_before" "$@"
    }
    if fresh "$pbin/$bin_name" --offline self setup >"$work/profile-setup.out" 2>&1; then
        shell_bin=$(command -v "$shell")
        # -i: the ocx wrapper function is part of the interactive stream only.
        case $shell in
            zsh)
                rc=.zshrc
                probe=". \"\$HOME/$rc\"; type ocx; ocx version >/dev/null; echo \"version_rc=\$?\""
                out=$(fresh "$shell_bin" -f -i -c "$probe" 2>/dev/null)
                ;;
            *)
                rc=.bashrc
                probe=". \"\$HOME/$rc\"; type ocx; ocx version >/dev/null; echo \"version_rc=\$?\""
                out=$(fresh "$shell_bin" --noprofile --norc -i -c "$probe" 2>/dev/null)
                ;;
        esac
        case $out in
            *function*version_rc=0*) pass "9 fresh $shell sourcing only \$HOME/$rc: 'ocx' is a function and 'ocx version' exits 0" ;;
            *) fail "9 fresh $shell sourcing only \$HOME/$rc: $out" ;;
        esac
    else
        cat "$work/profile-setup.out"
        fail "9 'ocx self setup' with profiles enabled failed"
    fi
fi

printf '\n%s passed, %s failed, %s skipped\n' "$passes" "$failures" "$skips"
if [ "$failures" -ne 0 ]; then
    printf '%s ACTIVATION CHECK(S) FAILED (%s)\n' "$failures" "$shell"
    exit 1
fi
printf 'ALL ACTIVATION CHECKS PASSED (%s)\n' "$shell"
