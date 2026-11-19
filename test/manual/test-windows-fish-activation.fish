#!/usr/bin/env fish
# test-windows-fish-activation.fish - fish activation gate.
#
# Usage: fish test/manual/test-windows-fish-activation.fish <path-to-ocx-binary>
#
# Counterpart of test-windows-git-bash-activation.sh for fish, which cannot run a bash
# script. On Windows fish exists only as an MSYS2 or Cygwin package, so the binary is a
# native ocx.exe and the stream must carry `C:/...` binary paths and POSIX PATH entries
# (`/c/...`, or `/proc/cygdrive/c/...` under Cygwin), never `C:\...`. Under Cygwin pass the
# script and the binary in mixed form (`D:/a/...`): Cygwin converts no arguments.
# Also passes on Linux/macOS, where it checks the same contract with no conversion in play.
#
# Every check runs and the script exits non-zero at the end if any failed. It never
# touches the real OCX_HOME or any profile file (--offline, --no-modify-path, --no-profile).

# Hermetic start: an inherited ocx session or project would redirect what the run reads
# and writes, and fail checks with no defect behind them.
for v in __OCX_ENV_STATE __OCX_ENV_PWD __OCX_ENV_STOOD_DOWN OCX_BINARY_PIN \
    OCX_CONSENT_PATHS OCX_CONSENT_NAMESPACES OCX_NO_CONSENT OCX_PROJECT OCX_NO_PROJECT \
    OCX_CEILING_PATH OCX_CONFIG OCX_NO_CONFIG OCX_MANAGED_CONFIG OCX_INDEX \
    OCX_DEFAULT_REGISTRY OCX_REMOTE OCX_OFFLINE OCX_FROZEN OCX_NO_COMPLETION \
    OCX_NO_HOOK OCX_NO_MODIFY_PATH OCX_TOOLCHAIN_ACTIVATE OCX_TOOLCHAIN_DIR \
    OCX_TOOLCHAIN_PINNED OCX_TOOLCHAIN_UPDATE OCX_PATCHES OCX_PATCH_SNAPSHOT \
    OCX_LAZY_MODE OCX_LOG_LEVEL OCX_LOG_CONSOLE
    set -e $v
end

if test (count $argv) -ne 1
    echo "usage: fish "(status filename)" <path-to-ocx-binary>" >&2
    exit 2
end

set -g windows 0
set -g cygwin 0
set -g bin_name ocx
switch (uname -s)
    case 'MINGW*' 'MSYS*'
        set windows 1
        set bin_name ocx.exe
    case 'CYGWIN*'
        set windows 1
        set cygwin 1
        set bin_name ocx.exe
end

# cmd.exe's switch: MSYS rewrites `//c` to `/c` for a native child, Cygwin passes it through.
set -g cmd_c //c
test $cygwin = 1; and set cmd_c /c

# Absolute POSIX path of the binary: the cwd changes below. An argument spelled
# `D:/a/...` is only a drive-relative name to fish, so cygpath resolves it first.
set -l src_arg $argv[1]
test $windows = 1; and set src_arg (cygpath -u -- $argv[1])
set -l src_bin (path resolve -- $src_arg)
if not test -f "$src_bin"
    echo "ocx binary not found: $src_bin" >&2
    exit 2
end

set -l tmp_base /tmp
set -q TMPDIR; and set tmp_base $TMPDIR
set -q RUNNER_TEMP; and set tmp_base $RUNNER_TEMP
# RUNNER_TEMP is a native path; work in POSIX form.
test $windows = 1; and set tmp_base (cygpath -u -- $tmp_base)
set -g work (mktemp -d "$tmp_base/ocx-fish.XXXXXX"); or exit 2
function cleanup --on-event fish_exit
    rm -rf -- $work
end

set -g failures 0
set -g passes 0
set -g skips 0
function pass
    printf '[PASS] %s\n' $argv[1]
    set -g passes (math $passes + 1)
