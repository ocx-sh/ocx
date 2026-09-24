# Plan: Port-down waves (ADR Stage 7)

## Status

- State: blocked
- **Plan:** plan_test_port_down_waves
- **Active phase:** 0 — entry conditions (none met)
- **Step:** blocked on the Stage 6 NO-GO — see § Entry conditions
- **Last update:** 2026-09-23 (written at the WP-13 pilot's NO-GO, on the owner's request that the follow-up plan exist regardless of the verdict)

---

## Overview

**Status:** Draft — blocked
**Author:** WP-13 coordinator (plan_test_speed_tiers)
**Date:** 2026-09-23
**Related ADR:** [`adr_test_speed_tiers.md`](./adr_test_speed_tiers.md) § C-RUBRIC, § C-SEAM, Implementation Plan Stage 7
**Decision:** [`decision_port_down_pilot.md`](./decision_port_down_pilot.md) — NO-GO
**Inputs:** [`classification_test_port_down.md`](./classification_test_port_down.md), [`port_evidence/pilot.md`](./port_evidence/pilot.md)

## Objective

Move every port-capable acceptance case into a crate test, with the pilot's discipline: one
file-disjoint wave per target crate, and every deletion admitted by the C-007(e) guard on an
executed, mutation-proven port. The goal is that a Rust edit re-runs the cheapest test that pins a
behaviour, instead of an acceptance module that needs `test/bin/ocx`.

## Entry conditions

The ADR says Stage 7 runs "only on GO". Every unmet pilot criterion is an entry condition here,
and the plan stays `State: blocked` until each one is met or an ADR amendment retires it.

| # | Condition | Today | Unblocked by |
|---|---|---|---|
| E1 | ≥ 300 port-capable cases below `ocx_cli` (C-RUBRIC GO criterion 4) | 195 classified; the pilot landed 1 of 68 below-`ocx_cli` cases below `ocx_cli` | a re-classification under the pilot's rubric refinements that still reaches 300, **or** an ADR amendment that counts seam ports (`ocx_cli`) toward the threshold, with the payoff re-argued (the pilot measured 1.77 s of case time for 45 cases) |
| E2 | A per-case cost median, not a mean (criterion 3 as written) | mean only: agent usage arrives as one total | per-case sessions (one porter call per module or case group), or an amendment that accepts the mean |

Conditions that a GO would still need before wave 1 (pilot findings, `decision_port_down_pilot.md`
§ "What the pilot changes in the rubric"):

| # | Condition | Why |
|---|---|---|
| P1 | C-RUBRIC amended with the pilot's five refinements (composition in the command body is `ocx_cli`; a re-implemented command is not a port; `log::` diagnostics stay e2e; reference-validator cases are R8; seam co-residence) | the classification counted cases the pilot could not land |
| P2 | Seam decisions per wave: which verbs join `ADMITTED`, and whether `run` bridges the `log` facade | `config setup` (13 cases) and the warn/no-warn cases are blocked on exactly these |
| P3 | The OQ3 ruling recorded in the ADR: Opus porters, plus an independent Opus audit per wave | Sonnet's pilot evidence was not reproducible (0/57) |

## Scope

### In Scope

- The port-capable cases in `classification_test_port_down.md`, re-derived per case under P1 at
  each wave's start (the classification's module-level rulings are a starting list, not a verdict).
- Seam extensions a wave needs (P2), each a separate reviewed commit with its own invariant tests.
- Per wave: `port_evidence/<wave>.md`, the originals' deletion under C-007(e), and
  `test/SUITE_FLOOR`, suite census `PINNED`, `ACCEPTANCE_MODULE_TARGETS` (when a whole module
  goes), `test/scoped_rows.toml` and `crates/TEST_TARGET_MAP.toml` updated in the same commit.

### Out of Scope

