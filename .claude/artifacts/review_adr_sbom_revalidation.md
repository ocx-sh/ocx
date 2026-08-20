# Re-validation — adr_sbom_attestations.md (post batch-fix, commit c456de7c)

Focus: SPEC. Three passes: (1) R1–R20 resolution closure, (2) regression sweep
over the ~950 rewritten lines, (3) verification of newly introduced factual claims.

Inputs: `review_adr_sbom_resolutions.md` (the directive), the three panel reviews,
the worktree source tree, `discover_attest_architecture_map.md`, `research_*.md`.

## Verdict

**NEEDS WORK** — 4 actionable findings (2 block-tier), 3 nits. R1–R20 all landed;
the defects are in text the fix pass *introduced*, not in resolutions it skipped.

## Pass 1 — Resolution closure (R1–R20)

All twenty resolutions are implemented, not merely mentioned. Code claims
spot-checked against the worktree; every cited `file:line` below resolved exactly.

| R | Where implemented | Code claims verified |
|---|---|---|
| R1 | L307 (rejection cell rewritten), L312–352, L1209–1213, L1229–1247, row 13 L787 | `pipeline.rs:382` `verifier.verify(subject_bytes, bundle, &PolicyDeferredToOcx, true)` ✓; `trust_root.rs:301` `impl SigstoreTrustRootTrait for TrustRoot` (alias of `sigstore::trust::TrustRoot`, :35) ✓; `pipeline.rs:248` single `Verifier::new` ✓; `verify/identity.rs:31 parse_certificate` ✓; `pipeline.rs:404` `let cert = parse_certificate(&parts.leaf_der)?;` ✓; `verify/tlog.rs` module doc scopes to SET + proof and performs **no** window check ✓. Gap (5) is FALSE → **F2** |
| R2 | L563, L581–595, L864, L1215–1220, L1879 | sigstore `models.rs:409–434` `tlog_entry_for_dsse` returns `None` for `kind != "dsse"` / `apiVersion != "0.0.1"` ✓ |
| R3 | L236–257, L776, L846–852, L942, L1180, L1288–1292, L1614, L1663, L1741–1744 | `ocx_lib/Cargo.toml:50` `serde_json = { workspace = true, features = ["preserve_order"] }`, comment names `indexmap` + workspace-wide unification ✓ (BTreeMap claim correctly deleted); root `Cargo.toml:66` bare version string ✓; `serde_json_canonicalizer` is a real workspace dep, used by `verify/tlog.rs` for RFC 8785 ✓ |
| R4 | L86–115, L433–458, L873–877, L897–898, L1606–1607, L1794 | `research_cosign_v3_attestation_wire.md:22–24` shows exactly the three keys ✓; `referrer/manifest.rs:25–46` six fields, no annotations ✓; `to_canonical_json` is `serde_json::to_vec(self)` (:80–82) so `skip_serializing_if` is genuinely load-bearing ✓. Attribution slip → **F5** |
| R5 | L416–431 | `native_transport.rs:213–243` doc says the client-side pass "is the only filtering callers can rely on"; `None => true` at :228 ✓ |
| R6 | L354–373, L1723–1724 | `MAX_SIGNATURE_CANDIDATES = 8` (`pipeline.rs:73`), `MAX_TOTAL_REFERRER_BYTES = 4 MiB` (:81), `MAX_BUNDLE_SIZE_BYTES = 512 KiB` (`sign/bundle.rs:165`) ✓ — the "512 KiB / 8 / 4 MiB" triple is exact |
| R7 | L375–384, L1168–1172, L1189–1192, L1321, L1519, L1881 | — |
| R8 | L282–291, L795, L1540, L1707, L1880 | — |
| R9 | L733–752, L1007–1015 | — |
| R10 | L676–681, L718–726 | — |
| R11 | L1138–1151, L1106, L1296, L1708 | `package_sign.rs:127–133` offline refusal short-circuits **before** `resolve_override_token` at :136 ✓ — "before the token resolver, not inside it" is exact |
| R12 | L1387–1397 | — |
| R13 | L145, L1560–1584 | `verify/error.rs:188/:395/:666`, `sign/error.rs:210/:387`, `error_envelope.rs:307`, `exit_code.rs:77`, `command-line.md:3770/:3778/:3934`, `test_verify.py:446`, `test_sign.py:737` all resolve exactly ✓. Table incomplete → **F1** |
| R14 | L1502–1526 | Counts re-derived: `VerifyErrorKind` 23 variants / 23 pinned rows, `SignErrorKind` 12 / 12 → 23+16=39 and 12+4=16 are **correct** ✓ |
| R15 | L402–405, L1318, L1338–1342, L1611 | `VerifyOptions` (`tasks/verify.rs:39–55`) has exactly **seven** fields ✓ |
| R16 | L508–527, L1046–1048, L1477, L1613, L1623 | `oci/sign/keyless.rs` does not exist, `signer.rs:50 pub struct KeylessSigner;` is a unit struct ✓; `publisher.rs:41–42` `#[derive(Debug)] pub struct PushOutcome` ✓; `Platform` (`platform.rs:65`) derives `Debug, Clone, PartialEq, Eq, Hash`, no `Ord` ✓; `PushReport` (`api/data/push.rs:26`) doc names `ocx-mirror pipeline push` and "the first five keys" ✓ |
| R17 | L533–545, L1842–1850 | `adr_oci_referrers_signing_v1.md:502–510` — S1-I chose new-referrer-each-time and explicitly rejected *replace* and *append-only-if-absent* ✓ |
| R18 | L1673–1676, L1693–1708, L1731–1744 | `test_verify.py:245` `test_verify_error_envelope_golden_shape`, `:304` `test_verify_success_envelope_golden_shape` ✓ |
| R19 | L1770–1771, L1784–1794, L1801–1805 | — |
| R20 | L623–627, L466, L1266–1276, L1634 | `trust_policy_invalid_maps_to_config_error` (`verify/error.rs:526`), `trust_config_tolerates_unknown_fields_from_newer_ocx` (`trust.rs:834`) both exist ✓ |

