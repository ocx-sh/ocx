# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""The acceptance suite as Bazel test targets — one `sh_test` per module (C-024).

`acceptance_suite()` is the whole surface: it writes one runner script and
declares one `sh_test` per `test/tests/test_*.py`, each of them tagged
`no-sandbox` (plus `external` on `UNCACHED_MODULES`), each declaring its own
module plus the shared `:suite_inputs` group. The targets run CONCURRENTLY
(see "Concurrency" below).

**`sh_test` is not a native rule on a Bazel 9 pin.** It is loaded from
`@rules_shell//shell:sh_test.bzl` below; without that load the package fails at
load time with `name 'sh_test' is not defined`, and no amount of `bazel_dep` on
`rules_shell` changes that (BZL-LARK-10).

## Result caching is ON, and the two tags say exactly that

The ADR's stage 4 ruled result caching off and carried `external` + `local` to
enforce it. That ruling is **amended** (see
`adr_bazel_build_adoption.md` § Stage 4): the acceptance stage now runs with its
results cached, and the compensating control the ADR itself named — A4's red
half 3, the binary's declared input digest — is what turns on.

Both tags that had to go were measured out, on this exact pin (bazel 9.2.0,
`rules_shell` 0.8.0, five `sh_test` targets over one script, `--disk_cache`
only, cold / warm-same-server / warm-fresh-server;
`scripts/bazel_accept_proofs.py --prove-s015` is the probe and ships this table
as an expectation, so a Bazel release moving a cell reds rather than silently
invalidating the decision):

    target            tags                             cold   warm    warm
                                                              (same)  (fresh)
    sibling           -                                 ran   CACHED  CACHED
    acc_local_only    local, exclusive                  ran   CACHED  ran
    acc_nocache       local, exclusive, no-cache        ran   CACHED  ran
    acc_external      local, external, exclusive        ran   ran     ran
    acc_no_sandbox    no-sandbox, exclusive             ran   CACHED  CACHED

**`external` had to go** — it is the only tag that stops a test result being
reused at all, which was its whole job and is now the opposite of the job.

**`local` had to go too, and that is the non-obvious half.** `local` is
`no-remote` + `no-sandbox` combined, and the `no-remote` half suppresses the
`--disk_cache` (and `--remote_cache`) hit while leaving only the in-server
action-cache hit. The row above says it out loud: `acc_local_only` re-runs on a
warm **fresh server** — which is the state every CI runner is in, and the state
a developer is in after `bazel shutdown`. A lane tagged `local` would report
`(cached)` on a laptop and recompute the whole suite on CI.

**`no-sandbox` buys the property the runner actually needs** without the cache
half: the test runs in the execroot rather than in a sandbox, which is a hard
requirement here (the suite drives `docker compose` and a uv-managed virtualenv
from the source tree, neither of which survives a sandboxed working directory).
Measured above: it caches exactly like an untagged sibling in both warm states.

And measured again on **this** workspace, where the probe's rc does not apply:
after a run that rebuilt the binary under test and re-executed every acceptance
target (181 at the time), the binary was restored and the next `bazel test
//test:all` reported `Executed 0 out of 181 tests` in 2 s. The local action cache could not have
served that — it holds one entry per action and every one of them had just been
rewritten with the mutated binary's key — so the results came back from
`--disk_cache`, which is the tier a fresh server has and `local` would have
suppressed.

## Concurrency: `exclusive` is gone, and xdist parity is the bar

No target is `exclusive`-tagged any more (ADR `adr_test_speed_tiers.md` AM-9).

The targets share one compose stack, exactly as `task test:parallel`'s xdist
workers do (`-n auto --dist loadgroup`, UUID-scoped repositories per test). So
whatever is xdist-safe is safe across concurrent pytest processes, except for
what xdist gives a run that separate processes do not — and the runner below
supplies each of those instead of serialising the suite (WP-12 A3: 1734 s cold
serial against ~135 s for `task test:parallel`):

* **One stack bring-up.** `test/conftest.py::pytest_sessionstart` brings the
  stack up in the xdist controller only; N sessions would race `docker compose
  up`. The runner runs that same hook once per target under an exclusive
  `acceptance-stack-<port>.lock` — and inside it `test/src/helpers.py`'s
  host-wide `_COMPOSE_LOCK`, the lock a session's sigstore `up -d` and a
  registry recycle take — before pytest starts, so each session's own call
  finds everything reachable and does no compose work.
* **A basetemp per target.** pytest wipes `--basetemp` at start, so the arena
  is `<checkout>-<cksum of the suite path>-bazel/<module stem>` rather than one
  shared directory. The checksum is what keys it: two repositories can both
  hold a worktree named `speed`, and a folder-name key would hand them one
  arena. Two copies of ONE target (`--runs_per_test=N`) still share it, so
  pytest runs under an innermost exclusive `<basetemp>.lock`.
