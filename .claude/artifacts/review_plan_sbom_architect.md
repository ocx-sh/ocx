# Plan review — ARCHITECT lens (trade-off honesty)

Target: `.claude/state/plans/plan_sbom_attestations.md` (12 WPs, 6 waves)
Scope: plan decomposition only. ADR (`adr_sbom_attestations.md`, e7f30cd2) treated as settled.
Verdict: **approve with required corrections** — no design defect; the file sets are
under-declared in ways that break handovers, and three WPs have real re-cuts.

Severity: **Block** = a builder handed this WP cannot compile. **Warn** = rework or
lost width. **Note** = accuracy.

---

## F1 — Block. WP0's file set under-declares the rename by 6+ files, which falsifies the merge-plan claim

Plan line 82 (WP0 file list); plan lines 130-133 ("conflicts are impossible within a
wave by file-disjointness — verified per WP file sets").

`RekorUnavailable|rekor_unavailable` occurs in **12 files**; WP0 names 7.
Missing, with occurrence counts:

| File | Hits | Owned by |
|---|---|---|
| `crates/ocx_lib/src/oci/sign/rekor.rs` | 8 | **WP7** (wave 3) |
| `crates/ocx_lib/src/oci/sign/bundle.rs` | 5 | **WP7** (wave 3) |
| `crates/ocx_lib/src/oci/verify/pipeline.rs` | 7 | **WP5** (wave 2) |
| `crates/ocx_lib/src/package_manager/tasks/auto_verify.rs` | 1 | nobody |
| `test/tests/fixtures/adversarial.py` | 1 | nobody |

The flag half is worse. `--trusted-root` is **defined** at
`crates/ocx_cli/src/command/package_verify.rs:107` — a **WP9** file, wave 5 — with doc
references in `oci/verify/error.rs:318,342`, `oci/verify/trust_root.rs:23,206`,
`oci/verify/trust_resolve.rs:15,68`, `package_manager/tasks/auto_verify.rs:79,110`,
`crates/ocx_cli/src/app/context.rs:984`. WP0's list covers this with the phrase
"`--sigstore-trusted-root` flag rename sites", which names no file.

Sequential waves make every one of these merge-safe (WP0 lands first; WP5/WP7/WP9 branch
from a renamed tree). The defect is not conflict — it is that the disjointness claim was
never actually verified against the tree, and a builder given WP0's list lands a
half-rename that does not compile.

**Fix:** enumerate the full set in WP0's Expected-files cell, and add one line to the
merge plan: "WP5, WP7 and WP9 inherit files WP0 renamed; they add no rename work."

---

## F2 — Warn. The WP5/WP6 seam shares three files, declares one, and puts both WPs inside the same function

Plan lines 87-88; plan lines 131-133 ("the single deliberate exception: WP5 stubs
`verify/dsse.rs`").

Three shared files, not one:

1. `verify/dsse.rs` — declared. Correct: WP5's `run_attestations` cannot compile without
   the signatures, and contract-first stubbing is the project's own pattern.
2. `verify/pipeline.rs` — **undeclared**. WP5 rewrites `verify_one_referrer`
   (`pipeline.rs:330`) for mode selection, cap selection and the mode-mismatch skip; WP6
   then "wires the candidate step" in that same function. Guaranteed rework: WP5 writes a
   mode branch around a stub, WP6 rewrites the arm.
3. `verify/tlog.rs` — **undeclared**. WP5 owns the file, but C-012 (WP6) adds
   `verify_integrated_time_within_certificate` to it (ADR lines 1260-1274).

`verify/pipeline.rs` is 1599 lines today; both WPs are L partly because of it.

**Recommended re-cut (region split, not file split):**

- **WP5** keeps only the mode-agnostic half — `VerifyContentMode` on `VerifyContext`,
  cap selection by mode, `from_bundle(bundle, mode)`, and the `tlog.rs` widening
  (row-13 fn, which runs for *both* modes and therefore genuinely belongs to whoever
  owns tlog.rs). Drops **L → M**.
- **WP6** takes the whole candidate-loop rework: `run_attestations`, the attestation arm
  of `verify_one_referrer`, and all of `dsse.rs`.

Two WPs still touch `pipeline.rs` sequentially, but each now owns a disjoint *region*
rather than the same function twice.

**If the seam is kept as-is**, two things are mandatory: add `verify/tlog.rs` to WP6's
file list, and put this sentence verbatim in WP5's handover — *"the attestation arm of
`verify_one_referrer` belongs to WP6; leave it `todo!()` and do not shape it."*

---

## F3 — Warn (width win). WP8 splits into two wave-4 WPs; the one shared file merges clean

Plan line 90 (`WP8 pipelines + manager`, L, wave 4, width 1).

The two halves have disjoint dependency sets, verified against the ADR:

- **Attest-side** — `oci/attest/pipeline.rs` (new) + `package_manager/tasks/attest.rs`.
  C-009's `AttestPipeline::run` consumes `Signer::sign_dsse` (C-006),
  `rekor::upload_dsse_entry` (C-007), `build_dsse_bundle` (C-008) — all **WP7** — plus
  statement/predicate from **WP4**. Touches nothing under `oci/verify/`.
- **SBOM/verify-side** — `tasks/sbom.rs` + `tasks/verify.rs` (`VerifyOptions.content`).
  C-014's `sbom_one -> SbomReport { attestations: Vec<AttestationMatch> }` needs
  `AttestationMatch` (**WP5/WP6**) and the CycloneDX reader (**WP3**). Touches nothing
  under `oci/sign/` or `oci/attest/`.

Both land in wave 4 (WP7 and WP6 are both wave 3). Only shared file:
`package_manager/tasks.rs`, one `pub(crate) mod` row each. `attest` sorts before
`auto_verify` (line 13); `sbom` sorts between `resolve` (line 36) and `select` (line 37)
— 23 lines apart in a 42-line file, so git merges both without a conflict hunk.

**Wave 4 goes width 1 → width 2 at no disjointness cost.**

---

## F4 — Warn (width win + risk reduction). Pull `package_sign_common.rs` out early; WP9 then splits disjoint

Plan line 91 (`WP9 CLI + DTOs`, L, wave 5, width 1) — the largest WP in the plan.

**Size first.** WP9 is XL, not L. Reference sizes for the files it mirrors:
`command/package_sign.rs` 465, `package_verify.rs` 320, `package_push.rs` 356,
`api/data/signature.rs` 379, `api/data/verification.rs` 333. A `package_attest.rs`
mirroring `package_sign.rs`, a `package_sbom.rs` with `--output`/`--summary`/TTY
refusal, and two DTOs is ~1400 new lines *plus* three file modifications *plus* a
security-critical extraction.

**The extraction is the lever.** `command/package_sign_common.rs` (token resolver +
offline refusal, ADR lines 1407-1413: "security-critical and must not fork") depends on
**nothing** in this milestone — it is a pure refactor of `package_sign.rs`, and WP0's
rename does not touch that file. It can run in **wave 1 or 2** as an **S** WP, off the
critical path, with its own standalone security review. That is the single highest-value
re-cut in this document: the plan currently buries a must-not-fork credential path inside
the largest, latest, most time-pressured WP.

**`iso8601` must move with it.** `SbomEntry.signed_at` is RFC-3339-Z (ADR line 1490), but
`api/data/verification.rs:40` already stores `signed_at` as a `String` — the u64→RFC-3339
conversion is a *private free function* at `command/package_verify.rs:305`, not a
reusable DTO-layer helper. ADR line 1218 says the conversion "already exists on
`VerificationReport`'s path and is reused"; that is true of the code but not of its
location. Move `iso8601` into `package_sign_common.rs` or WP9's two halves acquire a
cross-file dependency on each other.

**With the extraction pulled out, WP9 splits file-disjoint at wave 5:**

- **WP9a** — `command/package_attest.rs`, `command/package_push.rs`,
  `api/data/attestation.rs`, `api/data/push.rs`
- **WP9b** — `command/package_sbom.rs`, `command/package_verify.rs`, `api/data/sbom.rs`

Aggregators merge cleanly: in `command/package.rs`, `Attest` inserts after `Announce`
(line 20) and `Sbom` after `Push` (line 46) — 26 lines apart, with match arms similarly
separated; in `api/data.rs`, `attestation` after `announce` (line 5) and `sbom` after
`removed` (line 31) — 26 lines apart.

**Wave 5 goes width 1 → width 2**, and the largest WP drops to two M-sized ones.

---

## F5 — Warn. WP10 is XL and splits file-disjoint at no cost

Plan line 92.

`test_sign.py` is 764 lines and `test_verify.py` is 505 today. WP10 adds two new files
covering 15 S-IDs — including S-009's seven distinct negative kinds, each asserting kind
and exit separately — plus four fixture artifacts. Realistically 800-1200 new Python
lines. That is XL against the same L label as WP2 (four files, S).

**Split by file, no dependency introduced:**

- **WP10a** — `test/tests/test_attest.py`, `test_sbom.py` (both new)
- **WP10b** — `test/tests/test_verify.py`, `test_sign.py` (S-009 kinds, S-014 renames)

Fully disjoint, both wave 6, width 2 alongside WP11 → wave 6 width 3.

Fixtures are the only shared surface, so give them a **third slot at wave 5**
(`test/tests/fixtures/` additions only): they are pure ADR Part III data and need no CLI
surface at all, so the plan's own "tests need the CLI surface real" justification does
not cover them. Doing so also settles them one wave after the spike (ADR line 1803, "the
spike MAY adjust") rather than two.

---

## F6 — Note (no action). Do not split WP11 — this is the one case where the narrow cut is a real constraint

Plan lines 93, 135-138.

Steelmanned and rejected. Threat-model prose and the `.claude/rules/` updates are
ADR-derivable and could in principle start at wave 5. But both halves of any WP11 split
would edit `website/.vitepress/config.mts` — the threat-model row goes into the collapsed
"In Depth" items list, the two user-guide rows into the flat sibling list (plan lines
181-185). Unlike every other shared file flagged in this review, those two writers would
be **in the same wave**: a genuine concurrent-writer conflict, not a sequential handoff.
Keep WP11 whole. The plan's width justification is correct here.

---

## F7 — Block. WP5 needs the `raw_value` feature and has no dependency edge to WP2

Plan line 87 (`WP5 ... Depends: WP0`); plan line 115 (`WP0 --> WP5`).

`VerifiedAttestation.predicate: Box<serde_json::value::RawValue>` (ADR line 1203) lives
in `oci/verify/pipeline.rs` — WP5's file. `crates/ocx_lib/Cargo.toml:50` currently reads
`serde_json = { workspace = true, features = ["preserve_order"] }`; `raw_value` is absent
and **WP2** adds it (plan line 84).

WP2 is wave 1 and WP5 is wave 2, so the merged tree happens to carry it — but the
declared DAG is wrong, and it breaks the moment wave order is revised or WP5 is developed
against a WP0-only base.

**Fix:** add `WP2` to WP5's Depends cell and `WP2 --> WP5` to the mermaid graph.

**Second, for WP2's handover:** `raw_value` must be **added to** the existing features
list, never replace it. `preserve_order` at that line carries a documented
feature-unification decision (`Cargo.toml:42-50`, referencing CONTRACTS §14) that a
"tidy the features list" edit would silently revert.

---

## F8 — Block. Three module-declaration edits are in no WP's file list

Each is a single line and each is a compile blocker for its WP.

| Needed edit | File | WP that needs it | Currently listed? |
|---|---|---|---|
| `pub mod attest;` | `crates/ocx_lib/src/oci.rs` | WP4 (plan line 86) | no |
| `pub mod sbom;` | `crates/ocx_lib/src/lib.rs` | WP3 (plan line 85) | no |
| `mod dsse;` | `crates/ocx_lib/src/oci/verify.rs` | WP5 (plan line 87) | no |

The `sbom` one is the easy mistake to make in the other direction: C-013 puts `sbom.rs`
at the **crate root** ("no oci dep", plan line 85), so `oci.rs` is the wrong aggregator
even though every sibling in this milestone lives under it. `oci/verify.rs` already
declares `mod tlog;` at line 32, so the `dsse` row goes beside it.

All three are single-owner — no conflict, just missing from the handovers.

---

## Confirmations (checked, no finding)

- **WP2 → WP4 `raw_value` edge exists.** Plan lines 84, 86, 115. C-004's
  `Statement { predicate: Box<RawValue> }` needs the feature; the edge is declared. Only
  WP5's edge is missing (F7).
- **`tasks.rs`, `api/data.rs`, `command/package.rs` are single-WP-owned as declared**
  (plan lines 90, 91) — and stay conflict-free even under the F3/F4 splits, verified by
  insertion-point separation rather than by assertion.
- **`oci/verify/error.rs` (WP0) has no second writer** in the declared plan. It stays
  that way only if the ADR's 16 `VerifyErrorKind` variants are complete before wave 3 —
  a gap found by WP6 means editing a wave-1 file from wave 3. Sequential, so safe, but
  worth naming as the one thing WP0 must get exhaustively right rather than approximately.
- **Wave 6 gating on WP9 is correct.** Casts genuinely need a real binary
  (`test/recordings/` scripts drive `test/bin/ocx`), and `command-line.md` rows are
  `--help`-derived. No re-cut proposed.
- **Schema regen timing is fine.** WP1 (wave 1) owns the one `ocx_schema` regen; nothing
  in waves 2-6 changes config shape (`--sigstore-trusted-root` is a flag, `[trust.sigstore]`
  is untouched). Worth adding `task schema` to the per-WP worktree bootstrap line
  (plan lines 139-142) since every fresh worktree needs it, not just WP1's.

## Note on the brief

The review brief says "five WPs are marked L". The table (plan lines 82-93) marks
**seven**: WP0, WP4, WP5, WP6, WP8, WP9, WP10. Of those, F2/F4/F5 argue WP9 and WP10 are
XL and WP5 should be M.

---

## Recommended wave geometry after the re-cuts

| Wave | Current | After F3/F4/F5 |
|---|---|---|
| 1 | WP0, WP1, WP2, WP3 (4) | + WP9-pre (`package_sign_common.rs`, S) → 5 |
| 2 | WP4, WP5 (2) | WP4, WP5 (M after F2) → 2 |
| 3 | WP6, WP7 (2) | WP6 (L after F2), WP7 → 2 |
| 4 | WP8 (1) | WP8a, WP8b → **2** |
| 5 | WP9 (1) | WP9a, WP9b, WP10-fixtures → **3** |
| 6 | WP10, WP11 (2) | WP10a, WP10b, WP11 → **3** |

Critical path is unchanged (WP0 → WP4 → WP6 → WP8b → WP9b → WP10b), so this buys
throughput and review quality, not schedule. The schedule win is F4's: the
must-not-fork credential extraction leaves the critical path entirely.
