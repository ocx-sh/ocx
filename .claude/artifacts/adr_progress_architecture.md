# ADR: Progress Architecture — Decouple from Tracing Spans

## Status

- **State:** Accepted — implemented in 5 phases (commits `31ee16d1`, `de7843e9`, `29ed83d2`, `223776a6`, + this hardening commit)
- **Date:** 2026-05-18
- **Deciders:** user (approved Option A, all 5 phases)
- **Supersedes:** the "Progress Pattern" section of `subsystem-package-manager.md` (tracing-indicatif span-attached model)
- **Cross-subsystem:** oci, package_manager, archive, cli, mirror

## Context

### Triggering defect

Flaky panic during concurrent package resolve/download (observed in `task website:build`, parallel tiny-blob pulls):

```
thread 'tokio-rt-worker' panicked at tracing-subscriber-0.3.23/src/registry/sharded.rs:317:
assertion `left != right` failed: tried to clone a span (Id(...)) that already closed
ERROR Task panicked: task 45 panicked ...
ERROR failed to find package: localhost:5000/t_ff7896d4_nodejs:24.0.0 — task panicked unexpectedly
```

### Root cause

Progress is driven entirely through `tracing-indicatif`'s `IndicatifLayer`. Progress state **is** the tracing span tree. Span lifetime is governed by tokio task/guard lifetimes, which interleave non-deterministically on the multi-thread runtime. `tracing-indicatif` 0.3.14 (latest; no upstream fix — main HEAD 2025-12-03) mutates per-span progress state in `on_new_span`/`on_enter`/`on_close` under a single `Mutex<ProgressBarManager>`; under concurrent close of sibling spans it drives the sharded registry's span ref-count to 0 while another worker thread still clones that span — hitting the `clone_span` `assert_ne!(refs, 0)`.

This is a **structural limitation of span-attached progress**, not a patchable bug: tracing guarantees no ordering between `on_close` on one task and `on_enter`/`on_new_span` on another running concurrently. Closed upstream issues #3/#14/#16 each patched a specific race; the architectural pattern remains risky exactly in OCX's scenario (concurrent JoinSet tasks, nested transitive-dep JoinSets, short-lived bars).

**Local amplifier:** `cli/progress.rs:98` clones `tracing::Span` into an `Arc<dyn Fn(u64)>` callback detached from the `bar.enter()` guard scope (`client.rs:370`, `:575`), adding cross-thread span-handle churn.

### Current surface (all span-coupled)

| Site | Pattern |
|---|---|
| `cli/progress.rs` | `ProgressBar` wraps `tracing::Span`; `bytes`/`files`/`spinner`; `callback()` clones span |
| `package_manager/tasks/{resolve,find,find_or_install,pull,find_symlink,deselect,uninstall}.rs` | spinner spans `.instrument()`-ed onto parallel JoinSet tasks, incl. nested JoinSet-in-JoinSet for transitive deps |
| `oci/client.rs:365`, `:567` | byte bars + cloned-span `Arc` callback (panic site) |
| `archive/{tar,zip}.rs`, `archive.rs:109` | file bars via `Span::current().pb_inc(1)` (span propagated into `spawn_blocking`) |
| `package/bundle.rs:64` | files bar |
| `ocx_mirror` (`pipeline/progress.rs`, `command/sync.rs`, `pipeline/orchestrator.rs`) | same `IndicatifSpanExt` path |
| `cli/log_settings.rs` `init_with_indicatif` | `IndicatifLayer` + `fmt_layer` writer = `indicatif_layer.get_stderr_writer()` |

### Industry context (research_progress_architecture.md, 2026-05-18)

- `indicatif::ProgressBar` is `Arc`-backed `Send + Sync + Clone`; clones share one instance via refcount; concurrent mutation from many threads is **panic-free** — no global registry. Bottleneck under extreme parallelism is throughput (internal lock), not correctness.
- `MultiProgress` is `Send + Sync`; `suspend(FnOnce)`, `println`, `insert_after` available for log interleave + nested bars.
- tracing/log interleave without `tracing-indicatif`: custom `MakeWriter` buffering + `MultiProgress::suspend` (~40 LOC), or `mp.println` for structured events.
- Prior art: **cargo** — hand-rolled RAII `Progress`, no indicatif, `Drop` clears, rate-limited. **uv** — indicatif 0.18.x, typed `Printer` enum, `hidden()` draw target when disabled. **turborepo** — indicatif 0.18.3.
- `indicatif` latest 0.18.4; `tracing-indicatif` pins 0.17.x. Dropping `tracing-indicatif` unblocks 0.18.x.

## Decision Drivers

1. **Race elimination guarantee** — must be structural, not probabilistic.
2. Robustness under concurrency + nested transitive-dep tasks.
3. Clean `fmt_layer` ↔ progress stderr interleave (no torn lines).
4. Bounded API churn across `ocx_lib` + `ocx_mirror`.
5. Replace the `archive` `Span::current().pb_inc` ambient pattern cleanly.
6. Testability — deterministic-enough stress regression for a race.
7. Effort / reversibility.

