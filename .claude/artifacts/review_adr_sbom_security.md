# Security Review — ADR: SBOM and DSSE Attestations over OCI Referrers

- **Subject:** `.claude/artifacts/adr_sbom_attestations.md` (1262 lines, Proposed, 2026-08-20)
- **Focus:** SECURITY (mandatory — `crates/ocx_lib/src/oci/**` is always security-sensitive)
- **Threat model:** malicious registry content + malicious predicate files + hostile network
- **Baseline:** `research_dsse_verification_pitfalls.md` (20-row checklist, 4 CVEs), `research_cosign_v3_attestation_wire.md`, `research_rekor_dsse_entry_kinds.md`, `discover_attest_architecture_map.md`
- **Binding rules:** `security.md` (SEC-*), `package-manager-domain.md` (PKG-*), `platform-and-paths.md` (PLAT-01/02), `data-and-formats.md` (DATA-DIG/DET)
- **Reviewer:** opus (per CLAUDE.md model policy — security review is non-negotiably opus)

Findings are appended below as they are confirmed. Each carries: severity, ADR line,
attack scenario as concrete inputs -> wrong outcome, and a fix. Every finding was
subjected to a refutation attempt before being written.

Severity: **Block** = must be resolved before the plan is decomposed. **Warn** =
should be resolved, negotiable with owner. **Note** = observation, no action forced.

---

## Block findings

### B1 — `system_locked` is deleted from `TrustPolicy`, removing the anti-escalation boundary

**Severity:** Block · **ADR:** lines 400–415 (D-j "Serde shape") · **Rule:** `adr_trust_policy.md` system_locked amendment; SEC-32

The D-j struct is presented in full — doc comment, every field, an explicit
`// future: pub key` placeholder — and it does **not** carry
`system_locked`. The ADR uses an explicit elision marker elsewhere when it
means elision (`// ... existing fields unchanged ...`, line 771), so the
omission reads as deliberate.

Shipped code, `crates/ocx_lib/src/trust.rs:250-255`:

```rust
    /// ... a managed-config payload that writes `system_locked = true` is
    /// parsed as an unknown key and dropped, so it cannot promote itself.
    #[serde(skip)]
    #[schemars(skip)]
    pub system_locked: bool,
```

and `trust.rs:398-430` — `resolve()` gives a matching locked policy sole
governance: every unlocked match is dropped whatever its scope.

**Attack scenario.** Operator pins `/etc/ocx/config.toml`:
`scope = "ghcr.io/acme"`, `identity = "release@acme.example"` (system tier →
`system_locked = true` via `lock_as_system`). A local user writes
`~/.config/ocx/config.toml` with `scope = "ghcr.io/acme/tool"` (longer literal
prefix) and their own identity. With `system_locked` present, `resolve()`
returns only the locked entry and the user policy is dropped. With the field
deleted, the longest-prefix rule elects the user policy and
`ocx package verify ghcr.io/acme/tool:1.0` accepts an artifact signed by the
attacker — and `auto_verify` fires on every install surface, so this lands on
`ocx install` too, not only on an explicit verify.

**Refutation attempted:** could `resolve()` keep reading a field the schema
type no longer declares? No — `resolve()` takes `&TrustPolicy` and reads
`policy.system_locked` directly; deleting the field is a compile error, and the
cheapest fix an implementer reaches for is deleting the branch.

**Fix.** Carry `system_locked` verbatim into the nested shape, keep both
`#[serde(skip)]` and `#[schemars(skip)]`, and state in D-j that the existing
anti-escalation test (a managed/project payload cannot set it) is extended to
the nested form rather than replaced. Also state which of `builder` / `keyless`
a locked policy governs — see W3.

---

### B2 — Row 12's `envelopeHash` recomputation is not achievable from a Sigstore bundle

**Severity:** Block · **ADR:** lines 335–337, 476, 576–581, 800–804 · **Rule:** DATA-DIG-04; checklist row 12 (GHSA-8gw7-4j42-w388)

The ADR makes envelope-hash binding normative (row 12) and specifies the
mechanism as "recompute `sha256(envelope_json_bytes)` … must equal
`envelopeHash` inside the canonicalized body" (line 335), implemented by

```rust
pub(super) fn verify_tlog_binding(entry: &BundleParts, envelope_json: &[u8], payload: &[u8]) -> …
```

**The ADR never says where `envelope_json` comes from on the verify path, and
there is no source for it.** What Rekor hashed is
`proposedContent.envelope` — a *stringified DSSE envelope JSON* produced by the
signer's DSSE library (`research_rekor_dsse_entry_kinds.md` §1, §6). What a
verifier holds is the bundle, whose `dsseEnvelope` is a **structured
protobuf-JSON object** (`{payload, payloadType, signatures[]}` —
`research_cosign_v3_attestation_wire.md` §2), parsed by
`crates/ocx_lib/src/oci/sign/bundle.rs:172` into a typed `Bundle`. Those are two
different serializations of one logical envelope. Reconstructing the first from
the second is a re-serialization whose byte-identity depends on key order,
whitespace, base64 padding and whether an empty `keyid` is emitted — none of
which is specified anywhere.

**Scenario (inputs → outcome).** `cosign attest --type cyclonedx` publishes an
attestation; `ocx package verify --attestation` fetches it. OCX reconstructs the
envelope JSON, computes a hash that differs from `envelopeHash` by one byte of
whitespace, and returns `EnvelopeHashMismatch` (65). Every cosign-produced
attestation fails — the exact interop criterion D1 exists to satisfy (line 58).
The cheapest implementation-time fix is to stop comparing `envelopeHash` and
compare `payloadHash` only, which is *precisely* the splice the row exists to
prevent.

