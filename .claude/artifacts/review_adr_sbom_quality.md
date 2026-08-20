# Adversarial QUALITY review — `adr_sbom_attestations.md`

Reviewer: hex-architect review panel, QUALITY axis (adversarial framing).
Model: opus (design review, per CLAUDE.md model policy).
Date: 2026-08-20.

**Subject:** `.claude/artifacts/adr_sbom_attestations.md` (1262 lines).

**Framing.** Break the design, steelman what it rejected, test the honesty of
its claims. Every finding below was refuted before it was written: I opened the
cited source and tried to prove the ADR right. Findings that survived that are
here; the ones that did not are recorded in "Refuted" at the end, because an
adversarial pass that reports only hits is not evidence of anything.

Line numbers are `adr_sbom_attestations.md` unless a path is given.

---
## F1 — BLOCK — D-d's rejection of sigstore-rs delegation is factually false, and the option as written is unimplementable

**Severity:** Block. **Classification:** Actionable.
**ADR line:** L239 (D-d option table, third row); L241–245 (the Decision paragraph).
**Attack surface:** 1(a) — steelman the rejected option.

**What the ADR says.** L239:

> Rejected because that method is bound to sigstore-rs's own `Verifier`
> construction, which would fork the trust-root and identity path away from
> `TrustRoot` + `PolicyDeferredToOcx`. Two trust roots in one binary is the
> defect this avoids.

**Why it is false.** OCX *already* constructs and drives sigstore-rs's `Verifier`
with OCX's own trust root and OCX's own identity policy. Three call sites, all
shipped today:

- `crates/ocx_lib/src/oci/verify/trust_root.rs:35` — `use sigstore::trust::TrustRoot as SigstoreTrustRootTrait;`
- `crates/ocx_lib/src/oci/verify/trust_root.rs:301` — `impl SigstoreTrustRootTrait for TrustRoot`
- `crates/ocx_lib/src/oci/verify/pipeline.rs:248` — `Verifier::new(RekorConfiguration::default(), ctx.trust_root.clone())`
- `crates/ocx_lib/src/oci/verify/pipeline.rs:383` — `verifier.verify(subject_bytes, bundle, &PolicyDeferredToOcx, true)`

`Verifier::new<R: TrustRoot>(rekor_config, trust_repo)` is generic over the
trust root; `verify` takes the policy by reference. The module doc at
`trust_root.rs:12-17` states the intent outright: "The type implements
`sigstore::trust::TrustRoot`, so it plugs straight into
`sigstore::bundle::verify::Verifier`, which owns chain building, validity
windows and SCT verification."

There is exactly one `Verifier`, holding exactly one trust root — OCX's. A
second trust root is not merely avoided by the rejection, it is **unreachable**:
nothing in the delegation option constructs one. The stated defect cannot occur.

**And the option as phrased cannot be taken either.** `verify_bundle_content` is
`pub(crate)` in sigstore 0.14.0
(`~/.cargo/registry/src/*/sigstore-0.14.0/src/bundle/verify/verifier.rs`, the
`fn verify_bundle_content(content, signing_key, signature, input_digest)`
declaration). It is not callable from `ocx_lib` at any privilege. So D-d rejects
an option for a reason that is untrue, and the option it rejects is one no
implementer could have taken regardless.

**Why this matters beyond pedantry.** The delegation doctrine
(`adr_real_sigstore_stack`) prefers delegating crypto. A future reader hitting a
DSSE verification bug will find a rejection rationale that reads as a considered
architectural boundary, will believe delegation was evaluated on the merits, and
will not re-derive the *actual* reasons — which are good ones and are currently
recorded nowhere.

