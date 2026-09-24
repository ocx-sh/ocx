# Decision: port-down pilot — NO-GO

- **Status:** decided — NO-GO (ADR Stage 6 exit)
- **Date:** 2026-09-23
- **Plan:** [`plan_test_speed_tiers.md`](./plan_test_speed_tiers.md) C-023, C-025, WP-13
- **ADR:** [`adr_test_speed_tiers.md`](./adr_test_speed_tiers.md) § C-RUBRIC ("Pilot GO criteria"), § C-SEAM, Open Question 3
- **Inputs:** [`classification_test_port_down.md`](./classification_test_port_down.md) (WP-10),
  [`port_evidence/pilot.md`](./port_evidence/pilot.md) (the per-case mutation proofs)
- **Follow-up:** [`plan_test_port_down_waves.md`](./plan_test_port_down_waves.md) — written, `State: blocked`

## Verdict

**NO-GO.** Two of the four criteria are not met (3 as written, and 4), and the ADR makes all four binding:

> **Pilot GO criteria** (all must hold): seam invariants 1-5 proven red/green (if the seam is
> built); 100 % of pilot ports mutation-proven with committed evidence; median agent cost per
> ported case recorded for both models; classification shows ≥ 300 port-capable cases (≈ 8 % of
> 3,884) whose target is a crate *below* `ocx_cli`. Otherwise NO-GO: keep the pilot's ports, stop.

The pilot's ports stay (37 test functions, 45 collected cases, deleted from the acceptance suite
under the C-007(e) guard). No porting wave starts.