**Refutation attempted.** (a) Extract the `dsseEnvelope` sub-object's byte range
out of the raw bundle blob? That yields the bundle's serialization, not the one
uploaded to Rekor — still not the hashed bytes. (b) OCX signs its own
attestations, so it holds the uploaded bytes? True on the sign path only; a
verify run fetches the bundle from the registry and is in the same position as
any other verifier. (c) Row 12's parenthetical "(or both hashes)" as an escape?
Both options named contain `envelopeHash`; neither is reachable.

**Fix.** Replace the envelope-hash mechanism with what the canonicalized body
actually permits: compare the server-returned body's `signatures[]` **and**
`payloadHash` against the bundle's DSSE envelope — signature + payload together
bind the presented signature to the logged entry, which is the property
GHSA-8gw7-4j42-w388 demands, without requiring byte-identical envelope
reconstruction. Keep the envelope-hash check on the **sign** path, where OCX
does hold the uploaded bytes, as a self-consistency assertion. Restate row 12 in
those terms and name the negative fixture accordingly (a body whose
`signatures[]` does not match the bundle's, currently untested).

---

### B3 — `intoto:0.0.2` is accepted on an unsourced claim, and its acceptance branch has no reachable red state

**Severity:** Block · **ADR:** lines 329–338, 543 · **Rule:** quality-core "Unchecked Green"; checklist row 12

Line 331 asserts "Both accepted kinds carry a hash that binds the full
envelope"; line 338 reduces `intoto:0.0.2` to "`hash` / `payloadHash`, same
shape". Neither carries a citation.

The research the ADR builds on quotes `dsse:0.0.1`'s `Canonicalize()`
**verbatim from source** (`research_rekor_dsse_entry_kinds.md` §3) and states
exactly what it commits to. For `intoto:0.0.2` it gives only schema field names
(§1) — never the canonicalization function, never what `hash` is computed over.
So the ADR's load-bearing claim for the second accepted kind rests on nothing,
on the one row that closes a CVE class.

Second, independent problem: cosign writes `dsse` (§2), and the compose stack is
the only producer the acceptance suite has. Nothing in the pinned environment
produces an `intoto:0.0.2` entry, so the acceptance branch for that kind can
never be exercised — while `UnsupportedTlogEntryKind` for `intoto:0.0.1` *is*
tested (line 1127). **D-g rejects `hashedrekord:0.0.2` on exactly this ground**
("Accepting a kind with no reachable red state is an unchecked green", line 330)
and then accepts `intoto:0.0.2` without applying the same test.

**Scenario.** A producer (or a registry able to substitute the bundle's
`tlogEntries[]`) presents an `intoto:0.0.2` entry whose canonicalized body does
not commit to the presented signature. OCX accepts the kind, runs a binding
check written against an assumed field meaning, and either passes an unbound
entry or fails every honest one — and no test in the suite can tell which,
because the branch never executes.

**Fix.** Either (a) cite `rekor pkg/types/intoto/v0.0.2/entry.go`'s
`Canonicalize()` and state precisely what `hash` covers, **and** name a fixture
that produces such an entry against rekor 1.4.2, or (b) drop `intoto:0.0.2` from
`ACCEPTED_TLOG_KINDS` for v1 and add it when the spike can produce one — a
one-row table edit, already anticipated by Part V row 6.

---

### B4 — Annotation-based narrowing contradicts the ADR's own invariant and hands the registry a silent false negative

**Severity:** Block · **ADR:** lines 266–279 (D-e) · **Rule:** SEC-32; checklist row 7 (CVE-2022-35929), row 20

D-e specifies: list referrers with an empty artifactType filter, then "narrow
client-side on the `dev.sigstore.bundle.content` annotation (`dsse-envelope`)
and, when `--type` is given, on `dev.sigstore.bundle.predicateType`". Six lines
later: "A candidate whose annotation and signed predicateType disagree is
rejected as `PredicateTypeMismatch` — never silently accepted, **and never
silently skipped**."

Those two sentences cannot both hold. A candidate filtered out by annotation is
never fetched, so its signed predicateType is never read and the disagreement is
never detected. "Never silently skipped" is true only of candidates that survive
the filter — i.e. only of candidates the (unsigned, registry-controlled)
annotation chose to admit.

This also diverges from the behaviour the ADR cites as precedent. Cosign's read
path "discovers referrers with an EMPTY artifactType filter and discriminates
client-side by **parsing each bundle and type-switching on the content
oneof**. The annotations are for other tooling's filtering, not cosign's own"
(`research_cosign_v3_attestation_wire.md` §1). The ADR adopts cosign's discovery
call and replaces its discrimination step with an annotation filter, while
presenting the whole as cosign parity.

**Scenario (inputs → outcome).** A hostile or merely mirror-rewriting registry
serves the real CycloneDX attestation referrer with
`dev.sigstore.bundle.predicateType: https://cosign.sigstore.dev/attestation/vuln/v1`
(or drops `dev.sigstore.bundle.content` entirely). `ocx package sbom --type
cyclonedx pkg:1.0` filters it out before any fetch and exits 79
`attestation_not_found`. The operator concludes the artifact carries no SBOM.
A validly signed SBOM from an admitted identity exists and was suppressed by one
unsigned string. The failure is fail-closed for verification but is an
**omission attack** for any policy of the form "no SBOM → treat as unattested",
and there is no signal distinguishing it from genuine absence.

**Refutation attempted.** Is fail-closed sufficient? Not here: the ADR sells
`ocx package sbom` as the SBOM-consumption surface, so "no attestations" is the
answer a consumer acts on. Does the mismatch check catch it? No — it runs only
post-filter. Does the empty artifactType filter help? It widens the *listing*;
the narrowing happens after.

**Fix.** Make annotations an ordering hint only, never an exclusion filter:
fetch and parse candidates (already bounded by the count/size/budget caps),
type-switch on the bundle content oneof, and match predicateType from the
verified payload — cosign's shape. If annotation pre-filtering is kept as an
optimisation, it must fall back to the unnarrowed candidate set when narrowing
yields zero, and the docs must state that annotation-derived absence is not
proof of absence. Delete the "never silently skipped" sentence or make it true.

---

### B5 — The determinism premise is false in this workspace (`preserve_order`), and row 2's only named proof cannot pass

**Severity:** Block · **ADR:** lines 202–206, 466, 1101 · **Rule:** DATA-DET-01/03/04; checklist row 2; quality-core "Unchecked Green"

ADR lines 202–204:

> the Statement is built from typed values and serialized once with
> `serde_json`, whose `Map` is `BTreeMap`-backed — so key order is stable
> (DATA-DET-01/03). The predicate document is parsed to `serde_json::Value` and
> embedded; its keys sort the same way.

`crates/ocx_lib/Cargo.toml:50`:

```toml
serde_json = { workspace = true, features = ["preserve_order"] }
```

with the workspace's own comment at `Cargo.toml:44-49`: *"backing map to
`indexmap` … Blast radius is **WORKSPACE-WIDE** via Cargo feature unification …
**Adding ordering-sensitive code elsewhere must account for this.**"*

`serde_json::Map` here is `IndexMap`-backed. Keys are in **insertion order**,
not sorted. The ADR states the opposite of a documented, deliberate workspace
decision, in the design record for a document that gets PAE-signed and hashed
into a public append-only log — and DATA-DET-03 names `preserve_order` verbatim
as the thing that flips it.

**The concrete consequence is the row-2 seam test.** Line 1101 makes this the
proof for checklist row 2:

> `ocx package attest` → `ocx package sbom --output` | Round-trip: the extracted
> bytes are **byte-identical** to the input predicate (checklist row 2 — this is
> the seam, not a source scan).

The predicate is parsed to `serde_json::Value` and re-embedded in the Statement
(line 591, `predicate: serde_json::Value`). Extracting it back is therefore a
re-serialization: `preserve_order` preserves key *order*, but not the input
file's whitespace, indentation, number formatting or escaping. Every real SBOM
(`syft`, `cyclonedx-cli`, `cdxgen` all emit pretty-printed JSON) fails the
assertion. The test goes green only when the fixture is already in OCX's exact
compact serialization — i.e. it passes for the wrong reason and never exercises
the case it exists to cover.

**Scenario.** Implementer runs the round-trip test against a 2-space-indented
CycloneDX file; it reds. The cheapest green is to weaken the assertion to
"parses to an equal `Value`" — which is satisfied by a full parse-and-reserialize
and therefore proves the exact opposite of row 2 ("never re-parse or
re-serialize the verified payload before downstream use").

**Fix.** Three edits: (1) correct lines 202–206 to state the real serialization
contract (`preserve_order` is on, order is insertion order, determinism holds
per-input-file rather than by canonicalization); (2) decide and record how the
predicate survives the round trip byte-exactly — the honest options are storing
the predicate's byte range within the payload, or defining `--output` as
emitting the verified **Statement** payload verbatim with the predicate
extracted by offset, not by re-serialization; (3) make the row-2 fixture a
**pretty-printed** SBOM and demonstrate it red before green. Note
`serde_json_canonicalizer` (RFC 8785) is already a workspace dependency
(`Cargo.toml:67`) and goes unmentioned — if canonicalization is wanted anywhere,
it is the existing tool.

---

### B6 — Attestation mode's caps have no stated selection mechanism in a deliberately shared pipeline

**Severity:** Block · **ADR:** lines 236–239 (D-d), 506–528, 479–481 · **Rule:** PKG-04…07, PKG-11

The ADR introduces attestation-scoped caps — 32 MiB per envelope, 32
candidates, 64 MiB total (lines 512–528) — and D-d mandates **one** pipeline
whose gate "becomes a mode check". It never says the size, count and budget caps
are selected by `VerifyContentMode`.

Shipped values in the same pipeline (`crates/ocx_lib/src/oci/verify/pipeline.rs`):

| Constant | Line | Value | ADR's attestation counterpart |
|---|---|---|---|
| `MAX_BUNDLE_SIZE_BYTES` | `sign/bundle.rs:165` | 512 KiB | 32 MiB (**64×**) |
| `MAX_SIGNATURE_CANDIDATES` | `pipeline.rs:73` | 8 | 32 (**4×**) |
| `MAX_TOTAL_REFERRER_BYTES` | `pipeline.rs:81` | 4 MiB | 64 MiB (**16×**) |

**Scenario.** An implementer reads "one pipeline" and hoists the new constants
into `verify_one_referrer` (the per-candidate size gate at `pipeline.rs:357` and
the read cap at `pipeline.rs:613`). A plain `ocx package verify` — and
`auto_verify`, which fires on *every install surface* per
`discover_attest_architecture_map.md` §5 — now permits a hostile registry to
force 32 MiB per candidate against a 64 MiB budget where 512 KiB / 4 MiB held
before. Nothing in the ADR forbids it and nothing in the test plan detects it:
the oversize fixtures (lines 1128–1130) assert only that the *attestation* caps
trip.

Note the caps genuinely cannot be chosen from the candidate alone: the mode of a
candidate is unknown until its bundle is parsed, and the only pre-fetch signal is
the unsigned annotation (B4). So the selection must come from the *requested*
mode, not from the candidate.

**Fix.** State in D-d that the per-candidate size cap, the candidate-count cap
and the cross-candidate budget are selected from `VerifyContentMode` before the
first fetch, and that `Signature` mode retains 512 KiB / 8 / 4 MiB unchanged.
Add the red-before-green pair the ADR already uses for the mode gate: a 1 MiB
bundle must be **rejected** in `Signature` mode and **accepted** in
`Attestation` mode, so a hoisted constant reds.

---

### B7 — Attestation selection is undefined: `VerifyResult.attestation` is `Option`, `SbomReport.attestations` is `Vec`

**Severity:** Block · **ADR:** lines 775–787, 858, 284–287 · **Rule:** checklist row 7; PKG-21

Two contracts in the same ADR disagree about arity:

```rust
pub struct VerifyResult {
    /// `Some` only in attestation mode.
    pub attestation: Option<VerifiedAttestation>,   // line 779
}

pub struct SbomReport { pub attestations: Vec<VerifiedAttestation> }   // line 858
```

The shipped pipeline is ANY-of: it returns the **first** candidate that fully
passes (`pipeline.rs:264-268` and the comment at 262 — "returning the first that
fully passes crypto + identity/policy"). D-d changes the content gate and
nothing else, so `sbom_one` has no stated way to obtain a `Vec` from a pipeline
that returns at most one. ANY-of is correct for "is this artifact signed"; it is
**not** obviously correct for "what SBOMs does this artifact have", and the ADR
never takes the decision.

The gap is security-relevant twice over:

1. **Default mode silently under-reports.** `ocx package sbom` promises "List
   verified SBOM attestations" (line 285, plural). Implemented on first-match
   ANY-of it shows one of N, and a consumer auditing "which SBOMs exist" sees a
   registry-chosen subset.
2. **`--output` selection is registry-ordered.** With two verified attestations
   of the same predicate type from two identities the policy admits — the
   ordinary case under an `identity_regexp` policy, or after a signing-identity
   rotation where old and new coexist as two ANY-of entries by design
   (`trust.rs:386-389`) — whichever the registry lists first wins. An attacker
   holding any admitted identity plants an SBOM omitting a vulnerable component,
   and **the registry decides** which document the consumer reads.

**Refutation attempted.** Does `--type` narrowing resolve it? No — both
candidates carry the same predicate type. Does the ANY-of comment cover it? That
comment justifies first-match for *key rotation on one logical signature*, a
different question from "which of several documents is authoritative".

**Fix.** Decide and record: the attestation path collects **all** verified
candidates (bounded by `MAX_ATTESTATION_CANDIDATES`) rather than short-circuiting;
`--output` refuses with a typed error when more than one verified attestation
matches the requested type, naming the referrer digests so the operator can
disambiguate; and `--json` reports every match. Reconcile `VerifyResult` and
`SbomReport` to one arity in the ADR.

---

### B8 — Attestation mode has no stated source for the chain walk, SCT and cert-validity checks

**Severity:** Block · **ADR:** lines 236–245 (D-d), 789–796 · **Rule:** checklist rows 11, 13; SEC-32

D-d rejects delegating to sigstore-rs and specifies the new step as:

```rust
pub(super) fn verify_envelope(
    bundle: &Bundle, target_digest: &Digest,
    expected_predicate_type: Option<&PredicateType>,
    verifying_key: &CosignVerificationKey,        // line 795
) -> Result<VerifiedAttestation, VerifyErrorKind>;
```

It takes an **already-extracted verifying key**. Something upstream must have
walked the certificate chain to the trust root, checked the embedded SCT, and
checked cert validity at signing time. In shipped code that something is one
call — `crates/ocx_lib/src/oci/verify/pipeline.rs:382`:

```rust
verifier
    .verify(subject_bytes, bundle, &PolicyDeferredToOcx, true)
    .await
    .map_err(map_verification_error)?;
```

with the comment at `:400-406` naming exactly what it covers: "the chain is
built against the trust root *at the certificate's own issuance time*, the
embedded SCT is checked against the CT log keys, the signature is verified …,
the Rekor entry body is rebuilt". **The ADR never says whether this call still
runs for a DSSE candidate.** If the DSSE branch routes to `verify_envelope`
instead, the chain walk, the SCT check and cert-validity-at-signing-time have no
stated source in attestation mode — and `verify_envelope`'s signature is
evidence that shape was contemplated.

**Scenario.** Implementer branches at the gate: `MessageSignature` →
`verifier.verify(...)`; `DsseEnvelope` → `verify_envelope(...)` with a key
extracted from the leaf. A Fulcio certificate is presented whose chain does not
build to the pinned root, or whose SCT is absent. The DSSE path never checks
either, the identity in the cert still matches the policy, and the attestation
verifies. That is a full trust-root bypass on the attestation path.

**Compounding: D-d's stated reason for not delegating is refuted by the shipped
code.** Line 239 rejects `Verifier::verify_bundle_content()` because it "is
bound to sigstore-rs's own `Verifier` construction, which would fork the
trust-root and identity path away from `TrustRoot` + `PolicyDeferredToOcx`. Two
trust roots in one binary is the defect this avoids." That `Verifier` is already
constructed from the OCX `TrustRoot` and already receives `PolicyDeferredToOcx`
(`pipeline.rs:382`). There is no second trust root and never would be. Per
`research_rekor_dsse_entry_kinds.md` §5, the same `verify()` entry point runs
the identical 7-step pipeline for both content kinds, and its DSSE branch
verifies PAE **and** compares the in-toto subject digest against the
caller-supplied `input_digest`. So the decision to hand-roll checklist rows 1
and 4 — the two CVE-backed rows — was taken on a premise that does not hold
(quality-core "Don't Own Non-Domain Code", Block-tier for wire-format work).

**Fix.** State explicitly that `verifier.verify(subject_bytes, bundle,
&PolicyDeferredToOcx, true)` runs unchanged for **both** content modes and that
`verify_envelope` is an additional, defence-in-depth layer over it — not a
replacement. Correct line 239's rationale to the true one (defence in depth over
a checklist whose rows sigstore-rs does not all enforce), and add a negative
fixture asserting an attestation with a chain that does not build to the pinned
root is rejected in attestation mode. If instead delegation is chosen for rows
1/4, say so and keep OCX-side enforcement of rows 3, 5, 6, 7, 8 and 18.

---

### B9 — Row 13 (CVE-2024-55655) names an enforcement site that does not implement it, proven by "existing coverage" that does not exist

**Severity:** Block · **ADR:** line 477 · **Rule:** checklist row 13; quality-core Verification Honesty

Row 13 as adopted:

> | 13 | Assert `NotBefore <= integratedTime <= NotAfter` **as OCX's own check,
> not a library default** | shipped (`verify/tlog.rs`), unchanged | Existing
> coverage; re-asserted for the DSSE path |

Both columns are wrong:

- **The named site does not contain the check.** `rg` for
  `not_before|not_after|NotBefore|NotAfter` across
  `crates/ocx_lib/src/oci/verify/` returns nothing — `verify/tlog.rs` handles
  the SET and the inclusion proof only. The check exists solely inside
  sigstore-rs's `verify()` (its "cert-validity-at-signing-time" step,
  `research_rekor_dsse_entry_kinds.md` §5).
- **There is no existing coverage.** `test/tests/test_verify.py` has no case
  matching `integrated|not_before|expired|validity`.

So the row is satisfied by a library default at a file that does not implement
it — which is *literally* the regression CVE-2024-55655 was: sigstore-java 2.0.0
silently dropped this check, and an attacker with a stolen short-lived key could
sign indefinitely. The row's own wording ("not a library default") exists
because of that incident.

**Scenario.** A short-lived Fulcio certificate is exfiltrated after expiry. The
attacker signs an attestation and submits it to a Rekor instance, producing an
`integratedTime` outside the certificate's validity window. If a sigstore-rs
upgrade relaxes or reorders that step, nothing in OCX's own code or test suite
notices, and the attestation verifies indefinitely. The ADR records the control
as covered, so no reviewer looks again — the SEC-32 shape.

**Fix.** Either (a) implement the assertion in OCX over `parts.integrated_time`
and the leaf certificate's validity window, with a negative fixture whose
`integratedTime` sits outside it, and correct the enforcement site; or (b)
record an explicit argued deviation the way D-b does for row 18 — naming
sigstore-rs's step, the version pinned, and the test that would detect its
removal. Silence is the one option the checklist's normative adoption (line 460:
"A row with no enforcement site is a defect, not a note") forecloses.

