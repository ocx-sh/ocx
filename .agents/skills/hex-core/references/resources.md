# hex resource knob sheet

## Scope

**Conditional-load: read this file only when a run will build or test in
parallel.** A parse-only project (arcana's own `grim build <skill-dir>`)
never reaches it.

**This is a knob sheet plus three rules.** The sheet records what a tool's
knob is *called* and where its artifacts land — never which value a project
should choose, and never which command it should run. **hex never defines
how to verify a project**: that rule has one home, in
[`verify.md` § Verification](verify.md#verification), linked here and never
copied.

## Job budget

**Parallel builds share one build's budget:** live pipelines × jobs per
pipeline ≤ the project's single-build jobs (its own build config; `/hex-init`
records it). The orchestrator sets each pipeline's job knob (the sheet below
names it) to `floor(single-build jobs / live pipelines)`, at least 1, in the
step brief. RAM then stays at about one build with no measurement. Shared
caches do not substitute: a project can have them and still serialize.

## Gate-only tools

Heavy or exclusive tools (build servers, container-backed acceptance
projects, full suites, anything that cannot share a host with a second copy
of itself) run **only at a gate**. A step that needs a one-off exclusive
check hands it to the orchestrator in the background and returns. It does not
wait for the result.

## Never wait

**A worker never waits on a lock, a gate or a poll — it returns.** No shipped
text and no learned project lesson may add a worker-side lock or semaphore;
`/hex-init` flags one it finds in project memory or brief files. The
orchestrator's main loop only dispatches and merges; anything slow runs in
the background.

## Worktree cleanup

**Whoever creates a worktree removes it** when the work lands, with its build
outputs: the worktree's artifact directory (sheet below), redirected build
directories, daemons it started, containers it started. Rules:

1. **Delete only what this run created.** A target that does not resolve —
   after symlink resolution — under the run's own worktree or scratch root
   is reported and skipped. Another run's leftovers are swept and reported,
   never deleted. Ambiguity means report, not delete.
2. **Containers: label-scoped only.** `docker container prune -f --filter
   label=<the run's label>`, and only where the run set that label. Where it
   did not, report the leftovers and delete nothing. Never `docker system
   prune -f --volumes` — it destroys unrelated work on the machine.
3. **Daemons stop with the worktree** (`./gradlew --stop`), so no daemon
   outlives the tree it serves.

## Per-ecosystem knob sheet

What the orchestrator sets per pipeline. **Every cell names a knob, never a
value a project should choose.**

| Ecosystem | Parallelism default | Cap knob | Per-worktree artifact | Share / redirect | Retention |
|---|---|---|---|---|---|
| cargo | `-j nproc` | `CARGO_BUILD_JOBS`, `[build] jobs` | `target/`, 2–20 GB | do **not** share the target dir; `sccache` + `SCCACHE_BASEDIRS` | `cargo sweep --time N`; delete with the worktree |
| nextest / `cargo test` | ncpu | `--test-threads`, `--jobs`, `RUST_TEST_THREADS`, `threads-required` | tempfiles under `TMPDIR` | `TMPDIR` | — |
| pytest | xdist `-n auto` | `-n K` | `/tmp/pytest-of-*`, `.hypothesis`, `.coverage.*`, reports | `--basetemp`, `TMPDIR`, `HYPOTHESIS_DATABASE_FILE` | `tmp_path_retention_policy=failed`, `count=1`; `coverage combine && erase` |
| Jest | `cores-1` | `--maxWorkers`, `--workerIdleMemoryLimit` | `cacheDirectory` (OS tmp) | per-worktree `cacheDirectory` | `--clearCache` |
| Node deps | — | — | `node_modules`, ~2 GB | pnpm store + `node-linker=hardlink` | `pnpm store prune` |
| tsc | one V8 heap per process | `NODE_OPTIONS=--max-old-space-size` | — | — | — |
| Gradle | workers = ncpu; one daemon per JVM-args combo | `--max-workers`, `maxParallelForks`, `--no-daemon` | `.gradle/` | identical `org.gradle.jvmargs`; `~/.gradle/caches` shared | `./gradlew --stop` at cleanup |
| Go | `-p NumCPU` | `GOFLAGS=-p=K` | — | `GOCACHE` / `GOMODCACHE` shared by default | `go clean -cache` |
| Docker / testcontainers | — | — | containers, anonymous volumes, build cache | label every container the run starts | label-filtered container prune at cleanup |
| Playwright | — | — | 1.2 GB of browsers *if* `PLAYWRIGHT_BROWSERS_PATH=0` | leave the default `~/.cache/ms-playwright` | — |

`/tmp` is commonly a tmpfs sized at half of RAM: test artifacts written
there are memory. Point `TMPDIR` at a disk-backed directory where a suite
writes a lot.