| # | Criterion | Number | Result |
|---|---|---|---|
| 1 | Seam invariants 1–5 proven red/green | 5/5 invariants, each red on a mutation proven landed and green on the proven restore (commit `da1d8a75` body); under `//crates/ocx_cli:ocx_cli_seam_test` the poisoned `HOME`/`XDG_CONFIG_HOME`/`OCX_HOME` fail an ambient read: with the hermetic user-tier branch disabled, 39 of 44 seam tests red, green on restore (the poison's shape is itself checked by a presence test) | **MET** |
| 2 | 100 % of pilot ports mutation-proven, evidence committed | 37/37 landed ports, one mutation each, per-case rows in `port_evidence/pilot.md`; an independent auditor's own mutations killed 8/8 of its sampled landed cases (11/12 across its whole sample, the survivor in a port that did not land) | **MET** |
| 3 | Median agent cost per ported case, both models | Recorded below. A median needs per-case cost, and an agent's usage arrives as one total, so the figure is the mean (total ÷ cases). Every case was ported in one session, so no per-case split exists to take a median over | **NOT MET as written** (mean recorded; median unavailable) |
| 4 | ≥ 300 port-capable cases below `ocx_cli` | **195** (WP-10, collected cases; L1 error bar ~150–235). The pilot says the real number is lower still (below) | **NOT MET** |

### Criterion 4, re-read with the pilot's numbers

The classification is an estimate from reading. The pilot measured what actually ports. Of the 68
pilot cases classified as targeting a crate *below* `ocx_cli` (`test_config_setup` 13,
`test_execution_record_standards` 16, `test_platform_pairs` 12, `test_config_test` 27):

- **35 landed**, and **34 of those 35 landed in `ocx_cli`**, not below it. `ocx config test`'s
  properties are the env/tier composition and report rendering inside the command body
  (`command/config_test.rs`), not `preview_managed_config`. The `--platform` pair checks are
  clap-parse properties of the CLI. Only `test_wasm_target_claims_no_binaries` landed below
  (`ocx_package::bin_scan`).
- **33 did not land.** 16 need a real `exec` record or the Python reference validators. 13 are
  `config setup` cases the seam does not admit, so the ports re-implemented the command. 3 need a
  registry. 1 needed `log::` output the seam does not bridge.

So the below-`ocx_cli` yield on this sample is 1/68. Extrapolating that to 195 is not meaningful,
but the direction is: 195 is an upper bound, not an estimate.

**Measured payoff.** The 45 deleted cases took **1.77 s** of case time per acceptance run (per-case
JUnit, `target/bazel/accept/{test_status,test_config_test,test_platform_pairs}/junit.xml`, the last
full run before deletion). All three modules keep a remaining case, so their per-module `sh_test`
overhead (session start, fixtures) stays. Their ports run in 0.18 s for 44 seam cases under Bazel.
They re-run on every `ocx_cli` edit, where the acceptance modules re-ran on every
`test/bin/ocx` change. The saving is real but small, and it is the same kind of saving a wave
would buy.

## Two-model comparison (ADR Open Question 3)

Both porters got the same brief (`.tmp/hex-tiers/wp13_porter_brief.md`), the same base
(`dac88a67`, tree-identical to `da1d8a75`), and separate throwaway worktrees with the same pre-warmed `target/`. They ran in
parallel with `CARGO_BUILD_JOBS=8` each. Each output was then audited by a separate Opus
verification pass that did not write it: faithfulness per case, 12 of the auditor's own mutations,
and reproducibility of the porter's evidence.

| | Sonnet 5 | Opus 5.5 |
|---|---|---|
| Tokens (agent total) | 485,087 | 382,899 |
| Wall clock | 54.6 min | 53.5 min |
| Tool calls | 293 | 201 |
| Ported, claimed (test functions / collected cases) | 49 / 57 | 47 / 55 |
| Ported, audit `full` (functions / cases) | 32 / 33 | 37 / 45 |
| Ported, audit `partial` (functions) | 17 (3 only lacked a marker on a sibling test; 14 real gaps) | 10 |
| First-pass mutation-proof, self-reported | 53/57 cases | 37/47 functions (47/47 red on first mutation, assertions unchanged) |
| First-pass, audit-supported | **not supported**: the log is not per case, its timestamps run backwards, and two reworks appear only in scratch logs | **supported**: drafts match the final code; mutation driver logs 34/34 `target_red=True restored=True` |
| Auditor's own mutations killed | 7/12 | 11/12 |
| Evidence rows reproducible (file:line + change + failing line) | **0/57**. Rows name functions and parameters that do not exist in 3 modules. The porter's own log shows two platform ports green under the mutation its evidence calls red | 12/12 sampled |
| Mutations batched | yes: `config_test` "mutation 2" applied ~11 cuts across 5 files in one run, so 14 reds cannot be attributed | no, one at a time with byte-exact restore |
| Landable as-is | no | the 37 `full` functions, yes |
| Cost at list price, whole run (in/out split not reported: all-input … all-output) | $0.97 … $4.85 | $1.53 … $7.66 |
| Mean cost per claimed ported case | 8,510 tok = $0.017 … $0.085 | 6,962 tok = $0.028 … $0.139 |
| Mean cost per audit-`full` case | 14,700 tok = $0.029 … $0.147 | 8,509 tok = $0.034 … $0.170 |
| Mean cost per landable, mutation-proven case | undefined: 0 landable without redoing every proof | 8,509 tok = $0.034 … $0.170 |
| Wall clock per claimed ported case | 0.96 min | 0.97 min |

Prices are the ADR's (Sonnet 5 $2/$10, Opus 5.5 $4/$20 per MTok). Overheads outside both columns:
the seam build (Opus, 491,034 tok, 32.9 min) and the two audits (Opus, 257,205 and 260,664 tok,
about 11–12 min each).

**Which port landed.** The plan's rule is first-pass rate, then cost. Sonnet's self-reported rate
(53/57) is higher, but no evidence supports it and it cannot be reproduced: the rows were
reconstructed after the fact, some describe tests that do not exist, and mutations were batched.
The deletion guard retires the only coverage of a behaviour, so an unverifiable first-pass claim is
treated as no first-pass evidence. On the audited numbers, Opus leads on every axis except raw
tokens per claimed case.

### OQ3 recommendation

**Porting workers are Opus, and every wave carries an independent Opus audit.**

- Sonnet's failure is the disqualifying kind for a task that deletes tests. The evidence did not
  correspond to what ran, and 5 of 12 independent mutations survived its ports. The token saving
  (about 27 % more tokens than Opus for a 2× cheaper rate) disappears once the proofs have to be
  redone.
- CLAUDE.md's rule ("if Sonnet falls short twice on the same port, porting is Opus work") needs a
  second failure. It is not the binding reason here. The binding reason is that a porting task is
  exactly its "non-mechanical" class: which assertions matter, which code path a mutation must
  hit, and whether a lower API really carries the property.
- The audit is not optional even with Opus. It caught 10 partial Opus ports and one survivor that
  the porter's own proofs had passed.

## What the pilot changes in the rubric (C-RUBRIC is the "starting contract")

1. **Composition in the command body is `ocx_cli`-targeted.** If the asserted value is assembled
   in `command/<verb>.rs` (env tiers, report rendering, exit classification), the target is
   `ocx_cli` via the seam, even when a lower API computes part of it. The classification
   attributed 27 `config test` cases below `ocx_cli` this way.
2. **A port that re-implements the command in the test is not a port.** The `config_setup` ports
   copied `ConfigSetupArgs::execute` and skipped its exit-code mapping. Unadmitted verbs stay
   e2e until the seam admits them.
3. **`log::` diagnostics are invisible to the seam.** "Warns / does not warn" cases stay e2e until
   `run` bridges the `log` facade, which is a process global (C-SEAM invariant 4).
4. **Reference-validator cases are R8.** A case whose oracle is a third-party validator in Python
   (in-toto protobuf bindings, the ECS/OTel vocabulary) needs that validator in Rust (a new
   dependency) or stays.
5. **Seam tests have a co-residence cost.** A seam invariant that inspects process globals
   (`tracing::dispatcher::has_been_set`) fails in a shared test process whose other tests install
   those globals. It runs only in the seam's own Bazel target, and `ocx_cli_test` skips it.

## What stays in the tree

- The seam (`crates/ocx_cli/src/app/seam.rs`, testing-gated) and its Bazel twin target.
- The 37 ports and their `// ported-from:` markers.
- The unmarked `status` drift companion test.
- `test/SUITE_FLOOR` 3400, suite census 2885 / 147.
