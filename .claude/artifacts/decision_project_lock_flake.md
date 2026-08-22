# Decision: `project::mutate` lock flake

Status: diagnosed, fixed, uncommitted in the working tree.
Surface: `crates/ocx_lib/src/project/project_lock.rs` (only file changed).
Date: 2026-08-22. Branch `evelynn`, worktree `/home/mherwig/dev/ocx-evelynn`.

## Symptom

`cargo test -p ocx_lib --lib` intermittently fails members of the
`project::mutate::tests::{add,remove}_binding_*` family with

```
Project(ProjectError { path: ".../ocx.toml", kind: Locked })
```

Each test uses its own `tempdir()`, and the lock lives on `ocx.toml` itself,
so no two tests share a lock file. A `Locked` there means the acquire was
refused by a lock nobody in the test was supposed to be holding.

## Reproduction

The trigger is **another process in the same test binary forking**, not CPU
load. Two recipes, both measured on this machine:

| Recipe | Command | Rate |
|---|---|---|
| Pure CPU load | full suite ×10, 48 `sha1sum /dev/zero` hogs, `--test-threads 32` | **0/10** — does not reproduce |
| Two concurrent suites | two full-suite processes in parallel ×8 rounds | **3/8 rounds** (4 of 16 processes) |
| **Targeted (fast)** | `ocx_lib-<hash> --test-threads 32 project::mutate shell::` ×10 | **8/10 runs, 53 lock panics** |
| Control | `... --test-threads 32 project::mutate` ×10 | **0/10** |

The targeted recipe is the one to use — ~7 s per run, high yield. `shell::`
is simply the nearest module whose tests spawn subprocesses (`bash`, `zsh`,
`dash`, `ksh`, `fish`, `pwsh`); any fork-heavy module does the same job.

All runs used `TMPDIR="$HOME/.cache/ocx-tmp"` (ext4, not a 9p mount — ruled
out as a factor).

## Root cause

**A forked child inherits the lock's open file description, so dropping the
guard in this process does not release the `flock`.**

`flock(2)` is held by the *open file description*, not by the fd number and
not by the process. `fork` duplicates every descriptor into the child, and
`O_CLOEXEC` — which Rust's `File::open` does set — only drops it at
`execve`, not at `fork`. So for the whole fork→exec window of any subprocess
spawned anywhere in the process, every lock this process holds has a second
reference keeping it alive. If the guard is dropped in that window, the lock
stays taken until the child execs.

Evidence, from instrumenting the `Ok(None)` branch of
`acquire_project_lock_for_file` to read `/proc/locks` and `/proc/self/fd`:

```
LOCKHOLDER      path=.../.tmp1fxeF0/ocx.toml ino=16004156 our_pid=1515496
                holder_pid=1515496 kind=FLOCK
LOCKHOLDER-END  path=.../.tmp1fxeF0/ocx.toml ino=16004156 our_pid=1515496
                own_fds_on_inode=[]
                children=["1516062:fish -c …", "1516074:fish -c …",
                          "1516396:pwsh -NoProfile -Command …"]
```

`own_fds_on_inode=[]` is the proof: **this process holds no fd on that
inode**, yet `flock(LOCK_EX|LOCK_NB)` returned `EWOULDBLOCK`. `/proc/locks`
still credits our PID because `fl_pid` is stamped at lock time and survives
the duplication. Some captures also showed children with an empty
`cmdline` — processes cloned but not yet through `execve`, i.e. the window
itself, caught in the act.

The same run reproduced `utility::fs::file_lock::tests::test_file_lock`
failing at `file_lock.rs:174` — `expect("acquired shared one")` immediately
after a synchronous `drop(lock)` in the same thread. Same mechanism, second
instance (see Not fixed).

Contributing design fact — `crates/ocx_lib/src/project/project_lock.rs:113`
before the change:

```rust
let maybe_guard = LockedFile::try_exclusive(config_path).await…?;
match maybe_guard {
    Some(guard) => Ok(guard),
    None => Err(… ProjectErrorKind::Locked),
}
```

One-shot. A single refusal became a hard error. The doc comment said
"caller should retry with backoff" — grep confirms **no caller anywhere does**
(`mutate.rs:306`, `mutate.rs:362`, `project_context.rs:619`), and
`error.rs:558` maps `Locked` straight to `ExitCode::TempFail`.

Measured width of the window, over 5 targeted runs with a 1 ms poll and a
5 s budget (40 samples): **min 2.16 ms, p50 2.84 ms, p90 5.35 ms, max
27.94 ms**.

## Production impact

**Yes — mild for the fork window, more real for the missing retry.**

- `MutationGuard` (`project_context.rs:619`) holds this flock across an
  entire `ocx add` / `ocx lock`, including network resolve. Any subprocess
  `ocx` spawns while that guard is live leaks the lock into the child for
  its fork→exec window; a concurrent `ocx` acquiring in that window got a
  spurious `Locked` → exit 75.
- Independently of forks: two parallel CI jobs doing `ocx add` in one project
  meant the loser failed immediately rather than waiting the few milliseconds
  the winner needed. That is the behaviour the docstring already disclaimed.
