# Review: [ocx-sh/ocx#332](https://github.com/ocx-sh/ocx/pull/332) "chore: migrate HawkEye to v7"

Branch `codex/migrate-hawkeye-v7` onto `main`, author tisonkun. Not a migration
record — this repo already had one in flight when #332 surfaced as prior art;
this file reviews #332 instead.

## What it changes

One file, `.licenserc.toml`, 3 additions / 1 deletion:

```diff
-inlineHeader = """
+[header]
+text = """
 SPDX-License-Identifier: Apache-2.0
 Copyright 2026 The OCX Authors"""

+[files]
 includes = ["crates/**/*.rs"]
 excludes = ["external/**"]
```

`taskfiles/rust.taskfile.yml` is untouched (`git diff main -- taskfiles/rust.taskfile.yml`
on the PR branch is empty) — `install:hawkeye` still installs via the
unpinned `.ensure-cargo-tool` path. The PR's own description confirms this is
deliberate: "install HawkEye from its latest release without pinning the
tool version."

## Coverage: identical

`includes`/`excludes` values are untouched strings — same `crates/**/*.rs`
scan root, same `external/**` exclusion, same header text (byte-identical,
including the no-trailing-newline TOML triple-quote behavior). Confirmed
live on the checked-out PR branch, not just by reading the diff:

- `hawkeye check --output-format json` → 0 occurrences of `external/` in the
  output (`grep -c 'external/'`), matching this repo's pre-migration
  behavior — nothing under `external/**` is scanned.
- `find crates -iname '*.rs' | wc -l` → 488, matching hawkeye's own report of
  488 files scanned. The PR description says "479 files" — that count is
  stale relative to current `main` (more `.rs` files have landed since the
  PR was opened); it is not a discrepancy in what the config covers, since
  both counts come from the same unchanged `crates/**/*.rs` glob.

No widening or narrowing of scope.

## Gate still goes red: confirmed

Checked out the PR branch (`gh pr checkout 332`), all commands redirected to
a file with `$?` echoed after (never piped):

**Pass on unmodified PR branch:**
```
$ direnv exec . task rust:license:check --force
488 files, 0 changes, 0 conflicts, 0 unsupported
EXIT: 0
```

**Fail after stripping the SPDX header from `crates/ocx_lib/src/lib.rs`:**
```
$ direnv exec . task rust:license:check --force
        add  crates/ocx_lib/src/lib.rs
488 files, 1 change, 0 conflicts, 0 unsupported
task: Failed to run task "rust:license:check": exit status 1
EXIT: 201
```
Names the exact mutated file.

**Restore + re-verify:**
```
$ git status --porcelain
(empty)
$ direnv exec . task rust:license:check --force
488 files, 0 changes, 0 conflicts, 0 unsupported
EXIT: 0
```

The gate demonstrably distinguishes a real violation from a clean tree on
this PR's config, in both directions.

## Version-pinning trap: NOT closed

`install:hawkeye` in `taskfiles/rust.taskfile.yml` is unchanged by #332 and
still reads:

```yaml
install:hawkeye:
  internal: true
  cmds:
    - task: :.ensure-cargo-tool
      vars: { TOOL: hawkeye, CRATE: hawkeye }
```

`.ensure-cargo-tool`'s status check only asserts "some version of the binary
runs" (`{{.TOOL}} --version` succeeding), not a specific version — the same
mechanism that let hawkeye 6.x silently become 7.0.0 and break config
parsing on every branch with zero code change. #332 fixes today's schema
break but leaves the exact same trap armed for hawkeye 8.x: a future
`cargo install --locked hawkeye` will again resolve to whatever is newest at
install time, with no version-mismatch signal before the config fails to
parse.

Precedent already in this same taskfile for exactly this failure mode:
`install:cargo-about` pins an exact version with a `status:` check matching
it verbatim, specifically because an unpinned bump changed that tool's
output grouping under an unrelated file. `install:cargo-zigbuild` pins for
the same reason (link invocation affects a byte-equality gate). Neither
`cargo-deny` nor nextest are pinned, so pinning is not a blanket convention —
but hawkeye has now demonstrated the identical risk class (and a harder
failure: an unrunnable gate, not just wrong output), so leaving it unpinned
is inconsistent with how this repo already treats that risk.

## Verdict

- Coverage: preserved exactly — same files checked, same files skipped, same
  header text. ✅
- Gate goes red on a real violation and green again after restore, verified
  live on the PR branch, not just by inspection. ✅
- Version pin: not added. The PR closes today's break but leaves the same
  unpinned-`cargo install` mechanism in place for a future hawkeye 8.x. ⚠️
