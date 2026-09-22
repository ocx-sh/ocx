# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""The 39 cast recordings rendered to animated GIFs through `@tools//:agg`.

`gif_targets()` is the whole surface: one `genrule` per entry of `cast.bzl`'s
`CAST_SCRIPTS`, taking that cast's *target* — never a file on disk — and writing
`gifs/<slug>.gif`; plus `:gif_check`, an `sh_test` that reads every one of them
and asserts three structural properties of the bytes.

It reuses `CAST_SCRIPTS` rather than enumerating anything: a second list here
would be a second source of truth for "which docs have a recording", and
`cast.bzl`'s `:manifest_drift` gate — which is what keeps that list honest
against `test/scripts/doc_scripts_list.py` — is keyed on the manifest, not on
this file.

Measured on this pin and this host: agg 1.9.0 from `ocx.sh/asciinema/agg:1.9.0`,
Bazel 9.2.0, Linux 6.18 WSL2, `--remote_cache=` on every invocation so nothing
reached the owner-gated realm (C-029).

## The tag, and the tag this package does NOT carry

`no-remote-cache` alone. Deliberately **not** `no-sandbox` and not
`requires-network`, which the 39 recording targets next door all carry:

| capability | a cast recording | a GIF render |
|---|---|---|
| a PTY | yes | no |
| the `localhost:<port>` registry | yes | no |
| writes outside the execroot (`test/.venv`, the uv cache, the pytest arena) | yes | no |
| reads ambient host state | the whole suite | **the font set** |

agg reads one declared `.cast` and writes one declared `.gif`. It was measured
under `linux-sandbox` — the default strategy — and exits 0: fontconfig resolves
through `/etc/fonts` and `/usr/share/fonts`, which the sandbox reconstructs
read-only rather than unmounting, the same property `cast.bzl` records for an
absolute-path source read. So `no-sandbox` would be a decoration, and the tag
guard's clause 1 (`scripts/bazel_tag_guard.py`, `BUILD_ACTION_EXCLUDING_TAGS`)
never acquires these targets as a subject.

`no-remote-cache` is not a decoration, and the row above says why. The **font is
ambient**: nothing in the action's declared inputs pins which typeface renders
the text, so two hosts with different fonts installed produce different pixels
from identical inputs. That is precisely the state the tag exists for — *an
action that may read undeclared inputs must not have its result offered to
another machine.* It does not exclude `--disk_cache`, which is correct here: the
font set is a property of the host, and the disk cache never leaves it.

`:gif_check` carries no *cache* tag, and that is not an oversight. Its inputs are
the 39 rendered GIFs, in its action key by content digest, so a result reused
across machines is a result about those exact bytes. The host-shapedness stops at
the producer. It does carry `manual`, which is a different question — below.

## `manual`, on all 42, and the one consumer that is left

Nothing in this repository reads a rendered GIF. The site plays the `.cast`
files through `Terminal.vue`; the GIFs are for a README and for social embeds,
which is a human picking one file, not a build step. So 39 agg invocations on
every `bazel build //...` bought a wildcard green over artefacts with no reader,
and `manual` takes them out of wildcard expansion (`//...`, `:all`) while leaving
them addressable by label — `bazel query` still lists all 42, which is why the
tag guard's reader floor does not move.

The tag is on **all 42**, and the two that are not renders are the load-bearing
ones. `manual` governs *wildcard expansion only*: a target Bazel reaches through
a dependency edge is built regardless. `:gifs` holds the 39 as srcs and
`:gif_check` holds them as `data`, so either one left visible pulls all 39 back
into `bazel build //...` and the other 40 tags become decoration. Measured on
this pin: `bazel build --nobuild //...` went 322 -> 280 targets, and
`bazel query 'kind(rule, //...)'` answered 322 both times.

What keeps the render honest is that `manual` is not "off": the consumer is
`website/recordings.taskfile.yml`'s `gifs` task, and it names `:gif_check` — the
proof — not just `:gifs`. A `manual` proof that no task names is how a proof
stops running, so the tag and that task line are one change, and removing either
alone is a defect.

## The font, which is the ceiling on all of this

ponytail: the font list below is ambient host state, and the upgrade path is a
declared font. C-021 already reserves the carve-out — "the Nerd Font stays an
`http_archive` with `integrity`, because it is data, and there is no `@tools//`
route for data even in principle" (`MODULE.bazel:109-112`). Taking it would make
every GIF byte-reproducible and let `no-remote-cache` come off. It needs a
`MODULE.bazel` stanza, which this package does not own; the patch is in the
work package's handover rather than half-applied here.

