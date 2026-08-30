# WP-6 — ruff + test lint gate (#367, #385)

Branch `hex/issue-sweep--wp6`, base `52eecc15`. Files touched: `test/**` (141) and
`taskfile.yml`. Zero files under `crates/**`.

## Contracts

| ID | Status | Evidence |
|---|---|---|
| C-050 | **DONE** | `test/pyproject.toml` floor `>=3.10` → `>=3.13`; `[tool.ruff]` added; `uv.lock` regenerated. Commit `6dc5c45e`. |
| C-051 | **DONE** | `ruff>=0.16` in `[dependency-groups] dev`, pinned to 0.16.5 by `uv.lock`. Invoked `uv run ruff check .`. Not `ocx add`. Commit `6dc5c45e`. |
| C-052 | **DONE** | `test:lint` in `test/taskfile.yml`, no `sources:`/`generates:`, wired into `.verify:lint`. S-012 red/green below. Commit `6d8acd4a`. |
| C-053 | **DONE** | 279 `PLW1510` sites, each read via AST, all → `check=False`. Commit `b66c95ed`. |
| C-054 | **DONE** | 95 `RUF100` evaluated *after* C-050, then removed. Commit `01823f75`. |
| C-055 | **DONE** | `test_attestation_fixtures_self_check` added. S-013 red/green below. Commit `72a0b4fc`. |

## Ordered-step audit trail

The order **C-050 → re-run ruff → C-053 → C-054** was honoured; one commit per step,
bisectable.

| Step | Findings | Δ |
|---|---|---|
| baseline (no config) | **589** (234 autofixable) | — matches the plan's measurement exactly |
| after **C-050** (floor + `[tool.ruff]`) | **587** | −3 `invalid-syntax`, +1 `UP017` |
| after **C-053** (`check=` sweep) | **308** | −279 `PLW1510` |
| after **C-054** (unused noqa) | **213** | −95 `RUF100` |
| after safe autofixes | **73** | −140 (+8 surfaced during fixing) |
| after the manual pass | **2** | −71 |
| after the `test_cascade.py` fix | **0** | −2 |

### Why the order was load-bearing (measured, not asserted)

`requires-python = ">=3.10"` made `tests/test_deps_interpolation.py` unparseable to ruff
(three f-strings reusing the outer quote). **A file ruff cannot parse is a file ruff does
not lint.** Correcting the floor made it parse and surfaced 3 real findings in it
(`I001` + 2× `RUF059`) that no earlier run could see, and enabled one target-version-gated
rule elsewhere (`UP017`, `tests/test_cosign_matrix_extras.py:827`).

### C-053 — per-site, not blanket

AST classification of all 279 (not a sample): **211** assign the `CompletedProcess` to a
name read afterwards (**0** assigned-and-never-read), **67** `return` it from a helper to an
inspecting caller, **1** is best-effort `docker rm` in a `finally`. Every one would *raise
instead of returning* under `check=True`. The two genuinely fire-and-forget calls
(`bench/harness.py:842`, `src/helpers.py:71`) already declare `check=True` and were not in
the finding set.

The rewriter's insertion point came from each call's AST end position. At **3** sites the
last token before the closing paren was a trailing comment and the kwarg landed *inside*
it — caught only because ruff's count stopped at 3 instead of 0, then fixed by hand. The
tool's own "279 patched" would have been a false green.

### C-054 — evaluated, not autofixed

All 95 name a rule **outside** the configured set — `PLC0415` (33), `PLR2004` (32),
`E402` (17), `N802` (7), `A002` (3), `S603` (2), `PLR0912`, `PLR0915` — and **zero** are
the other kind of unused (an enabled rule that stopped firing). None suppresses anything
today.

> A `--select RUF100` probe reports a *different, wrong* answer: selecting one rule
> disables every other, so directives naming `F401`/`BLE001` also read "non-enabled". The
> numbers above come from the full-rule-set run, filtered.

## S-012 — `task verify` fails on a ruff finding

Mutation: `check=False` removed from `test/tests/test_sign.py:113`; presence of the mutation
verified by reading the line back before and after.

```
RED    task verify        -> exit 201
       task: Failed to run task "verify": task: Failed to run task ".verify:lint":
       task: Failed to run task "test:lint": exit status 1
       PLW1510 ... --> tests/test_sign.py:103:19
GREEN  task test:lint     -> exit 0   "All checks passed!"
```

The red aborts in phase 1: `rust:build`, `test:parallel`, `test:default` and `docker` appear
nowhere in the output, so **the shared registry is never reached**. `task --dry verify`
confirms the ordering — `test:lint` is entry 1 of the graph, `test:default` entry 34.

