# test-windows-elvish-activation.elv - elvish activation gate.
#
# Usage: elvish test/manual/test-windows-elvish-activation.elv <path-to-ocx-binary>
#
# Counterpart of test-windows-git-bash-activation.sh for a native elvish. It seeds a
# throwaway store, lets `ocx self setup` write env.elv, evaluates that shim the way the
# rc.elv fence does (`eval (slurp < env.elv)`) and checks the bin directory is on PATH and
# `ocx version` runs. The per-prompt hook is not asserted: a non-interactive elvish binds
# no `edit:` namespace, so only a real terminal can register it.
# It runs twice: once with OCX_HOME set, and once with it unset and the store under a
# throwaway USERPROFILE (HOME on unix), which exercises the shim's own default. Both runs use
# `--no-modify-path --no-profile`, so no registry PATH or profile file is touched and neither
# needs a CI gate. HOME is removed on Windows, as a native process has none.

use os
use path
use str

if (!= (count $args) 1) {
  echo 'usage: elvish test-windows-elvish-activation.elv <path-to-ocx-binary>' >&2
  exit 2
}

var is-windows = (eq $path:separator '\')
var bin-name = (if $is-windows { put ocx.exe } else { put ocx })
var src = (path:abs $args[0])
if (not (path:is-regular $src)) {
  echo 'ocx binary not found: '$src >&2
  exit 2
}

# The shim joins with `/` where the caller may hold `\`; compare in one spelling.
fn norm {|p| str:replace '\' '/' $p }

var work = (os:temp-dir 'ocx-elvish-*')
var base = (path:dir $work)

var failures = 0
var passes = 0
fn check-pass {|msg| echo '[PASS] '$msg; set passes = (+ $passes 1) }
fn check-fail {|msg| echo '[FAIL] '$msg; set failures = (+ $failures 1) }

# Hermetic start: an inherited ocx session or project would redirect what the run reads.
# elvish has no listing of the environment, so the names are spelled out.
fn reset-env {
  for v [__OCX_ENV_STATE __OCX_ENV_PWD __OCX_ENV_STOOD_DOWN OCX_HOME OCX_BINARY_PIN OCX_CONSENT_PATHS
         OCX_CONSENT_NAMESPACES OCX_NO_CONSENT OCX_PROJECT OCX_NO_PROJECT OCX_CEILING_PATH
         OCX_CONFIG OCX_NO_CONFIG OCX_MANAGED_CONFIG OCX_INDEX OCX_DEFAULT_REGISTRY OCX_REMOTE
         OCX_OFFLINE OCX_FROZEN OCX_NO_COMPLETION OCX_NO_HOOK OCX_NO_MODIFY_PATH
         OCX_TOOLCHAIN_ACTIVATE OCX_TOOLCHAIN_DIR OCX_TOOLCHAIN_PINNED OCX_TOOLCHAIN_UPDATE
         OCX_PATCHES OCX_PATCH_SNAPSHOT OCX_LAZY_MODE OCX_LOG_LEVEL OCX_LOG_CONSOLE USERPROFILE] {
    unset-env $v
  }
  # A native Windows process has no HOME; a unix one needs it.
  if $is-windows { unset-env HOME }
}

# One run: seed the store, `ocx self setup`, evaluate env.elv, probe. `profile` is the directory
# the default store hangs off, or the empty string when OCX_HOME names the store.
fn run-case {|label store profile|
  var bin-dir = (path:join $store symlinks ocx.sh ocx cli current content bin)
  os:mkdir-all $bin-dir
  var ocx-bin = (path:join $bin-dir $bin-name)
  cp $src $ocx-bin

  reset-env
  if (eq $profile '') {
    set-env OCX_HOME $store
  } else {
    set-env USERPROFILE $profile
    if (not $is-windows) { set-env HOME $profile }
  }
  # The repository's own ocx.toml would make the reconcile report on it.
  cd $work

  echo "\n["$label"]"
  echo '  ocx  : '$ocx-bin
  echo '  store: '$store

  try {
    var setup-out = ($ocx-bin --offline self setup --no-modify-path --no-profile | slurp)
  } catch e {
    echo 'ocx self setup failed: '(to-string $e[reason]) >&2
    exit 2
  }
  var env-elv = (path:join $store env.elv)
  if (not (path:is-regular $env-elv)) {
    echo 'env.elv not written by ocx self setup: '$env-elv >&2
    exit 2
  }

  # 1: the shim evaluates. Its completion unit is allowed to fail without `edit:`; PATH must not.
  try {
    eval (slurp < $env-elv)
    check-pass '1 env.elv evaluated without raising'
  } catch e {
    check-fail '1 env.elv raised: '(to-string $e[reason])
  }

  # 2: the seeded bin dir is on PATH, in either slash spelling.
  var want = (norm $bin-dir)
  var on-path = $false
  for p [(str:split $path:list-separator $E:PATH)] {
    if (eq (norm $p) $want) { set on-path = $true }
  }
  if $on-path {
    check-pass '2 seeded bin dir on PATH: '$bin-dir
  } else {
    check-fail '2 seeded bin dir missing from PATH: '$bin-dir
  }

  # 3: `ocx version` runs, and resolves to the seeded copy rather than one the runner had.
  var resolved = ''
  try { set resolved = (norm (search-external ocx)) } catch _ { }
  try {
    var out = (ocx version | slurp)
    if (eq $resolved (norm $ocx-bin)) {
      check-pass "3 'ocx version' exits 0 through the seeded copy"
    } else {
      check-fail "3 'ocx version' ran, but via "$resolved" (want "(norm $ocx-bin)")"
    }
  } catch e {
    check-fail "3 'ocx version' failed: "(to-string $e[reason])
  }
}

echo 'Running elvish activation gate (os: '(if $is-windows { put windows } else { put other })')'
var explicit-store = (path:join $work home)
run-case 'OCX_HOME set' $explicit-store ''
# The default store: no OCX_HOME, the profile directory (HOME on unix) holds `.ocx`.
var profile = (path:join $work profile)
os:mkdir-all $profile
run-case 'OCX_HOME unset' (path:join $profile .ocx) $profile

cd $base
os:remove-all $work
echo "\n"$passes' passed, '$failures' failed'
if (> $failures 0) {
  echo $failures' ACTIVATION CHECK(S) FAILED (elvish)'
  exit 1
}
echo 'ALL ACTIVATION CHECKS PASSED (elvish)'