Until then the list is a **widened fallback**, not a pin, and the widening is
measured rather than guessed:

* agg 1.9.0's own default is
  `JetBrains Mono,Fira Code,SF Mono,Menlo,Consolas,DejaVu Sans Mono,Liberation Mono`.
* `ubuntu-latest` carries DejaVu, so CI resolves on the default list alone.
* This development host (Fedora 43, WSL2) carries **neither DejaVu nor
  Liberation**: `fc-list : family spacing` answers exactly two monospace
  families, `Adwaita Mono` and `Nimbus Mono PS`. On the default list agg exits 1
  with `no faces matching font family options` and writes nothing.

So the list below is agg's default plus the families a Linux host is actually
likely to have. Widening it is safe in the one direction that matters: agg
**refuses** when no family matches — it does not silently substitute — so a host
missing every entry fails the build out loud rather than rendering blanks.

A companion `doctor` script (a separate work package) reports the same condition
up front, as advice rather than a gate. Note for whoever writes it: `fc-list` is
not on a plain `PATH` on this host — it is `/usr/sbin/fc-list` — so a probe
spelled `fc-list | grep -qi dejavu` answers "no font" here for the wrong reason,
and would answer it identically on a host with the font installed and the binary
elsewhere. Resolve the binary before reading its output.
"""

# `sh_test` is not a native rule on a Bazel 9 pin — `test/bazel.bzl` records the
# same load and the load-time error its absence produces.
load("@rules_shell//shell:sh_test.bzl", "sh_test")
load(":cast.bzl", "CAST_SCRIPTS", "written_file")

# The same declaration `cast.bzl` and `test/bazel.bzl` carry, and it is what
# lets this file load `written_file` at all: `cast.bzl` is `visibility("private")`
# too, which is package-scoped, and both files are `//test/doc_scripts`.
visibility("private")

# agg 1.9.0's `--text-font-family` default, plus five families a Linux host may
# have when it has none of agg's. Ordered most-preferred first: CI resolves at
# `DejaVu Sans Mono` and never reaches the tail. See the module docstring for
# why widening is the safe direction and what replaces this entirely.
GIF_TEXT_FONT_FAMILY = ",".join([
    "JetBrains Mono",
    "Fira Code",
    "SF Mono",
    "Menlo",
    "Consolas",
    "DejaVu Sans Mono",
    "Liberation Mono",
    "Noto Sans Mono",
    "DejaVu Sans Mono Book",
    "Ubuntu Mono",
    "Adwaita Mono",
    "Nimbus Mono PS",
])

# See the module docstring's tag table for `no-remote-cache`, and its `manual`
# section for the second. One cache tag, the two next door that this package
# deliberately does not repeat, and the wildcard exclusion.
GIF_TAGS = ["no-remote-cache", "manual"]

# ---------------------------------------------------------------------------
# The checker.
#
# Floors, all measured across the 39 casts on this host rather than chosen:
# every cast is a 100-column terminal, so every GIF is 979px wide; heights run
# 134px (`authoring/package-create`, 5 rows) to 829px
# (`in-depth/shell-integration/adding-a-package`, 36 rows); frame counts run 29
# to 75. The floors sit far below the measured minimum on purpose — they are
# there to reject a degenerate render (a 1x1, a single still frame), not to
# pin a layout that a font change legitimately moves.
# ---------------------------------------------------------------------------

MIN_GIF_WIDTH = 256
MIN_GIF_HEIGHT = 64
MAX_GIF_DIMENSION = 65535  # the format's own ceiling: both fields are u16
MIN_GIF_FRAMES = 2

_GIF_CHECK_TEMPLATE = """#!/bin/bash
# GENERATED by test/doc_scripts/gif.bzl. There is no source copy: the bytes are
# written by `ctx.actions.write`, so a change to them re-keys this test.
#
# argv: <expected count> <gif>...
#
# Three structural properties per GIF, and a floor on the reader. Deliberately
# NOT a byte or digest comparison: a GIF is not reproducible across hosts — the
# font decides the pixels — so a golden digest would be a gate that fails on
# every machine but the one that recorded it, which is the shape that gets
# disabled rather than fixed.
set -euo pipefail

fail() {
    echo "gif_check: $*" >&2
    exit 1
}

