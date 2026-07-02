# ADR: Patch-Aware Env Resolution Uniformity — `PatchScope` Type-Forced Construction

<!--
Architecture Decision Record — MADR format.
Owner: Architect (/architect). Handoff: Builder (/builder), QA (/qa-engineer).
Hardens the milestone design in adr_infrastructure_patches.md (C4/C5 site-tier + opt-out).
-->

## Metadata

**Status:** Proposed
**Date:** 2026-07-01
**Deciders:** @michael-herwig, architect
**GitHub:** PR #172 maintainer review comments C4/C14 (+ launcher-semantic fork); milestone #111 (Corporate infrastructure v1)
**Relates to:** `adr_infrastructure_patches.md` (the milestone ADR — this hardens its C4 site-tier + per-package `no-patches` opt-out); `adr_global_toolchain_tier.md` (project vs global resolution sites)
**Tech Strategy Alignment:**
- [x] Rust 2024, thiserror in lib / anyhow in CLI, no new runtime deps. Internal-API change only (ocx_lib consumed by ocx_cli + ocx-mirror; no stable public surface, no persisted format).
**Domain Tags:** package-manager, cli, infrastructure, correctness

---

## Context

The compose-time site overlay (Option B of `adr_infrastructure_patches.md`) resolves companion patches at `resolve_env` on the `PackageManager`. Two axes feed each `resolve_env` call:

1. **`self_view: bool`** — interface vs private surface (pre-existing, orthogonal).
2. **The per-package `no-patches` opt-out set** — canonical `registry/repository` strings for `[package."<id>"] no-patches = true` entries in a project `ocx.toml`. A base in this set gets **no** companion overlay *unless* the tier is system-required (C7 enforcement wins).

The opt-out set is a **project-toolchain** concern. It lives on `ProjectConfig::no_patches_repositories()` (`crates/ocx_lib/src/project/config.rs:227`). The `PackageManager` carries only `patches: Option<ResolvedPatchConfig>` (`package_manager.rs`), whose `ResolvedPatchConfig` (`config/patch.rs:79`) has tier-level `registry`/`path_template`/`required`/`system_required` and **no opt-out field**. So the opt-out set is **structurally un-derivable from manager state** — it must be threaded in by the command layer that holds a `ProjectConfig`.

Today two entry points exist:

- `resolve_env(packages, self_view)` — a convenience wrapper (`resolve.rs:341`) that **silently** passes `&BTreeSet::new()` for the opt-out set.
- `resolve_env_with_patch_boundary(packages, self_view, no_patches)` — the real entry (`resolve.rs:365`), consulting `no_patches` inside `build_site_patch_set` (`resolve.rs:417`) and returning the `--show-patches` boundary index.

The silent-default wrapper is the defect vector. It produces **two live bugs, not theory**:

- **BUG 1 — direnv (concrete, HIGH).** `command/direnv_export.rs:86` calls plain `resolve_env(&applied.infos, false)`. `ProjectState` (`project/hook.rs:30`) carries `pub config: ProjectConfig` and is in scope at line 47 (`project.config`). A project's `no-patches = true` is **silently ignored** in the direnv-composed env — the opt-out never reaches the resolver.
- **BUG 2 — launcher (structural, HIGH).** `command/launcher/exec.rs:65` calls plain `resolve_env(&[info], true)` (self_view hardcoded). This is the wire-ABI re-entry (`ocx launcher exec '<pkg-root>' -- argv0 ...`) hit on **every direct tool invocation**. It resolves from the package root + forwarded `OCX_*` config only and has **no `ProjectConfig` by construction**. The empty opt-out here is not wrong per se — but it is **invisible**: nothing at the call site records *why* there is no opt-out, so a reviewer cannot distinguish "correctly no project here" from "forgot to thread the opt-out" (exactly BUG 1's failure mode).

Correct-by-design callers already pass the real set: `ocx run` (`run.rs:215`), `ocx env` project (`toolchain_env.rs:213`), `ocx --global env` (`toolchain_env.rs:368`, fresh `ProjectConfig` from `$OCX_HOME/ocx.toml`), and `ocx package env` (`env.rs:126`, empty set, OCI-tier no `ocx.toml`). OCI-tier / isolated callers structurally have no project: `ocx package exec` (`exec.rs:65`), `ocx package test` (`package_test.rs:210`), the isolated patch-test scratch manager (`patch_test.rs:146`), and the self-update version probe (`update_check.rs:482`).

The root cause is a **stringly-implicit convention** (`bool` + an easily-defaulted `BTreeSet`) where an **explicit, compile-visible choice** belongs. The opt-out axis has exactly two honest states — *"a project is in scope, here is its opt-out set"* and *"no project toolchain is in scope"* — and the current API lets a caller land in the wrong one by omission.

---

## Decision Drivers

| # | Driver | Weight |
|---|--------|:---:|
| D1 | **Fix both live bugs** (direnv opt-out honored; launcher labelled honestly) | 30% |
| D2 | **Prevent recurrence** — a new `resolve_env` caller must make a compile-forced, reviewable choice; no silent default | 25% |
| D3 | **Honest at the no-project boundary** — "no project here" is a *named* state, not an ambiguous `None`/empty | 15% |
| D4 | **Layer purity** — the `PackageManager` stays project-agnostic; project state is threaded in, not baked into the facade | 15% |
| D5 | **Migration simplicity** — churn across ~13 production + ~30 test call sites is mechanical, not semantic | 15% |

Constraint: pre-1.0, no back-compat shim. `ocx_lib` has no stable public API — this is an internal refactor, reversible in principle, but it sets the pattern for every future env-resolving command (a "one-way medium" internal-API decision).

---

## Considered Options

### Option A — `PatchScope` enum, required argument (RECOMMENDED)

Introduce a two-variant enum and make it a **required** parameter of the resolver; delete the silent-default wrapper.

```rust
/// Where the caller sits relative to a project toolchain, and thus which
/// per-package `no-patches` opt-outs apply to the companion overlay.
pub enum PatchScope {
    /// A project (or global) `ocx.toml` is in scope. Carries its `no-patches`
    /// opt-out repositories (canonical `registry/repository`, tag/digest excluded).
    Project(std::collections::BTreeSet<String>),
    /// No project toolchain is in scope — OCI-tier commands, the launcher
    /// re-entry, isolated scratch managers, the self-update version probe.
    /// No project opt-out applies; a system-required *tier* still enforces (C7).
    NoProjectContext,
}
```

- The current 2-arg `resolve_env(packages, self_view)` (the silent-default wrapper) is **deleted**.
- `resolve_env(&self, packages, self_view, scope: PatchScope) -> Result<Vec<Entry>>` becomes the common path; it delegates to the boundary method and drops the index.
- `resolve_env_with_patch_boundary(&self, packages, self_view, scope: PatchScope) -> Result<(Vec<Entry>, usize)>` keeps the `--show-patches` boundary; its `no_patches: &BTreeSet<String>` param is **replaced** by `scope`.
- Inside `build_site_patch_set`, `PatchScope::Project(set)` supplies the opt-out; `PatchScope::NoProjectContext` supplies an empty opt-out **by construction, with an auditable name**.
- **No `ProjectConfig::patch_scope()` helper** (dependency-direction hygiene): such a helper would force the `project` module to import the `package_manager` resolver module, inverting the dependency. Instead the ~4 project/global call sites — which already hold both a `ProjectConfig` and the manager — inline `PatchScope::Project(cfg.no_patches_repositories())`. `ProjectConfig` stays zero-knowledge of `PatchScope`.

**D1 ✓** direnv passes `PatchScope::Project(project.config.no_patches_repositories())`; launcher passes `PatchScope::NoProjectContext`.
**D2 ✓** a new caller cannot compile without choosing a variant — no default exists.
**D3 ✓** `NoProjectContext` names the honest state; `Project(empty)` (a project with zero opt-outs) is a *different, distinguishable* value from `NoProjectContext`.
**D4 ✓** the manager never learns about projects; the command layer that holds `ProjectConfig` constructs the scope, exactly like `resolve_mirror_map` → `MirrorMap` is resolved at `Context` and handed to the client.
**D5 ~** ~13 production sites take a one-line variant; ~30 test sites append `PatchScope::NoProjectContext`. Mechanical.

### Option A′ — delete the wrapper, keep the raw `&BTreeSet`, pass `&BTreeSet::new()` explicitly

The cheapest option that still fixes both defects: delete the silent-default 2-arg `resolve_env`, but keep `resolve_env_with_patch_boundary(packages, self_view, no_patches: &BTreeSet<String>)` as the single entry — no new enum. No-project sites write `&BTreeSet::new()` *explicitly* at the call site.

- **Also fixes D1 and D2**: with the wrapper gone, every caller must pass *some* `&BTreeSet`, so BUG 1 (silent empty) cannot recur by omission.
- **Weaker on D3 only**: an explicit `&BTreeSet::new()` at a no-project site does not *name* the reason it is empty — a reader cannot tell "intentionally no project here" from "project with zero opt-outs." The enum buys **exactly one thing** over A′: the auditable `NoProjectContext` **name** (driver D3, 15%). Nothing else — A′ closes the recurrence hole just as well.

Chosen A over A′ because ~13 no-project sites (launcher, OCI-tier, scratch, probe) benefit materially from a self-documenting variant over a bare `&BTreeSet::new()`, and the enum makes the launcher re-injection discussion (above) legible at the call site. This is a deliberate, low-cost spend for D3, not a claim that A is the *only* footgun-closer.

### Option B — thread `Option<&ProjectConfig>` (or fold opt-out into `ResolvedPatchConfig`)

Two sub-shapes: (b1) `resolve_env(packages, self_view, project: Option<&ProjectConfig>)`; (b2) bake the opt-out set into `ResolvedPatchConfig` / a manager field at construction.

- b1 **fails D2/D3**: `None` is a *silently valid* value that reads as "forgot to pass it" — the exact footgun class of BUG 1, just relocated. A reviewer still cannot tell an intentional `None` from an omission.
- b2 **fails D2 and D4**: the manager is built once per `Context`, but the project is resolved *later* (`load_project_with_lock`), and OCI-tier/launcher callers have **no** project. The real killer is D2, not just layer purity: a `with_no_patches(set)` setter (paralleling the existing `with_patches`) would be **un-called at the ~8 no-project construction sites and silently default to empty** — literally reproducing BUG 1 one layer up, on the manager. Baking the opt-out into the manager also couples manager lifetime to project resolution and leaks a project concept into a project-agnostic facade (D4).

Rejected: b1 does not close the recurrence hole; b2 reproduces BUG 1 on the manager (D2) and violates layer purity (D4).

### Option C — status quo + a lint/doc guard

Fix the two sites by hand; add a clippy/doc note that `resolve_env` must receive the opt-out.

- **Fails D1 partially, D2 fully**: fixes today's two sites but leaves the silent-default wrapper in place, so the *next* caller re-opens the hole. No structural prevention; a comment is not a compile error.

Rejected: the reviewer comment C4/C14 is explicitly about making the choice *structural*, not documented.

### Weighted comparison

| Criterion (weight) | A: `PatchScope` enum | A′: explicit `&BTreeSet::new()` | B: `Option<&ProjectConfig>` / manager field | C: status quo + lint |
|---|:---:|:---:|:---:|:---:|
| Fix both live bugs (30%) | ✓ | ✓ | ✓ | ~ (manual, no structure) |
| Prevent recurrence — compile-forced (25%) | ✓ | ✓ | ✗ (`None`/field default) | ✗ |
| Honest no-project boundary (15%) | ✓ (named) | ~ (explicit but unnamed) | ✗ (`None` ambiguous) | ✗ |
| Layer purity (15%) | ✓ | ✓ | ✗ (b2) / ~ (b1) | ✓ |
| Migration simplicity (15%) | ~ | ✓ | ~ | ✓ |
| **Outcome** | **chosen** | runner-up (loses only D3) | rejected | rejected |

---

## Decision Outcome

**Chosen: Option A — the `PatchScope` enum as a required argument, deleting the silent-default wrapper.** Every `resolve_env` / `resolve_env_with_patch_boundary` caller now makes a compile-visible, reviewable choice between `PatchScope::Project(opt_out)` and `PatchScope::NoProjectContext`. BUG 1 is fixed (direnv threads the real opt-out); BUG 2 gets an honest, auditable label.

To be plain about what the enum earns: **deleting the wrapper is what fixes the bug and closes the recurrence hole — Option A′ (an explicit `&BTreeSet::new()`) does that too.** The enum's *sole* advantage over A′ is driver D3 (15%): a self-documenting `NoProjectContext` name at the ~13 no-project sites, which also makes the launcher re-injection gap legible at the call site. It is a deliberate, cheap spend for that clarity, not the only way to close the footgun.

### Sub-decision: does a direct launcher invocation honor project `no-patches`? (the 7th fork)

**Decision: No — and this is correct by design. Accept and document (option (a)).**

`ocx launcher exec` runs a tool directly (`self_view=true`) with no `ProjectConfig` — it is reached when a tool on `PATH` (e.g. from `~/.ocx`) is invoked outside `ocx run`/`ocx env`/`direnv`. Project-tier `no-patches` is a **project-toolchain** opt-out (it lives in `ocx.toml`); a tool launched directly is not "running in a project," so the project opt-out legitimately does not apply. This initial design called for `PatchScope::NoProjectContext` as the honest label; the shipped implementation instead constructs `PatchScope::Project(no_patches)`, decoding the caller's opt-out set forwarded over `OCX_PATCHES` (empty when nothing was forwarded — byte-identical to `NoProjectContext` for enforcement purposes) — see "Resolution" below.

Rationale:
1. The launcher is the execution-env twin used by *direct* invocation — deliberately project-agnostic. It knows only the package root and the forwarded `OCX_*` resolution config.
2. The security-critical case is unaffected: a **system-required tier** is not suppressible by an opt-out under any scope (`ResolvedPatchConfig::system_required` is a tier-level property — all-or-nothing, not per-companion), so `NoProjectContext` still enforces it. Only a user's *non-enforced* project opt-out is skipped when they bypass the toolchain — acceptable.

Documented contract (env-composition reference + `patches.md`): *"A project `no-patches` opt-out applies to `ocx run`, `ocx env`, and `direnv export`. A directly-invoked tool launcher applies every non-opted patch (a system-required tier always; a project opt-out only takes effect when the tool runs through the toolchain — see the re-injection gap below)."*

### The launcher re-injection gap (through-project runs) — MATERIAL FINDING

The 7th-fork decision above covers *direct* invocation. But the launcher is also the **last hop even for tools run *through* a project**, and there the picture is worse than "the opt-out doesn't apply because there's no project":

`ocx run` composes the child env with the opt-out honored (`run.rs:215-217`, `PatchScope::Project(...)` after this ADR), then resolves `argv0` against the *composed* `PATH`. For an ocx-managed tool that resolves to a **generated entrypoint launcher**, the launcher re-enters `ocx launcher exec '<pkg-root>' -- argv0` (`launcher/exec.rs:65,134-135`), which calls `resolve_env(.., true)` → `NoProjectContext` → **re-derives every companion, re-injecting the one the opt-out just removed.** So `no-patches = true` does **not** stop an ocx-launchered tool process from receiving the companion under **any** scope — the BUG-1 fix only cleans the *exported shell vars*, not the launchered tool's own process env.

**Design response (recommended, gated on maintainer approval — see plan fork "Launcher opt-out forwarding"):** forward the resolved project opt-out set over the **already-existing** `OCX_PATCHES` wire ABI. That channel already round-trips `system_required` (`config/patch.rs` `encode_patches`/`patches_from_env`), so the marginal cost is one more forwarded field (`no_patches: [..]`); the launcher's `resolve_env` then reconstructs `PatchScope::Project(forwarded_set)` when the parent forwarded one, and falls back to `NoProjectContext` for genuinely-direct invocation (no forwarded opt-out present). This keeps the 7th-fork decision intact (direct invocation stays `NoProjectContext`) while closing the gap for toolchain-mediated runs.

**Alternative:** document the weaker contract honestly — "a project `no-patches` opt-out suppresses a companion from the exported/composed env, but an ocx-launchered tool still receives it; use it for env hygiene, not as a security boundary." A system-required tier is unaffected either way (always enforced).

**Missing validation (either path):** there is currently **no test** exercising a *launchered tool's* process env under a **non-system-required** tier with an **opted-out** base. Added below.

### Resolution (implemented 2026-07-02)

Maintainer **approved forwarding** (not the weaker documented contract). Implementation surfaced a refinement the recommendation missed: forwarding `registry/repository` opt-out keys is **insufficient at the launcher**, because a content-shared package resolves there to a synthetic `file-url-mode/<content-digest>` identifier with no real `registry/repository` — so a forwarded repo-key never matches (only a global `match:*` companion re-injects at all; per-base companions never re-resolve at the launcher). Closed by **matching the opt-out by content digest**: `ocx run` forwards both the opted-out bases' repo-keys AND their resolved content digests over `OCX_PATCHES.no_patches`, and the resolver's opt-out check matches an admitted base by repo-key OR content digest (`resolve.rs`). C7 preserved — a `system_required` tier still overlays under any leg. Shipped in `feat(patch): forward the no-patches opt-out to launchered tools` + the digest-match follow-up.

Two cross-model (Codex) findings on the shipped diff:
- **F2 (fixed):** the forwarded opt-out leaked into *unrelated* child processes — `Context::try_init` grafted it onto any config-file tier, which `apply_ocx_config` re-forwarded to every child. Confined to the launcher-exec path (decode `no_patches` locally from `OCX_PATCHES` at `launcher/exec.rs`; drop the global graft). Shipped in `fix(patch): confine forwarded no-patches opt-out to the launcher path`.
- **F1 (accepted limitation):** digest matching over-suppresses on a content-digest collision (two distinct identities, byte-identical content, one opted out → the other, run through its launcher in the same `ocx run`, is also skipped). Inherent to the digest approach; the only avoidance is identity-recovery (option a, declined). Blast radius is tiny and further narrowed by the F2 confinement. Documented as a known limitation.

### Reversibility

Reversible internally within this repo (no stable public API, no persisted format). The migration is mechanical and re-doable; the risk is churn, not lock-in. **Out-of-tree caveat:** `ocx-mirror` (an external repo that vendors `ocx_lib` as a submodule + path dependency) also calls `resolve_env` — the signature change is a coordination cost the "no external consumers" framing would elide. Tracked as a Phase-7 downstream issue in the plan. The enum can later grow (e.g. a `Global`-specific variant) without breaking callers because internal enums omit `#[non_exhaustive]` (matches stay total — `arch-principles.md`).

### Consequences

**Positive:** the silent-default footgun is deleted; both live bugs fixed; a named no-project state; the manager stays project-agnostic (D4); the pattern generalizes to every future env-resolving command. Feeds the persisted rule (flag-naming + no-silent-default) via the plan's Phase 7.
**Negative:** ~43 call sites touched (mostly tests); one new enum (no helper — call sites inline `PatchScope::Project(cfg.no_patches_repositories())`); the `resolve_env` API signature changes (acceptable pre-1.0). `resolve_env_from_package_root` (`package_manager.rs:489`) also gains the scope parameter (its callers are launcher-parity → `NoProjectContext`). External `ocx-mirror` consumer needs a coordinated bump (see Reversibility).

### Risks

| Risk | Mitigation |
|---|---|
| A migrated test accidentally masks a real Project-scope path | **Phase-1** regression tests assert the *behavioral* delta (direnv opt-out honored; a system-required tier still enforced despite no project) — not just that it compiles. |
| The launcher re-injects a companion the opt-out removed (through-project runs) | Recognised above as a material finding; recommended fix = forward the opt-out over `OCX_PATCHES`; a new test exercises a launchered tool's env under a non-system-required tier + opted-out base. Gated on maintainer approval. |
| `NoProjectContext` used where `Project` was intended (future caller) | The variant name is self-documenting; a `Project`-tier command passing `NoProjectContext` is a visible review smell. Covered by the plan's acceptance criteria. |
| Enum churn re-touches ~30 tests | Mechanical refactor commit (Option A′-equivalent site edits), no behavior change in those sites (they were already empty-set). |

---

## Validation

- [ ] The 2-arg `resolve_env(packages, self_view)` wrapper no longer exists; every call site passes an explicit `PatchScope`.
- [ ] **BUG 1 regression:** a project declaring `[package."<id>"] no-patches = true`, composed via `ocx direnv export`, omits that base's companion overlay (and still emits it without the opt-out).
- [ ] **BUG 2 label:** `ocx launcher exec` resolves with `PatchScope::Project(no_patches)`, decoding the caller's opt-out set forwarded over `OCX_PATCHES` (empty set for a genuinely direct invocation — byte-identical to `NoProjectContext` for enforcement purposes); a **system-required tier**'s companions are still applied (not suppressed) under any leg.
- [ ] **Launcher re-injection (material finding):** a launchered tool's *process* env under a **non-system-required** tier with an **opted-out** base — asserts the shipped contract (weaker-documented, or opt-out honored if `OCX_PATCHES` forwarding is approved). Currently untested.
- [ ] Project/global/run callers **inline** `PatchScope::Project(cfg.no_patches_repositories())`; OCI-tier/scratch/probe callers pass `PatchScope::NoProjectContext`. No `ProjectConfig::patch_scope()` helper (dependency-direction hygiene).
- [ ] `resolve_env_from_package_root` carries the scope through to `NoProjectContext` at its launcher-parity callers.
- [ ] No `[patches]` configured → byte-identical output for every scope (no-config no-op preserved).
- [ ] `task rust:verify` passes; no `resolve_env` call site relies on a defaulted opt-out.

## Links

- Hardens: `adr_infrastructure_patches.md` (C4 site tier, C7 enforcement, per-package `no-patches`).
- `adr_global_toolchain_tier.md` — project vs global resolution sites.
- Plan: `.claude/state/plans/plan_patch_feedback_adoption.md` (Phase 1).

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-07-01 | architect | Initial — `PatchScope` type-forced env construction (C4/C14); launcher-semantic sub-decision (accept + document); delete the silent-default `resolve_env` wrapper. |
| 2026-07-01 | architect (post-review) | Add the launcher re-injection gap (material: opt-out doesn't reach a launchered tool process under any scope; recommend `OCX_PATCHES` forwarding) + its missing test; add Option A′ (explicit empty set) and state the enum's sole gain is D3's name; strengthen b2 rejection (un-called `with_no_patches` reproduces BUG 1); drop `patch_scope()` helper for dependency-direction hygiene (inline instead); correct `system_required` to tier-level; fix regression-test phase ref (Phase 1); note `ocx-mirror` out-of-tree consumer in Reversibility. |
| 2026-07-02 | /swarm-execute | Implemented. Launcher opt-out forwarding approved + shipped, but repo-key forwarding was insufficient (synthetic content-digest launcher id) → closed by content-digest matching (see "Resolution"). Codex F2 (opt-out leaked to unrelated children) fixed by confining consumption to the launcher path; F1 (digest-collision over-suppression) accepted as a narrow documented limitation. `PatchScope`, both bug fixes, and forwarding all landed; `task verify` green. |
