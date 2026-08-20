# Spec review — adr_sbom_attestations

Reviewer: hex-architect review panel, focus SPEC (opus).
Subject: `.claude/artifacts/adr_sbom_attestations.md` (1262 lines).
Ground truth cross-checked against: `discover_attest_architecture_map.md`,
`plan_milestone_split_supply_chain.md`, the three research artifacts, and the
real source tree at `crates/`.

Findings appended as confirmed, in the order confirmed — not grouped by severity.

## F1 — Block — `BTreeMap<Platform, Digest>` does not compile

**ADR:** L313 (D-f), L1054 (Affected Code Surfaces).
**Ground truth:** `crates/ocx_lib/src/oci/platform.rs:65`.

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Platform {
```

`Platform` derives no `Ord`/`PartialOrd`. `BTreeMap<K, V>` requires `K: Ord` for
`insert`/`get`/iteration, so `platform_digests: BTreeMap<Platform, Digest>` is a
compile error the moment it is populated.

Deriving `Ord` is not the cheap fix and should not be reached for reflexively:
`Platform` is documented (same file, L48-51) as the canonical `ocx.lock` /
dependency-pin **map key**, so declaration order would become an observable
ordering (API-07), and `Any` sorts first by accident of position.

**Fix:** key by the canonical string form the type already guarantees —
`BTreeMap<String, Digest>` over `Platform::Display`, which the doc comment at
L47-49 calls "the single canonical, lossless, injective string form" and which
is already the lockfile map-key spelling. State that in D-f so a builder does not
"fix" the compile error by deriving `Ord`.

Secondary, same decision: `Platform`'s serde goes through `native::Platform` and
emits a JSON **object** (`{"os":…,"architecture":…}`), which serde_json cannot
use as a map key. Any path that serializes this field needs the string form
regardless.

---

## F2 — Block — `SbomReport` cannot populate `SbomEntry`; four DTO fields have no source

**ADR:** L858 (`SbomReport { pub attestations: Vec<VerifiedAttestation> }`),
L782-786 (`VerifiedAttestation`), L964-972 (`SbomEntry`), L285 (D-e default mode).

`VerifiedAttestation` carries exactly three fields: `predicate_type`, `payload`,
`subject_digest`. `SbomEntry` requires seven, of which **four have no source in
the manager's return type**: `referrer_digest`, `certificate_identity`,
`certificate_oidc_issuer`, `signed_at`.

Those four exist on `VerifyResult`
(`crates/ocx_lib/src/oci/verify/pipeline.rs:114-125` — `referrer_digest`,
`certificate_identity`, `certificate_oidc_issuer`, `signed_at: u64`), which
`SbomReport` discards. D-e's default-mode contract at L285 explicitly promises
all four in the listing, so this is not a DTO the CLI can build.

**Fix:** `SbomReport` carries the per-candidate `VerifyResult` alongside the
attestation — e.g. `pub attestations: Vec<(VerifyResult, VerifiedAttestation)>`,
or a named `VerifiedSbomAttestation` struct holding both. Note `signed_at` is
`u64` epoch seconds on `VerifyResult` and `String` RFC-3339-Z on `SbomEntry`, so
name where that conversion happens (the existing `VerificationReport` already
solves it — reuse, do not re-derive).

---

## F3 — Block — `--attestation` and `--type` have no path from CLI into the pipeline

**ADR:** L939 ("`ocx package verify` gains `--attestation` … and `--type`"),
L920-922 (`PackageSbom.predicate_type`), L848-856 (`SbomOptions`), L1048/L1059
(Affected Code Surfaces).
**Ground truth:** `crates/ocx_lib/src/package_manager/tasks/verify.rs:39-55`.

The CLI never builds a `VerifyContext`. Per map §5 and the real code, the chain is
`CLI → VerifyOptions → manager().verify_one() → VerifyContext`. `VerifyOptions`
has seven fields (`policies`, `client`, `trust_root`, `rekor_url`, `offline`,
`state`, `no_cache`) and no content-mode field; the ADR's `SbomOptions` is a
field-for-field copy of it and likewise carries no `predicate_type`.

So three declared flags are unreachable:

- `verify --attestation` → nothing sets `VerifyContentMode::Attestation`.
- `verify --type` → nothing narrows the predicate type.
- `sbom --type` → `PackageSbom.predicate_type` has no `SbomOptions` field.

`crates/ocx_lib/src/package_manager/tasks/verify.rs` does not appear in the
Affected Code Surfaces table at all, and no Part IV contract amends
`VerifyOptions`.

**Fix:** add `pub content: VerifyContentMode` to `VerifyOptions` and
`pub predicate_type: Option<PredicateType>` (or the same `VerifyContentMode`) to
`SbomOptions`, give both a Part IV contract block, and add
`package_manager/tasks/verify.rs` to the Affected Code Surfaces table.

---

## F4 — Block — the mode gate's own signature is never given

**ADR:** L256-262 (D-d: "The load-bearing gate at `pipeline.rs:498` changes …"),
L1048.
**Ground truth:** `crates/ocx_lib/src/oci/verify/pipeline.rs:479-481`.

```rust
fn from_bundle(
    bundle: &sigstore_protobuf_specs::dev::sigstore::bundle::v1::Bundle,
) -> Result<Self, VerifyErrorKind> {
```

`BundleParts::from_bundle` takes only the bundle. The ADR puts the mode on
`VerifyContext`, which `from_bundle` cannot see, and never states the new
signature — even though this is named as the single load-bearing change point of
the whole design (Context L39-43, map §"Synthesis").

Part IV gives explicit signatures for `verify_envelope` and
`verify_tlog_binding`, both new. The one *changed* signature is the one omitted.

**Fix:** state it, e.g.
`fn from_bundle(bundle: &Bundle, mode: &VerifyContentMode) -> Result<Self, VerifyErrorKind>`,
and name the caller (`verify_one_referrer`, `pipeline.rs:329-338`) that threads
`ctx.content` in.

*Verified and correct, for the record:* D-d's claim that a mode mismatch is
"skipped … without aborting the scan over the remaining candidates" holds against
the real loop — `pipeline.rs:310-311` does
`Err(kind) => merge_failure(&mut best_error, kind)` and continues. No finding.

---

## F5 — Warn — two module paths in Affected Code Surfaces do not exist

**ADR:** L675 + L1043 (`oci/sign/keyless.rs`), L1054 (`oci/publish/…`).

- `crates/ocx_lib/src/oci/sign/keyless.rs` does not exist. `KeylessSigner` is a
  unit struct at `crates/ocx_lib/src/oci/sign/signer.rs:50` (`pub struct KeylessSigner;`).
  The row is not marked **new**, so a work package reads it as an existing file.
  Being a unit struct, the free-function `issue_ephemeral_certificate(token, fulcio_url)`
  shape is fine — only the path is wrong.
- `crates/ocx_lib/src/oci/publish/…` does not exist; there is no `oci/publish`
  module. `PushOutcome` is at `crates/ocx_lib/src/publisher.rs:42`, which is
  exactly what the map says (§6). The ADR contradicts its own cited source.

**Fix:** `oci/sign/signer.rs` and `crates/ocx_lib/src/publisher.rs`.

---

## F6 — Block — D-e's empty-artifactType rationale is refuted by the code it describes

**ADR:** L266-276 (D-e Discovery).
**Ground truth:** `crates/ocx_lib/src/oci/client/native_transport.rs:215-243`,
`crates/ocx_lib/src/oci/verify/pipeline.rs:457`.

D-e argues for `list_referrers(..., None)` because "OCI 1.1 lets a registry ignore
the filter entirely … so a client that *depends* on server-side filtering is
depending on an optional behaviour."

OCX has never depended on server-side filtering. `filter_and_convert_referrers`
carries the opposite conclusion drawn from the same spec fact, verbatim:

```rust
/// The OCI spec permits a server to ignore the `artifactType` query filter
/// (or apply it without setting the advisory `OCI-Filters-Applied` header),
/// so this client-side pass is the only filtering callers can rely on.
```

and implements it as `Some(wanted) => entry.artifact_type.as_deref() == Some(wanted)`,
**`None => true`**. Today's verify path passes `Some(SIGSTORE_BUNDLE_V03)`
(`pipeline.rs:457`). Passing `None` does not sidestep an unreliable server filter —
it turns off the only reliable *client* filter.

Consequence: the attestation candidate set admits referrers of arbitrary
`artifactType`, gated only by `dev.sigstore.bundle.content`, which the ADR itself
classifies as "discovery hints only" and unsigned registry metadata (L50, L277-278).
A registry (or anyone who can push referrers) can then fill
`MAX_ATTESTATION_CANDIDATES` (32) and `MAX_TOTAL_ATTESTATION_BYTES` (64 MiB) with
unrelated artifacts before a real attestation is examined.

D-e's *other* premise — "signatures and attestations share the same `artifactType`,
so filtering on it excludes nothing" — is the argument for **keeping**
`Some(SIGSTORE_BUNDLE_V03)`: it costs nothing and it is the one typed filter in
the chain.

**Fix:** keep `Some(SIGSTORE_BUNDLE_V03)`, and rewrite D-e's rationale to say the
annotation narrowing is layered *on top of* the existing client-side artifactType
re-filter, not in place of it. If cosign parity on this specific point is genuinely
wanted, that is a different argument and needs stating as one.

---

## F7 — Block — `SubjectDigestMismatch` already exists and means something else

**ADR:** L995 (new-variant table), L1119-1121 (three negative fixtures all mapped
to it), L468-470 (checklist rows 4/5/6).
**Ground truth:** `crates/ocx_lib/src/oci/verify/error.rs:120-128`.

```rust
/// The registry served subject-manifest bytes that do not hash to the
/// digest the index resolved.
/// ...
#[error("registry served a subject manifest that does not match its digest")]
SubjectDigestMismatch,
```

The variant exists as a **unit** variant, its slug `subject_digest_mismatch` is
already in the frozen `kind_detail_values_are_stable` table, and it is constructed
at `pipeline.rs:747` and `pipeline.rs:1279`. The ADR lists it as new, with fields
`{ expected, actual }`.

Three problems, in order of cost:

1. **Meaning collision.** The existing variant is a *transport-integrity* failure:
   the registry served the wrong bytes for a manifest. The ADR's is an
   *attestation-substitution* failure: a validly signed Statement whose
   `subject[].digest.sha256` names a different artifact — the CVE-2026-31830 shape
   the ADR calls the single highest-value assertion in the suite (L1146-1147).
   Collapsing both under one slug means a consumer script cannot tell "the registry
   is corrupt" from "someone served artifact A's attestation as artifact B's".
2. **It is a change, not an addition.** Unit → struct variant breaks both existing
   construction sites and the existing pinned table row. The ADR budgets neither.
3. **The count is wrong.** "12 variants + 12 slug rows" (L1047) is 11 new rows plus
   one mutated row; L1024-1027 says the tables "grow by the rows above and change
   the one renamed row" — there are two changed rows, not one.

The other 11 proposed verify slugs and both sign slugs are collision-free (checked
against the full `kind_detail()` slug set).

**Fix:** mint a distinct variant — `StatementSubjectMismatch { expected, actual }`,
slug `statement_subject_mismatch`, exit 65 — and leave `SubjectDigestMismatch`
alone. Then split L1119-1121's three fixtures: the zero-subject and
weak-algorithm cases want their own slugs too (`statement_subject_absent`,
`statement_subject_weak_algorithm`) or the ADR must say why one slug covers all
three, since Part III rows 4, 5 and 6 are three separate requirements.

---

## F8 — Block — the `rekor_unavailable` rename misses the doc contract and three code sites

**ADR:** L110 (D3), L1016-1027 (Renames), L1074 (Documentation Surfaces).
**Ground truth:** the five literal sites plus the docs.

The rename table has three rows and the third is vacuous —
`ErrorCategory | matching variant name | matching variant name` names neither the
old nor the new identifier.

Actual sites of the literal string `rekor_unavailable`:

| Site | ADR covers it? |
|---|---|
| `oci/verify/error.rs:395` `kind_detail()` | yes |
| `oci/sign/error.rs:210` `kind_detail()` | yes |
| `oci/verify/error.rs:666` pinned slug table | yes (L1025) |
| `oci/sign/error.rs:387` pinned slug table | yes (L1025) |
| `ocx_cli/error_envelope.rs:307` **ErrorCategory serde-name pinning test** | **no** |
| `website/.../command-line.md:3770` — the `kind` enumeration a script matches on | **no** |
| `website/.../command-line.md:3778` — sign envelope kind table row | **no** |
| `website/.../command-line.md:3934` — verify envelope kind table row | **no** |

The three docs rows are the user-facing half of the contract being renamed. The
Documentation Surfaces table lists `command-line.md` only for the new commands and
the `--sigstore-trusted-root` flag, so as written the docs keep telling scripts to
match `rekor_unavailable` after the wire slug changed. That is exactly the failure
mode D3 exists to avoid.

Unresolved and unstated, needed before implementation:

- **`ExitCode::RekorUnavailable = 83`** (`crates/ocx_lib/src/cli/exit_code.rs:77`).
  "Exit code 83 unchanged" settles the number, not the identifier. D3's stated
  rationale ("will outlive the name Rekor") argues for renaming it; renaming it
  touches `cli/classify.rs`, `error_envelope.rs:99`, `:548` and 9 reference sites.
  Decide explicitly.
- **`VerifyErrorKind::RekorUnavailable` / `SignErrorKind::RekorUnavailable` variant
  names** (43 identifier references in `ocx_lib`). If only the slug moves, the
  variant name and its wire value disagree permanently. Also a decision, also unstated.

**Fix:** replace the three-row table with the eight rows above, state the two
identifier decisions, and add the three `command-line.md` line refs to Documentation
Surfaces. Also note the two existing acceptance tests that assert this path
(`test/tests/test_verify.py:446`, `test/tests/test_sign.py:737`).

---

## F9 — Block — D-c misstates issue #102, and the conflict it names is then left unresolved

**ADR:** L209-231 (D-c), L626 (`SlsaProvenance` → v0.2), Not Doing table L1249-1260.
**Ground truth:** [ocx-sh/ocx#102](https://github.com/ocx-sh/ocx/issues/102),
revised 2026-08-20 — the same day as the amendment and this ADR.

D-c L217 states:

> #102's requirement is a *verification* rule about which provenance versions a
> policy accepts — which is where it belongs, next to `builder` matching.

#102 says the opposite. It is an **attach-side** requirement, twice:

> "2. Validate SLSA spec version from the predicate type URI; reject < v1.0."
>
> Acceptance criteria: "Predicate type validation rejects versions < v1.0 and
> unknown predicate types with a clear error."

and its Revision block scopes it explicitly to this ADR's engine: "this is
`ocx package attest --predicate FILE --type slsaprovenance` on the #198 engine".

So the collision is concrete, not theoretical: under the ADR,
`ocx package attest --type slsaprovenance` resolves to
`https://slsa.dev/provenance/v0.2` (L626, cosign parity) and **publishes exactly
the artifact #102 requires the attach path to reject** — with no error, no warning,
and no policy hook. D-c's mitigation ("the resolved predicateType URI is echoed in
the attest report", L225-227) is publisher-side *visibility* offered against a
requirement for publisher-side *refusal*; it does not satisfy #102 on either
reading.

Then the relocated requirement lands nowhere. `>= v1.0` appears exactly twice in
1262 lines, both inside D-c's own option table. There is no policy field for it in
D-j's `TrustPolicy`, no error variant, no exit row, no Part III row, no test, and
no Not Doing row. Every other issue in the ADR is either delivered (#103 → the
`builder` field, L404) or explicitly deferred (#107 → Not Doing L1252 + Risks
L1235; #104 → D5; #200 → D6). #102 alone is named, reinterpreted, and dropped.

This is an owner decision, not a builder's — the two admissible resolutions differ
in scope:

**(a) Keep cosign parity.** `slsaprovenance` → v0.2 stands; #102 is amended in the
issue to drop "reject < v1.0" for the alias path, recording that a v1 publisher
writes `--type slsaprovenance1` or the full URI. Costs nothing here; needs the
issue edited and a Not Doing row saying so.

**(b) Enforce at attach.** `AttestPipeline` rejects a resolved provenance URI
below v1.0. This contradicts D-c's own "the alias table stays a pure lookup with
no policy in it" only if the check is put in the table — put it in the pipeline and
both hold. Costs: one `SignErrorKind` variant + slug + exit row, one Part III row,
one negative fixture.

Either way the ADR must say which, in D-c and in Not Doing. Silence freezes a
contradiction between two open issues in the same milestone.

*Checked and clean, for the record:* Part I D1–D8 are otherwise faithful to the
amendment's decisions 1–8, clause for clause. #102's other acceptance criterion —
"DSSE payload subject digest == the pushed manifest digest" — is delivered by D-f
plus Part III row 4. #102's stale `fake_sigstore.py` reference is already corrected
by its own Revision block.

---

## F10 — Warn — D-f's `attestation: failed` report has no DTO and no touched file

**ADR:** L314-318 (D-f Failure atomicity), L943-973 (Report DTOs),
L1055-1063 (Affected Code Surfaces, CLI section).

D-f fixes the push-then-attest failure contract:

> The report carries `attestation: failed` with the per-item error, and the
> process exits with the attest error's classified code (PKG-24: worst classified
> failure).

Nothing carries it. `AttestationReport` (L950-959) has eight fields, all
success-shaped — no status, no error. `crates/ocx_cli/src/api/data/push.rs` exists
in the tree and is **absent from the Affected Code Surfaces table**, so the DTO
that would actually render `attestation: failed` is never named as changing.

Same gap on the exit-code half: PKG-24 is invoked ("worst classified failure") but
`push --sbom` is a single-target command whose report is `push.rs`, not a
`BatchReport`, and the ADR does not say where the two outcomes are combined.

**Fix:** name the `push.rs` DTO change and give the attestation sub-result a
contract — a `attestation: Option<AttestationOutcome>` where `AttestationOutcome`
is succeeded-or-failed-with-slug, reusing the existing error-slug envelope
(CLI-04). Add `crates/ocx_cli/src/api/data/push.rs` to Affected Code Surfaces.

---

## F11 — Warn — four declared error variants have no negative fixture

**ADR:** L481 (Part III row 17), L1113-1135 (Testing Strategy negative paths),
L989-1011 (error tables).

The Testing Strategy says every negative fixture "asserts a **specific** error
kind, never merely non-zero" (L1115), and Part III row 20 makes fail-closed a
normative requirement. Four declared variants get no row:

| Variant | Status |
|---|---|
| `AttestationBudgetExhausted` (`MAX_TOTAL_ATTESTATION_BYTES`) | **Part III row 17 explicitly promises this fixture** ("Fixtures → `TooManyAttestations`, `AttestationBudgetExhausted`") and the test table omits it |
| `AttestationNotFound` | no fixture; also no stated relationship to the existing `NoSignaturesFound` / zero-candidate `aggregate_failure` path, so it is not clear it is even reachable |
| `PredicateNotJson` (`SignErrorKind`) | no fixture |
| `PredicateTooLarge` (`SignErrorKind`) | no fixture |

`AttestationBudgetExhausted` is the sharpest: it is the cap that "closes the
candidates x per-envelope product, which neither cap closes alone" (L526-527), and
an untripped limit constant is precisely the unchecked green the ADR refuses
elsewhere (L330 on `hashedrekord:0.0.2`, L1252 on Rekor v2).

**Fix:** four rows in the negative-path table. For `AttestationNotFound`, also
state how it is produced — the existing loop aggregates per-candidate failures via
`merge_failure` (`crates/ocx_lib/src/oci/verify/pipeline.rs:310-314`), so a run
where annotation narrowing removed every candidate has to reach it through
`aggregate_failure`, not through the loop.

---

## F12 — Warn — two Part V spike rows are wider than "a constant or a field position"

**ADR:** L1171-1181 (The spike MAY adjust), L1183-1198 (MAY NOT).

Rows 3, 4, 5 and 6 are genuinely constants, request-body spellings or table rows —
correctly bounded. Two are not:

**Row 1 (`artifactType` at manifest top level vs on the config descriptor)** is
described as "One field position." It is not. OCI 1.1 builds each Referrers-API
index descriptor's `artifactType` from the referring manifest's `artifactType`,
falling back to the config descriptor's `mediaType` — so moving it changes what
`list_referrers` reports per candidate, which is the value
`filter_and_convert_referrers` compares against
(`crates/ocx_lib/src/oci/client/native_transport.rs:224-227`) and the value F6
argues discovery should keep filtering on. It also forks
`ReferrerManifest::build(subject, artifact_type, payload)`, today shared with the
signature path (map §3). Row 1 reaches discovery, not just serialization.

**Row 2 (`STATEMENT_TYPE_WRITTEN` flips to v0.1)** is a one-constant edit that
silently retires D-b's decided property. D-b is argued at length as "Strict
producer, tolerant consumer" (L186) and is recorded as a documented deviation to
be written into `signing.md` (L192-194). If the spike flips what OCX writes, OCX
is no longer a strict producer and the `signing.md` paragraph is wrong — but the
row authorises the constant change alone.

**Fix:** for row 1, state the discovery consequence and require the artifactType
filter decision (F6) to be re-confirmed with it. For row 2, add "and amend D-b plus
the `signing.md` deviation paragraph" — the constant is not the whole change.

---

## F13 — Warn — no error/success envelope golden for `attest` or `sbom`

**ADR:** L1103-1111 (Golden shapes), L943-973 (Report DTOs).
**Ground truth:** `test/tests/test_verify.py:245` `test_verify_error_envelope_golden_shape`,
`:304` `test_verify_success_envelope_golden_shape`.

The four golden shapes listed are all *wire* artifacts — referrer manifest, DSSE
envelope, bundle JSON, PAE vector, `dsse:0.0.1` canonicalized body. None pins the
**JSON envelope** the two new commands emit, and the map names the existing pair
above as "THE pattern for new test_attest/test_sbom envelope pinning" (map §8).

This matters more than usual here: the ADR adds two new commands, two new DTOs and
fourteen new slugs on a frozen envelope contract (C-S1-1), and renames one existing
slug. CLI-04 asks for a snapshot test per exit code. Without golden envelopes, the
only thing pinning the new `--format json` output shape is the DTO struct
definition, which is not a contract test.

**Fix:** two rows in Golden shapes — `test_attest_*_envelope_golden_shape` and
`test_sbom_*_envelope_golden_shape`, built on the existing pair.

---

# Verified clean

Checks run that produced no finding, recorded so a silent dimension is not
mistaken for a skipped one.

- **`Signer` trait extension (D-a).** Part IV L650-664 reproduces the real trait
  (`crates/ocx_lib/src/oci/sign/signer.rs`, map §1 orchestrator-verified) exactly
  and adds one method. `KeylessSigner` is a unit struct, so the extracted
  free-function `issue_ephemeral_certificate(token, fulcio_url)` needs no receiver.
  Only the file path is wrong (F5).
- **`AttestContext` / `AttestPipeline::run`.** Mirrors `SignContext`
  (`crates/ocx_lib/src/oci/sign/pipeline.rs:40-59`) field-for-field in the same
  order plus `predicate_type`/`predicate`; `run(client, ctx) -> Result<_, SignError>`
  matches `SignPipeline::run` at `:85`. Field types (`Identifier`, `Platform`,
  `dyn Signer`, `dyn TokenProvider`, `Index`, `Url`, `StateStore`) all resolve.
- **`AttestOptions`.** Mirrors `SignOptions`
  (`crates/ocx_lib/src/package_manager/tasks/sign.rs:38-49`) including
  `identity_token: Option<Zeroizing<String>>`.
- **`AttestResult`.** Mirrors `SignResult` (`sign/pipeline.rs:62-75`) plus
  `predicate_type`.
- **D-d's non-aborting scan.** The claim that a mode mismatch skips one candidate
  without ending the scan holds: `verify/pipeline.rs:310-311` is
  `Err(kind) => merge_failure(&mut best_error, kind)` inside the candidate loop.
- **Slug-table row counts.** L1025's "23 verify rows, 12 sign rows" is exact
  (counted in both `kind_detail_values_are_stable` tables).
- **Slug collisions.** Of the 14 proposed slugs, exactly one collides with the
  existing set — `subject_digest_mismatch` (F7). The other 13 are new.
- **Envelope categories.** All new variants map to exits 64/65/79/83, every one of
  which already has an `ErrorCategory` (`error_envelope.rs`, `from_exit_code` total
  map test-pinned at `:548`). No new category needed — the ADR's claim holds.
- **Exit 85 kept free.** No attestation path claims it; D5 reserves it for #104 and
  the ADR restates the consequence at L124. Confirmed against
  `crates/ocx_lib/src/cli/exit_code.rs` (next free slot is 85).
- **Part I fidelity.** D1–D8 track the amendment's decisions 1–8 clause for clause;
  the only divergence is D7's "five-item list" vs the amendment's four, which is the
  CycloneDX reader that amendment decision 2 independently requires. Not drift.
- **Part III transcription.** All 20 checklist rows from
  `research_dsse_verification_pitfalls.md:163-182` are carried over with their
  citations intact. Row 17 is strengthened (adds a cumulative-byte cap); row 18 is
  marked as a deviation, matching the research's own "(explicit decision)" tag.
  Rows without a behavioural seam are named as such at L486-491, honestly — row 2
  gets a real byte-comparison seam and row 19 claims no test.
- **cosign interop and TEST-10.** Both directions are covered (L1097-1101),
  including the `_type: v0.1` case that D-b exists for. Exit-code/stream separation
  is stated explicitly at L1092-1093.
- **Red-before-green.** Both named mutations (mode gate, `binds_subject` deletion)
  are concrete and target the two properties most likely to pass for the wrong
  reason.
- **Part V MAY NOT list.** Correctly locks storage shape, verify composition, error
  taxonomy, module layout, dependency direction, every Part III requirement, and the
  `sbom` verify-by-default contract, with an explicit escalation clause. Rows 3–6 of
  MAY-adjust are genuinely constants.

# Verdict

**13 findings, 8 blocking, 5 warn.**

Block: F1 (`BTreeMap<Platform, Digest>` does not compile), F2 (`SbomReport` cannot
populate `SbomEntry`), F3 (three flags with no plumbing contract), F4 (the
load-bearing gate's own signature is never given), F6 (D-e's rationale refuted by
the code it describes), F7 (`SubjectDigestMismatch` already exists and means
something else), F8 (rename misses the doc contract and three code sites), F9 (D-c
misstates #102).

Warn: F5, F10, F11, F12, F13.

**All 13 are actionable** — each names a concrete fix — with one exception that
needs an owner decision before it can be actioned:

- **F9** requires the owner to choose between keeping cosign's `slsaprovenance`
  → v0.2 mapping (and amending issue #102 to drop "reject < v1.0" on the alias
  path) or enforcing the v1.0 floor at attach (and paying for one error variant,
  one Part III row, one fixture). Reason: two open issues in the same milestone
  make contradictory demands on the same flag value, and the ADR resolves it by
  restating #102 as something it does not say. A builder cannot pick.

F8 additionally carries two sub-decisions that are cheap but must be made
explicitly rather than discovered during implementation: whether
`ExitCode::RekorUnavailable` and the `*ErrorKind::RekorUnavailable` variant
identifiers are renamed alongside the wire slug.