One soft residue on R1: the directive asked to "name the new test" for row 13's
corrected coverage cell. The ADR corrects the false "Existing coverage" claim and
names the fixture, but gives no test function name — unlike R18's
`test_attest_*_envelope_golden_shape`. Cosmetic; not a finding.

## Pass 2 — Regression sweep

Clean:

- **Counts.** 16 new verify variants (table L1511–1526 = 16 rows) matches
  L1502 "Sixteen", L1604 "16 variants + 16 slug rows". 4 sign variants matches
  L1603. Five `MAX_*` constants matches L818 / L657 / L1592 / L1851. Five
  annotation constants matches L870 / L1607. Part V MAY-adjust has 5 rows,
  matching L1776; the "five findings the security review raised" list (L1801–1803)
  enumerates five and matches R19's B2/B3/B4/B7/B8.
- **Part III row 21.** Coherent: L797–799 states rows 1–20 are the research
  checklist and 21 is OCX's own; every row 1–21 names an enforcement site that
  resolves to a real Part IV contract (row 8 excepted — F2).
- **Fixture coverage.** All 20 new variants (16 verify + 4 sign) have a negative
  fixture in the Testing Strategy table. No orphan variants.
- **No stale text.** No residual "two annotations", "two trust roots",
  `verify_prehash`-as-DSSE-primitive, `oci/sign/keyless.rs` or `oci/publish/`
  claim survives; every `intoto:0.0.2` mention is a rejection.
- **D-letter references.** D1, D2, D-a … D-j are all defined; no reference to an
  undefined section.

Defects: **F1** (rename table), **F3** (annotation narrowing stated two ways),
**F4** (docs placement vs D8), **F5** (Part V attribution).

## Pass 3 — New-claim verification

Every factual claim the fix pass introduced was checked against sigstore 0.14.0
source (`~/.cargo/registry/…/sigstore-0.14.0`) or the research artifacts.

Confirmed exact:

- `verify_bundle_content` is `pub(crate)` (`bundle/verify/verifier.rs:56`);
  `InTotoStatementV1` is `pub(crate) struct` (`bundle/intoto.rs:35`) — the
  delegation-only option is genuinely unimplementable.