[ "$#" -ge 1 ] || fail "usage: <expected count> <gif>..."
expected="$1"
shift

# The reader floor. A `data` attribute that resolved to nothing would leave the
# loop below with no iterations and every assertion in it vacuous, and an empty
# loop exits 0. This is what makes that state a failure instead of a green.
[ "$expected" -ge 1 ] || fail "expected count is $expected - nothing would be read"
[ "$#" -eq "$expected" ] || fail \\
    "$# GIF arguments for an expected $expected - the runfiles set and the manifest disagree"

checked=0
for gif in "$@"; do
    [ -s "$gif" ] || fail "$gif is missing or empty"

    magic=$(head -c 6 "$gif")
    [ "$magic" = "GIF89a" ] || fail "$gif opens with '$magic', not the GIF89a signature"

    # Bytes 6..9 of the logical screen descriptor: width then height, each a
    # little-endian u16. Read as unsigned BYTES and recombined by hand rather
    # than with `od -tu2`, whose word order is the host's - a detail that would
    # make this check quietly wrong on a big-endian machine instead of loudly.
    read -r b0 b1 b2 b3 <<<"$(od -An -tu1 -j6 -N4 "$gif")"
    width=$(( b0 + 256 * b1 ))
    height=$(( b2 + 256 * b3 ))

    [ "$width" -ge @MIN_WIDTH@ ] && [ "$width" -le @MAX_DIM@ ] || fail \\
        "$gif is ${width}px wide, outside @MIN_WIDTH@..@MAX_DIM@ - not a rendered terminal"
    [ "$height" -ge @MIN_HEIGHT@ ] && [ "$height" -le @MAX_DIM@ ] || fail \\
        "$gif is ${height}px tall, outside @MIN_HEIGHT@..@MAX_DIM@ - not a rendered terminal"

    # Graphic Control Extension blocks: `21 f9 04`, one per animation frame.
    # Counted over SPACE-SEPARATED hex so the match is byte-aligned; on a bare
    # hex string the same three bytes match across a nibble boundary too, which
    # over-counts and would let a still image clear the floor below. Validated
    # against a byte-wise scan of three real renders (29 / 42 / 75 frames) and
    # against agg's own progress totals: identical on all three.
    #
    # `|| true`: `grep -o` exits 1 on no match, which under `pipefail` would
    # abort here with grep's exit code instead of this file's message.
    frames=$(od -An -tx1 -v "$gif" | tr -s ' \\n' ' ' | grep -o ' 21 f9 04' | wc -l || true)
    [ "$frames" -ge @MIN_FRAMES@ ] || fail \\
        "$gif carries $frames animation frame(s), fewer than @MIN_FRAMES@ - a still image, not a recording"

    checked=$((checked + 1))
done

# The floor again, from the other end: a `continue` or an early `break` added
# later cannot leave this reporting success over a partial read.
[ "$checked" -eq "$expected" ] || fail \\
    "checked $checked of $expected GIFs - the loop did not read them all"