## Options

### Option A — Independent `ProgressManager` (own `indicatif::MultiProgress`)

`ocx_lib::cli::progress` rewritten: `ProgressManager { mp: Arc<MultiProgress> }` (`Clone`), constructed once at CLI startup, threaded as an explicit handle into `PackageManager` / `Client` / `archive` / mirror. Hands out **RAII guards** (`Spinner`, `BytesBar`, `FilesBar`) — move-only, `Drop` calls `finish_and_clear`/`abandon`. Byte callback captures a cloned `indicatif::ProgressBar` (no span). `tracing` becomes logging-only; `tracing-indicatif` dropped. Log interleave via a custom `MakeWriter` routing through `MultiProgress::suspend`. Disabled/non-TTY = `ProgressDrawTarget::hidden()` → all ops cheap no-ops.

### Option B — Minimal callback hardening only

Keep span coupling; only remove the `progress.rs:98` span clone (e.g. atomic + poller, or restructure callback lifetime).

### Option C — Keep coupling, serialize span open/close

Wrap progress span creation/close in a global mutex / single-threaded progress driver so siblings never close concurrently.

## Trade-off Matrix

| Criterion (weight) | A: ProgressManager | B: Callback hardening | C: Serialize spans |
|---|---|---|---|
| Race elimination (×3) | **Structural — impossible by construction** (5) | Probabilistic; upstream race survives (2) | Eliminates *this* race; reintroduces contention, fragile (3) |
| Concurrency/transitive robustness (×2) | High — Arc bars, no lifecycle coupling (5) | Low — span tree still load-bearing (2) | Medium — serialization hurts parallelism (3) |
| Log interleave (×2) | Clean (custom MakeWriter / println) (4) | Unchanged (relies on tracing-indicatif writer) (4) | Unchanged (4) |
| API churn (×2) | High — ~15 files, Context wiring (2) | Minimal (5) | Low–Medium (4) |
| archive pattern replacement (×1) | Explicit handle param — clearer (4) | Untouched (3) | Untouched (3) |
| Testability (×1) | Stress loop, no panic possible (5) | Stress loop, still racy (2) | Stress loop (3) |
| Effort / reversibility (×1) | High effort; reversible per phase (2) | Low effort (5) | Medium (3) |
| **Weighted total** | **~3.9** | **~2.9** | **~3.2** |

Reversibility: A is phased and each phase independently revertible; the public CLI/progress output is preserved, so user-visible behavior is unchanged.

## Decision

**Adopt Option A.** Only A makes the panic *impossible by construction* (the decisive ×3 driver). B leaves the upstream span-close race load-bearing — a symptom patch that violates "fix root cause, not symptom". C trades the race for contention and keeps the fragile coupling. The API churn cost of A is bounded and mechanical, and the project explicitly favors rewrite over compat shims for unreleased internals (`project_breaking_compat_next_version`).

## Design Spec

### Module: `ocx_lib::cli::progress`

```text
ProgressManager            // Clone; { mp: Arc<MultiProgress>, enabled: bool }
  ::disabled() -> Self     // hidden draw target; all guards are cheap no-ops
  ::stderr() -> Self       // MultiProgress on stderr
  .spinner(label) -> Spinner
  .bytes(label, total) -> BytesBar
  .files(label, total) -> FilesBar
  .writer() -> ProgressWriter   // MakeWriter for fmt_layer (suspend-based)

Spinner / BytesBar / FilesBar  // move-only RAII; !Clone
  Drop -> finish_and_clear()    // success default
  .abandon(msg)                 // failure path (explicit)
  BytesBar::callback() -> Arc<dyn Fn(u64) + Send + Sync>  // captures ProgressBar clone, NO span
  FilesBar::inc(n)              // replaces Span::current().pb_inc(n)
```