- Gap (1): `validate_cosign_v1` (`bundle/intoto.rs:74`) has exactly one non-test
  call site, `cosign/signature_layers.rs:447` — not on the bundle path.
- Gap (2): `models.rs:264–267` parses `InTotoStatementV1` with no `payloadType`
  check first.
- Gap (3): `subject_sha256_digest()` is `self.subject.first()` (`intoto.rs:115`).
  Attribution slip → **F6**.
- Gap (4): `models.rs:243–244` `serde_json::to_vec(&dsse)` → `envelope_json`,
  hashed at `:466` — a re-serialization, exactly as claimed.
- The DSSE primitive correction: `verifier.rs:73–75`
  `signing_key.verify_signature(Signature::Raw(signature), pae)` — and the
  `MessageSignature` arm does use `verify_prehash` (:64), so the correction is
  precisely calibrated.
- `verifier.verify` does bind subject[0] to the input digest (`verifier.rs:78–82`)
  and does check `integrated_time < not_before || > not_after`
  (`verifier.rs:206–217`) — row 13's "two sites, deliberately" is accurate.
- CVE-2026-31830, CVE-2022-35929, CVE-2024-55655, GHSA-8gw7-4j42-w388 sourced in
  `research_dsse_verification_pitfalls.md`; CVE-2026-39395 in the slice-1
  researcher reviews; **CVE-2026-22703** (new in this pass) sourced in
  `review_adr_sbom_sota.md:41` as "CVE-2026-22703 / GHSA-whqx-f9j3-ch6m (Jan 2026):
  confirmed REGRESSION", and corroborated in-tree by `pipeline.rs:781`'s existing
  "GHSA-whqx splice" comment.

Falsified: gap (5) → **F2**.

---

# Findings

## F1 — Block — Rename table omits the enum that produces the `error.kind` slug

**Where:** L145 (D3 identifier list), L1560–1574 (rename table), L620–621 and
L1854 ("thirteen sites").

**What is wrong.** The user-visible slug `rekor_unavailable` occupies two
positions in the envelope, and the ADR accounts for only one. `EnvelopeError.kind`
is typed `ErrorCategory` (`crates/ocx_cli/src/error_envelope.rs:130`), a **fourth**
enum carrying its own `RekorUnavailable` variant (`:59`) under
`#[serde(rename_all = "snake_case")]` (`:50–51`). D3 (L145) enumerates three
identifiers to rename — `ExitCode`, `VerifyErrorKind`, `SignErrorKind` — and the
rename table cites `error_envelope.rs:307` only as the "serde-name pinning test",
never the variant it pins.

**Failure scenario.** A planner renames the three named identifiers. At
`error_envelope.rs:99` (`ExitCode::RekorUnavailable => Self::RekorUnavailable`)
the LHS breaks and is fixed; the RHS is `ErrorCategory::RekorUnavailable` and
compiles unchanged. Internal vocabulary moves, `error.kind` keeps emitting
`"rekor_unavailable"`, and the pinning test at `:307` goes red. The shortest fix
in reach is `#[serde(rename = "transparency_log_unavailable")]` — exactly the
shim L1578 forbids — leaving the two-vocabulary state L1856–1857 says the full
rename exists to prevent.

**Fix.** Add `ErrorCategory::RekorUnavailable` to D3's identifier list (L145).
Add three rename-table rows: `error_envelope.rs:59` (variant declaration), `:99`
(`ExitCode`→`ErrorCategory` arm), `:548` (`(ExitCode::…, ErrorCategory::…)` test
row). Correct "thirteen sites" at L620 and L1854. Optionally state in D-h
(L616–621) that the rename changes a member of the frozen `error.kind` set, not
only a `detail` slug — the reader currently has to infer it.

## F2 — Block — Delegation gap (5) is false, and row 8's fixture cannot fire

**Where:** L307 item (5), L322 ("what the delegated path provably does not"),
L333, L782 (Part III row 8), L1518 (`MultipleSignatures`), L1692 (fixture).