end
function fail
    printf '[FAIL] %s\n' $argv[1]
    set -g failures (math $failures + 1)
end
function skip
    printf '[skip] %s\n' $argv[1]
    set -g skips (math $skips + 1)
end

# bin_dir stays POSIX for the shell (cp, PATH checks). OCX_HOME reaches the native ocx.exe
# and cmd.exe unconverted under Cygwin, so on Windows it is the mixed form (`C:/...`), which
# all three runtimes accept as a path too.
set -g home_dir $work/home
set -gx OCX_HOME $home_dir
test $windows = 1; and set -gx OCX_HOME (cygpath -m -- $home_dir)
set -g bin_dir $home_dir/symlinks/ocx.sh/ocx/cli/current/content/bin
mkdir -p $bin_dir; or exit 2
cp -f -- $src_bin $bin_dir/$bin_name; or exit 2
set -g ocx_bin $bin_dir/$bin_name
printf 'Running fish activation gate (uname: %s)\n' (uname -s)
printf '  ocx     : %s\n' $ocx_bin
printf '  OCX_HOME: %s\n' $OCX_HOME

if not $ocx_bin --offline self setup --no-modify-path --no-profile >$work/setup.out 2>&1
    cat $work/setup.out
    echo "ocx self setup failed" >&2
    exit 2
end
if test -f $OCX_HOME/env.fish
    echo "  env.fish written"
else
    fail "env.fish not written by 'ocx self setup'"
end

# Whether the native directory $argv[1] is on PATH, spelled the way this runtime asks for.
# Cygwin's fixed `/proc/cygdrive/<drive>/...` alias is built from the mixed form, so a
# changed cygdrive prefix in fstab cannot make the check pass or fail.
function has_dir
    if test $windows = 1
        contains -- (cygpath -u -- $argv[1]) $PATH; and return 0
        test $cygwin = 1; or return 1
        set -l mixed (cygpath -m -- $argv[1])
        set -l parts (string split -m1 : -- $mixed)
        contains -- "/proc/cygdrive/"(string lower -- $parts[1])$parts[2] $PATH
    else
        contains -- $argv[1] $PATH
    end
end

echo
echo "[activate] source of the activation stream ..."
set -g path_before $PATH
# Baseline for check 8: a segment the runner already had is not ocx's to answer for.
set -g native_before
if test $windows = 1; and command -q cmd.exe
    set native_before (string split ';' -- (cmd.exe $cmd_c "echo %PATH%" 2>&1 | tr -d '\r'))
end
# --interactive is what makes the stream register the prompt hook, so this
# non-interactive script gets it too.
$ocx_bin --offline self activate --shell=fish --no-completion --interactive 2>$work/activate.err | source
if test -s $work/activate.err
    fail "1 activation stream wrote to stderr:"
    cat $work/activate.err
else
    pass "1 activation stream wrote nothing to stderr"
end

if functions -q ocx; and ocx --offline version >/dev/null 2>&1
    pass "2 'ocx' is a shell function and 'ocx version' exits 0"
else
    fail "2 'ocx' is not a working shell function: "(type ocx 2>&1 | string collect)
end

set -l added_bad
set -l any_bad
for p in $PATH
    if string match -rq -- '^[A-Za-z]$|^\\\\' $p
        set -a any_bad "[$p]"
    end
    contains -- $p $path_before; and continue
    string match -q -- '/*' $p; or set -a added_bad "[$p]"
end
if test -z "$added_bad$any_bad"
    pass "3 PATH hygiene: added segments absolute, no drive-letter or backslash segment"
else
    fail "3 PATH hygiene; non-absolute added: $added_bad bad segments: $any_bad"
end

if has_dir $bin_dir
    pass "4 seeded bin dir on PATH: $bin_dir"
else
    fail "4 seeded bin dir missing from PATH: $bin_dir"
end

