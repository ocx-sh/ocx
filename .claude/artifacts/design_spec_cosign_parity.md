# Design Spec — cosign parity for signing, attestation and SBOM

**Status:** Approved (design), not started
**Date:** 2026-08-29
**Origin:** [#356](https://github.com/ocx-sh/ocx/issues/356), scoping discussion 2026-08-28/29
**Supersedes:** `handoff_keep_tag_rename.md` (folded in as WP7)
**Related:** [#197](https://github.com/ocx-sh/ocx/issues/197) (closed on stale reasoning — see §Background),
`adr_oci_referrers_signing_v1.md` (S1-F reversed here), `adr_sbom_attestations.md`

## Goal

Bidirectional parity with cosign: anyone with a stock `cosign` can verify artifacts OCX
signed, and OCX can verify artifacts cosign signed — across both the OCI 1.1 referrers
world and the simplesigning sidecar-tag world, on registries with and without the Referrers API.

Parity is a **hard requirement across every axis** — both directions (OCX signs → cosign
verifies, cosign signs → OCX verifies), both formats (simplesigning sidecar and OCI 1.1), and both
key models (keyless and key-pair). Not a best-effort claim: every combination is an
acceptance test, and an untested cell is a failed release.

## Non-goals

- Signing tags. See §Rejected.
- Signing the index root of the OCX index. Separate ADR.
- **Interop with a self-hosted stack running Rekor v2.** cosign has a v2 client since
  v2.6.0; `sigstore-rs` does not. Irrelevant for the public-good instance (see below), but
  a self-hosted Rekor v2 deployment is out of reach until sigstore-rs ships a client
  ([#107](https://github.com/ocx-sh/ocx/issues/107)).

**Rekor v2 is not a blocker for this work.** Sigstore has stated the public-good instance
stays on **Rekor v1 for the foreseeable future** — v2's client-breaking changes are being
held back so they do not stack with the PQC transition. The keyless half of the WP6 matrix
therefore runs against a stable target, and the Rekor-v2 anxiety recorded in
`deferrals_107_197.md` does not gate this release. (Bundle **v0.3** likewise remains the
current media type; protobuf-specs *package* versions 0.4.x/0.5.0 are not bundle-format
versions.)

## Background — the blocking finding

OCX signs a **messageSignature** over the raw manifest digest (`oci/sign/signer.rs:109`,
annotation `dev.sigstore.bundle.content: message-signature`). Cosign v3's image signature
is a **DSSE in-toto Statement**, predicateType `https://sigstore.dev/cosign/sign/v1`,
empty predicate, subject = image digest, written by `WriteAttestationNewBundleFormat`.
`cosign verify` on a new-format bundle requires a DSSE envelope. Same artifactType, same
annotations, **different payload** — so cosign cannot verify an OCX signature today, in
any mode.

Two facts make this cheap to fix:

1. **Signing is unreleased.** Latest tag `v0.5.8`, no sign/verify entries in
   `CHANGELOG.md`. The wire format carries no compat debt — change it outright.
2. **The DSSE machinery already exists** for attestations: `sign/signer.rs` PAE signing,
   `attest/statement.rs`, `attest/dsse.rs`, and a Rekor client that already posts the
   `dsse` proposed-entry kind.

`deferrals_107_197.md` claims interop is "infeasible regardless" because OCX signs against
a fake stack with a custom `ocx-rekor-set-v1` SET. **That is stale** — `verify/tlog.rs:33`
records the custom SET being replaced, and `sign/fulcio.rs` speaks the real
`/api/v2/signingCert`. [#197](https://github.com/ocx-sh/ocx/issues/197) was closed on that
stale reasoning and should be reopened or superseded.

**Corrected 2026-08-29 — a bundle-level interop spike already exists and passes.**
`test/tests/test_cosign_interop.py` (5 tests, driving cosign v3.1.1 from
`test/tests/fixtures/cosign.py`) proves bidirectional agreement on the *bundle*: cert chain,
signature and Rekor entry, both directions, against the local Fulcio/Rekor. So "cosign
unreachable in the sandbox" is already false, and the crypto already interoperates.

What those tests do **not** prove, and what this spec is actually about, is *image-level*
parity. Every existing cell goes through cosign's **blob** commands — `verify-blob`,
`sign-blob`, `verify-blob-attestation`, `attest-blob` — which hand the bundle over as a file
and accept a `messageSignature` payload. `cosign verify <ref>`, which resolves a registry
reference and demands a DSSE envelope, is never invoked. The suite's own module docstring
says so and states the reason: *"Discovery is deliberately out of scope … ocx has no
`sha256-<hex>.sig` tag-schema fallback"*. WP1/WP2/WP5 make that sentence false and WP3/WP4
make the DSSE demand satisfiable; WP6 therefore **extends** that file rather than creating
one, and rewriting its docstring is a required WP6 edit, not a nicety.

## Decisions

| # | Decision | Rationale |
|---|---|---|
| D1 | **Coverage:** both the index and every platform manifest end up signed — by two different commands, see §Where signing happens. Verify accepts either, with a membership check when the signature is on the index | `cosign verify <tag>` resolves to the index digest and looks for a signature there, so the index must be signed. OCX's install path pins a platform manifest, so verify must accept an index signature covering it. Both directions force both halves. This is a statement about coverage, **not** about one command fanning out. |
| D2 | Delete `messageSignature` — write **and** read | Nothing published uses it; keeping it means a third shape in the discovery merge for zero consumers |
| D3 | Keep `ExitCode::ReferrersUnsupported` (84), **narrow** it to the *write* path: "Referrers API absent **and** the fallback tag write was refused". On the **read** path, no Referrers API and no fallback tag means *no signatures found* (79), never 84 | Exit codes are contract; deleting one breaks scripts that branch on it. Once WP1 can read a fallback index, an absent Referrers API is no longer by itself a read failure |
| D4 | Fallback-index writes use **read-back + bounded retry**, dedupe by descriptor digest, loud failure on exhaustion | The OCI spec has no conditional manifest PUT, so no CAS exists. Optimistic retry converges: the loser re-reads, sees the winner's descriptor, appends its own. Both land. |
| D5 | simplesigning sidecars pass through the **identical** identity + trust gate as v0.3 | A `.sig` carries its verification material in *annotations* rather than a bundle blob; that is a parsing difference, never a trust difference. Under **keyless** those are `certificate` / `chain` / `bundle`; under a **key** there is no cert, no chain, and — with `--no-rekor-upload` — no `bundle` either. Their absence is a legal shape, not malformed input. **What "identical gate" means under a key:** the gate is satisfied by public-key match against `--key` or a `kind = "key"` signers entry; the keyless certificate matchers (`--certificate-identity`, `--certificate-oidc-issuer`) are an **error** alongside `--key`, and a policy whose applicable signers are all `kind = "keyless"` **refuses** a key-signed artifact |
| D6 | Discovery merges all shapes into the existing `signatures[]` contract (`signature_format` + `discovery_method` per element), deduped on Rekor log index | One subject can carry four shapes, and the same signature can surface twice (referrers API *and* fallback tag) |
| D7 | cosign **3.x** floor for the interop suite; simplesigning-format *read* fixtures are **committed bytes** | No second toolchain in the matrix; the simplesigning format is frozen so fixtures cannot go stale |
| D8 | OCX **writes** both formats, selected by flag. Default `bundle` (OCI 1.1 + v0.3); `simplesigning` forces the cosign sidecar; `both` emits each | Owner requirement: parity must be enforceable, not merely preferred. Mirrors cosign's own `--registry-referrers-mode` / `--new-bundle-format` knobs |
| D9 | Verify **prefers** `bundle` and falls back to `simplesigning` when absent, unless pinned by flag | Same default posture as the write side; a pin exists so a policy can refuse the older shape |
| D10 | Support **key-pair signing alongside keyless**, on both sign and verify, in both formats | Owner requirement. Keyless stays the default and the differentiator; key mode is added, never substituted. Air-gapped and policy-bound orgs cannot reach Fulcio, and cosign users who signed with `--key` must verify under OCX |

### Where signing happens

- **push** signs each platform manifest inline, **behind an opt-in `--sign`**. Those digests
  never change — sign once. The flag is not optional design: `push` has no sign path today, and
  signing unconditionally would spend a Fulcio cert and a Rekor entry on every `ocx package push`
  and fail every push made without an OIDC identity.
- **`ocx package sign <identifier>`** signs **whatever the reference resolves to**. An index
  → the index. A bare manifest → that manifest. It does *not* fan out to an index's
  children: push already signed them, and re-signing would spend a Fulcio cert and a Rekor
  entry per platform for nothing. Because resolution happens at call time, intermediate
  indices are never signed.
- **`ocx package attest`** signs the SBOM as a cosign-shaped DSSE attestation.

**`--platform` is a narrowing modifier, not a required selector.** Given, it resolves *into*
an index and acts on that child instead of the index itself; it is an error when the
resolved object is not an index. Absent, the command acts on the resolved object as-is. The
same rule applies whether the reference carries a tag or a digest — a tag does not imply an
index (OCX supports bare-manifest tags), so the branch is on what resolution returns, never
on the reference's form.

**Division of labour — write it in the docs, not just here.** The two halves are signed by
two different commands, at two different times, for a structural reason:

| What | Signed by | When | Why there |
|---|---|---|---|
| Platform manifests | `push` | inline, per platform | Their digests are final the moment they are pushed |
| Index | `sign` | after the last platform lands | The index digest changes on every merge, so it is only final at the end |

`--tags-file` / `--tags` exist **solely to sweep up the indices** once the pushes are done:
push records each tag it wrote, and a later `ocx package sign --tags-file …` signs the index
each of those tags now resolves to. The manifests underneath are already signed and are not
revisited.

That makes `--platform` and `--tags-file` **mutually exclusive**, and not as an arbitrary
restriction: a sweep is by definition about indices, while `--platform` narrows into one
index to reach a child that `push` already signed. Combining them asks for work that is
either redundant or, on a tag resolving to a bare manifest, an error. Naming a single
reference is the way to sign one specific child. `--platform` is exclusive with **both**
`--tags` and `--tags-file`, for the same reason.

**Sweep semantics.** A swept tag that resolves to a bare manifest is **skipped with a warning**,
not an error — `push` already signed it, and a mixed tag list is the normal case for a repo that
publishes both single-platform and multi-platform packages. The sweep **continues past a per-tag
failure** and exits non-zero at the end with every failure listed; aborting at the first failure
on a twenty-tag sweep would leave the operator with no idea which of the remaining nineteen
succeeded.

Rationale for the split: `push_manifest_and_merge_tags` (`oci/client.rs:1282`) rebuilds
the index on every platform push, so an N-platform package walks through N index digests.
Signing per push would leave N−1 dead signatures. Platform manifest digests are stable.

Stale intermediate signatures are self-cleaning: the old index goes untagged and registry
GC reaps it together with its referrer. **Exception:** under the fallback-tag scheme the
referrers index at `sha256-<old-index-digest>` is a real tag and pins the dead referrer.
Litter, not a correctness bug — worth a cleanup pass when the subject is gone.

Signing the index closes the child-swap hole (the index digest binds every platform
descriptor). It does **not** close rollback — an older index carries a valid signature
too. Yanks in the OCX index are the answer there.

## Work packages

### WP1 — Read the referrers fallback tag

`oci/client/native_transport.rs:687` (inside `list_referrers`, declared at `:673`) calls
`pull_referrers_native` deliberately, to fail
closed. The fork already implements the fallback
(`external/rust-oci-client/src/client.rs:2266`, `pull_referrers_via_tag_schema`, reached from
`pull_referrers` at `:2175` on a 404, tested by `test_pull_referrers_with_tag_schema_fallback`).
Switch to the fallback-capable `pull_referrers` and re-derive the 84 semantics per D3.

### WP2 — Write the referrers fallback tag

New transport method. GET the index at `sha256-<subject-hex>` (404 → empty), append the
descriptor **with `artifactType` and annotations preserved**, PUT it back, then read back
and retry per D4.

Reverses ADR `adr_oci_referrers_signing_v1.md` S1-F, which bans fallback-tag writes and
enforces the ban with a test tape asserting no `sha256-<hex>.sig|.att` manifest write. The
ADR needs an amendment and that assertion needs inverting.

> Note: cosign's own fallback write is broken upstream
> ([sigstore/cosign#4641](https://github.com/sigstore/cosign/issues/4641) — `artifactType`
> and annotations lost, stalled on go-containerregistry). Getting this right is a
> differentiator, not merely parity.

### WP3 — Sign emits cosign-shaped DSSE

Replace the messageSignature payload with a DSSE in-toto Statement: predicateType
`https://sigstore.dev/cosign/sign/v1`, empty predicate, subject = the signed digest.
Referrer annotations become `dev.sigstore.bundle.content: dsse-envelope` +
`dev.sigstore.bundle.predicateType`. Mostly rewiring `sign/pipeline.rs` onto the existing
attest machinery.

### WP4 — Verify accepts cosign DSSE signature bundles

Mirror of WP3 on the read side, plus the index-membership check from D1.

**The membership check, stated precisely.** Membership is proved by fetching the index the
reference resolved to and matching the platform manifest's digest against its `manifests[]`
descriptors — the index digest binds every descriptor, so a signature over the index digest
covers each child it lists. Two cases where the check cannot run, and both fail *closed* rather
than silently widening: when verify was handed a bare platform digest with no enclosing index to
resolve, and when the index is not fetchable (`OCX_OFFLINE`, or absent from the cache). In both,
the index signature is not considered and only signatures on the manifest itself count.

### WP5 — simplesigning sidecars, read

- `sha256-<hex>.sig` tag: image manifest whose **layers** are simplesigning payloads, one
  per signature, with `dev.cosignproject.cosign/signature`,
  `dev.sigstore.cosign/certificate`, `/chain`, `/bundle` annotations. Verify the signature
  over the payload bytes and check `critical.image.docker-manifest-digest` against the
  subject.
- `application/vnd.dev.cosign.artifact.sig.v1+json` OCI-1.1 referrer: same payload logic,
  different discovery.
- `.att` and `.sbom` equivalents.

All of it passes through the D5 trust gate.

### WP5b — simplesigning sidecars, write

Per D8. Writing a simplesigning `.sig` is **not** a re-packaging of the v0.3 bundle — it signs a
different payload:

- Build the simplesigning claim
  (`{"critical":{"identity":{"docker-reference":…},"image":{"docker-manifest-digest":…},"type":"cosign container image signature"},"optional":…}`)
  and sign **those bytes**, not the manifest digest.
- Rekor entry is a `hashedrekord` over the payload, so `--signature-format both` costs two
  Fulcio certs and two Rekor entries per subject.
- Push an image manifest at `sha256-<hex>.sig` whose layers are simplesigning payloads
  (`application/vnd.dev.cosign.simplesigning.v1+json`), one layer per signature, carrying
  `dev.cosignproject.cosign/signature`, `dev.sigstore.cosign/certificate`, `/chain`,
  `/bundle` annotations.
- Re-signing **appends a layer** to the existing manifest rather than replacing it — same
  read-modify-write hazard as D4, so it reuses the same retry loop.
- `.att` and `.sbom` equivalents for `attest`.

### WP6 — Interop acceptance tests

**Test stack — already exists, nothing new to stand up.** `test/docker-compose.yml` runs a
complete self-hosted Sigstore deployment plus both registry shapes:

| Service | Image | Role in the matrix |
|---|---|---|
| `dex` | `dexidp/dex:v2.45.1` | OIDC issuer for the keyless axis |
| `fulcio` | `ghcr.io/sigstore/fulcio:v1.8.8` | Short-lived signing certs |
| `sigstore-ct` | `tesseract/posix` | CT log Fulcio writes to |
| `rekor` + `trillian-log-{server,signer}` + `sigstore-mysql` | `rekor-server:v1.4.2` | Transparency log — **Rekor v1**, matching the public-good target |
| `registry` / `target-registry` / `prod-registry` | `zot:v2.1.18` | Referrers-API-present half |
| `mirror-registry` | `registry:2` | Referrers-API-absent half (fallback tag) |

**Keyless axis** — runs entirely against that stack. dex issues the identity token, fulcio
the cert, rekor v1 the log entry. The stack's own trusted root feeds both tools:
`--sigstore-trusted-root` / `[trust.sigstore] trusted_root` for OCX, and cosign v3's
`--trusted-root` + `--signing-config` for the other direction. No public-good dependency,
no network, no ambient CI identity required.

> This is precisely the spike [#197](https://github.com/ocx-sh/ocx/issues/197) was closed
> without running. Its stated reason — "the *bundle contents* are fake-stack-shaped" — no
> longer holds: this is a real Fulcio and a real Rekor v1 emitting standard wire format, so
> pointing cosign at the same root is expected to work, not hoped to.

**Key axis** — needs no services at all. Generate one key pair once with
`cosign generate-key-pair`, commit the public key as a fixture and the encrypted private
key with a known `OCX_KEY_PASSWORD`; signing is then deterministic and offline. Default
cells assert **no** Rekor entry (per §Rekor-upload default); one cell opts in with
`--rekor-upload` and lands in the local rekor.

**Public-good Sigstore is deliberately not used by any test.** It needs an ambient OIDC
identity that only exists in GitHub Actions, it writes permanent world-readable log entries
from CI, and it rate-limits. The stack above is a superset of what the matrix needs.

**Driving cosign — decided 2026-08-29, two drivers with one version source.**

The acceptance job in `.github/workflows/verify-basic.yml` (`acceptance-tests`) does **not**
run the `setup-ocx` action and does not materialise the `ocx.toml` toolchain: it downloads a
built `test/bin/ocx`, installs `uv` and `task`, and runs `task test:parallel`. An `ocx.toml`
entry alone therefore puts nothing on `PATH` there. Making it do so would mean the system
under test installing its own test tool — a broken `ocx install` would then fail every
interop cell for a reason that has nothing to do with signing.

So:

- **The matrix keeps the existing container driver.** `test/tests/fixtures/cosign.py` already
  runs `ghcr.io/sigstore/cosign/cosign` under `docker run --network host`, version-pinned in
  one constant. Docker is already a hard dependency of both the registry and the `sigstore`
  compose profiles, so no interop cell — key axis included — gains a dependency it did not
  already have. A real cosign in the loop is the requirement, and the container is one.
- **`ocx.toml` gains `cosign = "ocx.sh/sigstore/cosign:3"` anyway**, for local development and
  for WP8's casts, which must show a bare `cosign verify …` with no OCX prefix. Casts are
  recorded locally, where direnv is active — the one place the toolchain entry actually
  resolves.
- **One version source, and it must be a concrete one.** `fixtures/cosign.py` derives its image
  tag from the version `ocx.lock` resolves the `ocx.toml` pin to — **not** from the `ocx.toml`
  pin itself. `cosign = "ocx.sh/sigstore/cosign:3"` is a floating tag, and the fixture's existing
  comment is right that a floating tag makes a green run unattributable to a version. The lock is
  concrete, so the container tag stays concrete while the two cannot drift.

This still revisits the ADR's "never invoke `cosign sign` in CI" ruling — the point of that
reversal was that a green with no cosign in it is an unchecked green, and that holds whichever
way cosign is delivered.

**Required matrix — the full cross-product, every cell a test, none asserted by inspection.**

Four axes: producer/consumer × format × key model × registry.

| Axis | Values |
|---|---|
| Direction | ocx signs → cosign verifies · cosign signs → ocx verifies |
| Format | bundle (OCI 1.1 + v0.3) · simplesigning sidecar |
| Key model | keyless (Fulcio + Rekor) · key-pair |
| Registry | Referrers API present · absent (fallback tag) |

2 × 2 × 2 × 2 = **16 cells**, each also exercised for `sign`, `attest` and SBOM attach
where the shape differs. Plus:

- `--signature-format both` producing an artifact each consumer accepts through its own
  preferred path.
- The D9 fallback firing when only the simplesigning shape is present.
- A key-signed artifact with `--no-rekor-upload` verifying without a Rekor entry, and the
  same absence being **refused** for a keyless signature.

Two cosign-side facts the matrix has to plan around rather than discover:

- **`--new-bundle-format` defaults to on since cosign v3.0.0**, so a plain `cosign sign` writes a
  bundle referrer, never a `sha256-<hex>.sig`. The four "cosign signs → simplesigning" cells
  drive cosign with `--new-bundle-format=false`. The suite asserts that flag still exists in the
  pinned version — the day it is removed, those cells become a documented gap rather than a
  silent pass.
- **Known gap, foreseeable now:** [sigstore/cosign#4641](https://github.com/sigstore/cosign/issues/4641)
  loses `artifactType` and annotations on cosign's own fallback-tag write, so the two
  "cosign signs × bundle × Referrers-API-absent" cells (one per key model) cannot produce a
  faithful artifact. Assert on what cosign actually emits, and record the annotation loss as a
  WP8 gap. Do not weaken OCX's reader to accommodate it — OCX's own write getting this right is
  the differentiator (§WP2).

A cell that cannot be produced (e.g. an upstream cosign limitation) is recorded as a
documented gap in WP8, never quietly dropped from the suite.

### WP9 — Key-pair signing and verification

Per D10. Keyless stays the default; key mode is additive.

**Sign.** Accept a cosign key pair — cosign's encrypted PEM (`ENCRYPTED SIGSTORE PRIVATE
KEY`, scrypt-wrapped ECDSA P-256), password from `OCX_KEY_PASSWORD` (empty password
allowed, as cosign permits). Skip Fulcio entirely; the bundle's verification material
becomes a `publicKey` with a key hint instead of a certificate chain. Rekor upload is
opt-in in this mode — see §Rekor-upload default.

**Verify.** A `--key` / public-key path, and a trust-policy backend for key pinning. Note
the asymmetry: **verify needs only the public key**, so it is plain SPKI PEM parsing with
no decryption at all. Only `sign` touches the encrypted private-key format.

**What `sigstore` 0.14.0 actually gives us (verified 2026-08-29 — 0.14.0 is the newest
release; there is nothing to upgrade to).** Two of three assumptions hold and one does not:

- **Crypto layer: better than assumed.** `sigstore::crypto::signing_key::SigStoreKeyPair::from_encrypted_pem(pem, password)`
  decrypts cosign's `ENCRYPTED SIGSTORE PRIVATE KEY` directly — the repo does **not** own scrypt
  or the PEM envelope. `CosignVerificationKey::try_from_pem` covers the SPKI public-key side and
  auto-detects the algorithm. This lowers WP9's sign-side risk materially.
- **Bundle layer: a real gap.** `sigstore::bundle::sign`'s `to_bundle()` hardcodes
  `Content::X509CertificateChain(..)`, and `bundle::verify`'s verifier extracts the key
  exclusively from `tbs_certificate.subject_public_key_info`. **Neither side has a
  `Content::PublicKey` arm.** The protobuf type models it
  (`sigstore_protobuf_specs::…::VerificationMaterial`), the high-level API does not.
  So the key-mode path must hand-assemble `VerificationMaterial::Content::PublicKey` on write
  and hand-match it on read, verifying the signature itself against `CosignVerificationKey`,
  **bypassing `sigstore::bundle::{sign,verify}` for that one code path**. WP9 does *not* ride
  the same rails as WP3/WP4 and must not be planned as if it does.
- A key-mode bundle fixture with exactly that shape is already in the tree —
  `test/tests/fixtures/spike_cosign_bundle.json` carries `verificationMaterial.publicKey.hint`
  — so the read side has committed cosign bytes to build against from day one.

**Key generation is not implemented — documented instead.** `cosign generate-key-pair`
defines the format, and cosign ships in the OCX index, so the documented answer is
`cosign generate-key-pair`, run straight from an activated environment. Owning
generation would mean owning scrypt KDF parameter choice, cosign-compatible PEM encryption,
password prompting and TTY handling, and key-file permissions: a security-sensitive surface
for a one-time bootstrap act, against `quality-core.md` §"Don't Own Non-Domain Code".
Revisit only if users ask. WP8 documents the command and the resulting `--key` /
`signers` wiring end to end.

**Trust policy.** `PolicyBackend` (`trust.rs:663`) has one variant, `Keyless`, and is
deliberately not `#[non_exhaustive]` — add `PolicyBackend::Key`. `TrustPolicy`
(`trust.rs:519`) carries a placeholder comment anticipating a singular
`pub key: Option<KeyMatcher>` sub-table; **do not build that shape.** It predates the
`signers` array below, which replaces both singular sub-tables. Keep the comment's intent
(a second backend variant) and drop its field layout.

**A policy accepts a *set* of signers.** The singular `[trust.policy.keyless]` sub-table is
replaced by a tagged array — one policy, one scope, N accepted signers of either kind:

```toml
[[trust.policy]]
scope = "ghcr.io/acme/*"
builder = "https://github.com/acme/.github/..."
signers = [
  { kind = "keyless", identity = "release@acme.example", oidc_issuer = "https://accounts.google.com" },
  { kind = "keyless", identity_regexp = "^ci-.*@acme\\.example$", oidc_issuer = "https://token.actions.githubusercontent.com" },
  { kind = "key",     key = "file:etc/acme-release.pub" },
]
```

Rationale (revised 2026-08-29, superseding "use N policies for a key ring"): the two OR
levels are **not** the same idea twice. The policy level answers *which policies apply* —
scope matching, tier precedence, `system_locked` — and its OR is a consequence of several
scopes matching one target. The `signers` array answers *which signers a given applicable
policy trusts*, which is an intentional set. Collapsing them forces `scope` to be repeated
once per trusted signer, and a duplicated scope string is exactly where a security config
goes wrong silently. Precedent: sigstore policy-controller's `ClusterImagePolicy` uses an
`authorities:` array whose entries are each `keyless:` or `key:`.

Shape:

- Serde-tagged enum on `kind` (`#[serde(tag = "kind")]`), deserializing straight into
  `PolicyBackend`. `CompiledPolicy` holds `Vec<PolicyBackend>` instead of one.
- `keyless` entries keep today's fields and the "exactly one of `identity` /
  `identity_regexp`, `oidc_issuer` required" rule, per entry.
- `key` entries hold a single key **reference** in the WP9 scheme grammar. No `key_regexp`
  — a public key is a fixed value, not a pattern.
- `scope` and `builder` stay policy-level siblings: both are backend-independent, as
  `TrustPolicy::builder`'s doc comment already argues.
- **An empty `signers` array is a configuration error**, never a catch-all. Fail closed.
- Pre-1.0 and unreleased, so this **replaces** the singular sub-table outright — one
  canonical spelling, no dual-form parsing.

Consequences to honour:

- **Mixed kinds in one policy are legal and useful** — a keyless entry and a key entry side
  by side is how a fleet migrates between models without touching scope.
- **Adding a signer always *widens* acceptance, never narrows it.** Inherent to ANY-of, and
  it must be said loudly in the docs: most readers hear "add a key policy" as tightening.
- Tier precedence is unchanged: operator-scope policies displace project-scope ones
  wholesale (`resolve_tiered`, `trust.rs:810`), and `system_locked` pins specificity so a
  lower tier cannot outbid with a narrower scope.

**Command naming.** Mirror cosign exactly: `--key` on *both* `sign` (private) and `verify`
(public), as cosign does, alongside the existing `--certificate-identity` /
`--certificate-oidc-issuer` keyless flags which already mirror it. In config the same word
appears as the `kind = "key"` discriminant and the `key` / `key_pem` fields of a `signers`
entry — there is no `[trust.policy.key]` sub-table.

**simplesigning sidecar under a key**: annotations carry
`dev.cosignproject.cosign/signature` only — no `certificate`, no `chain`, and no
`dev.sigstore.cosign/bundle` when Rekor was skipped. The reader must not treat their
absence as malformed (D5).

**KMS — decided: file-based keys only, but the *contract* is prepared.**

Cosign supports `awskms://`, `gcpkms://`, `azurekms://`, `hashivault://`, `k8s://`. None is
implemented here. What must be right now is only what a later backend cannot change without
breaking users — the interfaces, not the internals:

1. **`--key` takes a key *reference*, not a path.** Parse it as `[scheme://]<rest>`; a bare
   value or `file:` resolves to a file. Any other scheme is recognised as a *known but
   unimplemented backend* and fails with a distinct, actionable error naming the scheme —
   never "no such file or directory". Same grammar on `sign`, `attest` and `verify`.
2. **The config mirrors the flag grammar.** A `signers` entry's `key` field stores a
   reference in the same spelling, so a KMS entry later needs no config-format change.
3. **A dedicated error kind + exit code** for "unsupported key backend", classified like any
   other, so scripts can branch on it before the backends exist.
4. **`--format json` names the backend** that produced or verified a signature, so a
   consumer can already distinguish `file` from a future `awskms`.

The scheme grammar, the config spelling, the error taxonomy and the JSON field are
contracts — a later backend cannot change them without breaking users, so they are fixed
now even though no KMS exists.

**Build the `KeyBackend` trait now** (revised 2026-08-29, justification corrected). ARCH-07
earns a trait on "a second real implementation **or an exercised test double**". The earlier
wording pointed at WP6's key axis for the double — **that was wrong**: WP6 commits a real
`cosign generate-key-pair` key and drives the file backend, so no double appears there. The
double that does exist, and must, is the in-memory signer `oci/sign`'s unit tests use so they
never touch a key file or the filesystem; that is the exercised second implementation.
Independently, the KMS contract below is a second *interface* consumer. Precedent: `Signer`
(`sign/signer.rs:33`) and `TokenProvider` are both crypto-source traits introduced for the same
reason.

**Shape it for KMS, not for files**, or it gets rewritten when the first KMS lands — which
is worse than having no trait:

- **`async` and fallible with a transport-class error.** A KMS signs over the network.
- **Never exposes private key material.** `sign_prehash(&self, digest) -> Signature`, not
  `private_key() -> Key`. A KMS cannot satisfy the latter, and a file backend fits the
  former trivially.
- **Supplies its own public key / key hint** for the bundle's verification material — the
  caller must not reconstruct it from a private key it is not allowed to see.
  **The hint is wire-visible, so its derivation is fixed here:** base64 of the SHA-256 over the
  DER `SubjectPublicKeyInfo` of the public key, matching cosign. Assert the value against
  `spike_cosign_bundle.json`'s `verificationMaterial.publicKey.hint` in a unit test — an
  unstated derivation is a wire-format hole, not an implementation detail.

Layering: `KeyBackend` is the narrow signing primitive; the key-mode `Signer` impl delegates
to it. `Signer` stays the pipeline-level abstraction that returns a whole bundle. Two
responsibilities, two traits (ISP) — `Signer`'s existing signature is keyless-shaped
(`token`, `fulcio_url`), so folding key mode into it directly would widen it for every
caller.

Only the file backend is implemented. Adding `awskms://` later is a new impl plus a scheme
arm, touching no contract.

### WP8 — Parity documentation and recorded casts

The parity claim has to be visible, not just tested.

- A parity page under `website/src/docs/in-depth/` (extending `signing.md`) stating exactly
  which cosign commands verify which OCX artifacts, and vice versa, per format.
- **Asciinema casts that actually execute cosign** as a bare `cosign …` command against a
  real registry — no `ocx run --`, no `ocx package exec`. The env is already activated
  (shell activation locally, the `setup-ocx` action in CI), and a cast showing
  `cosign verify ocx.sh/…` with no OCX prefix is both the honest invocation and the
  stronger demonstration. Recorded through
  the existing pipeline (`website/recordings.taskfile.yml`, casts under
  `website/src/public/casts/in-depth/`). A cast showing `cosign verify` succeeding on an
  OCX-signed package is the artifact that makes the claim credible to a corporate reader.
- Remove the corrected overclaim noted in `deferrals_107_197.md` — `package sign` may claim
  cosign interop again once WP6 is green, and not before.

Cast recordings must be regenerated in the same commit as any change to the commands they
show; see `project_doc_cast_two_tree_drift.md` for the drift failure this repo already hit.

### WP7 — Rename the canonical tag to `__ocx.keep.sha256-<hex>`

Folded in from `handoff_keep_tag_rename.md`. Minor, but it touches
`crates/ocx_lib/src/package/tag.rs`, which WP1/WP2 also touch — so it is planned here
rather than run as a concurrent worktree.

**Why:**

1. `canonical` is already taken in this codebase for a different concept — the canonical
   *registry* (non-mirror). `canonical_reference()` and "push stays canonical
   (mirror-free)" appear 63 times across `oci/client.rs` and `oci/identifier.rs` alone, so
   `push_canonical_tag` reads as "push the tag to the canonical registry", which is not
   what it does.
2. It is a second private reserved namespace alongside `__ocx*`.
3. `sha256.<hex>` is one character from `sha256-<hex>`, which is spec-reserved.
4. It breaks at sha512 — see below.

**Target:** `__ocx.keep.sha256-<hex>` (82 chars, legal OCI tag). `keep` names the role: it
holds the manifest reachable so registry GC and a stray delete of a rolling/cascade tag
cannot orphan a digest a lock pins (`oci/client.rs:702`, `adr_index_indirection.md`
Decision E).

**Rejected spellings — do not re-open:**

| Candidate | Rejected because |
|---|---|
| `sha256-<hex>` (bare) | **Is** the OCI dist-spec v1.1 referrers fallback tag: *"the Truncated Algorithm, a `-` character, and the Truncated Encoded section"*, encoded truncated to 64 — for sha256 that is the full hex, byte-identical. A spec-conforming client that 404s on the referrers API would read OCX's platform manifest as a referrers index. The `.sig`/`.att`/`.sbom` suffixes are cosign's separate convention; a suffix is not what makes the bare form reserved. |
| `__ocx.digest.sha256-<hex>` | `digest` re-describes the value; the kind slot should name the role |
| `__ocx.alias.sha256-<hex>` | Misdescribes it — the tag names the manifest by its own identity and holds it against GC; it is not a second name |
| `__ocx.pin.sha256-<hex>` | Overloads `pin`, which already means lockfile pinning |
| `__ocx.keep.sha256_<hex>` | WP2 writes the bare referrers index at `sha256-<hex>`; an underscore here would mean two spellings for one compound |
| `__ocx.sha256-<hex>` (no infix) | Burns `sha256`/`sha384`/`sha512` as kind-words and makes `from_tag`'s kind slot hold either a noun or an algorithm |

No collision: `Tag::from` checks the `__ocx` namespace at step 2, before any digest-alias
arm (`package/tag.rs:60`), so the prefixed form classifies as `Internal` and never reaches
`is_referrer_fallback_tag`. Spec-conforming clients compute the referrers tag from a digest
they hold and never pattern-match arbitrary tags.

**Long digests.** `Algorithm::ALL` carries Sha384 and Sha512 (`oci/digest.rs:26`). OCI tags
cap at 128 chars:

| Algorithm | `__ocx.keep.<alg>-<hex>` | Legal? |
|---|---|---|
| sha256 | 82 | yes |
| sha384 | 114 | yes |
| sha512 | 146 | **no** |

**Decision: refuse to write the tag when the full form exceeds 128 chars** — return
`Ok(None)`. `push_canonical_tag` already has that no-op path and is documented as "a safety
net layered on top of an already-committed push, never load-bearing" (`oci/client.rs:691`).
Truncating to 64 like the referrers schema would let two digests collide on one tag and
silently drop one manifest's GC protection — worse than no tag. (Existing latent bug either
way: `parse_canonical` accepts `sha512.<128 hex>`, a string that can never legally exist as
a tag.)

**Surface — 45 files (corrected 2026-08-29; was 29, and the list under it held 33).**

Census method matters here: an identifier grep for `canonical_tag|CanonicalTag|parse_canonical`
is **needle-blind**. It misses `sha256.<hex>` string literals in fixtures, prose spellings like
"canonical digest alias", and test names like `a_canonical_digest_tag_is_ignored`. Thirteen
files were found only by grepping for the *tag string form* (`sha256.`,
`format!("sha256.{…}")`), and one listed file (`oci/host_capabilities.rs`) turned out to be a
decoy — its only hit is `os_feature_tag_renders_canonical_tags`, about libc/os-feature tag
rendering, a third unrelated meaning of "canonical".

Four files carry **both** meanings and are the highest conflict risk — rename only the keep-tag
half in each: `oci/client.rs`, `package/cascade.rs`, `announce.rs`, and `oci/attest/pipeline.rs`
(whose lone `--no-canonical-tag` comment sits beside CANONICAL subject-descriptor prose).

| Surface | From | To | Contract? |
|---|---|---|---|
| CLI flags (`options/canonical_tag.rs`) | `--canonical-tag` / `--no-canonical-tag` | `--keep-tag` / `--no-keep-tag` | **Yes** |
| JSON field (`api/data/package_copy.rs:111`, `api/data/push.rs`) | `canonical_tags_written` | `keep_tags_written` | **Yes** |
| Option struct | `CanonicalTag` | `KeepTag` | internal |
| Tag variant (`package/tag.rs:87`) | `Tag::Canonical` | `Tag::Keep` | internal |
| Parser (`package/tag.rs:104`) | `parse_canonical` | `parse_keep` | internal |
| Client method (`oci/client.rs:706`) | `push_canonical_tag` | `push_keep_tag` | internal |
| Emitted tag (`oci/client.rs:739`) | `format!("{algorithm}.{hex}")` | `format!("__ocx.keep.{algorithm}-{hex}")` | **Yes** — wire |

```
crates/ocx_cli/src/options/canonical_tag.rs      → rename file to keep_tag.rs
crates/ocx_cli/src/options.rs
crates/ocx_cli/src/options/referrers.rs
crates/ocx_cli/src/command/package_push.rs
crates/ocx_cli/src/command/package_copy.rs
crates/ocx_cli/src/command/package_cascade.rs        (added — sha256. literal in a test)
crates/ocx_cli/src/command/package_announce.rs       (added — doc comment)
crates/ocx_cli/src/api/data/package_copy.rs
crates/ocx_cli/src/api/data/push.rs
crates/ocx_cli/src/api/data/announce.rs              (added — doc comment + fixture)
crates/ocx_lib/src/oci/client.rs                     BOTH meanings
crates/ocx_lib/src/oci/manifest.rs
crates/ocx_lib/src/oci/index.rs                      (doc comment :285, fixture :1524)
crates/ocx_lib/src/oci/index/local_index.rs
crates/ocx_lib/src/oci/index/oci_index.rs
crates/ocx_lib/src/oci/index/index_impl.rs           (doc comment :18)
crates/ocx_lib/src/oci/attest/pipeline.rs            BOTH meanings
crates/ocx_lib/src/announce.rs                       (added — BOTH meanings)
crates/ocx_lib/src/announce/pipeline.rs
crates/ocx_lib/src/announce/error.rs                 (added — fixture)
crates/ocx_lib/src/publisher.rs
crates/ocx_lib/src/publisher/copy.rs
crates/ocx_lib/src/managed_config/publish.rs
crates/ocx_lib/src/package/tag.rs
crates/ocx_lib/src/package/cascade.rs                BOTH meanings
crates/ocx_lib/src/package/cascade/gather.rs         (added — fixture)
crates/ocx_lib/src/package/cascade/graph.rs          (added — doc comments)
crates/ocx_lib/src/package/cascade/graph/tests.rs    (added — test names)
crates/ocx_lib/tests/fixtures/index_wire/tag_verdicts.json   (added — JSON fixture + prose)
test/src/helpers.py
test/tests/test_package_push.py
test/tests/test_package_copy.py
test/tests/test_package_cascade.py
test/tests/test_announce.py
test/tests/test_announce_push_file.py
test/tests/test_index_selfcontained.py
test/tests/test_index.py                             (added — doc comment)
test/tests/test_tag_reserved.py
test/manual/announce-e2e/README.md
test/manual/announce-e2e/scripts/run_sequence.sh     (added)
test/manual/announce-e2e/scripts/selfcheck_g2.sh     (added)
website/src/docs/reference/command-line.md
website/src/docs/user-guide/promoting-packages.md
website/src/docs/in-depth/indices.md
.claude/rules/subsystem-cli-commands.md              (added — living rule doc, documents the flag)
```

Dropped from the earlier list: `crates/ocx_lib/src/oci/host_capabilities.rs` (decoy).
`.claude/artifacts/*.md` are point-in-time records and are deliberately **not** renamed.

**The `--tags-file` grammar move rides this rename** (added 2026-08-29). `push --announce-file`
→ `--tags-file`, `announce --tags-from-file` removed and replaced by `--tags-file` + `--tags`
are pure CLI renames over the same files this rename already opens
(`command/package_push.rs`, `command/package_announce.rs`, `announce/pipeline.rs`,
`api/data/announce.rs`, the same tests and the same three website pages). Landing them together
means `package_announce.rs` and `announce/pipeline.rs` never appear in any later work package.

Comments and doc-comments count, not just identifiers — `oci/index.rs:285` and
`index/index_impl.rs:18` spell out "`sha256.<hex>` digest aliases" in prose.

**Parser changes (`package/tag.rs`).**

1. `parse_canonical` (`:104`) hardcodes `.strip_prefix('.')`. Give it a separator
   parameter — `'.'` for the frozen legacy form, `'-'` for the new namespaced form — so one
   hex/length validator serves both.
2. **Keep the legacy arm.** `sha256.<hex>` is not in the `__ocx` namespace; already-published
   repositories carry those tags and they must keep classifying as reserved. Name it
   honestly (`Tag::LegacyKeep`, or `Tag::Keep { legacy: bool }`) and never write it again.
3. `InternalTag::from_tag` (`:39`) is `match value` over literal strings. The keep tag is
   the **first parameterized internal tag**, so that function gains a prefix-strip arm and
   `InternalTag` gains a variant carrying data. This is the one non-mechanical edit.

## Keyless stays

**Format and key model are orthogonal axes.** D8's `simplesigning` selector picks the cosign
*sidecar wire shape*; it says nothing about how the signature was produced. Either format
can be produced keyless or with a key pair, which is exactly why the WP6 matrix multiplies
them rather than conflating them.

Keyless remains the **default and the differentiator** (#12): ephemeral P-256 key, Fulcio
cert bound to the OIDC identity, Rekor entry. WP9 *adds* key-pair support (D10); it does not
substitute for keyless, and no default changes. Under keyless a simplesigning `.sig` carries the
Fulcio cert and chain in annotations rather than in a bundle blob — that is the whole
difference between the formats.

## User-facing surface (review before implementation)

Everything below is a contract. Internal names are not listed.

### `ocx package sign`

| Flag | Status | Notes |
|---|---|---|
| `-p, --platform` | **changed** | No longer `required`. Absent = act on whatever the reference resolves to (index or bare manifest). Present = resolve into the index and act on that child; an error when the resolved object is not an index. |
| `--signature-format <bundle\|simplesigning\|both>` | **new** | Default `bundle` (D8). |
| `--key <REF>` | **new** | `[scheme://]<rest>`; bare or `file:` = file. Other schemes error by name (WP9). Unset = keyless. |
| `--rekor-upload` / `--no-rekor-upload` | **new** | **Keyless: always uploads** — `--no-rekor-upload` is an *error*, never a silent no-op (a Fulcio cert is valid ~10 minutes; the Rekor timestamp is the only proof the signature happened inside that window). **Key mode: off unless `--rekor-upload`.** Full reasoning in §Rekor-upload default. Paired-boolean with `overrides_with`, matching `--keep-tag` / `--no-keep-tag` and `login --[no-]verify` — not cosign's `=false` Go idiom. Named for Rekor, not "tlog": OCX names endpoints by product (`--fulcio-url`, `--rekor-url`), and *tlog* is the generic role, not a third service. The internal `verify/tlog.rs` keeps the role name; internal names are not contract. |
| `--tags <T,…>` | **new** | Repeatable *and* comma-delimited. |
| `--tags-file <PATH>` | **new** | Same file `push` writes and `announce` reads. |
| `--fulcio-url`, `--rekor-url`, `--identity-token-file`, `--identity-token-stdin`, `--no-tty`, `--no-cache` | unchanged | The keyless-only ones (`--fulcio-url`, `--identity-token-*`, `--no-tty`) are an **error** alongside `--key`, not silently ignored — a flag that does nothing is the failure mode this spec rejects everywhere else. `--rekor-url` stays meaningful in key mode under `--rekor-upload`. |

### `ocx package attest`

Same additions as `sign` (`--signature-format`, `--key`, `--rekor-upload`, `--tags`,
`--tags-file`, `--platform` optional). `--predicate`, `--type` unchanged.

### `ocx package verify`

| Flag | Status | Notes |
|---|---|---|
| `--signature-format <bundle\|simplesigning>` | **new** | Pin. Unset = prefer `bundle`, fall back to `simplesigning` (D9). |
| `--key <REF>` | **new** | Public key, same grammar as `sign`. Mirrors cosign, which reuses `--key` on both sides. |
| `-p, --platform` | **changed** | Optional, same rule as `sign`: absent verifies the resolved object, present narrows into an index to that child. Independent of D1's membership check, which decides *which signature satisfies* a subject — a platform manifest may be covered by a signature on its enclosing index. |
| `--certificate-identity`, `--certificate-oidc-issuer`, `--rekor-url`, `--attestation`, `--type`, `--no-cache`, `--sigstore-trusted-root` | unchanged | |

### `ocx package push`

| Flag | Status | Notes |
|---|---|---|
| `--keep-tag` / `--no-keep-tag` | **renamed** | Was `--canonical-tag` / `--no-canonical-tag` (WP7). |
| `--tags-file <PATH>` | **renamed** | Was `--announce-file`; now read by `announce` *and* `sign`. |
| `--sign` | **new** | Opt-in. Enables inline platform-manifest signing; `push` does not sign without it. |
| `--signature-format`, `--key`, `--rekor-upload` | **new** | Apply to the inline platform-manifest signing and to `--sbom`. An **error** without `--sign` unless `--sbom` is given — a flag that does nothing is the failure mode this spec rejects everywhere else. |

### `ocx package announce`

`--tags-from-file` **removed**, replaced by `--tags-file`. Gains `--tags` for symmetry.

### Config — `ocx.toml` / `config.toml`

```toml
[[trust.policy]]
scope   = "ghcr.io/acme/*"                       # unchanged
builder = "https://github.com/acme/..."          # unchanged
signers = [                                      # NEW — replaces [trust.policy.keyless]
  { kind = "keyless", identity = "release@acme.example",
                      oidc_issuer = "https://accounts.google.com" },
  { kind = "keyless", identity_regexp = "^ci-.*@acme\\.example$",
                      oidc_issuer = "https://token.actions.githubusercontent.com" },
  { kind = "key",     key = "file:etc/acme-release.pub" },   # path form
  { kind = "key",     key_pem = """                          # inline form
-----BEGIN PUBLIC KEY-----
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE...
-----END PUBLIC KEY-----""" },
]

[trust.sigstore]
rekor_upload = false     # NEW — fleet-wide default for --no-rekor-upload
```

`trusted_root` / `trusted_root_json` unchanged. Empty `signers` is an error, not a
catch-all.

**Inline key material — `key` XOR `key_pem`.** A managed-config payload is a `config.toml`
distributed as a package, so `key = "file:…"` names a path that exists on the operator's
disk and nowhere else. `SigstoreTrust` already solved this for the keyless side:
`trusted_root` (path) XOR `trusted_root_json` (verbatim), with `ocx config push` reading the
path form at publish time and inlining it, *"because a path on the operator's disk means
nothing on a consumer's"* (`trust.rs:131`). Key entries mirror that convention exactly —
same XOR rule, same publish-time inlining, no new concept. A self-hosted CA chain for
keyless verification is already covered by `trusted_root_json`.

**Rekor-upload default — keyless on (mandatory), key mode off** (revised 2026-08-29,
reversing an earlier "on for both").

- **Keyless: always uploads.** Not a default — a requirement. A Fulcio cert is valid ~10
  minutes; the Rekor timestamp is the only proof the signature happened inside that window.
  `--no-rekor-upload` is an error here, never a silent no-op.
- **Key mode: does not upload unless asked.** `--rekor-upload` opts in;
  `[trust.sigstore] rekor_upload = true` opts a fleet in.
- **`[trust.sigstore] rekor_upload` applies to key mode only.** Under keyless it is ignored,
  without a warning, because the keyless upload is a requirement rather than a default. The
  alternative — erroring on every keyless sign because a fleet-wide key-mode setting says
  `false` — would make an unrelated config key break the default signing path.

Why off is right for key mode, despite cosign defaulting on: `rekor_url` defaults to the
**public** Rekor, so an on-by-default key path publishes the digest and signer identity of a
private corporate artifact to a world-readable append-only log on first run. That is
irreversible. The opposite error — a signature with no transparency record — is fixed by
re-signing. Asymmetric harm decides it. And in key mode the log is not load-bearing for
verification, so its absence costs auditability, not verifiability.

This is **not** inferred from whether `rekor_url` is configured — that would be a silent
mode switch. It follows from the key model, which the user selected explicitly with `--key`.

**Deliberate divergence from cosign.** Cosign uploads by default in key mode too. This is a
flag default, not a wire format: an OCX-signed artifact and a cosign-signed artifact remain
byte-compatible either way, and every WP6 cell still passes. Divergence noted in WP8 docs so
the difference is discoverable rather than surprising.

**Visibility, since off is now possible.** The sign result — human-readable and
`--format json` — states whether a transparency record was created. A missing Rekor entry
must be a fact the operator can see, not an omission they infer.

### Environment

| Variable | Status | Notes |
|---|---|---|
| `OCX_KEY_PASSWORD` | **new** | Password for an encrypted key. Never a flag. |
| existing `OCX_IDENTITY_TOKEN`, `OCX_SIGSTORE_TRUSTED_ROOT`, `OCX_OFFLINE` | unchanged | |

### Registry-visible names

| Name | Status |
|---|---|
| `__ocx.keep.sha256-<hex>` | **new** — replaces `sha256.<hex>` (WP7) |
| `sha256-<hex>` | **new** — OCI referrers fallback index, written by WP2 |
| `sha256-<hex>.sig` / `.att` / `.sbom` | **new (opt-in)** — written only under `--signature-format simplesigning\|both` |

### `--format json`

| Field | Where | Status |
|---|---|---|
| `keep_tags_written` | push, copy | **renamed** from `canonical_tags_written` |
| `platform_digests` | push | **new** — the sign input, independent of keep tagging |
| `signatures[].signature_format` | verify | **new** — `bundle` \| `simplesigning` |
| `signatures[].discovery_method` | verify | **new** — referrers API \| fallback tag \| sidecar tag |
| `signatures[].key_backend` | verify | **new** — `keyless` \| `file` \| future scheme |
| `sbom[].shadowed` | sbom | **new** — a shadowed index-level SBOM stays listed |

### Exit codes

| Code | Status |
|---|---|
| 84 `ReferrersUnsupported` | **narrowed** — now only "Referrers API absent *and* fallback tag write refused" (D3) |
| unsupported key backend | **new** — a recognised scheme with no implementation, distinct from "file not found" |

## SBOM

Attached via **referrers**, never as a package layer. Unsigned attach writes the document
itself typed `application/vnd.cyclonedx+json` / `application/spdx+json` / `text/spdx`;
signed attach writes a Sigstore bundle with the SBOM's type as the DSSE `predicateType`
(`oci/referrer/media_types.rs`). This already matches cosign's modern model — you do not
sign an SBOM, you attest it.

**Subject:** per-platform preferred, index-level allowed. Dependencies genuinely differ per
architecture, so a single index-level SBOM is a lie for a multi-arch package.

**Resolution rules:**

- A platform-level SBOM shadows an index-level one **only within the same predicateType** —
  a platform CycloneDX must not hide an index-level SPDX; they are not substitutes.
- A shadowed entry stays visible in `--format json`, marked as shadowed. Only the
  human-readable default collapses to the preferred one.
- With no platform selected, report all, grouped by subject.

Multiple SBOMs per package is normal: different formats for different consumers, different
lifecycle phases, and rescans. Two SBOMs of the same format have no disambiguation
convention beyond `org.opencontainers.image.created`; take all and let policy decide.

The fallback index is per-subject and type-agnostic, so signatures, attestations and SBOMs
all append through the one D4 retry loop — nothing SBOM-specific to build.

### Multiple formats, side by side

`SBOM_ARTIFACT_TYPES` already declares CycloneDX JSON, SPDX JSON and SPDX tag-value; the
attach path picks one per `--type`, so attaching several formats to one subject is
attaching several times. Each lands as its own referrer and all of them list.

Both entry points must honour `--signature-format` (D8): `ocx package push --sbom FILE` and
`ocx package attest`. Signed attach produces a full DSSE attestation — the SBOM is the
in-toto predicate, the subject is the manifest — so an SBOM is signed exactly as any other
attestation, under whichever key model the command was given, with no separate mechanism.

**Known asymmetry:** discovery and verification are format-agnostic (the payload is opaque
bytes), but `crates/ocx_lib/src/sbom.rs` parses and summarizes **CycloneDX 1.5–1.7 only** —
there is deliberately no `SbomFormat` trait (`adr_sbom_attestations.md` D2/D-i). So an SPDX
SBOM attaches, signs, verifies and lists correctly but cannot be summarized. Resolved in
§Resolved below: `--summary` stays CycloneDX-only.

## Sequencing

> Section order above is **not** execution order — the work packages are written in the
> order they were decided. Execute in the order below.

```
WP7 (keep-tag rename + --tags-file grammar move)  ──►  golden cosign fixtures  ──┐
                                                                                 ▼
WP1 (read fallback)  ──►  WP2 (write fallback)  ─────────────────────────────────┐
WP9a (trust policy + key primitives) ────────────────────────────────────────────┤
                                                                                 ▼
WP3 (sign DSSE) ∥ WP5b (simplesigning write) ∥ WP9b-sign   ──┐
WP4 (verify DSSE) ∥ WP5 (simplesigning read) ∥ WP9b-verify ──┤
                                                             ▼
                                        WP6 (interop matrix) ──► WP8 (docs + casts)
```

**Revised 2026-08-29 — WP7 goes first, not second.** The spec's earlier reasoning ("easier
second, once WP2 has settled what the bare dash form means") does not survive contact with the
census: `sha256-<hex>` is reserved by the OCI dist-spec whether or not OCX writes it, so WP7's
rejected-spellings argument never depended on WP2. What WP7 *does* depend on is being alone —
45 files across every subsystem, including two files WP1/WP2 own. Running it first means every
later work package writes the new names from the start instead of being renamed under itself,
and it retires the whole `--tags-file` CLI-grammar move at the same time (§WP7).

**Revised 2026-08-29 — WP4 no longer waits on WP3.** "Verify has nothing to read until sign
emits the new shape" is only true if verify's only test input is OCX's own output. It is not:
`spike_cosign_bundle.json` and `spike_cosign_attestation_referrer.json` are already committed
cosign bytes, and D7 already commits simplesigning read fixtures. Verifying against *cosign's*
bytes is the actual parity requirement, so building the read side against committed fixtures is
both the stronger test and the schedule unlock. WP4 and WP5 therefore run **concurrently with**
WP3 and WP5b, split by file (`verify/**` versus `sign/**`) rather than in sequence. WP6 still
gates everything, and it is where the round trip through OCX's own output is proved.

Dependencies that actually bind:

- **WP2 after WP1** — the write path needs the read path to verify what it wrote.
- **WP3 ∥ WP4, and WP5 ∥ WP5b** — split by file (`sign/**` versus `verify/**`), not in
  sequence. Both read sides build against **committed cosign bytes**, which is the stronger
  test as well as the schedule unlock. This edge is deleted *conditionally*: it holds only once
  the golden fixtures below exist. Until then, the old serial edge stands.
- **Golden fixtures before WP3/WP4/WP5/WP5b.** The two fixtures already in the tree are not
  enough — `spike_cosign_bundle.json` is an **attestation** (predicateType
  `https://cyclonedx.org/bom`, non-empty predicate), its verification material is a `publicKey`
  with no cert chain, and its tlog entry is a public-good `rekor.sigstore.dev` checkpoint, which
  §WP6 forbids depending on. So the rename wave also generates and commits, with cosign 3.x
  against the local stack: (a) a keyless DSSE **image-signature** bundle + its referrer manifest,
  (b) the key-mode equivalent, (c) a `sha256-<hex>.sig` simplesigning manifest and layer bytes,
  keyless and key. The generation script is committed beside them.
- **WP6 after everything** — the matrix cannot be populated until every axis exists. It is
  the gate, not a parallel track.
- **WP8 after WP6** — a cast recording an unproven claim is worse than no cast.
- **WP9a before WP3/WP4** — the key-mode `Signer` delegates to its `KeyBackend`, and key
  verification resolves its `PolicyBackend::Key`. WP9b (the pipeline wiring) rides the sign and
  verify halves respectively.

File conflict: `crates/ocx_lib/src/package/tag.rs` is touched by WP1/WP2
(`is_referrer_fallback_tag`, the bare-dash write path) and by WP7 (`parse_canonical`,
`Tag::Canonical`, `InternalTag::from_tag`). Different functions, same file — they must not
run as concurrent worktree packages. WP7 runs **first and alone** (see the revision above), so
the conflict never arises.

**Two unrelated things share the word "fallback" — do not wire them together.** The OCI
*fallback index* (tag `sha256-<hex>` holding an image index of referrer descriptors)
substitutes for a missing Referrers API, and is WP1/WP2's subject. cosign *simplesigning
sidecars* (`sha256-<hex>.sig`) never used the Referrers API at all — they are ordinary tags
that work on every registry. So under `--signature-format simplesigning` the fallback index
is **not involved** and WP2's machinery is never reached. The `.sig` manifest has its own
append race, handled by WP5b reusing D4's retry loop, but the two objects are unrelated.

## Verification

- `task verify` — never piped, always `--force`.
- **Red/green both ways** on every new classifier arm: a `__ocx.keep.sha256-<hex>` tag must
  be filtered from `list_tags` and the legacy `sha256.<hex>` form must still be filtered
  (`test/tests/test_tag_reserved.py`) — prove each can go red by breaking its arm.
- WP6 interop suite is the only evidence that counts for the parity claim. A green that
  never invoked cosign is indistinguishable from cosign never having run — assert on
  cosign's own output, not merely on its exit code.
- WP8 casts are regenerated in the same commit as any command change they display.
- Concurrency test for D4: two writers against one fallback index, both descriptors present
  afterwards.
- Grep gate before commit: no `canonical_tag`, `canonical-tag`, `CanonicalTag`,
  `parse_canonical`, `canonical_tags` outside the frozen legacy arm and its comment.

## Contract note

CLI grammar, `--format json` shapes, exit codes and emitted tag strings are all interfaces.
Pre-1.0 they break outright — no dual-form parsing, no deprecation window, no compat alias.
**The changelog entry is the commit subject**; never edit `CHANGELOG.md`.

## Rejected

**Signing tags.** A tag cannot be signed, only a *statement about* a tag, which yields
detection rather than prevention and only for a client holding a trusted prior record.
Repointing to attacker content is already dead (no valid signature on the new digest).
Rollback and freeze are untouched by any signature — the OCX index already answers them
with `content` + `observed` + `yanked` (`oci/index/wire.rs:163`) plus lock pinning, which is
TUF's targets role in a different shape. Cosign punts on this entirely; TUF/Notary v1
solved it and was retired as too heavy.

**Renaming the canonical tag to `sha256-<hex>`.** See WP7's rejected-spellings table.

**Index-root signing.** The OCX index's integrity today is digest-chain verification plus
TLS (`oci/index/chained_index.rs:119`), with nothing signing the root. That is the one place
a signature would add real security value — and it is a separate ADR, not this release.

**Entangling index signing with `announce`.** Announce needs forge access and opens a PR;
private-registry publishers never announce. Signing must stand alone.

## Resolved (2026-08-29)

- **`PushReport` gains `platform_digests`** as a first-class field. Deriving them by parsing
  keep tag names breaks under `--no-keep-tag`, which would silently remove the sign input.
  Additive JSON field, so `ocx-mirror pipeline push` is unaffected.
- **`.sbom` classifier fix moves to WP1/WP2** (revised 2026-08-29, was "folds into WP5").
  `is_referrer_fallback_tag` (`package/tag.rs:121`) strips `.sig` and `.att` but not `.sbom`,
  so a `cosign attach sbom` sidecar in a third-party repo classifies as a package version. It
  is a two-line edit in `package/tag.rs`, and WP1/WP2 is the only work package that owns that
  file — putting it in WP5 would make two concurrently-planned packages edit one file for no
  benefit. The reader still lands in WP5; classifier and reader are independently testable.
- **`--summary` stays CycloneDX-only.** SPDX attaches, signs, verifies and lists; only the
  human-readable summary refuses it. `adr_sbom_attestations.md` D2/D-i deliberately withheld
  an `SbomFormat` trait until a second real implementation earns it, and an SPDX parser is
  SBOM tooling, not cosign parity. Document the asymmetry rather than close it here.
- **GitHub untouched for now.** [#197](https://github.com/ocx-sh/ocx/issues/197) and
  [#356](https://github.com/ocx-sh/ocx/issues/356) stay as they are; this spec is the plan
  until implementation starts. Note for whoever picks that up: #197's closing rationale is
  stale (see §Background).

## Spec refinements (2026-08-29, hex-architect pass)

Validation pass against the tree at `e598cfc8`. Every `file.rs:NNN` in this spec was
re-checked, every "current state" claim was re-read from source, and the WP7 surface was
re-censused. Refinements that **changed a decision** are marked ⚑; the rest are corrections.

**Decisions changed**

1. ⚑ **§Background — the #197 spike was run, at blob level, and passes.**
   `test/tests/test_cosign_interop.py` (5 tests) + `test/tests/fixtures/cosign.py` already prove
   bidirectional bundle interop against the local Fulcio/Rekor with cosign v3.1.1. They use
   `verify-blob` / `sign-blob` / `verify-blob-attestation` / `attest-blob`, which accept a
   `messageSignature` payload and never resolve a registry reference. The blocking finding
   survives but narrows: what is missing is *image-level* parity (`cosign verify <ref>`, which
   demands DSSE) and discovery. **WP6 extends that file; it does not create one.** Rewriting its
   module docstring — which currently states OCX has no tag-schema fallback and that discovery
   is out of scope — is a required WP6 edit.

2. ⚑ **WP6 cosign driver — keep the container, add the `ocx.toml` entry for casts only.**
   The `acceptance-tests` job in `verify-basic.yml` does not run `setup-ocx` and does not
   materialise the `ocx.toml` toolchain, so a toolchain entry alone puts no `cosign` on `PATH`
   in CI; wiring it would make the system under test install its own test tool. The existing
   `docker run` driver is a real cosign, already pinned, already green, and adds no dependency
   the suite lacks. `ocx.toml` still gains `cosign = "ocx.sh/sigstore/cosign:3"` for local dev
   and WP8's bare-`cosign` casts, with `fixtures/cosign.py` deriving its image tag from that pin
   so the two cannot drift. **This is a decision, not an open question** — cosign resolves in
   the index today (`ocx index list ocx.sh/sigstore/cosign` → 3.0.6 … 3.1.3), so there is no
   WP6 blocker.

3. ⚑ **WP9 cannot ride WP3/WP4's crate machinery.** `sigstore` 0.14.0 (the newest release) has
   no `Content::PublicKey` arm in either `bundle::sign` or `bundle::verify`; the key-mode path
   must hand-assemble and hand-match `VerificationMaterial` against `sigstore_protobuf_specs`.
   Offsetting good news: `SigStoreKeyPair::from_encrypted_pem` already decrypts cosign's
   encrypted PEM, so no scrypt/PEM ownership is created. Recorded in WP9.

4. ⚑ **The `--tags-file` grammar move rides WP7's rename.** `push --announce-file` →
   `--tags-file`, and `announce --tags-from-file` → `--tags-file` + `--tags`, are pure CLI
   renames over files WP7 already opens. Landing them together retires
   `command/package_announce.rs` and `announce/pipeline.rs` from every later work package.

5. ⚑ **The `.sbom` classifier fix moves from WP5 to WP1/WP2.** It is a two-line edit in
   `package/tag.rs`, the one file WP1/WP2 owns and WP5 otherwise would not need to touch.

6. ⚑ **WP7's surface is 45 files, not 29** (and the list under that heading held 33). An
   identifier grep is needle-blind: 13 files carry only `sha256.<hex>` string literals, prose
   spellings, or test names like `a_canonical_digest_tag_is_ignored`. One listed file
   (`oci/host_capabilities.rs`) was a decoy. Four files carry both the keep-tag and the
   canonical-*registry* meaning and must be renamed by half.

**Corrections (no decision changed)**

7. `resolve_policies` does not exist **in `trust.rs`** — the tier-precedence function there is
   **`resolve_tiered`** (`trust.rs:810`). The behavioural claim is accurate; only the symbol name
   was wrong. (A different `resolve_policies` does exist, in
   `crates/ocx_cli/src/command/package_sign_common.rs:358` — it turns flags into a
   `CompiledPolicy`. Do not conflate them; the meta-plan puts that one under G1's ownership.)
8. Line drift: `oci/client.rs:1261` → `:1282` (`push_manifest_and_merge_tags` declaration; 1261
   is a call site); `native_transport.rs:673` → `:687` (673 is `list_referrers`, the enclosing
   function); `package/tag.rs:123` → `:121`; `oci/client.rs:704` → `:702`, `:740` → `:739`,
   `:692` → `:691`.
9. Confirmed unchanged and correct: `signer.rs:109`/`:33`, `tlog.rs:33`, `client.rs:706`,
   `rust-oci-client/client.rs:2266`, `digest.rs:26`, `index.rs:285`, `index_impl.rs:18`,
   `wire.rs:163`, `chained_index.rs:119`, `tag.rs:39`/`:60`/`:104`, `trust.rs:131`/`:519`/`:663`,
   `package_copy.rs:111`, `media_types.rs`, `sbom.rs`.
10. Confirmed unchanged: the WP6 service table matches `test/docker-compose.yml` exactly (dex
    v2.45.1, fulcio v1.8.8, `tesseract/posix` pinned by digest, rekor-server v1.4.2 + trillian +
    mysql, zot v2.1.18 ×3, `registry:2`). `PolicyBackend` is single-variant `Keyless` and
    documents that it is deliberately not `#[non_exhaustive]`. `--platform` is `required = true`
    on sign, verify and attest today. `PushReport` has no `platform_digests`. Nothing in the tree
    implements DSSE image signatures, simplesigning, `--key`, `--signature-format`,
    `--rekor-upload`, or a fallback-tag write.
11. External facts re-verified against upstream: cosign's `CosignSignPredicateType`, the
    `dev.sigstore.bundle.content` / `.predicateType` annotations, bundle media type v0.3 as
    current with no v0.4, every simplesigning annotation key and both media types, the
    dist-spec fallback-tag truncation rule (byte-identical to `sha256-<full hex>`),
    [sigstore/cosign#4641](https://github.com/sigstore/cosign/issues/4641) still open, and the
    public-good instance staying on Rekor v1
    ([blog.sigstore.dev/rekor-v2-ga](https://blog.sigstore.dev/rekor-v2-ga/), 2025-10-10).
    One nuance for §Rekor-upload default: cosign 3.x spells the flag `--tlog-upload` and
    defaults it `true` **uniformly** across keyless and key mode, so OCX's key-mode-off default
    really is a deliberate divergence, exactly as the spec frames it.
12. `--new-bundle-format` is still a flag in cosign 3.x but **defaults to on** since v3.0.0.
13. Also present in the tree and usable as committed fixtures:
    `test/tests/fixtures/spike_cosign_bundle.json` (key-mode bundle, `publicKey.hint`) and
    `spike_cosign_attestation_referrer.json`.

**Holes filled by the adversarial review round (same pass, all ⚑ — each changed a decision or
made an undecidable one decidable)**

14. ⚑ **`push` gains `--sign`.** `push` has no sign path today, so D1's "every platform manifest
    ends up signed" had no trigger. Signing unconditionally would spend a Fulcio cert and a Rekor
    entry on every push and fail every push made without an OIDC identity. `--signature-format` /
    `--key` / `--rekor-upload` are an error without `--sign` unless `--sbom` is given.
15. ⚑ **The unsupported-key-backend exit code is 85**, and `error_envelope.rs`'s
    `ExitCode → ErrorCategory` match ends in `_ => Internal` — a new code compiles clean and
    misclassifies silently, so it needs its own arm plus a test that reds without it.
16. ⚑ **`--signature-format both` is best-effort per leg, never atomic.** Two Fulcio certs and
    two Rekor entries make rollback impossible; the report lists each leg and the exit code is
    non-zero if any leg failed.
17. ⚑ **D6's dedup key extended.** "Deduped on Rekor log index" fails exactly where
    double-discovery is most likely: key mode defaults to no upload, so two surfacings of one
    key-mode signature carry no log index. Fallback key is (signature bytes, subject digest,
    `signature_format`).
18. ⚑ **The `publicKey` `hint` derivation is pinned**: base64 of SHA-256 over the DER
    `SubjectPublicKeyInfo`, matching cosign, asserted against the committed fixture. It is
    wire-visible and hand-assembled, so leaving it unstated was a wire-format hole.
19. ⚑ **`[trust.sigstore] rekor_upload` applies to key mode only**; under keyless it is ignored
    without warning. Otherwise a fleet-wide key-mode setting would error out every keyless sign.
20. ⚑ **D1's membership check has a stated mechanism and two fail-closed cases**: fetch the
    resolved index and match the platform digest against `manifests[]`; a bare platform digest,
    or an unfetchable index (offline/uncached), means the index signature is not considered.
21. ⚑ **Sweep semantics stated**: a swept tag resolving to a bare manifest is skipped with a
    warning; the sweep continues past a per-tag failure and exits non-zero at the end with every
    failure listed. `--platform` is exclusive with both `--tags` and `--tags-file`.
22. ⚑ **D5's "identical gate" under a key is defined**: public-key match against `--key` or a
    `kind = "key"` signers entry; keyless certificate matchers are an error alongside `--key`;
    a policy whose applicable signers are all keyless refuses a key-signed artifact.
23. ⚑ **WP6 plans around two cosign-side facts instead of discovering them.**
    `--new-bundle-format` defaults on since v3.0.0, so the four "cosign signs → simplesigning"
    cells drive `--new-bundle-format=false` and the suite asserts that flag still exists. And
    [#4641](https://github.com/sigstore/cosign/issues/4641) makes the two
    "cosign signs × bundle × Referrers-absent" cells unproducible faithfully — recorded as a WP8
    gap now, not found later.
24. ⚑ **The `KeyBackend` ARCH-07 justification was wrong and is corrected.** WP6's key axis uses
    a real committed key pair and the file backend — no test double appears there. The double
    that earns the trait is the in-memory signer `oci/sign`'s unit tests use.
25. ⚑ **The golden-fixture gap is the one that nearly shipped as a false schedule.** Decoupling
    WP4 from WP3 rests on committed cosign bytes, and the two fixtures already in the tree do not
    serve: `spike_cosign_bundle.json` is an *attestation* (predicateType
    `https://cyclonedx.org/bom`, non-empty predicate), carries `publicKey` material with no cert
    chain, and its tlog entry is a public-good `rekor.sigstore.dev` checkpoint that §WP6 forbids
    depending on. Both are also currently unreferenced by any test. The rename wave therefore
    generates and commits real fixtures first; until they exist the serial WP3 → WP4 edge stands.
26. Corrected in passing: the both-meanings file count is **four**, not three
    (`oci/attest/pipeline.rs` was in a parenthetical); `--summary` is stated once, as resolved.

**Execution decomposition** lives in
[`meta-plan_cosign_parity.md`](./meta-plan_cosign_parity.md). Nothing in this spec's scope was
cut.

## References

- OCI distribution spec, Referrers Tag Schema — `<truncated-alg>-<truncated-encoded>`,
  encoded truncated to 64; a 404 on the referrers API **MUST** fall back to it
- [cosign BUNDLE_SPEC](https://github.com/sigstore/cosign/blob/main/specs/BUNDLE_SPEC.md)
- `pkg/types/predicate.go` — `CosignSignPredicateType = "https://sigstore.dev/cosign/sign/v1"`
- `pkg/oci/remote/write.go` — `WriteReferrer`, `WriteAttestationNewBundleFormat`,
  `WriteSignaturesExperimentalOCI`
- [sigstore/cosign#4641](https://github.com/sigstore/cosign/issues/4641) — broken fallback-tag write
- [sigstore/cosign#3927](https://github.com/sigstore/cosign/issues/3927) — image signature bundles as OCI artifacts
