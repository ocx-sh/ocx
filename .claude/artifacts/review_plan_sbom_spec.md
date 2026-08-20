# Spec review — `plan_sbom_attestations.md`

Focus: SPEC (plan-artifact fitness to execute the ADR). Reviewer: opus.
Scope: `.claude/state/plans/plan_sbom_attestations.md` (210 lines) vs
`.claude/artifacts/adr_sbom_attestations.md` (commit e7f30cd2).
The design record itself is out of scope — only the plan's decomposition of it.

## Mechanical check results

| # | Check | Result |
|---|---|---|
| 1 | Traceability C-001..C-021 → WP Scope | 21/21 mapped |
| 1 | Traceability S-001..S-015 → WP Scope | 15/15 mapped (WP10 carries all end-to-end) |
| 1 | Test surface per ID | Covered by the blanket WP-execution clause (`plan:139-141`); **F-3** is the one enforcement gap |
| 2 | Within-wave file disjointness | Holds in all six waves — no two same-wave WPs share a file |
| 2 | ADR Affected Code Surfaces → exactly one WP | 35/35 rows owned, but three WP file sets are untrue: **F-1**, **F-2**, **F-5**, **F-7**; **F-8** is a double-ownership |
| 3 | Review budgets | **F-6** — WP2 `light` on a wire-format change |
| 4 | Contracts testable / stub surface unambiguous | **F-2** (WP0 has no file set for the flag rename), **F-3** (no WP owns the builder-pin enforcement), **F-5** (WP6 stub surface incomplete) |
| 5 | Dependency/wave correctness | **F-4** — table omits two edges, graph names one wrong WP |
| 6 | Status block dual-schema | Both schemas structurally satisfied; **F-9** — the values contradict each other |

Verdict: **Needs Work** — 3 blockers, 6 lower-severity actionable, 1 deferred.
Waves 2-6 and the C-/S- decomposition are sound; every blocker sits in WP0 or
in an unassigned enforcement site.

## Actionable findings

### F-1 (blocker) — WP0 cannot compile inside its declared file set

`plan_sbom_attestations.md:82` (WP0 expected files). WP0 renames the Rust
identifier `RekorUnavailable` → `TransparencyLogUnavailable` on
`VerifyErrorKind`, `SignErrorKind`, `ExitCode` and `ErrorCategory`. Three files
carry **live construction sites** of those variants and are absent from WP0's
file set:

| Site | Code | Owned by |
|---|---|---|
| `crates/ocx_lib/src/oci/verify/pipeline.rs:549` | `None if ctx.offline => return Err(VerifyErrorKind::RekorUnavailable),` | WP5 (wave 2) |
| `crates/ocx_lib/src/oci/verify/pipeline.rs:1048` | `failure_rank(&VerifyErrorKind::RekorUnavailable)` | WP5 (wave 2) |
| `crates/ocx_lib/src/oci/sign/bundle.rs:110` | `.ok_or(SignErrorKind::RekorUnavailable)?` | WP7 (wave 3) |
| `crates/ocx_lib/src/oci/sign/bundle.rs:229` | `matches!(err, SignErrorKind::RekorUnavailable)` (test) | WP7 (wave 3) |
| `crates/ocx_lib/src/oci/sign/rekor.rs:162` | `.map_err(\|_\| SignErrorKind::RekorUnavailable)?` | WP7 (wave 3) |
| `crates/ocx_lib/src/oci/sign/rekor.rs:191` | `classify_upload_status` return | WP7 (wave 3) |

Renaming the variant declaration without these is a compile error. WP0's own
gate (`plan:189` — "`cargo fmt` + `task` (fast gate)") fails, and per the merge
plan (`plan:128-131`) wave 1 merges onto `feat/sbom-attestations` before wave 2
branches, so the integration branch is red after wave 1.

Two further stale doc-comment references are owned by **no WP**:
`crates/ocx_lib/src/package_manager/tasks/auto_verify.rs:301` and
`test/tests/fixtures/adversarial.py:207`.

Root cause: WP0's file set was transcribed from the ADR's rename table
(`adr_sbom_attestations.md:1590-1607`), which is itself an incomplete census —
it lists 12 files where `grep` finds 14.