* **xdist groups.** `xdist_group` names a registry-wide singleton
  (`patch_global_slot`: `ocx patch publish --global` writes one reserved
  repository). xdist runs a group on one worker; here every target is its own
  process, and two runs of one target — from sibling checkouts sharing the
  stack — are two processes too. So a module holds
  `acceptance-slot-<port>-<group>.lock` for its whole run for EVERY group it
  names, shared with another module or not (`module_slots` below, acquired in
  sorted order). The per-module list is derived from source and checked by
  `scripts/bazel_tag_guard.py` (`tag-slot-*`), which also reds a group it
  cannot resolve to literals and any `xdist_group` spelled outside a
  `tests/test_*.py` module.
* **No overlap with `task test:parallel`'s pytest step.** Every target holds the
  host's `acceptance-suite-<port>.lock` SHARED; `test/taskfile.yml`'s pytest
  step takes it EXCLUSIVE. Targets overlap each other — including the targets
  of a bazel run from a sibling checkout on the same stack, at xdist parity —
  and never that step. flock(2) is not fair, so an exclusive waiter could sit
  behind overlapping shared holders for a whole bazel run; the step therefore
  holds `acceptance-turnstile-<port>.lock` while it waits and runs, and every
  target passes through that turnstile before it takes the suite lock, so no
  new target joins once the step is queued. What runs before that step is NOT under the lock: its
  stale-run eviction (which kills processes executing that checkout's
  `test/bin/ocx`) and its binary rebuild, and `task test:smoke` takes no lock
  at all. So never start a `task test*` in a checkout whose bazel run is in
  flight.
* **No pytest cache writes.** `-p no:cacheprovider`: N processes writing one
  `.pytest_cache/` is a race xdist never had (only its controller writes).
* **No flock(1), no run.** Without it none of the locks above can be taken, so
  the runner refuses unless `OCX_ACCEPTANCE_UNSERIALISED=1` reaches it through
  `--test_env` (an action-key input, unlike `env_inherit`). Then it runs pytest
  with no barrier and no slots: pair it with `--local_test_jobs=1`.

Lock order, everywhere: turnstile (passed, then released) → suite (shared) →
stack → `_COMPOSE_LOCK` in the barrier; turnstile (passed) → suite (shared) →
slots in sorted order → the arena's `<basetemp>.lock` in a session, which may
take `_COMPOSE_LOCK` itself inside pytest; turnstile → suite (exclusive) in
`test/taskfile.yml`. The runner never holds the turnstile while it waits on
anything, `_COMPOSE_LOCK` is always innermost and its holders never wait on
another lock, and no session waits on the stack lock, so no cycle exists.
Every blocking take is preceded by a `flock -n` probe that says on stderr
which lock the target is waiting for.

`--local_test_jobs` stays off every rc file for the reason it always was — an
rc line is per-command and never per-target-pattern, so a global one would
throttle the 35 Rust test targets too — and
`bazel_accept_proofs.py::global_serialisation_findings` reds one. `task
bazel:test:accept` passes it on the command line, where it bounds this
invocation's concurrency and nothing else.

What concurrency does not change: a full registry — and a re-probe that lies.
Each session's own `pytest_sessionstart` still probes the registry with no
lock held (the barrier only guarantees the stack was up when it ran), and so
does xdist's per-worker `registry` fixture. A probe that answers "refuses
writes" — a full store, or a false negative under load — hands the decision to
`_recycle_registry`, which re-asks under `_COMPOSE_LOCK` before each of its
`rm` and `up`, and keeps the lock until the fresh registry accepts writes: a
registry a sibling session already recycled, or one that answers the second
time, is left alone, so only a store that still refuses writes under the lock
is wiped under the targets still running. Every such state is red (a wiped
registry fails the in-flight pushes, it serves no stale green). An
"unreachable" answer runs `compose_up`, an unlocked `up -d` of the whole
stack: from the checkout that created the containers it leaves them alone,
but from a sibling checkout the bind-mount paths differ, so it recreates
them under that checkout's running targets (`helpers.py`
`start_sigstore_stack` says why).

## What each target declares, and what the unit honestly is

Per target: the module itself, the `uv` launcher, `:suite_inputs` — the one
shared group carrying what (nearly) every module reads that is not the module
(`conftest.py`, `pyproject.toml`, `uv.lock`, `ocx.toml`, `ocx.lock`,
`src/**/*.py`, the non-`test_*` helpers and the fixture tree under `tests/**`,
`sigstore/**`, `docker-compose.yml`, `zot-config.json`), the binary under test
and the launcher (`//crates/ocx_cli:ocx`, `//crates/ocx_shim:ocx_shim`) — and
its `module_data`: the files only a few modules read
(`taskfile.yml`, `bench/**`, `recordings/**/*.py`, `scenarios/**`,
`scripts/**`, `specs/**`, and the cross-package reads), declared on exactly
those modules so an edit to one re-runs its readers and not the suite.
`test/BUILD.bazel` says why each shared file is shared. Every service image
reference and every host port enters the key by virtue of `docker-compose.yml`
being an input.

