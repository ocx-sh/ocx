# Design Spec: Announce Initiative — Canonical Decision Register

<!--
Consolidated design record for the announce initiative (ocx package announce +
index-side bot hardening + E2E + rollout). Extracted from the owner design
session of 2026-07-18..22 (Fable orchestrator). EVERY sub-plan author and EVERY
implementation orchestrator MUST read this file fully before acting. Decisions
here are RATIFIED BY THE OWNER — consume, do not re-litigate. Open cells are
explicitly marked OPEN.
-->

**Status:** Ratified (owner, 2026-07-22)
**Authority stack (read in this order):**
1. `../index/.claude/artifacts/adr_fork_pr_announce.md` (ADR-6) — server-side design authority: FP-1..FP-9, governance G-01..G-20
2. `.claude/artifacts/adr_announce_publisher_surface.md` (Accepted 2026-07-19) — client-side surface D1..D5 + ratification changelog. ⚠ Needs alignment pass (see §9 chores) — its D5 references a deleted `[indices]` config table
3. `../index/bot/CONTRACTS.md` §12 (announce pipeline), §14 (canonical serializer — client-facing byte contract)
4. `.claude/artifacts/research_publish_to_bcr_anatomy.md` + `_transfer.md` — BCR problem→solution catalog + adopt/adapt/avoid map
5. `.claude/artifacts/research_grimoire_announce_port.md` — grimoire copy-and-own inventory (persisted alongside this file)
6. GitHub: ocx-sh/ocx#216 (tracking), #218 (SSRF), index PR #49 (phase-3 workflows, open+green)

---

## 1. Non-negotiable strategy decisions