---

## Warn findings

### W1 — `--predicate FILE` ingestion has no stated open-time hardening, and its contents are published irreversibly

**Severity:** Warn · **ADR:** lines 530–532, 880–882, 902–907 · **Rule:** SEC-11/12; CWE-367; the in-tree precedent

The ADR specifies exactly one control on the predicate file:
`MAX_PREDICATE_FILE_BYTES = 16 MiB` with a `PredicateTooLarge` variant. Nothing
about symlinks, ownership or mode.

The sibling input on the same command family *is* hardened, and the ADR moves
that resolver into a shared module in this very change (line 907,
`command/package_sign_common.rs`). `crates/ocx_cli/src/command/package_sign.rs:243`
opens `--identity-token-file` with `O_NOFOLLOW`, rejects `ELOOP` as
"identity-token-file is a symlink; refuse to follow (CWE-367)", rejects a file
not owned by the effective uid, and rejects `mode & 0o077 != 0` — all on the
*same* open handle to close the stat/read race.

The predicate is the higher-consequence input of the two. A token is read and
sent to one OIDC endpoint. A predicate is read, embedded in a Statement, signed
with the caller's identity, pushed to a registry and hashed into an append-only
transparency log. **Publication is irreversible.**

**Scenario (inputs to outcome).** A CI workflow runs
`ocx package attest --predicate "$SBOM_PATH" --type cyclonedx ...` where
`SBOM_PATH` derives from workflow input, or an earlier job step can place a
symlink at that path. Point it at `~/.docker/config.json` — valid JSON,
contains base64 registry credentials. OCX reads it, embeds it as the predicate,
signs it with the workflow's OIDC identity, publishes it to the registry and
logs its hash to Rekor. The credentials are now in a signed, permanently
logged, publicly fetchable artifact.