**The real reasons delegation is insufficient** (found by reading sigstore
0.14.0's own DSSE path; these are what the ADR should say):

1. **No `_type` check.** `bundle/verify/models.rs` deserializes the payload as
   `InTotoStatementV1` and reads `subject`. The `validate_cosign_v1()` method
   that enforces `_type == "https://in-toto.io/Statement/v1"` lives in
   `bundle/intoto.rs` and is called **only** from
   `cosign/signature_layers.rs:447` — never from the bundle verify path. So
   delegation would not enforce checklist rows about Statement type at all.
2. **No `payloadType` check.** Nothing in the bundle path asserts
   `payloadType == "application/vnd.in-toto+json"` before parsing the payload as
   an in-toto Statement.
3. **`subject[0]` only.** `InTotoStatementV1::subject_sha256_digest()` uses
   `.first()`; its own doc comment concedes "Go cosign and sigstore-go both
   iterate all subjects … we only consume `subject[0]` for now." A multi-subject
   attestation binds only on its first entry under delegation.
4. **Re-serialization, not received bytes.** `models.rs` computes
   `serde_json::to_vec(&dsse)` to derive `envelope_json` for the tlog comparison,
   not the bytes that arrived. That is a DATA-DIG-04 violation
   (`rust-quality/data-and-formats.md`: "Hash exactly the bytes received").
5. **`InTotoStatementV1` is `pub(crate)`** (`bundle/intoto.rs`), so OCX cannot
   reuse the parsed type even if it wanted the parse.

**Remediation.** Rewrite the L239 rejection cell to state (1)–(5) and delete the
two-trust-roots claim. Amend L241–245: the current text says sigstore-rs
provides "the raw ECDSA `verify_prehash`" — but DSSE signature verification is
over PAE with a `CosignVerificationKey::verify_signature(Signature::Raw(sig),
pae)` shape, not a prehash. Naming the wrong primitive in the delegation-boundary
paragraph is the same class of error as the rejection rationale.

---
## F2 — BLOCK — the ADR never says whether attestation mode still calls `verifier.verify`, and both answers are defective

**Severity:** Block. **Classification:** Actionable.
**ADR lines:** L241–245 (delegation boundary); L763–770 (`VerifyContentMode`);
L791–796 (`verify_envelope` signature); L324–336 (D-g accepted kinds).
**Attack surface:** 1(a)/1(b) — the boundary D-d actually draws.

**The gap.** D-d says "the DSSE gate becomes a mode check" and puts the DSSE
steps in `oci/verify/dsse.rs`. Part IV then specifies:

```rust
pub(super) fn verify_envelope(
    bundle: &Bundle,
    target_digest: &Digest,
    expected_predicate_type: Option<&PredicateType>,
    verifying_key: &CosignVerificationKey,   // <- where does this come from?
) -> Result<VerifiedAttestation, VerifyErrorKind>;
```

`verifying_key` is an input. In the shipped signature path OCX never holds one:
`verifier.verify(subject_bytes, bundle, &PolicyDeferredToOcx, true)`
(`pipeline.rs:382-385`) extracts the leaf key internally and, in the same call,
builds the chain, checks the SCT, and checks the integrated time against the
certificate's validity window — the comment at `pipeline.rs:366-372` enumerates
exactly that. The ADR does not say whether the attestation branch still makes
that call. Both readings break something, so this is not a documentation nit.

**Reading A — attestation mode still calls `verifier.verify`.** Then D-g's
accepted-kind set is dead on arrival. sigstore 0.14.0's DSSE branch routes
through `MaterialsWithTlogEntry::tlog_entry_for_dsse`
(`sigstore-0.14.0/src/bundle/verify/models.rs`), which does:

```rust
let kind = actual.get("kind").and_then(|v| v.as_str())?;
if kind != "dsse" { warn!(kind, "tlog entry kind is not 'dsse' for DSSE bundle"); return None; }
let api_version = actual.get("apiVersion").and_then(|v| v.as_str())?;
if api_version != "0.0.1" { ...; return None; }
```

D-g chose to accept `{dsse:0.0.1, intoto:0.0.2}`. An `intoto:0.0.2` entry is
refused by sigstore before OCX's own `verify_tlog_binding` ever runs, so the
`intoto:0.0.2` half of the decision cannot be exercised — an accepted kind with
no reachable green state, the mirror image of the unchecked-green argument D-g
itself uses to *reject* `hashedrekord:0.0.2`. The same call also re-derives the
envelope via `serde_json::to_vec(&dsse)` (`models.rs`, `BundleContent::Dsse`
construction), so its `envelopeHash` comparison runs against a re-serialization
— directly contradicting L780–781 ("Carries the ORIGINAL payload bytes …
never a re-serialization") and DATA-DIG-04.

**Reading B — attestation mode skips `verifier.verify` and extracts the key
itself from `parts.leaf_der`.** Then attestations get **no chain building, no
SCT verification, and no certificate-validity-window check**, because
`pipeline.rs:382` is the only site in the crate that performs any of them. An
attestation would be accepted on a Fulcio-shaped leaf certificate that chains to
nothing, carries no valid SCT, and whose Rekor `integratedTime` falls outside
its own validity window. That is a strictly weaker verification than `ocx
package verify` performs today on a message signature, offered to the user under
the same command.

**Refutation attempted.** I checked whether `BundleParts::from_bundle` might
already extract a usable key — it does not; it holds `leaf_der` and Rekor
material (`pipeline.rs:364`, `parts.leaf_der` used at `verify_policies`,
`pipeline.rs:394`). I checked whether sigstore exposes a chain-build-only entry
point OCX could call beside a hand-rolled DSSE step — `verify_digest` and
`verify` are the only public verification entry points; both route content
verification through the private `verify_bundle_content`. So there is no third
reading in which the ADR gets chain building without also getting sigstore's
DSSE content path.

**Remediation.** State the branch explicitly in D-d and in Part IV, and pick one
of:

1. Call `verifier.verify` and **narrow D-g to `dsse:0.0.1` only** — the accepted
   set then matches what the delegated path can actually reach, and the
   `intoto:0.0.2` row moves to Not Doing with sigstore's kind check as the
   stated reason. `verify_envelope` then re-checks `_type`, `payloadType` and
   all subjects on the *received* bytes, which is what delegation genuinely
   fails to cover (F1's list).
2. Skip `verifier.verify` and specify, in the ADR, where chain building, SCT
   verification and the validity-window check happen for attestations — naming
   the sigstore API each uses. A DSSE mode that silently loses three checks the
   signature mode performs is a Block regardless of how the code is factored.

Either way `verify_envelope`'s `verifying_key` parameter needs a documented
provenance in the ADR, since it is the seam the whole question turns on.

---
## F3 — BLOCK — the `builder` matcher names a SLSA v0.2 field path, but D-c's validation layer accepts only `>= v1.0`

**Severity:** Block. **Classification:** Actionable.
**ADR lines:** L445–447 (`builder` semantics); L211–217 (D-c); L639, L1153.
**Attack surface:** 2 — the `--type` table decision.

**The contradiction, in the ADR's own two sentences.**

L211–212 + L217 (D-c, chosen option):

> bare `slsaprovenance` resolves to `https://slsa.dev/provenance/v0.2`, while
> #102 wants `>= v1.0`. … **(chosen) cosign's table verbatim; enforce `>= v1.0`
> at the validation layer** … which is where it belongs, next to `builder`
> matching.

L445–447:

> **`builder` semantics.** An opaque string matched against the SLSA provenance
> predicate's `builder.id` during attestation verify.

`predicate.builder.id` is the **SLSA v0.2** field path. In SLSA v1.0 the builder
identity moved to `predicate.runDetails.builder.id`, and `buildType` moved to
`predicate.buildDefinition.buildType`. The two schemas do not share a single
field path for either value.

So the ADR specifies a matcher that reads a path present only in documents the
same decision's validation layer is specified to reject. Concretely: a policy
carrying `builder = "https://github.com/..."` verifying a v1.0 provenance
attestation reads `predicate.builder.id`, finds nothing, and either

- fails every provenance verify that declares a builder (fail-closed), or
- treats "field absent" as "no constraint" and admits any builder (fail-open —
  a policy that reads as enforcing and enforces nothing).

The ADR states neither, so a reader cannot tell which one ships. Grep confirms
the gap is not covered elsewhere: `runDetails` and `buildDefinition` appear
**zero** times in the ADR, and `builder.id` appears exactly once, at L446.

**Provenance of the error.** Issue [#103](https://github.com/ocx-sh/ocx/issues/103)
(revision 2026-08-20) has the same shape — "Extract `builder.id` and `buildType`
from the predicate" while citing `slsa.dev/spec/v1.0/provenance`. The ADR
inherited the v0.2-shaped prose along with the v1.0 citation rather than
resolving it. That is what makes it a Block rather than a typo: the design record
is the artifact that was supposed to catch this.

**Second-order.** Because D-c adopts cosign's table verbatim, `--type
slsaprovenance` labels a predicate `v0.2`. Nothing in the ADR validates that the
predicate *document* matches the resolved predicateType, so `--type
slsaprovenance` over a v1-shaped document produces a v0.2-labelled v1 payload —
and vice versa. L1153 pins the alias resolution in a test ("`slsaprovenance` →
v0.2 asserted **explicitly** so a later 'fix' to v1 …"), which locks the label
without locking any relationship between label and content.

**Refutation attempted.** I checked whether a version-dispatching accessor is
specified anywhere in Part IV — `predicate.rs` is described (L340-ish, D-i) as
"PredicateType, alias table, CosignPredicate wrapper", with no field-extraction
surface at all. No shape-aware accessor exists in the design.

**Remediation.** Pick one and write it into the ADR:

1. **Version-dispatching accessor.** Specify a `fn builder_id(predicate_type,
   predicate: &Value) -> Option<&str>` that reads `runDetails.builder.id` for
   `slsa.dev/provenance/v1` and `builder.id` for `v0.2`, and say which versions
   the `builder` policy field accepts. Then D-c's `>= v1.0` sentence and the
   matcher agree, and both shapes verify.
2. **Accept v1 only.** Correct L446 to `predicate.runDetails.builder.id`, and
   state that a `builder` policy against a v0.2 predicate is an explicit refusal
   with a named error — not a silent non-match.

In both cases the acceptance suite needs a case per shape; a single-shape fixture
is exactly the test that cannot distinguish fail-open from correct.

---
## F4 — BLOCK — the ADR specifies two annotations, tests three, and never decides on cosign's third (which is `time.Now()`)

**Severity:** Block. **Classification:** Actionable.
**ADR lines:** L77, L547–555 (constants), L1049 (Affected Surfaces), L1108
(golden shapes), L1177 (spike-may-adjust row 5).
**Attack surface:** 8 — what is missing; DATA-DET on referrer manifest bytes.

**The inconsistency.** Three places in the ADR, three different answers:

- L77 names cosign's "`dev.sigstore.bundle.content` / `dev.sigstore.bundle.predicateType` annotations" — **two**.
- L551–554 declares exactly two annotation *keys* plus two value constants:
  `ANNOTATION_BUNDLE_CONTENT`, `ANNOTATION_BUNDLE_PREDICATE_TYPE`,
  `BUNDLE_CONTENT_DSSE`, `BUNDLE_CONTENT_MESSAGE_SIGNATURE`. L1049 counts them:
  "4 annotation constants".
- L1108, golden shapes: "The referrer manifest OCX pushes (top-level
  `artifactType`, empty config descriptor actively pushed, one bundle layer,
  **the three annotations**)."

The golden-shapes fixture is pinned against a third annotation the constants
block does not name and the design never mentions again.

**What cosign actually writes.** `research_cosign_v3_attestation_wire.md` L21–26
quotes `WriteAttestationNewBundleFormat` verbatim:

```go
annotations := map[string]string{
    "org.opencontainers.image.created": time.Now().UTC().Format(time.RFC3339),
    "dev.sigstore.bundle.content":      "dsse-envelope",
    BundlePredicateType:                predicateType,
}
```

Three, and the third is a wall-clock read.

**Why this is a Block and not a doc nit.** The two candidate resolutions have
materially different consequences and the ADR picks neither:

1. **Write two.** OCX's referrer manifest then differs from cosign's for the same
   artifact. The interop test at L1100 (`cosign attest` → `ocx package verify
   --attestation`) still passes — it reads cosign's manifest — but the reverse
   direction is where a difference bites, and the golden fixture as worded
   (`the three annotations`) would be wrong on day one.
2. **Write three.** Then `org.opencontainers.image.created = time.Now()` lands
   in the referrer manifest, whose SHA-256 **is** the referrer's registry
   address. Two `ocx package attest` runs over byte-identical inputs then
   produce two different referrer digests. That is a
   `rust-quality/data-and-formats.md` DATA-DET-05 concern
   ("one fixed mtime from `SOURCE_DATE_EPOCH` or a constant … never
   `SystemTime::now()`"), and it makes the golden-shape fixture
   unpinnable byte-for-byte without a clock seam the ADR does not specify.
   S1-I idempotency reasoning (D-f, F6 below) also depends on a re-run
   converging, which a timestamped manifest cannot do.

The ADR's own escape hatch does not cover this. L1177, spike-may-adjust row 5,
scopes the spike to "the literal annotation *values* cosign writes →
`BUNDLE_CONTENT_DSSE` and siblings". A third *key*, and a decision about clock
determinism in a content-addressed manifest, is not a value edit — it is exactly
the class L1181 says the spike may not touch ("None requires a new type, a new
module, a new error family, a different pipeline, or a different storage
shape"). A nondeterministic referrer digest is a storage-shape decision.

**Refutation attempted.** I searched the ADR for any determinism carve-out or
clock injection: `SOURCE_DATE_EPOCH`, `created`, and `image.created` appear
nowhere (the only `image.created` hit in the tree is the research file).
`grep -n "annotation" adr_sbom_attestations.md` returns 18 hits; none reconciles
two against three.

**Remediation.** Decide in D1 (or a new sub-decision) and make all four sites
agree:

- If two: correct L1108 to "the two annotations", and record in Not Doing that
  OCX omits `org.opencontainers.image.created`, with the interop consequence
  stated.
- If three: add the key to the constants block, raise L1049's count to 5, and
  state the timestamp policy — a fixed constant, `SOURCE_DATE_EPOCH`, or an
  explicit acceptance that referrer digests are non-reproducible, with the
  golden fixture asserting every field *except* that one.

---
## F5 — BLOCK — `ReferrerManifest` cannot carry annotations, and the ADR does not list it as an affected surface

**Severity:** Block. **Classification:** Actionable.
**ADR lines:** L33, L44–46, L267–268, L1049 (Affected Code Surfaces).
**Code:** `crates/ocx_lib/src/oci/referrer/manifest.rs:25-46` and `:55-70`.
**Attack surface:** 6/8 — boundary and what is missing.

**What the ADR claims.** L44–46, in the "why this is cheap" delta list:

> The transport already carries `artifact_type` **and** `annotations` through
> `list_referrers` into `oci::Descriptor`. Annotation-based narrowing needs no
> transport change and no second fetch.

True — for the **read** side. `oci::Descriptor` (re-exported from
`external/rust-oci-client/src/manifest.rs`, `OciDescriptor`) carries
`annotations: Option<BTreeMap<String, String>>`, and the referrers listing
returns it.

**What the write side actually is.** `ReferrerManifest`
(`crates/ocx_lib/src/oci/referrer/manifest.rs:25-46`) has six fields —
`schema_version`, `media_type`, `artifact_type`, `config`, `layers`,
`subject`. **There is no `annotations` field.** Its constructor is

```rust
pub fn build(subject: Descriptor, artifact_type: &str, payload: Descriptor) -> Self
```

— no annotations parameter — and `to_canonical_json` (`:80-82`) is a plain
`serde_json::to_vec(self)`, so nothing can be injected downstream either.

**The consequence.** A registry's referrers listing surfaces the annotations
present on the *referring manifest*. OCX cannot put any there. So the
annotation-narrowed discovery the ADR specifies at L267–268 —

> client-side on the `dev.sigstore.bundle.content` annotation (`dsse-envelope`)
> and, when `--type` is given, on `dev.sigstore.bundle.predicateType`

— filters **every OCX-written attestation out of its own candidate set**. Signed
by ocx, discoverable by cosign, invisible to `ocx package verify --attestation`.
The narrowing works only against cosign-produced referrers, which is exactly the
direction the interop test (L1100) exercises, so the suite as designed would not
catch it.

The `content` annotation is worse than the predicate one: it is the *only*
discriminator between a signature referrer and an attestation referrer, because
both carry the same `artifactType`
(`research_cosign_v3_attestation_wire.md` L42–44: "Signature vs attestation
referrers share the SAME artifactType; discriminated only by
`dev.sigstore.bundle.content`"). Without it on the write side, OCX's own
signature and attestation referrers are indistinguishable in a listing.

**The design-record failure.** The Affected Code Surfaces table (L1024–1062)
lists `crates/ocx_lib/src/oci/referrer/media_types.rs` — the *constants* — and
has **no row for `referrer/manifest.rs`**. A builder following the ADR's
surface list adds four `&str` constants and never touches the type that must
carry them. The list is the artifact whose job is to prevent that.

**Refutation attempted.** I checked whether `Descriptor::annotations` on the
*bundle layer* descriptor could serve instead — it cannot: a referrers listing
reports the referring manifest's top-level `annotations`, not its layers'. I
also checked whether the pipeline might push a hand-built JSON body bypassing
`ReferrerManifest` — `to_canonical_json`'s doc comment (`:72-76`) states the
registry addresses the referrer by the SHA-256 of exactly those bytes, i.e. this
type is the push shape.

**Remediation.**

1. Add `#[serde(skip_serializing_if = "Option::is_none")] pub annotations:
   Option<BTreeMap<String, String>>` to `ReferrerManifest` and a constructor
   that takes them (`build_with_annotations`, or extend `build`). The
   `skip_serializing_if` is load-bearing: without it every existing *signature*
   referrer manifest gains an `"annotations": null` field, changing its bytes
   and therefore its digest — a wire-format break on a shipped path.
2. Add the `referrer/manifest.rs` row to Affected Code Surfaces, describing the
   field and the serialization-compat constraint above.
3. Decide, in the same edit, whether signature referrers also start carrying
   `dev.sigstore.bundle.content: message-signature`
   (`BUNDLE_CONTENT_MESSAGE_SIGNATURE` is already in the constants block at
   L554 with no stated writer). If yes, that is a byte change to a shipped
   manifest shape and needs saying out loud; if no, delete the constant or
   document it as read-side only.
4. Add a golden fixture asserting an **OCX-written** attestation referrer is
   selected by the annotation filter — the current fixture set only proves
   cosign's is.

---
## F6 — BLOCK — the byte-identity round-trip test cannot pass under the types the ADR specifies

**Severity:** Block. **Classification:** Actionable.
**ADR lines:** L829–836 (`AttestOptions`), L778–782 (`VerifiedAttestation`),
L466 (checklist row 2), L1101 (acceptance round-trip).
**Attack surface:** 7 — complexity/claim honesty.

**The claim.** L1101, acceptance suite:

> `ocx package attest` → `ocx package sbom --output` | Round-trip: the extracted
> bytes are **byte-identical** to the input predicate (checklist row 2 — this is
> the seam, not a source scan).

Row 2 (L466) is the normative requirement behind it: "Never re-parse or
re-serialize the verified payload before downstream use — pass the original
bytes forward."

**Why it cannot hold.** Two independent re-serializations are baked into Part IV's
own type choices.

*On the write side*, L833:

```rust
pub struct AttestOptions {
    ...
    pub predicate: serde_json::Value,
}
```

The predicate reaches the pipeline as a parsed `Value`. Whitespace, indentation
and trailing newline are gone; object key order is normalized to `serde_json`'s
map ordering. Whatever the Statement embeds is `serde_json`'s serialization of
that `Value`, not the bytes of the file the user passed to `--predicate`. A
pretty-printed CycloneDX document — the overwhelmingly common input — cannot
survive.

*On the read side*, L779–781:

```rust
pub struct VerifiedAttestation {
    pub predicate_type: String,
    pub payload: Vec<u8>,          // original decoded Statement bytes
    pub subject_digest: Digest,
}
```

`payload` is the **Statement**, correctly kept verbatim. But `sbom --output`'s
contract (L288) is to "Write the verified predicate document verbatim — the
bytes from inside the envelope". The predicate is a *sub-object* of the
Statement. Extracting it from `Vec<u8>` requires either
`serde_json::value::RawValue` (which preserves the sub-slice's bytes) or a
parse-and-re-serialize. The ADR specifies neither, and `VerifiedAttestation`
carries no field that could hold the raw sub-slice. Under the default
`serde_json::Value` route the sub-object is re-serialized, which is precisely
what row 2 forbids — inside the module row 2 names as its enforcement site.

**What the round-trip would actually prove as designed.** `attest` normalizes
the predicate through `Value`; `sbom --output` re-serializes the same sub-object
through `Value` again. The two normalizations agree, so the test **passes** —
against the normalized form, not the input file. It is green whether or not the
implementation preserves anything, which makes it an unchecked green in the
`quality-core.md` sense: a check whose passing state is indistinguishable from
the check never having run. The ADR asserts it is "the seam, not a source scan",
which is the strongest possible framing for a test that cannot fail on the
property it names.

**Refutation attempted.** I checked whether row 2's own proof wording rescues it
— it reads "`sbom --output` byte-compares against the fixture the attest step
wrote", which is satisfiable if "wrote" means the normalized embedded form. But
L1101 says "the input predicate", and the two sentences describe different
tests. One of them is wrong; the ADR does not say which. I also checked whether
`RawValue` appears anywhere in the design — it does not.

**Remediation.** Pick the property and make types and test agree:

1. **Preserve input bytes.** Change `AttestOptions.predicate` to `Vec<u8>` (or
   `Box<RawValue>`), validate it parses without materializing a normalized copy,
   and embed the original slice. Add a raw-predicate field to
   `VerifiedAttestation` so `sbom --output` can hand back the sub-slice without
   re-serializing. Then L1101's test means what it says.
2. **Drop the byte-identity claim.** Keep `Value`, and restate L1101 as
   "semantically equal after JSON normalization", with a fixture whose input is
   deliberately *not* in normalized form so the weaker claim is still falsifiable.
   Row 2 then applies only to the Statement payload, which it already does
   correctly, and the ADR must say so rather than implying predicate-level
   byte fidelity.

Option 1 is the one row 2's threat model actually wants; option 2 is honest but
gives up a property the checklist calls normative.

---

## F7 — BLOCK — checklist row 13 names an enforcement site that does not perform the check

**Severity:** Block. **Classification:** Actionable.
**ADR line:** L484 (Part III, row 13).
**Code:** `crates/ocx_lib/src/oci/verify/tlog.rs`; `crates/ocx_lib/src/oci/verify/pipeline.rs:366-372`.
**Attack surface:** 7/8 — claim honesty; what is missing.

**The claim.** Row 13:

> | 13 | Assert `NotBefore <= integratedTime <= NotAfter` as OCX's own check, not
> a library default | shipped (`verify/tlog.rs`), unchanged | Existing coverage;
> re-asserted for the DSSE path |

Part III's own preamble (L461) states the rule this violates: "Every row names
where it is enforced and how it is proven. **A row with no enforcement site is a
defect, not a note.**"

**The check is not there.** `crates/ocx_lib/src/oci/verify/tlog.rs` contains zero
occurrences of `not_before`, `not_after`, `NotBefore`, `NotAfter` or `validity`.
Its module doc (`:4-18`) states its scope precisely: the Signed Entry Timestamp
and the inclusion proof, with "No cryptography is computed here" — SET
verification and Merkle audit path, delegated to `CosignVerificationKey` and
`InclusionProof::verify`.

The validity-window check is performed by **sigstore's** `Verifier`, and the
codebase says so at `pipeline.rs:366-372`:

> the Rekor entry body is rebuilt … and compared to the logged body … and **the
> integrated time is checked to fall inside the certificate's validity window**.

So row 13 asserts the opposite of the truth twice over: the check is a library
default (sigstore's), not OCX's own, and it does not live in the module named.

**Why this compounds F2.** Row 13 is the reason a reader would conclude the
validity-window check survives a DSSE branch that bypasses `verifier.verify`:
`verify/tlog.rs` is "unchanged", so the check appears carried over for free. It
is not. If the attestation path skips `verifier.verify` (F2 Reading B), row 13
becomes a requirement with **no enforcement site anywhere** — the exact defect
Part III's preamble defines.

"Proven by: Existing coverage" is the second half of the problem. There is no
named test, so the claim is unfalsifiable as written; a reader cannot check it
without doing what I just did.

**Refutation attempted.** I grepped the whole `verify/` directory listing and
read `tlog.rs`'s module documentation in full before writing this, on the
assumption the check was under a name my pattern missed. It is not present under
any spelling of the four terms, and the module's own stated scope excludes it.

**Remediation.** Correct row 13 to name `sigstore::bundle::verify::Verifier`
(via `pipeline.rs:382`) as the enforcement site, name the specific test in the
"Proven by" column, and — as part of F2's resolution — state explicitly what
performs it in attestation mode. If the answer is "the same call", say so; if
the attestation branch bypasses it, row 13 needs a new enforcement site written
into the design, not inherited.

---
## F8 — WARN — D-f's cross-tool compatibility argument names the wrong type and a path that does not exist

**Severity:** Warn. **Classification:** Actionable.
**ADR lines:** L303–308 (D-f, `platform_digests`); L1055 (Affected Surfaces).
**Code:** `crates/ocx_lib/src/publisher.rs:36-66`; `crates/ocx_cli/src/api/data/push.rs:25-47`.
**Attack surface:** 4 — `PushOutcome` cross-tool contract.

**What the ADR says.** L307–308:

> `PushOutcome` is a cross-tool contract (ocx-mirror parses it) — the addition
> is additive-only and the existing fields are untouched.

and L1055, Affected Surfaces:

> `crates/ocx_lib/src/oci/publish/…` (`PushOutcome`) | **additive**
> `platform_digests: BTreeMap<Platform, Digest>` (D-f)

**Three factual corrections.**

1. **`crates/ocx_lib/src/oci/publish/` does not exist.** `ls crates/ocx_lib/src/oci/`
   returns `client/ digest/ identifier/ index/ platform/ referrer/ sign/ verify/`
   and no `publish`. `PushOutcome` is at `crates/ocx_lib/src/publisher.rs:42` —
   a top-level module, not under `oci/`.
2. **`PushOutcome` is not a wire type.** It derives `Debug` and nothing else
   (`publisher.rs:41`). It cannot be parsed by anything.
3. **The actual cross-tool contract is `PushReport`**
   (`crates/ocx_cli/src/api/data/push.rs:25`), which derives `Serialize` and
   whose doc comment states it outright: "The first five keys are the
   machine-readable contract consumed by `ocx-mirror pipeline push`, which keys
   its go/no-go bookkeeping off `status` and records `cascade_tags_written` in
   the run summary."

**Does the conclusion survive?** Yes — and that is worth saying plainly rather
than treating the finding as fatal. `platform_digests` is needed *in process*,
so `push --sbom` can name the per-platform subject under `--no-canonical-tag`;
it does not need to reach ocx-mirror at all. Because ocx-mirror consumes
`PushReport` and `PushReport` is unchanged, the compatibility claim holds. The
reasoning is wrong; the answer is right.

**The residual worth checking.** `PushOutcome` is not `#[non_exhaustive]`, and
ocx-mirror takes `ocx_lib` as a path dependency. Adding a public field to a
non-exhaustive struct breaks any downstream struct-literal construction — which
for an output type is unlikely outside tests, but is a compile-time break, not a
wire one, so the "additive-only" framing does not cover it. Two lines close it:
mark `PushOutcome` `#[non_exhaustive]` in the same change, or state in the ADR
that ocx-mirror was checked for construction sites.

**Remediation.** Correct the path to `crates/ocx_lib/src/publisher.rs`. Replace
the "ocx-mirror parses it" sentence with the true relationship: `PushReport` is
the parsed contract and is unaffected. Then decide and record whether
`platform_digests` should *also* surface in `PushReport` — if `push --sbom` is
meant to report which platform got which attestation, `api/data/push.rs` is a
missing Affected Surfaces row and a real (additive) wire change.

---

## F9 — WARN — "convergent" mischaracterizes the append-only re-attest semantics the ADR inherits

**Severity:** Warn. **Classification:** Actionable.
**ADR line:** L310–314 (D-f, failure atomicity).
**Source:** `adr_oci_referrers_signing_v1.md:502-510` (Decision S1-I).
**Attack surface:** 4 — is partial-attest state re-runnable?

**The claim.** L311–312:

> The push is **not** rolled back: a pushed manifest is immutable, OCI offers no
> un-push, and re-running `ocx package attest` against the same digest is
> **convergent**.

**What S1-I actually decided.** `adr_oci_referrers_signing_v1.md:504`:

> **Chosen:** Each invocation writes a new signature as an additional referrer.

with the con recorded in the same table: "Referrer list grows over re-signs;
cleanup is GC's job, not sign's". The two rejected options were "Replace
existing referrer" and "Append only if no existing valid signature" — i.e. the
design deliberately rejected both forms of convergence.

Attestation inherits this: `AttestPipeline`'s step order (L755–758) ends "push
bundle blob → push referrer manifest", with no listing, no dedupe, and no
existing-referrer check. Each ephemeral keypair yields a different certificate,
a different Rekor entry and therefore a different bundle blob and a different
referrer digest. Re-running does not converge to one state; it adds one.

**Why the word matters here.** It is doing load-bearing work in an *atomicity*
decision. A reader building retry automation on "convergent" writes a
retry-until-success loop; under append-only semantics an intermittently failing
attest step accumulates one attestation referrer per attempt, all of them valid,
all of them discoverable, and none of them wrong — so nothing ever surfaces the
accumulation. `MAX_ATTESTATION_CANDIDATES = 32` (L521) is then reachable by
retries alone, at which point verify starts refusing with `TooManyAttestations`
on a subject whose only sin was a flaky CI job.

**Refutation attempted.** I checked `crates/ocx_lib/src/oci/sign/pipeline.rs`
for any existing-referrer check that would make the claim true for signatures —
`list_referrers` appears once (`:571`, `:579`) as the pipeline's own helper, with
no call from the push path and no dedupe logic anywhere. I also re-read S1-I in
full to be sure "convergent" was not a later amendment; it is not.

**Remediation.** Replace "convergent" with the accurate property: *idempotent in
outcome, additive in state* — re-running always yields a valid attestation for
the digest, and leaves the previous attempt's referrer in place (S1-I). Then say
whether that is acceptable for a retry loop, and if `MAX_ATTESTATION_CANDIDATES`
is meant to bound it, say what a user does when they hit it.

---

## F10 — WARN — attest has no offline refusal, while the ADR claims it mirrors `sign_one` exactly

**Severity:** Warn. **Classification:** Actionable.
**ADR lines:** L826 ("Mirrors `SignOptions` / `SignReport` / `sign_one` exactly");
L829–836 (`AttestOptions`); L873–893 (`PackageAttest`); L755–758 (step order).
**Code:** `crates/ocx_cli/src/command/package_sign.rs:122-132`;
`crates/ocx_lib/src/oci/sign/error.rs:137`, `:216`.
**Attack surface:** 8 — offline attest semantics.

**The shipped policy the ADR claims parity with.** `package_sign.rs:122-132`:

```rust
// S1-E policy: offline sign is a deliberate rejection, NOT a passive
// network-access failure. Route through `SignErrorKind::OfflineSignRefused`
// so the exit-code classifier returns 77 (PermissionDenied). This
// short-circuits before we touch the token-resolution path: the acceptance
// test `test_sign_offline_refused` drives this contract.
if context.is_offline() {
    return Err(anyhow::Error::from(SignError::new(
        identifier,
        SignErrorKind::OfflineSignRefused,
    )));
}
```

**The gap.** Nothing in the attest design reproduces it:

- `AttestOptions` (L829–836) has no `offline` field — while its sibling
  `SbomOptions` (L845–853) does have `pub offline: bool`, so the asymmetry is
  visible inside Part IV itself.
- `PackageAttest` (L873–893) declares no offline handling.
- The step order (L755–758) lists "SSRF floor on both trust URLs, resolve the
  per-platform target, index indirection, referrers capability probe, token
  acquisition" — no policy gate.
- The ADR's own reuse note (L895–901) moves only the **token resolver** to
  `package_sign_common.rs`. The offline refusal is not in the resolver: it sits
  in `execute()` at `:127`, deliberately *before* `resolve_override_token` is
  called at `:136`. Reusing the resolver verbatim therefore reuses everything
  except this.

**Consequence.** `ocx --offline package attest …` built to this ADR proceeds
past the gate, dials Fulcio, and fails with a transport error — exit 69 or 75
depending on classification — where `ocx --offline package sign` returns a
deliberate exit 77 with a policy message. Two sibling commands doing the same
keyless-signing work answer the same user error two different ways. Not a
security bypass (an offline attest cannot succeed either way), but a contract
divergence under an explicit claim of exact parity.

**Refutation attempted.** I checked whether `offline` might reach the pipeline
through `AttestContext` instead (L718–730) — it carries `state`, `index`,
`no_cache`, both URLs, and no offline flag. I checked whether the root
`--offline` might be enforced centrally before dispatch — `package_sign.rs`'s
own inline check at `:127` is evidence it is not.

**Remediation.** Either add the `context.is_offline()` gate to
`package_attest.rs` mirroring `:127-132` (and say so in the step order), or
state in D-f/Not Doing that attest deliberately reports a transport failure
rather than a policy refusal — and correct "Mirrors … exactly" either way. If
the gate is added, the extraction to `package_sign_common.rs` should take the
refusal along with the resolver, since forking it is exactly what that
extraction exists to prevent.

---

## F11 — WARN — `MAX_PREDICATE_FILE_BYTES == MAX_STATEMENT_PAYLOAD_BYTES` cannot deliver the invariant its comment claims

**Severity:** Warn. **Classification:** Actionable.
**ADR lines:** L515–517, L529–531 (constants block).
**Attack surface:** 7 — are the five `MAX_*` constants each a real distinct role?

**The claim.** L529–531:

```rust
/// Local `--predicate` file. Matches MAX_STATEMENT_PAYLOAD_BYTES so a predicate
/// that attest accepts can always be read back by verify.
pub(crate) const MAX_PREDICATE_FILE_BYTES: usize = 16 * 1024 * 1024;
```

against L515–517:

```rust
/// Decoded in-toto Statement payload. ...
pub(crate) const MAX_STATEMENT_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;
```

**Why the invariant fails at the boundary.** The Statement payload is not the
predicate — it is the predicate *plus* the in-toto wrapper: `_type`,
`predicateType`, and a `subject` array carrying at least one name and a
`sha256` DigestSet. That is a few hundred bytes minimum. So a predicate file of
exactly `MAX_PREDICATE_FILE_BYTES` produces a Statement payload strictly larger
than `MAX_STATEMENT_PAYLOAD_BYTES`, and verify refuses it with
`AttestationPayloadTooLarge` — the precise outcome the comment says cannot
happen. The window is narrow (the top few hundred bytes of a 16 MiB range) and
therefore exactly the kind no fixture will exercise unless someone writes it
deliberately.

F6 widens the window in an unpredictable direction: with `AttestOptions.predicate:
serde_json::Value`, the embedded bytes are a re-serialization of the file, which
may be smaller (whitespace stripped) or larger (escaping, key reordering has no
size effect but non-canonical numeric forms do). So the file-size cap does not
bound the embedded size in either direction with certainty.

**Refutation attempted.** I checked whether the payload cap might be applied to
the predicate rather than the Statement — checklist row 16 (L488) says "A
separate decoded-payload cap, checked from the base64 length **before**
allocating the decode buffer", and the base64 in a DSSE envelope encodes the
whole Statement, not the predicate. The cap is on the Statement.

**This is the only one of the five constants I could fault.** The other four
each carry a distinct, non-overlapping role — per-envelope bytes, decoded
payload, candidate count, cumulative budget — which is exactly the shape
`package-manager-domain.md` PKG-05/06/11 require, and each has a rationale, a
configurability decision, a named error variant and a fixture. The
Consequences section states this itself and the statement holds.

**Remediation.** Set `MAX_PREDICATE_FILE_BYTES` below
`MAX_STATEMENT_PAYLOAD_BYTES` by a stated wrapper allowance (e.g. 15 MiB, with
the comment naming the reserve and why), or drop the claimed invariant and say
the two caps are independent. Add a boundary fixture at
`MAX_PREDICATE_FILE_BYTES` exactly — a cap whose off-by-wrapper edge is never
tested is a cap nobody has seen go red.

---

## F4 addendum — a fourth data point, and it makes L1108 the outlier

L757–758, in the `AttestPipeline` step order, says the pipeline pushes "the
referrer manifest with **the two DSSE annotations**". That is the fourth site
and the third to say two (with L77 and the L551–554 constants block); L1108's
"the three annotations" now stands alone. This makes the *intent* legible —
two — but it does not resolve F4, because two is the reading that silently
diverges from cosign's wire shape, and nothing in the ADR records that as a
decision. The fix is still to state it in D1 and reconcile all four sites; the
addendum only says which way the evidence leans.

---
## Refuted — concerns raised by the review brief that the ADR survives

Recorded because a review that reports only hits is indistinguishable from a
review that only looked where it expected to find something. Each item below was
investigated against code, then dropped.

**"D-e always-verify makes `ocx package sbom` unusable with no policy."**
Refuted. `PackageSbom` (L903–938) carries `--certificate-identity`,
`--certificate-oidc-issuer` and `--sigstore-trusted-root`, so the no-policy user
has a documented single-invocation path, and `SbomOptions.policies` (L847) is
the same resolved-policy vector `verify` uses. `NoIdentityProvided` → exit 64 is
the same contract `package verify` already ships, so the failure mode is
consistent rather than novel. The ADR states the cost in Consequences. The
stance is defensible and honestly priced.

**"The 12 new `VerifyErrorKind` variants are YAGNI."** Refuted. I mapped each of
L975–1010 to a distinct enforcement site in the Part III checklist, and to a
distinct *user action*: a payload-type mismatch, a subject-binding failure, a
predicate-type mismatch and a cap trip lead to four different next steps for the
operator. Eleven of twelve share exit 65 and one takes 79, so the exit-code
surface does not grow with the variant count — which is the shape
`quality-rust-errors.md` asks for (structure carries the distinction, the exit
code carries the class). No collapse recommended.

**"`PredicateType` enum vs string."** Refuted as a finding. The set is closed at
the *keyword* layer (the `--type` table) and open at the *URI* layer (full URI
passed verbatim). The ADR's design carries both, which is what `IDIOM-03`
prescribes: an enum where the crate enumerates the valid values, a string where
it does not. Note that F2 attacks what the enum *maps to*, not whether it should
exist.

**ARCH-16 (dependency direction) and ARCH-17 (module naming).** Refuted.
`oci/attest` as a format leaf with a one-way `verify → attest` edge runs the
same direction as the shipped `oci/sign` → `oci/verify` relationship, and
`sbom.rs` at top level with no `oci` dependency matches the `trust.rs` precedent
the ADR cites. `name.rs` + `name/` throughout; no `mod.rs` introduced.

**ARCH-03 (god-struct growth).** Refuted. The added surface lands on new types
(`AttestPipeline`, `AttestContext`) rather than on `SignPipeline` /
`VerifyPipeline`, and neither existing type crosses two inherent `impl` blocks or
25 methods as a result.

**"No `--format json` for attest."** Refuted. `--format` is a root flag
(`CLI-10`) and the ADR routes attest output through the existing `Printable` DTO
path, so JSON is inherited rather than omitted.

**"Multi-platform attest batching / `BatchReport`."** Refuted as scoped out. The
ADR authors one attestation per resolved platform target, single-platform per
invocation; `PKG-21`'s batch-report obligation does not attach to a
non-batch command. F8's `platform_digests` question is about the *subject*
lookup, not about batching.

**"Capability-cache interaction for attest."** Refuted. `AttestContext` carries
`state: &StateStore` and `no_cache` (L718–730), which is the same pair the
shipped referrers capability probe uses.

**Consequences and Not Doing (L1196–1262).** Read in full and adversarially.
The negative-accepted list names the real costs (append-only referrer growth,
the cap surface, the second trust-root URL) and the Not Doing table names the
right exclusions with reasons rather than silence. No manufactured optimism
found.

---

## Reversibility honesty (attack surface 5)

The ADR's "spike MAY adjust" / "MAY NOT adjust" split (L1168–1195) is a genuine
reversibility statement and most of it is correctly assigned. Two corrections:

**The unnamed one-way door is the annotation set.** Once a referrer manifest is
pushed with two annotations rather than three, those bytes are immutable and
their digest is the referrer's identity. Changing the set later does not migrate
anything — it forks the wire shape between everything published before and
after. That is the sharpest one-way decision in the ADR and it appears in
neither list, while being stated four inconsistent ways in the body (F4).

**D-h's "pre-release, so free to rename" is false for `rekor_unavailable`.** The
slug is shipped and asserted: `crates/ocx_cli/src/error_envelope.rs:307`
(`ErrorCategory::RekorUnavailable => "\"rekor_unavailable\""`),
`crates/ocx_lib/src/oci/verify/error.rs:395`, with tests pinning it at
`cli/classify.rs:695` and `cli/exit_code.rs:180`. `CLI-04` makes error slugs a
stable contract and `EXIT-06` makes exit-code meanings append-only; exit 83 is
allocated and its slug is observable by any script that already runs
`ocx package verify`. Renaming it to `transparency_log_unavailable` is a
breaking change to a shipped surface, which is permitted pre-1.0 but is a
changelog-bearing decision, not a free edit. Either keep the slug and widen only
the human-facing text, or make the break explicit and own the commit subject.

Correctly assigned as reversible, and I agree: the module layout, the constant
values, the `--type` keyword table (the *keywords*; the URIs they map to are
wire-visible via the signed payload and are not).

---

## Summary

```
Summary: Needs Work
Focus:   quality (adversarial design review)
Subject: .claude/artifacts/adr_sbom_attestations.md
Findings: 11 (7 Block, 4 Warn) + 1 addendum
Actionable: 11
Deferred:  0
```

**Verdict — Needs Work, not Fail.** The ADR is unusually well researched, its
Part III checklist is the right artifact, its Consequences are honest, and eight
separate lines of attack found nothing (see Refuted). What it cannot do yet is
be handed to an implementer: seven Block findings are places where the document
states something the code contradicts, or leaves a decision unstated that the
implementer must then invent — and inventing it wrong is silent in each case.

**The Block set, in the order they should be fixed:**

- **F1** — the sigstore-rs delegation rejection rests on a "two trust roots"
  claim the code refutes (`oci/verify/trust_root.rs:301` already implements
  `sigstore::trust::TrustRoot`), while the real disqualifying reasons go
  unrecorded. Wrong rationale, right conclusion — and the delegation doctrine
  makes the rationale load-bearing.
- **F2** — default `slsaprovenance` attaches v0.2, whose builder identity lives
  at `predicate.builder.id`; the ADR's `builder` matching (L445–447) and #102's
  `>= v1.0` policy read the v1 shape. Default attach produces provenance the
  default verify cannot builder-match.
- **F3** — the attestation branch's relationship to `verifier.verify` is
  unspecified, and both readings break something concrete (sigstore 0.14's
  `tlog_entry_for_dsse` hard-rejects any kind but `dsse:0.0.1`, so D-g's
  `intoto:0.0.2` is unreachable through it).
- **F4** — the annotation set is stated four different ways (two/two/two/three),
  and `ReferrerManifest` (`oci/referrer/manifest.rs`) has no `annotations` field
  at all, so the write side cannot emit any of them.
- **F5** — checklist row 13 names `verify/tlog.rs` as the enforcement site for
  the `NotBefore <= integratedTime <= NotAfter` check; that file performs no
  certificate-validity check (its own doc: "No cryptography is computed here").
  Under the checklist's own preamble — "a row with no enforcement site is a
  defect, not a note" — this is a defect by the ADR's own rule.
- **F6** — `AttestOptions.predicate: serde_json::Value` cannot satisfy the
  byte-identity round-trip the test plan asserts (L1103).
- **F7** — the `VerifiedAttestation.payload` "never a re-serialization"
  invariant is contradicted by the path the design routes through.

**The Warn set** — F8 (wrong type and nonexistent path in the `PushOutcome`
compat argument), F9 ("convergent" vs inherited S1-I append-only), F10 (no
offline refusal under a claim of exact `sign_one` parity), F11
(`MAX_PREDICATE_FILE_BYTES` boundary invariant off by the Statement wrapper) —
are each a short edit, but F10 and F11 also want a test that has been seen red.

**Zero deferred.** Every finding names a concrete fix that needs no human
judgment call: correct a factual claim, state an unstated decision, or add a
field. The one place I would flag human attention rather than a fix is D-h's
slug rename, which is a legitimate pre-1.0 break and a product decision — but
the *finding* (that "pre-release, so free" is false) is itself actionable.