Full-verify *green* was deliberately not run: it reaches `test:parallel` and eight worktrees
share one registry. The green half is the identical command `task verify` invokes.

**`task --force` trap:** `test:lint` carries no `preconditions:` at all — its guard is that it
has no `sources:`/`generates:`, so nothing to skip. Nothing about this gate lives where
`--force` can bypass it.

## S-013 — a drifted attestations fixture fails pytest

```
RED    canonicalize tests/fixtures/pretty_cyclonedx.json (the exact drift it guards)
       pytest ...::test_attestation_fixtures_self_check -> exit 1
       AssertionError: fixture is accidentally canonical -- round-trip test
                       would pass for the wrong reason
GREEN  git checkout -- the fixture (byte-exact; `git status` reports it clean)
       same command                                      -> exit 0
```

## Defect found and fixed

`tests/test_cascade.py::test_variant_only_no_default_tags` **could not fail**: its
`raise AssertionError` sat inside a `try` whose own `except Exception: pass` caught it, so a
rolling tag that *did* exist was swallowed by the handler meant to prove it did not. Surfaced
by the new gate (`S110`), then run down as a shape: an AST census of every `raise` inside a
`try` whose handlers catch it found **4** sites in `test/**`. The other three
(`fixtures/attestations.py:400`, `fixtures/golden/generate.py:891,970`) are already correct —
each re-asserts on the caught error. This adopts that same idiom. Commit `60573f1d`.

Discrimination demonstrated against a stub where the tag exists: shipped shape → passes,
new shape → raises.

## Verification

- `task test:lint` → **exit 0**, "All checks passed!" (589 → 0 findings).
- `pytest --collect-only` (`OCX_TESTS_NO_REGISTRY=1`) → **exit 0, 3000 tests**, unchanged
  across the import-removing commits and the final tree.
- `tests/test_golden_fixtures.py` → **2 passed**.
- `python -m compileall` over the suite → exit 0 after each bulk rewrite.
- `bench/shell_latency.py --self-check` → exit 0 (21 evaluator + 6 needle cases); it is
  the one file with a manual edit that carries its own gate.
- `task rust:verify` → **exit 0**: clippy, both Windows cross-checks, hawkeye,
  cargo-deny, cargo-about, release build, and 6423 unit tests passed (8 skipped).
  The diff contains no Rust; this evidences that rather than asserting it. It needed
  `git submodule update --init --recursive` first — a fresh-worktree prerequisite,
  not a defect (`external/docker_credential` was absent).
- Structural check: all 141 changed paths under `test/` are status `M`. No file added,
  deleted or renamed, so the five siblings adding tests here conflict at line level at
  worst.

## Deferrals (not fixed — outside this package's contracts)

1. **The 95 removed directives are a record of a wider rule set.** Enabling what they name
   has a measured price: `PLR2004` 493 unsuppressed sites, `PLC0415` 297, `S603` 398,
   `E402` 12, `PLR0912` 7, `PLR0915` 6, `A002` 2, `N802` 1 — ~1,200 findings, far outside
   D3's decided 589. Ruff names every site if that day comes.
2. **`test_clean_retains_reachable_blobs` (`tests/test_resolution_chain_refs.py`)** computed a
   post-clean blob count it never compared, unlike its two siblings in the same file which
   assert `blobs_after == blobs_before`. The AC3 contract *is* carried by the ref-link loop
   below it, so this is a dropped diagnostic rather than a hole — but equality is a stronger
   claim than AC3 makes, so I removed the dead binding rather than guess at the assertion.
3. **`_check_no_public_good_rekor` (`fixtures/golden/generate.py:1057`)** scans
   `root.rglob("*")` with no `__pycache__` exclusion, so any byte-compile under
   `tests/fixtures/golden/` reds it on a derived file. My own `compileall` tripped it. Normal
   runs do not byte-compile there (`runpy.run_path` writes no `.pyc`), so this is latent.
4. **`test_cascade.py`'s live behaviour is unverified here** — it needs the shared registry and
   a built binary, and this worktree has neither. If it reds in CI it has found the tag leak
   it was written for.
5. **`_run_publish(..., check=False)` (`tests/test_doc_scripts_publish.py:1111`)** still
   discards a failing publish's exit status. Its rc/stderr now reach the assertion message,
   but nothing asserts `returncode == 0`; every other call site takes the `check=True`
   default. Changing it is a behaviour change I cannot run here.
