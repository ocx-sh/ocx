---
paths:
  - test/**
---

# Test Subsystem

Pytest acceptance tests with Docker Compose registry at `test/`.

## Design Rationale

Pytest (not Rust integration tests) because acceptance tests exercise real compiled binary against real OCI registry — catch issues mocked unit tests miss. Session-scoped registry (started once in `pytest_sessionstart`) enables fast parallel runs with pytest-xdist. UUID-prefixed repo names provide isolation on shared registry, no per-test cleanup. See `arch-principles.md` for full pattern catalog.

## Structure

| Path | Purpose |
|------|---------|
| `test/tests/conftest.py` | Function-scoped fixtures (ocx, published_package, etc.) |
| `test/conftest.py` | Session-scoped fixtures (registry, ocx_binary) + `pytest_sessionstart` |
| `test/src/runner.py` | `OcxRunner`: subprocess wrapper with test isolation |
| `test/src/assertions.py` | Cross-platform assertion helpers |
| `test/src/helpers.py` | `make_package()`: build + push test packages |
| `test/src/registry.py` | OCI registry helpers (fetch manifest, extract platforms) |
| `test/taskfile.yml` | Task runner (default, quick, parallel, smoke, scoped; suite floor/ceiling checks) |
| `test/SUITE_FLOOR`, `test/SKIP_CEILING`, `test/XFAIL_CEILING` | Plain-integer floor files the run is bracketed by (below) |

## Key Fixtures

| Fixture | Scope | Purpose |
|---------|-------|---------|
| `registry` | session | localhost:5000 **zot** (referrers-capable primary; auto-started via docker-compose; port override `OCX_TEST_REGISTRY_PORT`) |
| `mirror_registry` | session | localhost:5001 registry:2 (mirror-test target; port override `OCX_TEST_MIRROR_PORT`; skips if absent) |
| `legacy_registry` | session | localhost:5001 registry:2 — referrers-**negative** fixture (no Referrers API) for `test_referrers_capability.py` (#106); same service as `mirror_registry` |
| `ocx_binary` | session | Path to compiled `ocx` binary |
| `ocx_home` | function | Isolated temp dir for `OCX_HOME` |
| `ocx` | function | `OcxRunner` instance with test isolation |
| `unique_repo` | function | UUID-prefixed repo name (e.g., `t_a1b2c3d4_test`) |
| `published_package` | function | Pre-built + pre-pushed test package (v1.0.0) → `PackageInfo` |
| `published_two_versions` | function | Two versions (v1.0.0, v2.0.0) → `tuple[PackageInfo, PackageInfo]` |

**Registry topology (#195):** the primary `registry` (5000) is **zot** because it
serves the OCI 1.1 Referrers API natively — distribution's `registry:2`/`registry:3`
do NOT (they omit `OCI-Subject` on push and 404 the `/v2/<name>/referrers/<digest>`
route), and OCX is referrers-only with no tag-schema fallback (#106). The single
`registry:2` instance (`mirror-registry`, 5001) doubles as the mirror-test target and
the permanent referrers-unsupported negative fixture. `test_referrers_smoke.py` proves
the round-trip. Referrers push/list helpers live in `src/registry.py`.

## OcxRunner API

```python
runner = OcxRunner(binary, ocx_home, registry)
runner.run(*args, format="json", check=True)  # Run command, assert success
runner.json(*args)                             # Run + parse JSON stdout
runner.plain(*args)                            # Run without --format flag
```

Env: `OCX_HOME`, `OCX_DEFAULT_REGISTRY`, `OCX_INSECURE_REGISTRIES` set per instance.

## PackageInfo

Returned by `published_package` / `published_two_versions`:

| Field | Example |
|-------|---------|
| `repo` | `"cmake"` |
| `tag` | `"1.0.0"` |
| `short` | `"cmake:1.0.0"` |
| `fq` | `"localhost:5000/cmake:1.0.0"` |
| `marker` | UUID-based unique string |

## make_package()

Creates, bundles, pushes, indexes test package:

```python
pkg = make_package(ocx, repo, tag, tmp_path,
    bins=["hello"],          # Binary names (default)
    env=[...],               # Custom metadata env entries
    cascade=True,            # Auto-tag latest/major/minor/patch
    size_mb=0,               # Random padding for progress bar tests
)
```

**Default env visibility in tests**: `make_package()` defaults env entries to `"visibility": "public"` (see `test/src/helpers.py` lines 160–165). This matches the convention used by in-tree mirrors and acceptance tests that verify env resolution. Tests asserting on env output in `consumer` mode rely on this default. When writing tests for `private` or `interface` entries, pass explicit `visibility` in the `env` list.

## Assertion Helpers

- `assert_path_exists(path)` — exists (file, dir, or symlink)
- `assert_dir_exists(path)` — is directory
- `assert_symlink_exists(path)` — is symlink or Windows junction
- `assert_not_exists(path)` — not exist and not symlink

**Always use `assert_symlink_exists()` instead of `path.is_symlink()`** for Windows junction compat.

## Test Isolation

- **Per-test OCX_HOME**: each test gets isolated `tmp_path` as `OCX_HOME`
- **UUID repo names**: `unique_repo` fixture prevents collisions in shared registry
- **Shared registry**: session-scoped; all tests push/pull same instance
- **Minimal env**: OcxRunner strips ambient env; only PATH, HOME, OCX vars

## Running Tests

```bash
task test              # Build + registry + all tests
task test:quick        # Skip rebuild
task test:parallel     # pytest-xdist (-n auto)
task test:smoke        # The smoke tier only (-m smoke), ~5 s
task test:scoped -- tests/test_login.py   # The modules verify:scoped selected; every glob must collect >= 1 test

# Single test (runs prebuilt test/bin/ocx — a copy of Bazel's //crates/ocx_cli:ocx; refresh via `task test` after Rust changes):
cd test && uv run pytest tests/test_install.py::test_name -v
```

## Verification tiers

Four tiers (ADR `adr_test_speed_tiers.md` § C-TIER; plan `plan_test_speed_tiers.md` C-008…C-020):

| Tier | Runs | Trigger | Budget |
|---|---|---|---|
| **T0 lint** | `task test:lint:structure` (pytest over `test/lint/`, uncached) + `scripts/scoped_gate.py --check-coverage` (`task test:rows:check`) | verify phase 1; every `verify:scoped`, regardless of decision (an escalate reaches it through `verify`) | ≤ 30 s wall (`OCX_LINT_BUDGET_SECONDS`), floored on `test/LINT_FLOOR`, ceilinged on `test/LINT_SKIP_CEILING` / `test/LINT_XFAIL_CEILING` |
| **T1 inner** | T0 + `task bazel:test:unit` (cached, reverse-dependents) + `cargo clippy -p <crate>` per changed crate + `test:smoke` + `test:scoped` over the plan's `acceptance_globs` | `task verify:scoped --force`, per task / review-fix iteration | ≤ 120 s on the first verification after a code-changing edit |
| **T2 full** | `task verify` (both phases, ending in `bazel:test:accept`) | WP merge commit (mechanically enforced — `workflow-git.md` "Work-Package Merges"), `/hex-finalize`, any `verify:scoped` escalation, commits on `main` | unbounded, measured |
| **T3 deep** | `verify-deep.yml` | push to `main`, merge queue, `workflow_dispatch` | CI, cold disk |

### Lint tier admission (`test/lint/`)

`test/lint/conftest.py` refuses a lint test that requests `ocx`, `ocx_binary`, `registry`, `mirror_registry`, `legacy_registry`, `target_registry`, `published_package` or `sigstore_stack`; it wraps `subprocess` so an argv resolving under `test/bin/` or spelled `task` raises. `git` and `sys.executable` stay allowed. The session budget is `OCX_LINT_BUDGET_SECONDS` (default 30); `task test:lint:structure` also floors the collected count on `test/LINT_FLOOR`, as a `cmds:` entry (never `preconditions:`, so `--force` cannot skip it), and caps the run's `skipped` / `xfailed` counts at `test/LINT_SKIP_CEILING` / `test/LINT_XFAIL_CEILING` through the acceptance tier's `.suite-ceilings`. `test_lint_tier_guards.py` drives both with `pytester` fixtures, red and green.

### `command` markers and `test/scoped_rows.toml`

The coverage guard (`scripts/scoped_gate.py --check-coverage`, wired as `task test:rows:check`) reds unless: every acceptance module is reached by a `test/scoped_rows.toml` glob or a `@pytest.mark.command(...)` marker; every `crates/ocx_cli/src/command/*.rs` file is named by a marker, a `[verbs]` entry, or falls under `[security]`; and `[security]` covers hex config's `reviewer:security` globs. Each reader is floored independently of the globs it checks — a table that collects nothing reds rather than passing vacuously.

**Adding a verb needs a `command` marker** on its acceptance test (`subsystem-cli-commands.md` § Command Summary), or `task test:rows:check` reds naming the file — at T0, before any build is spent.

`test/scoped_rows.toml` carries more than the ADR's original list: `[verbs]` escalates every multi-verb group file (`package`, `config`, `shell`, `self_group`, `index`, `patch`, `direnv`, `launcher`); `[security]` widens to every `build.rs`, `auto_verify.rs`/`verify.rs`, `consent.rs`, `shell_state.rs`, `self_group/{activate,setup}.rs`; every other path under `crates/ocx_cli/` escalates by default (permit-list, not deny-list); `test/lint/**` routes to the lint tier.

### `UNCACHED_MODULES` and the `external` tag

`test/bazel.bzl`'s `UNCACHED_MODULES` lists acceptance modules whose declared input closure `scripts/bazel_tag_guard.py` cannot yet certify complete; each carries `external`, the only tag that stops a cached verdict being reused, so it re-executes on every `bazel:test:accept` run. The list shrinks only by declaring the input, never by deleting a line. It holds **one named residual**: `tests/test_windows_shim.py` — its `_find_shim_binary` fallback reads `target/{release,debug}/` on two adjacent lines, and `scripts/test_diff_guard.py`'s one-line-for-one-line `--allow` admits only a one-line-for-one-line hunk, so dropping both lines at once is refused by design and the only 1:1 rewrite would hide the read from the tag guard instead of fixing it. The module is `skipif(sys.platform != "win32")` as a whole, so on the Linux lane that runs this suite `external` costs one collection and hides no verdict.

`bazel:test:accept` runs its targets **concurrently** against the one compose stack (no `exclusive` tag), at parity with `test:parallel`'s xdist run — `test/bazel.bzl` § Concurrency. Two obligations follow for a module author: every `xdist_group` a module names — shared with another module or not, because every target is its own process and runs from sibling checkouts overlap — is listed for that module in `test/BUILD.bazel` `module_slots` (the runner's per-group host lock replaces xdist's one-worker placement), spelled in the module itself as a string literal or a module-level dict of literals; and a file only some modules read (anything outside `conftest.py`, `src/`, the `tests/` helpers and the stack config) is declared in those modules' `module_data`, not in `:suite_inputs` — importing a helper counts as reading it. `scripts/bazel_tag_guard.py` reds a missing or extra slot, a group spelled outside a `tests/test_*.py` module, and a read its AST derivation can see; a read it cannot see (a `task` recipe, the lock beside a manifest) is declared in the guard's hand-kept `HAND_DECLARED_READS` / `IMPLIED_READS`, and a module that launches `task` or names by string a path under a `test/` directory holding no `:suite_inputs` member reds `tag-hand-read-unreviewed` until it has a `HAND_DECLARED_READS` entry (empty when the review finds no read).

### `--tiered-shapes`

`scripts/test_diff_guard.py` gained the sanctioned shapes this plan needed — a move into `test/lint/`, a new `test/lint/**` module, an added `@pytest.mark.command(...)` marker line, a `test/scoped_rows.toml` / floor-file edit, and a fresh `// ported-from:` deletion (C-RUBRIC) — all behind one opt-in flag: `task scripts:test-diff-guard -- --tiered-shapes`. Without the flag the guard behaves exactly as it did before this plan (crate-split DEC-10's default is untouched). Whether the marker/config shapes become the default is an open owner decision (`plan_test_speed_tiers.md` § Deferred owner decisions, B2).

## Smoke Tier and Guards

The crate split (`plan_crate_split_workspace.md` DEC-10, C-012…C-014, C-069, C-077) makes
the suite an unmodified proof while `ocx_lib` is taken apart. Four mechanisms:

- **`smoke` marker.** One `@pytest.mark.smoke` happy path per visible top-level `ocx` verb
  (22), registry only, never `sigstore_stack` / `identity_token`, never a `sign|attest|verify|cosign`
  test. `test/pyproject.toml` registers the marker and sets `--strict-markers`, so a misspelled
  marker is a collection error, not a silently unselected test. `task test:smoke` runs
  `-m smoke -n auto --dist loadgroup` under a 90 s wall-clock budget (target 60 s);
  `test/lint/test_smoke_coverage.py` reds a verb without a smoke test and a smoke test on the
  signing surface. `task test:scoped -- <glob>...` runs the acceptance modules `verify:scoped`
  selected (`scripts/scoped_gate.py`'s `acceptance_globs`, from `test/scoped_rows.toml` rows
  and `command` markers — moved off `test/taskfile.yml`'s old `SCOPED_ROWS`, C-012); every
  glob must collect at least one test, or the run fails.
- **Floor files.** `test/SUITE_FLOOR` (collected count, only ever rises — in the commit that
  adds the tests), `test/SKIP_CEILING`, `test/XFAIL_CEILING`. `task test` (and `test:quick` /
  `test:parallel`, which route through it) refuses below the floor before the run and above a
  ceiling after it, both as `cmds:` (`--force` skips `preconditions:`).
- **Diff guard.** `scripts/test_diff_guard.py <base>..<head>` (run at every merge over both the
  per-WP range and `merge-base(origin/main)..HEAD`, with `--tiered-shapes` on ranges that
  legitimately move or add a `test/lint/` module, add a `command` marker, edit
  `test/scoped_rows.toml`/a floor file, or delete a ported original under C-007(e) — see
  "Verification tiers" § `--tiered-shapes` above; every other range runs without the flag)
  refuses any diff under `test/` except:
  an added `@pytest.mark.smoke` line (and the `import pytest` a module lacked); an added
  `@pytest.mark.xdist_group("<name>")` line — scheduling, the one mark that cannot change
  what a test asserts, and the only way to serialise writers of a registry-wide singleton
  such as the reserved `global` patch-descriptor repository; the group must be a string
  literal, so the slot being joined is greppable; a
  docstring/comment line that carries none of `assert`, `pytest.mark.skip`, `xfail`,
  `parametrize`, `pytest.fixture` or a `def` signature — the keyword check runs before the
  inertness check on purpose, so commenting an assertion out is refused exactly like deleting
  it; a wholly new test function or `Test*` class, or a new
  `test/tests/**/*.py` module (never `conftest.py`, `__init__.py`, `pytest.ini`, `tox.ini`,
  `setup.cfg`); a change to any `test/` file a taskfile invokes (tooling the repo drives —
  `test/scripts/*.sh` and friends — decided by scanning every taskfile's non-prose lines, with a
  file the collected suite *names* still frozen, so `docker-compose.yml` cannot be thawed by the
  task that starts it); edits to `test/pyproject.toml`, `test/taskfile.yml` and the three floor
  files.
  A new def or class is exempt from the line checks but not from the rebinding checks — the
  shapes `scripts/test_diff_guard.py`'s docstring names (that list is the authority; it
  widens as adversaries find shapes). One `--allow <path>:<line>` exempts a single
  one-line-for-one-line hunk (the trampoline blob path, DEC-10 d). The plan's own oracles
  (`test/lint/test_smoke_coverage.py`, `test/lint/test_logging_structure.py`,
  `test/lint/test_no_crate_path_assertions.py`) are the only modules exempt from
  the line checks. `--self-test` shows every shape red and green.
- **Adding a verb** (see `subsystem-cli-commands.md`): its acceptance test carries a `smoke`
  row and a `command` marker, or `test:rows:check` (T0) and
  `test/lint/test_smoke_coverage.py` (T0) both red; `SUITE_FLOOR` rises in the same commit.

## Unit-Test Duration Budget (Rust)

Not this suite — the *unit* suite, recorded here because it is the other half
of the bracket around `cargo nextest` and the two are read together.

Every Rust unit test must run in **under 1.000 s**. `task rust:test:duration`
(`scripts/unit_test_duration_gate.py`) asserts it over the run log
`rust:test:unit` tee'd to `target/nextest/run.log`, inside the same
`rust:test:floor` → `rust:test:unit` → `rust:test:ceiling` bracket: the floor
step deletes the log, so a duration read there can only be that run's. It runs
on `.verify:build-test` and in `verify-basic.yml`'s `smoke` job.

- **What a green claims, exactly.** *Every unit test that runs in the Linux
  unit suite is under 1 second.* Not "every unit test on every platform" —
  `platforms: [linux]`, the guard the sibling floor/ceiling tasks carry because
  cfg-gated tests make counts platform-dependent. The uncovered legs, named
  rather than papered over: `verify-deep.yml`'s `Build & Unit Test` matrix runs
  **raw** `cargo nextest run --workspace --target=…`, not through `task`, on
  Linux, macOS and Windows; `build-windows-shims.yml` runs three crates the
  same way. Deep is moving to manual dispatch, so a copy of this gate there
  would be a green nobody could tell from one that never ran — the hole stays
  named.
- **Hard on CI, advisory elsewhere.** With `CI` set (GitHub Actions always sets
  it) an over-budget unlisted test fails the run; without it the same findings
  print and the run exits 0. Wall clock on a shared machine measures a test's
  neighbours: four tests at 6-190 ms quiet and 12-160 ms on CI were measured at
  1.0-1.2 s on a 32-core box under load 27, and allowlisting those would file a
  false reason for tests that are fast everywhere it matters. **Not an
  off-switch** — *advisory here; the gate is the Linux CI leg of
  `verify-basic.yml`, which runs on every pull request.* Both modes print which
  one they ran in, on every run, green or red. Only the budget verdict is
  advisory: the reader floor and the unbounded-pattern refusal fail in both
  modes, being wiring faults rather than slow tests. A load-average check is
  deliberately **not** used — a nextest run saturates the box by design, so
  load at gate time describes the gate's moment, not the measurement's.
- **What it asserts.** Every timed result line is parsed (`PASS`, `FAIL`,
  `SLOW`, `LEAK`, `TIMEOUT`, `TRY n FAIL`, `FLAKY n/m`; `[>120.000s]` too), the
  slowest duration per test id wins, and any test at or above the budget that
  no allowlist line covers reds the gate, listed slowest first. Every numeric
  field nextest prints is right-aligned, counter included (`(   1/8215)`), and
  the log comes in three spellings — plain, real `ESC`, and the `^[` plus
  line-prefix form `gh run view --log` hands you, which the gate also reads so
  a CI run can be investigated by hand.
- **Headroom.** 1.0 s is wall clock on the run's own log. ubuntu-latest
  measures roughly 2x this project's dev box for scan-heavy tests and worse for
  process-spawning ones, and CI is where the gate gates — a test above ~0.7 s
  there is a latent red, not a pass. After the current debt rows land fast the
  slowest unlisted test on CI is about 0.8 s. **The budget is never raised
  above 1.0 s to buy headroom; 1.0 s is the contract.**
- **Reader floor.** The gate refuses a log holding fewer timed lines than
  `crates/NEXTEST_FLOOR`, naming both numbers. A subset run, a truncated log or
  a nextest grammar reshape each leave a duration reader with nothing to
  complain about — and a reader that read nothing prints the same green as a
  fast suite. There is deliberately no flag that lowers it.
- **The allowlist.** `crates/NEXTEST_SLOW_ALLOWLIST`, one line per entry
  (`<binary id> <test name>`, exactly as nextest prints it after the progress
  counter), `#` comments ignored. A line is an `fnmatch` pattern; one with no
  metacharacter is plain equality. Use a pattern only for a family slow for one
  structural reason — `ocx_shell *live_*` is the whole live-interpreter family
  as one line that cannot rot into a dozen stale ones, spans both module paths
  the family lives in, and is anchored on the `live_` convention so an ordinary
  `ocx_shell` test that goes slow is still caught (the slowest non-`live_` one
  is 0.271 s here, 0.020 s on CI). **An unbounded pattern is refused outright**
  — a line matching an id belonging to no crate allows the whole workspace, and
  one fat-fingered `*` would disable the gate while the file still read as a
  careful list. Every debt row stays an exact id so
  `grep TODO(fast-unit-tests)` remains the deletion list.
  An entry is a **debt, not a licence**: it needs a one-line reason above it,
  measured on CI, and the list only shrinks. Rows marked
  `# TODO(fast-unit-tests):` are being made fast. A line matching nothing at or
  above the budget prints as `stale allowlist candidate: <line>` rather than
  failing — `live_powershell_*` needs `pwsh` on PATH, and a machine-dependent
  red is worse than a stale line.
- **Never `#[ignore]` a test to get under the budget.** That lands on
  `rust:test:ceiling` instead, which is the gate for exactly that move. Making
  the test fake what it is waiting on is the fix.
- **Seeing it red.** `task rust:test:duration:self-test` shows every state on
  throwaway fixture logs — red over budget, red under the reader floor, red on
  a pattern that matches nothing, red on an unbounded pattern, green in all
  three log spellings and through a pattern entry, and the same over-budget log
  failing under `CI=1` while printing the identical finding and passing without
  it; it then runs the shipped allowlist through the same reader. By hand
  against any run log (`CI=1` to see the verdict CI will reach):

  ```sh
  python3 scripts/unit_test_duration_gate.py target/nextest/run.log \
      --allowlist /dev/null          # red: every slow test is now unlisted
  python3 scripts/unit_test_duration_gate.py <(head -n 5 target/nextest/run.log)
                                     # red: the reader floor
  ```

## Observing a Running Suite

An acceptance run that has gone quiet is either **slow** or **hung**, and the two
need opposite actions. Never guess — poll four fields and let the discriminator
decide. Contention with another checkout's gate over the shared registry is the
usual cause of slow, and it is indistinguishable from hung until measured.

Three checks lie here. Each has produced a wrong verdict in this repo:

- **Never pipe a gate through `tail` / `head`.** The status you get back is the
  *pipeline's* — the pager's — and is always 0. `task claude:tests | tail -15`
  reported success over `29 failed`; `task website:build | tail` reported success
  over a hard failure. Redirect to a file, then read the file.
- **Never use `find -newermt` to test for recent activity.** It returns silent
  false negatives under this host's command proxy, so "nothing was touched" is
  evidence of nothing at all. Use `ls -lt` or `stat`.
- **Never read the parent's CPU time.** `uv run` and `pytest` both `wait()` on
  children, so both sit at `00:00:00` CPU straight through a healthy run. The
  work is always in a leaf `ocx` descendant.

| Field | Read it with | Means |
|---|---|---|
| Leaf process | `pgrep -a -P <pytest-pid>` | the `ocx` subprocess actually working. Legitimately absent *between* tests |
| Leaf age | `ps -o etime,time -p <leaf-pid>` | a package push older than ~5 min is wedged, not slow |
| Progress | `ls -1 <basetemp>/pytest-of-*/pytest-0/ \| wc -l` | test directories created; monotonic while progressing |
| Contention | `pgrep -c -f 'pytest tests/'` | `>1` means another checkout's gate is sharing the registry on `:5000` |

Verdict, across two polls 60-120s apart:

- Test-dir count rose → **slow**, not hung. Measure contention before killing anything.
- Count static, same leaf PID, leaf CPU static → **hung** in that subprocess.
- pytest alive, no leaf, count static → **hung inside pytest** — collection, a fixture, or a lock.

Killing a wedged run is scoped to your own checkout or basetemp
(`pkill -f 'basetemp=<yours>'`). Never `pkill -f 'pytest tests/'` — it matches an
identical run in every sibling worktree on this host.

## Adding a New Test

1. Create function in appropriate `test/tests/test_*.py` (or new file)
2. Use `ocx: OcxRunner` and `published_package: PackageInfo` fixtures
3. Call `ocx.json("command", pkg.short)` and assert results
4. Custom packages: use `make_package()` with `unique_repo` and `tmp_path`
5. Run: `cd test && uv run pytest tests/test_file.py::test_name -v`

For shell-friendly assertions (exec output, file existence, exit-code branches), prefer `test/scenarios/` — see Platform Split below.

`scripts/test_diff_guard.py` refuses an attribute call on a receiver the new code does not
own unless the verb is a pure read, so a few ordinary-looking spellings red at merge. The
verb is judged without the receiver's type — `shutil.copy` and `dict.copy`, `requests.get`
and `dict.get`, `pickle.load` and `json.load` are the same name — so the mandated spellings
are:

| Instead of | Write |
|---|---|
| `os.environ.copy()` | `env = dict(os.environ)`, then mutate `env` |
| `os.environ.get("X")` | `dict(os.environ).get("X")`, or `os.environ["X"]` |
| `json.load(fh)` | `json.loads(p.read_text(encoding="utf-8"))` |
| `Path(p).write_text(...)` on a path outside `tmp_path` | don't — the suite's inputs are frozen |

`pickle`, `marshal`, `shelve` and `dill` are refused by name: unpickling is `exec` under
another spelling, and a reviewer reading `loads(b"…")` sees a parsed fixture.

## Test Files

Test files cover: install, find, select, uninstall, purge, clean, offline, env, exec, package lifecycle, cascade, package pull, package description push, package description pull, inspect (candidates/metadata/resolution + `--closure` dependency closure + read-only no-index-growth contract), index, color, mirror, CI export, shell profile.

Acceptance coverage for the embedded Starlark host API (`ocx.*`, `expect.*`, the `ocx.os.*` / `ocx.arch.*` typed enum namespaces, and `RunResult`/`Platform` typed values) lives in `test/tests/test_package_test_script.py`. See [subsystem-script.md](./subsystem-script.md) for the host-API style rule those tests pin.

## Platform Split

Two complementary harnesses with different platform reach:

| Harness | Platforms | Use for |
|---------|-----------|---------|
| Pytest (`test/tests/test_*.py`) | **Linux only in CI.** The `acceptance-tests` job in `.github/workflows/verify-deep.yml` matrices a single `ubuntu-latest` leg — macOS runners lack the nested virtualization Docker/Colima needs, and the embedded `registry:2` image is unreliable on `windows-latest` (Linux container under Hyper-V routing issues); see the job's inline comment. Windows/macOS-only assertions in this suite (junction resolution, `.exe` shim behaviour, etc.) are real `skipif`-gated tests that currently run on **no** CI leg — treat them as unverified, not as coverage. | JSON-output assertions, structured fixtures, anything where Python expressivity beats shell |
| Shell scenarios (`test/scenarios/*.sh`) | Linux + macOS only (Windows skipped via `pytestmark` in `test_scenarios_smoke.py`) | Exec output, marker grep, file/dir existence, exit-code branches — bash is the natural language |

When extending shell scenarios, reuse the harness in `test/src/scenarios/__init__.py` (`Scenario` base class, `# scenario: <Name>` header, registered subclasses for pre-publish state). Do not duplicate setup logic — extend the existing `Scenario` API.

A behaviour assertion belongs in **one** harness, not both. If a pytest case can be expressed verbatim as a shell scenario, prefer the scenario; if it needs structured output parsing or Windows-specific paths, keep it in pytest.

## Shell-Activation Matrix (Docker)

Three self-contained (stdlib + pytest only) modules share one shell zoo:
`test_shell_activation.py` (login-shell activation), `test_shell_reconcile.py` (the
per-prompt reconciler, tiers 2 and 3) and `test_shell_reconcile_edge_cases.py` (one named
test per row of `analysis_shell_env_edge_cases.md`). Their shared helpers live in
`test/src/shell_matrix.py`, imported top-level as `import shell_matrix` — `test/pyproject.toml`
puts `src` on `pythonpath`, which is what lets the same import work inside the container.

**Five shells host a per-prompt hook: bash, zsh, fish, PowerShell, and elvish.**
`crates/ocx_shell/src/shell/hook.rs::registration` returns `Some` for each of those five and `None` only
for `Ash | Ksh | Dash | Nushell | Batch` — `ash`/`ksh`/`dash` have no append-safe prompt-hook point at
all, and nushell's hook is a different mechanism (`env_change.PWD`, fires on directory change rather
than every prompt, inlined in its shim body). Tier 3 (a real pty, the hook firing on its own) drives
bash, zsh (via the third-party prompt-framework coexistence rows), PowerShell and nushell's own
directory-change hook; elvish's is verified by hand instead of through the harness — a non-interactive
`elvish -c` binds no `edit:` namespace, so the emitted wrapper's `edit:add-var` raises before `ocx` is
ever defined, and the pty rig has no dependency that drives an interactive elvish (see `hook.rs`'s
`elvish_wrapper` test doc comment). Tier 2 (eval the emitted `self activate --reconcile` stream) reaches
all nine arms.

`test/tests/test_shell_activation.py` proves `ocx self setup` activation survives an **unset `OCX_HOME`** in every login shell — the durable net for a regression class where the managed block sources `env.*` to locate ocx but `env.*` is what sets `OCX_HOME`. It runs the real activation path per shell in a "shell zoo" container and asserts: exit 0, no missing-`env.*` error, the ocx bin dir lands on `PATH`, and (for POSIX/fish/pwsh) a second source does not duplicate it.

- **Files:** `test/docker/shells.Dockerfile` (Debian/glibc + nu/elvish/pwsh, plus starship / oh-my-zsh / powerlevel10k so the prompt-hook coexistence rows run rather than skip) and `test/docker/shells.alpine.Dockerfile` (Alpine/musl, busybox `ash`); `.github/workflows/shell-activation.yml` (build a static musl ocx once → run both image legs; triggers also include `shell.rs` and `command/self_group/**` so activation-logic changes run the matrix); the local entrypoint is `task test:shells` (Docker required).
- **Cross-platform (macOS + Windows):** `.github/workflows/shell-activation-deep.yml` — `workflow_dispatch` + weekly `schedule` (NOT per-PR; macOS/Windows minutes are costly). macOS reuses this same self-contained matrix, installing the ocx-packaged shells (nushell/elvish/pwsh) via `ocx package env`; Windows runs `test/manual/test-windows-activation.ps1` under built-in Windows PowerShell 5.1, built-in pwsh 7, and ocx-installed pwsh 7. cmd/batch PATH idempotency is covered by the `live_batch_*` unit test on the `verify-deep.yml` windows-latest `nextest` leg — which is also not per-PR any more (`workflow_dispatch`, the merge queue and pushes to `main`; see subsystem-ci.md "Verification tiers").
- **Self-contained:** resolves the binary from `$OCX_ACTIVATION_BINARY` / `$OCX_COMMAND` / `test/bin/ocx`, uses `shutil.which` to **skip-if-absent**, so a host `uv run pytest` stays green while the container runs the full matrix. It needs a clean child env (no `OCX_*` leakage such as a stale `OCX_HOME`) so the unset-`OCX_HOME` path is exercised; activation carries no guard variable and the prepend is idempotent move-to-front, so a re-source never duplicates it.
- **Pty driving no longer shells out to `script(1)`.** `test/src/shell_matrix.py`'s `pty_session` (plus its `line_editor_is_reading` keystroke-timing helper) drives real ptys with stdlib `pty`/`termios` directly. This removed a macOS/BSD-vs-util-linux `script` argument/stdin-forwarding split and an undeclared transitive dependency neither shell-zoo image ships (Debian moved `script` to `bsdextrautils`). `line_editor_is_reading` reads whether the pty's line discipline is out of canonical mode — the direct signal that a line editor (readline/ZLE/PSReadLine) is at a prompt reading keystrokes — so a line is fed only once the previous one went quiet **and** a line editor owns the pty, never on a wall-clock guess alone.
- **Known gaps (xfail/skip, tracked separately):** nushell applies the **global** toolchain only — its inlined `env_change.PWD` hook never calls `--reconcile`, so it cannot revert a project scope or advance `__OCX_ENV_STATE` (WP-12b of `adr_shell_env_overhaul.md`). Every nushell project-scope row skips through a helper that greps the shipped `env.nu` and reports the count it observed, so the skip disappears by itself once the arm lands. One `self activate` arm still emits a bare `ocx` rather than the absolute binary — nushell's `which` probe (`ENV_NU`) — pinned as a strict xfail (`test/tests/test_shell_reconcile.py::_BARE_OCX_XFAIL`). The elvish pin was removed: its global-env line now probes `?(test -x '<path>')` and calls the resolved absolute binary, so it had to gain an `ocx` wrapper (`has-external ocx` is a name lookup that finds a function).

## Benchmark Harness {#bench-harness}

`test/bench/` is a standalone performance harness, separate from the pytest acceptance
suite. It is not pytest-collected for normal runs.

| File | Role |
|------|------|
| `harness.py` | Entry point; owns session lifecycle (toxiproxy proxy, reachability, teardown) |
| `scenarios.py` | 21-row scenario matrix v3 + `SCALING_GROUP_ANCHORS` + `SUITE_BUDGET_SECONDS` |
| `baseline.py` | curl+tar floor command builder |
| `compare.py` | Pure comparison function + `__main__` exit-code handler |
| `report.py` | Pure `generate_report()` + `__main__` file-IO wrapper |
| `conftest.py` | Smoke-validation fixtures only (no Docker required) |
| `dashboard/template.html` | Vue 3 single-file app template for generated HTML report |
| `dashboard/vendor/vue.global.prod.js` | Vue 3.5.x global prod build (inlined into output) |
| `shell_latency.py` | Per-prompt reconcile latency gate (C-044): `exec_floor + Δ`, artifact-emitting, `task test:shell-latency` |
| `test/tests/test_bench_smoke.py` | pytest-collected smoke tests for harness internals |

Task targets: `task test:bench:setup`, `task test:bench`, `task test:bench:baseline`,
`task test:bench:teardown`, `task test:bench:quick`, `task test:bench:large`,
`task test:bench:scenario`, `task test:bench:report`. The `bench` Docker Compose
profile is isolated — `task test` never starts toxiproxy. See `test/bench/README.md`
for full usage.

## Test-Only Seams (`__testing` feature + `__OCX_*` env overrides)

When an acceptance test must force internal state that production code derives at runtime (the running self-image, the detected host libc, …), **do not invent a new env var or a plain `cfg(test)` gate.** There is one canonical project convention — reuse it:

1. **Cargo feature `__testing`** — declared `__testing = []` in each seam crate's `Cargo.toml` and forwarded from `crates/ocx_cli/Cargo.toml` (the list `testing_feature_forward_list_matches_grep` checks). Follows the Rust-ecosystem `__name` convention (axum `__private`, reqwest `__tls`): internal, no stability guarantee, never enabled by downstream code.
2. **Gate every seam** `#[cfg(any(test, feature = "__testing"))]` — `test` covers unit tests, `feature = "__testing"` covers the acceptance binary. **Release artifacts physically lack the code path.**
3. **Env-var name is double-underscore-prefixed `__OCX_*`** (e.g. `__OCX_SELF_IMAGE`, `__OCX_TEST_LIBC`) — the prefix signals "private test seam, not user-facing config." These are NOT documented in `website/src/docs/reference/environment.md` and are NOT forwarded via `Env::apply_ocx_config`.
4. **Defense-in-depth assert inside the gate** where misuse is dangerous — e.g. `__OCX_SELF_IMAGE` asserts the override targets a loopback registry, so even a build with the feature on cannot be coerced against a real registry.

The acceptance harness already builds with the feature: the binary under test is Bazel's `//crates/ocx_cli:ocx`, and every tier crate's `rust_library` in `crates/*/BUILD.bazel` sets `crate_features = ["__testing"]`. Adding a new seam needs **no build change** — just gate it and read the `__OCX_*` var. Reference implementation: `crates/ocx_package_manager/src/tasks/update_check.rs::ocx_cli_identifier` (the `__OCX_SELF_IMAGE` seam). Acceptance usage: `test/tests/test_self_update.py`.

## Unfalsifiable Greens

Shapes that pass without proving anything. See "Unchecked Green" in
[quality-core.md](./quality-core.md) for the general rule and its Block-tier.

| Shape | Why it passes anyway | Instead |
|---|---|---|
| **Arbitrary selection** — `next(x.glob(...))`, `[0]` off an iterator, an incidental sort key | Guarding emptiness is not guarding ambiguity. `rglob` order is directory order, not a contract, so the pick moves when a sibling file appears | Select by identity (a path the command reported, a predicate on the name); or assert cardinality where identity is genuinely unreachable |
| **Negative assertion over an empty iteration** | `for x in glob(...): assert not bad(x)` proves the negative vacuously when the glob matches nothing — and a drifted path is exactly how it matches nothing | Assert the collection is non-empty first |
| **Input and output share a path** | The assertion reads a file the command never had to touch | Check the asserted content differs from what the fixture authored |
| **Exit-code tolerance band** — `rc in (64, 65, 74)` | Cannot tell "still a stub" from "the binary rejected my input" | Assert the one exit code the contract names |
| **Text grep where a parser exists** — `assert "foo:" in content` | Passes against a file no parser accepts | Run the real parser and assert on its output |
| **A skip naming an assumed condition** | "skipped: X unimplemented" outlives X being implemented; the reason was never observed | Assert the condition, or observe it before skipping |

A whole file skipping itself away is indistinguishable from a pass — prefer a
failed assert on a missing prerequisite over `pytest.skip`.

## Quality Gate

Per task / review-fix iteration: `task verify:scoped --force` (T0 lint + rows check, then `test:parallel` over the touched modules at T1/T2). Full `task verify` runs at WP merge (enforced by the commit gate), at finalize, and whenever `verify:scoped` escalates (it then runs `task verify` itself). Direct `uv run pytest` never builds: it runs the existing `test/bin/ocx` (stale after Rust changes — refresh via `task test` / `task test:parallel`, which `bazel build` `//crates/ocx_cli:ocx` and copy it there; `task bazel:test:accept` runs that Bazel output directly and reads nothing from `test/bin/`).

**Never run `task website:build` while an acceptance suite is running.** Its
`website:recordings:ensure-binary` step rebuilds `-p ocx` in *release* — without
`--features ocx/__testing` — and copies the result over `test/bin/ocx`. A suite
that survives the swap runs its remaining rows against a binary with no test
seams and reports a meaningless green. The only thing that reliably prevents it
is the `Text file busy` you get from overwriting a binary that is executing.