**Fix:** add `crates/ocx_lib/src/oci/verify/pipeline.rs`,
`crates/ocx_lib/src/oci/sign/bundle.rs`, `crates/ocx_lib/src/oci/sign/rekor.rs`,
`crates/ocx_lib/src/package_manager/tasks/auto_verify.rs` and
`test/tests/fixtures/adversarial.py` to WP0's expected files. Because WP5 and
WP7 later edit three of them, extend the declared stub/implement exception in
`plan:128-133` to name the WP0→WP5 and WP0→WP7 file sharing as sequential. Also
correct the ADR rename table in the same change, since `plan:24-26` makes the
ADR authoritative on disagreement.

### F-2 (blocker) — the `--sigstore-trusted-root` rename has no file set, and breaks the acceptance suite

`plan:82` lists the WP0 entry as the bare phrase "`--sigstore-trusted-root` flag
rename sites" — no file is named, so the WP has no executable file set for this
half of C-019. The real sites:

- Flag definition: `crates/ocx_cli/src/command/package_verify.rs:107`
  (`#[clap(long = "trusted-root", ...)]`), plus doc comments at `:21`, `:23`,
  `:98`, `:253`, `:255`. **That file is WP9's (wave 5).**
- Library doc comments, owned by **no WP**: `crates/ocx_lib/src/oci/verify.rs:13`,
  `crates/ocx_lib/src/oci/verify/trust_resolve.rs:15`, `:18`, `:68`.
- Acceptance suite, owned by **no WP** — these build the flag, so the suite
  fails the moment the rename lands: `test/tests/fixtures/sigstore_stack.py:89`
  (the shared `verify_args` builder every signing test routes through),
  `test/tests/test_verify.py:59`, `test/tests/test_trust_policy.py:64`,
  `test/tests/test_offline_verify.py:200` (asserts the flag name in stderr).
  Docstrings at `sigstore_stack.py:50`, `test_verify.py:10`,
  `test_offline_verify.py:57`, `test_trust_root_distribution.py:10`,
  `test/sigstore/README.md:12,31,52`.
- **Must NOT be renamed:** `test/tests/test_cosign_interop.py:94,99,145` spell
  `--trusted-root` as *cosign's* flag. A mechanical rename corrupts the interop
  test — the plan needs to say so, because this is precisely the site an
  executing agent would sweep.
- Docs: `website/src/docs/reference/environment.md:222,230`,
  `website/src/docs/reference/command-line.md:3803,3805,3829,3844` — the
  ADR's Documentation Surfaces table names environment.md and command-line.md,
  but the plan routes those to WP11 (wave 6), five waves after the flag changes.

**Fix:** enumerate the file set explicitly in WP0, add the four `test/tests/`
files and `test/sigstore/README.md`, add the two `ocx_lib` doc-comment sites,
add an explicit "do not touch `test_cosign_interop.py` — that is cosign's flag"
note, and either move the two doc rows into WP0 or accept a documented
five-wave window where the published flag reference contradicts the binary.

### F-3 (blocker) — the builder pin (#103 / S-013) has no enforcement WP, and `BuilderMismatch` is uncounted