**What is wrong.** sigstore 0.14.0 **does** enforce exactly-one-signature on the
bundle DSSE path:

```rust
// bundle/verify/models.rs:246-252
// Spec requires exactly one signature — reject if count != 1.
if dsse.signatures.len() != 1 {
    return Err(BundleErrorKind::DsseInvalidSignatureCount(dsse.signatures.len()));
}
```

`DsseInvalidSignatureCount` is defined at `models.rs:111` and covered by the
crate's own rstest cases for 0 and 2 signatures (`:685–686`).

**Failure scenario.** D-d orders `verifier.verify` (`pipeline.rs:382`) **before**
`verify_envelope`. A two-signature envelope is rejected inside that call as
`BundleErrorKind::DsseInvalidSignatureCount` → `VerificationError::Bundle`
(`models.rs:156`) → OCX's `map_verification_error` `E::Bundle(_) =>
VerifyErrorKind::BundleParseFailed` (`pipeline.rs:778`). The run exits 65 with
slug `bundle_parse_failed`. `verify_envelope` never executes, so
`MultipleSignatures` is never produced: the L1692 fixture fails as written, and
`multiple_signatures` becomes a row in the table L1584 calls "the contract" whose
state is indistinguishable from never running — the unchecked-green class the ADR
itself invokes at L564 and L1878.

**Fix.** Delete item (5) from L307's gap list and drop "provably" from L322's
scope for it. Then pick one:
(a) *Keep the layer, make it reachable* — run OCX's signature-count check on the
received envelope **before** `verifier.verify`, consistent with D-d's own stance
that redundancy with sigstore is not a licence to drop a check (L1786–1788).
Row 8's "Enforced in" then names the pre-check site.
(b) *Delegate it* — remove `MultipleSignatures`, re-point row 8 at the delegated
path, and change the L1692 fixture's expectation to `bundle_parse_failed`. Under
(b) the verify variant count drops 16 → 15 and L1582's "23 → 39" becomes "23 → 38".

## F3 — Actionable — Annotation narrowing is both forbidden and required

**Where:** D-e L433, L441–443, L447–454 vs L1511, L1605, L1703.

**What is wrong.** D-e states annotations are "ordering and pre-filter hints only
— **never an exclusion**" (L433) and "never as the reason a candidate is absent
from the answer" (L441–443), and spends L447–454 explaining that a registry
relabelling `dev.sigstore.bundle.predicateType` must **not** be able to make
`ocx package sbom --type cyclonedx` exit 79. Three later sites assert the
opposite mechanism:

- L1511 — `AttestationNotFound` … "No attestation referrer for this subject
  (after annotation narrowing)"
- L1703 — "Annotation narrowing empties the candidate set … `AttestationNotFound`
  (79)"
- L1605 — `pipeline.rs` gains an "annotation-narrowed candidate filter"

L1703 describes the exact mechanism L449–451 names as the attack D-e forecloses.

**Fix.** Attribute the empty candidate set to predicate-type narrowing on the
**verified payload** (post-fetch, post-verify), and reserve "annotation" wording
for ordering and cap-pressure skips. Concretely: L1511 → "(after predicate-type
narrowing on the signed payload)"; L1703 → "Requested predicate type matches no
verified attestation on a subject that has referrers"; L1605 → "mode and
predicate-type narrowed candidate filter (annotations order only)".

## F4 — Actionable — Docs placement contradicts owner-fixed D8, and the sidebar is missing

**Where:** D8 L173–178 vs L1641–1642; Documentation Surfaces L1632–1646.

**What is wrong.** D8 is Part I (owner-fixed, "not open for re-litigation") and
fixes docs to "the existing `website/src/docs/` structure (`in-depth/` precedent
— **`guides/` does not exist**)". Documentation Surfaces then places two new
pages at `website/src/docs/use-cases/attach-an-sbom.md` and
`…/verify-an-sbom.md`. The tree has exactly four docs directories —
`authoring/`, `in-depth/`, `reference/`, `user-guide/` — so `use-cases/` does not
exist either, and the table invents precisely the kind of directory D8's stated
reason rules out.

Second half: `website/.vitepress/config.mts` registers **every** docs page with a
literal sidebar row (`:89–96`), and it appears in no Documentation Surfaces row.
`threat-model.md` and both use-case pages would ship unreachable — the failure
L1630 says the table exists to prevent.

**Fix.** Move both pages under `in-depth/` (D8-consistent), or amend D8 in the
same change if a `use-cases/` section is genuinely wanted. Either way add a
`website/.vitepress/config.mts` row to Documentation Surfaces covering the
sidebar entries for `threat-model.md` and the two new pages.

## F5 — Actionable (nit) — Part V attributes the annotation set to the wrong decision

**Where:** L1794, and the same slip at L1706 and L1541.

**What is wrong.** L1794 lists "**The annotation set** (D-f)" on Part V's MAY-NOT
list. The annotation set is decided in **D1** (L86–100), whose own text says "it
joins Part V's MAY-NOT list"; D-f is `push --sbom` mechanics and contains no
annotation decision. Part V is the one-way-door register, so a reader following
the citation lands in the wrong section for the ADR's self-described "sharpest
one-way door" (L97–100). Same pattern at L1706 (`--predicate` symlink refusal is
decided in Part IV CLI, L1387–1397) and, weakly, L1541 (offline refusal is
decided in Part IV, L1138–1151).

**Fix.** L1794 → `(D1)`; L1706 → `(Part IV, CLI)` or drop the reference;
L1541 → `(Part IV, attest pipeline)`.

## F6 — Nit — sigstore's subject[0] concession cited to the wrong doc comment

**Where:** L307 item (3).

**What is wrong.** The claim reads "`InTotoStatementV1::subject_sha256_digest()`
reads `.first()` only, and **its own doc comment** concedes Go cosign and
sigstore-go iterate all subjects". Both facts are true but they sit on different
items: `subject_sha256_digest`'s doc (`bundle/intoto.rs:111–113`) says only
"Return the SHA-256 digest of the first subject"; the concession is in
`validate_cosign_v1`'s doc at `:64–68` ("Go cosign and sigstore-go both iterate
all subjects when matching against an artifact digest; we only consume
`subject[0]` for now").

**Fix.** Cite `bundle/intoto.rs:64–68` for the concession, keeping `:115` for the
`.first()` call.

## F7 — Nit — `SbomReport` is defined twice with different shapes

**Where:** L1321 (lib) and L1452 (CLI DTO).

**What is wrong.** Part IV declares `pub struct SbomReport { pub attestations:
Vec<AttestationMatch> }` for `package_manager/tasks/sbom.rs` and, 130 lines later,
`pub struct SbomReport { entries: Vec<SbomEntry> }` for `api/data/sbom.rs`. L1280
claims the facade "Mirrors `SignOptions` / `SignReport` / `sign_one` **exactly**",
but that precedent uses distinct names: lib `SignReport`
(`package_manager/tasks/sign.rs:55`) vs CLI `SignatureReport`
(`api/data/signature.rs:43`). The attest pair already follows it
(`AttestReport` / `AttestationReport`).

**Fix.** Rename the CLI DTO — e.g. `SbomListingReport` — so the two are
distinguishable in prose and in a planner's file-by-file decomposition.

---

# Addendum — cross-model overlap and additional sites

Lead owns three defects found by the cross-model pass: (1) payloadHash-over-PAE
in D-g/row-12, (2) D-d's "every subject" framing vs `verifier.verify`'s
subject[0] hard-fail, (3) annotation narrowing at L1511/L1703.

**F3 above is defect (3) — withdrawn from my count, lead owns it.** Below: only
ADDITIONAL sites of the same three errors.

## Additional site of defect (3) — annotation narrowing

**L1605**, Affected Surfaces, `oci/verify/pipeline.rs` row: "…`from_bundle` takes
the mode; **annotation-narrowed candidate filter**". Third instance beyond
L1511/L1703. Fix with the same wording: "mode and predicate-type narrowed
candidate filter (annotations order only)".

## Defect (1) — no additional sites, and the type contract is already right

Swept every `payloadHash` / `PAE` / `envelope_hashes` mention. Only L582 (D-g)
and L1216 (Part IV doc comment) carry the wrong claim; both are yours.

Positive finding worth keeping: the Part IV contract already encodes the correct
semantics —

```rust
// L925, L929
pub(crate) struct EnvelopeHashes { pub envelope: Digest, pub payload: Digest }
pub(crate) fn envelope_hashes(envelope_json: &[u8], payload: &[u8]) -> EnvelopeHashes;
```

`payload: &[u8]` hashed to a `payload` digest is exactly
`Sha256::digest(payload_bytes)` (sigstore `models.rs:488`). **The fix is
prose-only at L582/L1216; no signature moves.** L1028 (`Signs
sha256(PAE(payload_type, statement_bytes))`) is the *signature* over PAE and is
correct — do not "fix" it while editing the neighbours.

## Defect (2) — additional sites, and it amplifies F2 from one variant to four

Sites beyond D-d's framing paragraph: **L964–965** (`binds_subject` doc: "over
EVERY subject — not `subject[0]`, which is the gap in the delegated path"),
**L1686–1689** (four negative fixtures), **L1738–1740** (red-before-green step 3).

The consequence is F2's class, not a re-derivation: because `verifier.verify`
runs first (`pipeline.rs:382`), the subject checks are unreachable exactly as
`MultipleSignatures` is. Traced through sigstore 0.14.0:

| ADR fixture | Expected (ADR) | Actually produced | Path |
|---|---|---|---|
| L1686 valid attestation for A served as referrer of B | `StatementSubjectMismatch` | `SignatureInvalid` | `verifier.rs:78–82` compares `subject_sha256_digest` vs `hex(input_digest)` → `SignatureErrorKind::Transparency` → `E::Signature(_)` → `pipeline.rs:786` |
| L1687 zero-subject Statement | `StatementSubjectAbsent` | `BundleParseFailed` | `intoto.rs:115` `.first()` returns `None` → `Err` → `models.rs:266` `map_err(\|_\| DssePayloadDecode)` → `E::Bundle(_)` → `pipeline.rs:778` |
| L1688 matching `md5`, non-matching `sha256` | `StatementSubjectMismatch` | `SignatureInvalid` | as row 1 |
| L1689 DigestSet with no `sha256` | `StatementSubjectWeakAlgorithm` | `BundleParseFailed` | `.digest.get("sha256")` → `None` → same as row 2 |
| L1692 two signatures | `MultipleSignatures` | `BundleParseFailed` | `models.rs:248–252` (F2) |

So **four** of the sixteen new `VerifyErrorKind` variants are unreachable through
the pipeline as composed, not one. Five fixtures fail as written, and the
red-before-green step at L1738–1740 ("delete the `binds_subject` call; the
cross-subject fixture must red") cannot demonstrate red for the stated reason —
the fixture reds either way, on sigstore's check.

Checked and **reachable**, so the layer is not pointless: `PayloadTypeUnsupported`
(sigstore uses `payload_type` in `compute_pae` but never validates it) and
`StatementTypeUnsupported` (`validate_cosign_v1` is not on the bundle path —
gap (1)). Those two are the delegated path's real shape gaps.

Directional note for whoever writes the D-d fix: for the CVE-2026-31830 shape
sigstore already refuses, and the every-subject rule's only differential effect
would be to **accept** a multi-subject Statement whose `subject[0]` is not the
target — which `verifier.verify` rejects first anyway. The layer's subject value
is therefore zero as composed, in both directions. Whatever L328–330 ends up
saying, it cannot keep claiming this is "the sharpest single reason this layer
exists" while `verifier.verify` runs first.

**Fix (same options as F2, now covering four variants):** either move OCX's
Statement-shape checks ahead of `verifier.verify` on the received envelope so all
four are reachable, or drop the four variants, re-point rows 4/5/6/8, and correct
the five fixtures to `signature_invalid` / `bundle_parse_failed`. Under the second
option the verify variant count drops 16 → 12 and L1582's "23 → 39" becomes
"23 → 35".