- Not a corruption risk. Mutual exclusion was never weakened — the failure
  mode is over-refusal, never a double writer.

## Options

1. **Bounded retry inside the acquirer.** Poll `try_exclusive` for a short
   budget before reporting `Locked`. Fixes both the flake and the CI
   papercut, in one place all callers already route through.
2. **Serialize the tests / stop spawning.** Test-only band-aid, leaves
   production untouched. Rejected.
3. **Switch to `LockedFile::open_exclusive_with_timeout`.** Rejected, and
   worth recording why: it runs a *blocking* `flock` inside `spawn_blocking`
   and abandons the task on timeout (`file_lock.rs:66-72`). The orphan later
   acquires the lock and immediately drops it — manufacturing exactly the
   spurious contention we are trying to remove. It also collapses the
   three-way outcome the `Locked`-vs-`Io` mapping depends on.
4. **Raise a timeout and call it a day.** There is no timeout to raise; the
   old path had none.

## Recommendation

Option 1, with a budget sized from the measurement rather than picked.

This is not a widened timeout papering over a guard held too long — the
instrumentation shows this process holding *no* fd at the moment of refusal,
so there is no over-long guard to shorten. The lock is genuinely held, by a
child, for a bounded interval nobody in the code can observe or shorten.
Waiting it out is the correct response, and a budget an order of magnitude
above the measured worst case leaves a real concurrent writer still failing
with `Locked` exactly as designed.

## Fix

`crates/ocx_lib/src/project/project_lock.rs` — one file, `+87/-22`,
**uncommitted**:

- `CONTENTION_BUDGET = 500 ms`, `CONTENTION_TICK = 25 ms` (the tick matches
  the house cadence already in `FileLock::lock_exclusive_blocking_with_timeout`).
  500 ms is ~18× the measured 27.9 ms worst case.
- `acquire_project_lock_for_file` polls `try_exclusive` until the budget is
  spent, then returns `Locked` as before. Acquired / contended / I/O-error
  stay three distinct outcomes.
- Doc comments corrected — the old text still described a non-blocking
  one-shot.
- Regression test `acquire_waits_out_a_transient_holder`: holds the lock,
  releases it after 60 ms (> one tick, so the acquire must really retry),
  asserts the second acquire succeeds.

Behaviour deliberately unchanged: the three tests that assert `Locked`
(`concurrent_mutation_contention_blocks_second_writer`,
`acquire_project_lock_blocks_second_writer`,
`add_binding_returns_locked_when_config_is_held`) all still pass — they now
take 500 ms each.

### Proof the regression test discriminates

| State | `cargo test -p ocx_lib --lib project::project_lock` |
|---|---|
| `CONTENTION_BUDGET` mutated to `0 ms` (one-shot, i.e. old behaviour) | **FAILED** — 3 passed, 1 failed; only `acquire_waits_out_a_transient_holder`, with `kind: Locked` |
| Restored to `500 ms` | **ok** — 4 passed |

Mutation and restore were each confirmed by grepping the file for the marker
before running.

## Before/after failure rate, same load recipe

| Recipe | Before | After |
|---|---|---|
| `project::mutate shell::` ×10 | **8/10 runs failed**, 53 lock panics | **0/10 runs failed, 0 lock panics** |
| Two concurrent full suites ×8 rounds | **3/8 rounds** | **0/8 rounds with any lock panic** |

Gate on the final tree: `cargo test -p ocx_lib --lib` → 4634 passed, 0
failed. `cargo clippy -p ocx_lib --all-targets --all-features -D warnings` →
clean. `cargo fmt --check` → clean.

## Not fixed — deliberately left for a decision

1. **`utility::fs::file_lock::tests::test_file_lock` has the same defect.**
   It failed once in 16 processes before the fix, at `file_lock.rs:174`,
   `expect("acquired shared one")` right after a synchronous `drop(lock)`.
   Same fork-inheritance cause. Not patched here because the fix is not
   free: the test's whole subject is the primitive's raw semantics, so a
   retry has to be applied to the *positive* acquisitions only (lines
   174, 175 and the two `now_or_never()` completions) and never to the
   negative assertions — the fork race can only manufacture spurious
   *contention*, so the `is_none()` assertions cannot false-fail and must
   stay strict. That is a judgement call about a lock primitive's own test,
   and it was outside the reported symptom.

2. **`shell::tests::live_powershell_list_matches_the_in_process_fold`** is an
   unrelated flake with the same trigger conditions: under two concurrent
   suites, `pwsh` aborts with `SIGABRT` and
   `System.IO.FileLoadException: The given assembly name was invalid` —
   a .NET runtime failure under memory pressure, nothing to do with locks.
   It is the only failure left in the after-fix pair runs (1 of 8 rounds).

3. **`FileLock::lock_exclusive_with_timeout` orphans its blocking task on
   timeout** (`file_lock.rs:66-72`), so a timed-out waiter still acquires
   and releases the lock later. No production caller reaches it on this
   path, but it is a latent source of exactly this class of spurious
   contention. Left alone — out of scope, and it deserves its own change.
