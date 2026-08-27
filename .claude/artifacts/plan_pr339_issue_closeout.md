# Plan: PR #339 issue closeout

## Status

- **Plan:** plan_pr339_issue_closeout
- **State:** ready
- **Tier:** high
- **Active phase:** 1 — triage complete, work packages ready to dispatch
- **Step:** triage → dispatch
- **Last update:** 2026-08-27 (triage of the 13 open issues arising from [ocx-sh/ocx#339](https://github.com/ocx-sh/ocx/pull/339), plus [#170](https://github.com/ocx-sh/ocx/issues/170) and [#265](https://github.com/ocx-sh/ocx/issues/265))
- **Next:** dispatch P1/P4/P5/P6/P7/P8/P9 in parallel worktrees; P2 and P3 wait on the `reconcile.rs` lane

---

## Overview

**Status:** Approved
**Branch:** `feat/shell-env-overhaul` @ `705d4afb` (worktree clean)
**PR:** [ocx-sh/ocx#339](https://github.com/ocx-sh/ocx/pull/339)
**Related ADR:** [`adr_shell_env_overhaul.md`](./adr_shell_env_overhaul.md) + [`adr_shell_env_addenda.md`](./adr_shell_env_addenda.md) (binding on semantics)
**Related plan:** [`plan_shell_env_overhaul.md`](./plan_shell_env_overhaul.md) (§0 precedence: code > addendum > ADR > design spec on semantics; plan wins on decomposition)

### Objective

Close every issue arising from [#339](https://github.com/ocx-sh/ocx/pull/339) before the squash + finalize, under three binding owner rules:

1. **Do not get paranoid with security issues.**
2. **Explicitly document the decision to always consent the global toolchain, and do not change it.**
3. **Stick to [`adr_shell_env_overhaul.md`](./adr_shell_env_overhaul.md).**

### Decision summary

| Issue | Decision | Package | Prod LOC |
|---|---|---|---|
| [#170](https://github.com/ocx-sh/ocx/issues/170) native shell hook | CLOSE-ALREADY-DONE | P11 | — |
| [#265](https://github.com/ocx-sh/ocx/issues/265) `unset` directive | DEFER (ADR excludes) | P11 | — |
| [#343](https://github.com/ocx-sh/ocx/issues/343) orchestration in `ocx_cli` | FIX | P3 | ~40 net (~600 moved) |
| [#344](https://github.com/ocx-sh/ocx/issues/344) clause 2 on a lock claim | CLOSE-ALREADY-DONE | P11 | — |
| [#345](https://github.com/ocx-sh/ocx/issues/345) `reconcile.rs` three concepts | FIX | P2 | 0 net (~1315 moved) |
| [#346](https://github.com/ocx-sh/ocx/issues/346) host-capabilities path | CLOSE-ALREADY-DONE | P11 | — |
| [#347](https://github.com/ocx-sh/ocx/issues/347) framework clobbers registration | FIX | P4 | ~50 |
| [#348](https://github.com/ocx-sh/ocx/issues/348) `record_origin` without wire | DEFER (persisted-format decision) | P11 | — |
| [#349](https://github.com/ocx-sh/ocx/issues/349) elvish/nu greens can't go red | CLOSE-ALREADY-DONE | P11 | — |
| [#350](https://github.com/ocx-sh/ocx/issues/350) A-09 prefix not canonicalized | FIX | P1 | ~20 |
| [#351](https://github.com/ocx-sh/ocx/issues/351) casts record a dead reconciler | FIX | P5 | ~60 |
| [#352](https://github.com/ocx-sh/ocx/issues/352) four addenda too wide | FIX | P6 + P10 | ~10 |
| [#353](https://github.com/ocx-sh/ocx/issues/353) EC rows wait on a skipped leg | FIX | P7 + P10 | 0 (~120 test) |
| [#354](https://github.com/ocx-sh/ocx/issues/354) Deep gate installed no shells | **FIX-DONE** ([`b0d10b91`](https://github.com/ocx-sh/ocx/commit/b0d10b91)) | P11 | — |
| [#355](https://github.com/ocx-sh/ocx/issues/355) conformance-drift 404 | **FIX-DONE** ([`7d2c23ae`](https://github.com/ocx-sh/ocx/commit/7d2c23ae)) | P11 | — |
| — global-toolchain consent record | FIX (owner directive) | P9 + P3 + P6 + P10 | 0 (prose only) |

Six FIX (open), two FIX-DONE on the branch, four CLOSE-ALREADY-DONE, two DEFER.

**Correction, 2026-08-27 (HEAD `7d2c23ae`).** [#354](https://github.com/ocx-sh/ocx/issues/354) and [#355](https://github.com/ocx-sh/ocx/issues/355) were fixed on the branch while this triage ran, and my DEFER on #354 rested on a wrong diagnosis. #354 was not an announce gap: the Deep workflow asked the index for flat `ocx.sh/pwsh` when the real roots are namespaced (`powershell/powershell`, `nushell/nushell`, `elvish/elvish`), and the macOS leg wrapped the resolve in `eval "$(...)"`, which exits 0 when the substitution dies — so the gate read green while resolving no shells at all. Fixed in [`b0d10b91`](https://github.com/ocx-sh/ocx/commit/b0d10b91). #355 was `bot/tests/golden` moving to `ocx-sh/indexbot`, hiding real `owners[]` drift — fixed in [`7d2c23ae`](https://github.com/ocx-sh/ocx/commit/7d2c23ae). **Package P8 is retired; both issues move to P11 for closure.** The `manual-only` retier in [#353](https://github.com/ocx-sh/ocx/issues/353) no longer has "the Windows job cannot complete anyway" as a supporting argument — see open question 4.

**Correction, 2026-08-27 (P6).** A-30's premise was also falsified: the two canonicalisation helpers never diverged — `ProjectRegistry::register` re-canonicalises through `dunce` before `name_for_path`, so the ledger key and the consent key were both dunce-derived. §1's "latent defect" framing and §4's acceptance-check row are therefore wrong as written; the specified check ("red before: the two helpers diverge") has no reachable red and was deliberately not shipped. The current text is A-30 in [`adr_shell_env_addenda.md`](./adr_shell_env_addenda.md).

---

## 1. Per-issue decisions

### [#343](https://github.com/ocx-sh/ocx/issues/343) — FIX (package P3)

**Root cause.** The per-prompt state machine — `Outcome` construction, the consent gate, walk determinacy, fingerprinting and `next_ledger` (which builds the persisted `__OCX_ENV_STATE` payload) — lives in `ocx_cli`, so `ocx shell state` has no shared entry point and re-derives the same answers independently.

**Files.**
- `crates/ocx_cli/src/command/self_group/activate.rs` (3538 lines; production ends 1467) — moves out: `compose` 385-484, `mod consent_gate` 610-650, `project_entries` 693-758, `next_ledger` 853-949, `ledger_lines` 959-989, `resolve_walk` 1065-1109, `walk_is_indeterminate` 1119-1140, `ProjectIdentity` 1027-1035. `reconcile_plan` 768-770 is already a thin wrapper and collapses into the callee.
- `crates/ocx_cli/src/command/shell_state.rs` (744 lines) — `ResolvedProject` 213-226 and `resolve_project` 256-310 collapse onto the shared entry point. Both call `consent::canonical_project_dir` in a `spawn_blocking` and `ReferenceManager::name_for_path` identically today.
- `crates/ocx_lib/src/activation.rs` — **new**. Planned as `shell/reconcile/session.rs`; shipped at the crate root instead — see Open question 1 below, which this move answers.
- `crates/ocx_lib/src/lib.rs` — add `pub mod activation;`.

**The planned nesting was not available.** `crates/ocx_lib/src/shell.rs:8` already carries `pub mod reconcile;`, so a `session` module under it needed no top-level `mod` line — but it would have closed a module cycle: `project::consent` reads `shell::coexistence::Observation` and `shell::reconcile::ScopeId`, and the sequencing reads `project::consent`. No other file under `shell/` reaches for `project`, so the cycle would have been that one file alone, and a `use` cycle does not compile across a crate boundary — a blocker for the planned `ocx_lib` split ([#313](https://github.com/ocx-sh/ocx/issues/313), [#324](https://github.com/ocx-sh/ocx/issues/324)). At the crate root the sequencing may depend on both, and both stay independent of it. `shell/reconcile.rs` keeps only the three pure pieces (carrier format, planner, fingerprint), and a directory-walk test — `shell_does_not_import_project`, in `activation.rs` — holds it there. `crates/ocx_cli/src/command/self_group.rs:8` and `crates/ocx_cli/src/command.rs:71` are unaffected as planned.

**Production LOC.** ~0 net — roughly 600 lines relocate. Budget ~40 lines of glue for the parameterised input struct that keeps `session` `Context`-free (Decision 5 option C: no `Context::try_init` in the lib).

**Acceptance check.** New rust-unit test asserting `ocx shell state` and the reconcile path derive the identical `Decision`, fingerprint and walk verdict from one fixture — i.e. one entry point, called twice. Red before: the two derivations are separate code. Existing `mod consent_evidence_tests` (`activate.rs:3363`) pins the divergence class that already bit once (`shell_state.rs:457-463` read the consent env vars as string literals where `activate.rs` used the constants). The whole existing suite passing unchanged is the second half of the check — a pure move must not move behaviour.

**Also in scope for P3.** The two global-consent code comments that live in this file (see §2).

### [#344](https://github.com/ocx-sh/ocx/issues/344) — CLOSE-ALREADY-DONE (P11)

All three options the issue itself lists have shipped on this branch:

- Option 3 (drop the `<host>/*` spelling) — [`7da783df`](https://github.com/ocx-sh/ocx/commit/7da783df); `[shell.consent] namespaces` now refuses both `ocx.sh/*` and bare `ocx.sh`, accepting only `<host>/<org>`.
- Option 2 (require materialisation) — [`0735f9f4`](https://github.com/ocx-sh/ocx/commit/0735f9f4); clause 2 moved off `lock_sources` onto `verified_sources` (`crates/ocx_lib/src/project/consent.rs:346`), which reads `PackageStore::recorded_origins()` and treats absent evidence as refusal (`consent.rs:382-388`). A lock naming a granted org no longer suffices.
- Option 1 (document precisely) — the `in-depth/shell-integration.md:41` "resolves inside that namespace" sentence is corrected on this branch; the RCE half was closed by gating the project-file `[env]` channel on clause 1 or clause 3.
- The regression the narrowing introduced (a `[shell.consent]` parse error propagating to the whole `Config` and discarding `[registries]`/`[mirrors]`/`[[trust.policy]]` fleet-wide) is fixed by [`690da2a6`](https://github.com/ocx-sh/ocx/commit/690da2a6); the loader now strips only `[shell.consent]` and records a `consent_strip_reason`.

**Applying "do not get paranoid".** What survives is not this issue's stated residual. The two remaining bounds are (a) the origin marker's weak write gate, tracked at its true strength in [#348](https://github.com/ocx-sh/ocx/issues/348), and (b) coordinate identity under an operator-configured `[mirrors]` entry or index indirection — which requires the operator to have configured the redirect, is unreachable from the project tier (`ProjectConfig` carries no routing keys, `ConfigTier` has no project variant, project-tier `[shell]` is stripped in `fold_project_tier`, and `is_reserved_ocx_key` blocks `OCX_MIRRORS`/`OCX_INDEX` from every project- and package-authored env surface), and whose instrument is `[[trust.policy]]` plus signature verification, already recorded as A-39. Neither is this issue.

**Close rationale text.**

> Closing: all three options this issue lists have shipped on [#339](https://github.com/ocx-sh/ocx/pull/339). Option 3 — the whole-registry `<host>/*` spelling is refused ([`7da783df`](https://github.com/ocx-sh/ocx/commit/7da783df)). Option 2 — clause 2 quantifies over `verified_sources`, the package store's own `refs/origins/` record, not over `ocx.lock` text ([`0735f9f4`](https://github.com/ocx-sh/ocx/commit/0735f9f4)); a lock naming a granted org no longer grants. Option 1 — the overstated `shell-integration.md` sentence is corrected. The residual this issue names has moved: the marker's write gate is weaker than "a registry served under this name", tracked at its true strength in [#348](https://github.com/ocx-sh/ocx/issues/348), and coordinate identity under an operator-configured mirror or index indirection is recorded in the ADR as A-39's residual, whose instrument is `[[trust.policy]]` plus signature verification. Neither belongs on this issue any more.

### [#345](https://github.com/ocx-sh/ocx/issues/345) — FIX (package P2)

**Root cause.** `crates/ocx_lib/src/shell/reconcile.rs` is 3584 lines (production ends 1315) holding three separable concepts, against `arch-principles.md`'s "one concept per file".

**Files.** `crates/ocx_lib/src/shell/reconcile.rs` → split into flat named files, no `mod.rs`:
- `crates/ocx_lib/src/shell/reconcile/ledger.rs` — `LedgerEntry` 69-98, `ScopeId` 106-111, `Verdict` 123-145, `Prior` 154-159, `ProjectScope` 178-193, `Scopes` 197-225, `Ledger` 233-426 (`decode` 281-298, `encode` 307-325).
- `crates/ocx_lib/src/shell/reconcile/plan.rs` — `Plan` 440-453, `plan` 484-521, `emittable` 868-882, `apply_set` 927-940, `retire_recorded_element` 1060-1090, `retire_recorded_constant` 1097-1114, `repair_owned_segments` 1125-1164, `is_owned` 1221-1224, `element_eq` 1246-1258, `element_norm` 1261-1273, `unquote` 1275-1280.
- `crates/ocx_lib/src/shell/reconcile/fingerprint.rs` — `fingerprint` 669-705, `current_fingerprint` 721-728, `watch_paths` 742-805, `fold` 809-815, `fold_optional` 818-826, `mtime_bytes` 830-845.

Test modules move with their concepts: `mod tests` 1318, `mod fingerprint_tests` 3113, `mod watch_path_tests` 3316, `mod summary_tests` 3403 — the file's own test module names already mark the seams.

**Note:** issue [#350](https://github.com/ocx-sh/ocx/issues/350) cites `is_owned` at line 1129. It is at **1221**, inside the planner cluster.

**Production LOC.** 0 net; ~1315 production lines and ~2269 test lines relocate.

**Acceptance check.** The full existing suite green, unchanged, with zero edits to any assertion. A pure split that needs a test edit is not a pure split. `crates/ocx_lib/src/shell.rs:8` is untouched.

### [#346](https://github.com/ocx-sh/ocx/issues/346) — CLOSE-ALREADY-DONE (P11)

**Verified on this branch.** `StateStore::host_capabilities_file()` exists at `crates/ocx_lib/src/file_structure/state_store.rs:209`, beside `referrers_capability_file` (`:198`) and `trust_root_file` (`:230`). `record_path` at `crates/ocx_lib/src/oci/host_capabilities.rs:1207-1211` now builds a `StateStore` and delegates rather than joining segments by hand. The test `the_record_lands_where_this_module_documents_it` is at `host_capabilities.rs:1911`. Commit [`5a3c0ff3`](https://github.com/ocx-sh/ocx/commit/5a3c0ff3) carries `Closes #346` and touches exactly those two files. The `command/version.rs` static-command bypass still works because the accessor takes a root rather than requiring a live `FileStructure`.

The issue already carries the closure comment, including the mutation evidence (repointing the accessor to `hostMUTANT` left all 38 existing tests green because they hand-build the path — the new test is what reds).

**Close rationale text.** Reuse the existing comment; close with `state_reason: completed` and a one-line pointer to [`5a3c0ff3`](https://github.com/ocx-sh/ocx/commit/5a3c0ff3).

### [#347](https://github.com/ocx-sh/ocx/issues/347) — FIX (package P4)

**Root cause.** Three arms guard on a marker that is a *different object* from the registration it protects, so a prompt framework that overwrites the registration wholesale leaves the marker behind and re-sourcing reads "already registered".

Confirmed per arm in `crates/ocx_lib/src/shell/hook.rs` (2454 lines, production ends 908; `registration()` dispatches at 219-238):

| Arm | Guard | Registration | Verdict |
|---|---|---|---|
| bash `:370-408` | `typeset -f __ocx_prompt_hook` `:391` | `PROMPT_COMMAND` `:395-403` (array-aware via `declare -p`) | **broken** |
| zsh `:410-431` | `typeset -f __ocx_prompt_hook` `:421` | `add-zsh-hook precmd` `:425` → `precmd_functions` | **broken** |
| PowerShell `:579-619` | `Test-Path variable:global:__ocxPrevPrompt` `:590` | `function global:prompt` `:591,598` | **broken** |
| fish `:516-534` | `functions -q __ocx_prompt_hook` `:521` | `function … --on-event fish_prompt` | **already correct** — the function *is* the handler |
| elvish `:860-874` | `elvish_already_registered` `:776-781`, structural read of the closure's rest parameter `ELVISH_HOOK_MARKER` `:751` | `$edit:before-readline` | **already correct** |
| ash / ksh / dash / nushell / batch | `registration()` → `None` `:238` | no per-prompt hook installed | n/a |

So this is **three arms, not "most"** — fish is immune for the same structural reason elvish is, and the five `None` arms have nothing to clobber. That materially shrinks the "per-arm redesign" the issue feared.

**The fix is one change, applied three times: make the guard's subject the registration, not the marker.**
- bash — test whether `PROMPT_COMMAND` actually contains the call, covering both the string form and the bash 5.1+ array form. The emitter already branches on `declare -p PROMPT_COMMAND` at `:395-403`, so the array/string duality is already handled in this file and the check reuses that branch.
- zsh — test membership in `precmd_functions` (`(($precmd_functions[(Ie)__ocx_prompt_hook]))`) rather than function existence. `precmd_functions` appears nowhere in `crates/` today; zsh relies entirely on `add-zsh-hook`'s own idempotency.
- PowerShell — test whether the current `$function:prompt` body is still our wrapper, and re-capture `$global:__ocxPrevPrompt` when it is not.

No live-inspection helper exists to reuse: grepping `PROMPT_COMMAND|precmd_functions|__ocxPrevPrompt|function:prompt` across `crates/` matches only `hook.rs`, and only inside emission. `HookStatus` (`crates/ocx_cli/src/api/data/shell_state.rs:112-121`) reports the *config* decision and has no channel into a live shell.

**Files.** `crates/ocx_lib/src/shell/hook.rs`; `test/tests/test_shell_reconcile.py`.

**Production LOC.** ~50 (emitted shell text across three arms).

**Acceptance check.** `test/tests/test_shell_reconcile.py::test_prompt_hook_coexists_with_a_third_party_prompt_framework` (`:2617-2669`) is already parametrised over `("starship", "oh-my-zsh", "powerlevel10k")` and drives `matrix.pty_session`. The existing fixture runs the framework preamble *before* the `eval "$(… self activate --hook …)"` line (`:2650-2662`); the new case reorders those two blocks — no new fixture primitive needed. Red before: the hook stops firing and a re-source does not repair it. `test/docker/shells.Dockerfile` already installs all three frameworks, so the rows run rather than skip.

### [#348](https://github.com/ocx-sh/ocx/issues/348) — DEFER (P11)

**Category: a persisted-format and OCI-resolve-path decision, not a defect of this branch's shell subsystem.** The issue itself says so and this branch already corrected every doc that overstated the gate (`package_store.rs:418-434` now states the bug verbatim and links the issue).

The sizing was done rather than assumed. The smallest honest fix is a manifest-provenance ledger on `ChainedIndex`, recorded at `chained_index.rs:746` — the one line that already knows a configured remote source answered — surfaced through a defaulted `IndexImpl::served_by_source` trait method so the 8 test mocks do not churn, and consumed in place of `pull.rs:334`'s `provided_metadata.is_none()`. **≈55 production LOC across four files** (`chained_index.rs`, `index/index_impl.rs`, `oci/index.rs`, `package_manager/tasks/pull.rs`); no `ResolvedChain` field and no return-type widening needed.

LOC is not the blocker. Two things are:

1. **The marker format is unversioned.** `package_store.rs:481` writes the bare `"<registry>/<repository>"` string as the whole record; the file name is its own content hash, so there is no `v` field to bump (contrast `ConsentStamp.v`, `consent.rs:46-49`). And because both store-hit early returns (`pull.rs:341`, `:378`) mean a warm, fully-installed package never reaches `record_origin` again, tightening the gate drops clause-2 grants **permanently**, not until the next pull. Choosing between a prefixed payload, a sibling `refs/origins-wire/` directory, or accepting the drop is a persisted-format decision.
2. **It retracts prose this branch just wrote.** Decision 4's "Named residual" paragraph, A-39, `consent.rs:288-341` and `package_store.rs:418-437` were all written on this branch to state the gate at its *true* (weak) strength. Closing #348 flips them back.

**Applying "do not get paranoid".** The attack chain needs four preconditions, one of which is not attacker-controlled: the victim must already have pulled attacker content from the same registry, a GC or interrupted pull must then have removed the package directory while the layer cache survived, the victim must have whitelisted a namespace, and only then does an attacker-authored lock borrow the name. The branch's clause 2 is strictly stronger than the claim-based spelling it replaced. This does not gate a squash.

**Deferral comment text.**

> Deferring past [#339](https://github.com/ocx-sh/ocx/pull/339). The fix was sized rather than waved off: the honest gate is a manifest-provenance signal recorded where `ChainedIndex` already knows a configured source answered, surfaced through a defaulted `IndexImpl` method and consumed in place of `pull.rs:334`'s `provided_metadata.is_none()` — about 55 production lines across four files. What blocks it is not size. The origin marker carries no version field (its payload *is* the record, and the file name is that payload's hash), and because a warm package never re-reaches `record_origin` past the two store-hit early returns, tightening the gate drops existing clause-2 grants permanently rather than until the next pull. Choosing between a prefixed payload, a sibling `refs/origins-wire/` directory, or accepting the drop is a decision about a persisted format on the OCI resolve path, which is exactly the design pass this issue asks for and not something a shell-subsystem PR should make in passing. The branch's disclosure is already honest at the code's true strength (`package_store.rs`, `consent.rs`, ADR Decision 4, A-39), so nothing here is overstated while this stays open.

### [#349](https://github.com/ocx-sh/ocx/issues/349) — CLOSE-ALREADY-DONE (P11)

**Verified on this branch.** `PARITY_ARMS` at `crates/ocx_lib/src/shell.rs:3025-3035` now carries `&["elvish", "-c"]` (`:3033`) and `&["nu", "-c"]` (`:3034`). `every_hook_shell_has_a_parity_arm` at `:3046` anchors coverage on the `Shell` enum rather than on the array it guards. `assert_every_present_interpreter_ran` replaces the old any-of rule and is used across the live legs, including the elvish (`:2496`) and nushell (`:2514`) arms. `__OCX_TESTING_REQUIRE_LIVE_SHELLS` is set to `nu,elvish` in `.github/workflows/verify-basic.yml:134`, and `test/taskfile.yml:153` derives its list from the zoo image via `{{.SHELLS_REQUIRE_LIVE}}`.

Commits [`d6d571c7`](https://github.com/ocx-sh/ocx/commit/d6d571c7), [`f8dddc62`](https://github.com/ocx-sh/ocx/commit/f8dddc62), [`7074e7f9`](https://github.com/ocx-sh/ocx/commit/7074e7f9), [`2b434638`](https://github.com/ocx-sh/ocx/commit/2b434638). The issue's own closure comment carries a seven-row red-state mutation table with the mutation proven landed by `grep -c` either side — which is precisely what the issue demanded before closing.

**Close rationale text.** Reuse the existing comment; close with `state_reason: completed`.

### [#350](https://github.com/ocx-sh/ocx/issues/350) — FIX (package P1)

**Root cause.** `is_owned` (`crates/ocx_lib/src/shell/reconcile.rs:1221-1224`) is `Path::starts_with` against prefixes handed in raw: `activate.rs:334` passes `[file_structure.root()]`, and `FileStructure::root()` (`crates/ocx_lib/src/file_structure.rs:135`) returns the uncanonicalised root from `default_ocx_root()` (`:183-190`), which never resolves symlinks. Under a symlinked `$OCX_HOME`, a `PATH` element spelled through the resolved path is foreign to the prefix, so the C-006 ledger-loss repair leaves ocx's own stale segments in place.

**Fix (the shape the issue recommends, and the cheap one).** Canonicalise the prefix once at reconcile start, then test each segment against **both** spellings — two `starts_with` calls per segment, no `realpath` per element. Canonicalising the segment side instead would cost one syscall per `PATH` element per prompt, against a C-044 budget measured at `exec_floor + 2 ms`.

**Files.** `crates/ocx_lib/src/shell/reconcile.rs` (`is_owned` 1221-1224; `plan` 484-521 or `repair_owned_segments` 1125-1164 as the canonicalise-once entry — the natural place, since `plan` already owns `owned_prefixes`); `crates/ocx_cli/src/command/self_group/activate.rs:334` only if canonicalisation is pushed to the caller. `shell_state.rs` never constructs an owned-prefix list and is untouched.

**Production LOC.** ~20.

**Acceptance check.** New rust-unit test in `reconcile.rs`: a `$OCX_HOME` behind a symlink, a `PATH` carrying the resolved spelling of an ocx-owned bin dir, and a lost ledger — the repair must retire the segment. Red today: the segment survives. Plus a C-044 measurement against the existing per-prompt gate, since the change adds a comparison per segment.

**Register.** New row `EC-LIST-011` (next free in the family; `EC-LIST-009` covers the adjacent component-boundary case and its recommendation text already claims a canonicalisation pass that never shipped). `reconcile.rs:469-476` already carries a comment naming this gap and linking the issue — it comes out with the fix.

### [#351](https://github.com/ocx-sh/ocx/issues/351) — FIX (package P5)

**Root cause.** `run_doc_script` (`test/src/doc_scripts.py:530-650`) always shells out via `subprocess.run(["bash", "-c", body], …)` at `:629`, regardless of the `# shell:` header — whose own doc comment (`:144-152`) states it is "not consulted by the verify-path executor". A non-interactive `bash -c` never reaches a prompt, so `__ocx_prompt_hook` never fires and all four casts honestly record a shell where the reconciler never ran, under prose (`shell-integration.md:14`) that promises the opposite.

**Fix is rung-2 laziness — the machinery already exists.** `shell_matrix.pty_session` (`test/src/shell_matrix.py:670-677`) drives a real pty, gates keystrokes on `line_editor_is_reading` rather than a wall-clock guess, and raises `TimeoutError` rather than returning a partial transcript. Import graph checked: `shell_matrix.py` imports stdlib only and nothing from `doc_scripts.py`; `doc_scripts.py` imports nothing from `shell_matrix`. **No cycle — `doc_scripts.py` can import and call it directly.** Do not write a second pty implementation.

Four items, all in scope:
1. `# pty: true` header in `doc_scripts.py`, spawning the declared `# shell:` interactively with a fixed `PS1`, delegating to `pty_session`.
2. Re-record the four casts under it so the apply, the per-prompt summary line and the revert are what the reader sees.
3. Fix the `project: dir: ""` artefact — `test/recordings/test_recordings.py:84-87` blanket-replaces `str(work_dir)` with `""` for privacy, which collides with `shell_state.rs:342`'s genuinely informative `project:` line. Replace the blanket substitution with a stable placeholder for this field.
4. Add `in-depth/shell-integration` to `WALKTHROUGH_PAGES` (`test/src/doc_binding.py:50-57`, currently six pages, this one absent) and pin the casts' key output lines with `# expect:`. **Confirmed: 65 doc scripts, 0 use `# expect:`** — so this page becomes the mechanism's first user, and the mechanism itself needs its red state demonstrated.

Also re-home `cd-into-project.cast` / `cd-out-of-project.cast`, currently embedded under `## Repairing a stuck shell`, whose prose is about `unset __OCX_ENV_STATE` and never mentions `cd`.

**Files.** `test/src/doc_scripts.py`, `test/src/doc_binding.py`, `test/recordings/test_recordings.py`, `test/doc_scripts/shell-integration__{adding-a-package,cd-into-project,cd-out-of-project,inert-to-consented}.sh`, `website/src/public/casts/in-depth/shell-integration/*.cast`, `website/src/docs/in-depth/shell-integration.md`. **`test/src/shell_matrix.py` is imported, not edited.**

**Production LOC.** ~60 (pty mode) + four re-recorded casts.

**Acceptance check.** The `doc_binding` gate on `in-depth/shell-integration` plus `# expect:` pins asserting a cast contains a `PATH` change and a `+JAVA_HOME ~PATH`-shaped summary line. Red before: every cast ends `activation: active: no`. **Demonstrate the `# expect:` mechanism red as well as green** — it has zero users today, so a green is otherwise indistinguishable from never having run.

### [#352](https://github.com/ocx-sh/ocx/issues/352) — FIX (packages P6 + P10)

Each of the four verified against the tree. **Two of the issue's four claims need correction:**

| Addendum | Verified | Resolution |
|---|---|---|
| **A-19** quote strip is Windows-only | Confirmed. `crates/ocx_lib/src/shell.rs:930` and `crates/ocx_lib/src/utility/path.rs:62` both gate on `cfg!(windows)`. `PATH_CASES` (`shell.rs:3129-3140`) pins it with two A-19-tagged rows. | Amend the addendum, with the reason the gate is correct (a leading `"` is part of a Unix directory name). Code unchanged. **P10.** |
| **A-41** record carries more than the text lists | Confirmed. `HostCapabilityRecord` (`crates/ocx_lib/src/oci/host_capabilities.rs:1085-1104`) is `{version, loaders, detected_at, ttl_seconds}`. | Amend the addendum to the shipped shape — it is a persisted format and the record must name it. **P10.** |
| **A-29** `record()` should be `pub(crate)` | Confirmed `pub` at `crates/ocx_lib/src/project/consent.rs:481`. **But `pub(crate)` is not viable** — `crates/ocx_cli/src/app/project_context.rs:375` calls it cross-crate. | Amend the addendum to `pub`, naming the caller. Code unchanged. **P10.** |
| **A-30** "one shared helper" is two | Confirmed two copies, **but the issue's path is wrong**: there is no `crates/ocx_lib/src/oci/registry.rs`. The second copy is `crates/ocx_lib/src/project/registry.rs:195-215` (`register_project_dir_best_effort`). They are **not** the same logic: sync `std::fs::canonicalize` vs async `tokio::fs::canonicalize`, and `consent.rs:423-439` re-canonicalises the parent through `dunce::canonicalize` while `registry.rs` omits that step entirely. | **This one is a latent defect, not prose.** The project ledger keys on `registry.rs`'s form and the consent stamp on `consent.rs`'s, so on Windows the two can disagree on a `\\?\`-prefixed path. Make `register_project_dir_best_effort` call `consent::canonical_project_dir` through `spawn_blocking`. **P6, ~10 prod LOC.** |

**Acceptance check for the A-30 dedup.** Rust unit test asserting the two call sites produce the same key for a path that exercises the `dunce` step. Red before: the two helpers diverge on that input. The other three are prose and are covered by the record-reconciliation pass.

### [#353](https://github.com/ocx-sh/ocx/issues/353) — FIX (packages P7 + P10)

**Root cause confirmed, with one correction to the issue.** The module-level skip is `pytest.mark.skipif(sys.platform == "win32", reason="…the Windows leg is WP-18.")` at `test/tests/test_shell_reconcile_edge_cases.py:53-58`, and `.github/workflows/shell-activation-deep.yml`'s `windows` job runs `test/manual/test-windows-activation.ps1` three times and **invokes pytest not at all**. The citation is circular exactly as described.

**The count is six, not seven.** `_UNCOVERED_ROWS` (`:3983-3995`) has nine rows; six cite WP-18 (`EC-HOOK-009`, `EC-PATH-013`, `EC-QUOTE-004`, `EC-QUOTE-010`, `EC-QUOTE-011`, `EC-SIZE-003`). The other three cite a **build**-matrix gap, not a platform gap: `EC-FP-005` (no runtime seam for `CARGO_PKG_VERSION`), `EC-VER-003` and `EC-VER-007` (two staged builds with different `Plan` schema versions). `EC-VER-003` has no "Windows half".

Per-row portability of the six:

| Row | Verdict | Reason |
|---|---|---|
| `EC-HOOK-009` | Windows-only | 5.1 prompt-wrap semantics and the absent `LocationChangedEventArgs` exist nowhere else. |
| `EC-PATH-013` | Windows-only | Needs a live `cmd.exe` applied N=20 times to prove segment count stabilises. |
| `EC-SIZE-003` | Windows-only | The 32767-char environment-block ceiling is a `CreateProcessW` limit. |
| `EC-QUOTE-004` | rust-unit half portable | The register's own cell says the rust-unit half is *unwritten*, not blocked: Batch quoting is pure `Shell` logic. |
| `EC-QUOTE-010` | rust-unit half portable | `Shell::Batch.export_path("PATH", "C:\\a%b\\bin")` `%`-handling is pure Rust string logic. |
| `EC-QUOTE-011` | rust-unit half portable | `escape_value`'s Batch `!`-non-escaping is a pure-Rust documented precondition. |

**Resolution, taking option (1)'s spirit without pretending a Windows pytest leg exists.** Option (1) as literally written is blocked by [#354](https://github.com/ocx-sh/ocx/issues/354) — the Windows deep job cannot complete today for an unrelated reason — so:

1. Write the three `EC-QUOTE-*` rust-unit halves in `crates/ocx_lib/src/shell.rs`. No runner needed; the register already flags them "unwritten".
2. Retier the three live Batch/cmd.exe halves and the three irreducibly-Windows rows to `manual-only` — a tier the register already defines and uses for three rows — with the honest reason and a pointer to `test/manual/test-windows-activation.ps1`.
3. Correct the module-level skip reason and the `_UNCOVERED_ROWS` comments so they stop naming a leg that would not run them.

**`_UNCOVERED_ROWS` shrinks 9 → 3**, leaving only the three build-matrix rows (`EC-FP-005`, `EC-VER-003`, `EC-VER-007`) that genuinely need two staged binaries. Note the honest caveat: writing the rust-unit halves **alone** shrinks it by **zero** — a row stays uncovered while any tier it names is uncovered, so the retier in step 2 is what does the work, and step 1 is what makes the retier honest rather than a bookkeeping trick.

**Files.** `crates/ocx_lib/src/shell.rs` and `test/tests/test_shell_reconcile_edge_cases.py` (**P7**); `.claude/artifacts/analysis_shell_env_edge_cases.md` (**P10**).

**Production LOC.** 0; ~120 test LOC.

**Acceptance check.** Each new Batch rust-unit test shown red by a one-byte mutation of the emitter, with the mutation proven landed by `grep -c` either side. The register's `test_traceability_every_register_row_is_cited_by_a_real_test` gate must stay green across the retier.

### [#354](https://github.com/ocx-sh/ocx/issues/354) — DEFER (P11)

**The claim was verified, not assumed.**

- `gh run list --workflow=shell-activation-deep.yml`: run [32999798797](https://github.com/ocx-sh/ocx/actions/runs/32999798797), `headBranch=main`, `headSha=7285d639`, `workflow_dispatch`, **failure**. The failing step is the one the issue names, and its log carries the exact error: `failed to find package: ocx.sh/pwsh — chained index source walk failed: 'ocx.sh/pwsh:latest' is not in the index at https://index.ocx.sh…`.
- Independently against the live index (URL shape from `crates/ocx_lib/src/oci/index/ocx_index.rs:1029`, `{base}/p/{repository}.json`, with a non-`Python-urllib` UA): `ocx.sh/pwsh` → **404**, `ocx.sh/nushell` → **404**, `ocx.sh/elvish` → **404**. Control: `ocx.sh/go-task` → 404 (flat names generically unserved), `astral-sh/uv` → **200** (namespaced names serve).

**Verdict: reproduces on `main` with no involvement of `feat/shell-env-overhaul`.** Cause is [`7285d639`](https://github.com/ocx-sh/ocx/commit/7285d639) making a configured index authoritative for its whole registry; the three flat fleet names predate the namespaced layout and were never announced.

**Category: cross-repo/infra work this repo cannot land.** The real fix is `ocx package announce` against index.ocx.sh for the flat fleet names — fleet work, and per the standing record gated behind renaming the flat mirrors.

**An in-repo escape hatch exists and is deliberately not taken.** `RegistryConfig.index` is `Option<String>` (`crates/ocx_lib/src/config/registry.rs:31`) with field presence as the kind marker, so a workflow-local config carrying `[registries."ocx.sh"]` with no `index =` line would route those names through plain OCI and green the workflow. That masks the refusal [`7285d639`](https://github.com/ocx-sh/ocx/commit/7285d639) deliberately introduced and stops the gate exercising the real dogfood path. Surfaced for the owner as an option, not proposed.

**Deferral comment text.**

> Deferring past [#339](https://github.com/ocx-sh/ocx/pull/339): verified pre-existing and cross-repo. Run [32999798797](https://github.com/ocx-sh/ocx/actions/runs/32999798797) on `main` at [`7285d639`](https://github.com/ocx-sh/ocx/commit/7285d639) fails at the same step with the same error, with no feature branch involved, and the index independently 404s `ocx.sh/pwsh`, `ocx.sh/nushell` and `ocx.sh/elvish` while a namespaced name serves — so the cause is the flat fleet names, not any diff here. The fix is `ocx package announce` for those names against index.ocx.sh, which is fleet work this repository cannot land. There is an in-repo escape hatch — a workflow-local `[registries."ocx.sh"]` with no `index =` key routes them through plain OCI — but taking it would mask exactly the refusal [`7285d639`](https://github.com/ocx-sh/ocx/commit/7285d639) introduced and would stop the gate exercising the dogfood install path it exists to test, so it is recorded here as an option rather than applied. `Shell Activation (Deep)` stays unusable as a release gate until the announce lands.

### [#355](https://github.com/ocx-sh/ocx/issues/355) — FIX (package P8), with one conditional

**The claim was verified, and the diagnosis needs amending.** `gh api repos/ocx-sh/index/contents/bot/tests/golden/serializer` 404s. But the goldens did not vanish — the whole `bot/` tree was extracted to a new repository. `ocx-sh/index#733` ("consume ocx-indexbot from PyPI instead of the in-tree bot", merged 2026-08-24) removed `bot/` entirely; `repos/ocx-sh/index/git/trees/main?recursive=1` has no `bot/` path at all. The goldens live at **`ocx-sh/indexbot`, `tests/golden/serializer/root/*.json`** — same relative layout, minus the `bot/` prefix — verified present with the same four file names.

So it is fixable inside this repository, but **not by repointing one constant**: `test/scripts/sync_index_conformance.sh:27` sets `src_rel="bot/tests/golden"` and every fetch in the script targets `repos/ocx-sh/index/…` literally. Both the repository and the relative root change.

**The conditional.** Repointing surfaces genuine content drift that the 404 has been hiding: the vendored `crates/ocx_lib/tests/fixtures/index_wire/root/minimal.json` (pinned at `SOURCE_COMMIT=6ffe7599`, 2026-08-09) still carries the old `login`/`id`/`github`/`github_id` owner shape, while upstream migrated to the forge-neutral spelling in `ocx-sh/index#740` (merged 2026-08-25).

- If the drift is fixture-only, refresh the vendored copies in this package.
- If the forge-neutral owner shape requires an `ocx_lib` index-wire parser or serializer change, **that half splits out as a new issue** — it is a wire-format change with its own review, not a script repoint, and it must not ride a shell-env squash.

**Files.** `test/scripts/sync_index_conformance.sh` (repo name + `src_rel`), `crates/ocx_lib/tests/fixtures/index_wire/**`. Task entry is `test/taskfile.yml:400`, wired at `.github/workflows/verify-deep.yml:50`.

**Production LOC.** ~5, plus fixture refresh.

**Acceptance check.** `task test:index-conformance-drift` itself. Red today with `gh: Not Found (HTTP 404)` (run [33012635452](https://github.com/ocx-sh/ocx/actions/runs/33012635452) on `main`); green after. That gate is currently providing zero coverage — the vendored fixtures could drift arbitrarily and it would keep reporting the same 404 — so this restores a real seven-day drift window rather than a cosmetic pass.

### [#170](https://github.com/ocx-sh/ocx/issues/170) — CLOSE-ALREADY-DONE (P11)

The PR body already carries `Closes #170` and the feature ships: per-prompt reconciliation with enter/leave/project-to-project restore, ledger-based lifecycle state in `__OCX_ENV_STATE`, idempotent PATH handling, offline digest-pinned resolution, and no direnv dependency.

**One residual, and it must not become untracked on close.** `plan_shell_env_overhaul.md`'s wave status lists **WP-12b `pending`** — the nushell JSON-`Plan` apply body — so nushell is global-toolchain-only today (`in-depth/shell-integration.md:35`: "global toolchain only — no project reconcile, no revert, no consent gate, today"). That is one of the five shells #170's acceptance criteria name. The acceptance suite already skips every nushell project-scope row through a probe that reads the shipped `env.nu` and reports the `reconcile` count it observed, so the skips vanish by themselves the day WP-12b lands.

**Action:** file one successor issue, "nushell project-scope reconcile (WP-12b)", and reference it in #170's close comment. Closing #170 without it turns a tracked `pending` into an untracked gap.

### [#265](https://github.com/ocx-sh/ocx/issues/265) — DEFER (P11)

**Category: work the ADR explicitly excludes**, which is the cleanest of the three deferral grounds and is directly reinforced by the owner rule "stick to the ADR".

`adr_shell_env_overhaul.md:8` lists it among the driving issues as "**unblocked, still deferred**", and the changelog entry around `:555` records `priors: Unset` being deliberately separated from a desired-unset so the two can never be confusable. Decision 3's ledger vocabulary carries `Prior::Unset` as ledger-internal — "the variable did not exist before ocx set it" — precisely so that a future `unset = [...]` config directive has an unambiguous slot to occupy. The design work to unblock it is done; the config surface is out of scope by decision.

**Deferral comment text.**

> Staying deferred past [#339](https://github.com/ocx-sh/ocx/pull/339), by ADR decision rather than by omission. [`adr_shell_env_overhaul.md`](https://github.com/ocx-sh/ocx/blob/main/.claude/artifacts/adr_shell_env_overhaul.md) lists this issue among its drivers as "unblocked, still deferred", and Decision 3 deliberately separates the ledger-internal `Prior::Unset` (meaning "the variable did not exist before ocx set it") from a *desired* unset, so the two can never be confused — which is exactly the slot an `unset = [...]` directive will occupy. The blocking design work is therefore done; what remains is the project `[env]` config surface, which this ADR excludes on purpose. Reopen against the config-schema work, not against the reconciler.

---

## 2. The global-toolchain consent decision

**Owner decision, verbatim intent:** the ocx home (global) toolchain is **always consented** — it is controlled by the user by definition — and this must not change.

**Does the code behave that way? Yes, verified.**

`compose()` (`crates/ocx_cli/src/command/self_group/activate.rs:385-483`) resolves the global entries at `:386-389` and stores them into `outcome.global` **before** the project walk, before the direnv/mise yield check, and before the lock is read. `consent::evaluate_with_stamp` runs at `:449-455` against `project.dir` only; an `Inert` decision at `:457-467` zeroes `outcome.project` and returns with `outcome.global` untouched. `desired_entries()` (`:761-765`) then unions global first. Nothing downstream can retract it.

The consent module is written exclusively in project terms: `consent.rs:4-6` scopes itself to "a project's environment"; `evaluate` (`:547-556`) and `evaluate_with_stamp` (`:564-570`) take a `project_dir` and no tier selector; `Grant::{Stamp, Namespace, Path}` (`:106-118`) and `Decision::{Activate, Inert}` (`:65-74`) have no global variant. Repo-wide, `consent::evaluate*` is called from exactly two files — `activate.rs` and `shell_state.rs` — never from anything touching the global tier. **Global is exempt by omission, not by an explicit exemption branch, and nothing in Rust says why.**

### Surfaces that must carry the decision

Each must make the one-sentence claim named. Prose is the implementer's; the claim is not.

| Surface | Present today? | Claim it must make | Package |
|---|---|---|---|
| `.claude/artifacts/adr_shell_env_overhaul.md` — Decision 4 (`:213`), OQ-1 (`:271`, `:532`), Decision 2 premise (`:130`) | States "always **trusted**" explicitly and repeatedly | Restate as the owner's ratified decision in the owner's word — *always **consented*** — and mark it not-to-change: the global toolchain is always consented because `$OCX_HOME` is under the user's own control by definition, so consent is project-scope only and no clause may ever be added for the global scope. | **P10** |
| `.claude/artifacts/adr_shell_env_addenda.md` — mentioned only in passing at `:991` | Passing mention inside another addendum's text | A dedicated addendum entry (next free `A-` number) recording the decision as an owner-ratified invariant that binds future changes, since the addendum is what wins on semantics. | **P10** |
| `website/src/docs/reference/environment.md` — **absent** | The `OCX_GLOBAL` section (`:233-249`) and the `OCX_CONSENT_PATHS`/`OCX_CONSENT_NAMESPACES` sections (`:197-225`) sit adjacent and never cross-reference | The consent variables govern project activation only; the global toolchain is always consented and no `OCX_CONSENT_*` value can grant or withhold it. **This is the reference page a user reads while configuring consent — the highest-value gap.** | **P9** |
| `website/src/docs/user-guide.md:511` | Already states it ("always trusted, since `$OCX_HOME/ocx.toml` is your own file") | No change. | — |
| `website/src/docs/in-depth/shell-integration.md:14` | Already states it ("needs no consent at all") | No change. | — |
| `crates/ocx_cli/src/command/self_group/activate.rs:386-389` — the unconditional global resolve, **no comment at all** | Absent | Why the global half is composed before, and independently of, the consent decision. | **P3** — and these lines **did not move**: `resolve_global_pinned_env` stays in `ocx_cli` (`activate.rs:333-341`), because the login exporter owns it and `--env` overrides and group selection are argv concerns a prompt never has. The resolved entries cross into `ocx_lib` as `SessionInput::global` (addendum A-44). |
| `crates/ocx_cli/src/command/self_group/activate.rs:1354-1362` — `format_global_env_eval`, which states "guarded **only** by an existence probe" with no justification | Names the sole guard, justifies nothing | Why an existence probe is the only gate the global tier needs. | **P3** |
| `crates/ocx_lib/src/project/consent.rs:4-6` module doc and `evaluate`'s doc `:494-546` | Scopes itself to projects; never says the global tier is deliberately out of scope | Consent is project-scope by definition; the global tier is always consented and is deliberately absent from every clause. | **P6** |

### One thing to verify before writing the prose

`crates/ocx_cli/src/command/run.rs:165` calls `load_project_with_lock_consenting`, which **writes** a stamp via `record_activation_consent` without evaluating one, and neither `record_activation_consent` nor `canonical_project_dir` special-cases `$OCX_HOME`. If `ocx run --global` / `ocx pull --global` writes a stamp under `state/projects/<key-for-$OCX_HOME>/`, that contradicts `adr_shell_env_overhaul.md:130`'s stated invariant that "nothing ever writes that directory" — the same premise Decision 2 uses to delete the global-tier sweep carve-out.

**Assigned to P6**, verify-first: reproduce with `ocx --global run` against an empty state root and check for a stamp directory. If it reproduces, the smallest honest fix is a guard in `record_activation_consent` skipping the ocx root, plus the doc line; if it does not, correct nothing and record the negative result so the next reader does not re-ask. Do **not** write the ADR prose in P10 until this returns an answer.

---

## 3. Work packages

Hard constraint: **one worktree is one concurrent writer.** No two packages that can run in parallel share a file. The `.claude/artifacts/` design record is owned by exactly one package (P10) which runs last and absorbs every other package's record delta as text — this is what keeps four packages from fighting over `analysis_shell_env_edge_cases.md`. Plan §0's "fix the artifact in the same commit" is satisfied by the squash.

| ID | Closes | Model | Files (exclusive) | Depends on |
|---|---|---|---|---|
| **P1** | [#350](https://github.com/ocx-sh/ocx/issues/350) | **opus** | `crates/ocx_lib/src/shell/reconcile.rs`, `crates/ocx_cli/src/command/self_group/activate.rs` | — |
| **P2** | [#345](https://github.com/ocx-sh/ocx/issues/345) | sonnet | `crates/ocx_lib/src/shell/reconcile.rs` → + `reconcile/{ledger,plan,fingerprint}.rs` | **P1** |
| **P3** | [#343](https://github.com/ocx-sh/ocx/issues/343) + 2 global-consent comments | **opus** | `crates/ocx_cli/src/command/self_group/activate.rs`, `crates/ocx_cli/src/command/shell_state.rs`, **`crates/ocx_cli/src/api/data/shell_state.rs`**, `crates/ocx_lib/src/shell/reconcile.rs`, + `crates/ocx_lib/src/activation.rs` (planned as `reconcile/session.rs`) | **P2** |
| **P4** | [#347](https://github.com/ocx-sh/ocx/issues/347) | **opus** | `crates/ocx_lib/src/shell/hook.rs`, `test/tests/test_shell_reconcile.py` | — |
| **P5** | [#351](https://github.com/ocx-sh/ocx/issues/351) | **opus** | `test/src/doc_scripts.py`, `test/src/doc_binding.py`, `test/recordings/test_recordings.py`, `test/doc_scripts/shell-integration__*.sh`, `website/src/public/casts/in-depth/shell-integration/*.cast`, `website/src/docs/in-depth/shell-integration.md` | — |
| **P6** | [#352](https://github.com/ocx-sh/ocx/issues/352) A-30 + consent module doc + the `run --global` stamp check | **opus** | `crates/ocx_lib/src/project/consent.rs`, `crates/ocx_lib/src/project/registry.rs`, `crates/ocx_cli/src/app/project_context.rs`, `crates/ocx_cli/src/command/run.rs` | — |
| **P7** | [#353](https://github.com/ocx-sh/ocx/issues/353) code half | sonnet | `crates/ocx_lib/src/shell.rs`, `test/tests/test_shell_reconcile_edge_cases.py` | — |
| ~~P8~~ | ~~[#355](https://github.com/ocx-sh/ocx/issues/355)~~ | — | **RETIRED** — landed as [`7d2c23ae`](https://github.com/ocx-sh/ocx/commit/7d2c23ae) | — |
| **P9** | global-consent user docs | sonnet | `website/src/docs/reference/environment.md` | — |
| **P10** | record reconciliation ([#352](https://github.com/ocx-sh/ocx/issues/352) prose half, [#353](https://github.com/ocx-sh/ocx/issues/353) register half, global-consent record, all new EC rows) | **opus** | `.claude/artifacts/adr_shell_env_overhaul.md`, `.claude/artifacts/adr_shell_env_addenda.md`, `.claude/artifacts/analysis_shell_env_edge_cases.md`, `.claude/artifacts/plan_shell_env_overhaul.md` | **all** |
| **P11** | issue closeout ([#170](https://github.com/ocx-sh/ocx/issues/170) [#265](https://github.com/ocx-sh/ocx/issues/265) [#344](https://github.com/ocx-sh/ocx/issues/344) [#346](https://github.com/ocx-sh/ocx/issues/346) [#348](https://github.com/ocx-sh/ocx/issues/348) [#349](https://github.com/ocx-sh/ocx/issues/349) [#354](https://github.com/ocx-sh/ocx/issues/354) [#355](https://github.com/ocx-sh/ocx/issues/355)) | sonnet | none — `gh` only | **P10** |

### Model rationale

`opus` for P1 (ownership semantics on a security-relevant repair path, with a perf gate), P3 (crate-boundary move of persisted-format construction and consent sequencing), P4 (per-shell emitted wire text, bash string/array duality), P5 (pty session design and a gate mechanism with zero prior users), P6 (consent/identity keying plus an ADR-invariant contradiction to adjudicate), P10 (design-record decisions). `sonnet` for P2 (pure mechanical split, shape fully decided), P7, P8, P9, P11 (mechanical against decided shapes).

### Parallelism

**Run concurrently, wave 1:** P1, P4, P5, P6, P7, P9 — six worktrees, no shared file. (P8 retired.)

**P3 file-list correction at `7d2c23ae`.** `crates/ocx_cli/src/api/data/shell_state.rs` (1944 lines) — redesigned by [`dd5a8188`](https://github.com/ocx-sh/ocx/commit/dd5a8188), [`06c91815`](https://github.com/ocx-sh/ocx/commit/06c91815), [`705d4afb`](https://github.com/ocx-sh/ocx/commit/705d4afb) — imports the reconciler's carrier types directly: `reconcile::{CARRIER_KEY, Ledger, LedgerEntry, MAX_CARRIER_BYTES, Prior, …}` at `:50`, `reconcile::{ProjectScope, Scopes}` at `:780`, `consent::Reason` at `:48`. It renders exactly the types [#345](https://github.com/ocx-sh/ocx/issues/345) relocates and [#343](https://github.com/ocx-sh/ocx/issues/343) re-homes, so **P3 must own it**. Neither `command/shell_state.rs` nor `api/data/shell_state.rs` has a `#[cfg(test)]` module — both are pure production, so a mis-scoped move here has no local test to catch it.

**Cannot be parallel — the `reconcile.rs` lane, strictly serial: P1 → P2 → P3.**

Three packages write `crates/ocx_lib/src/shell/reconcile.rs`, and P1 and P3 also both write `activate.rs`. The order is not arbitrary:

1. **P1 first.** [#350](https://github.com/ocx-sh/ocx/issues/350) is ~20 lines in `is_owned`/`plan` plus its test. Landing it against the current single file is the cheapest possible placement — fix and test in one location, no rebase.
2. **P2 second.** The split is behaviour-preserving, so it carries P1's lines into `plan.rs` mechanically. Splitting first would force P1 to guess which of three new files its fix belongs in, and would rebase a 20-line semantic change across a 3584-line move.
3. **P3 last.** The moved sequencing then lands against a `reconcile/` directory that already exists beside `ledger.rs`/`plan.rs`/`fingerprint.rs` — it landed at the crate root as `activation.rs` rather than inside that directory (Open question 1), but the ordering argument is unchanged: the split had to define the pure pieces before anything could be moved out from beside them. P3 also absorbs `activate.rs:334` — the owned-prefix call site P1 touches — into the moved code, so P1 must precede it. Running P3 first would mean splitting a file that had just grown by 600 lines.

**P10 last, alone.** It is the single writer of the design record and needs every other package's record delta as input. **P11 after P10** — the close comments cite the reconciled record.

**Handover contract for every code package:** deliver the record delta (new EC rows with full column values, addendum amendments, ADR line edits) as *text in the completion report*, never as an edit to `.claude/artifacts/`. A package that edits the record breaks P10.

---

## 4. Regression-test posture

| Issue | Tier | EC row | Red state to demonstrate |
|---|---|---|---|
| [#343](https://github.com/ocx-sh/ocx/issues/343) | rust unit (`activation.rs`) | none needed — structural, plus the `shell_does_not_import_project` directory-walk guard that holds `shell/` pure | Two derivations of the same answer exist; after, one entry point called twice. Whole existing suite green **unchanged** is the other half. |
| [#345](https://github.com/ocx-sh/ocx/issues/345) | rust unit (existing, relocated) | coverage-column repoint for every row citing `shell/reconcile.rs::<test>` — **P10** | None. A split needing a test edit is not a split. `test_traceability_every_register_row_is_cited_by_a_real_test` is the gate. |
| [#347](https://github.com/ocx-sh/ocx/issues/347) | pytest acceptance (`test_shell_reconcile.py`, pty tier 3) | **new** `EC-HOOK-017` | Framework loads *after* activation; hook stops firing and a re-source does not repair it. Existing `EC-HOOK-001/002/004/007/008/010/011` cover normal load order only. |
| [#350](https://github.com/ocx-sh/ocx/issues/350) | rust unit (`reconcile.rs`) + C-044 measurement | **new** `EC-LIST-011` | Symlinked `$OCX_HOME`, resolved-spelling PATH element, lost ledger — the segment survives. `EC-LIST-009` covers the adjacent lookalike-prefix case; its recommendation text claims a canonicalisation pass that never shipped and must be corrected. |
| [#351](https://github.com/ocx-sh/ocx/issues/351) | pytest (`doc_binding` gate + `# expect:` pins) | none — no EC row covers doc casts, and none should | Every cast ends `activation: active: no`. **Also demonstrate the `# expect:` mechanism itself red** — 0 of 65 scripts use it today, so its green is otherwise indistinguishable from never having run. |
| [#352](https://github.com/ocx-sh/ocx/issues/352) | rust unit (A-30 dedup only) | none — A-19 is already pinned by `PATH_CASES` (`shell.rs:3129-3140`) | Two canonicalisation helpers disagree on a path exercising the `dunce` step. A-19/A-29/A-41 are prose, no test. |
| [#353](https://github.com/ocx-sh/ocx/issues/353) | rust unit ×3 (Batch quoting) + register retier | six existing rows retiered; **no new rows** | One-byte emitter mutation per new test, mutation proven landed by `grep -c` either side. |
| [#355](https://github.com/ocx-sh/ocx/issues/355) | shell gate (`task test:index-conformance-drift`) | none | `gh: Not Found (HTTP 404)` today; green after. |
| global consent | none — prose and comments only; behaviour already conforms | none | n/a. The `run --global` stamp question in §2 is the one item that may produce a test. |

### `_UNCOVERED_ROWS`

Nine rows today (`test/tests/test_shell_reconcile_edge_cases.py:3983-3995`). [#353](https://github.com/ocx-sh/ocx/issues/353)'s resolution shrinks it **to three** — `EC-FP-005`, `EC-VER-003`, `EC-VER-007`, all genuinely needing two staged binary builds rather than a platform.

The reduction comes from the retier in step 2, not from step 1: writing the three Batch rust-unit halves alone shrinks the set by **zero**, because a row stays uncovered while any tier it names is uncovered. Step 1 is what makes step 2 honest rather than bookkeeping — the three `EC-QUOTE-*` rows get real coverage for the half that never needed Windows, and only the live `cmd.exe` half moves to `manual-only` alongside the three irreducibly-Windows rows.

Two rows also get a corrected citation regardless: `EC-VER-003`'s `_UNCOVERED_ROWS` comment names a Windows half it does not have, and the module-level skip reason names a leg that would not run these rows.

---

## 5. Open questions

Four things a follow-up agent should settle decisively before or during dispatch.

1. **Can `reconcile::session` stay `Context`-free?** `project_entries` (`activate.rs:693-758`) is `async` and reaches the package manager. If the moved orchestration cannot take config and project inputs as plain parameters, P3 either drags `Context` into `ocx_lib` — breaking Decision 5 option C — or leaves a seam in `ocx_cli` that defeats the issue's purpose. Settle by drafting the `session` input struct signature **before** P2 starts, since it determines whether P3 is viable at all.

   **Answered — yes, and it cost a module move.** Every input arrives as a plain field on `SessionInput`, so nothing in the moved code can reach for ambient CLI state and no `Context::try_init` enters `ocx_lib` (Decision 5 option C holds). The unplanned half is *where* it landed: not `shell/reconcile/session.rs` but the crate root, `crates/ocx_lib/src/activation.rs`. Under `shell/` it would have closed a `shell` ⇄ `project` cycle that does not compile across a crate boundary, blocking [#313](https://github.com/ocx-sh/ocx/issues/313) / [#324](https://github.com/ocx-sh/ocx/issues/324). `shell/` is now pure — carrier format, planner, fingerprint — and `shell_does_not_import_project` (a directory walk over `shell/`, in `activation.rs`, with a non-vacuity twin asserting the scanner still recognises the import it hunts) keeps it that way. Recorded as addendum **A-45**.
2. **Does `ocx run --global` write a consent stamp under `$OCX_HOME`?** If yes, `adr_shell_env_overhaul.md:130`'s "nothing ever writes that directory" is false, and it is the premise Decision 2 uses to delete the global-tier sweep carve-out. P6 answers this; P10 must not write the global-consent prose until it has.
3. **Does repointing [#355](https://github.com/ocx-sh/ocx/issues/355) at `ocx-sh/indexbot` surface drift that needs an `ocx_lib` change?** The forge-neutral `login`/`id` owner migration (`ocx-sh/index#740`) is already in the upstream goldens and not in the vendored copies. If the parser needs to change, that half must leave this PR as its own wire-format issue.
4. **Is retiering three rows to `manual-only` acceptable against the ADR's Validation §Windows contract?** It is the honest answer given the Windows deep job cannot complete for [#354](https://github.com/ocx-sh/ocx/issues/354)'s unrelated reason, but it trades a claimed-future automated leg for a documented manual one, and that is an owner call rather than a test-scaffolding call.