**Refutation attempted.** "The operator chose the path" — true for
`--identity-token-file` too, and that path is hardened anyway; the argument was
already rejected in-tree. "It must be JSON, so the blast radius is small" —
`~/.docker/config.json`, cloud credential caches and JSON-shaped tool configs
are all valid JSON.

**Fix.** Take the decision explicitly in the ADR, one row either way. If
hardened: reuse the extracted resolver's open-time discipline (`O_NOFOLLOW`,
read from the same handle) — ownership/mode checks are arguably wrong here since
a predicate is not a secret, so state which of the three apply and why. If not
hardened: say so in `signing.md` with the reason, so a later reviewer does not
read the asymmetry with the token path as an oversight. Also state that the
16 MiB cap is enforced by a bounded read, not by a `metadata().len()` check
followed by an unbounded read (PKG-04/07).

---

### W2 — `sbom --output -` writes attacker-authored bytes verbatim to a terminal

**Severity:** Warn · **ADR:** lines 286, 915–917, 975–981 · **Rule:** SEC-31/34/35, CWE-150

Line 286: `--output PATH` (`-` for stdout) — "Write the verified predicate
document **verbatim** — the bytes from inside the envelope, never a
re-serialization (row 2)."

Verbatim is right for a file. For `-` on a TTY it is a raw write of
attacker-authored bytes to the terminal. The ADR's sanitization paragraph
(lines 975–981) enumerates DTO *fields* — `identifier`,
`certificate_identity`, `certificate_oidc_issuer`, `predicate_type`, summary
strings — and does not reach the `--output` byte stream, so the ADR reads as
covering a surface it does not cover.