# The prompt hook the stream registered, run once in this session: nothing else
# executes its command word. It returns the status it was called with, so call it
# right after a command that succeeded.
if functions -q __ocx_prompt_hook
    true
    __ocx_prompt_hook 2>$work/hook.err
    set -l hook_rc $status
    if test $hook_rc -eq 0; and not test -s $work/hook.err
        pass "5 prompt hook runs: status 0, stderr empty"
    else
        fail "5 prompt hook: status $hook_rc; stderr: "(cat $work/hook.err | string collect)
    end
else
    fail "5 prompt hook function __ocx_prompt_hook not defined by the stream"
end

# The shim must activate on its own. Restore the pre-activation PATH and drop the
# session state first, or the bin dir is still on PATH from the source above and
# env.fish, which swallows its own activation errors, could not fail this check.
set -gx PATH $path_before
set -e __OCX_ENV_STATE
source $OCX_HOME/env.fish
if has_dir $bin_dir
    pass "6 env.fish alone puts the bin dir on a reset PATH"
else
    fail "6 env.fish did not put the bin dir on a reset PATH: $bin_dir"
end

# ---------------------------------------------------------------------------
# 7: one reconcile apply and revert. The revert is the load-bearing half: an
# apply-only reconciler passes an apply-only assertion vacuously.
# ---------------------------------------------------------------------------
echo
echo "[reconcile] apply/revert ..."
set -l project_root $work/reconcile
set -l project_dir $project_root/alpha
mkdir -p $project_dir
printf '%s\n' '[env]' 'WP18_CONST = "alpha"' >$project_dir/ocx.toml
# An empty tool set keeps this registry-free.
printf '%s\n' 'tool = []' '' '[metadata]' 'lock_version = 3' \
    'declaration_hash_version = 1' \
    'declaration_hash = "sha256:0000000000000000000000000000000000000000000000000000000000000000"' \
    'generated_by = "ocx acceptance fixture"' \
    'generated_at = "2026-01-01T00:00:00Z"' >$project_dir/ocx.lock

set -l start_dir $PWD
cd $project_dir; or exit 2
# `ocx lock` stamps consent; without it the reconciler is inert by design.
if $ocx_bin --offline lock >/dev/null 2>$work/lock.err
    $ocx_bin --offline self activate --reconcile --shell=fish 2>$work/apply.err | source
    if test "$WP18_CONST" = alpha; and not test -s $work/apply.err
        pass "7a reconcile apply: WP18_CONST=alpha, stderr empty"
    else
        fail "7a reconcile apply: WP18_CONST='$WP18_CONST'; stderr: "(cat $work/apply.err | string collect)
    end
else
    fail "7a 'ocx lock' failed: "(cat $work/lock.err | string collect)
end

cd $project_root; or exit 2
$ocx_bin --offline self activate --reconcile --shell=fish 2>$work/revert.err | source
if not set -q WP18_CONST; and not test -s $work/revert.err
    pass "7b reconcile revert: WP18_CONST gone, stderr empty"
else
    fail "7b reconcile revert: WP18_CONST='$WP18_CONST'; stderr: "(cat $work/revert.err | string collect)
end
cd $start_dir; or exit 2

# ---------------------------------------------------------------------------
# 8: PATH as a native child sees it.
# ---------------------------------------------------------------------------
printf '\n[diag] final PATH: %s\n' (string join : -- $PATH)
if test $windows = 1; and not begin command -q cmd.exe; and command -q cygpath; end
    fail "8 native child PATH: cmd.exe or cygpath not found on a Windows runtime"
