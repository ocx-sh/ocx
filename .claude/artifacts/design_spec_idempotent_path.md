# Design Spec: Idempotent (move-to-front) PATH manipulation

## Overview

**Status:** Draft
**Author:** Architect (`/architect`)
**Date:** 2026-06-19
**Issue:** [#26](https://github.com/ocx-sh/ocx/issues/26) · **Blocks:** [#170](https://github.com/ocx-sh/ocx/issues/170)
**ADR:** [`adr_idempotent_path_move_to_front.md`](./adr_idempotent_path_move_to_front.md)

Technical design for making every OCX PATH-prepend surface idempotent with
move-to-front semantics. The ADR fixed the *direction* (self-contained inline
emit, no helper functions; Rust-side helper for in-process surfaces). This spec
resolves the three open items the ADR flagged and pins the exact contracts,
per-shell constructions, escaping model, and test matrix for the Builder.

## Goals

- Re-sourcing/re-applying any PATH-prepend output never duplicates entries.
- **Move-to-front**: re-adding a present dir removes the old occurrence and puts it first (last activation wins lookup).
- **Self-contained emit**: each emitted shell unit works with no ocx process, no ocx-set guard var, capturable into a profile.
- Preserve the `Shell::export_path(key, value) -> Option<String>` one-call-one-return contract → emitter / `activate.rs` / shims untouched.
- No new dependency; reuse `PATH_SEPARATOR`, `VecExt::unique`, `escape_value`.

## Non-Goals

- Cross-provider PATH-precedence unification (GitHub LIFO vs GitLab leftmost stays provider-native — only dedup is added).
- Batch (cmd.exe) dedup — remains prepend-only (documented limitation).
- Constant (non-PATH) variable handling — already has conflict detection, untouched.
- Changing composition order semantics among distinct dirs — only duplicate removal + move-to-front of the re-added dir.
- **No `--idempotent` / `--no-idempotent` CLI flag, and no internal `IdempotencyStrategy` enum.** Idempotency is an unconditional correctness invariant (ADR "Rejected: idempotency as a CLI toggle"). No peer tool exposes such a toggle; `--no-idempotent` is incompatible with the capture invariant and #170; a `PrependOnly` enum variant would be dead code (Batch's prepend-only lives in its own `export_path` arm). `move_to_front` is the single behavior; the function name documents it.

## Surface inventory (from discovery)

| # | Surface | Location | Class |
|---|---|---|---|
| 1 | `Env::add_path` | `crates/ocx_lib/src/env.rs:204` | Rust in-process |
| 2 | `Shell::export_path` (10 shells) | `crates/ocx_lib/src/shell.rs:125` | Emitted shell text |
| 3a | `ci::prepend_existing` | `crates/ocx_lib/src/ci.rs:65` | Rust in-process |
| 3b | `GitHubFlavor::path_entries` | `crates/ocx_lib/src/ci/github_flavor.rs` | Rust in-process (→ `$GITHUB_PATH`) |
| 4 | `activate.rs::emit_path_prepend` | `crates/ocx_cli/src/command/self_group/activate.rs` | delegates to #2 (no change) |

---

## Part A — Rust-side helper (surfaces 1, 3a, 3b)

### A.1 New utility

```text
// crates/ocx_lib/src/utility/path.rs   (new module; wire into utility/mod)
//
/// Move-to-front dedup for a PATH-style value.
///
/// Splits `existing` on `PATH_SEPARATOR`, drops empty segments and every
/// segment equal to `value`, then returns `value` followed by the survivors,
/// re-joined. Infallible. `OsStr` to match `Env`'s `OsString` storage and to
/// avoid lossy UTF-8 round-trips on non-UTF-8 paths.
pub fn move_to_front(existing: &OsStr, value: &OsStr) -> OsString
```

Semantics:

- `move_to_front("", "/a")            == "/a"`
- `move_to_front("/b:/c", "/a")       == "/a:/b:/c"`
- `move_to_front("/b:/a:/c", "/a")    == "/a:/b:/c"`   (old `/a` removed, prepended)
- `move_to_front("/a", "/a")          == "/a"`         (idempotent)
- `move_to_front("/b:", "/a")         == "/a:/b"`      (trailing empty dropped — no `.`-in-PATH)
- `move_to_front("/usr/bin", "/usr/bin/x") == "/usr/bin/x:/usr/bin"` (no partial match)

Comparison is exact segment equality (no prefix/substring match). On Windows the
existing `EnvKey` already upper-cases the *key*; segment comparison stays
case-sensitive on the *value* (paths) — matches current `add_path` behavior; do
not introduce case-folding here.

### A.2 Wiring

- **#1 `Env::add_path`** — replace the unconditional `value + SEP + existing`
  branch with `move_to_front(existing, value)`. The empty/absent branch is
  subsumed (`move_to_front("", v) == v`). Stays infallible, signature unchanged.

- **#3a `ci::prepend_existing(key, values)`** — build move-to-front over the
  *combined* list. Reuse `VecExt::unique` (keep-first, order-preserving):

  ```text
  parts = values_in_accumulation_order ++ split(existing, SEP)
  result = unique(parts).filter(non-empty).join(SEP)
  ```

  New buffered `values` precede `existing` → a value already in `existing` is
  removed from the old position and kept at the front. Keep-first within `values`
  preserves accumulation order; identical dirs collapse.

- **#3b `GitHubFlavor::path_entries`** — `$GITHUB_PATH` is line-per-dir; the
  runner **reverses** before joining (last line written = highest priority) and
  itself dedups keeping the last occurrence. To avoid duplicate lines and stay
  consistent, dedup the buffer **keeping the last occurrence**, preserving order,
  before flush. `VecExt::unique` is keep-first → apply as
  `entries.reverse(); unique(); reverse()` (or add `VecExt::unique_last`). Write
  order unchanged. Document the runner LIFO behavior in a `//` comment with the
  toolkit reference.

---

## Part B — Shell emit (surface 2): temp-var-once construction

### B.1 Why temp-var-once (refines the ADR's inline idioms)

The literal dir must appear in **two roles** per line: as a *prepended value*
(double-quote context) and as a *match pattern* (glob context in zsh/bash). The
inline ADR forms interpolated the literal into both, demanding two different
escapes (double-quote vs glob/`/`-delimiter) for the same string in one line.

**Resolution:** assign the literal to a throwaway variable **once**, reference
the variable everywhere, and **quote the variable in the match position** so the
shell treats it literally (no glob), then `unset` the variable. This collapses
escaping to a **single context** (double-quote / shell-word) per shell — exactly
what the existing `escape_value` already produces — and removes all glob/`/`
pattern-escaping. The variable is created and unset within the same emitted unit,
so the line stays self-contained with net-zero namespace residue.

> The emitted unit is still one `Option<String>` from `export_path`; it simply
> contains a few `;`-separated statements. The emitter contract is unchanged.

Top-level note: emitted lines run at profile top level, **not** inside a
function — so `local` is unavailable. Use a plain assignment + `unset`. Variable
name: a collision-resistant fixed token, `__ocx_p` (documented; same risk class
as any tool's session helper).

### B.2 Per-shell final constructions

`E(x)` = the existing `Shell::escape_value` output for that shell (double-quote
context). `V` = the PATH dir. No glob/`/` escaping is required anywhere because
match positions quote the variable.

| Shell | Emitted unit | Subproc | Notes |
|---|---|---|---|
| Bash / Zsh¹ | `__ocx_p="E(V)"; PATH=":$PATH:"; PATH="${PATH//:"$__ocx_p":/:}"; PATH="${PATH#:}"; PATH="${PATH%:}"; export PATH="$__ocx_p${PATH:+:$PATH}"; unset __ocx_p` | 0 | Quoted `"$__ocx_p"` in `${//}` → literal match (no glob). Colon-wrap → exact-segment boundary. |
| Ash / Ksh / Dash | `__ocx_p="E(V)"; export PATH="$__ocx_p$(printf %s "$PATH" \| awk -v RS=: -v d="$__ocx_p" 'BEGIN{ORS=""} $0!=d && $0!=""{printf ":%s",$0}')"; unset __ocx_p` | 1 awk | `awk -v d="$__ocx_p"` passes via shell var — no re-escaping; literal `RS=:` (POSIX). Value interpolated once into the assignment. |
| Fish | `fish_add_path --prepend --move --path "E(V)"` | 0 | Builtin (fish 3.2+). `--move` = move-to-front; `--path` = `$PATH`. No temp var needed. |
| Elvish | `set paths = ["E(V)" (keep-if {\|p\| !=s $p "E(V)"} $paths)]` | 0 | `!=s` exact match → no glob. `keep-if` (0.21+); `each`-fallback for older — pick one, see B.4. |
| PowerShell | `$__ocx_p='E_sq(V)'; $__ocx_s=[IO.Path]::PathSeparator; $env:PATH=(@($__ocx_p)+($env:PATH -split [regex]::Escape($__ocx_s) \| Where-Object {$_ -and $_ -ne $__ocx_p})) -join $__ocx_s; Remove-Variable __ocx_p,__ocx_s` | 0 | `[IO.Path]::PathSeparator` = `;`/`:` per OS. Single-quote literal → `E_sq` = `'`→`''`. |
| Nushell | `$env.PATH = ($env.PATH \| split row (char esep) \| where {\|p\| $p != "E(V)"} \| prepend "E(V)")` | 0 | See B.3. `split row` guards string-vs-list. |
| Batch | `SET "PATH=E(V);%PATH%"` | — | Prepend-only (documented). |

¹ Bash and Zsh share the colon-sentinel form: `${var//"pat"/repl}` literal-match
works identically in both. Zsh's tied `path` array is *also* updated because zsh
keeps `PATH`/`path` in sync on any `PATH` assignment — so the scalar form needs
no `path=(...)` and avoids the `:#` glob caveat entirely. (This supersedes the
ADR's zsh `path=(... ${(@)path:#...})` sketch — the scalar colon-sentinel form is
simpler and removes the glob-escaping open item.)

### B.3 Nushell resolution (open item #1)

- Nushell ≥ 0.101 (Dec 2024) auto-converts `PATH` to a **list** before config runs.
- Robust cross-version form normalizes first: `split row (char esep)` yields a
  list from a string and is a no-op-shaped pipeline input on a list path that has
  already been split; `where` removes existing, `prepend` puts the new dir first.
  Idempotent, zero subprocess.
- **Validation required**: behavior of `split row` on an already-list `$env.PATH`
  differs by version. Builder MUST verify the chosen line on a real nushell
  (≥0.101 and one older if feasible). If `split row`-on-list mangles, fall back to
  the `describe`-guarded form:
  `($env.PATH | (if ($in | describe) == 'string' { split row (char esep) } else { $in }) | where {|p| $p != "E(V)"} | prepend "E(V)")`.
- Nushell is the **only** shell whose idempotency is gated on a live-shell
  acceptance test, not unit tests alone.

### B.4 Elvish version policy (open item carried)

Pick `keep-if` (elvish 0.21+, 2024) as the emitted form; document the minimum
elvish version in the env-composition reference. If OCX must support pre-0.21
elvish, emit the `each`-based equivalent
`(each {|p| if (!=s $p "E(V)") { put $p }} $paths)` instead. Builder picks one
based on the project's stated elvish floor (default: `keep-if`).

### B.5 Escaping invariant (security)

- The dir is interpolated by Rust into **exactly one** position per shell (the
  assignment, or fish/elvish/nushell string literals), through the existing
  `escape_value` (or single-quote escaping for PowerShell). Match positions
  reference the *quoted variable*, never a re-interpolated literal → no second
  escape context, no glob-escape gap.
- `is_valid_env_key` still gates the key (unchanged). `escape_value` still gates
  the value. No new injection surface is opened; this is a **Block-tier** review
  checkpoint (see Part D + security handoff).

---

## Part C — Surfaces inherited for free

- **#4 `activate.rs::emit_path_prepend`** calls `export_path("PATH", bin_path)` →
  receives the new idempotent unit. No change.
- **Shims (`shims.rs`) / `.envrc`** source `ocx self activate` / `ocx direnv
  export` → inherit idempotency. No change. The `_OCX_ENV_LOADED` guard has been
  **removed entirely** (see ADR amendment 2026-06-20); `shims.rs` no longer emits
  or checks it.

### C.1 direnv (verified — code path + direnv source)

`ocx direnv export` (`direnv_export.rs`) is **stateless** and routes its
PATH-prepend **entirely** through `emit_lines(Shell::Bash)` → `Shell::export_path`
(no `_OCX_APPLIED`, no deactivation, no independent prepend; evidence:
`direnv_export.rs:16-23,41,91`). The `.envrc` written by `ocx direnv init` is:

```sh
watch_file ocx.toml ocx.lock
eval "$(ocx direnv export)"
```

— **no inline prepend**. So direnv inherits the fix with **zero change to
`direnv_export.rs`**.

- **Safe under direnv's lifecycle.** direnv reverts the prior `DIRENV_DIFF` (whole-string PATH Prev/Next snapshot) then re-runs `.envrc` against the clean baseline → never accumulates across re-evals; restores the exact pre-entry PATH on leave, with **no reorder/restore bug** even when a parent dir is moved to front.
- **Not redundant.** direnv's revert covers only the re-eval case; the capture invariant (snippet sourced with no direnv/ocx) and #170 still require the emitted unit to be self-idempotent.
- **Gotcha — bash subprocess.** direnv evaluates `.envrc` in a **bash** subprocess regardless of the user's interactive shell, so the emitted unit MUST be valid *bash*. The Bash+Zsh `${//}` colon-sentinel form (B.2) is bash-valid; `Shell::Bash` is correctly hardcoded for direnv. **Spec constraint:** the Bash idiom may not rely on zsh-only syntax even though B.2 shares the arm — the shared form must stay strictly bash-compatible (it is: `${//}`, `${PATH:+…}`, `${PATH#:}` are all bash).
- `__ocx_p` temp var created+unset within the unit → never captured into `DIRENV_DIFF` (direnv records post-eval env only).
- **Testing note:** `direnv_export.rs` currently has no idempotency unit test. Acceptance test D.4 covers `.envrc` re-source; add a `direnv_export` emit-twice unit assertion so the guarantee is locked at the command layer too.

---

## Part D — Test matrix

Unit tests live inline (`#[cfg(test)] mod tests`) per module; process-env tests
use `crate::test::env::lock()`.

### D.1 `utility/path::move_to_front`

empty; single; prepend-new; move-to-front (mid); idempotent (already-front);
trailing/leading separator → no empty segments; partial-path (`/usr/bin` vs
`/usr/bin/extra`); repeated value in `existing`.

### D.2 Rust-side wiring

- `Env::add_path` twice same value → no dup; different value → move-to-front; `ocx run` / `ocx package exec` child env assertion.
- `ci::prepend_existing` — value already in process env → not re-added; new values precede existing; empties dropped.
- `GitHubFlavor` — duplicate dir buffered → single line; keep-last order; (document runner reversal).

### D.3 Shell emit — per shell (all 10), three classes

1. **Idempotency**: emit twice for same dir, simulate sourcing → one occurrence. (Where a real interpreter is available in CI: bash, zsh, fish, dash, pwsh, nushell, elvish — run it; otherwise assert the emitted string shape.)
2. **Move-to-front**: PATH pre-seeded with the dir mid-list → after eval, dir is first.
3. **Injection / escaping**: dir containing `$(touch x)`, `;`, backtick, `*`, `[`, space, `'` (pwsh), `(`/`)` (nushell) → no command execution, dir present verbatim, still idempotent.
4. **Boundary**: `/usr/bin` present, add `/usr/bin/extra` → both retained.
5. **Empty PATH**: no leading/trailing separator.

### D.4 Acceptance (pytest, `test/tests/`)

- `ocx package env <pkg> --shell <s>` captured + sourced twice → no dup (per available shell).
- `.envrc` / shim re-source → no dup.
- `ocx run` / `ocx package exec` PATH → no dup.
- CI export (`--ci=github`, `--ci=gitlab`) → no re-add of present value.
- **Nushell live test** (B.3).

### D.5 Acceptance-criteria mapping to issue #26

| Issue criterion | Covered by |
|---|---|
| Source `ocx env`/`ocx package env` twice → no dup | D.3.1, D.4 |
| Re-source shim/`.envrc` → no dup | Part C, D.4 |
| `ocx run` / `ocx package exec` → no dup | D.2, D.4 |
| CI `prepend_existing` no re-add | D.2, D.4 |
| Move-to-front (re-add → front) | A.1, D.1, D.3.2 |
| ~~Helper unset / once-per-block~~ | **superseded** — replaced by "each emitted unit self-contained + idempotent" (D.3.1) |
| Partial path preserved | D.1, D.3.4 |
| Empty PATH clean | D.1, D.3.5 |
| zsh/fish/elvish/**nushell** zero subprocess | B.2 |
| bash zero subprocess | B.2 |
| POSIX ≤1 awk | B.2 |
| PowerShell zero subprocess | B.2 |
| Batch prepend-only documented | B.2, docs |
| Per-shell idempotency tests | D.3 |
| **(added) Nushell covered** | B.3, D.4 |

---

## Risks & mitigations

| Risk | Mitigation |
|---|---|
| Nushell `split row` on list mangles | Live-shell test; `describe`-guard fallback (B.3) |
| Shell interpreter unavailable in CI for some shells | String-shape assertion fallback; run real interpreters where present (D.3) |
| Escaping regression opens injection | Single escape context per shell; injection tests (D.3.3); security-auditor handoff |
| `__ocx_p` collides with a user var | Fixed uncommon token; created+unset within unit; documented |
| Elvish version floor | Policy in B.4; doc the minimum version |

## Documentation surfaces

- `website/src/docs/reference/env-composition.md` (or shell-integration ref) — note idempotent move-to-front; batch limitation; elvish/nushell min versions. No migration prose (pre-1.0).
- Doc comments: `move_to_front`, `export_path` (per-shell rationale), `prepend_existing` / `path_entries` (runner LIFO). (`_OCX_ENV_LOADED` guard removed entirely — no doc reframe needed.)
- `crates/ocx_lib/src/utility/` catalog row in `arch-principles.md` (new `move_to_front` helper).

## Handoffs

- **Builder** (`/builder`) — implement per phased plan (`plan_idempotent_path.md`), contract-first TDD.
- **Security Auditor** (`/security-auditor`) — review the emit-escaping model (Part B.5) before merge; emitted shell code is an injection surface.
- **QA Engineer** (`/qa-engineer`) — own the per-shell test matrix (Part D), especially live-interpreter coverage and the Nushell gate.