echo "gif_check: $checked GIFs, all GIF89a, >= @MIN_WIDTH@x@MIN_HEIGHT@px, >= @MIN_FRAMES@ frames"
"""

# `@NAME@` placeholders and `.replace`, not `%` formatting: Starlark's `%` has
# no mapping form (`%(name)d` is a load-time error), and `str.format` would
# collide with the script's own `${width}` and `$((checked + 1))` braces.
_GIF_CHECK = (
    _GIF_CHECK_TEMPLATE
        .replace("@MIN_WIDTH@", str(MIN_GIF_WIDTH))
        .replace("@MIN_HEIGHT@", str(MIN_GIF_HEIGHT))
        .replace("@MAX_DIM@", str(MAX_GIF_DIMENSION))
        .replace("@MIN_FRAMES@", str(MIN_GIF_FRAMES))
)

# ---------------------------------------------------------------------------
# The macro.
# ---------------------------------------------------------------------------

def _gif_name(script):
    """`getting-started__install.sh` -> `gif_getting-started__install`.

    The `cast_` prefix's counterpart, so the two targets for one doc script sort
    together under `bazel query //test/doc_scripts:*` and a label names which
    half it is.
    """
    if not script.endswith(".sh"):
        fail("gif script %r is not a `.sh` file" % script)
    return "gif_" + script[:-len(".sh")]

def _cast_name(script):
    """The `cast_targets()` label this GIF renders, spelled the same way."""
    return "cast_" + script[:-len(".sh")]

def gif_targets(name, agg = "@tools//:agg"):
    """One `genrule` per cast, rendering `casts/<slug>.cast` to `gifs/<slug>.gif`.

    Args:
      name: the `filegroup` collecting every `.gif` this package produces.
        `:gif_check` is declared alongside it and reads exactly that set.
      agg: the `agg` launcher's label; `@tools//:agg` resolves from `ocx.lock`,
        never from `PATH`. The launcher text carries the tool's content digest,
        so a re-pinned agg re-keys all 39 renders — which is the property that
        makes `ocx.lock` a real edge here and not a comment.

    The input is the cast **target**, not a path under `website/src/public`: a
    GIF must re-render when its recording changes, and only the target edge
    says so. The pre-Bazel path (`website/recordings.taskfile.yml:gifs`) is not
    a fallback for this and never was: it globbed `*.cast` non-recursively out
    of `website/src/public/casts`, which against the nested `<doc>/<name>.cast`
    layout matched **zero** files, so it had been rendering nothing and exiting
    1 for as long as the layout has been nested. It now globs recursively and
    asserts its own count, and it renders with a pinned Nerd Font into
    `out/recordings/` — a different artefact from these, for README and social
    use. That task is also this package's one consumer; see the `manual`
    section above.
    """
    if not CAST_SCRIPTS:
        fail("gif_targets: CAST_SCRIPTS is empty - there is nothing to render")

    scripts = sorted(CAST_SCRIPTS)

    for script in scripts:
        native.genrule(
            name = _gif_name(script),
            srcs = [":" + _cast_name(script)],
            outs = ["gifs/%s.gif" % CAST_SCRIPTS[script]],
            # agg writes its progress bar to stderr and the GIF to argv[2]; no
            # shell quoting subtleties beyond the font list, which is one
            # argument and is quoted as one.
            cmd = "$(location %s) --text-font-family '%s' $(location :%s) $@" % (
                agg,
                GIF_TEXT_FONT_FAMILY,
                _cast_name(script),
            ),
            tools = [agg],
            tags = GIF_TAGS,
        )

    # `manual` here is not a third copy of the tag on the 39 — it is the one
    # that makes those 39 stick. A filegroup reached by `//...` builds its
    # srcs whether or not they are `manual`: the tag governs wildcard
    # *expansion*, never a dependency edge. So an untagged `:gifs` would pull
    # all 39 renders back into every `bazel build //...` and the exclusion
    # would buy exactly nothing. Same reason on `:gif_check` below.
    native.filegroup(
        name = name,
        srcs = [":" + _gif_name(s) for s in scripts],
        tags = ["manual"],
    )

    # The probe is a text file, so excluding it saves no work. It is `manual`
    # so that the package's wildcard-visible set is *empty* rather than
    # one-of-42: a lone survivor is the thing a later reader takes for the
    # whole surface having stayed.
    written_file(name = "gif_check_probe", text = _GIF_CHECK, tags = ["manual"])

    # `$(rootpath)`, not `$(location)`. A test runs with its working directory
    # at the runfiles root, where a generated file is `test/doc_scripts/gifs/…`;
    # `$(location)` would hand it the exec-root spelling
    # (`bazel-out/k8-fastbuild/bin/…`), which resolves to nothing there. The
    # count is passed separately and asserted against the argument count, so
    # the two cannot silently disagree.
    sh_test(
        name = "gif_check",
        srcs = [":gif_check_probe"],
        args = [str(len(scripts))] + [
            "$(rootpath :%s)" % _gif_name(script)
            for script in scripts
        ],
        # The 39 labels, not `[":" + name]`. `$(rootpath)` resolves only
        # against a *direct* prerequisite, and a filegroup is one label holding
        # 39 — so through it every expansion above is an analysis error. Naming
        # them individually is also the stricter declaration: the test's inputs
        # are exactly the files it is handed.
        data = [":" + _gif_name(script) for script in scripts],
        # `manual`, and this is the tag the whole arrangement turns on. This
        # test's `data` IS the 39 renders, so a wildcard-visible `:gif_check`
        # makes `bazel test //...` build every one of them as its inputs —
        # tagging only the producers would move the cost, not remove it. The
        # proof does not go dark: `website/recordings.taskfile.yml`'s `gifs`
        # task names this label, so it runs there, and it runs on the real
        # renders. See that task before deleting this tag.
        tags = ["manual"],
    )