**No target reads its siblings.** A module that sweeps the acceptance modules'
own source (the smoke-coverage, no-crate-path, deprecated-spelling, global-patch
and doc-publish sweeps, and the shell register's traceability checks) is a lint
test and lives in `test/lint/`, outside this package's `tests/test_*.py` glob,
run uncached by `task test:lint:structure` (plan_test_speed_tiers.md C-LINT).
They used to be acceptance targets handed the module set as extra inputs, which
made one ordinary module's edit re-execute **5** targets (6 for a `test_shell*`
module); it now re-executes **1**. `scripts/bazel_tag_guard.py` derives a sweep
from source and reds any acceptance module that has one, because there is no
longer a way to declare it here.

**Per-module fixture attribution is not feasible, and this is measured rather
than assumed:** **160 of the then 181** (172 today) modules carry an `import`/`from` of the one
shared `src` package (an AST walk over `test/tests/test_*.py`, not a grep), so a
per-module input set would be the shared set plus a rounding error for almost
every target. The honest unit is therefore **module + shared group**: a
change to any shared file re-runs the whole suite, a change to one module
re-runs one target. That is the selection C-024 asked for, stated at the
granularity it actually holds at.

## The residual under-declaration, named out loud

Caching a result means trusting the declared inputs to cover everything the
action reads. These do not, and each gap is written here rather than left for a
stale green to announce:

* **The compose stack's running state is not an action input.** `docker-compose.yml`
  is, so a changed service image reference or a changed port re-keys every
  target — but a stack that is *up with different content* (a registry holding
  another run's blobs, a Rekor with a different log) does not. Stale-green risk:
  a suite that passed against one live stack reports cached against another.
  Bounded by the host-wide suite lock the runner takes (no `task
  test:parallel` pytest step overlaps a bazel run on the same registry port;
  § Concurrency names what precedes that step unlocked) and by the
  fact that the fixtures create their own repositories per test.
* **Reads that cross the package boundary are undeclared, and their targets
  are not cached.** `target/**` (cargo output, `.bazelignore`d, cannot be a
  label at all), `website/**`, `crates/**`, `.github/**`, `packaging/**`, the
  root `ocx.toml`, and `test/doc_scripts/**` (a *subpackage*, which no glob in
  this package can reach). Every module whose source — or a helper it imports,
  or a `conftest.py` above it — names such a path is in `UNCACHED_MODULES`
  below and carries `external`, so it re-executes on every run instead of
  serving a stale green (plan_test_speed_tiers.md C-019).
  `scripts/bazel_tag_guard.py` derives that set from the AST against each
  target's input closure and reds the list the moment it disagrees. The list
  shrinks by declaring the input (C-020), never by deleting a line.
* **`env_inherit` values reach the test and are NOT part of the action key**, so
  a result recorded under one value is served under another. `_INHERITED_ENV`
  below carries the per-name accounting, including the one name for which the
  two values ask for different verdicts from the same test
  (`__OCX_TESTING_REQUIRE_ENVIRONMENT_D`; `CI` was the other until its only
  reader, `test_schema_generation.py`, was ported to Rust — C-020).

**None of these results reaches another machine**, which bounds every gap
above to the host that produced it: `.bazelrc` carries
`--remote_upload_local_results=false`, and the acceptance job holds the shared
cache's *read* credential and no write credential on any trigger. A wrong entry
here is one developer's or one runner's, and a `NOCACHE=1` (which adds
`--nocache_test_results`) clears it.

What is **not** a gap any more: the binary under test. It is a Bazel output,
`//crates/ocx_cli:ocx`, in every target's `data`, so the targets key on the
binary Bazel built from the source graph — a Rust edit that changes its bytes
re-runs every module (A4's red half 3), one that leaves them identical re-runs
nothing, and "the built and the under-test binary are the same bytes" holds by
construction: the runner executes the runfile, never a copy.

**Which control is live, exactly.** The *declaration* is enforced on every run:
`scripts/bazel_tag_guard.py` reads the graph in `task verify` and in
`verify-basic.yml`, reds an acceptance target whose input closure has lost
`//crates/ocx_cli:ocx`, `//test:suite_anchor` or `//test:docker-compose.yml`,
reds one that has acquired a cache-suppressing tag, and floors what
`//test:suite_inputs` itself contains. The *run-side* proof — a binary-swap run
in which no target reports cached — is **not** in any lane: it needs a rebuilt
binary and a warm run, so it is a command a person runs,
`scripts/bazel_accept_proofs.py --check-s015` (`bazel:test:accept`'s summary
spells it out). Nothing here should be read as claiming it runs on its own.
"""

load("@rules_shell//shell:sh_test.bzl", "sh_test")

visibility("private")

# C-024's tag set, as amended. Sorted, because `bazel query` reports tags sorted
# and `task test:bazel:tags` reduces that output into the table the S-015
# comparator judges — two readers of one fact, and they have to line up.
#
# `external` and `local` are both absent deliberately and both are measured out
# in the module docstring's table: `external` stops the result being reused at
# all, and `local`'s `no-remote` half suppresses the disk-cache hit, which is
# the CI-runner case. Adding either here turns result caching off for the whole
# suite; `external` is added per target, and only through `UNCACHED_MODULES`.
ACCEPTANCE_TAGS = [
    "no-sandbox",
]

# Modules whose source reads a path this package cannot declare (ADR
# `adr_test_speed_tiers.md` C-UNCACHED, plan C-019). Their targets carry
# `external` on top of `ACCEPTANCE_TAGS`, which is the one tag measured to stop
# a test result being reused: a cached verdict over `target/`, `website/` or
# `test/doc_scripts/` is stale the moment that file changes, and nothing in its
# key would say so.
#
# The list is not hand-curated. `scripts/bazel_tag_guard.py` derives it from
# each module's AST (its own reads, its imported helpers', its conftests')
# against the target's input closure and reds both directions: a module that
# reads an undeclared path and is absent here, and a module listed here that no
# longer does (so it cannot rot into a permanent cache opt-out). It shrinks by
# declaring the input or moving the read, never by deleting a line.
UNCACHED_MODULES = [
    # The one named residual (plan_test_speed_tiers.md C-020). Its
    # `_find_shim_binary` still lists cargo's `target/{release,debug}/` as
    # fallbacks behind `OCX_SHIM_BINARY`, on two adjacent lines, and
    # `scripts/test_diff_guard.py` admits only a one-line-for-one-line
    # re-point — dropping them is a two-line hunk it refuses by design. The
    # module is `skipif(sys.platform != "win32")` as a whole, so on the Linux
    # lane that runs this suite `external` costs one collection and hides no
    # verdict; it leaves the list with that edit.
    "tests/test_windows_shim.py",
]

# The binary under test and the Windows launcher, built by Bazel (`stamp = 0`,
# the `__testing` provenance on `:ocx_cli`), declared on every target and handed
# to the runner by rlocationpath. Declaring them is what makes a Rust edit
# re-run the suite: the targets re-key on the binary's source graph, and an
# edit that leaves its bytes unchanged re-runs nothing.
_OCX = "//crates/ocx_cli:ocx"
_OCX_SHIM = "//crates/ocx_shim:ocx_shim"
_BINARY_ENV = {
    "OCX_COMMAND": "$(rlocationpath %s)" % _OCX,
    "OCX_SHIM_BINARY": "$(rlocationpath %s)" % _OCX_SHIM,
}

# The runner's slot-lock list (§ Concurrency). Spelled once; the tag guard
# reads the same name off each target's `env`.
_SLOTS_ENV = "OCX_ACCEPTANCE_SLOTS"

# The tag `UNCACHED_MODULES` adds. The same spelling
# `scripts/bazel_accept_proofs.py::CACHE_DEFEATING_TAG` measures.
_UNCACHED_TAG = "external"

# The environment the suite reads that Bazel does not synthesise. `HOME` is
# uv's cache root and ocx's default store root; `DOCKER_HOST` /
# `XDG_RUNTIME_DIR` are how a rootless docker daemon is found; the two port
# knobs are the ones `docker-compose.yml`, `test/conftest.py` and
# `test/taskfile.yml` all read, so a machine that moved the registry off 5000
# moves this suite with it.
#
# With results cached this list is no longer free, and the docstring's third
# gap is this one: an inherited value is supplied by the client, so a run that
# reuses a result recorded under a different `OCX_TEST_REGISTRY_PORT` is reusing
# a verdict about another stack. The list is kept as short as the runner can
# work with for exactly that reason — every entry is a value the suite cannot
# synthesise, not a convenience.
# **What an `env_inherit` name costs, stated once and applying to every name in
# this list.** An inherited value is supplied by the client and is NOT part of
# the action key: a result recorded under one value is served under another. So
# a name here buys the suite a value it cannot synthesise and pays for it with a
# cache entry that is only as true as the client that produced it.
#
# Where that bites, per name, rather than as a blanket warning:
#
#   * `OCX_TEST_REGISTRY_PORT` / `OCX_TEST_MIRROR_PORT` / `DOCKER_HOST` /
#     `XDG_RUNTIME_DIR` — a machine that moves the registry, or points at
#     another daemon, reuses a verdict about a different stack. This is the
#     docstring's stack gap reached through the environment instead of through
#     `docker-compose.yml`.
#   * `__OCX_TESTING_REQUIRE_ENVIRONMENT_D` — the sharp one, because its two
#     values ask for *different verdicts* from the same test. Unset, an absent
#     `environment.d` generator is a skip; set, it is a failure. A run that
#     skipped on a developer box and a run that must fail on CI therefore share
#     an action key, and whichever ran first wins. `verify-deep.yml`'s
#     acceptance job is the only setter and CI's output base is fresh per job,
#     which is what bounds it; `NOCACHE=1` (`--nocache_test_results`) is the
#     escape when it does not.
#   * `HOME`, `USER`, `DOCKER_CONFIG`, `SSH_AUTH_SOCK` — per-machine by
#     construction; two machines do not share an output base, so the mismatch
#     is unreachable rather than merely unlikely.
#
# The list is kept as short as the runner can work with for exactly that
# reason: every entry is a value the suite cannot synthesise, not a
# convenience.
_INHERITED_ENV = [
    "DOCKER_CONFIG",
    "DOCKER_HOST",
    "HOME",
    "OCX_TEST_MIRROR_PORT",
    "OCX_TEST_REGISTRY_PORT",
    "SSH_AUTH_SOCK",
    "USER",
    "XDG_RUNTIME_DIR",
    # `verify-deep.yml`'s acceptance step sets this,
    # and without the line `test/tests/test_session_path.py` read `""` and
    # `pytest.skip`ped in the one lane that exists to make that absence fatal —
    # silently, under about 99 of `SKIP_CEILING` headroom. Sorted last because
    # `__` sorts after the uppercase names and buildifier does not reorder a
    # list it did not write.
    #
    # **What inheriting costs, for every name in this list.** `env_inherit`
    # values are not part of the action key, so a result cached under one value
    # is served under another: a target that passed with
    # `__OCX_TESTING_REQUIRE_ENVIRONMENT_D` unset stays cached when CI sets it.
    # The names here are host bindings (`HOME`, `DOCKER_HOST`, the two ports)
    # and lane bindings, not inputs under test, and the lane that sets the two
    # testing names runs on a fresh runner whose cache is populated by the
    # `main` push — which sets them too. `task bazel:test:accept NOCACHE=1`
    # maps to `--nocache_test_results` for the case where that is not enough.
    "__OCX_TESTING_REQUIRE_ENVIRONMENT_D",
]

_RUNNER = """#!/bin/sh
# GENERATED by test/bazel.bzl. There is no source copy of this file: it is
# written by `ctx.actions.write`, so its bytes are an action input and a change
# to them re-keys every target that runs it.
#
# argv: <uv rlocationpath> <package-relative module> [extra pytest arg ...]
set -eu

fail() {
    echo "acceptance sh_test: $*" >&2
    exit 1
}

[ "$#" -ge 2 ] || fail "usage: <uv rlocationpath> <module> [pytest arg ...]"
uv_rlocation="$1"
module="$2"
shift 2

# Every one of these is set by `bazel test` and by nothing else. A missing one
# means this script was invoked some other way, and the checks below would then
# be measuring a directory layout that is not the one under test.
[ -n "${TEST_SRCDIR:-}" ] || fail "TEST_SRCDIR is unset - run this through \\`bazel test\\`"
[ -n "${TEST_WORKSPACE:-}" ] || fail "TEST_WORKSPACE is unset - run this through \\`bazel test\\`"
[ -n "${TEST_TMPDIR:-}" ] || fail "TEST_TMPDIR is unset - run this through \\`bazel test\\`"

uv="$TEST_SRCDIR/$uv_rlocation"
[ -x "$uv" ] || fail "no executable uv launcher at $uv (@tools//:uv)"

# The suite runs in the SOURCE tree, and the path to it is resolved from a
# runfile rather than written into the BUILD file: an absolute path in a
# declared output or a rule attribute is BZL-HERM-01, and it would also make
# this package unusable from any other checkout. `conftest.py` is a runfile of
# every acceptance target; under `no-sandbox` Bazel materialises it as
# a symlink into the source tree, so resolving that symlink is the anchor.
anchor="$TEST_SRCDIR/$TEST_WORKSPACE/test/conftest.py"
[ -e "$anchor" ] || fail "no runfile at $anchor - the anchor is not in this target's data"
real_anchor=$(readlink -f "$anchor") || fail "cannot resolve the anchor runfile $anchor"
suite=$(dirname "$real_anchor")

# A runfiles tree materialised as COPIES rather than symlinks resolves to
# itself, and every check below would then pass against a tree that is not the
# source tree - the shape where a green says nothing. Refused by name.
case "$suite" in
    *.runfiles/* | */bazel-out/*)
        fail "runfiles are copies here, so $suite is not the source tree"
        ;;
esac

[ -f "$suite/pyproject.toml" ] || fail "$suite carries no pyproject.toml - wrong tree"
[ -f "$suite/$module" ] || fail "no acceptance module at $suite/$module"

# The binary under test and the Windows launcher, as Bazel built them
# (`//crates/ocx_cli:ocx`, `//crates/ocx_shim:ocx_shim`): every target carries
# both as `data` and names each by rlocationpath in its `env`
# (`acceptance_suite` below). Resolved to absolute paths because pytest runs in
# the source tree, where a runfiles-relative path means nothing. A suite run
# against an absent binary is silent, so its absence is a refusal.
[ -n "${OCX_COMMAND:-}" ] || fail "OCX_COMMAND is unset - the target's env names no binary under test"
OCX_COMMAND="$TEST_SRCDIR/$OCX_COMMAND"
[ -x "$OCX_COMMAND" ] || fail "no executable ocx runfile at $OCX_COMMAND"
[ -n "${OCX_SHIM_BINARY:-}" ] || fail "OCX_SHIM_BINARY is unset - the target's env names no ocx-shim"
OCX_SHIM_BINARY="$TEST_SRCDIR/$OCX_SHIM_BINARY"
[ -x "$OCX_SHIM_BINARY" ] || fail "no executable ocx-shim runfile at $OCX_SHIM_BINARY"

# The `ocx_schema` binary, on the targets whose `env` names it
# (`test/BUILD.bazel` `module_env`): Bazel builds it and hands its
# rlocationpath, never cargo's `target/release/ocx_schema`, which no key names.
# Resolved here because pytest runs in the source tree, where a
# runfiles-relative path means nothing. Offered two ways, because the two
# modules that run it can reach two: `test_execution_records.py` reads
# `OCX_SCHEMA_BINARY`, and `test_project_env.py`, which imports no `os`, finds
# `ocx_schema` on `PATH` — a directory holding nothing else, first on it.
if [ -n "${OCX_SCHEMA_BINARY:-}" ]; then
    OCX_SCHEMA_BINARY="$TEST_SRCDIR/$OCX_SCHEMA_BINARY"
    [ -x "$OCX_SCHEMA_BINARY" ] || fail "no executable ocx_schema runfile at $OCX_SCHEMA_BINARY"
    schema_path="$TEST_TMPDIR/ocx_schema_path"
    mkdir -p "$schema_path"
    ln -sf "$OCX_SCHEMA_BINARY" "$schema_path/ocx_schema"
    PATH="$schema_path:$PATH"
    export OCX_SCHEMA_BINARY PATH
fi
OCX_INSECURE_REGISTRIES="localhost:${OCX_TEST_REGISTRY_PORT:-5000}"
export OCX_COMMAND OCX_SHIM_BINARY OCX_INSECURE_REGISTRIES

# pytest arenas, and why they are NOT under $TEST_TMPDIR (BZL-TEST-12's usual
# answer). Measured on 9.2.0: bazel symlinks the workspace's `.git` into
# `execroot/_main`, and $TEST_TMPDIR lives under it — so `git rev-parse` from a
# pytest `tmp_path` answers the execroot as its toplevel and every test that
# shells out to git inside its own arena is silently inside a repository.
# `test_git_http_fixture.py::test_shim_preserves_exit_code_and_stderr_bytes`
# caught it: its positive control (`git rev-parse HEAD` in an empty directory
# must fail) returned this checkout's HEAD and exit 0.
#
# So the arenas go where `test/taskfile.yml` already puts them — disk-backed,
# off the reaped /tmp, outside every git tree — with a `-bazel` suffix so the
# two entry points never delete each other's basetemp at startup.
#
# One arena PER TARGET: pytest deletes `--basetemp` when it starts, and the
# targets run concurrently, so a shared one would be wiped under a sibling.
#
# Keyed on a checksum of the resolved suite path, with the checkout's folder
# name in front only so a person can tell the arenas apart: the folder name
# alone collides across repositories (two `.agents/worktrees/speed` trees on
# one host), and those two runs overlap on one stack.
[ -n "${HOME:-}" ] || fail "HOME is unset - the arena root and uv's cache both need it"
checkout=$(basename "$(dirname "$suite")")
suite_key=$(printf '%s' "$suite" | cksum | cut -d ' ' -f 1)
stem=$(basename "$module" .py)
basetemp="$HOME/.cache/ocx-pytest/$checkout-$suite_key-bazel/$stem"
TMPDIR="$HOME/.cache/ocx-test-tmp"
mkdir -p "$basetemp" "$TMPDIR"
export TMPDIR

# `-p no:cacheprovider`: concurrent sessions would all write one
# `.pytest_cache/`; under xdist only the controller does.
set -- -p no:cacheprovider --basetemp="$basetemp" "$module" "$@"
[ -z "${XML_OUTPUT_FILE:-}" ] || set -- "--junit-xml=$XML_OUTPUT_FILE" "$@"

cd "$suite"

# The host locks (`bazel.bzl` § Concurrency). All of them live outside the
# repository and are keyed on the registry port: a `<checkout>/.agents/…` path
# resolves to a DIFFERENT file in each worktree, so it would exclude nothing
# across exactly the checkouts that share the stack, and a project on another
# port is not a competitor. `test/taskfile.yml` computes the suite and
# turnstile paths the same way; `bazel_accept_proofs.py` holds the two equal.
#
#   acceptance-turnstile-<port>.lock  PASSED through (taken and released) before
#                                 every suite take here; HELD by the taskfile's
#                                 pytest step for its whole run. So an xdist
#                                 run waiting for the suite lock stops new
#                                 targets from joining the shared holders, and
#                                 is not starved by an unbroken chain of them.
#   acceptance-suite-<port>.lock  SHARED here, EXCLUSIVE in `test/taskfile.yml`'s
#                                 pytest step: targets overlap each other (a
#                                 sibling checkout's too), never that step.
#   acceptance-stack-<port>.lock  EXCLUSIVE around the one-shot bring-up.
#   ~/.cache/ocx-compose.lock     EXCLUSIVE inside the bring-up; the path is
#                                 read off `src.helpers._COMPOSE_LOCK`.
#   acceptance-slot-<port>-<g>    EXCLUSIVE for the whole run, one per xdist
#                                 group in OCX_ACCEPTANCE_SLOTS, sorted.
#   <basetemp>.lock               EXCLUSIVE, innermost, around pytest: two
#                                 copies of one target (`--runs_per_test`)
#                                 share its arena, which pytest wipes at start.
#
# Always taken as `flock -o <file> <command>`: `-o` closes the lock's fd in the
# command, so a process the suite leaves behind cannot go on holding it.
# Order (`bazel.bzl` § Concurrency): turnstile (passed, never held), then suite,
# stack and compose in the barrier; turnstile, then suite, slots in sorted
# order and the arena for pytest. No cycle.
lock_dir="$HOME/.cache/ocx"
mkdir -p "$lock_dir"
port="${OCX_TEST_REGISTRY_PORT:-5000}"
suite_lock="$lock_dir/acceptance-suite-$port.lock"
turnstile="$lock_dir/acceptance-turnstile-$port.lock"

if ! command -v flock >/dev/null 2>&1; then
    # A host with no flock(1) (macOS ships none) can take none of the locks,
    # so concurrent targets would race one stack. Refused unless asked for by
    # name, through `--test_env` so the choice is part of the action key.
    [ "${OCX_ACCEPTANCE_UNSERIALISED:-}" = 1 ] ||
        fail "no flock(1) on this host - the suite, stack and slot locks cannot be taken. Install util-linux flock, or pass --test_env=OCX_ACCEPTANCE_UNSERIALISED=1 --local_test_jobs=1 to run without them"
    echo "acceptance sh_test: no flock(1), OCX_ACCEPTANCE_UNSERIALISED=1 - running with no lock, barrier or slot" >&2
    exec "$uv" run pytest "$@"
fi

# Says out loud that a lock is contended before blocking on it: a wait is
# otherwise silent, and it is spent out of the target's 900 s timeout.
waits() {
    flock -n "$1" "$2" true 2>/dev/null ||
        echo "acceptance sh_test: $module is waiting for $2" >&2
}

# Through the turnstile and out again, then the suite lock SHARED.
enter_suite() {
    waits -x "$turnstile"
    flock -o "$turnstile" true
    waits -s "$suite_lock"
}

# The stack barrier: the controller-only half of `pytest_sessionstart`, run
# once per target but only ever one at a time. The first target through does
# the `docker compose up`; every later one finds the stack reachable and
# returns. Held under the suite lock too, so no bring-up races an xdist run.
#
# And under `_COMPOSE_LOCK`, so its `compose up` cannot race a running
# session's sigstore `up -d` or registry recycle, which take that lock. Taken
# in Python, not with flock(1): the recycle path inside the hook takes it too,
# and a flock belongs to an open file description, so the hook's own attempt
# on the same file would block on the barrier's hold forever. So the barrier
# holds the real file for the whole call and points the helpers at a SECOND
# file (`<lock>.barrier`) — not a re-entrant hold: the real lock stays
# exclusive against every other process, and the hook's own take succeeds
# because it takes a different file.
barrier='import fcntl, conftest, src.helpers as helpers
lock = helpers._COMPOSE_LOCK
lock.parent.mkdir(parents=True, exist_ok=True)
with open(lock, "w") as held:
    fcntl.flock(held, fcntl.LOCK_EX)
    helpers._COMPOSE_LOCK = lock.with_name(lock.name + ".barrier")
    conftest.pytest_sessionstart(None)'
stack_lock="$lock_dir/acceptance-stack-$port.lock"
enter_suite
waits -x "$stack_lock"
flock -o -s "$suite_lock" flock -o "$stack_lock" \\
    "$uv" run python -c "$barrier" ||
    fail "the compose stack did not come up (test/conftest.py pytest_sessionstart)"

case "${OCX_ACCEPTANCE_SLOTS:-}" in
    *[!a-z0-9_\\ ]*) fail "OCX_ACCEPTANCE_SLOTS='$OCX_ACCEPTANCE_SLOTS' is not a list of [a-z0-9_] names" ;;
esac

waits -x "$basetemp.lock"
set -- flock -o "$basetemp.lock" "$uv" run pytest "$@"
# Word-split on purpose (SC2086): the list is space-separated, and the `case`
# above has already refused anything but [a-z0-9_] names and spaces.
# Prepended in REVERSE sorted order, so the first name in sorted order is the
# outermost lock and therefore the first one taken.
# shellcheck disable=SC2086
for slot in $(printf '%s\\n' ${OCX_ACCEPTANCE_SLOTS:-} | sort -ru); do
    waits -x "$lock_dir/acceptance-slot-$port-$slot.lock"
    set -- flock -o "$lock_dir/acceptance-slot-$port-$slot.lock" "$@"
done
enter_suite
exec flock -o -s "$suite_lock" "$@"
"""

def _acceptance_runner_impl(ctx):
    runner = ctx.actions.declare_file(ctx.label.name + ".sh")
    ctx.actions.write(output = runner, content = _RUNNER, is_executable = True)
    return DefaultInfo(files = depset([runner]))

_acceptance_runner = rule(
    implementation = _acceptance_runner_impl,
    doc = """Materialise the shared runner.

`ctx.actions.write` rather than a `genrule` heredoc: the script's text reaches
the file byte for byte with no shell quoting layer between, so a `$`, a
backtick or a newline in `_RUNNER` cannot change meaning on the way out.
""",
)

def _target_name(module):
    """`tests/test_install.py` -> `test_install`, matching `//test:test_install`.

    The spelling is a contract, not a convenience: `bazel_accept_proofs.py`'s
    `module_target()` maps a module to that label, and `SCOPED_ROWS` is read as
    the crate -> target selection query through it. A rename here unwires S-004
    without reddening anything in this package.
    """
    if not module.startswith("tests/") or not module.endswith(".py"):
        fail("acceptance module %r is not a `tests/*.py` path" % module)
    return module[len("tests/"):-len(".py")]

def acceptance_suite(
        name,
        modules,
        uv = "@tools//:uv",
        extra_pytest_args = None,
        module_data = None,
        module_env = None,
        module_slots = None):
    """One `sh_test` per acceptance module, plus the runner they share.

    Args:
      name: the runner target's name; the tests are named after their modules.
      modules: package-relative `tests/test_*.py` paths. Empty is refused - a
        glob that matched nothing would otherwise declare zero targets and
        `bazel build //test:all` would exit 0 over an empty package, which is
        the exact shape a count floor exists to catch.
      uv: the label of the `uv` launcher; `@tools//:uv` resolves from
        `ocx.lock`, never from `PATH`.
      extra_pytest_args: appended after the module path on every target.
      module_data: `{module: [label, ...]}` — inputs one module reads outside
        this package, declared on that module's target only (C-020), so an
        edit to one of them re-runs the modules that read it and nothing else.
      module_env: `{module: {name: value}}` — the target's `env`, `$(...)`
        location expansion included. A value naming a runfile is an
        `rlocationpath`; `_RUNNER` turns the names it knows into absolute
        paths before pytest starts.
      module_slots: `{module: [group, ...]}` — every xdist group the module
        names; the runner holds one host lock per group for the
        module's whole run (§ Concurrency). `scripts/bazel_tag_guard.py`
        derives the expected value from source and reds a mismatch.
    """
    if not modules:
        fail("acceptance_suite: no modules - `glob([\"tests/test_*.py\"])` matched nothing")
    module_data = module_data or {}
    module_env = module_env or {}
    module_slots = module_slots or {}

    # A listed module that no longer exists would add `external` to nothing and
    # read as a list entry doing its job; a stale `module_data` key would
    # declare inputs for a target nobody builds, reading like a declaration
    # doing its job. Both fail at load time.
    for module in UNCACHED_MODULES:
        if module not in modules:
            fail("UNCACHED_MODULES names %r, which is not an acceptance module" % module)
    for module in module_data.keys() + module_env.keys() + module_slots.keys():
        if module not in modules:
            fail("module_data/module_env/module_slots names %r, which is not an acceptance module" % module)

    _acceptance_runner(name = name)

    for module in modules:
        target = _target_name(module)

        sh_test(
            name = target,
            size = "medium",
            timeout = "long",
            srcs = [name],
            args = ["$(rlocationpath %s)" % uv, module] + (extra_pytest_args or []),
            data = [
                module,
                uv,
                ":suite_inputs",
                _OCX,
                _OCX_SHIM,
            ] + module_data.get(module, []),
            env = _BINARY_ENV | module_env.get(module, {}) | (
                {_SLOTS_ENV: " ".join(sorted(module_slots[module]))} if module in module_slots else {}
            ),
            env_inherit = _INHERITED_ENV,
            # Sorted, for the reason `ACCEPTANCE_TAGS` is.
            tags = sorted(ACCEPTANCE_TAGS + ([_UNCACHED_TAG] if module in UNCACHED_MODULES else [])),
        )