`plan:16-17` lists #103 (builder pin) as delivered by this milestone. The ADR
requires a refusal variant for it: `adr_sbom_attestations.md:769` —
"fails with `BuilderMismatch { expected, found: Option<String> }`", and
`:767` makes fail-open explicitly wrong ("Absent or unparseable is a refusal,
never a skip"). `S-013` (`plan:74`) restates it: "absent builder field with pin
→ refusal `builder_mismatch`".

Two gaps:

1. **No WP owns the enforcement.** WP1 scope is `C-020, S-013(parse)` — config
   parse only. WP4 owns `builder_id()` extraction via C-005. WP10 owns the
   acceptance test. Nothing between them owns the comparison of the parsed pin
   against `builder_id()`, nor the raise. `verify/pipeline.rs` (WP5) or
   `verify/dsse.rs` (WP6) is the natural home, but neither scope cell names it.
2. **The variant is uncounted.** `plan:52` (C-017) says "16 new
   `VerifyErrorKind` variants + slugs + pinned rows (23→39)". The ADR's variant
   table (`:1539-1556`) also lists exactly 16 and does **not** contain
   `BuilderMismatch`. Confirmed absent from the tree today
   (`grep -rn 'BuilderMismatch' crates/` → no match; `trust.rs` has no `builder`
   field). On the ADR's own arithmetic the counts become 17 variants and
   23→40 pinned rows.

**Fix:** add `BuilderMismatch` to C-017 (17 variants, 23→40) in WP0, and give
the enforcement an owner — extend WP6's scope to `C-011 C-012 S-013(enforce)`
with `oci/verify/pipeline.rs` already in its file set. Note the ADR is
self-inconsistent here (Part IV D-j vs the Error Variants table), so the ADR
needs the same correction.

### F-4 — WP8's `Depends` cell contradicts the mermaid graph, and one graph edge names the wrong WP

`plan:90` gives WP8 `Depends = WP6 WP7`. The graph at `plan:120-121` adds
`WP1 --> WP8` and `WP3 --> WP8`. Two separate defects:

- **WP1 → WP8 is real and missing from the table.** `SbomOptions.policies:
  &'a [CompiledPolicy]` (`adr:1338`) — WP8's facade consumes the type WP1
  reshapes under C-020.
- **WP3 → WP8 is wrong; it should be WP3 → WP9.** `SbomReport` carries no
  summary field (`adr:1349`), `--summary` is a CLI flag (`adr:1436`), and the
  summary lands in the DTO as `Option<SbomSummaryOut> // populated only under
  --summary` (`adr:1491`). So `summarize_cyclonedx` is called from WP9's
  `package_sbom.rs`, not from WP8's `tasks/sbom.rs`.

No wave breaks either way (WP1/WP3 are wave 1, consumers are waves 4-5), so this
is a correctness-of-record defect, not a scheduling one — but `/hex-execute`
reads the table, not the graph.

**Fix:** WP8 `Depends: WP1 WP6 WP7`; WP9 `Depends: WP3 WP8`; redraw the graph
edge `WP3 --> WP9`.

### F-5 — WP6's file set omits `oci/verify/tlog.rs`, and the "single deliberate exception" claim is false

`plan:88` gives WP6 the files `oci/verify/dsse.rs` (implement) and
`oci/verify/pipeline.rs` (wire candidate step). But WP6's scope includes C-012,
and C-012 (`plan:47`) is explicitly two-sited: "`verify/dsse.rs::verify_tlog_binding`
… **+ `verify/tlog.rs` row-13 validity re-assertion, both modes**". The ADR puts
`verify_integrated_time_within_certificate` in `tlog.rs` and says so in as many
words (`adr:1257-1274`: "Row 13's re-assertion is **not** in `dsse.rs`, because
it must run for both content modes"). `tlog.rs` appears only in WP5's file set
(`plan:87`), whose scope is C-010 plus stubs.

Separately, `plan:131-133` claims "the single deliberate exception: WP5 stubs
`verify/dsse.rs` signatures which WP6 implements". WP5 and WP6 in fact share
**two** files — `verify/dsse.rs` and `verify/pipeline.rs` — and would share a
third once `tlog.rs` is added. Both shares are cross-wave and therefore
sequential, so nothing races; the claim is simply inaccurate, and an inaccurate
disjointness claim is what stops the next reader from re-checking it.

**Fix:** add `crates/ocx_lib/src/oci/verify/tlog.rs` (implement) to WP6's
expected files, and reword `plan:131-133` to "WP5 stubs the `verify/dsse.rs` and
`verify/tlog.rs` signatures and wires `verify/pipeline.rs`; WP6 implements all
three in the next wave — sequential, so no concurrent writer."

### F-6 — WP2 carries a `light` review budget on a wire-format change

`plan:84`. WP2's scope is C-002 + C-021. C-002 adds
`ReferrerManifest.annotations` with `skip_serializing_if`, and the ADR states
the consequence outright (`adr:913-919`): `to_canonical_json` is
`serde_json::to_vec(self)` and "the registry addresses the referrer by the
SHA-256 of exactly those bytes. Without it every manifest built by a caller that
passes `None` gains `"annotations": null` and changes digest." That is
contract weight 1 under `reviewing-a-diff.md` — an on-disk/wire format, where a
mistake is unrecallable because already-published signature referrers must keep
resolving (CLAUDE.md, "metadata and OCI manifest changes stay backward
compatible on the read path"). C-021 additionally changes a public report DTO
(`PushOutcome` + `#[non_exhaustive]`), and the WP edits
`crates/ocx_lib/Cargo.toml` to add the `raw_value` feature, which unifies across
the whole graph.

A `light` budget on the one WP that can silently change the digest of every
existing signature referrer is a false economy.

Secondary, weaker: WP3 (`plan:85`, also `light`) is the CycloneDX reader over
attacker-supplied JSON, and the ADR names a crash-class hazard for it
(`adr:1301-1304`: "the default recursion limit is the only thing between a
hostile document and a stack overflow — which is a crash, not a caught error").
The requirement there is an *absence* (nothing turns `unbounded_depth` on),
which is exactly the property a light pass does not check.

**Fix:** WP2 `Review: panel`. For WP3, either `panel` or `light` plus an
explicit security perspective.

### F-7 — module-declaration files are missing from three WP file sets

Adding a module requires editing its parent aggregator. Verified against the
tree: `crates/ocx_lib/src/lib.rs:33-61` has no `sbom` row, and
`crates/ocx_lib/src/oci.rs:46-103` has no `attest` row.

| WP | New module | Aggregator that must be edited | In WP file set? |
|---|---|---|---|
| WP3 | `sbom.rs` | `crates/ocx_lib/src/lib.rs` | no |
| WP4 | `oci/attest.rs` | `crates/ocx_lib/src/oci.rs` | no |
| WP8 | `oci/attest/pipeline.rs` | `crates/ocx_lib/src/oci/attest.rs` (WP4's file) | no |

WP8 and WP9 do list their aggregators (`tasks.rs`, `api/data.rs`), so the
omission is inconsistent rather than a convention. No wave conflict arises (the
pairs are cross-wave or single-owner), and a builder hits an immediate compile
error, so this is low severity — but it makes three file sets untrue.

**Fix:** add `crates/ocx_lib/src/lib.rs` to WP3, `crates/ocx_lib/src/oci.rs` to
WP4, and `crates/ocx_lib/src/oci/attest.rs` (mod row only) to WP8.

### F-8 — WP0 and WP11 both own the three `command-line.md` slug rows

`plan:82` gives WP0 "`website/…/command-line.md` (3 rows)". `plan:93` gives WP11
"ADR Documentation Surfaces table (all rows)", and that table's command-line.md
row (`adr:1669`) explicitly includes "**Plus three existing slug rows** —
`:3770`, `:3778`, `:3934`". So WP11 is instructed to redo work WP0 already did,
five waves later. Cross-wave, so no conflict — but a second agent re-editing
three already-correct rows is exactly where a regression gets introduced.

**Fix:** add "(slug rows `:3770`/`:3778`/`:3934` already done in WP0 — do not
re-edit)" to WP11's scope cell.

### F-9 — the Status block's two schemas contradict each other

`plan:3-12`. Both required schemas are structurally present — project four-field
(`Plan`, `Active phase`, `Step`, `Last update`) and hex (`State`, `Tier`,
`Updated`, `Next`). The values disagree:

- `State: planning` (line 9) against `Step: /hex-execute → Stub` (line 7).
  `planning` precedes `plan-approved` in the hex enum on the same line; a plan
  still in `planning` cannot be in the Stub stage of execution. Every WP `Status`
  cell reads `pending`, which corroborates "not started".
- `Step: /hex-execute → Stub` is not in the allowed-value list in
  `.claude/rules/meta-ai-config.md` "Plan Status Protocol", which enumerates only
  `/swarm-*` steps. `~/.claude/skills/hex-execute/` exists, so the *skill* is
  real and the reference is valid — the project rule's enumeration is what has
  not caught up with the hex skill family.

**Fix:** set `State: plan-approved` and `Step: awaiting /hex-execute` until the
Stub stage actually begins. Separately, extend the allowed `Step` values in
`meta-ai-config.md` with `/hex-execute → <stage>` and `/hex-review → round N`,
or state that the hex family reuses the `/swarm-*` spellings.

## Deferred

### D-1 — is the `rekor_unavailable` → `transparency_log_unavailable` rename in scope at all for milestone 4?

Reason: human judgment needed on whether a pre-release rename touching 66
occurrences across 14 files belongs in the same branch as the SBOM feature.
D3 (`adr:137`) says pre-release renames land first, and the plan honours that
by making it WP0 — but F-1 and F-2 show the true blast radius is roughly double
what the ADR's rename table records, including the whole acceptance-suite arg
builder. Splitting the two renames into their own merged PR ahead of the
milestone would make waves 1-6 read against a stable vocabulary. This is a
scope decision, not a defect.