- Cases the rubric keeps e2e (R1–R8 and the pilot's refinements).
- `test_state_providers.py` (R8).
- Any change to the deletion guard itself. If a wave needs a new shape, that is a plan
  amendment first.

## Technical Approach

### Key Decisions

| Decision | Rationale |
|----------|-----------|
| One wave = one target crate, file-disjoint | ADR Stage 7. Parallel worktrees stay mergeable, and the guard's range is one crate's ports |
| Porter: Opus; auditor: a second Opus pass per wave | OQ3 pilot result (`decision_port_down_pilot.md`) |
| Land only audit-`full` ports; partial ports stay unmarked or are dropped | the pilot's 10 partial Opus ports would have deleted coverage |
| Lowest crate whose public API carries the asserted property; the seam only for CLI-surface properties | C-SEAM ("the seam is not the default port target") |
| Mutations one at a time, needle-checked, byte-restored | batched mutations cannot attribute a red (pilot, Sonnet column) |

## Parallelization

≤ 3 concurrent waves (cargo jobs = 12, RAM, the acceptance-suite flock). Sizes are collected cases
from the WP-10 classification, before P1 re-derivation.

| WP | Scope | Key Files | Size | Base |
|----|-------|-----------|------|------|
| W1 | `ocx_package`: `test_archive_containment`, `test_package_create_bin_scan`, `test_package_create_extract`, `test_schema`, `test_metadata_forward_compat` (2 of 5) | `crates/ocx_package/src/**`, those modules | 57 | feature tip |
| W2 | `ocx_project`: `test_project_config`, `test_project_config_home_walk`, `test_project_init`, `test_project_toml_preservation` | `crates/ocx_project/src/**`, those modules | 43 | feature tip |
| W3 | `ocx_config`: `test_config.py` (18 of 24) | `crates/ocx_config/src/**`, `test/tests/test_config.py` | 18 | feature tip |
| W4 | `ocx_console`: `test_color.py` | `crates/ocx_console/src/**`, `test/tests/test_color.py` | 9 | feature tip |
| W5 | `ocx_cli` seam: `test_package_receipt.py` (4), `test_config.py` dispatch (6), the pilot's `ocx_cli` leftovers | `crates/ocx_cli/src/**`, those modules | ≤ 13 | after P2 |
| W6 | `config setup` through the seam (13), once P2 admits it with a stub transport | `crates/ocx_cli/src/app/seam.rs`, `command/config_setup.rs`, `test/tests/test_config_setup.py` | 13 | after P2 |

**Dependency DAG:** W1, W2, W3, W4 independent → W5 → W6 (W5 and W6 both edit the seam and
`ocx_cli`, so they run one after the other).

**Shared files, one writer per merge, serialized by merge order:** `test/SUITE_FLOOR`,
`scripts/suite_census.py`, `crates/TEST_TARGET_MAP.toml`, `scripts/bazel_gate_proofs.py`
(`ACCEPTANCE_MODULE_TARGETS` when a module goes), `test/scoped_rows.toml`.

## Implementation Steps

> **Contract-first TDD.** Per case: the Rust test is written from the original's assertions and
> shown red on a mutation of the exercised code before the original is deleted.

### Phase 1: Stubs

- [ ] **Step 1.1:** Per wave, list its cases under P1 (port / keep with the rubric item). Stub
  the target test modules (`#[cfg(test)] mod …`, `seam::` path for seam tests).

### Phase 2: Architecture Review

- [ ] **Step 2.1:** An Opus reviewer checks the case list against C-RUBRIC + P1 and the target APIs.

### Phase 3: Specification Tests

- [ ] **Step 3.1:** Ports with `// ported-from: test/tests/<module>.py::<case>` markers, each red on
  a mutation that is proven landed.

### Phase 4: Implementation

- [ ] **Step 4.1:** Restore (proven), green; independent Opus audit (faithfulness for every case,
  own mutations on a sample of at least 12, evidence reproducibility); land only `full` ports.
- [ ] **Step 4.2:** Delete the originals and update the floors; commit; run the guard post-commit:
  `task scripts:test-diff-guard RANGE=<base>..HEAD -- --tiered-shapes`.

Gate: guard green; `task verify` green; `SUITE_FLOOR` decrease == deleted collected count.

### Phase 5: Review & Documentation

- [ ] **Step 5.1:** `port_evidence/<wave>.md` committed with per-case proofs.
- [ ] **Step 5.2:** An Opus sample review per wave (ADR Stage 7).

## Acceptance (per wave, ADR Stage 7 exit)

- The deletion guard is green only on executed ports: shown red on one `#[ignore]`d port in a
  throwaway copy (S-020).
- An evidence file is committed.
- The `SUITE_FLOOR` decrease equals the deleted count.
- T2 is green.

## Risks

| Risk | Mitigation |
|---|---|
| The classification over-counts (the pilot landed 35 of 68 cases, only 1 below `ocx_cli`) | P1 re-derivation per wave; size each wave after it, not before |
| A partial port deletes coverage | Audit gate; only `full` ports land |
| Seam growth reintroduces process globals | Each `ADMITTED` or bridge change ships with its own C-SEAM invariant tests |
| Payoff too small to justify the agent-hours | E1's amendment path must re-argue the payoff against the pilot's 1.77 s per 45 cases |