| # | Decision | Rationale anchor |
|---|---|---|
| S1 | Transport = **REST API only**. No git subprocess, no local clone of the index repo. | Owner 07-22; ocx ships dep-free; index carries CAS objects (clone cost grows) |
| S2 | **Align with BCR model**, not grimoire's. Grimoire = parts donor ("copy and own"), never strategy donor. | Owner: "just a template, not one-to-one copy" |
| S3 | **Always fork** — no direct push to `ocx-sh/index`, first-party included. One reviewed path for everyone. | Owner 07-22. Kills grimoire's tri-state permission probe entirely |
| S4 | **PAT-only day one.** Machine account `ocx-bot` (classic PAT, `public_repo`) for the ocx-contrib fleet; third parties bring their own classic PAT. Fine-grained PATs cannot fork/PR public repos (github/roadmap#600). GitHub App recorded as future scaling option ONLY, with hard constraint: never any permission on a publisher's source repo (BCR #157 / xz lesson). | Owner 07-22 after BCR-App-sunset correction |
| S5 | **Copy Rust parts from grimoire and own them.** No shared crate, no cross-repo dependency, no vendoring. | Owner: "copy and own. We do not want to share anything" |
| S6 | Index bot **stays Python. No rewrite.** Quality bar raised instead (§5). | Owner 07-22 |
| S7 | v1 impl GitHub-only behind a **forge-neutral user surface** (no `--github-*` names, forge-neutral config keys, `OCX_ANNOUNCE_TOKEN`). GitLab-hosted index = confirmed real future track — neutrality enforced in every review. No forge trait until a second impl exists. | Ratified 07-19 |
| S8 | desc stays **in the root / `__ocx.desc`** per ADR-6. Grimoire's enrich-tree split explicitly rejected. | Owner: "we stick with our design" |
| S9 | E2E runs against the **real index** (`ocx-sh/index`), not a sandbox. Old sandbox pair deleted (07-19). | Owner |
| S10 | ocx-mirror adoption = final phase, **one package only**; fleet rollout (~42 repos) is a separate later plan. Handover artifact to ocx-mirror at the end. | Owner 07-22 |
| S11 | Third-party enablement is **day-one, maintained scope** — not follow-up. "A few lines of workflow" for any GitHub publisher. `ocx dist` (cargo-dist-style workflow renderer) = future context note only; reusable-workflow "level 2" deferred until then (D4 placement question dissolved). | Owner |
| S12 | One shared **`ocx-contrib/index` fork** for the entire mirror fleet (branch-per-package → no collision). Third parties: own fork. | Owner: "a fork per mirror… insane" |

## 2. Client semantics (`ocx package announce`, Track A)

| # | Decision |
|---|---|
| C1 | v1 "what's new" = **explicit owner-curated tags**. Registry-scan mode = future (`--sync`, not now). |
| C2 | `ocx package push --announce-file <path>` appends the pushed primary tag + cascade tags. Format = comma/newline tag names (byte-compatible with indexbot `--tags-file`). Dedupe on append. **Per-package, per-pipeline-run scratch file** — never persistent (stale file could re-add a deliberately deleted tag). |
| C3 | `--tags` = full **replace** (reference-impl parity: curated set is the universe; absent tag = deleted). `--tags-file` = **additive union** with the committed root — documented deviation from replace-only `regenerate()`; deletion happens only via explicit `--tags`. |
| C4 | **Update, don't overwrite**: when our own announce branch already exists on the fork, regenerate from the **branch head**, not main — sequential announces accumulate into the one open PR. Fixes the racing-CI edge (announce #2 force-push silently dropping announce #1's still-unmerged tag). Safe because index CI re-derives every claim. **Amendment (owner, 2026-07-24, sol F2):** branch updates are fast-forward-only CAS (`force:false`) with one re-read-and-retry on failure — closes the concurrent-announce lost-update window (a dropped yank is not re-derived by CI). |
| C5 | `--refresh`: re-observe every tag already in the committed root **plus** file additions — catches moved digests (`latest`, cascades) without becoming registry-scan. Universe stays owner-curated. |
| C6 | **Unchanged ⇒ no-op**: if serialized root is byte-identical to the committed root AND no new CAS files → skip commit, skip PR, exit 0, report `status: "unchanged"` (vs `"updated"` + PR URL). No warnings — benign state. Works because `regenerate()` preserves `observed` timestamps for unmoved digests and the serializer is deterministic. Reference bot gets the same short-circuit (Track B — `cli/announce.py:218-267` currently always commits). **Amendment (owner, 2026-07-24, sol F1):** in fork mode, an unchanged run whose branch is ahead of the upstream base additionally ensures an open PR exists (read-only check; opens one if absent) — recovery for the commit-succeeded/PR-open-failed double fault. Unchanged with no branch divergence stays a pure no-op. |
| C7 | Yank: `--yank/--unyank --yank-reason` — owner action only, applies only to tags in the curated set, yank ≠ delete. Never set automatically; `--refresh` must never touch yank markers. |
| C8 | Fork auto-create when missing (idempotent ensure; classic PAT covers it). Create the topic branch in the fork **directly at the upstream base SHA** (forks share GitHub's object store — no sync step, no stale-base risk). Scheduled `merge-upstream` on `ocx-contrib/index` = hygiene only, not a dependency. |
| C9 | Branch naming + PR dedupe: stable branch per package, **match the Python reference tool's convention** (`indexbot-announce-<ns>-<pkg>`) unless Track A records a reason to diverge — FP-9 makes the Python tool the executable spec, and a shared convention lets both tools dedupe against each other's PRs. PR open-or-update on 422/conflict (Homebrew-style duplicate handling; BCR/winget have none — known gap, don't copy). |
| C10 | Announce reads the committed root via **forge contents API** (main, or own branch head per C4) — NOT via the sparse-index HTTP client / `IndexSource`. No dependency on the read plumbing. |
| C11 | Index-repo coordinate: `--index-repo` flag, default `ocx-sh/index` (reference parity). Optional config field later if needed. |
| C12 | Multi-root PRs: **allowed** (no client refusal). G-19 evaluates ownership per-root from the base ref → no escalation; mixed owned/unowned falls to human lane. Client naturally emits one PR per package. |
| C13 | Exit codes: classify to existing sysexits (`Unavailable=69`, `ConfigError=78`, `AuthError=80`). No new codes. Report through `DataInterface` (JSON/plain), diagnostics through `UserInterface` — see `subsystem-cli.md` / `subsystem-cli-api.md` conventions. |
| C14 | Layering: orchestration in `ocx_lib` (CLI thin wrapper; ocx-mirror reuses lib). New CLI file `package_announce.rs` + `api/data/announce.rs` per flat-module convention. |
| C15 | Branch commits are **multi-file atomic** via the git data (blobs→tree→commit→ref) API — mirroring the reference tool's `GitHubPort.commit_files(files: dict)` semantics. The contents API is single-file; never loop it (N non-atomic commits, broken byte-contract windows between root and CAS files). One announce = one commit carrying root + all new CAS files. |

## 3. Canonical serializer (the cross-repo byte contract)

- Two forms, both pinned by CONTRACTS §14: **root** = 2-space indent, spec'd insertion-order fields, trailing newline; **CAS objects** = minified, alphabetized keys, `ensure_ascii`, no trailing newline; digest = sha256 of exactly those bytes.
- Rust writer is a **hand-written spec'd serializer** — never `serde_json::to_string_pretty` over maps/structs. JSON guarantees no key order; ambient serde ordering is a correctness bug here.
- **Golden fixtures published index-side (P0)**, CI-checked against the Python serializer, vendored/fetched by ocx's test suite. Minimum set: minimal root; root with yank + `superseded_by` + `upstream` + desc; observation object with multi-platform + `os.features`. Both sides gate on parse → re-serialize → byte-compare round-trips.
- Cross-language hazards to test explicitly: string escaping (`ensure_ascii` unicode escapes), indent style, ordering. No floats in the schema (mercifully).
- **Invariant (owner-stated): no representational churn** — refresh of unchanged data is byte-identical. No agent may ever "fix" the serializer with a generic pretty-printer.
- `wire.rs` on main is `Deserialize`-only and `IndexStore` writes fetched bytes verbatim — the canonical **writer is net-new work**, not an extension of existing serialization.

## 4. Security requirements (both repos)

| # | Requirement |
|---|---|
| X1 | **SSRF guard, default-on, range-based** (closes ocx#218): hosts arriving in remote-controlled data (root `repository:` pointers) must not resolve to private/loopback/link-local/metadata ranges. Public hosts never trip it — zero config for the open-source path. Check binds to **resolved IPs at connect time** (resolve → validate → pin) — hostname-string checks lose to DNS rebinding. (oci_client path: pin via a minimal injection seam added to the vendored `external/rust-oci-client` fork — owner-authorized; fallback to validate-floor + follow-up only if the seam exceeds S-size). |
| X2 | SSRF escape hatch = **explicit, dedicated, per-index-source** config: `trusted_hosts` (hosts/CIDRs) on the `[registries."<ns>"]` entry. NO inference from `[mirrors]`/other config (correspondence fails for private indices; coupling hazard). Managed-tier distributable; per-entry `system_locked` applies. |
| X3 | Ordering parity with index bot BD-1: guard runs **before the first registry request** in announce. Dedicated ordering test. Shared module consumed by announce AND the read path (#218 closes in this initiative). |
| X4 | Host **allowlist** (which hosts may appear in roots at all) stays index-side governance — client never duplicates that policy, only the SSRF floor. |
| X5 | Forge HTTP client: `redirect::Policy::none()` (bearer replay on cross-host 3xx), token via header only, never logged, one client per run. Port grimoire's invariants: fork-parent verified against upstream; fork identity (`full_name`, endpoints) built **only from API response bodies** (renamed-fork safety); rebuild every endpoint from the verified identity — never follow an API-returned URL blindly. Bounded fork-readiness poll (2s→30s doubling, 300s deadline, 8s per-request timeout) + one 3s retry for GitHub's "fork metadata ready before git objects" 404 race. |
| X6 | Token never enters `auth/store.rs` (docker credential store). `OCX_ANNOUNCE_TOKEN` env only. Token-leak assertions are first-class tests (grep stdout/stderr/argv in acceptance tests). |
| X7 | Index bot = **highest-risk surface** (owner). Always-loaded guardrail rule in the index repo's `.claude/rules/` stating the security bar + coverage bar + untrusted-PR-data-only contract. One **named test per governance contract G-01..G-20** and per threat class: SSRF ordering, `pull_request_target` head isolation, self-authorizing PR (owners edited inside the PR), login-recycling/numeric-id binding, path escape, yank tampering, empty-diff. |

## 5. Index bot quality bar (Track B)

- `fail_under = 100` branch coverage **already green — do not lower** (owner corrected own 90% suggestion). Keep 100.
- Add: warnings-as-errors lint posture; an **integration suite** (real local registry + git fixtures), not just monkeypatched units; the per-contract named tests (X7).
- Unchanged-skip fix (C6 mirror) with regression test — keeps the Python tool the executable spec.
- Observability floor (BCR #176 lesson): every publisher-visible failure surfaces a structured reason (step summary / PR comment), never a bare exit. Audit `validate.yml` job outputs against this.
- Stale-surface cleanup (Track B): delete `.github/workflows/announce.yml` (retired doorbell, calls nonexistent `--validate-only`), delete `site/.../ops/rotate-announce-pat.md`, add G-19/G-20 to `governance-contracts.md`. The `how-to/announce-a-package.md` rewrite and the `claim-a-namespace.md` cross-link fix are **Track F's** (ruling R2 — CLI-first framing, ocx CLI as the primary documented path; see §8).
- Land PR #49 first (owner merge click) — everything above assumes it.

## 6. Config decisions (ocx)

- Landed model is the unification the owner wants: `[registries."<prefix>"]` with optional `index` field (F5a); `[indices]` table deleted (F5c); `[mirrors]` = traffic policy. **Mental model: the table declares what a reference prefix means** — for index-kind namespaces "registry as host" deliberately falls apart (Decision C: identity logical, location routing).
- **Ratified simplification (pre-Track-A chore):** delete the `url` alias field from `RegistryConfig`; `[registry] default` takes a literal prefix only; every `[registries]` key is an identifier prefix, always. Kills the dual-key-semantics trap (`default = "corp"` → alias deref vs authority matching raw keys). Refactor-as-if-never-existed; unreleased, zero external users.
- `ocx.sh` is **not yet index-kind** with zero config — the flip ships as a managed/shipped default once the index is populated. Rollout work item (meta-plan), not day-one.
- One remote per namespace (Decision H) is a structural invariant — nothing in this initiative may add a second remote per namespace.

## 7. E2E + dev loop (Tracks C/D)

- Topology: `michael-herwig/ocx-e2e-publisher` (repurposed: **real small Rust app**, replaces hand-crafted dummy; existing repo + CI wiring kept) → fork `michael-herwig/index` (exists, parent verified) → real `ocx-sh/index`.
- Dev loop, exact mechanics (owner has been burned here — follow precisely):
  1. Push ocx feature branch → `gh workflow run "Deploy Dev" --ref <branch>` (workflow_dispatch only).
  2. It publishes `dev.ocx.sh/ocx/cli:<next>-dev_<TS>` with `--cascade`; cascade algebra moves the floating **`<next>-dev`** tag (prerelease+build cascades one level to parent prerelease).
  3. Test project `ocx.toml` pins `dev.ocx.sh/ocx/cli:<next>-dev` — **no `_<TS>` build segment, ever**.
  4. Refresh with **`ocx update` only. Never hand-edit `ocx.lock`.**
  5. If `ocx update` keeps resolving the old digest → the deploy failed to advance the floating tag → fix the deploy. Never pin a timestamp to work around it.
- Exit gate (Track D): tag release → build → dev-ocx push + announce → fork PR on real index → `validate.yml` green → correct lane classification → merge → rendered `index.ocx.sh` serves the root → `ocx install` resolves from a clean machine (with the namespace configured index-kind, since the global flip hasn't shipped). **Second identical run: `status: "unchanged"`, zero PRs, zero commits.** First claim goes through the real human lane (G-04, self-review formality). First claim is a manual claim-a-namespace PR (announce refuses unclaimed namespaces, reference parity); the first ANNOUNCE then lands as a tag refresh. A subsequent tag-refresh announce must prove **G-19 machine-lane auto-merge** with no human click.
- Test-harness pattern (lift from grimoire, minus the git layer): `registry:2` container (existing conftest pattern) + stdlib `http.server` fake forge API covering every fork/PR state branch (202/201/409 fork, 422-reuse PR, scripted readiness sequences, renamed fork, parent mismatch) + token-leak assertions.

## 8. Rollout (Track E) + third-party (Track F)

- Track E: ocx-mirror announces **one** real mirror package into the real index via the shared `ocx-contrib/index` fork, `ocx-bot` PAT (org secret), bot's numeric id seeded in that root's `owners[]` → machine lane proof in production posture. ocx-mirror consumes dev-channel ocx (vendors ocx as submodule). Ends with a handover artifact in the ocx-mirror repo for fleet rollout (separate plan: PR volume, rate limits, index CI load, ordering).
- **E-pilot requirements (owner, 2026-07-22):**
  - **E-P1 Pilot + namespace**: pilot = **bazelisk**, published under the real-vendor namespace **`bazelbuild/bazelisk`** (index identity `ocx.sh/bazelbuild/bazelisk`; root carries the governance-mandatory `upstream {org: "bazelbuild", …}`). Vendor-org namespaces are the convention for all third-party mirrors.
  - **E-P2 libc os-features**: releases exercise the unified platform model — musl **and** glibc variants where the upstream provides them ("if required"; a single static binary publishes one universal variant). `os.features` declared correctly either way; no `-musl` tag suffixes ever. If discovery finds bazelisk single-static (Go), the plan escalates whether a dual-libc package joins the pilot to cover the two-variant resolution path — owner decides then.
  - **E-P3 Container test matrix via the EXISTING mirror test interface** (owner correction 07-24): use ocx-mirror's established pipeline — `mirror.yml` `containers: []` per linux platform (ADR `adr_ocx_mirror_test_pipeline.md` D7) running **`ocx package test`** inside each container (D3/A1: container invokes `ocx package test --platform <P> <bundle> -- <test-cmd>`; JUNIT per leg; `(version, platform)` green = AND across containers, gating the push). Pilot containers: **alpine (musl), ubuntu (glibc), fedora (glibc)**. No ad-hoc `ocx install` matrix, no new mechanism — extend the existing test interface only. (Index-resolution install proof stays Track D's job.)
  - **E-P4 Physical GHCR path convention** (owner ruling 2026-07-25, supersedes any earlier `target:` shape): mirrors move off `ocx.sh` onto GHCR; physical paths use slash segments, never hyphen-flattened: index logical `ocx.sh/<vendor>/<package>` → physical `ghcr.io/ocx-contrib/<vendor>/<package>` (e.g. `ocx.sh/bazelbuild/bazelisk` → `ghcr.io/ocx-contrib/bazelbuild/bazelisk`). Publishing repo is named `mirror-<vendor>` — the repo names the **vendor**, not the tool, since one vendor may ship several packages (`mirror-bazelbuild`, `mirror-astral-sh`; renames follow, e.g. `mirror-bazelisk` → `mirror-bazelbuild`). Variants (libc flavors, slim builds) stay version-tag concerns per E-P2 — never separate namespace segments. GHCR has no real nested-namespace feature; multi-segment paths resolve as ordinary paths, and repo linkage is driven by the `org.opencontainers.image.source` manifest annotation, not path shape (verified against `ghcr.io/homebrew/core/wget`: path segment `core` matches no repo, yet the package links via its own annotation) — so `<vendor>` needs no matching repo under `ocx-contrib`. **Hard gate: the mirror fleet must not be published before `ocx package push --annotation` is available to it** (landed on ocx `main` as f40be710) — publishing earlier ships packages with no repo linkage and no path hint to fall back on. Slugify with `StringExt::to_slug` where a segment needs it — no second slug convention. Cross-refs: extends E-P1's vendor-org namespace identity to the physical-path layer; consistent with E-P2 (variants = tag concerns) and S10 (fleet rollout stays gated, one-package proof only).
- Gradual announce is THE population mechanism for the 42 packages (no batch seed — `handoff_m1_republish.md` owner decision).
- Track F day-one deliverables (maintained): index-site how-to rewrite (fork-PR + ocx CLI), index-repo `CONTRIBUTING.md`, namespace-claim path doc, copy-paste workflow snippet **designed to slot into an existing release workflow** (download pre-built artifacts → `setup-ocx` → `push --announce-file` → `announce`; PAT secret + machine-account guidance; `open_pull_request:false`-style fallback documented). ocx website: announce command reference, `OCX_ANNOUNCE_TOKEN` in `environment.md` (canonical env reference — plans must enumerate doc surfaces), user-guide publish section. No migration prose (pre-1.0). |

## 9. Chores & owner actions

**Chores (P0):**
- Index repo `.gitignore`: add `.agents/worktrees/` (+ handle 3 stray worktrees in `.claude/worktrees/`).
- Refresh + commit `../index/.claude/artifacts/handover_announce_alignment.md` (untracked; predates PAT/always-fork/shared-fork/coverage decisions).
- ADR alignment pass on `adr_announce_publisher_surface.md`: D5 rewritten to the landed `[registries]` anchor (dead `[indices]` refs), add C4/C5/S3/S4 ratifications to changelog; fix stale `mirror.rs` doc comment ("a later work package" — landed); fix ADR-indirection grammar leftover `index-ns = "ocx.sh"`.

**Owner actions (block only what they block):**
1. Merge index PR #49 (green, human-review-required label). Blocks Track B start.
2. Create `ocx-bot` machine account + classic PAT + org secret on ocx-contrib. Blocks Track E only.
3. Word on closing phantom PRs #217/#219 (landed via rebase, still show open).

## 10. Execution-process requirements (for every orchestrator)

- **Canonical plan location**: the meta-plan, all sub-plans, their `## Status` blocks, and `current_plan.md` live ONLY in `/home/mherwig/dev/ocx-sion/.claude/state/plans/` (gitignored, per-checkout — git worktrees do NOT contain them). Always read and mutate them at that absolute path; never look for or copy plans inside `.agents/worktrees/` checkouts.
- **Commit-verify hook mechanics**: the PreToolUse hook blocks ANY bash invocation whose command string contains `git commit` unless a fresh verified-marker exists — including chains that would have run `task verify` first. Sequence as two separate Bash calls: (1) `task verify && echo $(date +%s) > .claude/hooks/.state/commit-verified`, (2) the `git commit`. The marker is consumed per commit.
- **/swarm-execute tiers**: Tracks A and B run at `max` (mandatory opus builders + Codex sol code-diff gate); all other tracks at `high` minimum.
- **Deploy Dev needs the branch on origin**: `gh workflow run "Deploy Dev" --ref <branch>` only works for a pushed branch — every dev-loop iteration therefore needs an owner push of the ocx feature branch first (orchestrators request it, never push).
- Worktrees: ALL under `ocx-sion/.agents/worktrees/<repo>-<slug>`, foreign repos included (`git -C <repo> worktree add <path> <branch>`).
- Model policy: orchestrators Opus; workers Sonnet default, Opus for multi-subsystem; **never Fable subagents**; every spawn sets `model` explicitly + `Model rationale:` line.
- Checkpoint-commit each phase **before any review** (reviewers with Bash have destroyed uncommitted work). Never push — owner decides. Conventional commits; batch review fixes before committing.
- After any Rust change, rebuild `test/bin/ocx` with `--features ocx/__testing` before pytest (stale binary = phantom regressions). Stale registry container = phantom failures (`docker compose down && up -d` after image swaps). Testing-only env vars use `__OCX_TESTING_<PURPOSE>`.
- `Reference::clone_with_digest` drops the tag — build digest refs via `.without_tag()` or pulls 404.
- Trust compiler/test runner over mid-edit rust-analyzer diagnostics; orchestrator re-runs final `task --force verify`.
- PreToolUse hook blocks the entire bash invocation — on retry re-run the full `A && B`.
- Cross-model gates: `terra` default-on in review loops, `sol` at max gates; validate Codex output freshness (current-code anchors) before acting.
- Rebase conflicts: spawn a subagent per conflict; audit every rebased commit's stat against scope.
- Landing: feature branch + squash + rebase onto local main; never PR from the fixed worktree branch.