else if test $windows = 1
    set -l native_path (cmd.exe $cmd_c "echo %PATH%" 2>&1 | tr -d '\r')
    printf '[diag] PATH seen by a native child: %s\n' $native_path
    set -l want_native (cygpath -w -- $bin_dir)
    set -l native_has 0
    string match -rq -- (string escape --style=regex -- (string lower -- $want_native)) (string lower -- $native_path); and set native_has 1
    set -l bad_native
    for seg in (string split ';' -- (string trim -r -c ';' -- $native_path))
        string match -rq -- '^[A-Za-z]:[\\\\/]|^\\\\\\\\' $seg; and continue
        contains -- $seg $native_before; or set -a bad_native "[$seg]"
    end
    if test $native_has = 1; and test -z "$bad_native"
        pass "8 native child PATH has $want_native and no new rootless segment"
    else
        fail "8 native child PATH: has bin dir=$native_has (want $want_native); bad segments: $bad_native"
    end
else
    skip "8 native child PATH: needs cmd.exe and cygpath (Windows only)"
end

# ---------------------------------------------------------------------------
# 9 (CI only): the conf.d file `ocx self setup` writes finds the store by itself. OCX_HOME is
# unset and HOME holds no store, so on Windows the file must reach USERPROFILE's .ocx through
# cygpath. Setup edits the user's registry PATH and profiles, so a developer machine skips it.
# ---------------------------------------------------------------------------
if not set -q CI
    skip "9 conf.d fence: set CI to run it (setup edits the user PATH and profiles)"
else
    echo
    echo "[profile] conf.d file finds the store with OCX_HOME unset ..."
    set -g prof $work/userprofile
    set -g hm $work/shellhome
    set -l xdg $work/xdg
    mkdir -p $prof $hm $xdg; or exit 2
    set -g prof_native $prof
    set -g xdg_native $xdg
    set -l store $hm/.ocx
    if test $windows = 1
        set prof_native (cygpath -m -- $prof)
        # Cygwin converts no XDG variable for a native child, so ocx.exe gets the mixed form.
        set xdg_native (cygpath -m -- $xdg)
        set store $prof/.ocx
    end
    set -l pbin $store/symlinks/ocx.sh/ocx/cli/current/content/bin
    mkdir -p $pbin; and cp -f -- $src_bin $pbin/$bin_name; or exit 2
    # A unix host wires fish only when SHELL names it; a Windows host wires every shell on PATH.
    set -g fish_bin (command -s fish)
    # HOME is POSIX here on purpose: the runtime converts it for ocx.exe, as it does for a user.
    # PATH goes back to the pre-activation value and the ocx session variables are dropped.
    function fresh
        env -u OCX_HOME -u ZDOTDIR -u XDG_DATA_HOME \
            -u __OCX_ENV_STATE -u __OCX_ENV_PWD -u __OCX_ENV_STOOD_DOWN \
            HOME=$hm USERPROFILE=$prof_native XDG_CONFIG_HOME=$xdg_native SHELL=$fish_bin \
            PATH=(string join : -- $path_before) $argv
    end
    if fresh $pbin/$bin_name --offline self setup >$work/profile-setup.out 2>&1
        set -l probe 'source "$XDG_CONFIG_HOME/fish/conf.d/ocx.fish"; functions -q ocx; and echo is_function; ocx version >/dev/null; echo version_rc=$status'
        # -i: the ocx wrapper function is part of the interactive stream only.
        set -l out (fresh $fish_bin --no-config -i -c $probe 2>$work/fresh.err | string collect)
        if string match -q '*is_function*version_rc=0*' -- $out
            pass "9 fresh fish sourcing only conf.d/ocx.fish: 'ocx' is a function and 'ocx version' exits 0"
        else
            fail "9 fresh fish sourcing only conf.d/ocx.fish: $out"(cat $work/fresh.err | string collect)
        end
    else
        cat $work/profile-setup.out
        fail "9 'ocx self setup' with profiles enabled failed"
    end
end

printf '\n%s passed, %s failed, %s skipped\n' $passes $failures $skips
if test $failures -ne 0
    printf '%s ACTIVATION CHECK(S) FAILED (fish)\n' $failures
    exit 1
end
echo "ALL ACTIVATION CHECKS PASSED (fish)"