"Verified" does not mean "safe to print": the predicate is authored by whoever
holds an identity the policy admits, and under an `identity_regexp` policy that
can be a large set. Row 2 and CWE-150 pull in opposite directions here and the
ADR resolves neither.

**Scenario.** A CycloneDX document carries a component description containing an
OSC 52 sequence (ESC `]52;c;<base64>` BEL — sets the operator's system
clipboard), or U+202E in a component name. `ocx package sbom pkg:1.0 -o -` on a
TTY executes it. Piping to a file or another tool is unaffected.

**Fix.** Refuse `-o -` when `stdout().is_terminal()` and say why ("raw predicate
bytes are not sanitized; redirect to a file or a pipe"), or sanitize on the TTY
branch only and state in the docs that `-o -` to a terminal is not byte-exact.
Either is one branch; the ADR just has to pick. The TTY-detection precedent is
already in the tree (CLI-09 / CLI-07 call sites).

---

### W3 — `builder` matching is specified against a field path that is wrong for SLSA v1, with fail-open/fail-closed unstated

**Severity:** Warn · **ADR:** lines 446–448, 626–628 · **Rule:** checklist row 20; ARCH-05

D-j: "`builder` semantics. An opaque string matched against the SLSA provenance
predicate's `builder.id` during attestation verify."

`builder.id` is the **SLSA v0.2** path (`predicate.builder.id`). SLSA v1.0 moved
it to `predicate.runDetails.builder.id`. The ADR ships both versions in the same
enum (lines 626–628): `SlsaProvenance`/`SlsaProvenance02` resolve to v0.2,
`SlsaProvenance1` to v1. So a single stated path cannot be right for both, and
the ADR does not say what happens when the path is absent.

**Scenario.** Operator pins `builder = "https://github.com/acme/.github/..."`.
A v1 provenance attestation arrives; `predicate.builder.id` is absent. Two
implementations are equally consistent with the ADR text: absent means no match
means refuse (fail-closed — the pin works but rejects every v1 attestation), or
absent means constraint not applicable means pass (fail-open — the builder pin
is silently inert on exactly the version #102 wants). The second is a policy
bypass with no signal.

**Second issue — the pin is diluted by ANY-of.** `builder` sits on
`TrustPolicy` alongside `scope`, and `resolve()` returns *all* policies at the
winning specificity as an ANY-of set (`trust.rs:376-378`). A policy carrying
`builder` is ANDed internally but ORed against its siblings, so an equal-scope
policy **without** `builder` — which array-append across the pooled operator
tiers permits by design — removes the constraint from the set. `trust.rs:386-389`
already names this channel ("Equal-scope array-append across tiers is otherwise a
signer-enrollment channel") and answers it with `system_locked`, which B1
deletes.

**Fix.** (1) Name both field paths and state the version dispatch (predicateType
selects the path); (2) state that an absent or unparseable builder field is a
**refusal**, not a skip, and name the error variant; (3) state in D-j that
`builder` is ANDed within a policy and ORed across the ANY-of set, so an
equal-scope policy without it weakens the set — and that `system_locked` (B1) is
the operator's containment.

---

### W4 — `ocx package attest` has no offline refusal, diverging from `sign`'s exit-77 contract

**Severity:** Warn · **ADR:** lines 722–734, 754–758, 1006–1011

The sign CLI does an offline check *before* token resolution and returns
`SignErrorKind::OfflineSignRefused` — exit 77 `PermissionDenied`, slug
`offline_sign_refused` (`crates/ocx_lib/src/oci/sign/error.rs:134-137, 196-198,
216`), documented there as "policy rejection of the *action*, not a passive
network access".

The ADR's attest step order (lines 754–758) is "SSRF floor on both trust URLs,
resolve the per-platform target, index indirection, referrers capability probe,
token acquisition" — no offline check. `AttestContext` (lines 722–734) has no
`offline` field, while `SbomOptions` (line 853) does. The new `SignErrorKind`
table (lines 1008–1011) adds no offline variant.

**Scenario.** `ocx --offline package attest --predicate sbom.json --type
cyclonedx pkg:1.0`. Instead of the 77 policy refusal the sign path returns, the
run proceeds to a Fulcio call and fails with a network or
`transparency_log_unavailable` class error (69/83). A script branching on 77 to
tell "refused by policy" from "infrastructure down" mis-classifies, and the
operator gets a misleading diagnosis.

**Fix.** Add `offline` to `AttestContext`, mirror the sign path's pre-token
refusal, and add `OfflineAttestRefused` (77, slug `offline_attest_refused`) to
the `SignErrorKind` table plus the acceptance matrix.

---

### W5 — Attestation referrers consume the signature scan's 8-candidate budget

**Severity:** Warn · **ADR:** lines 266–274 (D-e), 236–239 (D-d)

Signatures and attestations share `artifactType`
(`application/vnd.dev.sigstore.bundle.v0.3+json`) — the ADR says so at line 271
and makes it the reason for the empty filter. They therefore land in the same
`list_referrers` result for the same subject digest. The shipped signature scan
truncates **before** any per-candidate inspection:

```rust
for descriptor in candidates.into_iter().take(MAX_SIGNATURE_CANDIDATES) {   // pipeline.rs:269
```

with `MAX_SIGNATURE_CANDIDATES = 8` (`pipeline.rs:73`). The ADR raises the
attestation cap to 32 but says nothing about the signature scan now sharing its
listing with a second artifact class.

**Scenario (no attacker required).** A publisher attaches CycloneDX, SPDX, SLSA
provenance, vuln and OpenVEX attestations to one per-platform manifest, and
re-runs `ocx package attest` a few times — each run mints a fresh certificate
and signature, so each produces a new bundle digest and a new referrer, and they
accumulate. Once nine or more referrers precede the signature in the registry's
listing order, `ocx package verify` never fetches it and returns
`NoSignaturesFound` (79) for a correctly signed artifact. `auto_verify` fires on
every install surface, so this blocks `ocx install` too. The failure is closed,
so this is availability rather than forgery — but it is a regression this ADR
introduces and does not name, and registry listing order is not under the
publisher's control.

**Fix.** Filter the candidate set by requested mode *before* the `take(N)` — with
annotations demoted to an ordering hint per B4, the truncation should apply to
candidates that survive a content-type check, not to the raw listing. State the
interaction in D-e and add a fixture: one signature plus nine attestations on one
subject, `ocx package verify` must still succeed.

---

## Audit 1 — checklist completeness, row by row

"Site real" = the enforcement site named in Part III resolves to a contract in
Part IV, not to prose. "Red reachable" = the proof is a test in the Testing
Strategy that can go red on an input the pinned environment can produce.

| # | Site real | Red reachable | Verdict |
|---|---|---|---|
| 1 PAE over decoded bytes | yes — `dsse.rs::pae` (562) | yes — PAE vector (1110, 1151) | Pass. The named mutation (decoded to base64) is not in the red-before-green list (1137–1147); add it there. |
| 2 no re-serialization | **name drift** — Part III says `VerifiedStatement`, Part IV defines `VerifiedAttestation` (782) | **no** — the byte-identical round trip (1101) cannot pass on a pretty-printed predicate | **Fail — B5** |
| 3 payloadType before parse | yes — variant (997) | yes — fixture (1123) | Pass |
| 4 subject binding *(CVE-2026-31830)* | yes — `binds_subject` (615) | yes — fixture (1119) **and** an explicit demonstrated-red step (1146) | **Pass — the strongest row in the ADR**, subject to B8 |
| 5 zero-subject | yes — same fn | yes — fixture (1120) | Pass |
| 6 sha256 only in DigestSet | yes — same fn | yes — fixture (1121) | Pass |
| 7 predicateType from payload *(CVE-2022-35929)* | yes — variant (994) | partial — the fixture only covers candidates the annotation admits | **Partial — B4** |
| 8 exactly one signature | yes — variant (998) | yes — fixture (1124) | Pass |
| 9 (t,n) out of scope | n/a — Not Doing (1251) | n/a | Pass |
| 10 keyid is a hint | prose ("field is read and discarded") | **no** — the "hostile keyid still verifies" fixture is absent from the negative table | Nit: add it or drop the claim |
| 11 algorithm from trust root | "shipped, unchanged" | inherits B8's uncertainty | Tied to B8 |
| 12 envelope-hash binding *(GHSA-8gw7-4j42-w388)* | site named, **mechanism unachievable** | no — the fixture asserts a hash that cannot be recomputed | **Fail — B2, B3** |
| 13 integratedTime in cert window *(CVE-2024-55655)* | **no** — `verify/tlog.rs` contains no such check | **no** — no matching test exists | **Fail — B9** |
| 14 checkpoint signature *(GHSA-jp26-88mw-89qr)* | yes — `tlog.rs:103-125` passes `proof.checkpoint.envelope` + the pinned key into `InclusionProof::verify` | yes — existing suite; the fn is content-agnostic over `canonicalized_body: &[u8]`, so it carries to `dsse` unchanged | **Pass — verified in source, genuinely clean** |
| 15 envelope byte cap | yes — constant (512) + variant (1001) | yes — fixture (1128) | Pass, modulo B6 |
| 16 decoded payload cap | yes — constant (518) + variant (1002) | yes — fixture (1129) | Pass |
| 17 candidate + cumulative caps | yes — constants (523, 528) | **partial** — `TooManyAttestations` has a fixture (1130); `AttestationBudgetExhausted` has none | Nit: the budget variant has no reachable red |
| 18 `_type` allowlist | yes — deviation argued in D-b, variant (996) | yes — fixture (1125) | **Pass — this is the model for how a deviation should be recorded (cf. B9)** |
| 19 docs state no freshness | docs surface (1072) + Not Doing (1255) | n/a — no test claimed, stated as such (488–491) | **Pass — honest** |
| 20 fail closed | "all of the above"; every negative asserts a specific kind (1115) | depends on 2/7/12/13 | **Partial** — inherits B2, B4, B9 |

Four of the five CVE-backed rows the brief singles out land as: row 4 **pass**
(and exemplary), row 14 **pass**, row 12 **fail**, row 13 **fail**, row 20
**partial**.

---

## Audit 2–7 — questions that came back clean

Recorded so a later reader can tell "checked and clean" from "not checked".

**N1 — `--write DIR` does not exist in this ADR (audit 2b).** The only
filesystem write on the read path is `--output PATH` (915–917), an
operator-supplied path with no registry-derived component. There is no
`<package>-<digest>.cdx.json` filename construction anywhere in the ADR, so
PLAT-01/PLAT-02 containment does not fire and no digest-format-before-filename
gate is owed. If `--write DIR` is added later, it becomes a PLAT-01 surface
(a digest is registry-influenced text used as a filename) and needs the
containment helper plus a `FromStr`-validated `Digest` before interpolation.

**N2 — CycloneDX parse bounds are adequate (audit 2d).** Input to
`summarize_cyclonedx` is signature-verified and already bounded by
`MAX_STATEMENT_PAYLOAD_BYTES` (16 MiB, line 518). Recursion depth is bounded by
serde_json's built-in 128-frame limit, which holds because the workspace does
**not** enable `unbounded_depth` (`Cargo.toml:66`, `crates/ocx_lib/Cargo.toml:50`
— only `preserve_order`). Action: keep `unbounded_depth` off, and state the
version sniff order in Part IV (read `specVersion` first, dispatch, then parse)
so the 1.5–1.7 refusal is a probe-then-dispatch rather than a post-hoc check
(DATA-FMT-02 shape).

**N3 — no decompression is in play on the envelope path (audit 5).** Neither the
workspace reqwest pin (`Cargo.toml:102`, features `json`/`rustls`/`charset`/`http2`)
nor the `oci-client` fork's (`external/rust-oci-client/Cargo.toml:47-51`,
features `json`/`query`/`stream`) enables `gzip`, `brotli` or `deflate`, so
reqwest performs no transparent decompression and `.take(cap)` bounds actual
wire bytes. `async-compression` is scoped to the package-layer pull pipeline, not
to bundle blobs. PKG-05's expansion-ratio cap therefore does not apply here.
Confirmed — no action.

**N4 — exactly-one-backend gets exit 78 for free (audit 3a).** A new
`TrustPolicyError::NoBackend` variant inherits the classification through
`VerifyErrorKind::TrustPolicyInvalid(#[from] crate::trust::TrustPolicyError)`,
which maps to `ExitCode::ConfigError` (78) with slug `trust_policy_invalid`
(`crates/ocx_lib/src/oci/verify/error.rs:255-257, 368-371, 401`), pinned by
`trust_policy_invalid_maps_to_config_error` (`:526`). The ADR does not state the
exit code; it is structurally correct anyway. Worth one sentence in D-h so a
planner does not invent a new one.

**N5 — `builder` with no backend table is answered (audit 3d).** D-j is explicit
and self-consistent: zero backends is `NoBackend` (so a `builder`-only policy is
invalid), while a `builder` on a policy that never verifies provenance is "not an
error — it is forward configuration" (447–448). Two different situations, both
covered. No action beyond W3.

**N6 — project tier cannot outbid an operator match (audit 3c, partial).**
`resolve_tiered` (`trust.rs:459-470`) discards the project tier entirely whenever
any operator policy matches the target, so a project `ocx.toml` cannot introduce
a weaker backend at a longer prefix for a scope the operator governs. The
remaining downgrade channel is *within* the pooled operator tier (system vs user
vs `$OCX_HOME`), which is exactly what `system_locked` exists to close — see B1
and W3.

**N7 — offline verify of attestations needs no new trust plumbing (audit 4).**
`verify_rekor_set` and `verify_inclusion` take `canonicalized_body: &[u8]` and are
content-agnostic; the trust cache is keyed by Rekor slug and stores key material,
not entry-kind-specific state (`discover_attest_architecture_map.md` §2). Rekor
key pinning therefore applies to `dsse` entries unchanged, and the mandatory
inclusion proof carries over (the ADR keeps it at line 711). The offline half of
audit 4 is clean; the *attest*-side offline gap is W4.

**N8 — the freshness/rollback honesty requirement is met (audit 4).** Line 1255
states plainly that attestation proves "validly signed and logged at T" and
nothing about a newer superseding SBOM, marks the docs update mandatory, and the
docs table (1072) forces the `signing.md` Current Limitations edit. No ADR text
implies freshness or rollback protection. SEC-32 satisfied — no action.

**N9 — mixed-candidate scan is skip-and-continue, and that is safe (audit 2e).**
The shipped loop merges each candidate's failure into `best_error` and continues
(`pipeline.rs:270-300`), pinned by the existing
`malformed-referrer-does-not-block-valid` acceptance test; D-d extends the same
discipline to a mode mismatch (260–262). A malformed candidate therefore cannot
abort the scan. The second half of the question — "does a pass on candidate N
hide a tampered candidate N-1" — is **yes by construction**, which is correct
ANY-of semantics for *signatures* and is the open question for *attestations*:
that is B7, not a defect in the skip-and-continue mechanism.

---

## Verdict

**Fail** — the ADR cannot be decomposed into a plan as written. Nine Block
findings, five Warn.

This is a strong design record: the threat model is stated, the checklist is
adopted as normative rather than as advice, the two rows with no behavioural
seam are called out honestly (488–491), row 18's deviation is argued rather than
smuggled, row 19 refuses to claim a control that does not exist, and the
red-before-green step for subject binding (1146) is exactly right. The findings
below are concentrated in three places, not spread thin:

1. **The tlog-binding mechanism (B2, B3)** — row 12 as specified is not
   achievable from a Sigstore bundle, and the second accepted entry kind rests
   on an unsourced claim with no reachable red state. Both need a decision
   before anyone writes `verify_tlog_binding`.
2. **What the shared pipeline actually runs for a DSSE candidate (B6, B8, B9)** —
   which caps apply, whether the chain walk and SCT check still execute, and
   where the cert-validity window is asserted. All three are one-or-two-sentence
   additions to D-d; all three are silent today and each has an unsafe reading
   an implementer can reach honestly.
3. **Two premises that the tree contradicts (B1, B5)** — `system_locked` is
   dropped from the nested `TrustPolicy`, and the determinism rationale asserts
   `BTreeMap`-backed `serde_json::Map` where `crates/ocx_lib/Cargo.toml:50`
   enables `preserve_order` with a comment warning that the blast radius is
   workspace-wide. Neither would be re-derived by a builder reading the ADR.

B4 and B7 are contract gaps rather than premise errors, but both let the
*registry* decide what the operator sees — which SBOM, or whether any SBOM
exists — and neither is visible from the code once written.

**Recommended sequencing.** B1, B5, B9 and B6 are edits to the ADR text and cost
little. B2, B3, B4, B7 and B8 are design decisions the Part V spike is well
placed to settle empirically — but they are **not** in Part V's "MAY adjust"
list, and B2/B4/B8 change the architecture, so under the ADR's own rule (1197)
they are amendments with the owner in the loop, not implementation decisions.
Part V should be extended to name them, or they should be resolved before the
spike starts.
