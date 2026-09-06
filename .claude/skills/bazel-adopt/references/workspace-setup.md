# Workspace setup: the pin, the module, the lockfile, the rc files

Read this when running steps 5 to 7. It holds the 8.x-versus-9.x decision with
the commands that prove each cost, the `MODULE.bazel` and lockfile setup, the
rc-file layering contract, and the flag-existence discipline.

Contents: [Pinning the version](#pinning-the-version) ·
[8.x against 9.x](#8x-against-9x) · [MODULE.bazel](#modulebazel) ·
[The lockfile](#the-lockfile) · [rc-file layering](#rc-file-layering) ·
[Flag-existence discipline](#flag-existence-discipline) ·
[Version-gating across a matrix](#version-gating-across-a-matrix)

## Pinning the version

`.bazelversion` holds an exact three-component semver. Never `rolling`,
`last_green`, `last_rc`, `latest`, `latest-N`, a bare RC string, a floating
`X.x`, or a commit hash (BZL-FLAG-01). Every one of those resolves to a value
that moves independently of any code change in the repository.

```sh
cat .bazelversion
```

Must match `^[0-9]+\.[0-9]+\.[0-9]+$`. **A missing file is equally a finding** —
the launcher then falls through to "the latest official release".

Two more pins in the same chain:

- Never write a literal version into a CI workflow's `USE_BAZEL_VERSION` or into
  `.bazeliskrc`; only a matrix or env reference. `USE_BAZEL_VERSION` resolves
  **ahead of** `.bazelversion`, so a literal silently overrides every future bump
  to the committed file (BZL-FLAG-03).
  `grep -rn --include=*.yml -e 'USE_BAZEL_VERSION' .github/workflows .bazeliskrc` — **empty =
  pass, the committed file is the single source.**
- Pin the launcher itself as explicitly as the Bazel version. The pin chain has
  two links and only one of them is `.bazelversion`.

A rolling or `last_green` leg is allowed as a non-blocking canary only, with the
non-blocking directive on the leg itself (BZL-FLAG-02).

## 8.x against 9.x

As of 2026-09-06: **9.2.0 is Active LTS**, 9.0.0 having shipped 2026-01-20;
**8.8.0 is Maintenance**. Carry a Maintenance major's dated guidance only through
its own published Support-Ends date (BZL-FLAG-05). Re-derive both stages from
[bazel.build/release](https://bazel.build/release) each time either is cited; the
live table shifts on the Active-track minor cadence and a citation ages within
one minor (BZL-FLAG-04).

**A new adoption starts on the Active LTS** unless a consumer pins the Maintenance
major, in which case both are blocking CI legs (BZL-CI-08).

What crossing to 9.x changes, all measured against real binaries:

| Change | Cost | Check |
|---|---|---|
| All WORKSPACE logic deleted | Any `WORKSPACE*` file is a hard break, not a warning | `find . -path ./.git -prune -o -iname 'WORKSPACE*' -print` — **empty = pass** (BZL-FLAG-12) |
| `--incompatible_autoload_externally` defaults empty | Every `py_*`, `sh_*`, `cc_*`, `proto_library` symbol needs an explicit `load()`; bare uses fail at load time | The bump commit must carry a non-trivial BUILD/`.bzl` diff from `buildifier --lint=fix -r -v .` — **an empty diff on a repo with such targets means it has not migrated, it has broken** (BZL-FLAG-16) |
| `--enable_workspace` / `--enable_bzlmod` removed | Passing either fails the whole invocation with `unrecognized option`; do not describe them as no-ops there | `bazel help build --long 2>&1 \| grep -c enable_bzlmod ; bazel help startup_options 2>&1 \| grep -c enable_bzlmod` — **0 from both is the 9.2.0 result** (BZL-FLAG-13) |
| `bazel sync` deleted | Any `sync --only=` recipe hard-fails; the replacement is `bazel fetch --repo=@<repo>`, valid on 8.7.0 and 9.2.0 alike | `bazel help fetch --long` lists `--repo`, `--all`, `--configure`, `--force` on both (BZL-RUST-03) |
| `bazel analyze-profile` deleted, `bazel dump --skyframe` enums renamed | Docs, task files and CI referencing them break | `grep -rn -e 'analyze-profile' -e 'skyframe=working_set' -e 'sync --only' --include='*.md' --include='*.yml' --include='*.yaml' --include='*.bzl' --include='BUILD*' --include='Taskfile*' .` — **empty = pass** (BZL-FLAG-31) |
| `--incompatible_strict_action_env` flips to `true` | Actions stop inheriting the ambient environment they silently depended on | Compare `bazel help build --long` on both majors (BZL-CACHE-19) |
| Protobuf minimum enforced from Bazel's own side | A module graph below that protobuf version fails to resolve | Read the incoming major's release notes for its enforced minimum (BZL-FLAG-15 step 4) |
| `--repo_contents_cache` on by default | Its path derives from `{--repository_cache}/contents`; an output user root inside the repository makes **every** command hard-fail, `bazel help` included | Keep `--output_user_root` and `--repository_cache` outside the repository tree (BZL-MOD-33) |
| `package(transitive_visibility = …)` available | A gain, not a cost — and it takes a single `package_group` label **string**, not a list | (BZL-ARCH-34) |

Move the pin across the boundary with the ordered checklist in BZL-FLAG-15, and
test the incoming major's flips on the **outgoing** pin first: `bazelisk --strict`
is the one-build pass/fail signal, and `bazelisk --migrate` scoped with
`BAZELISK_INCOMPATIBLE_FLAGS` is for isolating *which* flag broke a `--strict`
failure, not for the first pass (BZL-FLAG-17).

## MODULE.bazel

Start from the smallest module that resolves. Two rules bind immediately:

- Mark every dev-only `bazel_dep` — test, lint, docs, formatting tooling —
  `dev_dependency = True`. Without it your build toolchain leaks into every
  consumer's resolved graph (BZL-MOD-07). `grep -n 'bazel_dep' MODULE.bazel`, then
  classify each; **empty output means no deps are declared**, which is its own
  question, not a pass.
- Never write a `single_version_override` whose version is lower than any live
  `bazel_dep` requirement for the same module. Silently harmful through 8.x, a
  named hard resolution error from 9.0.0 (BZL-MOD-08).

`bazel mod tidy` rewrites the hand-authored `MODULE.bazel` and ships with no
`--check` or dry-run flag, so run it only behind a diff gate:
`bazel mod tidy && git diff --exit-code MODULE.bazel`. **Exit 0 = already tidy =
pass** (BZL-MOD-10).

## The lockfile

Commit `MODULE.bazel.lock` (BZL-MOD-01). `git ls-files MODULE.bazel.lock` —
**empty output = untracked = finding.**

**Put the freshness gate in on day one.** `--lockfile_mode=error` runs on a
dedicated CI leg and nowhere else — a bare `error` in the base config turns every
ordinary local build into a failure the moment a dependency moves. Add a
`schedule:`-triggered leg on `--lockfile_mode=refresh`, the only mode that
re-checks mutable registry data (BZL-MOD-02).

**Gate both legs on the exit code, never on a matched stderr string.** Measured
on 8.7.0 and 9.2.0: a lock stale because an *existing* dependency's locked version
changed exits **37** with an internal `IllegalStateException` from
`YankedVersionsFunction`; a lock stale because `MODULE.bazel` gained a *new*
`bazel_dep` exits **37** with the clean documented "Missing checksum for registry
file … run `bazel mod deps --lockfile_mode=update`" message. Same code, two texts,
and only one of them is matchable.

**Never hardcode the schema version.** It reads `10` in the docs, `24` from a
lock produced by 8.7.0, and `28` from 8.8.0, 9.2.0 and master alike, with an added
`factsVersions` key. A Maintenance patch bump crosses the boundary (BZL-MOD-05).

```sh
python3 -c "import json;print(json.load(open('MODULE.bazel.lock'))['lockFileVersion'])"
```

There is no empty case: a missing key or a parse error is itself the finding, and
means a corrupt or badly merged lock.

**Set the merge driver before the first conflict, not after.** A generic
line-based driver (`union`, `ours`) can interleave a changed digest key with **no
conflict markers at all**, which also defeats Bazel's own conflict detector — a
silent bad merge, strictly worse than a visible one (BZL-MOD-04).

```sh
git check-attr merge MODULE.bazel.lock
```

Must print `bazel-lockfile-merge`. **`unspecified` or a generic driver name is the
finding.** Scope it to that one file: the driver is written against this schema
and must never be aimed at a generator-owned JSON lockfile, which needs its own
JSON-aware driver.

**A `.bazelversion` change regenerates the lock in the same commit** and the diff
is reviewed there (BZL-MOD-06). `git show --stat <bump-commit>` must touch both
files; **a commit touching only `.bazelversion` is the finding**, on a patch bump
as much as a major one.

## rc-file layering

Three layers, and the order is a contract, not a convention.

1. The committed `.bazelrc`: everything the repository asserts about itself.
2. Optional `build:<name>` config blocks, selected on the command line.
3. A `try-import` of a gitignored personal file, as the **last non-comment line**.

Import position decides precedence: options in an imported file beat options
appearing before the import line and lose to those after it. A personal-override
import placed anywhere but last is silently defeated (BZL-FLAG-19).

```sh
grep -vE '^\s*(#|$)' .bazelrc | tail -1
```

The printed line must be the `try-import`. **Empty output means `.bazelrc` has no
non-comment content** — a valid starting state, not a pass.

Two things never live in a committed rc file: a value that is *executed*, and a
value that is a *secret*. Bazel resolves and spawns a `--credential_helper` with
the full client environment, before the sandbox, with no check on where the flag
came from — exploitable on a fresh clone by a plain `bazel query //...`, and
closed upstream as intended behaviour (BZL-CACHE-04).

For every filename a `try-import` names, write down next to that line which shape
CI has: CI never creates the file, or CI recreates that same path with a
different, non-overlapping flag set. The two look identical in the committed rc
and differ entirely in what a reviewer may assume (BZL-FLAG-20).

Give any numeric tuning value a comment stating why that number, or a citation to
the versioned source it came from. A bare integer cannot be told apart from a typo
or a stale local value (BZL-FLAG-22).

## Flag-existence discipline

Verify every flag name against the pinned binary before it is written into an rc
file, a workflow, a doc or a generated config — **including flags copied from a
community always-on list**. Measured against the current reference, 6 of the 8
flags in the most widely copied such post return zero hits: gone, not renamed, and
Bazel hard-fails with `unrecognized option` (BZL-FLAG-11).

```sh
FLAG=incompatible_strict_action_env
bazel help build --long 2>&1 | grep -c -- "$FLAG"
bazel help startup_options 2>&1 | grep -c -- "$FLAG"
```

**Zero from both is not yet the stop signal — confirm with `bazel build --nobuild
--$FLAG //... 2>&1 | grep 'Unrecognized option'`: a hit is absence, exit 0 with no
hit is presence (BZL-FLAG-11).** A flag can live on only one of the two surfaces — one remote-repo-contents flag is a
startup option only and is invisible to `help build --long`, which is exactly how
a check that reads one surface reports a live flag as absent.

Two refinements:

- A flag whose `documentationCategory` is `UNDOCUMENTED` renders nothing in
  `help build --long` even though it exists and works. A zero-hit help grep is
  the stop signal for *writing* the flag; for a flag you suspect exists, check the
  tagged source instead (BZL-CACHE-23).
- A `--@<module>//<package>:<flag>` build setting is versioned by the **ruleset
  that defines it**, not by Bazel. Re-derive its label and default from the pinned
  module version's own source and name that version beside any citation — one such
  flag changed both its canonical path and its default inside a single ruleset
  release while an official Bazel release note still names the old one
  (BZL-FLAG-34).

Treat a registry's `incompatible_flags.yml` and a community rc preset's flag
catalogue as hand-maintained third-party lists: consult them for "is this
presubmit-tested", never for "does this flag exist and what is its default"
(BZL-MOD-13, BZL-FLAG-07).

## Version-gating across a matrix

Where a CI matrix spans two majors, gate a flag that does not exist or defaults
differently with a **mechanism** — a version-conditional import at file level, or
a generated preset's per-flag predicate — never with a prose comment saying
"remove this after upgrading" (BZL-FLAG-21). A comment is not enforced, and the
matrix runs the wrong flag set on one leg the moment someone forgets it existed.

The measured cross-major set as of 2026-09-06, for 8.7.0 / 8.8.0 / 9.2.0:

| Flag | 8.7.0 | 8.8.0 | 9.2.0 |
|---|---|---|---|
| `--incompatible_strict_action_env` | false | false | **true** |
| `--incompatible_autoload_externally` | allowlist | allowlist | **empty** |
| `--repo_contents_cache` | off | off | **on** |
| `--enable_workspace`, `--enable_bzlmod` | present | present | **absent** |
| `--experimental_remote_merkle_tree_cache` | present | present | **absent** |
| `--incompatible_remote_use_new_exit_code_for_lost_inputs` | present | present | **absent** |

The two removed remote flags were removed **at 9.0.0**; they are not flags that
never existed. So an rc line carrying either is dead configuration on an 8.x leg
and a hard failure on the 9.x leg of the same matrix — a version-split failure,
not a uniform one.

Finally, an explicit rc value that already matches the default on **every** major
in the matrix is removed, or carries a comment naming the floor it defends. It
protects nothing today and reads as meaningful tuning (BZL-FLAG-26).