- `ProgressManager` lives in the CLI `Context` (singleton), constructed **before** subscriber init so its `MultiProgress` backs the fmt writer.
- Threaded explicitly: `PackageManager` and `Client` gain a `progress: ProgressManager` field (cheap `Arc` clone — matches "all fields cheap to clone" facade invariant). `archive::extract_with_options` / bundling take an explicit `Option<&FilesBar>` (or a small `ProgressSink`) param instead of reading `Span::current()`.
- Parallel tasks: each JoinSet closure takes a cloned `ProgressManager`, creates its own guard **inside** the async block; guard drops on task completion (success or error). No `.instrument()`, no spinner spans.
- Nested/transitive deps — **Phase 6 refinement** (initially flat; nesting restored on user feedback that the span tree's parent/child grouping was lost). A `tokio::task_local!` parent bar (`PARENT_BAR`), set by `Spinner::scope(fut)`, makes child `bytes`/`spinner` guards `insert_after` their parent with an indent `{prefix}`. The task-local holds a plain `indicatif::ProgressBar` `Arc` — never a span — so the clone-after-close class stays impossible. The `insert_after` staleness class is avoided structurally: the parent spinner guard outlives its scoped children, and if a parent is absent indicatif appends (no panic, unlike `tracing-indicatif` under `max_progress_bars`, which OCX does not set). Scoped at the download-bearing task spinners (`find_or_install`, `pull`) and the mirror prepare/push spinners.
- Visual parity: replicate current `{span_name}{msg}` / bytes / files templates as explicit `ProgressStyle` on each guard type so user-facing output is unchanged.

### Log interleave

`ProgressWriter` implements `tracing_subscriber::fmt::MakeWriter`; returns a buffer that on flush/drop calls `mp.suspend(|| stderr().write_all(&buf))`. Non-TTY path keeps the existing plain `init()` (no manager, direct stderr). `console_events`/`FmtSpan` behavior preserved.

### Disabled path

`ProgressMode::detect()` unchanged. Non-TTY → `ProgressManager::disabled()` (hidden target) and plain fmt subscriber. All guard methods become no-ops; zero overhead.

### Dependency change

Remove `tracing-indicatif` from `ocx_cli`, `ocx_lib`, `ocx_mirror` `Cargo.toml`; bump/confirm `indicatif` (0.18.x now unblocked — evaluate via `deps` skill, update `deny.toml`/`.licenserc.toml` if needed). Keep `tracing` / `tracing-subscriber` for logging.

## Phased Implementation Plan (TDD, workflow-feature)

Each phase: contract-first stub → failing test → implement → `task rust:verify` → review-fix loop → commit. Build green + visual output preserved at every phase boundary.

- **Phase 1 — Core + byte path (kills the panic).** New `ProgressManager` + guards + `ProgressWriter`; wire CLI `Context` + `log_settings` (manager before subscriber init); convert `client.rs` byte bars (`:365`, `:567`) to `BytesBar::callback` (no span). Drop `tracing-indicatif` from the byte path. *Regression: stress test (below) passes.*
- **Phase 2 — Spinner tasks.** Convert `package_manager/tasks/{resolve,find,find_or_install,pull,find_symlink,deselect,uninstall}.rs`: remove `.instrument(spinner_span(...))`/`.entered()` spinner spans; tasks own a `Spinner` guard. `PackageManager` gains `progress` field.
- **Phase 3 — Archive + bundle file bars.** Thread `Option<&FilesBar>`/`ProgressSink` into `archive::extract_with_options`, `archive/{tar,zip}.rs`, `package/bundle.rs`; delete `Span::current().pb_inc` + `archive.rs:109` span capture.
- **Phase 4 — Mirror migration + dep removal.** Migrate `ocx_mirror` (`pipeline/progress.rs`, `command/sync.rs`, `pipeline/orchestrator.rs`); remove `tracing-indicatif` from all `Cargo.toml`; `deps` skill pass; `task verify`.
- **Phase 5 — Regression hardening.** Land stress-loop test; update `subsystem-package-manager.md` "Progress Pattern" section + `arch-principles.md` ADR index row; full `task --force verify`.

### Regression test

Rust integration test (or pytest acceptance): under multi-thread tokio runtime, N concurrent tiny-blob pulls/resolves in a loop (≥100 iterations) with progress enabled (forced TTY/`MultiProgress` on a sink); assert process completes with no task panic. Locks in structural guarantee. Pre-Phase-1 it reproduces the panic (proof); post-Phase-1 it cannot.

## Risks & Mitigations

| Risk | Mitigation |
|---|---|
| `ProgressManager` creation vs subscriber init ordering | Construct manager first in `Context`/startup; subscriber borrows its `MultiProgress`; covered by an init-order test |
| Torn fmt/progress lines | `ProgressWriter` buffers per-event, flushes inside `mp.suspend`; verify visually + snapshot test |
| Visual regression vs current templates | Port exact `ProgressStyle` templates; doc-render harness / recordings catch drift |
| Wide API churn mid-refactor | Strict phase boundaries, each green + revertible; Two-Hats (no behavior change beyond removing the bug) |
| `indicatif` 0.17→0.18 break when dep bump | Gate behind `deps` skill; pin conservatively; defer 0.18 if churn unjustified (0.17.x still works once tracing-indicatif gone) |
| mirror behavior change | Phase 4 isolated; mirror acceptance tests as safety net |

## Follow-ups (config trail — apply at finalize)

- Add ADR row to `arch-principles.md` "ADR Index".
- Rewrite "Progress Pattern" in `subsystem-package-manager.md`; update `subsystem-oci.md` / `subsystem-mirror.md` progress mentions.
- Research evidence captured inline in "Industry context" above (worker-researcher returned findings inline, not persisted as a separate artifact).
