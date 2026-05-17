# ADR: Embedded Starlark Test Runner for `ocx package test --script`

## Metadata

**Status:** Accepted
**Date:** 2026-05-15
**Deciders:** Michael Herwig, Architect (/architect)
**Beads Issue:** N/A
**Related PRD:** N/A — handover at [`.claude/state/ocx-package-test-script-handover.md`](../state/ocx-package-test-script-handover.md)
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md` (Rust primary, no new runtime dependency on host)
- [ ] OR deviation justified in Rationale section
**Domain Tags:** api, devops
**Supersedes:** N/A
**Superseded By:** N/A

## Context

`ocx package test ... -- <cmd>` runs a single trailing command in the composed environment of a materialized package. It works well when the package ships its own runtime (`-- bun test ./tests/`). It breaks the OCX hermeticity promise in two cases:

1. **Tool packages without a runtime** (`cmake`, `shellcheck`, `goreleaser`) fall back to `-- sh -c '...'`. On `windows/amd64` this needs WSL/Git-Bash on the host — not hermetic, not cross-platform.
2. **Non-trivial test logic** — file-existence assertions, output parsing, multi-step sequencing — forces shell gymnastics or an external scripting runtime, contradicting the hermetic-test goal.

The project owner's stated trajectory is *more* complex tests over time, not fewer: compile-and-run tests for compiler packages, and (later, out of scope here) re-entrant `ocx` invocations that side-load extra artifacts. This rules out a purely declarative solution as the primary vector — the target is genuinely imperative orchestration.

This is not on the 0.3.0 critical path and does not land with the mirror-pipeline branch. It is forward design only.

## Decision Drivers

- **Hermeticity** — a test step must run identically on `linux/*`, `macos/*`, `windows/*` with no dependency on host shells or runtimes. Core OCX value prop.
- **Imperative expressiveness** — must scale to compile→run→assert sequences and (future) re-entrant `ocx` orchestration.
- **Sandbox safety (host-API scope only — Codex gate C2 honesty narrowing)** — the `ocx.*`/`assert.*` host surface and script-driven FS access are sandboxed: script FS writes confined to a scratch area, reads confined to {scratch, package} with symlink + best-effort TOCTOU containment, no host network/time/random. **NOT in scope:** binaries spawned via `ocx.run` run with normal host privileges, exactly as today's `-- <cmd>` trailing-command form does — OS/process-level confinement of spawned binaries is explicitly out of v1. "Hermetic" here means no host *shell/runtime* dependency, not that spawned binaries are jailed.
- **Reuse over reinvention** — env composition, materialization, child-process spawning, path-traversal guards already exist; the feature should be plumbing plus an interpreter, not a parallel pipeline.
- **Reversibility** — must not become a frozen backward-compat contract in published package metadata.
- **Marketing alignment** — a deterministic, Python-like, Bazel/Buck-familiar test DSL reinforces the reproducibility/hermeticity story (acknowledged as a soft driver, not the deciding one).

## Industry Context & Research

**Research artifact:** inline (worker-researcher findings, 2026-05-15) — not persisted separately; key facts captured below.

**Trending approaches:** Embedded deterministic config/script languages in Rust tooling — Starlark (Bazel/Buck2/Buck2-at-Meta-scale), and as a lighter alternative Rhai and Lua (`mlua`). OpenAI Codex CLI embeds starlark-rust to evaluate user-supplied `.star` permission-rule files — direct prior art for "load user scripts from files via host functions."

**Key insight:** `starlark` 0.13.0 (Meta-maintained, Apache-2.0, Buck2 production embedder) gives the required properties out of the box:

- Empty sandbox by default — no FS/network/time/random; only host-registered functions reachable. `load()` disableable via `Dialect { enable_load: false, .. }`.
- `#[starlark_module]` macro supports exactly the desired signature shape: positional list arg + keyword-only optional args with defaults (`run(args, *, env=None, cwd=None)`), so the API is forward-extensible by adding kwargs without breaking existing scripts.
- Error model cleanly separates assertion failure (`ErrorKind::Fail`) from script error (`ErrorKind::Parser | Function | Value`) → maps directly to an exit-code scheme.
- Defensive limit: `set_max_callstack_size` (recursion depth only). **Correction (review panel, 2026-05-16; refined Round-2):** `set_max_tick_count` and `set_max_heap_size` DO NOT EXIST in starlark 0.13.0 — only `set_max_callstack_size`. An earlier draft overstated the available limits. starlark 0.13.0 ALSO has no eval-deadline / cancellation / tick hook of any kind. The v1 runaway story is therefore: (a) `set_max_callstack_size` bounds recursion depth (the only in-process bound); (b) a per-`ocx.run` child-process kill deadline bounds I/O-bound scripts (the common case — scripts spend wall-time in subprocesses); (c) a pure-compute Starlark loop CANNOT be preempted in v1 — `tokio::time::timeout` around `tokio::task::block_in_place` is non-functional (the surrounding future never yields while the inline blocking closure runs), and the `!Send` `Evaluator` with non-`'static` roots cannot be relocated to a killable task. (c) is an explicitly accepted, documented v1 limitation, not a guard. `garbage_collect` is not a guard and is not called manually. The wall-clock timeout is best-effort, NOT a "primary" general runaway guard.

**Caveats from research:** `starlark::Error` deliberately does not impl `std::error::Error` (no accidental `?`); the crate explicitly does **not** promise API stability between releases (pin `=0.13.0`, treat upgrades as breaking); transitive footprint ~40–60 crates / ~23 MB — moderate, comparable to `clap + tokio + serde_json`.

## Considered Options

### Option 1: Embedded Starlark test runner (`--script PATH`)

**Description:** Add `--script PATH`, mutually exclusive with the trailing-command form. OCX materializes the package as today, then interprets the script with embedded `starlark` 0.13.0. Script gets a controlled `ocx.*` / `expect.*` API over the composed environment, plus a sandboxed writable scratch dir.

| Pros | Cons |
|------|------|
| Hermetic — zero host runtime/shell dependency | New ~40–60 crate dependency, pinned `=0.13.0` |
| Imperative — scales to compile→run→assert and future re-entrant `ocx` | starlark-rust API unstable across releases (pin + manual upgrades) |
| Sandbox by construction; reuses existing path-traversal guards | Interpreter is a permanent API surface to maintain |
| Reuses `composer::compose`, `pull_local`, `child_process`, `escapes_root` | No `while` loop — polling-style tests awkward (acceptable for this domain) |
| Bazel/Buck-familiar syntax, deterministic — marketing alignment | Implementation cost dominated by engine integration, not plumbing |

### Option 2: Declarative test manifest

**Description:** A non-Turing TOML/structured test block: `run` command, `expect_exit`, `expect_stdout_contains`, `expect_file`. No interpreter.

| Pros | Cons |
|------|------|
| Zero new heavy dependency | Cannot express compile→run→assert sequencing |
| Trivially deterministic, trivially sandboxed | Cannot express the owner's stated future (re-entrant `ocx`, conditional logic) |
| Smallest maintenance surface | Would need an escape hatch anyway → two systems to maintain |
| Covers ~80% of trivial smoke tests | Wrong baseline given the explicitly imperative target |

### Option 3: Status quo — trailing command only, document "ship a runner"

**Description:** Keep `-- <cmd>` as the only form. Document the pattern "package your own test runner" and accept `sh -c` for tool packages.

| Pros | Cons |
|------|------|
| No new code, no new dependency | Non-hermetic on Windows (`sh` dependency) — violates core value prop |
| Zero maintenance | Pushes complexity onto every package author |
| | Does not address the stated trajectory at all |

## Decision Outcome

**Chosen Option:** Option 1 — Embedded Starlark test runner.

**Rationale:** The decision driver that dominates is the owner's explicitly imperative target (compile→run→assert, future re-entrant `ocx` side-loading). That eliminates Option 2 as the *primary* vector and Option 3 entirely (it fails the hermeticity driver outright). Among imperative embeddings, Starlark wins on three grounds the alternatives do not jointly satisfy: (a) empty-by-default sandbox with no manual capability stripping, (b) language-level determinism as a *property* not a convention, (c) Bazel/Buck heritage that both lowers author learning cost and reinforces OCX positioning. Rhai/`mlua` are lighter but require manually constructing the sandbox boundary and offer no determinism guarantee — re-evaluating them is the documented fallback if the starlark-rust API churn proves unmanageable.

Option 2 is **not discarded** — it is demoted to a possible future thin sugar layer (a declarative block that desugars to a generated script) once the imperative engine exists. It is not built first.

The trailing-command form remains the default and documented entry point. `--script` and `-- <cmd>` are mutually exclusive (exit 64).

**Scope guardrail (reversibility):** Test scripts live in the *authoring* repository and are passed via the `--script` CLI flag. They are **never** referenced from package metadata or the OCI manifest. This keeps the embedded API a CLI-side, non-published surface — the engine choice stays a two-way door. This guardrail is non-negotiable for v1.

### Quantified Impact

| Metric | Before | After | Notes |
|--------|--------|-------|-------|
| Host runtime deps for tool-package tests | `sh`/WSL on Windows | none | Hermeticity restored |
| New transitive crates | 0 | ~40–60 | `starlark` pinned `=0.13.0` |
| Published-metadata surface added | 0 | 0 | Scripts are CLI-side only (guardrail) |

### Consequences

**Positive:**
- Hermetic, cross-platform package tests with no host shell/runtime.
- Single reusable scripting capability instead of per-author shell hacks.
- Forward-extensible API (kwargs) without breaking existing scripts.
- `--keep` already persists the temp dir → failed-script debugging is free.

**Negative:**
- New heavy, API-unstable dependency requiring a hard pin and manual upgrade review.
- A permanent script API surface to design conservatively and maintain.

**Risks:**
- *starlark-rust API churn* → mitigation: pin `=0.13.0`, register as a manually-tracked exact pin in `subsystem-deps.md` with an upgrade tripwire, isolate all `starlark*` usage behind one internal module. **Scope correction (review panel):** this firewall caps the *internal maintenance* blast radius of an engine swap ONLY. It is NOT a reversibility argument — the published exit-code contract and the corpus of author-written `.star` scripts are not reversible regardless of the firewall.
- *Sandbox escape via path args* → mitigation: route every script-supplied path through existing `lexical_normalize` + `escapes_root`; writes confined to scratch root, package root read-only. **Additional (review panel):** `ocx.run` refuses to spawn a binary resolving to `ocx` in v1 (see Open Question 1) so the v1 sandbox is not silently bypassable via unsandboxed nested `ocx`.
- *Runaway I/O-bound script* → mitigation: `set_max_callstack_size` (recursion depth — the only in-process bound); a per-`ocx.run` child-process kill deadline bounds wall-time spent in subprocesses (the common case). Best-effort, NOT a general "primary" guard.
- *Runaway pure-compute Starlark loop* (Round-2 honesty correction) → **NOT mitigable in v1.** starlark 0.13.0 exposes no eval-deadline/cancellation/tick hook; `tokio::time::timeout` around `block_in_place` does not fire (future never yields); the `!Send` non-`'static` `Evaluator` cannot move to a killable task. Accepted, explicitly documented v1 limitation — a pathological pure-compute loop hangs until the OS process is externally killed. **Future option (deferred, no v1 work):** run script eval in a separate killable OS process (self-re-invoked `ocx` script-runner mode) — the only way to hard-bound pure compute; out of v1 scope, would need its own ADR.
- *`Evaluator` `!Send` misuse* (review panel) → mitigation: `run_script` is sync, called via `block_in_place`; host fn uses `Handle::block_on`, never `.await`/`spawn_blocking`.
- *Scope creep into published metadata* → mitigation: the reversibility guardrail above, enforced at review.

## Technical Details

### Architecture

```
ocx package test --script smoke.star -p <plat> -i <id> <layers...>
        │
        ├─ materialize package  → temp root  (pull_local → setup_owned)        [reused]
        ├─ compose env          → resolve_env + Env::apply_entries/ocx_config  [reused]
        ├─ create scratch dir   → sibling of package root inside temp root     [new, thin]
        └─ run Starlark engine  (replaces the exec/spawn_and_wait branch)      [new]
               │  SYNC fn run_script (Evaluator is !Send+!Sync) called via
               │  tokio::task::block_in_place; wall_clock enforced as a
               │  per-ocx.run child-kill deadline (NOT timeout around
               │  block_in_place — that never fires); pure-compute loop
               │  unpreemptable in v1 (accepted, documented limitation)
               │
               ├─ Globals = GlobalsBuilder::standard().with(ocx_module).with(expect_module)
               ├─ Dialect { enable_load: false, ..Standard }
               ├─ Evaluator: set_max_callstack_size ONLY (no tick/heap setter in 0.13.0)
               └─ host fns spawn via own tokio Command (Stdio::piped) + composed Env;
                     child awaited via Handle::current().block_on (NOT .await /
                     NOT spawn_blocking); child_process has no capture variant
        │
        └─ map result → ExitCode (Ok(ExitCode), not Err — bypasses classify_error)
```

Engine integration replaces only the `td_guard.is_some()` / exec branch in
`crates/ocx_cli/src/command/package_test.rs` (~lines 225–242). All upstream
materialization and env composition is untouched.

**Concurrency correction (review panel, 2026-05-16):** `starlark::Evaluator`
is `!Send + !Sync`, so it cannot be held across an `.await` on a multi-thread
Tokio runtime. The earlier sketch (async `run_script`, async `ocx.run`) was
unsound. Corrected design: `run_script` is **sync**, called via
`tokio::task::block_in_place` (not `spawn_blocking` — its `'static` bound would
force cloning every borrowed root reference); the `ocx.run` host fn awaits the
child via `tokio::runtime::Handle::current().block_on(child.wait_with_output())`.
**Precondition (Codex gate C7):** `Handle::block_on` inside `block_in_place` is
correct ONLY on Tokio's multi-thread runtime — OCX `main.rs` uses the default
multi-thread `#[tokio::main]` today (no current bug); a future switch to
`current_thread` would panic this path. Encoded as a documented invariant +
`debug_assert!` on runtime flavor at the call site.

**Library error-type correction:** `run_script` is a permanent `ocx_lib`
public API. Per the Rust error-design rule it returns
`Result<ScriptOutcome, ScriptError>` where `ScriptError` is a small
`thiserror` enum (`HostSetup`, `RuntimeAbort`) — NOT erased `anyhow`. Erased
`anyhow` at a library boundary is Block-tier. Script-level failures
(assertion, syntax, sandbox, timeout) are NEVER `Err` — they are
`ScriptOutcomeKind`.

### API Contract (v1)

Script globals — `GlobalsBuilder::standard()` + two host modules. Verb-first
naming, kept minimal:

```
ocx.run(*args, *, env=None, cwd=None, stdin=None) -> RunResult
                                               # R2: positional varargs then kw-only;
                                               # first positional = program, rest = argv;
                                               # splat a list: ocx.run(*cmd)
                                               # zero positional args -> Failed
ocx.env(name) -> str | None
ocx.platform() -> {"os":..., "arch":...}      # reflects -p flag, not host
ocx.package_root() -> str                      # read-only
ocx.scratch_root() -> str                      # read-write sandbox root

ocx.read_file(path, *, max_bytes=1048576) -> str
ocx.write_file(path, content)                  # scratch-root only
ocx.exists(path) -> bool
ocx.mkdir(path)                                # scratch-root only, `mkdir -p` semantics

expect.ok(result, msg=None)                    # ERGONOMIC: result.exit_code == 0,
                                               # failure msg auto-embeds stderr
expect.eq/ne/true/false/contains/matches/fail(...)
```

> **Namespace note:** `assert` is a reserved lexer keyword in Starlark 0.13.0 — a
> script that uses `assert.*` parse-fails before evaluation and exits 65. The
> namespace is therefore `expect`. See ADR Changelog row (LDR-7).

**`RunResult`** = `{ exit_code: int, stdout: str, stderr: str, duration_ms: int,
truncated: bool }`. `ocx.run` does **not** raise on non-zero exit — the caller
asserts. Because asserting success is the dominant case, `expect.ok(result)` is a
first-class helper: it checks `exit_code == 0` and, on failure, builds the message
from `stderr` automatically (no manual `expect.eq(r.exit_code, 0, r.stderr)`
boilerplate). `expect.contains(result.stdout, ...)` covers output checks.

**Result rendering — `Printable`, `--format json|plain` IS v1 (R3, supersedes
the earlier review-panel "failure-text non-contract / defer JSON to v1.1"
framing, which is withdrawn):** the script outcome (overall status,
terminating assertion record, surfaced `RunResult` fields) is reported through
the EXISTING global `--format` mechanism — a new `api/data/` report type
implementing the `Printable` trait, emitted via `Api::report`, exactly like
every other OCX command (per `subsystem-cli` "Adding a New Report Type"). This
is v1, NOT deferred. It reuses the existing reporting path rather than
inventing a parallel one, which is what makes it backend-first by construction
(resolves the earlier concern by reuse, not by a non-contract caveat). The
ONLY non-stable surface is the exact human-readable *prose* of an individual
assertion-failure message; its *presence and JSON field shape are stable v1
contract* — tooling parses the fields, never the prose. The **exit code
remains the primary machine signal** (the JSON envelope is the structured
detail emitted alongside, not a replacement). Replicated in the plan and the
user-guide "Scripted tests" docs surface.

**Semantics of the two env surfaces (was ambiguous — clarified):**

- `ocx.run(..., env={"FOO": "bar"})` — the `env` kwarg is an **overlay dict
  applied on top of the composed package env for that one invocation**. `None`
  (default) = use the composed env unchanged. It does *not* replace the env.
  **Overlay policy (Codex gate C3):** (a) the overlay is applied to the child
  env *after* command resolution; `args[0]` resolution uses the **composed
  env's PATH only** — an overlay var can never change which binary runs
  (closes a hermeticity hole; hardens the v1 re-entrant-`ocx` refusal so env
  cannot redirect it). (b) The overlay **cannot override reserved keys**:
  `PATH`, `OCX_BINARY_PIN`, `OCX_HOME`, and OCX config/loader vars
  (`OCX_CONFIG`, `OCX_PROJECT`, `OCX_INDEX`, `OCX_NO_CONFIG`, `OCX_NO_PROJECT`,
  and any other resolution-affecting `OCX_*` in `OcxConfigView`). Setting a
  reserved key is a `Failed` outcome (chosen over silent-ignore so the author
  learns immediately).
- `ocx.env(name)` — **reads one variable from the composed package env**. Purpose:
  assert the package exported what it should, e.g.
  `expect.true(ocx.env("CMAKE_ROOT") != None)`. This is the package-env-composition
  test surface the owner asked about. (`ocx.envs()` dropped from v1 — `ocx.env`
  per-key covers the assertion use case; add later only if a real need appears.)

`stdin` kwarg = a string fed to the child process stdin (e.g. piping a source
snippet into a compiler that reads stdin). `None` = no stdin.

`ocx.run` future-extensible: extra packages / side-load / cycles arrive as new
keyword-only kwargs — existing scripts unaffected.

Path rule: every `path` arg AND `ocx.run(cwd=...)` is `lexical_normalize`d, then
`escapes_root`-checked against `{scratch_root (rw), package_root (ro)}`; absolute
paths and `..` escapes rejected. `/`-separators normalized on all platforms
(writable test scripts stay portable). `cwd` defaults to `scratch_root` so
compile artifacts land in writable space. `ocx.mkdir` is recursive and
idempotent (`mkdir -p`): creating an existing dir is not an error.

**Symlink + TOCTOU containment (Codex gate C1 — Claude review panel MISSED).**
Lexical guarding alone is insufficient for any path that is subsequently
*opened or used as a working directory*, not just writes. A script can
`ocx.run` a package binary that creates `scratch/link -> /` (or
`-> $OCX_HOME`), then `ocx.read_file`/`ocx.exists`/`ocx.run(cwd=...)`
traverses it; or race the check against the open/spawn (TOCTOU). Contract:
after the lexical check, EVERY path-consuming host fn (read-side included:
`read_file`, `exists`; plus `write_file`, `mkdir`, and `ocx.run(cwd=...)`)
re-validates symlink containment via the EXISTING
`crate::symlink::validate_target` / `validate_symlinks_in_dir` utility (no
hand-rolled symlink walk) and, where feasible, re-checks containment
post-canonicalize immediately before the syscall to shrink the TOCTOU window.
The residual check-to-open window cannot be fully eliminated against an
adversarial in-sandbox process that spawns binaries — documented as a known
best-effort bound and flagged for `/security-auditor`.

**Child-process lifecycle parity (Codex gate C4 — Claude review panel
MISSED).** The runner's own piped `tokio::process::Command` MUST replicate
`child_process::spawn_and_wait`: `kill_on_drop(true)` + SIGINT/SIGTERM
forwarding to the child. Without it, `package test --script` regresses
CI-termination / timeout-kill behaviour relative to the trailing-command
form. This same kill path is the concrete mechanism for the per-`ocx.run`
wall-clock deadline (Risks): deadline elapses → kill child → timed-out
outcome.

### Exit Code Scheme

Command returns `Ok(ExitCode)` (not `Err`) so it bypasses `classify_error` and
maps the Starlark `ErrorKind` directly:

| Code | ExitCode variant | Trigger |
|------|------------------|---------|
| 0 | `Success` | script completed, all assertions passed |
| 1 | `Failure` | `ErrorKind::Fail` (assertion / `expect.fail`) or host fn `Err` (`ErrorKind::Native`) |
| 64 | `UsageError` | `--script` + trailing cmd both given; missing/unreadable script file |
| 65 | `DataError` | `ErrorKind::Parser \| Function \| Value` (syntax / arity / type error) |
| 74 | `IoError` | scratch I/O failure (unchanged semantics) |
| 81 | `OfflineBlocked` | unchanged |

(`ErrorKind::Internal` → treat as `Failure`; no `2` invented — the existing enum has no `2`.)

### Editor / IDE Support

starlark-rust ships an LSP (`starlark --lsp`, lib `starlark_lsp`). Custom host
globals surface to completion/hover via `LspContext::get_environment()` returning
a `DocModule` whose members are generated **from the same `#[starlark_module]`
definitions used by the evaluator** — single source of truth, zero drift. This is
exactly how Buck2 exposes its rule API (`buck2 lsp`).

Phased, effort-ranked:

- **Phase A (zero effort, ship first):** add a recommended VS Code extension +
  `.star` language association in `.vscode/extensions.json` / settings (de-facto
  standard: `BazelBuild.vscode-bazel`). Syntax highlighting only — no `ocx.*`
  awareness.
- **Phase B+C (land together, medium effort):** ship an `ocx lsp`
  subcommand — a thin wrapper over `starlark_lsp` stdio server with a custom
  `LspContext` whose `get_environment()` is populated from the `#[starlark_module]`
  doc metadata. Gives full completion + hover for `ocx.run`, `expect.ok`, etc.
  Incremental cost over B is small because the macro already carries the doc data.
  Editors point `starlark.lspPath` at the `ocx` binary. **R4 — the subcommand
  carries `#[command(hide = true)]`: it is INTERNAL and UNSTABLE, never appears
  in `ocx --help`, and is NOT part of the user-facing CLI surface (OCX is a
  backend tool; the LSP name/wire is not a stability promise). It must be a
  subcommand (editors point at a binary + subcommand; a flag cannot serve
  this), but it is documented only in the authoring/IDE docs, not the
  command-line reference. The name `lsp` is intentionally generic to preserve
  room for a future dialect-selecting argument (e.g. `--dialect starlark`) if a
  second script dialect is introduced — the subcommand name would not need to
  change.**

Rejected: tilt-dev-style `.pyi` stub files — second source of truth, drifts from
the evolving module API.

### Testbed & CI

Adopt `ocx-mirror` directly as the first real exercise, not a synthetic fixture:

- **Package under test:** the `shfmt` mirror (`mirrors/shfmt/` in-repo) built via
  the existing mirror pipeline. Authoring testbed: sibling worktree
  `../mirror-shfmt-candidate/` (relative — siblings per CLAUDE.md Worktrees;
  matches the plan's Fix#14 correction).
- **Test script:** a `.star` smoke test — materialize `shfmt`,
  `r = ocx.run("shfmt", "--version")` (R2 varargs), `expect.ok(r)`,
  `expect.contains(r.stdout, "v3")`, then a compose check via `ocx.env(...)`.
  Proves the hermetic-tool-package case (the exact `sh -c` pain point)
  end-to-end against a mirror we own.
- **CI cost guard:** while proving the approach, run the scripted-test leg
  **linux-only**. Disable Windows + macOS runners for this leg (high CI cost) —
  re-enable once the approach is validated and the cross-platform claim needs
  proving. Document this as a temporary, intentional gap, not the end state.

## Implementation Plan

Design only — not for execution here. Suggested phasing for the eventual `/builder` handoff:

1. [ ] Isolate a **sync** `script` module in `ocx_lib` wrapping all `starlark*` (engine-swap firewall); `run_script` returns `Result<ScriptOutcome, ScriptError>` (`thiserror`, NOT `anyhow`); call site uses `block_in_place`.
2. [ ] Add `--script PATH` to `PackageTest`, mutually exclusive with `command` (clap `conflicts_with`) AND `command` carries `required_unless_present = "script"` so the neither-supplied case errors with exit 64. **R1: `--script -` reads the script SOURCE from stdin** (read to a `String`, fed to `AstModule::parse` with label `"<stdin>"`); stdin read failure → `IoError`/74. Disambiguation: `--script -` (parent stdin = script source) is independent of `ocx.run(stdin=...)` (per-call child stdin string).
3. [ ] Create scratch dir as sibling of package root in the existing temp root; thread both roots into the runner.
4. [ ] Implement `ocx.run(*args, *, env=None, cwd=None, stdin=None)` (**R2: positional varargs then keyword-only**; first positional = program, rest = argv; list var splatted `ocx.run(*cmd)`; zero positional → `Failed`) via own `tokio::process::Command` with `Stdio::piped()` + composed `Env` (already through `Env::apply_ocx_config`); child awaited via `Handle::current().block_on` (sync host fn, not `.await`/`spawn_blocking`); capture-to-memory with output cap + `truncated` flag; `env` overlay + `stdin` kwargs.
4a. [ ] **Refuse re-entrant `ocx`:** `ocx.run` rejects spawning when the resolved program is an `ocx` binary (hazard closure; awaits Q1 ADR).
5. [ ] Implement path-guarded file API (`read_file`/`write_file`/`exists`/`mkdir -p`) via `lexical_normalize` + `escapes_root`.
6. [ ] Implement `expect.*` (incl. ergonomic `expect.ok(result)`); assertion failures classified inside the firewall (the specific Starlark error mechanism is an engine internal — not named in any neutral type); expose builtin `fail()`.
7. [ ] Exit-code mapping per table; `Ok(ExitCode)` return path. **R3: build a `Printable` result envelope (`api/data/`) emitted via `Api::report` so `--format json|plain` works through the existing path in v1; exit code stays authoritative.**
8. [ ] `set_max_callstack_size` (the ONLY 0.13.0 limit) + per-`ocx.run` child-process kill deadline for the I/O bound. Do NOT implement `tokio::time::timeout` around `block_in_place` (non-functional). Document the pure-compute-loop hang as an accepted v1 limitation (no in-process preemption exists in starlark 0.13.0).
9. [ ] Testbed: `.star` smoke test against the `shfmt` mirror in `mirror-shfmt-candidate`; wire scripted-test CI leg **linux-only** (Windows/macOS legs disabled, documented as temporary).
10. [ ] Acceptance tests (pytest, `test/`): hermetic tool-package smoke + compile→run + sandbox-escape rejection.
11. [ ] Phase A editor support: `.vscode/extensions.json` recommendation + `.star` association.
12. [ ] Phase B+C: `ocx lsp` subcommand with `LspContext::get_environment()` fed from `#[starlark_module]` doc metadata.
13. [ ] Docs: add "Scripted tests" section to `website/src/docs/authoring/testing` once API stable.

## Open Questions (deferred — revisit before/with builder)

1. **Re-entrant `ocx` + side-loaded artifacts** (owner's stated future). **Honesty correction (review panel, 2026-05-16):** the earlier framing ("a script *can already* invoke pinned `ocx`") undersold the danger. Because `OCX_BINARY_PIN` is on the composed child env, a v1 `ocx.run("ocx", ...)` would be *unsandboxed re-entrancy* — the inner `ocx` writes the real `$OCX_HOME`, entirely outside the scratch sandbox. This is a HAZARD, not a latent feature. Furthermore the two-root `Access`/`resolve_guarded` model is NOT forward-compatible with nested-store sandboxing: Q1 will need a guard *rework* (a third inner-store axis), not an additive `ocx.run` kwarg. Therefore **v1 explicitly refuses to spawn a binary resolving to `ocx`** (added to the Implementation Plan), closing the hazard until Q1 gets its own ADR. The unsolved design — which store/temp root the inner `ocx` writes to, env re-composition, the `--side-load` mechanism — is the genuinely hard follow-up; the language choice does not constrain it. **Explicitly out of v1 scope.**
2. ~~Inline scripts (`--script -` from stdin)~~ — **PROMOTED to v1 (R1, user-directed 2026-05-16).** No longer deferred; see Implementation Plan step 2 + API/Invocation.
3. ~~Output format (TAP vs human + `--format json`)~~ — **RESOLVED in v1 (R3, user-directed 2026-05-16).** Result envelope is `Printable`, rendered via the existing `--format json|plain` / `Api::report` path in v1; no longer deferred. Only assertion-message *prose* is non-stable; field shape is stable. Exit code stays the primary machine signal.
4. Fail-fast vs collect-all — fail-fast for v1 (matches assertion-failure semantics).
5. Multiple test functions / discovery — single-script for v1.

## Validation

- [ ] **v1 verified on the linux acceptance leg** (no host `sh`). The
  cross-platform (macOS/Windows) hermeticity claim is **asserted-by-design**
  but its CI verification is **DEFERRED** until the Windows/macOS scripted-test
  legs are re-enabled — a documented, deliberate, temporary cost-saving gap,
  NOT the end state. (Codex gate C6: this criterion previously contradicted the
  same documents' mandated linux-only CI; reconciled here.)
- [ ] Sandbox-escape rejection tests pass: lexical (`..`, absolute, write
  outside scratch) AND symlink-on-read / symlink-as-`cwd` (Codex C1). TOCTOU
  residual window covered by `/security-auditor` review, not a gated (racy) test.
- [ ] `ocx.run(env=...)` reserved-key rejection + resolution-invariance tests
  pass (Codex C3).
- [ ] Child-process signal-forwarding parity with `child_process` verified
  (Codex C4).
- [ ] Security review of the host-function surface (new attack surface →
  /security-auditor) — explicitly covers symlink/TOCTOU residual window and the
  env-overlay policy.
- [ ] Dependency review of `starlark` tree (/deps — license + advisory scan,
  pin `=0.13.0`)

## Links

- Handover: [`.claude/state/ocx-package-test-script-handover.md`](../state/ocx-package-test-script-handover.md)
- [subsystem-cli.md](../rules/subsystem-cli.md), [subsystem-package-manager.md](../rules/subsystem-package-manager.md)
- [starlark 0.13 docs.rs](https://docs.rs/starlark/latest/starlark/) · [facebook/starlark-rust](https://github.com/facebook/starlark-rust)
- Prior art: [OpenAI Codex starlark integration](https://github.com/openai/codex/issues/8803)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-05-15 | Architect (/architect) | Initial draft |
| 2026-05-16 | Architect (/architect) | Review-panel Round-1 corrections: starlark 0.13.0 has only `set_max_callstack_size` (no tick/heap); `Evaluator` `!Send` → sync `run_script` + `block_in_place` + `Handle::block_on`; `run_script` returns `thiserror` `ScriptError` not `anyhow`; engine-firewall demoted to internal-blast-radius only (not a reversibility claim); Q1 reframed as hazard + v1 refuses re-entrant `ocx`; explicit v1 failure-text non-contract statement. Plan `plan_package_test_scripting` synchronized. (NOTE: this row's "wall-clock is primary guard" wording superseded by the Round-2 row below.) |
| 2026-05-16 | Architect (/architect) | Review-panel Round-2 honesty correction (supersedes Round-1 wall-clock wording): wall-clock is NOT a primary/general runaway guard. starlark 0.13.0 has no eval cancellation; `tokio::time::timeout` around `block_in_place` is non-functional; `!Send` non-`'static` `Evaluator` cannot move to a killable task. v1 guard = `set_max_callstack_size` (recursion) + per-`ocx.run` child-process kill deadline (I/O bound). Pure-compute Starlark loop is an explicitly accepted, documented v1 limitation; separate killable OS process recorded as deferred future option. Plan synchronized. Max-tier review cap reached — no Round 3. |
| 2026-05-16 | Architect (/architect) | Codex cross-model gate (one-shot, final): C1 symlink/TOCTOU containment extended to read-side + `cwd` (reuse existing `crate::symlink::validate_target`); C2 sandbox claim narrowed to host-API-only (spawned binaries have normal host privs, as `-- <cmd>` does); C3 `ocx.run(env=...)` overlay policy specified (applied post-resolution, PATH-only resolution, reserved keys → `Failed`); C4 child-process `kill_on_drop`+SIGINT/SIGTERM parity with `child_process` (also wires the R2-2 deadline); C6 Validation criterion reconciled with linux-only CI (cross-platform deferred, documented gap); C7 multi-thread runtime precondition encoded as invariant + `debug_assert!`. Plan synchronized. Codex one-shot — no re-loop. |
| 2026-05-16 | Architect (/architect) | Final user refinements (user-directed, no Codex re-loop): R1 `--script -` stdin script source promoted to v1 (Open-Q2 resolved; stdin read fail → 74; disambiguated from `ocx.run(stdin=)`); R2 `ocx.run(*args, ...)` positional varargs (not a list) — all list-form examples rewritten incl. testbed; zero args → `Failed`; R3 result envelope is `Printable` via existing `Api::report`, `--format json\|plain` IS v1 — the earlier "defer JSON / failure-text non-contractual" framing withdrawn (only message *prose* non-stable, field shape stable; exit code authoritative; Open-Q3 resolved); R4 `ocx starlark-lsp` `#[command(hide = true)]` INTERNAL/UNSTABLE, not in `ocx --help`; `ocx.path_join` helper dropped (YAGNI). Testbed worktree path also aligned to relative (matches plan Fix#14). Plan synchronized. Final edit before one spec-compliance validation + handoff. |
| 2026-05-16 | Doc writer (worker-doc-writer) | **LDR-7 rename — `assert.*` → `expect.*`:** `assert` is a reserved lexer keyword in Starlark 0.13.0 — a script containing `assert.*` call sites parse-fails before evaluation and exits 65. Renamed the assertion namespace to `expect.*` throughout (API contract, exit-code table, testbed examples, implementation steps). API contract note added. ADR Status flipped Proposed → Accepted per plan Documentation Surfaces mandate. |
