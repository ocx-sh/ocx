# ADR: Idempotent (move-to-front) PATH manipulation across all emit surfaces

## Metadata

**Status:** Proposed
**Date:** 2026-06-19
**Deciders:** Architect (planning agent), Michael Herwig
**Beads Issue:** [#26](https://github.com/ocx-sh/ocx/issues/26)
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md` — Rust 2024, no new dependency. Reuses `PATH_SEPARATOR`, the `Shell` emit machinery, `ci::prepend_existing`, and `utility/` extension-trait conventions.
**Domain Tags:** api | integration | devops
**Supersedes:** N/A
**Superseded By:** N/A
**Blocks:** [#170](https://github.com/ocx-sh/ocx/issues/170) (native per-prompt shell hook — hard-requires idempotent PATH)

## Context

PATH is prepended in several places across OCX, **none idempotent**. Sourcing the
same env output twice grows PATH with duplicate entries. This slows command
lookup, makes `echo $PATH` unreadable, and — critically — is a hard blocker for
the planned per-prompt native hook (#170), which re-runs on every prompt.

The discovery pass (`worker-architecture-explorer`) mapped the real surface area.
There are **four** independent prepend sites, falling into **two capability
classes**:

| # | Surface | Location | Class | Can inspect PATH at build time? |
|---|---|---|---|---|
| 1 | Runtime child env | `crates/ocx_lib/src/env.rs:204` `Env::add_path` | Rust in-process | **Yes** |
| 2 | Shell export lines | `crates/ocx_lib/src/shell.rs:125` `Shell::export_path` (10 shells) | Emitted shell text | **No** — runs later in a different shell |
| 3 | CI export | `crates/ocx_lib/src/ci.rs:65` `prepend_existing` (+ `github_flavor.rs` `path_entries`) | Rust in-process | **Yes** |
| 4 | `self activate` PATH prepend | `crates/ocx_cli/src/command/self_group/activate.rs` `emit_path_prepend` | Delegates to #2 | (via #2) |

Consumers:

- #1 → `ocx run`, `ocx package exec` (both via `Env::apply_entries`)
- #2 → `ocx env --shell`, `ocx package env --shell`, `ocx direnv export`, and #4
- #3 → `--ci[=provider]` flag on `ocx env` / `ocx package env`
- #4 → `ocx self activate`, sourced by the `$OCX_HOME/env.*` shims at shell startup

### Two findings that reshape the issue's design

**A. The "static shell scripts" surface collapses into #2 and #4.** `env.sh`/`env.fish`/… are *thin loaders* (`crates/ocx_lib/src/setup/shims.rs`): they `eval "$(ocx self activate)"`. `.envrc` runs `eval "$(ocx direnv export)"`. There are no inline `export PATH=` lines in those scripts. So fixing `Shell::export_path` (#2) transitively fixes re-source idempotency for the shims and `.envrc`. No separate "static script" work item exists (there is also no `install.sh` in this repo).

**B. The `_OCX_ENV_LOADED` guard is a *performance* mechanism, not a dedup mechanism.** `shims.rs:54` gates the `eval "$(ocx self activate)"` call so re-sourcing a profile does not re-spawn the `ocx self activate` subprocess (which itself spawns `ocx --global env`). It says nothing about PATH duplication, and **nothing may rely on it for correctness**. This matters because of the capture use case below.

### The capture invariant (decisive constraint)

A user may persist emitted output into a profile:

```sh
ocx package env cmake --shell bash >> ~/.bashrc
```

Every later shell sources `.bashrc` with **ocx not necessarily on PATH** and
re-runs that block. Therefore:

> **Emitted shell snippets must be self-contained and idempotent by
> construction** — they may not depend on ocx being present at eval time, on any
> ocx-set guard variable (`_OCX_ENV_LOADED`, `OCX_ACTIVATED`), or on being run
> through ocx at all. Idempotency is a property of the *emitted code*, never of
> external state.

This invariant is the architectural pivot of this ADR and the reason the
recommended option diverges from the issue's original "helper-function-per-block"
sketch.

## Decision Drivers

- **Capture invariant** (above) — self-contained, dependency-free, idempotent per emitted unit.
- **Move-to-front semantics** — re-activating an already-present dir moves it to the front (last activation wins for lookup), not "skip if present" (which would leave a stale earlier occurrence ahead).
- **Blast radius** — prefer the change that touches the fewest modules and preserves existing contracts.
- **Subprocess budget** — issue targets: native shells and bash zero-subprocess; POSIX ≤1 `awk` per entry; PowerShell zero.
- **No namespace pollution** — emitted code leaves no helper function or temp variable behind.
- **Industry alignment** — match how direnv / mise / nix emit env in eval contexts.

## Industry Context & Research

**Research artifact:** findings captured inline (two `worker-researcher` passes:
shell-snippet correctness + eval-output patterns). Cited sources at bottom.

**Key findings driving the decision:**

- **direnv and nix emit inline `export VAR=value`** with the fully-computed value — no helper function defined or unset. **mise installs a *persistent* named session function** (it lives for the shell lifetime to intercept subcommands). **None of the three use "define helper, then `unset`"** in eval output — the `unset` itself lands in the eval'd string and needs careful quoting. This directly contradicts the issue's Strategy B.
- **zsh**: `path`/`PATH` are tied unconditionally (built-in, every zsh, from line 1 of `.zshrc`); `typeset -U` not required. Caveat: `${(@)path:#pattern}` does **glob** matching — a path containing `[ * ?` could over-match. Mitigate by glob-escaping the literal at emit time.
- **fish**: `fish_add_path --prepend --move --path` is the correct, self-contained builtin (fish 3.2+, March 2021). `--move` relocates an existing element to the front; `--path` targets `$PATH` (session scope, must live in `config.fish`) and is idempotent on re-source.
- **elvish**: `$paths` is the list view of `$E:PATH`. Prefer `keep-if` (elvish 0.21+); `each`-based filter as fallback for older. `!=s` is exact string match — no glob risk.
- **PowerShell**: must use `[IO.Path]::PathSeparator` (`;` Windows, `:` Linux/macOS), never hardcode `;`. Split → filter empties + the value → rejoin.
- **bash**: a pure-builtin colon-sentinel removal (`${var//:pat:/:}` over a colon-wrapped copy) achieves zero-subprocess move-to-front. (The issue's `read -ra`/`${!1}` helper carries a bash-3.2 `export "var=val"` portability bug and needs indirect expansion.)
- **POSIX (dash/ash/ksh)**: single-char `RS=:` awk is POSIX-portable (regex `RS` is a gawk-ism). `${!1}` indirect expansion is **not POSIX** — but is unnecessary if the emitter writes the literal key name.
- **batch (cmd.exe)**: dedup is genuinely impractical (no array, `FOR /F` + delayed expansion corrupts `!`-bearing values). Prepend-only is consistent with chocolatey/scoop/winget.

**Trending approach:** inline, self-contained env diffs in eval output; persistent-state dedup pushed to the OS layer (Windows registry) where it exists.

## Considered Options

The Rust-side surfaces (#1, #3) are uncontroversial — a shared
`utility/path` move-to-front helper, applied in-process. The **real decision is
how #2 (`Shell::export_path`) achieves move-to-front when Rust cannot read the
runtime PATH.** Three options:

### Option S1: Self-contained inline line per entry (no helper, no framing)

**Description:** Each `export_path(key, value)` call returns **one** complete,
idempotent, dependency-free line that strips any existing occurrence of `value`
from the live PATH and prepends it. Native shells use list/array ops; bash uses
pure-builtin colon-sentinel removal; POSIX uses one `awk`; PowerShell uses
`-split`/`-join`. The current `export_path(key, value) -> Option<String>`
one-line-per-call contract is **preserved unchanged**.

| Pros | Cons |
|------|------|
| Self-contained at the finest granularity — one line, capturable in isolation | Individual lines are longer / less human-readable |
| Preserves `export_path` contract → `emit_lines`, `activate.rs`, shims **untouched** | zsh glob-match caveat needs emit-time escaping |
| No indirect expansion (`${!1}`) → emitter writes literal key → POSIX-clean, no `eval` | Nushell needs a list-aware form (issue omitted Nushell entirely) |
| Net-zero namespace: temp var created + `unset` within the same line | bash form must escape `/` in the path for the pattern |
| Matches direnv/nix inline emission | |

### Option S2: Helper function defined once per block, `unset` after (issue's original sketch)

**Description:** Emit a `__ocx_prepend()` helper once at the top of an output
block, call it N times, `unset -f` it at the end. Requires the emitter to grow
*block framing* (preamble + epilogue).

| Pros | Cons |
|------|------|
| Per-entry call lines are short and readable | **Breaks** the `export_path` one-line contract — needs block preamble/epilogue plumbing |
| One awk pattern reused across N POSIX entries | `activate.rs` emits a single PATH line → must special-case framing for one entry |
| | `unset` lands in the eval'd string (quoting footgun; the pattern no tool actually uses) |
| | Helper is generic over key name → needs `${!1}` indirect expansion → **not POSIX**, needs `eval` |
| | A captured *fragment* (one call line without its preamble) is broken — coarser self-containment |
| | bash-3.2 `export "var=val"` portability bug (macOS system bash) |

### Option S3: Persistent named helper (mise-style), never unset

**Description:** Define `__ocx_prepend()` once and leave it resident for the
session, mise-style.

| Pros | Cons |
|------|------|
| Simplest framing — define once, no cleanup | **Violates** the "no namespace pollution" acceptance criterion (helper persists) |
| Matches mise | A captured profile accretes a permanent ocx-named function |
| | Still needs `${!1}` indirection / framing like S2 |

## Decision Outcome

**Chosen Option: S1 (self-contained inline per line) for the shell-emit surface,
plus a shared `utility/path` move-to-front helper for the Rust-side surfaces.**

**Rationale:** S1 is the only option that satisfies the capture invariant at the
right granularity (one emitted line = one self-contained unit) while *preserving*
the `export_path` contract — so the emitter, `activate.rs`, and the shims need no
structural change. It also sidesteps the two correctness traps the research
surfaced (the `unset`-in-eval quoting footgun and the non-POSIX `${!1}`
indirection), because the emitter writes the literal key name and never defines a
function. S2/S3's only advantage — short, readable call lines — is weak: the
primary consumer is `eval`, and `ocx env` already serves the human-readable
key-value / JSON form. The acceptance criterion "helper defined at most once per
block" is rendered moot (there is no helper); it is **replaced** by "each emitted
line is independently self-contained and idempotent."

### Consequences

**Positive:**
- Re-sourcing any emitted output (shim, `.envrc`, captured profile block) is idempotent without ocx present.
- `Shell::export_path` contract unchanged → minimal blast radius (`emit_lines`, `activate.rs`, `shims.rs` untouched).
- Unblocks #170 — per-prompt re-emission no longer grows PATH.
- `_OCX_ENV_LOADED` / `OCX_ACTIVATED` guards keep their narrow perf role; correctness no longer depends on them.

**Negative:**
- Emitted eval lines are denser. Accepted: `eval` is the consumer; readable output is `ocx env` (table/JSON).
- Per-shell emit logic grows (one careful idiom per shell) and needs per-shell idempotency tests.

**Risks:**
- *zsh glob over-match* on paths with `[ * ?`. Mitigation: glob-escape the literal in the `:#` pattern at emit time; unit-test a bracketed path.
- *Nushell* PATH is emitted as a joined string today (`shell.rs:140`); a correct move-to-front needs list-aware handling. Mitigation: design-spec open item, test on a real nushell.
- *bash pattern escaping* — `/` must be escaped in `${//}` patterns. Mitigation: Rust escapes at emit; unit-test a path that is a prefix of another entry (`/usr/bin` vs `/usr/bin/extra`).

### Rejected: idempotency as a CLI toggle (`--idempotent` / `--no-idempotent`)

Considered a reusable flag (default on) across the PATH-emitting commands.
**Rejected — idempotency is a correctness invariant, not a user preference.**

- **No peer precedent.** Surveyed mise, direnv, rustup, pyenv, fnm, volta, asdf,
  nix/home-manager — **none** expose an idempotency/dedup toggle. All treat it as
  always-on correctness. `fish_add_path --move` is the only near-example and is an
  opt-*in* upgrade (skip-if-present → move-to-front), never an opt-*out*.
- **Skip-if-present is a known bug source.** rustup's `~/.cargo/env`
  skip-if-present guard caused a precedence bug (rustup #2848/#4723); the fix
  moved toward always-prepend. pyenv already ships our exact move-to-front
  (delete existing occurrences, prepend), hardcoded.
- **Incompatible with the unblocked use cases.** The capture invariant (a snippet
  piped into `~/.bashrc` must self-heal) and #170 (per-prompt hook) both
  hard-require idempotency — `--no-idempotent` has no valid consumer there.
- **Cost.** A flag on 6+ commands (`env`, `package env`, `run`, `package exec`,
  `direnv export`, `--ci`, `self activate`) multiplies API surface and contradicts
  the Backend-first / Determinism product principles.
- **No internal strategy enum either.** A `IdempotencyStrategy::{MoveToFront,
  PrependOnly}` enum was considered for testability; declined on YAGNI — the
  `PrependOnly` variant has no caller (Batch's prepend-only is a capability gap
  handled in its own `export_path` arm, not a shared strategy). The function name
  `move_to_front` documents the single behavior. If a concrete request ever
  appears, adding the enum (or flag) later is the same small change.

### direnv interaction (verified — code + direnv source)

`ocx direnv export` routes its PATH-prepend **entirely** through
`emit_lines(Shell::Bash)` → `Shell::export_path` (no independent prepend logic;
stateless — no `_OCX_APPLIED`/deactivation). The `.envrc` written by
`ocx direnv init` is `watch_file ocx.toml ocx.lock` + `eval "$(ocx direnv
export)"` with **no inline prepend**. → direnv inherits the move-to-front fix for
free; no change to `direnv_export.rs`.

Safe under direnv's lifecycle: direnv **reverts the prior `DIRENV_DIFF` then
re-runs `.envrc`** against the clean baseline, storing PATH as a whole-string
Prev/Next snapshot — so it never accumulates across re-evals and restores the
exact pre-entry PATH on leave (no reorder/restore bug even when a parent dir is
moved to front). The fix is **not redundant**: direnv's revert handles only the
re-eval case; the capture invariant (snippet sourced with no direnv/ocx present)
and #170 still require the emitted code to be self-idempotent. **Gotcha (folded
into the spec):** direnv evaluates `.envrc` in a **bash subprocess** regardless of
the interactive shell, so the emitted Bash idiom must be valid *bash* (the
`${//}` colon-sentinel form is) — `Shell::Bash` is correctly hardcoded. The
`__ocx_p` temp var is created+unset within the eval unit and never leaks into
`DIRENV_DIFF`.

## Technical Details

### Architecture

```
                         ┌─────────────────────────────────────────┐
   Entry(Path) ──┬──────►│ Rust-side move-to-front (in-process)     │
                 │       │ utility/path::move_to_front(existing,val)│
                 │       └─────────────────────────────────────────┘
                 │            ▲                         ▲
                 │   Env::add_path (#1)        ci::prepend_existing (#3)
                 │   GitHub path_entries dedup (#3)
                 │
                 └──────►┌─────────────────────────────────────────┐
                         │ Shell-emit move-to-front (emitted text)  │
                         │ Shell::export_path → one self-contained  │
                         │ idempotent line per call (#2)            │
                         └─────────────────────────────────────────┘
                              ▲              ▲              ▲
                        emit_lines    activate.rs    (shims/.envrc
                        (env/pkg env/  emit_path_      re-source these
                         direnv)       prepend (#4)    unchanged)
```

### API Contract — Rust-side helper (#1, #3)

```text
// crates/ocx_lib/src/utility/path.rs  (new; re-export via prelude if widely useful)
//
// Remove every existing occurrence of `value` from the PATH-style string
// `existing` (split on PATH_SEPARATOR, drop empties + matches), then prepend
// `value`. Infallible. OsStr-based to match Env's OsString storage.
fn move_to_front(existing: &OsStr, value: &OsStr) -> OsString
```

- `Env::add_path` (#1): call `move_to_front` instead of unconditional prepend.
- `ci::prepend_existing` (#3): build the joined value via `move_to_front` against `env::var(key)`.
- `GitHubFlavor::path_entries` (#3): dedup the accumulated Vec before flush, **keeping the last occurrence** (most-recent write wins — consistent with move-to-front).

### API Contract — Shell-emit (#2), verified idioms

`V` = the (escaped, per shell) literal directory. Each is the full return of one
`export_path("PATH", V)` call.

```bash
# bash — pure builtin, zero subprocess
__o=":$PATH:"; __o="${__o//:\/V:/:}"; __o="${__o#:}"; __o="${__o%:}"; export PATH="/V${__o:+:$__o}"; unset __o
```
```zsh
# zsh — native, tied path/PATH array (V glob-escaped)
path=(/V "${(@)path:#/V}")
```
```fish
# fish — native builtin (3.2+)
fish_add_path --prepend --move --path /V
```
```elvish
# elvish — keep-if on 0.21+, each-fallback older
set paths = [/V (keep-if {|p| !=s $p /V} $paths)]
```
```sh
# POSIX (dash/ash/ksh) — one awk, literal key (no ${!1})
export PATH="/V$(printf %s "$PATH" | awk -v RS=: -v v=/V 'BEGIN{ORS=""} $0!=v && $0!=""{printf ":%s",$0}')"
```
```powershell
# PowerShell — cross-platform separator, zero subprocess
$s=[IO.Path]::PathSeparator; $env:PATH=(@('/V')+($env:PATH -split [regex]::Escape($s) | Where-Object {$_ -and $_ -ne '/V'})) -join $s; Remove-Variable s
```
```bat
:: Batch — prepend only (documented limitation)
SET "PATH=/V;%PATH%"
```
```text
# Nushell — OPEN: list-aware move-to-front; current code emits joined string. Design-spec to resolve + test.
```

### Data Model

No persisted-format change. PATH remains a `PATH_SEPARATOR`-joined string
in-process (#1, #3) and shell-native ordering on emit (#2). No metadata or OCI
manifest impact.

## Implementation Plan

1. [ ] `utility/path::move_to_front` + unit tests (empty, trailing sep, prefix-not-matched, repeated-value).
2. [ ] Wire #1 `Env::add_path` → `move_to_front`; idempotency test for `ocx run` / `ocx package exec`.
3. [ ] Wire #3 `ci::prepend_existing` + `GitHubFlavor` Vec dedup (last-wins); CI export idempotency test.
4. [ ] Rewrite #2 `Shell::export_path` per-shell arms to the verified self-contained idioms; emit-time escaping (zsh glob, bash `/`).
5. [ ] Resolve Nushell list-aware form (design-spec); implement + test on nushell.
6. [ ] Per-shell idempotency unit tests (source twice → no dup; move-to-front → re-add moves to front; `/usr/bin` vs `/usr/bin/extra` boundary; empty PATH no leading/trailing `:`).
7. [ ] Confirm #4 + shims + `.envrc` inherit idempotency with **no** code change (re-source acceptance test).
8. [ ] Reframe `_OCX_ENV_LOADED` doc comment in `shims.rs` as perf-only; assert nothing depends on it for dedup.
9. [ ] Docs: env-composition / shell-integration reference notes idempotent move-to-front + batch limitation. No migration prose (pre-1.0).

### Acceptance-criteria delta vs issue #26

- **Dropped** (no helper exists under S1): "Helper function is `unset` after use"; "Helper defined at most once per block."
- **Replaced by**: "Each emitted line is independently self-contained and idempotent (capturable in isolation, no ocx/guard dependency)."
- **Added**: "Nushell move-to-front covered" (issue's design table omitted Nushell; 10 shells exist).
- **Retained**: zero-subprocess for native + bash + PowerShell; ≤1 awk for POSIX; batch prepend-only; partial-path preserved; empty-PATH clean; per-shell idempotency tests.

## Validation

- [ ] Per-shell unit tests: idempotent re-source, move-to-front ordering, boundary (`/usr/bin` vs `/usr/bin/extra`), empty PATH.
- [ ] Acceptance: `ocx package env <pkg> --shell <s>` sourced twice → no dup (all shells but batch); `.envrc` / shim re-source → no dup.
- [ ] `ocx run` / `ocx package exec` child env → no dup PATH.
- [ ] CI export does not re-add an already-present value.
- [ ] Security review: emit-time escaping closes no new injection surface (`escape_value` + `is_valid_env_key` still gate keys/values).
- [ ] No new dependency; `cargo clippy --workspace` clean.

## Links

- [Issue #26](https://github.com/ocx-sh/ocx/issues/26) — feature request (original helper-function sketch)
- [Issue #170](https://github.com/ocx-sh/ocx/issues/170) — native per-prompt hook (blocked by this)
- [`adr_ci_env_export_flag.md`](./adr_ci_env_export_flag.md) — `--ci` export surface (#3)
- [`adr_self_setup.md`](./adr_self_setup.md) — env.* shims + managed RC block (#4)
- [`handshake_toolchain_cli.md`](./handshake_toolchain_cli.md) — CLI authority for `ocx env` / `self activate`
- fish `fish_add_path` (3.2, 2021); zsh `${(@)path:#pat}` + tied `path`/`PATH`; elvish `keep-if` (0.21); PowerShell `[IO.Path]::PathSeparator`; direnv/mise/nix eval-output patterns

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-06-19 | Architect | Initial draft — S1 self-contained inline over issue's helper-function design; capture invariant; Nushell gap flagged |
| 2026-06-19 | Architect | Add "Rejected: idempotency CLI toggle" (no flag, no internal strategy enum — peer survey + YAGNI) and verified direnv-interaction section (inherits fix free; safe under revert-then-reapply; bash-subprocess gotcha) |
| 2026-06-20 | Builder | Amendment: guards removed for idempotent shells; batch keeps OCX_ACTIVATED (prepend-only) — see section below |

---

## Amendment 2026-06-20 — guards removed for idempotent shells (batch retains `OCX_ACTIVATED`)

**Supersedes:** the "Consequences / Positive" bullet point at line 169 which stated
"`_OCX_ENV_LOADED` / `OCX_ACTIVATED` guards keep their narrow perf role;
correctness no longer depends on them."

**Decision:**

- `_OCX_ENV_LOADED` (the `$OCX_HOME/env.*` shim double-source guard) is **removed
  entirely** from all five shims. No replacement.
- `OCX_ACTIVATED` (the `ocx self activate` global-env-eval guard + marker) is
  **removed for the nine idempotent shells** (ash/ksh/dash/bash/zsh/fish/pwsh/
  elvish/nushell) but **retained for batch (cmd.exe) only**. Batch `export_path`
  is prepend-only (cmd has no move-to-front primitive — out of scope per this
  ADR's batch row at line 92), so for batch the guard does genuine correctness
  work, not just perf: without it, repeated `ocx self activate --shell=batch`
  re-prepends the global toolchain and grows `%PATH%` toward the Windows length
  limit. cmd has no auto-sourced profile, so the batch guard never crosses the
  auto-activation boundary that caused the VS Code Remote leak. (Cross-model
  Codex review caught this; the original draft removed it for batch too.)

**Rationale:**

1. **Exported guards leak across process boundaries.** Both variables were
   `export`ed, which means a long-lived process — such as the VS Code Remote
   server (`vscode-server`) — holds the guard in its environment. Every integrated
   terminal spawned from that server inherits the guard, causing `env.sh` to
   short-circuit and never run activation. The result: tools missing in VS Code
   Remote terminals, while a fresh `ssh host` login (clean env) activates
   normally. This is a silent, environment-specific activation failure that is
   difficult for users to diagnose.

2. **Idempotency covers correctness — for the idempotent shells.** The primary
   motivation for the guards was preventing PATH duplication on re-source.
   Idempotent move-to-front PATH manipulation (the core of this feature) makes
   re-running activation safe by construction — re-sourcing produces the same
   PATH regardless of prior state. The guards therefore provide no correctness
   value **for the nine idempotent shells**. Batch is the exception: its PATH
   emission is prepend-only, so its guard is retained (see Decision).

3. **Re-source should refresh.** Removing the guards means a re-source (e.g.
   after `ocx --global upgrade`) picks up the new toolchain PATH immediately.
   With the guards in place, re-sourcing a shell that had already activated would
   silently skip the update.

4. **Perf cost is marginal.** The guards existed to skip spawning an `ocx`
   subprocess on re-source. Shell activation runs at init time only; the cost
   of executing activation unconditionally per shell is negligible and acceptable
   given the correctness benefit.

5. **Emit model is byte-stable — except batch's prepend-only PATH.** The two
   `ModifierKind` variants — `ModifierKind::Path` (idempotent move-to-front) and
   `ModifierKind::Constant` (absolute `export KEY="value"`) — produce the same
   output on every run with no accumulation, **for every shell whose
   `export_path` is move-to-front**. Batch's `export_path` is prepend-only, so
   re-running `ocx --global env --shell=batch` accumulates PATH entries — which
   is exactly why batch keeps its guard (see Decision).

**Scope of change:** `crates/ocx_lib/src/setup/shims.rs` (`_OCX_ENV_LOADED`
emission and guard check removed); `crates/ocx_cli/src/command/self_group/activate.rs`
(`OCX_ACTIVATED` emission removed). No other files required change.
