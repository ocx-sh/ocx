# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""The acceptance suite as Bazel test targets — one `sh_test` per module (C-024).

`acceptance_suite()` is the whole surface: it writes one runner script and
declares one `sh_test` per `test/tests/test_*.py`, each of them tagged
`exclusive` and `no-sandbox`, each declaring its own module plus the shared
`:suite_inputs` group.

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
requirement here (the suite drives `docker compose`, a uv-managed virtualenv and
a `cargo`-built binary, none of which survive a sandboxed working directory).
Measured above: it caches exactly like an untagged sibling in both warm states.

And measured again on **this** workspace, where the probe's rc does not apply:
after a run that rebuilt `test/bin/ocx` and re-executed all 181 targets, the
binary was restored and the next `bazel test //test:all` reported
`Executed 0 out of 181 tests` in 2 s. The local action cache could not have
served that — it holds one entry per action and every one of them had just been
rewritten with the mutated binary's key — so the results came back from
`--disk_cache`, which is the tier a fresh server has and `local` would have
suppressed.

**`exclusive` stays.** One compose stack on one set of ports, so two acceptance
targets running at once share it. The same probe measured three
`exclusive`-tagged 0.7 s tests running with disjoint windows (0 overlapping
pairs) while three untagged ones overlapped pairwise, with no
`--local_test_jobs` anywhere. The flag stays off every rc file for the reason it
always was — an rc line is per-command and never per-target-pattern, so a global
one would also serialise the 34 Rust test targets, which is stage 2's entire win
— and `bazel_accept_proofs.py::global_serialisation_findings` reds one.
`task bazel:test:accept` passes it on the command line, where it is scoped to
that invocation.

## What each target declares, and what the unit honestly is

Per target: the module itself, the `uv` launcher, and `:suite_inputs` — the one
shared group carrying everything a module reads that is not the module
(`conftest.py`, `pyproject.toml`, `uv.lock`, `ocx.toml`, `ocx.lock`,
`taskfile.yml`, the floor and ceiling files, `src/**/*.py`, `bench/**` minus its
generated `results/`, the non-`test_*` helpers and the fixture tree under
`tests/**`, `scenarios/**`, `specs/**`, `sigstore/**`, `scripts/**`,
`recordings/**/*.py`, `docker/**`, `docker-compose.yml`, `zot-config.json`, and
`bin/ocx*` through `:suite_anchor`). Every service image reference and every host port
enters the key by virtue of `docker-compose.yml` being an input.

**Plus, for five targets, `extra_data`.** Five modules read their SIBLING
modules' source — `test_smoke_coverage.py` and `test_no_crate_path_assertions.py`
sweep every `tests/test_*.py`, `test_patch_global_slot.py` and
`test_doc_scripts_publish.py` sweep every `.py` under `test/`, and
`test_shell_reconcile_edge_cases.py` sweeps `test_shell*.py`. With only their own
module declared they were served a cached PASS over a tree they had never read:
a `@pytest.mark.smoke` marker deleted from `test_install.py` re-ran
`//test:test_install` and left `//test:test_smoke_coverage` CACHED. The fix is
per module, never a widening of `:suite_inputs` — that would re-run all 181 on
any module's edit and delete the selection this whole design buys. Cost, stated:
an ordinary module's edit now re-executes **5** targets (itself and the four
whole-suite sweepers) instead of 1, and a `test_shell*` module's edit re-executes
**6**. Which modules sweep is derived from their source by
`scripts/bazel_tag_guard.py`, so a sixth reds that gate rather than joining the
cached-stale set.

**Per-module fixture attribution is not feasible, and this is measured rather
than assumed:** **160 of the 181** modules carry an `import`/`from` of the one
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
  Bounded by `exclusive` plus the host-wide `flock` the runner takes, which
  together mean one stack at a time per registry port, and by the fact that the
  fixtures create their own repositories per test.
