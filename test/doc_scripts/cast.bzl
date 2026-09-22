# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""The 39 cast *recordings* as build targets — one `.cast` artifact each (C-022).

`cast_targets()` is the whole surface. Per cast-enabled doc script it declares
one `genrule` that replays that script through a PTY against the local registry
and writes `casts/<slug>.cast`; plus `:manifest_drift`, which is what keeps
`CAST_SCRIPTS` below from being a second list.

Every claim here was measured on this pin and this host: Bazel 9.2.0, Linux
6.18 WSL2, `linux-sandbox` as the default strategy, `--remote_cache=` on every
invocation so nothing reaches the owner-gated realm (C-029).

## The tags, and why C-022's reason for the first one is false

C-022 wrote `no-sandbox, requires-network, local`. The set here is
`no-sandbox, requires-network, no-remote-cache`.

**`no-sandbox` — kept, but NOT for C-022's reason.** C-022 gave it on the ground
that a PTY does not survive the sandbox. WP-32 measured that it does, and so did
this package's own probe: a genrule under `linux-sandbox` calling
`pty.openpty()` got `/dev/pts/5` and exit 0. So did every other capability a
recording *reads* — an absolute-path read of the source tree (the sandbox
reconstructs the *execroot*; it does not unmount the host filesystem),
`readlink -f` on a declared input resolving to the real source path,
`getent passwd`, `/var/run/docker.sock`, and a TCP connection to
`localhost:5000`. On the stated reason the tag would be unjustified.

The real reason is **writes**, measured by running one probe twice, untagged and
`no-sandbox`:

| path the recording must write | sandboxed | `no-sandbox` |
|---|---|---|
| `<checkout>/test` (uv materialises `.venv` here) | READ-ONLY | writable |
| `$HOME/.cache/uv` (uv's cache root) | READ-ONLY | writable |
| `$HOME/.cache/ocx-pytest/…` (the pytest arena) | READ-ONLY | writable |
| `/tmp` | writable | writable |

The first sandboxed attempt failed on exactly that, before any PTY was opened:
`Could not create temporary file … Read-only file system (os error 30) at path
"/home/mherwig/.cache/uv/.tmpkBYH4g"`. Redirecting all three into `/tmp` is not
an escape — that is the reaped tmpfs `test/bazel.bzl` already moved the
acceptance arenas off, and 39 ocx homes full of package content is what would
fill it.

**`local` — dropped.** It suppresses the sandbox to the same effect as
`no-sandbox` while matching neither `sandbox_waiver_findings`'s literal
`"no-sandbox" in tags[label]` test nor `bazel_tag_guard.py`'s. Carrying it
*instead* would be reaching green by weakening the check (BZL-CORE-01). The
waiver is real, so it is spelled the way the checks can see it.

**`requires-network` — kept.** The compose registry on `localhost:<port>`, and
`uv run` reaching the network to sync the venv.

**`no-remote-cache` — added, and it keeps a host-shaped result on its host.**
These actions read host state their key cannot cover (the compose registry,
`test/.venv`, `$HOME`'s uv cache), so a result must not cross a machine
boundary, and this tag is what stops the upload and the download.

**It does NOT exclude `--disk_cache`, and the earlier claim here that it did was
measured false.** On 9.2.0, `bazel build //test/doc_scripts:cast_deps` after the
action key moved and came back reported `2 processes: 1 disk cache hit, 1
internal` with the tag in place — the BEP's `runnerCount` names the tier. The
disk cache is a per-host directory, so what the tag actually buys is the machine
boundary and nothing narrower; what keeps a *stale* result from being served on
this host is the declared-input set below, not a tag.

**`external` and `no-cache` — measured inert on a build action, so not added.**
WP-35 measured `external` as the one tag that stops a *test result* being
reused. Nobody had measured a build action, and `bazel_tag_guard.py`'s docstring
says so in as many words. Measured here, on a genrule that digests an
undeclared file, with that file changed between the two runs:

| tags on the genrule | did it re-observe? | what Bazel reported |
|---|---|---|
| `no-cache` | no | `1 process: 1 internal` (warm server) |
| `no-cache`, fresh server after `bazel shutdown` | no | `2 action cache hit` |
| `external` | no | `1 process: 1 internal` |
| `external`, `no-cache` | no | `1 process: 1 internal` |
| `local`, `external`, `no-cache` | no | `1 process: 1 internal` |

`aquery` confirms the requirement reaches the action (`executionInfo: [{"key":
"no-cache"}]`), so this is not a tag that failed to apply. **No tag makes a
build action re-execute.** Skyframe decides a build action is up to date from
its *declared inputs* alone; a cache-excluding tag governs only whether the
result may be shared. Adding one of these to look safe would be decoration —
the same shape as the `local` dodge above — so none is added, and the gap is
written down instead.

## C-022's declared inputs, and the one label that carries them

C-022 says the declared inputs are the script, `test/recordings/**`,
`test/src/**`, `conftest.py`, `pyproject.toml`, and the `ocx` binary **by
content digest**. Only the script can be named from this package: a glob never
crosses a package boundary, `//test`'s `exports_files` is visible to `//crates`
alone, and `//test:suite_anchor` is private, measured by referencing it from
here and reading back `Visibility error`. So the rest arrive as **one mandatory
parameter**, `recording_inputs`, which `BUILD.bazel` binds to
`//test:recording_inputs` — a filegroup the `//test` package owns and scopes to
this one package. Mandatory rather than defaulted: there is then no state of
this macro with the script declared and the rest silently not, which is the
state this file shipped in for one wave.

Three workarounds were tried first, and each is measurably unsound or unsafe:

* **A `no-cache` genrule that digests those files ambiently**, carried as a
  `srcs` input by all 39. This was implemented, and it is what produced the tag
  table above: it never re-observes, so its output freezes at whatever it read
  first and every downstream key freezes with it — a constant dressed as a
  digest, which is worse than an absent edge because it reads like a present
  one. Removed rather than shipped.
* **A source symlink in this package pointing at `../bin/ocx`.** Bazel digests
  the target's content, so the edge would be real — but `bin/ocx` is cargo-built
  and gitignored, so on a fresh clone the symlink dangles and `//...` fails to
  analyse for everyone.
* **`--workspace_status_command`**, the one mechanism Bazel has for ambient
  state. It is a `.bazelrc` line, and that file belongs to another work package.

**Measured on `//test/doc_scripts:cast_deps`, four real builds per row**
(`bazel build --build_event_publish_all_actions`, verdict from
`scripts/bazel_hermeticity_proofs.py --check-declared`, which needs that flag:
without it BEP publishes only *failed* actions, every label reads "not
invalidated" and the check greens over nothing):

| `srcs` | edit `test/src/doc_scripts.py` | edit `test/tests/test_install.py` | verdict |
|---|---|---|---|
| `[script]` | cache hit | cache hit | RED `hermetic-declared-input-missing` |
| `[script] + recording_inputs` | re-executed | cache hit | GREEN |

The second column is not a formality. "It rebuilt after the edit" is equally
consistent with *the inputs are declared* and with *this rule rebuilds on
everything*, and in the second state the first column's green would be vacuous.
`test/tests/**` is in the `//test` package and in no cast's input set, which is
what makes it the control rather than just another file.

What is still **not** in any key, said plainly: the compose registry stack,
`test/.venv` and `$HOME`'s uv cache. Those are host state rather than files, and
no patch makes them action inputs — the runner refuses when the registry is
unreachable instead. `no-remote-cache` is what keeps a result shaped by them on
the machine that produced it.

## `CAST_SCRIPTS`, and why it is not a second list

C-022 requires the enumeration to come from `test/scripts/doc_scripts_list.py`.
Starlark cannot read a file's bytes during loading, and the two routes that
could — a `repository_rule` or a module extension — both need a `MODULE.bazel`
edit this package does not own. So the manifest is Starlark, generated from that
enumerator, and `:manifest_drift` re-runs the enumerator at build time and fails
on any disagreement.

Three of the four drift directions were already loud before that target existed,
which is what bounds the gate's job:

| drift | what happens with no gate |
|---|---|
| a manifest entry's script is deleted | no such label — analysis error |
| a manifest entry loses `# cast: true` | pytest selects no test, `$@` unwritten — action fails |
| a manifest entry's `# doc:` slug changes | the cast lands elsewhere, `$@` unwritten — action fails |
| **a script gains `# cast: true`** | **nothing. No target, no cast, the docs page 404s** |

Only the fourth is silent, and it is the one a contributor actually hits. It is
also why `:manifest_drift` takes `glob(["*.sh"])` rather than the 39: a gate
keyed on the manifest's own members cannot be invalidated by a file the manifest
does not know about, and would answer from cache in exactly the state it exists
to catch. Both directions were watched go red, by name, on the live target.

The enumerator itself is in `recording_inputs` (`scripts/doc_scripts_list.py`,
`src/**/*.py`), which this gate carries too, so an edit to either re-runs it.

## What this package does NOT declare at all

The compose registry stack, `test/.venv` and `$HOME`'s uv cache are not action
inputs and cannot be made into them by any patch. The stack is a *precondition*
here rather than a subject: the runner refuses when the registry is unreachable,
instead of letting 39 concurrent pytest sessions each race `docker compose up`
through `pytest_sessionstart`.

A build action inherits **no** environment. Measured: the env is exactly
`PATH=/bin:/usr/bin:/usr/local/bin`, `PWD`, `SHLVL` and `TMPDIR=/tmp` — under
`local` no differently than under `linux-sandbox`. There is no `env_inherit` for
a build action and `--action_env` lives in `.bazelrc`, so `HOME` is derived from
the user database and the registry port falls back to 5000. `TMPDIR` is moved
off `/tmp` for the reason `test/bazel.bzl` already records: it is a reaped tmpfs
on the development host.
"""

visibility("private")

# ---------------------------------------------------------------------------
# The manifest. Generated from `test/scripts/doc_scripts_list.py`;
# `:manifest_drift` is what keeps it that way. Key is the file name under
# `test/doc_scripts/`, value is the `# doc:` slug, which is both the cast's
# output path and the `<Terminal src="/casts/<slug>.cast">` the website reads.
# ---------------------------------------------------------------------------

CAST_SCRIPTS = {
    "command-line__pinned.sh": "reference/command-line/pinned",
    "deps-flat.sh": "user-guide/deps-flat",
    "deps-why.sh": "user-guide/deps-why",
    "deps.sh": "user-guide/deps",
    "getting-started__env-multi.sh": "getting-started/env-multi",
    "getting-started__env.sh": "getting-started/env",
    "getting-started__exec-multi.sh": "getting-started/exec-multi",
    "getting-started__exec.sh": "getting-started/exec",
    "getting-started__find-candidate.sh": "getting-started/find-candidate",
    "getting-started__install-select.sh": "getting-started/install-select",
    "getting-started__install.sh": "getting-started/install",
    "getting-started__uninstall.sh": "getting-started/uninstall",
    "in-depth__cosign-parity.sh": "in-depth/cosign-parity",
    "in-depth__signing.sh": "in-depth/signing",
    "index.sh": "getting-started/index",
    "lazy-loading__lifecycle.sh": "lazy-loading/lifecycle",
    "package-cascade.sh": "authoring/package-cascade",
    "package-create.sh": "authoring/package-create",
    "package-describe.sh": "authoring/package-describe",
    "package-layer-reuse.sh": "authoring/package-layer-reuse",
    "package-multi-platform.sh": "authoring/package-multi-platform",
    "package-push.sh": "authoring/package-push",
    "package-test.sh": "authoring/package-test",
    "patches__consumer.sh": "user-guide/patches-consumer",
    "patches__maintainer.sh": "user-guide/patches-maintainer",
    "patches__test.sh": "user-guide/patches-test",
    "promote__dev-to-staging-to-prod.sh": "user-guide/promoting-packages",
    "select-deselect.sh": "getting-started/select-deselect",
    "shell-integration__adding-a-package.sh": "in-depth/shell-integration/adding-a-package",
    "shell-integration__cd-into-project.sh": "in-depth/shell-integration/cd-into-project",
    "shell-integration__cd-out-of-project.sh": "in-depth/shell-integration/cd-out-of-project",
    "shell-integration__inert-to-consented.sh": "in-depth/shell-integration/inert-to-consented",
    "shell-integration__toolchain-state.sh": "in-depth/shell-integration/toolchain-state",
    "user-guide__attestations-unsigned.sh": "user-guide/attestations-unsigned",
    "user-guide__attestations.sh": "user-guide/attestations",
    "user-guide__managed-config-test.sh": "user-guide/managed-config-test",
    "user-guide__toolchain-activation.sh": "user-guide/toolchain-activation",
    "user-guide__toolchain-bin-mode.sh": "user-guide/toolchain-bin-mode",
    "variants.sh": "user-guide/variants",
}

# `no-sandbox`: the recording writes to `test/.venv`, `$HOME`'s uv cache and the
# pytest arena, all of which the sandbox mounts read-only (measured, both ways).
# `requires-network`: the compose registry on `localhost:<port>`, and `uv run`
# syncing the venv. `no-remote-cache`: the same ambient reads are host-shaped,
# so a result must not cross a machine boundary. Deliberately NOT `local`, and
# deliberately no cache-excluding tag — both measured inert here; see the module
# docstring. Sorted, because `bazel query` reports tags sorted and the tag guard
# reduces that output into the table it judges.
CAST_TAGS = [
    "no-remote-cache",
    "no-sandbox",
    "requires-network",
]

# ---------------------------------------------------------------------------
# Generated files. `ctx.actions.write` rather than a `genrule` heredoc: the text
# reaches the file byte for byte with no shell quoting layer in between, so a
# `$`, a backtick or a newline cannot change meaning on the way out. Same
# reasoning `test/bazel.bzl` records for the acceptance runner.
# ---------------------------------------------------------------------------

def _written_file_impl(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ctx.attr.extension)
    ctx.actions.write(output = out, content = ctx.attr.text, is_executable = ctx.attr.executable)
    return DefaultInfo(files = depset([out]))

written_file = rule(
    implementation = _written_file_impl,
    attrs = {
        "executable": attr.bool(default = True),
        "extension": attr.string(default = ".sh"),
        "text": attr.string(mandatory = True),
    },
    doc = """Materialise a generated file whose bytes are an action input.

    Public, not `_`-private, because `gif.bzl` loads it for its own checker
    script. A second copy of a four-line rule in the sibling file is the only
    alternative Starlark offers, and two copies of a rule that writes action
    inputs is one copy too many.""",
)

# ---------------------------------------------------------------------------
# The two scripts.
# ---------------------------------------------------------------------------

# Shared prelude: resolve `test/` from a declared input, and refuse every way
# that resolution can be wrong. `bash`, not `sh`: Bazel runs genrule commands
# through bash already, and `/dev/tcp` below is a bash feature.
_PRELUDE = """
fail() {
    echo "$0: $*" >&2
    exit 1
}

# `readlink -f` on a declared input, measured on 9.2.0 to resolve to the real
# source path under `linux-sandbox` as well as under `local`. The anchor is a
# `test/doc_scripts/*.sh`, so `test/` is two directories up.
resolve_suite() {
    local anchor real
    anchor="$1"
    real=$(readlink -f "$anchor") || fail "cannot resolve the anchor input $anchor"
    suite=$(dirname "$(dirname "$real")")

    # A runfiles tree materialised as COPIES resolves to itself, and every check
    # below would then pass against a tree that is not the source tree — the
    # shape where a green says nothing. Refused by name.
    case "$suite" in
        *.runfiles/* | */bazel-out/*)
            fail "inputs resolve into $suite, which is not the source tree"
            ;;
    esac

    [ -f "$suite/pyproject.toml" ] || fail "$suite carries no pyproject.toml - wrong tree"
}

# An absolute path survives the `cd` into the source tree; a relative one does
# not, and would silently name a file under `test/` instead of under the
# execroot.
absolutise() {
    case "$1" in
        /*) printf '%s\\n' "$1" ;;
        *) printf '%s/%s\\n' "$PWD" "$1" ;;
    esac
}
"""

_CAST_RUNNER = """#!/bin/bash
# GENERATED by test/doc_scripts/cast.bzl. There is no source copy: the bytes are
# written by `ctx.actions.write`, so a change to them re-keys every cast.
#
# argv: <uv> <script> <pytest id> <cast dir> <out>
#
# One cast: replay one doc script through a PTY against the local registry and
# write `<cast dir>/<slug>.cast`. `<out>` is that same path, declared; the
# recorder derives it from the script's `# doc:` slug (`cast_layer._cast_path`),
# so a slug change lands the file elsewhere and this action fails rather than
# quietly publishing a cast under the old name.
set -euo pipefail
""" + _PRELUDE + """
[ "$#" -eq 5 ] || fail "usage: <uv> <script> <pytest id> <cast dir> <out>"
uv=$(absolutise "$1")
script="$2"
stem="$3"
cast_dir=$(absolutise "$4")
out=$(absolutise "$5")

[ -x "$uv" ] || fail "no executable uv launcher at $uv (@tools//:uv)"

resolve_suite "$script"
[ -f "$suite/recordings/test_recordings.py" ] || fail "no recorder at $suite/recordings"

# A build action inherits nothing (measured on 9.2.0: PATH, PWD, SHLVL and
# TMPDIR, under `local` no differently than under the sandbox). uv's cache root,
# ocx's default store root and the pytest arena below all need HOME, and there
# is no `--action_env` line this package could add — `.bazelrc` belongs to
# another work package. So it comes from the user database.
if [ -z "${HOME:-}" ]; then
    HOME=$(getent passwd "$(id -u)" 2>/dev/null | cut -d: -f6) || true
fi
[ -n "${HOME:-}" ] && [ -d "$HOME" ] || fail \\
    "no home directory for uid $(id -u): a build action inherits no HOME and the \\
user database gave none"
export HOME

# Built by `cargo`, never by Bazel: there is no `rust_binary` for `ocx` in this
# graph. It IS a declared input — `//test:recording_inputs` globs `bin/ocx`, so
# a rebuilt binary moves the action key and this action re-executes against it.
# The refusal below is therefore about the *absent* case only: on a fresh clone
# `bin/ocx` does not exist, the glob is `allow_empty = True`, and this line is
# what names the cause instead of a confusing pytest failure.
ocx="$suite/bin/ocx"
[ -x "$ocx" ] || fail \\
    "no recording binary at $ocx - \\`task recordings:build\\` builds and copies it \\
(cargo build --release -p ocx)"
export OCX_COMMAND="$ocx"

# The port `docker-compose.yml`, `test/conftest.py` and `test/taskfile.yml` all
# read. A build action cannot inherit an override, so a host that moved the
# registry off 5000 needs `--action_env=OCX_TEST_REGISTRY_PORT` in `.bazelrc` —
# said here because the fallback is otherwise silent.
port="${OCX_TEST_REGISTRY_PORT:-5000}"
export OCX_INSECURE_REGISTRIES="localhost:$port"

# A precondition, refused rather than started. `pytest_sessionstart` would
# `docker compose up` a missing stack, and 39 build actions racing that is not
# something a build action should be doing at all.
(exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null || fail \\
    "no registry on localhost:$port - \\`task test:up\\` starts the compose stack. \\
This target refuses to start it: 39 recordings racing one \\`docker compose up\\` \\
is not a build action's job"

# Off the reaped /tmp tmpfs (the reason `test/bazel.bzl` already records), and
# one arena PER TARGET: pytest wipes its basetemp at session start, so 39
# concurrent sessions sharing one would delete each other's ocx homes mid-run.
checkout=$(basename "$(dirname "$suite")")
basetemp="$HOME/.cache/ocx-pytest/$checkout-bazel-cast-$stem"
TMPDIR="$HOME/.cache/ocx-test-tmp"
mkdir -p "$basetemp" "$TMPDIR" "$cast_dir"
export TMPDIR

cd "$suite"

# `-p no:cacheprovider`: `.pytest_cache` is written into the source tree, and 39
# concurrent sessions would race over one directory holding a file nothing here
# reads.
"$uv" run pytest \\
    "recordings/test_recordings.py::test_record[$stem]" \\
    --cast-dir "$cast_dir" \\
    --basetemp="$basetemp" \\
    -p no:cacheprovider \\
    -q || fail "pytest exited $? recording $stem"

# pytest exiting 0 having collected nothing is exit 5, so this is belt to that
# brace: a renamed script whose id no longer matches, and a recorder that wrote
# under a different slug, both land here rather than on a missing-output message
# that names Bazel instead of the cause.
[ -s "$out" ] || fail \\
    "pytest exited 0 but no cast at $out - the recorder derives the path from the \\
script's \\`# doc:\\` slug, so a slug that no longer matches CAST_SCRIPTS lands the \\
file elsewhere"
"""

_DRIFT_GATE = """#!/bin/bash
# GENERATED by test/doc_scripts/cast.bzl.
#
# argv: <uv> <anchor> <expected> <out>
#
# `CAST_SCRIPTS` is Starlark because Starlark cannot read a file during loading,
# and the two routes that could — a repository rule, a module extension — both
# need a MODULE.bazel edit this package does not own. This is the compensating
# control: re-run the ONE enumerator C-022 names and refuse any disagreement.
set -euo pipefail
""" + _PRELUDE + """
[ "$#" -eq 4 ] || fail "usage: <uv> <anchor> <expected> <out>"
uv=$(absolutise "$1")
anchor="$2"
expected=$(absolutise "$3")
out=$(absolutise "$4")

[ -x "$uv" ] || fail "no executable uv launcher at $uv (@tools//:uv)"
resolve_suite "$anchor"

raw=$(mktemp) || fail "mktemp failed"
actual=$(mktemp) || fail "mktemp failed"
trap 'rm -f "$raw" "$actual"' EXIT

cd "$suite"
"$uv" run python scripts/doc_scripts_list.py >"$raw" \\
    || fail "test/scripts/doc_scripts_list.py exited $? - the enumeration is unreadable"

# Floor on the reader: an enumerator that printed nothing parses to an empty
# list, and an empty list would agree with an empty manifest in silence.
[ -s "$raw" ] || fail "doc_scripts_list.py wrote nothing - nothing was enumerated"

"$uv" run python -c '
import json, os, sys

export = json.load(open(sys.argv[1], encoding="utf-8"))
if not export:
    sys.exit("doc_scripts_list.py enumerated zero scripts")
rows = sorted(
    (os.path.basename(e["path"]), e["slug"]) for e in export if e["cast"]
)
for name, slug in rows:
    print(name, slug)
' "$raw" >"$actual" || fail "could not reduce the enumeration to <script> <slug> rows"

if ! diff -u "$expected" "$actual"; then
    fail \\
"CAST_SCRIPTS in test/doc_scripts/cast.bzl disagrees with test/scripts/doc_scripts_list.py.
The diff above is expected-vs-actual: a '+' line is a cast script with no target
(its cast is never recorded and the docs page 404s), a '-' line is a manifest
entry the enumerator no longer reports. Regenerate CAST_SCRIPTS from
\\`uv run python scripts/doc_scripts_list.py\\` in test/."
fi

cp "$actual" "$out"
"""

# ---------------------------------------------------------------------------
# The macro.
# ---------------------------------------------------------------------------

def _target_name(script):
    """`getting-started__install.sh` -> `cast_getting-started__install`.

    The spelling is the one `bazel_hermeticity_proofs.py` already writes when it
    names a cast label (`//test/doc_scripts:cast_<n>`), and the suffix is also
    the pytest parametrisation id: `conftest.pytest_generate_tests` ids each
    case by `path.stem`, which is what the runner selects on.
    """
    if not script.endswith(".sh"):
        fail("cast script %r is not a `.sh` file" % script)
    return "cast_" + script[:-len(".sh")]

def cast_targets(name, scripts, recording_inputs, uv = "@tools//:uv"):
    """One `genrule` per cast-enabled doc script, plus the drift gate.

    Args:
      name: the `filegroup` collecting every `.cast` this package produces —
        `//test/doc_scripts:<name>` is the one label a consumer needs. Each file
        lands at `casts/<slug>.cast` under this package's output root, so the
        site's `src/public/casts/<slug>.cast` is that path with the package
        prefix stripped.
      scripts: **every** `*.sh` in this package, not only the cast ones. The
        drift gate is keyed on this set precisely so that adding a script the
        manifest does not know about invalidates it; keyed on `CAST_SCRIPTS`
        alone it would answer from cache in the one state it exists to catch.
      recording_inputs: the label carrying C-022's rest — `test/recordings/**`,
        `test/src/**`, `conftest.py`, `pyproject.toml`, `uv.lock`, the
        enumerator and `bin/ocx`. Mandatory: a cast whose key covers its own
        script and not the recorder is served warm against a binary that no
        longer exists, and that state is not reachable from this signature.
      uv: the `uv` launcher's label; `@tools//:uv` resolves from `ocx.lock`,
        never from `PATH`.
    """
    if not scripts:
        fail("cast_targets: no scripts - `glob([\"*.sh\"])` matched nothing")

    missing = [s for s in CAST_SCRIPTS if s not in scripts]
    if missing:
        fail(
            "cast_targets: CAST_SCRIPTS names %d script(s) this package does not have: %s" %
            (len(missing), ", ".join(sorted(missing))),
        )

    # The anchor the drift gate resolves `test/` from. Any source file in this
    # package would do; taking the first script keeps it a file the manifest
    # already guarantees exists, so there is no marker file to delete by
    # accident.
    anchor = sorted(CAST_SCRIPTS)[0]

    written_file(name = "cast_runner", text = _CAST_RUNNER)
    written_file(name = "drift_probe", text = _DRIFT_GATE)
    written_file(
        name = "manifest_expected",
        executable = False,
        extension = ".txt",
        text = "".join([
            "%s %s\n" % (script, CAST_SCRIPTS[script])
            for script in sorted(CAST_SCRIPTS)
        ]),
    )

    native.genrule(
        name = "manifest_drift",
        srcs = scripts + [":manifest_expected", recording_inputs],
        outs = ["manifest.txt"],
        cmd = "$(location :drift_probe) $(location %s) $(location %s) $(location :manifest_expected) $@" % (uv, anchor),
        tools = [":drift_probe", uv],
        tags = CAST_TAGS,
    )

    for script in sorted(CAST_SCRIPTS):
        native.genrule(
            name = _target_name(script),
            srcs = [script, recording_inputs],
            outs = ["casts/%s.cast" % CAST_SCRIPTS[script]],
            cmd = "$(location :cast_runner) $(location %s) $(location %s) %s $(RULEDIR)/casts $@" % (
                uv,
                script,
                script[:-len(".sh")],
            ),
            tools = [":cast_runner", uv],
            tags = CAST_TAGS,
        )

    native.filegroup(name = name, srcs = [":" + _target_name(s) for s in sorted(CAST_SCRIPTS)])
