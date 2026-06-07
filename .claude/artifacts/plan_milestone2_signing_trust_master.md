# Master Plan — Milestone 2: Signing & Trust v1 (swarm-x)

## Status

- **Plan:** plan_milestone2_signing_trust
- **Active phase:** B — Per-issue loop
- **Step:** /swarm-x → Phase B, cursor at #195
- **Last update:** 2026-07-09 (after setup: swarm-loop + swarm-x skills, long-living branch)

## Goal

Milestone 2 ("Signing & Trust v1") fully implemented on the long-living branch
`feat/signing-and-trust`, closing with a merge-ready PR where the basic
(`task verify`) and deep (`task rust:verify` + acceptance suite) verification
workflows run through. Secure defaults: publisher signs after push, consumer
verifies by default, typosquat defeated.

Source of truth for scope + SOTA: `.claude/artifacts/plan_milestone_split_supply_chain.md`
and tracker issue #24.

## Long-living branch

- `feat/signing-and-trust`, cut from `feat/oci-referrers-sign-verify` (carries
  PR #87 slice 1: `ocx package sign|verify` scaffold, fake_sigstore.py stack,
  skipped acceptance specs). PR #87 counts as pre-merged into the base.
- Per-issue branches: `wip/<n>-<slug>` (NOT prefixed with the long-living branch
  name — git ref/dir collision). Merged back with `--no-ff` merge commits.

## Issue set + dependency DAG

```
#87 (slice 1) ──► already on base
#195 test-infra registry   ── independent, start first (unblocks positive acceptance tests)
#194 sigstore pipeline     ── critical path; depends #87; step 0 = timeboxed spike
#106 capability wiring      ── depends #194 (first e2e slice)
#98  trust policy           ── depends #194
#196 offline/trust-root     ── depends #194; gates #99
#99  auto-verify install    ── depends #98 + #194 + #196
#107 rekor v2 delta         ── gated on #194 spike (may close into #194)
#197 cosign v3 interop      ── last; depends #194 (+ cosign 3.x reachable)
```

No A→B milestone edges. Cross-repo: ocx-mirror#7 depends on #194 (out of scope).

## Execution order (Phase B cursor)

| # | Issue | Branch | Tier | Gate / notes |
|---|-------|--------|------|--------------|
| 1 | #195 | `wip/195-referrers-registry` | high | registry:3 (or zot) harness; keep one registry:2 as permanent ReferrersUnsupported negative fixture; pre-flight: confirm ocx.sh referrers support. Acceptance infra only — no product code path yet. |
| 2 | #194 | `wip/194-sigstore-pipeline` | max | **critical path.** Step 0 spike (sigstore-rs bundle write, TUF TrustedRoot fetch, Rekor v1, offline root override, `--trust-root` seam). Fill ~10 stub modules; NativeTransport referrers HTTP incl. `?artifactType=`; wire capability cache; flip `#[ignore]` + un-skip acceptance specs; `--format json` verify contract (global flag). Target Rekor v1 + TUF root; v2 = #107. |
| 3 | #106 | `wip/106-capability-wiring` | high | Wire capability cache into pipelines (`no_cache` seam exists); clean actionable error, no tag fallback; acceptance vs stubbed 404 + registry:2 fixture. Delete fictional `_catalog`/capabilities.toml/24h TTL from issue body. |
| 4 | #98 | `wip/98-trust-policy` | max | `[trust.policy]` in ocx.toml: `identity` (exact) + `identity_regexp` (mutually exclusive); most-specific scope wins, ANY-of among equal; tier array-merge; `--certificate-identity/-oidc-issuer` optional when policy matches. Docs: configuration.md, user-guide, exit codes. |
| 5 | #196 | `wip/196-offline-verify` | high | OCX_OFFLINE + policy-matched install: fail vs skip-with-warn (never silent skip); trust-root cache in `state/`, TTL/refresh, `OCX_SIGSTORE_TUF_ROOT` override; resolve verify-online-only vs install-offline-first. Gates #99. |
| 6 | #99 | `wip/99-auto-verify` | max | Verify after resolve, before download (metadata-first seam); offline semantics from #196; flag>env precedence, WARN once/invocation; OCX_NO_VERIFY into environment.md + `Env::apply_ocx_config`. |
| 7 | #107 | `wip/107-rekor-v2-delta` | high | Rekor v2 delta only, gated on #194 spike outcome. If spike showed day-one v2 support, close into #194 (skip branch) and record. |
| 8 | #197 | `wip/197-cosign-interop` | high | Standalone (not a #194 exit gate). Spike: cosign 3.x accepts fake-stack trusted-root JSON. Retarget cosign 3.x; pre-3.0 compat dropped. Prereq: cosign reachable. |

Each row runs via `swarm-loop #<n> --onto=feat/signing-and-trust --max-review=3`
in a dedicated Opus subagent; merge `--no-ff`; then a one-shot deferred
entirety-consistency pass on the long-living branch.

## Phase C — Milestone-completion loop (bounded ≤5)

After all issues merged: bounded `/swarm-review max` ↔ `/swarm-execute` loop
focused on *milestone* completion — every acceptance criterion met, threat table
claims only shipped defenses, docs + website cast complete, no dangling
cross-issue seams. Exit on clean review or 5 rounds. Oscillating → defer.

## Phase D — Merge-ready PR

`task verify` (basic) + deep gate (`task rust:verify` + acceptance suite) green,
or every red an honestly-documented dependency-gated skip. Prepare PR
`feat/signing-and-trust → main` body (Closes #194 #195 #196 #197 #98 #99 #106
#107; deferred findings; known gaps). Do not push/merge without human go.

## Known hard constraints (honest)

- Real keyless Sigstore needs network (Fulcio/Rekor/TUF). Positive-path tests
  run against `fake_sigstore.py`; real-network paths stay behind `#[ignore]` /
  feature gates, documented — never claimed green without evidence.
- #197 cosign interop feasibility is unproven until the spike; may land as a
  documented gap if cosign 3.x is unreachable in the sandbox.
- Sequential delegation (context focus) — no concurrent worktrees for the issue
  loop; dependency order enforced.