* **Five reads cross the package boundary** and are therefore undeclared
  (counted by naming each root in a module's source, `test_*.py` only):
  `target/**` (**8** modules, `test_schema_generation.py` among them — `target/`
  is `.bazelignore`d cargo output and cannot be a label at all), `website/**`
  (**7**, binding doc snippets to behaviour), `crates/**` (**5**,
  `test_deprecated_spellings.py` and `test_smoke_coverage.py` sweeping the CLI's
  own files), `.github/**` (**1**), and `test/doc_scripts/**`, which is a
  *subpackage* so no glob in this package can reach it at all. Stale-green risk:
  a change to one of those files leaves the acceptance verdict cached. Wiring
  them would need `exports_files` in four packages this file does not own plus a
  per-module `extra_data` argument, and `target/**` could not be wired at any
  price; it is declared as a gap instead of pretended away, and `//website/...`,
  `//crates/...` and `//test/doc_scripts/...` each have gates that red on their
  own content.
* **`env_inherit` values reach the test and are NOT part of the action key**, so
  a result recorded under one value is served under another. `_INHERITED_ENV`
  below carries the per-name accounting, including the two names for which the
  two values ask for different verdicts from the same test
  (`__OCX_TESTING_REQUIRE_ENVIRONMENT_D` and `CI`).

**None of these results reaches another machine**, which bounds every gap
above to the host that produced it: `.bazelrc` carries
`--remote_upload_local_results=false`, and the acceptance job holds the shared
cache's *read* credential and no write credential on any trigger. A wrong entry
here is one developer's or one runner's, and a `NOCACHE=1` (which adds
`--nocache_test_results`) clears it.

What is **not** a gap any more: the binary under test. `bin/ocx*` is a declared
input through `:suite_anchor`, so rebuilding `test/bin/ocx` with different bytes
moves every acceptance target's key and re-runs every module — A4's red half 3.

**Which of the two controls is live, exactly.** The *declaration* is enforced on
every run: `scripts/bazel_tag_guard.py` reads the graph in `task verify` and in
`verify-basic.yml`, reds an acceptance target whose input closure has lost
`//test:suite_anchor` or `//test:docker-compose.yml`, reds one that has acquired
a cache-suppressing tag, and floors what `//test:suite_inputs` itself contains.
The *binary digest* — A4's red half 3 proper, "the built and the under-test
binary are the same bytes, and moving them re-executes all 181" — is **not** in
any lane: it needs a rebuilt binary and a warm run, so it is a command a person
runs, `scripts/bazel_accept_proofs.py --check-s015 --warm BEP --mutated BEP
--tags TAGS.json --built-binary FILE --binary-under-test FILE`, with the tag
table from `task -d test bazel:tags`. Nothing here should be read as claiming it
runs on its own.
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
# the CI-runner case. Adding either back turns result caching off again, and
# `bazel_accept_proofs.py::caching_on_findings` reds on `external` by name.
ACCEPTANCE_TAGS = [
    "exclusive",
    "no-sandbox",
]

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
#   * `CI` — same shape, one branch wide: `test_schema_generation.py` skips
#     loudly when `target/release/ocx-schema` is missing under `CI` and falls
#     through to an in-job `cargo build` without it.
#   * `HOME`, `USER`, `DOCKER_CONFIG`, `SSH_AUTH_SOCK` — per-machine by
#     construction; two machines do not share an output base, so the mismatch
#     is unreachable rather than merely unlikely.
#
# The list is kept as short as the runner can work with for exactly that
# reason: every entry is a value the suite cannot synthesise, not a
# convenience.
_INHERITED_ENV = [
    # An `sh_test` receives only the names listed here, so without this line
    # `test/tests/test_schema_generation.py`'s "CI must supply the binary"
    # branch was dead — and a missing `ocx-schema` artifact fell through to the
    # in-job `cargo build` that `verify-deep.yml` exists to prevent.
    "CI",
    "DOCKER_CONFIG",
    "DOCKER_HOST",
    "HOME",
    "OCX_TEST_MIRROR_PORT",
    "OCX_TEST_REGISTRY_PORT",
    "SSH_AUTH_SOCK",
    "USER",
    "XDG_RUNTIME_DIR",
    # Same case as `CI` above. `verify-deep.yml`'s acceptance step sets this,
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

# Built by `cargo` through `task test`, never by Bazel: there is no
# `rust_binary` for `ocx` in this graph (the crates carry `rust_library` and
# `rust_test` only). A suite run against a stale or absent binary is silent,
# so its absence is a refusal rather than a skip.
ocx="$suite/bin/ocx"
[ -x "$ocx" ] || fail "no acceptance binary at $ocx - \\`task test\\` builds and copies it"

OCX_COMMAND="$ocx"
OCX_SHIM_BINARY="$suite/bin/ocx-shim"
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
[ -n "${HOME:-}" ] || fail "HOME is unset - the arena root and uv's cache both need it"
checkout=$(basename "$(dirname "$suite")")
basetemp="$HOME/.cache/ocx-pytest/$checkout-bazel"
TMPDIR="$HOME/.cache/ocx-test-tmp"
mkdir -p "$basetemp" "$TMPDIR"
export TMPDIR

set -- --basetemp="$basetemp" "$module" "$@"
[ -z "${XML_OUTPUT_FILE:-}" ] || set -- "--junit-xml=$XML_OUTPUT_FILE" "$@"

cd "$suite"

# One acceptance run at a time on this HOST. `exclusive` serialises the targets
# of one bazel invocation; it says nothing about a `task test:parallel` running
# in a sibling worktree, and both drive the same compose stack on the same
# ports. `test/taskfile.yml` computes this same path for its own pytest step,
# so the two entry points contend on one file rather than on two.
#
# Not under the repository. A `<checkout>/.agents/…` path resolves to a
# DIFFERENT file in each of the four fixed worktrees and in every agent
# worktree, so it would serialise nothing across exactly the checkouts that
# share the registry — the contention it exists to prevent. The key is the
# registry port, because the port is what identifies the contended stack: a
# second project on another port is not a competitor and must not queue.
lock_dir="$HOME/.cache/ocx"
mkdir -p "$lock_dir"
lock="$lock_dir/acceptance-suite-${OCX_TEST_REGISTRY_PORT:-5000}.lock"

if command -v flock >/dev/null 2>&1; then
    exec flock "$lock" "$uv" run pytest "$@"
fi

# Never silently: a host with no flock(1) (macOS ships none) runs unserialised,
# and whoever reads the log has to be able to see that it did.
echo "acceptance sh_test: no flock(1) on this host - running unserialised, $lock untaken" >&2
exec "$uv" run pytest "$@"
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

def acceptance_suite(name, modules, uv = "@tools//:uv", extra_pytest_args = None, extra_data = None):
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
      extra_data: `{target name: extra data labels}` for the few modules that
        read more than the shared group - today, the five that read their
        SIBLING modules' source. Per module and not a widening of
        `:suite_inputs`, because handing the whole `tests/test_*.py` set to
        every target would re-run all 181 on any module's edit and delete the
        selection C-024 buys. A key naming no module is refused: it would
        otherwise declare nothing and read exactly like a module that needs
        nothing.
    """
    if not modules:
        fail("acceptance_suite: no modules - `glob([\"tests/test_*.py\"])` matched nothing")

    extra_data = extra_data or {}
    targets = {_target_name(module): None for module in modules}
    unknown = sorted([key for key in extra_data if key not in targets])
    if unknown:
        fail(
            "acceptance_suite: extra_data names %r, which is not a target of this suite - " % unknown +
            "the key is the module stem, as in `test_smoke_coverage` for `tests/test_smoke_coverage.py`",
        )

    _acceptance_runner(name = name)

    for module in modules:
        target = _target_name(module)

        # A sweeping module is in its own sweep group - `glob(["tests/test_*.py"])`
        # matches the sweeper too - and Bazel refuses a label listed twice in one
        # `data` attribute by analysis error. Filtered here rather than in the
        # BUILD file so the groups stay plain globs a reader can check against the
        # directory.
        extra = [label for label in extra_data.get(target, []) if label != module]
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
            ] + extra,
            env_inherit = _INHERITED_ENV,
            tags = ACCEPTANCE_TAGS,
        )
