# Meta-Plan — cosign parity execution waves

**Status:** Approved (execution contract), not started
**Date:** 2026-08-29
**Branch:** `feat/cosign-parity`
**Source spec:** [`design_spec_cosign_parity.md`](./design_spec_cosign_parity.md) (validated
against the tree at `e598cfc8` — see its §Spec refinements)
**Origin:** [#356](https://github.com/ocx-sh/ocx/issues/356)

**Ownership is wave-scoped.** A file may be owned by different loops in *different* waves —
that is sequential, not concurrent, and it is legal. Only an intra-wave overlap is forbidden, and
the disjointness proof below covers exactly the two waves that run loops concurrently.

This file is the execution contract for the rest of the initiative. Each **inner loop** below is
one unit driven by a single Opus 5 sub-orchestrator through
`/hex-plan → /hex-execute → /hex-review (scoped to the loop) → /hex-execute (fix findings)`.
Nothing in the spec's scope is cut; work that looked too big became its own loop.

---

## Wave schedule

| Wave | Loops | Mode | Covers |
|---|---|---|---|
| **0** | G0 | solo, main worktree | WP7 + the `--tags-file` CLI-grammar move + golden cosign fixtures |
| **1** | G1 | solo, main worktree | Shared contracts, frozen for waves 2–3 |
| **2** | A ∥ B | 2 worktrees | WP1+WP2 (referrers fallback) ∥ WP9a (trust + key primitives) |
| **3** | C ∥ D | 2 worktrees | WP3+WP5b+WP9b-sign ∥ WP4+WP5+WP9b-verify |
| **4** | E | solo, main worktree | WP6 interop matrix |
| **5** | F | solo, main worktree | WP8 parity docs + casts |

Two solo waves up front are deliberate. A 45-file mechanical rename and a contract-stub commit
each conflict with everything and each finish fast; paying for them serially buys two waves of
genuine 2-way parallelism afterwards with zero rename churn.

### Dependency DAG

```
G0 (rename + CLI grammar)
 └─► G1 (contract stubs)
      ├─► A (referrers fallback read+write) ──┐
      └─► B (trust policy + key primitives) ──┤
                                              ├─► C (sign side) ──┐
                                              └─► D (verify side) ┤
                                                                  ├─► E (interop matrix) ─► F (docs + casts)
```

Binding edges, and why:

- **G0 → everything.** Renames the emitted tag string, two CLI flags and one JSON field. Any
  later loop written against the old names would be rewritten.
- **G1 → everything.** Concurrent loops must compile against one shared shape.
- **A → C, D.** C and D read fallback-tag referrers; the read path must exist first.
- **B → C, D.** C's key-mode `Signer` delegates to B's `KeyBackend`; D's key verification
  resolves B's `PolicyBackend::Key`.
- **C, D → E.** The matrix cannot be populated until every axis exists. E is the gate, not a
  parallel track.
- **E → F.** A cast recording an unproven claim is worse than no cast.

Non-edges, decided in this pass: **WP4 does not wait on WP3, and WP5 does not wait on WP5b** —
the read sides build against committed cosign bytes, which is the stronger test as well as the
schedule unlock. The round trip through OCX's own output is proved in E, where it belongs.

**This non-edge is conditional and the condition is not yet met.** The two fixtures already in
the tree do not serve: `spike_cosign_bundle.json` is a CycloneDX *attestation* (non-empty
predicate), carries `publicKey` material with no cert chain, and its tlog entry is a public-good
`rekor.sigstore.dev` checkpoint that §WP6 forbids depending on — and neither it nor
`spike_cosign_attestation_referrer.json` is referenced by any test today. G0 therefore generates
and commits real golden fixtures (see G0's acceptance criteria). **If G0 does not ship them, the
serial C → D edge returns and wave 3 becomes two solo waves.** That is the meta-orchestrator's
call at G0's exit gate, and it is the one place this schedule can legitimately lengthen.

---

## Inner loops

### G0 — keep-tag rename and CLI grammar move

| | |
|---|---|
| **Covers** | WP7, plus `push --announce-file`→`--tags-file`, `announce --tags-from-file` → `--tags-file` (~~+ `--tags`~~ — **amended by G0**: `announce --tags` already exists, `command/package_announce.rs:52-59`, already repeatable and comma-delimited; nothing to add), plus the **golden cosign fixtures** |
| **Tier** | `medium` — mechanical in shape, but it changes a wire-visible tag string and two CLI flags |
| **Mode** | solo, `feat/cosign-parity` in the main worktree |
| **Model** | builders `sonnet` (mechanical rename against a decided shape); reviewer `opus` (wire + CLI contract) |

**Owns** — the 45 files in the spec's corrected WP7 surface table, plus
`crates/ocx_cli/src/options/` (the `canonical_tag.rs` → `keep_tag.rs` file rename) and the
`--tags-file` sites in `crates/ocx_cli/src/command/package_announce.rs`,
`crates/ocx_lib/src/announce/pipeline.rs`, `crates/ocx_cli/src/api/data/announce.rs`.

**Acceptance criteria**

- `__ocx.keep.sha256-<hex>` is emitted; `sha256.<hex>` is never written again.
- The legacy `sha256.<hex>` read arm survives, named honestly (`Tag::LegacyKeep` or
  `Tag::Keep { legacy: bool }`), and still classifies as reserved.
- `InternalTag::from_tag` gains its first parameterized arm — the one non-mechanical edit.
- `Ok(None)` when the full tag would exceed 128 chars (sha512), never truncation.
- Four files renamed by half only: `oci/client.rs`, `package/cascade.rs`, `announce.rs`,
  `oci/attest/pipeline.rs` — the canonical-*registry* meaning is untouched.
- **Golden cosign fixtures generated and committed**, with their generation script, under
  `test/tests/fixtures/golden/`: (a) a keyless DSSE **image-signature** bundle
  (predicateType `https://sigstore.dev/cosign/sign/v1`, empty predicate) plus its referrer
  manifest, (b) the key-mode equivalent, (c) a `sha256-<hex>.sig` simplesigning manifest and
  layer bytes, keyless and key. All produced by cosign 3.x against the **local** stack.
  The two existing spike fixtures do **not** serve — `spike_cosign_bundle.json` is a CycloneDX
  *attestation* with public-good-Rekor material, and neither is referenced by any test today.
  This deliverable is what makes C ∥ D legal; without it the serial C → D edge returns.

**Verification** — `task rust:verify` then
`cd test && uv run pytest tests/test_tag_reserved.py tests/test_package_push.py tests/test_package_copy.py tests/test_package_cascade.py tests/test_announce.py tests/test_announce_push_file.py tests/test_index.py tests/test_index_selfcontained.py -v`.

**Red/green evidence required** — for both the new `__ocx.keep.sha256-<hex>` arm and the frozen
legacy `sha256.<hex>` arm: break each classifier arm in turn, show `test_tag_reserved.py` red,
restore, show it green. Two independent guards defend one property, so mutating one may not red
the suite — keep mutating until each arm reds on its own.

**Exit gate** — `task verify --force` green (never piped), plus the grep gate: no
`canonical_tag`, `canonical-tag`, `CanonicalTag`, `parse_canonical`, `canonical_tags` outside the
frozen legacy arm and its comment, **and** no `sha256.` tag literal outside that arm. Run the
grep with `grep -e` per needle — the `rg` binary here false-negatives on alternation.

---

### G1 — shared contracts

| | |
|---|---|
| **Covers** | Shared-contract set (below): frozen shapes, plus the three small units that are cheaper implemented than stubbed |
| **Tier** | `medium` |
| **Mode** | solo, `feat/cosign-parity` in the main worktree |
| **Model** | `opus` — these are wire formats, exit-code semantics and CLI grammar |

**Owns** — every file in the stub table below. After G1, no later loop creates a shared type.

**Two files were nearly missed and are the reason this wave is not optional.**
`crates/ocx_cli/src/command/package_sign_common.rs` is imported by `package_sign.rs`,
`package_attest.rs`, `package_push.rs` (all C) *and* `package_verify.rs` (D) — it holds the only
place a flag becomes a `CompiledPolicy` (`resolve_policies` / `resolve_policies_lenient`) and the
only place `resolve_sigstore_pair` / `refuse_when_offline` / `resolve_override_token` live. And
`crates/ocx_lib/src/oci/attest.rs` — the sibling module file, which `oci/attest/**` does **not**
match — owns `DSSE_PAYLOAD_TYPE`, `STATEMENT_TYPE_WRITTEN` and `ACCEPTED_STATEMENT_TYPES`, read
by D at `verify/pipeline.rs:45,46` and `verify/dsse.rs:29-31` and written through by C. Both are
G1's, fully implemented, read-only in wave 3.

**Rule G1 applies:** *fully implement* shapes that are pure data and smaller than their stub
(serde structs, enums, media-type constants, `platform_digests`); *stub with `unimplemented!()`*
only the behavioural seams (`KeyBackend::sign_prehash`, the format-dispatch arms).

**Acceptance criteria** — `cargo check --workspace` green; every stub is referenced by at least
one caller so `-D dead-code` cannot hide an unconstructed type; no `--help` text promises
behaviour that does not exist yet (flags land hidden or erroring, never silently ignored).

**Verification** — `task rust:verify`, plus named unit tests for the three rows this wave fully
implements with behaviour rather than shape: `resolve_sign_target` (the `--platform` optionality
rule), `oci/simplesigning.rs` claim construction, and `KeyRef::parse` including every recognised
but unimplemented scheme. `cargo check` alone is not this wave's gate — two of its rows are
behaviour, and a stub table that quietly grows logic without tests is how a contract wave becomes
an untested one.

**Exit gate** — `task verify --force` green; the shared shapes are frozen for waves 2–3.

---

### A — referrers fallback, read and write

| | |
|---|---|
| **Covers** | WP1, WP2, the `.sbom` classifier fix, the ADR amendment |
| **Tier** | `high` — transport, exit-code semantics, a read-modify-write race |
| **Mode** | worktree `.agents/worktrees/cosign-a-referrers` off `feat/cosign-parity` |
| **Model** | `opus` (protocol + concurrency + error semantics) |

**Owns**

```
crates/ocx_lib/src/oci/client/native_transport.rs
crates/ocx_lib/src/oci/client/transport.rs
crates/ocx_lib/src/oci/client/test_transport.rs
crates/ocx_lib/src/oci/client/error.rs
crates/ocx_lib/src/oci/referrer/capability.rs
crates/ocx_lib/src/oci/referrer/manifest.rs
crates/ocx_lib/src/oci/client.rs
crates/ocx_lib/src/package/tag.rs
crates/ocx_lib/src/oci/copy.rs
external/rust-oci-client/**                     (only if the fork needs a change)
test/tests/test_referrers_capability.py
test/tests/test_referrers_smoke.py
test/tests/test_referrers_fallback.py           (new)
.claude/artifacts/adr_oci_referrers_signing_v1.md
```

**Acceptance criteria**

- `list_referrers` routes through the fallback-capable `pull_referrers`; a 404 on the Referrers
  API is no longer by itself a read failure.
- D3 exactly: 84 narrowed to *write* — "Referrers API absent **and** the fallback tag write was
  refused". On read, no API and no fallback tag is **79** (no signatures found), never 84.
  **A owns only the client-layer producers** (`client/native_transport.rs:357,362,692`,
  `referrer/capability.rs:95,352`, `copy.rs:286`) and its evidence is client-level unit tests.
  The command-level producers live in files A does not own — `verify/pipeline.rs:1397-1399,2025`
  and `verify/error.rs:195,662` (D), `sign/pipeline.rs:296,304` + `sign/error.rs:113,269` and
  `attest/pipeline.rs:512,526` (C). `ocx package verify` still exits 84 after A lands; asserting
  the end-to-end 84/79 split inside A would be an unchecked green. The read-side assertion is D's
  acceptance criterion, the write-side is C's, and E proves the pair end to end.
- Fallback-index write preserves `artifactType` and annotations (the thing
  [sigstore/cosign#4641](https://github.com/sigstore/cosign/issues/4641) gets wrong).
- D4: read-back plus bounded retry, dedupe by descriptor digest, loud failure on exhaustion.
- `is_referrer_fallback_tag` strips `.sbom` alongside `.sig` and `.att`.
- `adr_oci_referrers_signing_v1.md` S1-F amended, and its "no `sha256-<hex>.sig|.att` manifest
  write" test tape assertion inverted.

**Verification** — `task rust:verify` then
`cd test && uv run pytest tests/test_referrers_capability.py tests/test_referrers_smoke.py tests/test_referrers_fallback.py tests/test_tag_reserved.py -v`
(needs the `mirror-registry` `registry:2` service — the Referrers-API-absent half).

**Red/green evidence required** — the D4 concurrency test: two writers against one fallback
index, both descriptors present afterwards; show it red by removing the read-back. The 84/79
split: show each code produced on its own path, and show the test red when the split is
inverted.

**Exit gate** — `task verify --force` green; the concurrency test proved both ways.

---

### B — trust policy and key primitives

| | |
|---|---|
| **Covers** | WP9a — the half of WP9 that touches no pipeline |
| **Tier** | `high` — security config, key parsing, fail-closed semantics |
| **Mode** | worktree `.agents/worktrees/cosign-b-keys` off `feat/cosign-parity` |
| **Model** | `opus` (crypto + trust boundary) |

**Owns**

```
crates/ocx_lib/src/trust.rs
crates/ocx_lib/src/oci/sign/key_backend.rs      (implementation; G1 stubbed the trait)
crates/ocx_lib/src/oci/sign/key_ref.rs          (scheme grammar; G1 stubbed)
crates/ocx_lib/src/oci/verify/trust_resolve.rs
crates/ocx_lib/src/oci/verify/trust_cache.rs
crates/ocx_lib/src/managed_config/**            (publish-time key inlining)
crates/ocx_schema/**                            (config.toml / ocx.toml schema regeneration)
crates/ocx_lib/src/config.rs                    (consumer of the policy shape)
crates/ocx_lib/src/config/loader.rs             (consumer)
crates/ocx_lib/src/project/config.rs            (consumer)
crates/ocx_lib/src/app/context.rs               (consumer)
crates/ocx_lib/src/oci/verify/identity.rs       (irrefutable `let PolicyBackend::Keyless(..)` — breaks)
crates/ocx_lib/src/oci/verify/dsse.rs           (consumer)
crates/ocx_lib/src/package_manager/tasks/auto_verify.rs   (constructs a TrustPolicy literal)
crates/ocx_lib/src/package_manager/tasks/verify.rs        (consumer)
crates/ocx_lib/src/package_manager/tasks/sbom.rs          (consumer)
crates/ocx_cli/src/command/package_sbom.rs      (consumer)
test/tests/test_trust_policy_signers.py         (new)
test/tests/test_config_test.py                  (if the signers array changes its output)
```

**Why the list is longer than it looks.** `CompiledPolicy` going from one backend to
`Vec<PolicyBackend>` is a breaking type change, and every consumer above must compile. Four of
these files belong to loop **D** in wave 3 — that is a *cross-wave* overlap, which is sequential
and legal (see "Ownership is wave-scoped"). None of them is A's, so `A ∩ B = ∅` still holds. The
merge protocol's rule that a conflict is a decomposition bug depends on this list being complete:
an under-declared Owns list is exactly how a "conflict-free by construction" claim goes wrong.

**Acceptance criteria**

- `[[trust.policy]] signers = [...]` replaces `[trust.policy.keyless]` **outright** — serde-tagged
  on `kind`, deserializing straight into `PolicyBackend`; `CompiledPolicy` holds
  `Vec<PolicyBackend>`. No dual-form parsing.
- `PolicyBackend::Key` added; `scope` and `builder` stay policy-level siblings.
- **Empty `signers` is a configuration error, never a catch-all. Fail closed.**
- `key` XOR `key_pem`, mirroring `trusted_root` / `trusted_root_json`, with `ocx config push`
  inlining the path form at publish time.
- `--key` grammar `[scheme://]<rest>`: bare or `file:` resolves to a file; any other recognised
  scheme (`awskms://`, `gcpkms://`, `azurekms://`, `hashivault://`, `k8s://`) fails with the
  distinct unsupported-key-backend error and exit code, naming the scheme — never "no such file
  or directory".
- `KeyBackend` is `async`, fallible with a transport-class error, exposes `sign_prehash`, never
  private key material, and supplies its own public key / hint. File backend implemented; an
  exercised test double exists (ARCH-07's second implementation, and WP6's key axis runs on it).
- Verify-side key loading is SPKI PEM via `CosignVerificationKey::try_from_pem` — no decryption.
- Sign-side encrypted PEM is `SigStoreKeyPair::from_encrypted_pem`; **no scrypt or PEM envelope
  is owned in this repo**. `OCX_KEY_PASSWORD`, empty password allowed.
- Key *generation* is not implemented — documented in F as `cosign generate-key-pair`.

**Verification** — `task rust:verify`, `task schema` (the config schema changes), then
`cd test && uv run pytest tests/test_trust_policy_signers.py tests/test_config_test.py -v`.

**Red/green evidence required** — empty `signers` must be shown refusing, and shown *accepting*
when the guard is removed. For the unsupported backend, assert at the level B can actually reach:
`KeyRef::parse("awskms://…")` → `Unsupported`, and the `ExitCode → 85 → ErrorCategory` mapping,
both red/green as unit tests. No command accepts `--key` until wave 3 — G1 lands the option
structs unattached — so an end-to-end exit-code assertion here would be testing nothing. That
assertion belongs to C and D.

**Exit gate** — `task verify --force` green; schema regenerated and committed.

---

### C — sign side: DSSE, simplesigning write, key-mode signing

| | |
|---|---|
| **Covers** | WP3, WP5b, WP9b-sign |
| **Tier** | `high` |
| **Mode** | worktree `.agents/worktrees/cosign-c-sign` off `feat/cosign-parity` |
| **Model** | `opus` (wire format + crypto) |

**Owns**

```
crates/ocx_lib/src/oci/sign.rs
crates/ocx_lib/src/oci/sign/pipeline.rs
crates/ocx_lib/src/oci/sign/signer.rs
crates/ocx_lib/src/oci/sign/bundle.rs
crates/ocx_lib/src/oci/sign/rekor.rs
crates/ocx_lib/src/oci/sign/error.rs
crates/ocx_lib/src/oci/sign/simplesigning_write.rs   (new)
crates/ocx_lib/src/oci/attest/pipeline.rs
crates/ocx_lib/src/oci/attest/statement.rs
crates/ocx_lib/src/oci/attest/dsse.rs
crates/ocx_cli/src/command/package_sign.rs
crates/ocx_cli/src/command/package_attest.rs
crates/ocx_cli/src/command/package_push.rs
crates/ocx_cli/src/api/data/push.rs
crates/ocx_cli/src/api/data/signature.rs
crates/ocx_cli/src/api/data/attestation.rs
test/tests/test_sign.py
test/tests/test_attest.py
```

**Acceptance criteria**

- WP3: the image signature is a DSSE in-toto Statement — predicateType
  `https://sigstore.dev/cosign/sign/v1`, empty predicate, subject = the signed digest.
  Annotations become `dev.sigstore.bundle.content: dsse-envelope` +
  `dev.sigstore.bundle.predicateType`. `messageSignature` is **deleted**, write and read (D2).
- `--platform` optional on `sign`, `attest`, `push` (D-§Where signing happens): absent acts on the
  resolved object; present narrows into an index and errors when the resolved object is not one.
  The branch is on what resolution returns, never on the reference's form.
- `--platform` and `--tags-file` are mutually exclusive, with the reason in the error text.
- `push --sign` is the opt-in for inline platform-manifest signing; `push` does not sign without
  it, and `--signature-format` / `--key` / `--rekor-upload` are an error without it unless
  `--sbom` is given. `PushReport.platform_digests` is push's inline signing input.
- `--tags` / `--tags-file` sweep the **indices those tags resolve to** — the input is tag names
  from the tags file, not `platform_digests`. A swept tag resolving to a bare manifest is skipped
  with a warning; the sweep continues past a per-tag failure and exits non-zero at the end with
  every failure listed.
- WP5b: `--signature-format bundle|simplesigning|both`, default `bundle`. The simplesigning path
  signs the **claim bytes**, not the manifest digest; Rekor entry is a `hashedrekord` over the
  payload; re-signing **appends a layer** through the same D4 retry loop. `.att` and `.sbom`
  sidecar equivalents for `attest`, and `push --sbom FILE` honours `--signature-format` on the
  same path.
- `--signature-format both` is **best-effort per leg, never atomic**: the report lists each leg's
  outcome and the exit code is non-zero if any leg failed.
- WP9b-sign: `--key` selects key mode; keyless-only flags (`--fulcio-url`,
  `--identity-token-*`, `--no-tty`) are an **error** alongside `--key`, never ignored.
  Key mode hand-assembles `VerificationMaterial::Content::PublicKey` — `sigstore::bundle::sign`
  has no arm for it (see spec §WP9).
- Rekor-upload default: **keyless always uploads and `--no-rekor-upload` is an error**; key mode
  is off unless `--rekor-upload` or `[trust.sigstore] rekor_upload = true`. The result — human
  and `--format json` — **states whether a transparency record was created**.

**Verification** — `task rust:verify` then
`cd test && uv run pytest tests/test_sign.py tests/test_attest.py -v` (needs the `sigstore` and
registry compose profiles).

**Red/green evidence required** — assert on the emitted bundle's `content` discriminant and
predicateType, not on exit code; show the test red against a messageSignature payload. For
`--no-rekor-upload` under keyless, show the error and show it red when the guard is removed.

**Exit gate** — `task verify --force` green.

---

### D — verify side: DSSE, simplesigning read, key verification, SBOM listing

| | |
|---|---|
| **Covers** | WP4, WP5, WP9b-verify, the D1 membership check, the SBOM `shadowed` field |
| **Tier** | `high` |
| **Mode** | worktree `.agents/worktrees/cosign-d-verify` off `feat/cosign-parity` |
| **Model** | `opus` (trust gate — a verify bug is a silent accept) |

**Owns**

```
crates/ocx_lib/src/oci/verify.rs
crates/ocx_lib/src/oci/verify/pipeline.rs
crates/ocx_lib/src/oci/verify/dsse.rs
crates/ocx_lib/src/oci/verify/identity.rs
crates/ocx_lib/src/oci/verify/tlog.rs
crates/ocx_lib/src/oci/verify/error.rs
crates/ocx_lib/src/oci/verify/simplesigning_read.rs   (new)
crates/ocx_lib/src/sbom.rs
crates/ocx_lib/src/sbom/cyclonedx.rs
crates/ocx_lib/src/package_manager/tasks/sbom.rs        (where shadowing is actually decided)
crates/ocx_lib/src/package_manager/tasks/verify.rs
crates/ocx_lib/src/package_manager/tasks/auto_verify.rs
crates/ocx_cli/src/command/package_verify.rs
crates/ocx_cli/src/command/package_sbom.rs
crates/ocx_cli/src/api/data/verification.rs
crates/ocx_cli/src/api/data/sbom.rs
test/tests/test_verify.py
test/tests/test_offline_verify.py
test/tests/test_auto_verify.py
test/tests/test_sbom.py
test/tests/fixtures/simplesigning/**                  (new, committed bytes per D7)
```

**Acceptance criteria**

- WP4: cosign DSSE **image-signature** bundles verify, built against G0's golden fixtures under
  `test/tests/fixtures/golden/` — not against `spike_cosign_bundle.json`, which is a CycloneDX
  attestation and cannot exercise this path.
- WP5: `sha256-<hex>.sig` sidecars (layers = simplesigning payloads), the
  `application/vnd.dev.cosign.artifact.sig.v1+json` OCI-1.1 referrer, and the `.att` / `.sbom`
  equivalents. `critical.image.docker-manifest-digest` is checked against the subject.
- **D5 is the load-bearing one**: a `.sig` carries its verification material in annotations
  rather than a bundle blob, and that is a parsing difference, never a trust difference. Under a
  key there is no `certificate`, no `chain`, and — with `--no-rekor-upload` — no `bundle`.
  Their absence is a legal shape, not malformed input, and it must not weaken the identity gate.
- D9: prefer `bundle`, fall back to `simplesigning`, `--signature-format` pins.
- D6: all shapes merge into the existing `signatures[]` contract with `signature_format`,
  `discovery_method` and `key_backend` per element. Dedup on the Rekor log index **when present**,
  otherwise on (signature bytes, subject digest, `signature_format`) — key mode defaults to no
  upload, so the log index is absent exactly where double-discovery is most likely.
- D1 membership, precisely: fetch the index the reference resolved to and match the platform
  digest against its `manifests[]`. Fail **closed** in both cases where the check cannot run — a
  bare platform digest with no enclosing index, and an unfetchable index (offline or uncached) —
  by not considering the index signature at all.
- D5 under a key: the gate is public-key match against `--key` or a `kind = "key"` signers entry;
  keyless certificate matchers are an error alongside `--key`; a policy whose applicable signers
  are all `kind = "keyless"` **refuses** a key-signed artifact.
- The read-side 84/79 split from D3: no Referrers API and no fallback tag exits **79**, never 84
  (`verify/pipeline.rs:1397-1399,2025`, `verify/error.rs:195,662` — A cannot reach these).
- `--platform` optional on `verify`, same resolution rule, independent of the D1 check.
- WP9b-verify: `--key` public key, key pinning through `PolicyBackend::Key`; a key-signed
  artifact with no Rekor entry verifies, and the same absence is **refused** for a keyless one.
- SBOM: platform-level shadows index-level **only within the same predicateType**; a shadowed
  entry stays visible in `--format json` marked `shadowed`. `--summary` stays CycloneDX-only.

**Verification** — `task rust:verify` then
`cd test && uv run pytest tests/test_verify.py tests/test_offline_verify.py tests/test_auto_verify.py tests/test_sbom.py -v`.

**Red/green evidence required** — for every new accept path, a matching refuse path shown red
when the guard is removed. Specifically: a simplesigning payload whose
`docker-manifest-digest` does not match the subject must be refused; a keyless signature with no
Rekor entry must be refused; a valid-signature-over-malformed-payload case (the existing
`malformed_payload_valid_signature.json` fixture) must stay refused.

**Exit gate** — `task verify --force` green.

---

### E — interop matrix

| | |
|---|---|
| **Covers** | WP6 |
| **Tier** | `high` — this is the gate; a false green here invalidates the whole release |
| **Mode** | solo, `feat/cosign-parity` in the main worktree |
| **Model** | orchestrator `opus` (assertion design — what makes a cell honest); scaffolding workers `sonnet` |

**Owns**

```
ocx.toml                                        (cosign = "ocx.sh/sigstore/cosign:3")
ocx.lock
test/docker-compose.yml                         (only if a cell needs a service that is absent)
test/tests/test_cosign_interop.py               (extended — 5 tests exist today)
test/tests/fixtures/cosign.py                   (extended; image tag derived from the ocx.toml pin)
test/tests/fixtures/sigstore_stack.py
test/tests/fixtures/keys/**                     (new — committed cosign.pub + encrypted cosign.key)
taskfiles/**                                    (only if a new test target is needed)
```

**Acceptance criteria**

- The full 2×2×2×2 = **16 cells** (direction × format × key model × registry), each also
  exercised for `sign`, `attest` and SBOM where the shape differs. Plus: `--signature-format both`
  accepted by each consumer through its own preferred path; D9's fallback firing when only
  simplesigning is present; a key-signed `--no-rekor-upload` artifact verifying, and the same
  absence refused for keyless.
- Every cell asserts on **cosign's own output**, not merely its exit code. A green that never
  invoked cosign is indistinguishable from cosign never having run.
- The existing 5 blob-level tests stay and keep passing; the module docstring is rewritten (it
  currently states OCX has no tag-schema fallback and that discovery is out of scope — both
  become false).
- New image-level cells use `cosign verify <ref>` / `cosign verify-attestation`, not the blob
  commands, because registry discovery is the half the existing tests skip.
- A cell that cannot be produced because of an upstream cosign limitation is recorded as a
  documented gap for F, **never quietly dropped**.
- Key axis: one key pair generated once with `cosign generate-key-pair`, public key and encrypted
  private key committed, known `OCX_KEY_PASSWORD`. Deterministic and offline apart from the
  container.
- ~~The four "cosign signs → simplesigning" cells drive cosign with `--new-bundle-format=false`~~
  **VOID — amended by G0, 2026-08-29.** `--new-bundle-format` does not exist in cosign v3.1.1 on
  any subcommand (`sign`, `attest`, `verify`, `sign-blob`, `attach signature`,
  `download signature`, `triangulate` — all measured, zero hits). Asserting the flag "still
  exists" would ship a permanently red check. **cosign 3.x has no simplesigning writer on `sign`
  at all**, and `--registry-referrers-mode=legacy` does not restore one. Replacement route,
  validated in G0: `cosign generate` (the simplesigning claim) → `cosign sign-blob` →
  `cosign attach signature --payload … --signature …` (keyless additionally
  `--certificate` / `--certificate-chain`). Assert `attach signature` still exists — it is
  deprecated-with-warning in 3.1.1, not absent. **Known limitation G0 hit and could not work
  around:** `cosign attach signature --rekor-response` accepts its argument but never writes the
  `dev.sigstore.cosign/bundle` annotation, so a simplesigning artifact carries no offline
  transparency-log material. If a cell needs that, it is a documented gap.
- The two "cosign signs × bundle × Referrers-API-absent" cells run against cosign's broken
  fallback write ([#4641](https://github.com/sigstore/cosign/issues/4641), still open): assert on
  what cosign actually emits and record the annotation loss as an F gap. Do **not**
  weaken OCX's reader to accommodate it. **Corrected by G0, 2026-08-29:** measured against
  registry:2, cosign's fallback index **preserves `artifactType`** and drops all three
  annotations (`dev.sigstore.bundle.content`, `dev.sigstore.bundle.predicateType`,
  `org.opencontainers.image.created`). A cell asserting `artifactType` loss fails against real
  cosign output. Committed evidence: `test/tests/fixtures/golden/fallback_index.json`.
- The C/D 84-versus-79 exit-code split proved end to end — neither loop can prove it alone.

**Verification** —
`cd test && uv run pytest tests/test_cosign_interop.py -v` (needs registry, `mirror-registry` and
`sigstore` compose profiles, plus docker for the cosign container).

**Red/green evidence required** — the whole loop's reason to exist. For at least one cell per
axis, corrupt the artifact and show cosign refusing it; a cell that passes on a corrupted input
is a cell that is not testing anything. Prove the mutation landed before trusting the result.

**Exit gate** — `task verify --force` green; all 16 cells present and named, **each with its
`sign`, `attest` and SBOM-attach variants or an explicit note that the shape does not differ**
("16 cells present" is otherwise satisfied by 16 sign-only cells); gaps listed for F.

---

### F — parity documentation and casts

| | |
|---|---|
| **Covers** | WP8 |
| **Tier** | `medium` |
| **Mode** | solo, `feat/cosign-parity` in the main worktree |
| **Model** | `sonnet` (docs); reviewer `opus` (the parity claim is a security claim) |

**Owns**

```
website/src/docs/in-depth/signing.md
website/src/docs/in-depth/cosign-parity.md      (new, or a signing.md section)
website/src/docs/in-depth/self-hosted-sigstore.md
website/src/docs/reference/command-line.md
website/src/docs/reference/environment.md       (OCX_KEY_PASSWORD)
website/src/docs/in-depth/configuration.md      (signers array, rekor_upload)
website/recordings.taskfile.yml
website/recordings/**
website/src/public/casts/in-depth/*.cast
.claude/rules/product-context.md                (differentiator #12 — keyless is no longer the only key model)
```

**Acceptance criteria**

- A parity page stating exactly which cosign commands verify which OCX artifacts and vice versa,
  per format, per key model.
- Every WP6 gap from E documented as a gap.
- **"Adding a signer always *widens* acceptance, never narrows it"** stated loudly — most readers
  hear "add a key policy" as tightening.
- The deliberate `--rekor-upload` divergence from cosign documented, so it is discoverable rather
  than surprising.
- `cosign generate-key-pair` documented end to end, through to the `--key` / `signers` wiring.
- The corrected overclaim from `deferrals_107_197.md` removed — `package sign` may claim cosign
  interop again, now that E is green.
- Casts execute a bare `cosign …` against a real registry, no `ocx run --`, no
  `ocx package exec`. **No migration prose** — pre-1.0 breaks just break.

**Verification** — `task recordings:build`, `task claude:lint:links`,
`cd website && bun run build`.

**Exit gate** — `task verify --force` green; casts regenerated **in the same commit** as the
command changes they display (see `project_doc_cast_two_tree_drift.md`).

---

## File-disjointness proof

Only waves 2 and 3 run concurrent loops. Every other wave is solo and needs no proof.

### Wave 2 — A ∩ B

| A owns | B owns |
|---|---|
| `oci/client/{native_transport,transport,test_transport,error}.rs` | `trust.rs`, `config.rs`, `config/loader.rs`, `project/config.rs`, `app/context.rs` |
| `oci/referrer/{capability,manifest}.rs` | `oci/sign/{key_backend,key_ref}.rs` |
| `oci/client.rs`, `oci/copy.rs` | `oci/verify/{trust_resolve,trust_cache,identity,dsse}.rs` |
| `package/tag.rs` | `managed_config/**`, `crates/ocx_schema/**` |
| `external/rust-oci-client/**` | `package_manager/tasks/{verify,auto_verify,sbom}.rs`, `command/package_sbom.rs` |
| `test/tests/test_referrers_{capability,smoke,fallback}.py` | `test/tests/test_trust_policy_signers.py`, `test_config_test.py` |

**A ∩ B = ∅.** A is the OCI transport and tag classifier; B is trust config, key material, and
every consumer that a `Vec<PolicyBackend>` breaks. They meet only through types G1 already froze.
B's column deliberately reaches into `oci/verify/**` and `package_manager/tasks/**`, which loop D
owns in **wave 3** — cross-wave, therefore sequential, therefore legal. A declares none of them.

### Wave 3 — C ∩ D

| C owns | D owns |
|---|---|
| `oci/sign/**`, `oci/attest/{pipeline,statement,dsse}.rs` | `oci/verify/**`, `sbom.rs`, `sbom/cyclonedx.rs` |
| `command/{package_sign,package_attest,package_push}.rs` | `command/{package_verify,package_sbom}.rs` |
| `api/data/{push,signature,attestation}.rs` | `api/data/{verification,sbom}.rs` |
| — | `package_manager/tasks/{sbom,verify,auto_verify}.rs` |
| `test/tests/{test_sign,test_attest}.py` | `test/tests/{test_verify,test_offline_verify,test_auto_verify,test_sbom}.py` |
| — | `test/tests/fixtures/simplesigning/**` |

**C ∩ D = ∅**, but only because G1 took five files out of contention first — this is the specific
reason G1 exists as its own wave, and each of the five was a live collision before it did:

| File | Why both wanted it |
|---|---|
| `oci/referrer/media_types.rs` | every new media type and annotation constant |
| `oci/sign/format.rs` | the `SignatureFormat` enum, selected on write, pinned on read |
| `oci/resolve_target.rs` | the `--platform` optionality rule, identical on sign and verify |
| `oci/attest.rs` | `DSSE_PAYLOAD_TYPE`, `ACCEPTED_STATEMENT_TYPES`, `COSIGN_SIGN_PREDICATE_TYPE` — **the sibling module file, which the glob `oci/attest/**` does not match** |
| `command/package_sign_common.rs` | imported by all three of C's command files *and* by `package_verify.rs`; the only home of `resolve_policies` and the sigstore/offline/token resolution |

All five are G1-owned and **read-only in wave 3**. `oci/attest.rs` and `package_sign_common.rs`
are the two that a glob-based reading of the split would have missed entirely.

No shared module-declaration file forces a second editor: all five `api/data/*.rs` files already
exist, `command.rs` already declares `package_sign_common`, C's new `simplesigning_write.rs`
declares in C-owned `oci/sign.rs`, D's `simplesigning_read.rs` in D-owned `oci/verify.rs`, and
`oci.rs` / `lib.rs` are touched only by G1 in a solo wave. `push` does not call verify —
`package_push.rs`'s `verify_dependency_pins` is lock-pin checking, and `--sbom` routes entirely
C-side through `attest_sbom`.

> Trap avoided: the team-lead draft paired WP9 with WP5+WP5b in one wave. Those collide on
> `sign/pipeline.rs` **and** `verify/pipeline.rs`. Splitting WP9 into a pipeline-free half (B)
> and a pipeline-wiring half (WP9b, absorbed into C and D) is what makes both waves disjoint
> without serializing anything.

---

## Shared-contract set (G1, wave 1)

Everything a wave-2 or wave-3 loop needs to compile against. **Owner is always G1**; the
Consumers column is read-only for those loops.

| Shape | File | Form | Consumers |
|---|---|---|---|
| `SignatureFormat { Bundle, Simplesigning, Both }` | `oci/sign/format.rs` (new) | full enum + serde + `FromStr` | C, D, E |
| `DiscoveryMethod { ReferrersApi, FallbackTag, SidecarTag }` | `oci/verify/discovery.rs` (new) | full enum + serde | A, D |
| `KeyBackend` trait — `async fn sign_prehash(&self, digest) -> Result<Signature, …>`, `fn public_key(&self)`, `fn hint(&self)` | `oci/sign/key_backend.rs` (new) | trait + `unimplemented!()` file impl | B (implements), C |
| `KeyRef` — `[scheme://]<rest>` parse, `Scheme { File, Aws, Gcp, Azure, HashiVault, K8s }` | `oci/sign/key_ref.rs` (new) | full parser + `Unsupported(scheme)` error | B, C, D |
| `ExitCode::UnsupportedKeyBackend` | `cli/exit_code.rs` | full, with its value test | B, C, D |
| simplesigning claim struct + `critical`/`optional` serde | `oci/simplesigning.rs` (new) | **fully implemented** — a pure data shape, cheaper than stubbing | C (write), D (read) |
| All new media-type / annotation constants | `oci/referrer/media_types.rs` | full consts | A, C, D, E |
| `resolve_sign_target(reference, platform) -> Resolved` — the `--platform` optionality rule | `oci/resolve_target.rs` (new) | **fully implemented** | C, D |
| `signers` serde-tagged enum (`#[serde(tag = "kind")]` → `PolicyBackend`) | `trust.rs` | type + `Deserialize` only, no evaluation | B (implements evaluation), D |
| `PushReport.platform_digests` | `api/data/push.rs` | **fully implemented** — ~20 lines, the digests are already in hand | C |
| `signatures[].signature_format` / `.discovery_method` / `.key_backend` | `api/data/verification.rs` | fields declared, populated by D | D |
| `sbom[].shadowed` | `api/data/sbom.rs` | field declared | D |
| Option structs: `SignatureFormatOpt`, `KeyOpt`, `RekorUploadOpt` (paired boolean, `overrides_with`), `TagsOpt` (`--tags` repeatable + comma-delimited, `--tags-file`) | `crates/ocx_cli/src/options/{signature_format,key,rekor_upload,tags}.rs` (new) + `options.rs` | full clap structs, not yet attached to commands | C, D |
| `COSIGN_SIGN_PREDICATE_TYPE` + the matching `PredicateType` arm | `oci/attest.rs`, `oci/attest/predicate.rs` | full consts + arm | C (writes), D (matches) |
| `--key` → `CompiledPolicy`, the key-vs-keyless flag rejection, and `--rekor-upload` resolution | `crates/ocx_cli/src/command/package_sign_common.rs` | **fully implemented** seams; C and D become callers only | C, D |
| `ExitCode::UnsupportedKeyBackend = 85` **plus its own `error_envelope.rs` arm** | `cli/exit_code.rs`, `crates/ocx_cli/src/error_envelope.rs` | full, with a test that reds without the arm | B, C, D |

The four new option structs are the whole CLI cross-cut, and they are answered here rather than
distributed: **G1 owns every options file; each wave-3 loop attaches them to the command files it
alone owns.** No two loops edit one options file, and no two loops edit one command file.

**`--platform` is the exception, and deliberately so.** `options/platform.rs:27` is already
`Option<oci::Platform>` with no `required`; sign, verify and attest do **not** flatten it — each
declares its own inline arg with `required = true` (`package_sign.rs:32`, `package_verify.rs:34`,
`package_attest.rs:55`). Editing `options/platform.rs` would change nothing for those three and
would touch the 18 unrelated commands that *do* flatten it. So the three `required = true`
removals stay with C (sign, attest) and D (verify), in command files each already owns alone —
disjoint as-is.

---


### Managed-config key material — decided 2026-08-29, loop B inherits

**A managed-config payload accepts `key_pem` only.** The path form
`key = "file:…"` is **rejected there**, with an error naming `key_pem` as the fix.

Not a new security control — the convention the spec already set for the keyless side,
applied consistently. `SigstoreTrust` solved exactly this: `trusted_root` (path) XOR
`trusted_root_json` (verbatim), with `managed_config::publish::inline_trusted_root` reading
the path form at publish time and inlining it, *"because a path on the operator's disk means
nothing on a consumer's"*. A managed payload is a `config.toml` shipped as a package to a
fleet, so a `file:` reference in one is already meaningless on every consumer — it names the
operator's disk. Rejecting it **removes an incoherent state rather than adding a guard**, and
traversal containment stops being a question because no config-sourced path is ever resolved
on a consumer.

| Rule | Tier | Owner |
|---|---|---|
| `key_pem` only; `key = "file:…"` refused, error names `key_pem` | managed payload | **B** — `managed_config/publish.rs`, beside the `AmbiguousTrustRoot` twin |
| `file:` unrestricted; a relative ref resolves against the directory of the config file that declared it | project / operator / user | **B** — ordinary resolution semantics, **not** a containment check and never to be described as one |
| `key` XOR `key_pem` a hard error | **every** tier | **G1 — shipped**, `trust::validate_signers` |
| `ocx config push` inlines the path form at publish time | managed payload | **B** — same seam as `inline_trusted_root` |

**Why G1 froze the rule but did not implement it:** the refusal has no field to scan.
`TrustPolicy` carries no `signers` field until loop B attaches it (adjudication D-4 keeps that
breaking cascade in B's wave), so a publish-time scan over `trust.policy[].signers[]` cannot be
written yet. G1 encodes the rule on `KeyMatcher::key`'s doc comment — the text loop B reads at
the point of use — and freezes it here. **Do not build a sanitizer**; the two rows above are the
whole scope.


## Merge protocol between waves

The meta-orchestrator, not the loop, merges. Per wave with concurrent loops:

1. Checkpoint each worktree before any review runs — reviewers have destroyed uncommitted work
   in this repo before.
2. Merge in loop-id order (A then B; C then D) onto `feat/cosign-parity` in the main worktree.
   File-disjointness means these are content-conflict-free by construction; a conflict is a
   decomposition bug and gets reported, not resolved by hand.
3. `task verify --force` on `feat/cosign-parity` after **each** merge, not only after the last.
4. `git worktree remove --force <dir>` then `git branch -D <branch>` in the same turn the merge
   lands. Whoever creates a worktree removes it.
5. Audit refs after any committing subagent — self-reports lie.

## Model policy (CLAUDE.md §MODEL POLICY — propagated)

`model` is set explicitly on **every** spawn, with a one-line `Model rationale:` in the prompt.
Never Fable, at any depth.

| Role | Model |
|---|---|
| Loop sub-orchestrators (all seven) | `opus` |
| Implementation in A, B, C, D | `opus` — wire format, crypto, error/exit-code semantics, trust boundary |
| Implementation in G0 | `sonnet` — mechanical rename against a decided shape |
| Implementation in F | `sonnet` — docs |
| Test scaffolding in E | `sonnet`; assertion design stays with E's `opus` orchestrator |
| Every review, security and adversarial pass | `opus` — never downgraded to save cost |
| Exploration, research, web fetch, censuses | `sonnet` |
| Cross-model gate | `terra` default; `sol` on E and on the C/D merge only |

## Standing constraints

- **Never edit `CHANGELOG.md`.** The changelog entry is the commit subject.
- Pre-1.0: interfaces break outright. No compat shims, no dual-form parsing, no deprecation
  windows — the single exception is WP7's frozen legacy `sha256.<hex>` **read** arm.
- `task verify` is never piped and always gets `--force`.
- Plans go in `.claude/state/plans/` with a `## Status` block per
  `.claude/rules/meta-ai-config.md` §Plan Status Protocol; artifacts stay in `.claude/artifacts/`.
- Never push. The owner decides when a branch goes to the remote.
- Use `grep -e` per needle in every gate — the `rg` binary here false-negatives on alternation.

---

## Frozen contracts (G1)

Appended 2026-08-29, after G1 landed. **This section records what the tree contains, not what
the table above promised.** Where the two differ, the code is the contract and the divergence is
listed in §Divergences below. Every signature here was copied from the file named beside it.

**Ownership.** G1 owns all of it. Every row is **read-only** for loops A–E unless the row says
otherwise. A loop that needs a shape changed reports it rather than changing it — two loops
editing one G1 file is the decomposition bug wave 1 exists to prevent.

### Commits, in order

| SHA | Subject | Surface |
|---|---|---|
| `ffa19b0d` | refactor(cli)!: classify exit codes in a match the compiler keeps total | `cli/error_category.rs` (new), `cli.rs`, `error_envelope.rs` |
| `f94e9a7d` | feat(cli): exit 85 when a --key names a key backend OCX cannot use | `cli/exit_code.rs`, `cli/error_category.rs` |
| `dc8238a3` | feat(sign): read cosign `--key` references and name an unimplemented backend | `oci/sign/key_ref.rs`, `oci/sign/key_backend.rs` (both new), `oci/sign.rs` |
| `e4229341` | feat(sign): freeze the cosign simplesigning claim and sidecar wire vocabulary | `oci/simplesigning.rs`, `oci/sign/format.rs`, `oci/verify/discovery.rs` (new); `oci/referrer/media_types.rs`, `oci/attest.rs` |
| `26c9c249` | feat(sign): decide --platform on what the reference resolved to, not on its form | `oci/resolve_target.rs` (new), `oci.rs` |
| `c53ccec2` | feat(push): report the per-platform manifest digests a push landed on | `publisher.rs`, `package/cascade.rs`, `api/data/push.rs`, `command/package_push.rs`, `test_package_push.py` |
| `2e43ee25` | feat(sign): exit 85 with its own error kind when --key names a backend OCX cannot use | `oci/sign/error.rs`, `oci/verify/error.rs` |
| `a5c5b7d7` | feat(trust): refuse an empty `signers` list instead of reading it as trust-anyone | `trust.rs` |
| `2b7e7b91` | feat(cli): report each discovered signature and whether an SBOM is shadowed | `api/data/verification.rs`, `api/data/sbom.rs`, `command/package_sbom.rs` |
| `9b1a46f1` | chore(cli): freeze the four cosign-parity option groups before anything attaches them | `options.rs`, `options/{key,rekor_upload,signature_format,tags}.rs` (new) |
| `297ea602` | feat(verify): let a `--key` reference resolve the trust policy instead of a keyless matcher | `command/package_sign_common.rs`, `command/package_verify.rs`, `command/package_sbom.rs`, `error_envelope.rs` |

### Exit codes and error classification

`ErrorCategory` **moved out of the CLI crate**: it is now `ocx_lib::cli::error_category`, re-exported
as `ocx_lib::cli::ErrorCategory`. `crates/ocx_cli/src/error_envelope.rs` imports it and owns no
classification table any more.

| Item | Path | Form | Consumers |
|---|---|---|---|
| `ExitCode::UnsupportedKeyBackend = 85` | `crates/ocx_lib/src/cli/exit_code.rs:87` | variant on the `#[non_exhaustive]` enum | B, C, D, E — read-only |
| `ErrorCategory::UnsupportedKeyBackend` | `crates/ocx_lib/src/cli/error_category.rs:35` | serializes `"unsupported_key_backend"` | B, C, D — read-only |
| `ErrorCategory::from_exit_code(code: ExitCode) -> Self` | `crates/ocx_lib/src/cli/error_category.rs:55` | **exhaustive, wildcard-free, in-crate** | all — read-only |

**The compiler is the gate.** `ExitCode` is `#[non_exhaustive]`, which binds *downstream* crates
only; `from_exit_code` lives in the crate that defines `ExitCode`, so its match is exhaustive with
no `_` arm. Adding an `ExitCode` variant without an `ErrorCategory` arm is an **`E0004` compile
error**, not the silent `internal` the former cross-crate form produced. **Any loop adding an exit
code adds both**, in the same commit, plus a row in `error_category_total_over_exit_codes`
(currently `assert_eq!(cases.len(), 16, …)`).

`ExitCode::PolicyBlocked` and `ExitCode::DirtyRcBlock` both classify to `PermissionDenied`;
`Success` and `Failure` classify to `Internal` as a fail-safe.

Two error kinds reach 85, one per taxonomy, with **byte-identical `kind_detail` slugs** so a script
reads one word for one failure:

| Variant | Path | `exit_code()` | `kind_detail()` |
|---|---|---|---|
| `SignErrorKind::UnsupportedKeyBackend(KeyRefError)` | `crates/ocx_lib/src/oci/sign/error.rs:272` | `ExitCode::UnsupportedKeyBackend` | `"unsupported_key_backend"` |
| `SignErrorKind::KeyReferenceInvalid(KeyRefError)` | `crates/ocx_lib/src/oci/sign/error.rs:282` | `ExitCode::UsageError` | `"key_reference_invalid"` |
| `SignErrorKind::RekorUploadRequiredForKeyless` | `crates/ocx_lib/src/oci/sign/error.rs:309` | `ExitCode::UsageError` | `"rekor_upload_required_for_keyless"` |
| `VerifyErrorKind::UnsupportedKeyBackend(KeyRefError)` | `crates/ocx_lib/src/oci/verify/error.rs:529` | `ExitCode::UnsupportedKeyBackend` | `"unsupported_key_backend"` |
| `VerifyErrorKind::KeyReferenceInvalid(KeyRefError)` | `crates/ocx_lib/src/oci/verify/error.rs:539` | `ExitCode::UsageError` | `"key_reference_invalid"` |

All five are `#[error(transparent)]` — the wrapped `KeyRefError`'s `Display` names the scheme.
Routing is decided once per side, by `impl From<KeyRefError>` (`sign/error.rs:387`,
`verify/error.rs:769`): `UnsupportedBackend` → 85, `UnknownScheme | Empty` → 64.

**The two slug tables are not compiler-forced.** `kind_detail()`'s match is exhaustive, so a new
variant forces a new *arm*; nothing forces a new *row* in the frozen-slug table, because
`pairs.len()` is a compile-time constant. Current counts, to be bumped **by hand** alongside any
new variant:

- `sign/error.rs::tests::kind_detail_values_are_stable` — `assert_eq!(pairs.len(), 21, …)`
- `verify/error.rs::tests::kind_detail_values_are_stable` — `assert_eq!(pairs.len(), 44, …)`

### Key-reference grammar and signing primitive

`crates/ocx_lib/src/oci/sign/key_ref.rs`, re-exported from `oci::sign` as
`{KeyBackendKind, KeyRef, KeyRefError, Scheme}`.

```rust
pub enum Scheme { File, AwsKms, GcpKms, AzureKms, HashiVault, Kubernetes }  // serde: k8s for Kubernetes
impl Scheme {
    pub const SPELLINGS: &'static [&'static str] = &["file", "awskms", "gcpkms", "azurekms", "hashivault", "k8s"];
    pub const fn as_str(self) -> &'static str;
    pub const fn is_implemented(self) -> bool;   // File only
    pub fn parse(token: &str) -> Option<Self>;
}

pub struct KeyRef { /* private */ }
impl KeyRef {
    pub fn parse(value: &str) -> Result<Self, KeyRefError>;
    pub fn scheme(&self) -> Scheme;
    pub fn rest(&self) -> &str;
    pub fn as_path(&self) -> Option<&Path>;      // Scheme::File only
}

#[non_exhaustive]
pub enum KeyRefError {
    UnsupportedBackend { scheme: Scheme },       // exit 85
    UnknownScheme { scheme: String },            // exit 64
    Empty,                                       // exit 64
}

pub enum KeyBackendKind { Keyless, File, AwsKms, GcpKms, AzureKms, HashiVault, Kubernetes }
impl From<Scheme> for KeyBackendKind;
```

Grammar, in evaluation order (module doc): `://` splits on the **first** occurrence; otherwise a
`file:` prefix is stripped; otherwise the whole value is a bare path. Keying on `://` and never on
a bare `:` is what keeps `C:\keys\cosign.pub` a path rather than a scheme. `Scheme` can never be
`Keyless`; `KeyBackendKind` must be, which is why they are two vocabularies with one bridge.

`crates/ocx_lib/src/oci/sign/key_backend.rs`, re-exported as `{KeyBackend, KeyBackendError, public_key_hint}`:

```rust
#[async_trait::async_trait]
pub trait KeyBackend: Send + Sync {
    async fn sign_prehash(&self, digest: &[u8]) -> Result<Vec<u8>, KeyBackendError>;  // DER signature
    fn public_key_der(&self) -> &[u8];                                                // SPKI DER
    fn kind(&self) -> KeyBackendKind;
    fn hint(&self) -> String { public_key_hint(self.public_key_der()) }               // defaulted, do not override
}

pub fn public_key_hint(spki_der: &[u8]) -> String;   // BASE64-with-padding(SHA256(spki_der))

#[non_exhaustive]
pub enum KeyBackendError {
    Unavailable { reason: String },   // exit 75
    Io(std::io::Error),               // exit 74
    MalformedKey { reason: String },  // exit 65
    Unsupported { scheme: Scheme },   // exit 85
}
```

`hint()` is **defaulted deliberately** — the derivation is wire-visible and cosign matches on it,
so no backend may compute it differently. URL-safe alphabet or dropped padding produce a hint no
cosign verifier recognises. Pinned by `key_backend.rs::tests::public_key_hint_matches_cosign`
against the golden key bundle's `verificationMaterial.publicKey.hint`.

**Loop B owns the implementors.** G1 shipped the trait alone — there is no file-backend `impl` in
the tree.

### Wire vocabularies

| Item | Path | Value / form | Consumers |
|---|---|---|---|
| `SignatureFormat { Bundle (default), Simplesigning, Both }` | `oci/sign/format.rs` | serde `snake_case` **and** hand-written `clap_builder::ValueEnum`; slugs `bundle` / `simplesigning` / `both`; `ALL`, `as_str()`, `Display` | C, D, E — read-only |
| `DiscoveryMethod { ReferrersApi, FallbackTag, SidecarTag }` | `oci/verify/discovery.rs` | serde `snake_case`; slugs `referrers_api` / `fallback_tag` / `sidecar_tag`; `ALL`, `as_str()`, `Display` | A, D — read-only |
| `SIMPLESIGNING_MEDIA_TYPE` | `oci/referrer/media_types.rs` | `"application/vnd.dev.cosign.simplesigning.v1+json"` | C, D, E |
| `COSIGN_SIG_ARTIFACT_TYPE` | `oci/referrer/media_types.rs` | `"application/vnd.dev.cosign.artifact.sig.v1+json"` | C, D, E |
| `COSIGN_SBOM_ARTIFACT_TYPE` | `oci/referrer/media_types.rs` | `"application/vnd.dev.cosign.artifact.sbom.v1+json"` | C, D, E |
| `DSSE_ENVELOPE_MEDIA_TYPE` | `oci/referrer/media_types.rs` | `"application/vnd.dsse.envelope.v1+json"` | C, D |
| `ANNOTATION_COSIGN_SIGNATURE` | `oci/referrer/media_types.rs` | `"dev.cosignproject.cosign/signature"` | C, D, E |
| `ANNOTATION_COSIGN_CERTIFICATE` | `oci/referrer/media_types.rs` | `"dev.sigstore.cosign/certificate"` | C, D, E |
| `ANNOTATION_COSIGN_CHAIN` | `oci/referrer/media_types.rs` | `"dev.sigstore.cosign/chain"` | C, D, E |
| `ANNOTATION_COSIGN_BUNDLE` | `oci/referrer/media_types.rs` | `"dev.sigstore.cosign/bundle"` | C, D, E |
| `COSIGN_SIGN_PREDICATE_TYPE` | `oci/attest.rs:113` | `"https://sigstore.dev/cosign/sign/v1"` | C (writes), D (matches) |
| `SIMPLESIGNING_CLAIM_TYPE` | `oci/simplesigning.rs:14` | `"cosign container image signature"` | C, D |

**The two annotation namespaces genuinely differ.** `dev.cosignproject.cosign/signature` versus
`dev.sigstore.cosign/certificate` (and `/chain`, `/bundle`). That is cosign's real wire shape,
measured in the golden fixtures and in the v3.1.1 binary's string table — **not a typo**. Unifying
them breaks interop with every signature cosign ever wrote, and
`simplesigning.rs::tests::cosign_annotation_keys_match_the_golden_manifests` asserts
`assert_ne!` on the two namespaces precisely to catch that mutation.

### The simplesigning claim — byte-exact

`crates/ocx_lib/src/oci/simplesigning.rs` (module `oci::simplesigning`; **no `pub use` at `oci.rs`** —
the module path is the spelling).

```rust
pub struct SimpleSigningClaim { pub critical: Critical, pub optional: Option<serde_json::Value> }
pub struct Critical { pub identity: Identity, pub image: Image, #[serde(rename = "type")] pub claim_type: String }
pub struct Identity { #[serde(rename = "docker-reference")] pub docker_reference: String }
pub struct Image { #[serde(rename = "docker-manifest-digest")] pub docker_manifest_digest: String }

impl SimpleSigningClaim {
    pub fn new(docker_reference: impl Into<String>, subject: &crate::oci::Digest) -> Self;
    pub fn to_signing_bytes(&self) -> Result<Vec<u8>, serde_json::Error>;  // compact JSON, no trailing newline
}
```

**Field order is wire order** — `serde_json` emits declaration order, and this file relies on it:
`critical` → (`identity`, `image`, `type`) → `optional`. `optional` is emitted as an explicit
`null` and **never omitted**; cosign writes the key unconditionally.

> A `skip_serializing_if` on `optional`, or any field reorder, changes the serialized bytes,
> therefore the SHA-256, therefore the layer's registry address. It is a silent wire break.

Proven, not asserted: `simplesigning_claim_bytes_hash_to_the_pushed_layer_digest` hashes the
constructed claim and compares against `layers[0].digest` and `layers[0].size` of the `.sig`
manifests **cosign pushed** (`test/tests/fixtures/golden/simplesigning_{key,keyless}_manifest.json`),
key and keyless. Siblings: `simplesigning_claim_bytes_match_the_golden_payload`,
`optional_is_emitted_as_explicit_null`, `a_populated_optional_serializes_as_an_object`.

**Trust boundary, from the type's own doc:** verification checks the signature over the **raw layer
bytes as served**. Never re-serialize a parsed claim to reconstruct the signed payload — a round
trip is not guaranteed byte-identical, and a reconstruction that differs is a silent verification
bypass. (The round trip is proven in test so production code never has to rely on it.)

### The `--platform` decision seam

`crates/ocx_lib/src/oci/resolve_target.rs` (module `oci::resolve_target`; no `pub use` at `oci.rs`).

```rust
pub struct SignTarget { pub subject_digest: Digest, pub enclosing_index: Option<Digest> }

pub fn resolve_sign_target(
    resolved_digest: &Digest,
    children: Option<&[(Platform, Digest)]>,
    platform: Option<&Platform>,
) -> Result<SignTarget, ResolveTargetError>;

#[non_exhaustive]
pub enum ResolveTargetError { NotAnIndex { platform: String }, PlatformNotFound { platform: String }, AmbiguousPlatform { platform: String } }
```

Rule: `platform` absent → act on the resolved object as-is; present → narrow into the index;
present but the resolved object is not an index → `NotAnIndex`. **The branch is on what resolution
returned, never on the reference's form** — the reference is deliberately not a parameter, so no
caller can reintroduce the guess. Selection reuses `select_best`, the one shared D1 matcher.

The candidate shape is `(Platform, Digest)` — **`PushOutcome::platform_digests` is
`Vec<(oci::Platform, oci::Digest)>` for exactly this reason**, so a push outcome feeds
`resolve_sign_target` with no shim. `ResolveTargetError` is local to the module by design; C and D
each wrap it into their own error kind.

**This is a pure decision, not a pipeline.** No registry, no index, no I/O, and it reads no clock.
The I/O sequence (SSRF guard, index select, physical rewrite, dial guard, transport reference,
sign's write reference) stays inline in each pipeline; **loops C and D wire their own call sites.**
Only tests call it today.

### Push reporting

| Item | Path | Signature | Consumers |
|---|---|---|---|
| `PushOutcome.platform_digests` | `crates/ocx_lib/src/publisher.rs:84` | `pub platform_digests: Vec<(oci::Platform, oci::Digest)>` | C — read-only |
| `PushOutcome::new` | `crates/ocx_lib/src/publisher.rs:96` | `(manifest_digest, cascade_tags, keep_tags, platform_digests, layer_counts)` — **5 params, changed** | C |
| `CascadePushOutcome` | `crates/ocx_lib/src/package/cascade.rs:257` | `{ index_digest, cascade_tags, keep_tag: Option<String>, platform_digest: Option<Digest>, layer_counts }` | C |
| `push_with_cascade` | `crates/ocx_lib/src/package/cascade.rs:289` | returns `Result<CascadePushOutcome>` — **was a 4-tuple** | C |
| `PushReport.platform_digests` | `crates/ocx_cli/src/api/data/push.rs` | `#[serde(skip_serializing_if = "BTreeMap::is_empty")] pub platform_digests: BTreeMap<String, String>` | C |

`platform_digests` names the **platform manifest**, never the index — `manifest_digest` already
reports the index, which is rewritten by every platform merge and therefore cannot be what a
signature covers. It is **independent of keep tagging**: a `--no-keep-tag` push reports an empty
`keep_tags_written` and a fully populated `platform_digests`, read from the same merged-index
descriptor `push_keep_tag` reads so the two can never disagree. A platform the merged index did not
carry is **omitted, never faked**. Pinned by `push_report_json_shape_carries_per_platform_manifest_digests`,
`push_report_omits_platform_digests_when_none_were_produced`, `platform_digests_survive_no_keep_tag`.

**Both breaks are source-compatible for `ocx-mirror`**: its only reference is a comment in
`src/pipeline/push.rs:53` ("discards its `PushOutcome` deliberately") — it calls
`Publisher::push_cascade` and constructs neither `PushOutcome` nor `CascadePushOutcome`, and never
calls `push_with_cascade`. Verified against the working copy at `/home/mherwig/dev/ocx-mirror`.

### JSON report fields

| Field | Path | Form | Populated by |
|---|---|---|---|
| `SignatureEntry` | `crates/ocx_cli/src/api/data/verification.rs` | `{ signature_format, discovery_method, key_backend, referrer_digest, certificate_identity?, certificate_oidc_issuer?, signed_at?, rekor_log_index? }` | D |
| `VerificationReport.signatures` | same | `#[serde(skip_serializing_if = "Vec::is_empty")] pub signatures: Vec<SignatureEntry>` | D |
| `SbomEntry.shadowed` | `crates/ocx_cli/src/api/data/sbom.rs` | `pub shadowed: bool` — **emitted unconditionally** | D |

The asymmetry is deliberate and both halves are pinned: an always-empty `signatures: []` would
claim a discovery pass that never ran, so it is **absent** while empty; `shadowed: false` is a
*true* statement (nothing supersedes this document), so it is always present. `SbomEntry`'s
field sits between `verified` and `subject_digest` in the byte-exact JSON assertion in
`api/data/sbom.rs`. Test: `sbom_entry_json_shape_always_carries_shadowed`.

The three enum-valued fields reuse the library vocabularies verbatim (`SignatureFormat`,
`DiscoveryMethod`, `KeyBackendKind`), so their frozen slugs are the wire spelling and one word
cannot mean two things across the two crates.

### CLI option groups — `pub mod`, and no `pub use`

`crates/ocx_cli/src/options.rs` gained exactly four `pub mod` lines. **The module path IS the frozen
import spelling:**

```rust
use crate::options::key::KeyOpt;
use crate::options::signature_format::SignatureFormatOpt;
use crate::options::rekor_upload::RekorUploadOpt;
use crate::options::tags::TagsOpt;
```

**No loop may add a `pub use` to `options.rs`.** Shortening the path would put both wave-3 loops
back into that one file, and two loops editing one options file is the collision this layout
exists to prevent. Reaching the types through the module path means neither C nor D edits
`options.rs` at all.

| Struct | Flags / arg ids | Resolvers |
|---|---|---|
| `KeyOpt` | `--key REF`, arg id **`key`** | `reference() -> Result<Option<KeyRef>, KeyRefError>`, `is_key_mode() -> bool` |
| `SignatureFormatOpt` | `--signature-format FORMAT` (`value_enum`), arg id `signature_format` | `write_format() -> SignatureFormat` (defaults `Bundle`), `pin() -> Result<Option<SignatureFormat>, SignatureFormatPinError>` (`both` is an error) |
| `RekorUploadOpt` | `--rekor-upload` / `--no-rekor-upload`, mutual `overrides_with`, arg ids `rekor_upload`, `no_rekor_upload` | `enabled(key_mode: bool, configured: Option<bool>) -> Result<bool, SignErrorKind>` |
| `TagsOpt` | `--tags TAG` (repeatable, `value_delimiter = ','`), `--tags-file PATH`, arg ids `tags`, `tags_file` | `is_sweep() -> bool`, `async resolve() -> anyhow::Result<Vec<String>>` |

Notes a wave-3 loop needs:

- **Never read the fields directly** — all four are private; the resolvers are the contract.
- `KeyOpt`'s arg id `key` is the frozen half: a command declaring `conflicts_with = "key"` on a
  keyless-only flag hooks that id. `key.rs::tests::the_arg_id_stays_key` pins it.
- Each **`impl` block** — never the individual resolvers — carries one
  `#[cfg_attr(not(test), expect(dead_code, …))]`. `expect`, not `allow`: an unfulfilled
  expectation reds the build, so the suppression cannot outlive its reason. **Block-level is the
  load-bearing part.** A block-level `expect` stays fulfilled while *any* item under it is still
  unattached, so the loop that attaches the first resolver compiles with the option file untouched;
  only the loop attaching the **last** one gets `unfulfilled-lint-expectations`, and deleting the
  attribute is then its own correct edit. Per-resolver attributes would instead force *every*
  attaching loop to edit the frozen file — the same two-loops-one-file collision the "no `pub use`
  in `options.rs`" rule above exists to prevent. Verified on rustc 1.95.0: one resolver attached,
  file untouched, `cargo check -p ocx --locked` green; both attached, red with
  `this lint expectation is unfulfilled`; attribute deleted while both unattached, red with
  `methods … are never used`.
- `TagsOpt::resolve` reads the file through `crate::conventions::parse_tags_file` — the same reader
  `package announce` and `package cascade repair` use. One file format, one reader.
- `TagsOpt` exclusivity against `--platform` is declared **in each command file**, on the arg ids.

### The `--rekor-upload` asymmetry (frozen resolver contract)

`RekorUploadOpt::enabled(key_mode, configured)`, `options/rekor_upload.rs`:

| Key model | `--rekor-upload` | `--no-rekor-upload` | neither |
|---|---|---|---|
| **keyless** (`key_mode == false`) | `Ok(true)` | **`Err(SignErrorKind::RekorUploadRequiredForKeyless)`** | `Ok(true)` |
| **key** (`key_mode == true`) | `Ok(true)` | `Ok(false)` | `Ok(configured.unwrap_or(false))` |

`[trust.sigstore] rekor_upload` is the `configured` argument and applies to **key mode only**;
under keyless it is ignored **without a warning** — erroring or warning on every keyless signature
because a fleet-wide key-mode setting says `false` would let an unrelated configuration key break
the default signing path.

The keyless refusal is **deliberately not** clap `requires = "key"`. Clap renders "the following
required arguments were not provided: --key", which inverts the reason: the problem is not a
missing key, it is that a keyless signature without a log entry becomes unverifiable once the
certificate expires. The error variant carries that sentence, and
`sign/error.rs::tests` asserts the message contains both `--key` and `"ten minutes"` so a reword
cannot drop the reason.

### The carried G0 constraint — signing-time proof, never wall-clock

G0's keyless golden fixture carries a Fulcio certificate that **expired about ten minutes after
capture**. That is by construction: a short-lived certificate is *designed* to be expired by the
time anyone verifies it.

> **Certificate validity is anchored to the signing-time proof — the Rekor entry / SET — never to
> wall-clock "is this certificate valid now".** A wall-clock check makes every keyless fixture rot
> within the hour *and* is a real trust bug.

G1 encoded this in four places, and C, D and E must not undo any of them:

- `oci/verify/discovery.rs` module doc — the header section "Carried constraint from G0".
- `oci/resolve_target.rs` module doc — "Nothing in this module reads a clock, and nothing added to
  it may."
- `SignErrorKind::RekorUploadRequiredForKeyless`'s doc (`oci/sign/error.rs`) — the same reason, at
  the point where a user could ask for the entry to be skipped.
- `SignatureEntry.signed_at`'s doc (`api/data/verification.rs`) — "judged against this instant,
  never against wall-clock now"; absent when no transparency record exists, which is legal under a
  key and must be visible rather than inferred.
- `SignatureEntry`'s struct doc (`api/data/verification.rs`) — **loop D must read this before it
  renders a signature row in plain text.** `certificate_identity` / `certificate_oidc_issuer` come
  out of a registry-served Fulcio certificate, so they are attacker input; every value in a row
  reaching a plain-text table goes through `crate::api::data::sanitize_for_terminal` first, per
  field including the typed ones, exactly as `VerificationReport::plain_fields` already does
  (CWE-150). Today the array is JSON-only and `serde_json` escapes C0 controls, so nothing is
  exposed — the constraint exists so attaching the rows does not quietly become the exposure.

### What G1 deliberately did NOT do — and who owns it

| Not done | State in the tree | Owner |
|---|---|---|
| `TrustPolicy.signers` | `TrustPolicy` still carries `pub keyless: Option<KeylessMatcher>` (`trust.rs:522`); `SignerSpec` exists but is **unreachable from `Config`**, so the published JSON schema is unchanged | **B** |
| `PolicyBackend::Key` | `pub enum PolicyBackend { Keyless(CompiledKeyless) }` — one variant, deliberately **not** `#[non_exhaustive]` | **B** |
| `CompiledPolicy` → `Vec<PolicyBackend>` | `CompiledPolicy { builder, backend: PolicyBackend }` — still **one** backend | **B** |
| `trust::compile_key_signer` | `trust.rs:807`, body `unimplemented!("loop B: --key policy compilation for …")` at `trust.rs:811`. **The only production `unimplemented!()` G1 shipped.** Unreachable: every caller passes `None` | **B** |
| A `KeyBackend` implementor | The trait exists; no file backend, no test double | **B** |
| `required = true` on inline `-p/--platform` | Untouched at `package_sign.rs:34`, `package_verify.rs:55`, `package_attest.rs:32` | **C** (sign, attest), **D** (verify) |
| Attaching the four option groups | No command flattens any of them; only a doc-link mention exists (`package_sign_common.rs:357`). **`--help` is unchanged from before G1** | **C**, **D** |
| A `PredicateType` variant for the cosign image-signature predicate | `COSIGN_SIGN_PREDICATE_TYPE` is a **const only**; `oci/attest/predicate.rs` is untouched | — (decided, not deferred) |
| Wiring `resolve_sign_target` into a pipeline | Only tests call it | **C**, **D** |

The `PredicateType` decision, from the const's own doc: `PredicateType::from_str` already yields
`Uri(_)` for any absolute URI, and `is_provenance` / `builder_id` / `wrap` dispatch on `.uri()`, so
a variant buys nothing — while adding it to `PredicateType::ALIASES` would expose an
image-signature predicate as a user-selectable `attest --type` value. Matching is a `.uri()`-level
string comparison, the same rule `is_provenance` follows.

**Measured `unimplemented!()` census** (`grep -e unimplemented!`, one needle per invocation, over
`crates/ocx_lib/src` and `crates/ocx_cli/src`): `crates/ocx_cli/src` has **zero**.
`crates/ocx_lib/src` has one production occurrence — `trust::compile_key_signer` — and the rest are
pre-existing `#[cfg(test)]` transport doubles (`oci/client.rs`, `oci/client/transport.rs`,
`oci/referrer/capability.rs`, `oci/verify/pipeline.rs`, `oci/attest/pipeline.rs`, …) or prose in
comments. G1 added exactly one.

### Trust config shapes G1 froze (evaluation is B's)

`crates/ocx_lib/src/trust.rs`:

```rust
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SignerSpec { Keyless(KeylessMatcher), Key(KeyMatcher) }

pub struct KeyMatcher { pub key: Option<String>, pub key_pem: Option<String> }   // XOR, both #[serde(default)]

pub fn validate_signers(signers: &[SignerSpec], scope: &str) -> Result<(), TrustPolicyError>;
```

New `TrustPolicyError` variants: `NoSigners { scope }`, `KeyConflict { scope }`, `KeyUnset { scope }`.
**Empty `signers` fails closed** — a policy naming no acceptable signer accepts nothing, and the
permissive reading would turn a deleted line into a silent bypass. `key` is the *same* `KeyRef`
grammar `--key` parses, so a KMS entry later needs no config-format change; `ocx config push`
inlines the reference form into `key_pem` at publish time. There is no `key_regexp` — a public key
is a fixed value, not a pattern.

**Both arms fail closed, through one rule each.** A `keyless` entry is validated by
`compile_keyless_matcher(keyless, scope)` — the *same* function `TrustPolicy::compile_keyless`
calls, extracted to a free, scope-parameterised helper so the `[trust.policy.keyless]` table and a
`{ kind = "keyless" }` signer can never drift into accepting different things. So
`signers = [{ kind = "keyless" }]` — no identity, no issuer — is refused
(`IdentityUnset` / `IssuerUnset`), not silently accepted as a signer naming nobody. Tests:
`an_empty_signers_array_is_refused`, `a_key_signer_declares_exactly_one_form`,
`a_keyless_signer_declares_an_identity`, `a_keyless_signer_declares_an_issuer`,
`signer_entries_parse_from_the_frozen_kind_tagged_spelling`,
`a_key_signers_reference_is_the_same_grammar_the_key_flag_parses`.

`SignerSpec` derives `schemars::JsonSchema` while unreachable — it costs nothing now and is one
fewer edit when B attaches the field.

### The `package_sign_common.rs` seam

```rust
pub(super) async fn resolve_policies(
    context: &crate::app::Context, identifier: &oci::Identifier,
    certificate_identity: Option<&str>, certificate_oidc_issuer: Option<&str>,
    key: Option<&KeyRef>,                       // ← added by G1
) -> anyhow::Result<Vec<CompiledPolicy>>;

pub(super) async fn resolve_policies_lenient(/* same five params */) -> anyhow::Result<Vec<CompiledPolicy>>;
```

Key mode **short-circuits ahead of the certificate-flag pair**: `Some(key)` calls
`trust::compile_key_signer` and returns a one-element `Vec`, mapping a `TrustPolicyError` through
`VerifyErrorKind::from` (→ `TrustPolicyInvalid`, exit 78). Every caller in waves 1–2 passes `None`
(`package_verify.rs:168` → `resolve_policies`, `package_sbom.rs:289` → `resolve_policies_lenient`,
both commented "Keyless until loop D attaches `--key` to this command"), so the `unimplemented!()` is unreachable. **Loops C and D supply the argument from
`KeyOpt::reference` in their own command files — this shared leaf is not edited again.**

The keyless-flag rejection is *not* here: each command that carries both `KeyOpt` and the keyless
certificate flags declares `conflicts_with = "key"` **in its own command file**.

### Divergences from this plan's prose, recorded because the code is the contract

| This plan said | The tree has |
|---|---|
| `ExitCode::UnsupportedKeyBackend = 85` "plus its own `error_envelope.rs` arm" | The classification table **moved** to `ocx_lib/src/cli/error_category.rs`; `error_envelope.rs` gained only two end-to-end tests |
| `SignatureFormat` — "full enum + serde + `FromStr`" | serde + a hand-written `clap_builder::ValueEnum`. **No `FromStr`.** `signature_format_slugs_are_frozen` pins the two channels to each other |
| `KeyBackend` — `fn public_key(&self)`, `fn hint(&self)`, plus an "`unimplemented!()` file impl" | `fn public_key_der(&self) -> &[u8]`, `fn kind(&self) -> KeyBackendKind`, `fn hint(&self) -> String` (defaulted). **No file impl at all** — B writes the first implementor |
| `signers` — "type + `Deserialize` only, no evaluation" | Also `Serialize`, `schemars::JsonSchema`, `validate_signers` and three `TrustPolicyError` variants. Shape validation landed; *backend compilation* did not |
| `COSIGN_SIGN_PREDICATE_TYPE` "+ the matching `PredicateType` arm" in `oci/attest/predicate.rs` | Const only; `predicate.rs` untouched, for the reason in the const's doc |
| `package_sign_common.rs` — "`--key` → `CompiledPolicy`, the key-vs-keyless flag rejection, and `--rekor-upload` resolution" all fully implemented there | The `--key` parameter is there but terminates in B's `unimplemented!()`; the flag rejection is each command file's `conflicts_with = "key"`; `--rekor-upload` resolution lives in `options/rekor_upload.rs` |
| `--platform` `required = true` at `package_sign.rs:32`, `package_verify.rs:34`, `package_attest.rs:55` | `package_sign.rs:34`, `package_verify.rs:55`, `package_attest.rs:32` — line numbers only |
| `signatures[].discovery_method` | The JSON field **is** `discovery_method`; `oci/verify/discovery.rs`'s module doc calls it `signatures[].discovery`. **The struct field is the contract** |

### Verification time is a typed anchor, never a clock (G1 addendum)

Appended after G1. **Read-only for loops C, D and E.**

**The rule.** Verification anchors certificate validity to the **signing-time proof** — the Rekor
entry's `integratedTime` / SET — never to a wall-clock "is this certificate valid now". G0's keyless
golden fixture (`test/tests/fixtures/golden/keyless_bundle.json`) carries a Fulcio certificate whose
window is `2026-08-29T02:07:54Z .. 02:17:54Z`, so a clock-reading check refuses a legitimately signed
artifact — and would refuse every real keyless signature older than its ten-minute window.

**The type.** `crates/ocx_lib/src/oci/verify/signing_instant.rs`:

```rust
pub(super) enum SigningInstant {
    TransparencyLog(i64),
    CallerSupplied(i64), // DELETED 2026-08-30 -- see the amendment below
}
impl SigningInstant { pub(super) const fn epoch_seconds(self) -> i64 }
```

`pub(super)` = `pub(in crate::oci::verify)`, the narrowest visibility that works: the type appears
only in `tlog::verify_integrated_time_within_certificate`'s signature and at the `pipeline.rs` call
site, both inside `oci::verify`. **No `Default`, no `From<SystemTime>`, no constructor spelling
"now"** — that absence is the whole contract.

**The changed signature** (`crates/ocx_lib/src/oci/verify/tlog.rs`, behaviour unchanged — same
inclusive window, same `VerifyErrorKind::CertificateValidityWindow` with the same three fields):

```rust
pub(super) fn verify_integrated_time_within_certificate(
    signed_at: SigningInstant,
    leaf: &Certificate,
) -> Result<(), VerifyErrorKind>
```

> **UNFROZEN AND REVERSED, 2026-08-30, owner decision (loop P5).** The `CallerSupplied` half of
> this row is gone. Taking the signing instant from the certificate's own `notBefore` is circular —
> it asks the certificate when it was valid and then judges the certificate against its own answer,
> so the window check can never fail — and it reached further than the note below claims:
> `sidecar_bundle` set the synthesised entry's `integrated_time` to the same value, so `sigstore`'s
> chain build *and* its expiry check anchored on it too. A Fulcio leaf valid for ten minutes a year
> ago verified for ever, and a later-revoked identity was undetectable. cosign refuses that shape by
> default and needs `--insecure-ignore-tlog` to accept it.
>
> **What replaced it.** `SigningInstant` has one variant, `TransparencyLog(i64)`. A keyless
> simplesigning sidecar verifies only with transparency-log evidence: the layer's
> `dev.sigstore.cosign/bundle` annotation, whose SET is verified against the log's public key
> (`tlog::verify_set`, no Merkle proof — cosign's v1 offline bundle carries none) and whose logged
> `hashedrekord` body must bind to this signature over this payload. Its `integratedTime` is then the
> instant, and is reported as `signed_at` beside the entry's `rekor_log_index`. Without an entry the
> refusal is `VerifyErrorKind::SignatureInvalid` (65 — no new `ExitCode`, no new kind), raised
> **after** chain, SCT, signature and identity so a wrong-identity sidecar still reports
> `identity_mismatch`. `--allow-unlogged-signature` is the opt-out for air-gapped CI; under it the
> explicit window check is skipped rather than fed a value nothing proved, which is why no caller of
> `CallerSupplied` survived.
>
> The paragraph below is kept as the record of what the contract was. It is no longer the contract.

**Loop D: the no-transparency-log simplesigning path is LEGAL, and its caller supplies the instant.**
`SigningInstant` does *not* mean "a transparency log is required". cosign v3.1.1's `attach signature
--rekor-response` validates its argument and never writes the `dev.sigstore.cosign/bundle`
annotation, so the committed `test/tests/fixtures/golden/simplesigning_*` fixtures carry a signature
and a certificate and **no** offline tlog material — spec D5 declares that absence a legal shape,
not malformed input. That path passes `SigningInstant::CallerSupplied(…)`. The variant carries
`#[cfg_attr(not(test), expect(dead_code, …))]` until that path constructs it; the loop that wires it
deletes the attribute (an unfulfilled `expect` is itself a build failure).

**Pinned by** `tlog::tests::the_golden_keyless_certificate_verifies_at_its_logged_instant_and_is_refused_later`
(the real G0 certificate: verifies at its logged `integratedTime`, refused a day past `notAfter`) and
`signing_instant::tests::the_certificate_validity_path_reads_no_clock` (source scan over
`src/oci/verify/`; `trust_cache.rs` and `trust_resolve.rs` are allow-listed because their clock reads
decide trust-material **cache freshness**, never a validity window).

## FC-FALLBACK — clarified, not reversed (2026-08-30, owner decision)

Companion to the `signing_instant` amendment above. Recorded separately because it is a
*decision not to change* something, which leaves no trace in the code.

The bundle→simplesigning fallback stays **automatic on absence**. The owner declined
gating it behind an explicit `--signature-format simplesigning` pin. Rationale: once
FC-SIGNING-INSTANT requires transparency-log evidence, the fallback target is no longer
the weak path, so absence-only is coherent — and gating it would break an artifact
genuinely signed with only a cosign sidecar, which cosign itself discovers automatically.
Costing parity to buy hardening was judged the wrong trade once the target was hardened.

Distinct from that decision, and fixed as a plain bug in `1d25c8c2`: the fallback also
fired when a bundle was fetched and **cryptographically refused**, ending in exit 0 with
nothing named as having failed. That was drift from D9's own stated intent ("fires only
when no pin excludes it"), not a design choice. Present-but-refused now fails closed.

Both halves are pinned by `test_cosign_matrix_extras.py::test_a_withheld_bundle_must_not_
expose_an_expired_certificate_sidecar` (X-04), which composes them into the real attack
and reds if either regresses.
