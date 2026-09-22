# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""The VitePress site as one coarse build target (C-023).

`site_target()` is the whole surface. It declares a single `genrule` that runs
`@tools//:bun install --frozen-lockfile` and then `bun x vitepress build`, and
writes the rendered site as one `.tar`.

Every claim below was measured on this pin and this host: Bazel 9.2.0, Linux
6.18 WSL2, `linux-sandbox` as the default strategy, `--remote_cache=` never
passed so nothing reaches the owner-gated realm (C-029).

## The BZL-JS-01 / BZL-JS-03 exemption — named, not implied

**This rule takes no `npm_translate_lock` and adopts no `rules_js`.** Both
`BZL-JS-01` and `BZL-JS-03` are MUST-severity in the vendored `bazel-quality`
bundle, and both are scoped to rules_js ingestion — a premise that does not
obtain here. The escape is the skill's own, at
`bazel-adopt/references/branches-python-ts-cpp.md:108-111`:

> **Adopting for a repository not already on pnpm is a "no" as a starting
> move.** The cost — conversion, `hoist: false`, phantom-dependency fixes — is
> paid before any Bazel benefit arrives. **The answer flips only when the JS
> package is a minority slice of a polyglot migration already justified by
> other languages.**

The website is a minority slice; the migration is justified by Rust. The
reasoned ruling — including that
[rules_js#1258](https://github.com/aspect-build/rules_js/issues/1258) is open
and unfunded and that no maintained Bun ruleset exists in the BCR catalogue —
is `adr_bazel_build_adoption.md` § "The BZL-JS exemption". That ADR does not
edit the vendored rule file, and neither does this one: the exemption lives in
the ADR and in this comment, which is the compensating control the ADR names.

**The ceiling, stated.** A coarse rule buys coarse-grained input-hash skipping
— the site rebuilds or it does not. No per-file JS caching, no `ts_project`
typecheck target, no incremental transpile. That is the price of the exemption.

## The tags, and what `no-remote-cache` does and does not do

`SITE_TAGS` is `no-remote-cache, requires-network`. C-023 gives this rule
`requires-network` only; `no-remote-cache` rides on top of it as a **shipping**
tag, and removing it is the one thing gated on WP-32's three greens
(`scripts/bazel_hermeticity_proofs.py --check-declared|--check-ambient|
--check-tool-pin` against `//website:site`). Gating the rule's *creation* on
proofs that must run against it would be a cycle, so the rule ships carrying
the tag.

**`no-remote-cache` is about what reaches the shared cache. It stops no reuse
at all.** WP-33 measured the general fact on a genrule digesting an undeclared
file: `no-cache`, `external`, `local` and every combination of them still
re-used the result. `external` is the only tag that stops a *test result* being
reused, and **no** tag stops a *build action* being reused — Skyframe decides a
build action's up-to-dateness from declared inputs alone. So this tag's whole
effect is that an entry built here never crosses a machine boundary. On this
machine, an unsound entry is served exactly as it would be without it, which is
why check (a) below is the thing that matters and not the tag.

**All three checks were driven against this rule, and all three are red.** The
tag is therefore not a formality — each red is a distinct reason an entry from
here must not cross a machine boundary, and each has a named owner:

| check | verdict on `//website:site` | what clears it |
|---|---|---|
| (a) declared-input invalidation | `hermetic-declared-input-missing` | the one-label patch below; **green the moment it lands**, measured |
| (b) ambient-input isolation | `hermetic-nondeterministic-output` | vitepress's local-search index (see below) — not this rule's to fix |
| (c) tool-pin participation | `hermetic-tool-repo-stale` | `ctx.watch` in rules_ocx's eager form; owner-gated |

(b) was three genuinely cold builds — fresh `--output_base` *and* fresh
`--disk_cache` — of one unchanged tree, producing three different tars of
identical length. 124 of the 170 files are byte-identical; what moves is
`assets/chunks/@localSearchIndexroot.*.js`, and it moves in **length**
(1 232 715 vs 1 232 640 bytes). The first divergence is at byte 871, in the
index's document-id table: id 15 is `/docs/authoring/dependencies#...` in one
build and `/docs/authoring/bundle-anatomy#...` in the other. VitePress's local
search assigns ids in page-processing order, which is transform-completion
order, which is scheduling-dependent. The other seven differing assets —
`app`, `theme`, `VPLocalSearchBox`, `index.md`, `team.md` — differ only in the
8-character content hashes that cascade from it, at identical length. It is
**not** a clock, a hostname or a path.

(c) reproduced WP-31's measurement on this rule rather than on a probe: bun
pinned `1` (→ 1.3.14) vs `1.4.0`, same `--disk_cache`. Plain build after the
lock bump → **no re-execution, output unchanged**; `bazel fetch --force
--repo=@tools` then build → re-executed, `bun install v1.4.0`, output moved. So
the tool digest **does** sit in the action key and C-023's "expected green by
construction" is right about the key and wrong about the outcome: the
repository does not refetch, because `@tools`'s marker records no `FILE:` entry
for `ocx.toml` or `ocx.lock`. `marker_watch_findings` reports the same defect
statically, with no build.

**No `no-sandbox`, deliberately.** `sandbox_waiver_findings` reds
`hermetic-site-no-sandbox` on a site rule that carries it, and the tag would be
a hermeticity waiver nothing else is looking at. It is also not needed, which
was measured rather than assumed: the reason the 39 cast targets need it is
*writes* outside the execroot (`test/.venv`, `$HOME/.cache/uv`, the pytest
arena), and this action writes none of those. `bun install` does write
`node_modules/` and a package cache, and both are redirected into the execroot
below (`HOME`, `BUN_INSTALL_CACHE_DIR`), which the sandbox leaves writable. If
a future change makes the tag necessary, that is a finding against C-023 and
belongs in a report, not in this list.

## The gap this package cannot close, and the one-label patch that does

**C-023 requires `//test/doc_scripts/**/*.sh` in the declared inputs**, and they
are not here. `//test/doc_scripts/` is its own Bazel package, a `glob` never
crosses a package boundary, and those sources are not exported. Measured on
9.2.0 — a `filegroup` in this package naming `//test/doc_scripts:index.sh`
fails analysis with

    Visibility error: target '//test/doc_scripts:index.sh' is not visible from
    target '//website:probe_visibility'
    Recommendation: ... use the exports_files() function

so the label is unreachable from here by construction, not by omission.
`//test/doc_scripts:casts` and `//test:recording_inputs` are equally out of
reach — the first is package-private, the second is visible to
`//test/doc_scripts:__pkg__` alone.

**The patch `//test/doc_scripts`'s owner applies**, after which
`site_target(doc_scripts = ["//test/doc_scripts:doc_script_sources"])` closes it
in one line in `website/BUILD.bazel`:

    filegroup(
        name = "doc_script_sources",
        srcs = glob(["*.sh"], allow_empty = False),
        visibility = ["//website:__pkg__"],
    )

A `filegroup` rather than `exports_files`, and the difference from the two
`exports_files` lists already in `//test` is deliberate: those exist so each
crate depends on the individual fixture it reads. This consumer is coarse by
contract — it rebuilds or it does not — so one label is the honest shape, and
the `glob` is the same one `cast_targets` already takes, so a new script joins
both sets at once.

**Until that lands, what a cached site can outlive.** A doc-script body edit.
`website/src/_scripts/**` — the *published* copies VitePress actually reads —
is declared, so the edit does invalidate once `task website:scripts:publish`
has run; what is not covered is the window between the edit and the publish, in
which the action key has not moved and the cache answers with the old site.
That is exactly S-014's red, and `--check-declared` reports it by name
(`hermetic-declared-input-missing`). It is also a *regression against*
`task website:build`, whose `scripts:publish` stage is unconditional — stated
plainly because it is the one thing a reader of this file most needs to know.

## Why the published copies are an input at all

`bun x vitepress build` on a tree with no `website/src/_scripts/` **fails**,
measured:

    build error:
    [vitepress] ENOENT: no such file or directory, stat
      '.../website/src/_scripts/getting-started/exec.sh'
    file: .../website/src/docs/getting-started.md

Thirty-odd pages carry `<<< @/_scripts/<slug>.sh{sh}`, VitePress `stat`s each
one while rendering, and one miss aborts the build. So the published tree is a
hard prerequisite rather than a nicety — and it is the *only* one. The same
tree with no generated schemas, no `.cast` files, no `src/public/data/` and no
`src/docs/reference/dependencies.md` builds clean, exit 0. That is why this
rule declares `src/**` (which picks the published copies up when they are
there) and refuses at action time when they are not, naming the task that
writes them.

## What this stage costs, measured — and A3's second abort trigger

`adr_bazel_build_adoption.md` § Acceptance A3 carries: *"WP-1c measures the
`bunx vitepress build` chain at under 3 minutes cold … → the site rule is not
written"*, on the ground that the exemption above is otherwise purchased
against an unsized cost.

Three genuinely cold builds of this rule — fresh `--output_base` **and** fresh
`--disk_cache`, `bun install` fetching all 147 packages over the network —
took **19.0 s, 16.7 s and 16.7 s** wall clock on this host. Outside Bazel, on a
warm bun cache, the same two commands are 0.3 s + 5.8 s. That is an order of
magnitude under the trigger.

It is recorded here rather than acted on: A3's number is WP-1c's to measure and
the owner's to move, the trigger's other half (invocation frequency over the
last 50 `deploy-website` runs) is not measured here at all, and the *chain*
A3 names is the five-stage `task website:build`, of which this rule is the
fifth stage only. A reader deciding whether this rule pays for itself should
start from these three numbers.

## What this rule does NOT own, and why the Taskfile path stays

`website/taskfile.yml`'s `build` is five stages: `schema:default`,
`scripts:publish`, `recordings:parallel`, `sbom:generate:page`, then
`bunx vitepress build`. **This rule is the fifth stage only.** The first four
need `cargo`, `uv`, a compose registry and a network fetch of `agg`; they stay
on Taskfile, which is also the rollback path the plan's partial-failure state
depends on. `task website:build` is unchanged and still works end to end.

One consequence is user-visible and is not a rollback concern:
`OCX_DEPLOY_TARGET=prod` changes the rendered head (`config.mts` reads it at
build time), a build action inherits **no** environment, and `--action_env`
lives in `.bazelrc`, which belongs to another work package. **So this rule can
only ever produce the `dev` variant**, and the production deploy stays on the
Taskfile path until that line exists.

## The output is a `.tar`, and that is a deviation with a reason

The ADR says "Output: the site directory". It is a single tar file here,
because WP-32 — the harness that gates this rule's tag — reads an artifact with
`digest_of(path)`, which is `path.read_bytes()`. On a `declare_directory` tree
artifact that raises `OSError`, `digest_of` returns `""`, and `""` compares
equal to `""`: checks (b) and (c) would both green over nothing read. A
directory output would make the gate vacuous, so the rule emits one file the
gate can actually hash, and `task website:bazel:build` unpacks it.

The tar is written `--sort=name --mtime=@0 --owner=0 --group=0
--numeric-owner`, so tar's own metadata cannot be the thing that differs when
check (b) compares two cold builds. Whatever difference survives that is
VitePress's.
"""

visibility("private")

# `no-remote-cache`: this rule's hermeticity is not yet proven, and the whole
# effect of the tag is that an entry never crosses a machine boundary (see the
# module docstring — it stops no local reuse). `requires-network`:
# `bun install` fetches the 147-package closure, and the in-execroot package
# cache below means it fetches on every genuinely cold run. Sorted, because
# `bazel query` reports tags sorted and the tag guard reduces that output into
# the table it judges.
SITE_TAGS = [
    "no-remote-cache",
    "requires-network",
]

# ---------------------------------------------------------------------------
# The runner, materialised by `ctx.actions.write` rather than inlined in a
# `genrule` heredoc: the text reaches the file byte for byte with no shell
# quoting layer in between, so a `$`, a backtick or a newline cannot change
# meaning on the way out. Same reasoning `test/bazel.bzl` and
# `test/doc_scripts/cast.bzl` already record.
# ---------------------------------------------------------------------------

def _written_file_impl(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".sh")
    ctx.actions.write(output = out, content = ctx.attr.text, is_executable = True)
    return DefaultInfo(files = depset([out]))

_written_file = rule(
    implementation = _written_file_impl,
    attrs = {"text": attr.string(mandatory = True)},
    doc = "Materialise a generated script whose bytes are an action input.",
)

_SITE_RUNNER = """#!/bin/bash
# GENERATED by website/site.bzl. There is no source copy: the bytes are written
# by `ctx.actions.write`, so a change to them re-keys the site action.
#
# argv: <bun> <website/package.json> <out.tar>
#
# Unlike the cast runner, this one never leaves the EXECROOT: it resolves no
# declared input back to the source tree, everything it reads is declared, and
# everything it writes belongs to the action. That is what lets it run
# sandboxed with no `no-sandbox` waiver.
set -euo pipefail

fail() {
    echo "$0: $*" >&2
    exit 1
}

# An absolute path survives the `cd` into the package directory; a relative one
# does not, and would silently name a file under `website/` instead of under
# the execroot.
absolutise() {
    case "$1" in
        /*) printf '%s\\n' "$1" ;;
        *) printf '%s/%s\\n' "$PWD" "$1" ;;
    esac
}

[ "$#" -eq 3 ] || fail "usage: <bun> <website/package.json> <out.tar>"
bun=$(absolutise "$1")
site=$(dirname "$2")
out=$(absolutise "$3")

[ -x "$bun" ] || fail "no executable bun launcher at $bun (@tools//:bun)"
[ -d "$site" ] || fail "no package directory at $site"

cd "$site"

[ -f .vitepress/config.mts ] || fail \\
    "no .vitepress/config.mts in $PWD - the site sources are not in the execroot, \\
which means the glob in website/BUILD.bazel stopped matching"

# A build action inherits NO environment (measured on 9.2.0: PATH, PWD, SHLVL
# and TMPDIR, under `local` no differently than under the sandbox). bun still
# wants a HOME for its install cache and would otherwise take it from the user
# database and try to write `$HOME/.bun`, which the sandbox mounts read-only.
# Both are redirected into the execroot, which is the action's own writable
# space — this is the reason the rule needs no `no-sandbox`.
HOME="$PWD/.bazel-home"
BUN_INSTALL_CACHE_DIR="$HOME/.bun-cache"
export HOME BUN_INSTALL_CACHE_DIR
mkdir -p "$BUN_INSTALL_CACHE_DIR"

# The one hard prerequisite, refused by name rather than as a VitePress ENOENT
# five stages later. These are the published doc-script snippets that ~30 pages
# include with `<<< @/_scripts/<slug>.sh`; `task website:scripts:publish`
# writes them and they are gitignored, so a fresh clone has none.
#
# A count, not a `[ -d ]`: an empty directory is the state this refusal exists
# to catch, and a directory test passes over it.
#
# `find -L`, and the `-L` is load-bearing: Bazel materialises a declared source
# input into the execroot as a SYMLINK to the source tree, so plain `-type f`
# reports 0 over a complete input set. Measured — the first run of this rule
# failed on this refusal with all 75 snippets present in `srcs`.
published=$(find -L src/_scripts -type f -name '*.sh' | wc -l)
[ "$published" -gt 0 ] || fail \\
    "no published doc-script snippets under $PWD/src/_scripts - \\`task \\
website:scripts:publish\\` writes them (they are gitignored build output, and \\
~30 pages \\`<<<\\`-include them, so vitepress aborts on the first miss)"

# Build from a DEREFERENCED copy, not from the execroot's symlink forest.
#
# Measured, and this is the whole reason the step exists: vite resolves a
# symlinked module to its real path (`preserveSymlinks` is false by default),
# so every page's id came back as the SOURCE path while the page universe was
# keyed on execroot paths, and vitepress reported `2252 dead link(s) found` on
# a tree that builds clean outside Bazel. `cp -RL` makes every input a real
# file under the execroot, which is also the stronger hermeticity position: no
# path inside the action points out of it.
work="$PWD/.bazel-site"
rm -rf "$work"
mkdir -p "$work"
trap 'rm -rf "$work"' EXIT
cp -RL .vitepress src package.json bun.lock "$work/" \\
    || fail "could not dereference the site inputs into $work"
cd "$work"

"$bun" install --frozen-lockfile \\
    || fail "bun install --frozen-lockfile exited $? - bun.lock disagrees with package.json, \\
or the network this action declares with requires-network was unreachable"

# `bun x`, not `bunx`: `ocx.project` exposes one target per DECLARED BINARY and
# the bun package declares only `bun` (`@tools//:uvx` exists because the uv
# package declares two). `bun x` is the same code path as the `bunx` shim, and
# after the install above it resolves vitepress out of ./node_modules rather
# than off the network.
"$bun" x vitepress build \\
    || fail "vitepress build exited $?"

dist=.vitepress/dist
[ -d "$dist" ] || fail "vitepress exited 0 but wrote no $PWD/$dist"

# Floor on the reader, not on the exit code: vitepress rendering zero pages
# into an existing directory would tar up cleanly and hash consistently, and a
# consistent hash is the green side of check (b).
pages=$(find "$dist" -type f -name '*.html' | wc -l)
[ "$pages" -gt 0 ] || fail "$PWD/$dist holds 0 rendered pages"

# Deterministic metadata, so that what check (b) compares is vitepress's output
# and never tar's framing of it.
tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner --format=gnu \\
    -cf "$out" -C "$dist" . \\
    || fail "tar exited $? packing $PWD/$dist"

[ -s "$out" ] || fail "tar exited 0 but wrote nothing to $out"
echo "site: $pages rendered pages packed from $PWD/$dist" >&2
"""

# ---------------------------------------------------------------------------
# The macro.
# ---------------------------------------------------------------------------

def site_target(name, srcs, doc_scripts, bun = "@tools//:bun"):
    """One coarse `genrule` producing `<name>.tar` — the rendered site.

    Args:
      name: the target name. `//website:site` is the label the ADR, the
        Taskfile and `scripts/bazel_hermeticity_proofs.py` all spell.
      srcs: every file VitePress reads, as one glob from `website/BUILD.bazel`.
        Coarse on purpose: the rule rebuilds or it does not, so a per-file edge
        would buy nothing and cost a list that goes stale.
      doc_scripts: the `//test/doc_scripts/*.sh` sources, as labels. C-023
        requires them; they are not reachable from this package until
        `//test/doc_scripts` exports them, and the exact patch is in this
        file's docstring. Pass `[]` to ship without the edge — and expect
        `--check-declared` to red `hermetic-declared-input-missing`, which is
        S-014 doing its job rather than a gate to silence.
      bun: the bun launcher's label; `@tools//:bun` resolves from `ocx.lock`,
        never from `PATH`.
    """
    if not srcs:
        fail("site_target: no srcs - the glob in website/BUILD.bazel matched nothing")

    _written_file(name = name + "_runner", text = _SITE_RUNNER)

    native.genrule(
        name = name,
        srcs = srcs + doc_scripts,
        outs = [name + ".tar"],
        cmd = "$(location :%s_runner) $(location %s) $(location package.json) $@" % (name, bun),
        tags = SITE_TAGS,
        tools = [":" + name + "_runner", bun],
        visibility = ["//visibility:public"],
    )
