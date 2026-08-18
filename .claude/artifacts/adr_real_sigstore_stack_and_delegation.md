# ADR: Real Sigstore stack for acceptance tests, and delegation of signing/verification to sigstore-rs

## Metadata

- **Status:** Proposed
- **Date:** 2026-08-18
- **Deciders:** OCX maintainers (owner gate)
- **Scope:** milestone 2 ([#24](https://github.com/ocx-sh/ocx/issues/24)), milestone 5 ([#205](https://github.com/ocx-sh/ocx/issues/205)), dial-site SSRF fix
- **Related Issues:** [#195](https://github.com/ocx-sh/ocx/issues/195) · [#196](https://github.com/ocx-sh/ocx/issues/196) · [#197](https://github.com/ocx-sh/ocx/issues/197) · [#107](https://github.com/ocx-sh/ocx/issues/107) · [#206](https://github.com/ocx-sh/ocx/issues/206) · [#207](https://github.com/ocx-sh/ocx/issues/207) · [#208](https://github.com/ocx-sh/ocx/issues/208) · [#209](https://github.com/ocx-sh/ocx/issues/209) · [#210](https://github.com/ocx-sh/ocx/issues/210) · PR [#203](https://github.com/ocx-sh/ocx/pull/203)
- **Tech Strategy Alignment:** ☑ Rust 2024 / Tokio · ☑ no new language or runtime · ☑ docker-compose acceptance harness
- **Domain Tags:** signing, verification, supply chain, test infrastructure, SSRF
- **Amends:** [`adr_oci_referrers_signing_v1.md`](./adr_oci_referrers_signing_v1.md) — Decision Driver **D9** ("Testability without live Sigstore": *"CI must not depend on live Fulcio/Rekor. We test against Sigstore staging and pre-generated deterministic fixtures"*) is re-decided by D1; **Amendment 5** (TUF rotation SLA — embedded-only, 90-day forced-upgrade window, nightly `sigstore-trust-root-drift.yml` parity check) is re-decided by D3, which takes that amendment's own named Option (B), `SigstoreTrustRoot::from_tuf()`; **Amendment 4** (Rekor v2 client tracking, still carrying `<TBD-upstream-rekor-v2-issue>` / `<TBD-internal-issue>` placeholders) is filled in by D6. Decisions S1-A through S1-I, the exit-code taxonomy, and the JSON error envelope stand unchanged. [`adr_offline_verify_trust_cache.md`](./adr_offline_verify_trust_cache.md) — its four-rung precedence ladder is preserved and extended by one field, not replaced.
- **Superseded By:** —

## Decision Drivers

- **Delete, do not add.** Milestone 5's purpose is removing hand-written crypto. A design
  that nets more ocx-owned cryptographic code has failed regardless of its other merits.
- **A green must be distinguishable from "never ran."** A gated service the bring-up path
  does not start makes every Sigstore test skip and report success — the failure mode
  [`quality-core.md`](../rules/quality-core.md) "Unchecked Green" names.
- **One trust-material code path** for public-good Sigstore, a self-hosted stack, the
  acceptance suite, and air-gapped verify. Four ladders is four sets of bugs.
- **The CLI contract is frozen.** Flags, exit codes (83 `RekorUnavailable`, 84
  `ReferrersUnsupported`), and `--format json` shapes do not change.
- **The guard belongs at a seam, not at call sites.** A fix that adds a fourth, fifth and
  sixth `guard_physical_dial` call is incomplete again at the seventh.
- **CI cost is real and, apart from the image footprint, unmeasured.** Per
  [`performance.md`](../rules/rust-quality/performance.md) PERF-01, no wall-clock figure
  appears here or in a commit message until a command produced it.

## Context

ocx signs and verifies packages keylessly over the OCI 1.1 Referrers API. The
v1 implementation hand-rolled the Fulcio CSR exchange, the Rekor upload, a
bespoke `ocx-rekor-set-v1` SET payload, and a single-hop certificate chain
walk — roughly 500 lines of security-critical code that `sigstore-rs` already
implements.

That hand-rolling was not a preference. It was forced. The acceptance suite
runs against `test/tests/fixtures/fake_sigstore.py` (909 lines), a Python fake
that **mints no SCT**. `sigstore-rs` calls `verify_sct` unconditionally on the
signing path (`bundle/sign.rs:140`, `:143`), so against the fake the crate can
only fail — and ocx bypassed it entirely.

The causal chain is therefore: *fake stack → no SCT → sigstore-rs unusable →
hand-rolled crypto*. Replacing the fake with a real stack inverts milestone 5
from "write more crypto" into "delete hand-rolled crypto".

### Empirical validation

Every claim in D1 below was verified by standing the stack up, not inferred.
Evidence captured 2026-08-18:

| Check | Result |
|---|---|
| dex resource-owner password grant → ID token | ✓ `iss=http://dex:5556/dex`, `email=ocx-test@example.com`, `aud=ocx-test` |
| Fulcio `--ca fileca` + CT → certificate | ✓ form `signedCertificateEmbeddedSct`, chain length 2 |
| Embedded SCT (OID `1.3.6.1.4.1.11129.2.4.2`) | ✓ **present** — the CT log accepted the chain |
| Fulcio issuer-v2 OID `1.3.6.1.4.1.57264.1.8` | ✓ `http://dex:5556/dex` |
| TesseraCT `posix` with a plain `openssl ecparam prime256v1` PEM | ✓ starts, `primarySigner=ocx-test`, no note-format tooling |
| Rekor tree provisioning | ✓ **auto-created** (`active tree 5996692224814922961`) — no `createtree` |
| Rekor entry → `signedEntryTimestamp` | ✓ inline on creation |
| Rekor entry → `inclusionProof` **with checkpoint** | ✓ inline on creation, no polling (`treeSize=1`) |

The last row settles `#197`: ocx emits `inclusion_proof: None`
(`oci/sign/bundle.rs:66`) under a **v0.3** media type — which `sigstore-rs`'s
own verifier and cosign v3 both reject — not because Rekor withholds the proof,
but because the hand-rolled client never asked for it.

## Considered options

Two decisions carry real optionality; the rest follow from them. Weights below are the
Decision Drivers, scored 1–5, higher is better.

### D1 — When the stack starts

The service *set* is forced (sigstore-rs has no Rekor v2 client → Rekor v1 → Trillian →
MySQL). The only genuine freedom is when those containers come up.

| Weight | Criterion |
|---|---|
| ×3 | Signal integrity — the tests that exist actually run |
| ×2 | Cold and warm wall-clock cost, local and CI |
| ×2 | Reachability on a non-amd64 developer machine |
| ×1 | Implementation size and number of bring-up paths |

| Option | Signal ×3 | Cost ×2 | Reach ×2 | Size ×1 | Total | Risk | Reversibility |
|---|---|---|---|---|---|---|---|
| **1. Default compose profile** — services join the default set; `test/src/helpers.py:69` already runs `docker compose up -d` with no `--profile`, so nothing in the bring-up path changes | 5 | 2 | 2 | 5 | **28** | Every cold `task test` and every CI job pays the full startup, including runs that touch no signing code | Trivial — add `profiles:` to seven services |
| **2. Opt-in `sigstore` profile** *(chosen)* — `profiles: [sigstore]`, following the `bench-proxy` precedent at `test/docker-compose.yml:77-84`, **with** the bring-up path taught to request it and a zero-skip assertion | 4 | 5 | 5 | 3 | **32** | If the bring-up change is omitted, the whole suite skips and reports green — the exact "Unchecked Green" trap. Mitigated by the assertion in D1 below, which is not optional | Trivial in both directions |
| **3. Record fixtures once, no live stack** — a manual task signs a corpus; committed bundles plus a committed `trusted_root.json` are all the suite ever sees | 1 | 5 | 5 | 4 | **22** | Cannot exercise the **sign** path at all, and fixtures rot silently when Fulcio's cert profile moves. Fixed `notAfter` / `integratedTime` force the temporal check to be faked — an escape hatch [`adr_oci_referrers_signing_v1.md`](./adr_oci_referrers_signing_v1.md) D4 forbids | One-way for the milestone: reaching options 1 or 2 means building the stack anyway |

Option 3 is nonetheless adopted *in part*: the committed `trusted_root.json` and key
material described in D1 are exactly that idea, applied to trust material rather than to
bundles. The live stack signs; the committed file supplies trust.

### D2 — How much ocx deletes

| Weight | Criterion |
|---|---|
| ×3 | Correctness of the shipped verification |
| ×3 | Risk of shipping a *silently* weaker verify than today |
| ×2 | Upstream convergence and long-run maintenance |
| ×1 | Lines deleted |

| Option | Correct ×3 | Silent-risk ×3 | Upstream ×2 | Lines ×1 | Total | Risk | Reversibility |
|---|---|---|---|---|---|---|---|
| **1. Delete everything; call `Verifier::verify_digest` and stop** | 2 | 1 | 5 | 5 | **24** | Ships a verifier performing **no** Merkle-inclusion check and **no** SET check (`verifier.rs:196-202` are `TODO`s) while the docs claim the gap is closed. Reintroduces the omission class behind five of the six CVEs in semantics research §9 | Recoverable only by re-adding the layer this option deleted |
| **2. Keep the hand-rolled clients; swap only the test stack** | 3 | 4 | 1 | 1 | **24** | Nets zero deletion — the stated purpose of [#205](https://github.com/ocx-sh/ocx/issues/205). ocx permanently owns X.509 chain walking, SCT parsing and Merkle arithmetic: Block-tier under "Don't Own Non-Domain Code" | Sticky — code accretes around the hand-written clients every month it holds |
| **3. Delete the clients and primitives; own only the orchestration** *(chosen)* | 5 | 5 | 4 | 4 | **42** | ocx carries one small adapter module until sigstore-rs#285 lands; it is the module a security review must read hardest | Deleting it later is mechanical: the call collapses to `verify_digest` |
| **4. Fork sigstore-rs and land the `TODO`s upstream first** | 5 | 5 | 5 | 5 | **45**\* | \*Discounted to unviable: blocks the milestone on an upstream review cycle of unknown length, and adds a second vendored fork. The precedent for forking here (`external/rust-oci-client`) was a *missing capability*, not a missing call sequence | Additive — the same logic, relocated |

**Bundle-struct assembly is not banned and is not being deleted.** `oci/sign/bundle.rs`
serialises a protobuf-JSON message and implements no cryptographic primitive.
`SigningArtifact::to_bundle()` emits bundle **v0.2** and ocx's published contract is
**v0.3** (`oci/sign/bundle.rs:27`), so hand-assembling the struct is the only way to honour
the format already in the wild. A reviewer should not re-litigate this.

## Decision

### D1 — The test stack

Seven services in `test/docker-compose.yml`, behind an opt-in compose profile
`sigstore` (the existing `bench`/toxiproxy profile at `test/docker-compose.yml:77-84`
is the in-repo precedent). Ordinary `task test` is unaffected; the signing suite
requests the profile.

**Bring-up, named — this is the part that silently fails if left implicit.**
`test/src/helpers.py:63-69` runs `docker compose -f … up -d` with **no
`--profile` flag**, so a profile-gated service does not start and every test
depending on it skips. Three concrete changes, all mandatory together:

1. `start_registry()` gains a `profiles: Sequence[str] = ()` parameter and
   inserts one `--profile <name>` per entry before `up`. Signature change only;
   existing callers are unaffected.
2. A session-scoped `sigstore_stack` fixture in
   `test/tests/fixtures/sigstore_stack.py` requests `profiles=("sigstore",)`,
   waits on the compose healthchecks, and yields the endpoint set. The whole
   profile is torn down as a unit at session end — never per-test, and never
   `restart rekor` (see tree churn below).
3. **A zero-skip assertion.** A `pytest_sessionfinish` check fails the run if any
   Sigstore-marked test was skipped for a missing stack. Without it, forgetting
   the profile produces a green run that verified nothing — the failure mode
   [`quality-core.md`](../rules/quality-core.md) "Unchecked Green" and
   [`testing.md`](../rules/rust-quality/testing.md) TEST-12 exist to stop. The
   assertion must itself be **shown red** by temporarily dropping the profile
   flag, before it is trusted.

Host ports are env-var-parametrized in the same style as the existing services
(`OCX_TEST_FULCIO_PORT`, `OCX_TEST_REKOR_PORT`, `OCX_TEST_DEX_PORT`,
`OCX_TEST_CT_PORT`), each service carries a why-comment, and every service
carries a healthcheck — including the three the current file lacks entirely.

**Always-on was considered and rejected.** MySQL alone declares
`start_period: 90s`, and `test/tests/` runs on every `task test`. Paying a
cold-start on every non-signing run for a dependency ~85% of the suite does not
use fails [`performance.md`](../rules/rust-quality/performance.md) PERF-04's
cold/warm split before it is measured. Migration step 2 measures the number
before anything depends on the choice.

Every image is pinned **by digest**, not by tag. TesseraCT publishes only a
floating `:latest`, so a digest pin is the only reproducible reference for it;
the rest are pinned for consistency and to survive tag re-pushes. Digests
captured 2026-08-18.

| Service | Image | Digest | Role |
|---|---|---|---|
| `dex` | `dexidp/dex:v2.45.1` | `sha256:8499afd6…08462` | OIDC issuer |
| `sigstore-ct` | `ghcr.io/transparency-dev/tesseract/posix` | `sha256:43eb4815…04116e` | CT log (static-ct-api) |
| `fulcio` | `ghcr.io/sigstore/fulcio:v1.8.8` | `sha256:ef72cf56…d60753` | CA |
| `rekor` | `ghcr.io/sigstore/rekor/rekor-server:v1.4.2` | `sha256:a8052cbe…5f1cbd` | transparency log |
| `trillian-log-server` | `gcr.io/trillian-opensource-ci/log_server:v1.7.2` | `sha256:d12a110a…0016cc` | Rekor backend |
| `trillian-log-signer` | `gcr.io/trillian-opensource-ci/log_signer:v1.7.2` | `sha256:195bd725…58369c` | Rekor backend |
| `mysql` | `gcr.io/trillian-opensource-ci/db_server:v1.4.0` | `sha256:0794abd3…5964d8` | Trillian storage, schema preloaded |

Three findings materially shrink this against the researched baseline:

1. **TesseraCT needs no Trillian, no MySQL, and no tree provisioning.** POSIX
   storage, self-contained, and it accepts a plain P-256 PEM — the note-format
   key concern was unfounded. This removes the legacy CTFE entirely.
2. **CTFE was rejected on a hard constraint, not preference.** No `createtree`
   image is published, and the CTFE image is distroless (no shell), so its
   config cannot be templated with a runtime-provisioned tree ID.
3. **Rekor auto-creates its Trillian tree**, so the same provisioning problem
   does not arise there.

**Key material is pre-generated and committed** (`test/sigstore/keys/`,
regenerable via `test/sigstore/generate-test-keys.sh`). Fulcio runs
`--ca fileca`, not `ephemeralca`: the CT log needs Fulcio's root at *its* own
startup, so the root cannot be minted at Fulcio's startup. Committing throwaway
localhost-only keys is what makes `trusted_root.json` a **static committed
file** rather than something the suite mints at runtime — no `cosign`
dependency in the test path, and it doubles as the worked example for the
self-hosting documentation.

**Issuer URL.** dex's issuer is the in-network name `http://dex:5556/dex`,
because that is what Fulcio fetches discovery/JWKS from and what lands in the
token's `iss` claim — hence what `[[trust.policy]]` pins. The host reaches only
the token endpoint, via the published port; nothing host-side needs the issuer
URL to resolve.

**Non-interactive OIDC** is a resource-owner password grant
(`oauth2.passwordConnector: local`): one HTTP call, no browser, no callback
listener. `test/sigstore/get-token.py` is the helper.

**Deleted:** `test/tests/fixtures/fake_sigstore.py` (909 lines) and
`test/tests/test_fake_sigstore.py` (365 lines / 15 tests). The remaining 64
signing tests across `test_sign.py`, `test_verify.py`, `test_offline_verify.py`,
`test_trust_policy.py`, `test_auto_verify.py`, `test_referrers_capability.py`
swap fixtures; 50 of them are constant swaps (`FAKE_SUBJECT` →
`ocx-test@example.com`, `FAKE_ISSUER_URL` → `http://dex:5556/dex`; 20 sites in
`test_trust_policy.py` alone) and 11 need real work — see D1a.

**Retracted: there are no skipped tests.** An earlier draft claimed "8 in
`test_offline_verify.py`, 12 in `test_auto_verify.py`" become runnable. A marker
scan for `mark.skip`, `mark.xfail`, `skipif` and `pytest.skip(` across all six
consuming files finds **two markers total**, neither of them that:
`test_sign.py:442` `@pytest.mark.skipif(sys.platform == "win32")`, which is
correct and stays, and `test_sign.py:637` `@pytest.mark.xfail(strict=True)` on
`test_sign_then_sign_again_is_idempotent`. `test_offline_verify.py` contains 5
test functions in total, so "8 skipped" there was arithmetically impossible. The
claim was this ADR's only unfalsifiable green — a benefit that would have been
reported as delivered without anything able to contradict it — and it is deleted
rather than softened. **The `xfail` is not fixed by a real stack either**: it
covers a re-push gap in `sign/pipeline.rs`, which this milestone does not touch,
so it stays `xfail(strict=True)` and any plan claiming otherwise is wrong.

**Cost and limits.** ~1.5 GB of images; MySQL dominates startup. The Trillian
and MySQL images are **amd64-only** — acceptable (dev host and GitHub runners
are amd64) but it must be documented, and the profile must skip with a stated
reason on arm64 rather than fail obscurely.

**Rekor tree churn (measured).** With `--trillian_log_server.tlog_id` unset,
Rekor creates a **new tree on every boot** — observed `5996692224814922961` →
`3163640041043182391` across one `docker compose restart`. Consequences, and
why this is tolerable:

- The trust root is **unaffected**: Rekor re-signs checkpoints with
  `--rekor_server.signer` (our committed static key), not with Trillian's
  per-tree key. `trusted_root.json` stays static across tree churn.
- Entries written before a restart become unreachable, so their inclusion
  proofs no longer verify. The profile must therefore be brought **down and up
  as a unit**; restarting `rekor` alone mid-suite silently invalidates every
  signature made earlier in that run.
- Pinning a fixed `tlog_id` is not available as a fix: it requires a
  pre-existing tree, and no `createtree` image is published (see D1 finding 2).

This is a documented operational constraint plus a suite-level guard, not a
silent hazard.

### D2 — What ocx deletes

**The visibility wall governs this table.** `sigstore-rs` 0.14 declares
`crypto::merkle`, `crypto::certificate`, `crypto::certificate_pool`,
`crypto::keyring` and `crypto::transparency` as `pub(crate)` with no
re-exports (`crypto/mod.rs:22,131,133,137,150`; a probe importing each yields
six `E0603`). So `verify_sct`, `CertificateEmbeddedSCT`,
`CertificatePool::verify_cert_with_time`, `MerkleProofVerifier` and
`Rfc6269Default` are **real, complete, and unreachable from ocx** — they run
only as internal side effects of `bundle::verify::Verifier`. Every "delegate
this" verdict below is stated in terms of what is *callable*, not what exists.
Evidence: `research_sigstore_rs_visibility_and_capabilities.md` §2.

| Issue | Verdict |
|---|---|
| `#206` real X.509 parsing | **Already done** — `oci/verify/identity.rs:26` uses `x509-cert`'s `Certificate::from_der`. The "structural TLV check" premise is stale. Close. |
| `#207` Fulcio chain walk + temporal validity | **Deletion, but only via `Verifier`.** `verify_cert_chain` (`oci/verify/pipeline.rs:441-468`) is single-hop and `cert_expired_but_tlog_valid` is hardcoded `false` (line 330); both die. Their replacement is *not* a direct call — `CertificatePool` is `pub(crate)`. The chain walk and the `integrated_time` window check (`verifier.rs:204-219`) are obtained by routing verification through `bundle::verify::Verifier`, which is public and takes a `ManualTrustRoot`. |
| `#208` SCT / CT-log verification | **Deletion via `Verifier` + a stack change.** ocx has *zero* SCT references today. `verify_sct` is itself `pub` (`crypto/transparency.rs:296`) but sits in `pub(crate) mod transparency` (`crypto/mod.rs:150`), so it cannot be called from outside the crate, so ocx never gains an SCT code path of its own: `Verifier` performs it internally (`verifier.rs:165-168`) once a real CT log makes an SCT exist. On the **sign** side ocx does not verify the SCT at all — Fulcio embeds it and ocx carries it into the bundle. |
| `#209` Rekor SET + Merkle inclusion proof | **Split; both halves are wired glue over public API, zero hand-rolled crypto.** *Merkle + checkpoint*: `rekor::models::InclusionProof` is fully public — `new(log_index, root_hash, tree_size, hashes, checkpoint)` (`rekor/models/inclusion_proof.rs:45`) and `verify(&self, entry: &[u8], rekor_key: &CosignVerificationKey)` (`:63`). `verify` alone enforces the checkpoint is present, verifies its signature (`checkpoint.rs:109`, public), binds checkpoint root+size to the proof, then does the RFC 6269 leaf hash and inclusion path. `entry` is the bundle's `canonicalized_body` directly, so **no reverse `LogEntry` adapter is needed** and the forward-only conversion at `bundle/models.rs:72` is irrelevant. The one seam, `SignedCheckpoint::decode`, being `pub(crate)` is dissolved by the public `impl Deserialize for SignedCheckpoint` (`checkpoint.rs:220`), which deserializes from a JSON string and is symmetric with its `Serialize`. *SET*: the true algorithm at `cosign/bundle.rs:80-105` is `pub(crate)`, and its only public door wants a whole cosign-v1 artifact bundle behind the `cosign` feature, which is refused — see **D2a** below.  ocx composes the two public halves instead: `serde_json_canonicalizer::to_vec` over the four-field `{body, integratedTime, logIndex, logID}` struct, then `CosignVerificationKey::verify_signature`. This replaces `ocx-rekor-set-v1` (`oci/sign/rekor.rs:189-198`) with the **real wire format**. |
| `#210` TUF trust root | **Mostly retired by the crate.** `trust/sigstore/constants.rs` `include_bytes!`-embeds the production root, which retires the `TrustRoot::load_embedded` stub outright. `SigstoreTrustRoot::new()` enforces expiry (`ExpirationEnforcement::Safe`) and resolves disk-cache → embedded → remote with sha256 write-back, but **still needs network** and its constants are `pub(crate)`, so it **cannot be redirected at a self-hosted TUF repo**. The public escapes are `from_trusted_root_json_unchecked(&[u8])` and `from_client_trust_config(&PathBuf)`. |
| `#194` sign/verify via sigstore-rs | **Partly ocx-owned, unavoidably.** See below. |

**Signing is delegable only against public-good Sigstore, and this milestone is
the case where it is not.** The earlier flat claim "signing cannot be delegated"
is *overstated* and is corrected here, because the precise version carries a
stronger argument than the loose one did.

`SigningContext` (`bundle/sign.rs:260`) has three public constructors, and they
split exactly along the line this milestone sits on:

| Constructor | Public? | Works for a self-hosted stack? |
|---|---|---|
| `SigningContext::new(fulcio, rekor_config, ctfe_keyring)` (`:267`) | yes | **no** — the third argument is a `Keyring`, a `pub struct` (`crypto/keyring.rs:85`) sealed behind `pub(crate) mod keyring` (`crypto/mod.rs:137`), with no public constructor and no public path from a `ManualTrustRoot` or a `trusted_root.json` to one |
| `SigningContext::async_production()` (`:283`) | yes, under `sigstore-trust-root` | **no** — hardcodes `FULCIO_ROOT` and `SigstoreTrustRoot::new(None)`, i.e. public-good infrastructure, and builds the `Keyring` internally from `trust_root.ctfe_keys()` |
| `SigningContext::production()` (`:302`) | yes, under `sigstore-trust-root` | **no** — same, and additionally builds its own `tokio` current-thread runtime and `block_on`s it, which panics when called from inside a tokio task (`async.md` ASYNC-08) |

So the public door exists, and it opens onto exactly one destination. Upstream
[sigstore-rs#562](https://github.com/sigstore/sigstore-rs/issues/562) (open)
states the only escape for any other destination is vendoring the `Keyring`
construction.

**This strengthens the decision rather than weakening it.** Taking the delegated
path where it works would leave ocx with *two* signing implementations — a
delegated one reachable only against public-good Sigstore, and an ocx-owned one
for every self-hosted, air-gapped and test deployment, which is the whole
subject of `#196` and of this ADR. Two implementations means the path the
acceptance suite exercises is not the path most users run, which is the specific
failure this milestone exists to end. One ocx-owned orchestration that is
identical for public-good and self-hosted is the better shape even at the moment
upstream opens the door.

**So replacing the Python fake with a real stack is necessary but not
sufficient**, and the plan must not assume otherwise.

ocx therefore keeps an ocx-owned **signing orchestration** assembled from
public pieces — `FulcioClient::request_cert_v2` (returns the full chain, both
SCT shapes, errors when `certs.len() < 2`), `rekor::apis::entries_api::create_log_entry`,
`x509-cert`/`p256`, and a hand-assembled `Bundle`. None of that is hand-rolled
crypto or a hand-rolled wire format, which is what the non-negotiable forbids.

Net: `oci/sign/fulcio.rs` (164 lines) and `oci/sign/rekor.rs` (223 lines)
**shrink to thin adapters rather than deleting outright**; `oci/verify/pipeline.rs`
(1583 lines) loses its chain walk, temporal check and SET check to `Verifier`
plus the two ocx-wired steps.

**Because `sigstore-rs`'s own verifier skips steps 5 and 6, ocx must keep an
explicit orchestration layer rather than calling `Verifier::verify()` and
trusting it.** This is the single most important correctness finding of the
design phase: delegating naively would ship a *weaker* transparency guarantee
than ocx has today.

> **Precision note on the wall (verified against crate source, not docs).** All
> four gated items are unreachable, but for two of them the gate is the *module*,
> not the item: `verify_sct` is `pub fn` (`crypto/transparency.rs:296`) and
> `Keyring` is `pub struct` (`crypto/keyring.rs:85`), each sealed by a
> `pub(crate) mod` line (`crypto/mod.rs:150` and `:137`). `CertificatePool`
> (`:133`, `:135`) and `MerkleProofVerifier` (`:22`) are `pub(crate)` at both
> levels. This matters twice. First, a reader grepping `pub fn verify_sct` will
> find `pub` and conclude this ADR is wrong — it is not, and the module line is
> the citation to check. Second, **the upstream ask is one word**: changing two
> `pub(crate) mod` declarations to `pub mod` would make direct SCT verification
> and a caller-supplied `Keyring` reachable without any API design work. That is
> a far cheaper upstream PR than sigstore-rs#562's signing story, and it is worth
> filing on those grounds — but it is *not* on this milestone's critical path,
> because routing through `Verifier` already delivers `#207`/`#208` today.

### D2a — Why the `cosign` feature is refused

The refusal stands, but **not** for the reason first recorded. The original
justification — that enabling `sigstore`'s `cosign` feature "drags a second
`oci-client` into the workspace" — is **false**, and was corrected on
verification rather than left in place:

`[patch.crates-io] oci-client = { path = "external/rust-oci-client" }`
(`Cargo.toml:16-17`) applies **graph-wide**, transitive dependencies included.
sigstore 0.14 requires `oci-client = "0.17"` (`sigstore-0.14.0/Cargo.toml`,
`[dependencies.oci-client]`) and the fork is `0.17.0`
(`external/rust-oci-client/Cargo.toml:22`), which satisfies `^0.17`. Cargo
therefore **unifies** — one `oci-client`, and it is ours. A design decision
resting on a false premise is exactly the failure this ADR exists to prevent,
so the real reasons are recorded instead:

1. **It would compile sigstore-rs against our diverged fork.** The fork exists
   *in order to* diverge — `pull_referrers_native` is the reason it exists, and
   it tracks `ocx/integration`, ahead of upstream. Enabling `cosign` makes every
   future fork change a potential break in a third-party crate's build, in a
   module ocx does not otherwise use.
2. **It pulls sigstore's `registry` layer**, which duplicates ocx's own
   transport. `OciTransport` already has seven implementations; a second
   registry client in the binary is a second set of timeout, retry, redirect and
   SSRF-guard semantics to keep in step, and only one of them would be ocx's.
3. **It re-opens a TLS-backend hazard that is currently closed by one line.**
   sigstore's `default` is `["full", "native-tls"]`, and its `native-tls`
   feature forwards `oci-client?/native-tls`. ocx pins
   `default-features = false, features = ["bundle", "rustls-tls"]`
   (`Cargo.toml:123`), which is what keeps `native-tls`/`openssl` out of the
   graph per `security.md` SEC-14. Widening the feature set is the moment that
   pin gets loosened by someone who does not know it is load-bearing.

What the feature would have bought — a public door to the SET algorithm at
`cosign/bundle.rs:80-105` — is obtained instead by composing two public halves,
as D2's `#209` row describes. The cost is a few lines of canonicalization glue;
the alternative costs a permanent coupling between our fork and a crate we
otherwise consume at arm's length.

**Side-finding, not blocking:** `deny.toml` has no `[[bans.deny]]` entry for
`native-tls`/`openssl`/`openssl-sys`, so item 3's protection is currently the
feature pin alone, with no gate behind it. `security.md` SEC-14 requires the
ban. Filed rather than fixed here — it is outside this ADR's scope.

### D3 — Trust root

`ManualTrustRoot<'a>` (`trust/mod.rs`) is the seam — it is the only
`TrustRoot` implementation that accepts caller-supplied material:

```rust
pub trait TrustRoot {
    fn fulcio_certs(&self) -> Result<Vec<CertificateDer<'_>>>;
    fn rekor_keys(&self)   -> Result<BTreeMap<String, &[u8]>>;
    fn ctfe_keys(&self)    -> Result<BTreeMap<String, &[u8]>>;
}
```

Four sources converge on it: the committed static `trusted_root.json`
(acceptance + self-hosted), `SigstoreTrustRoot` via TUF (public good, `#210`),
the existing offline cache (`#196`, unchanged — it already stores material in
this shape), and `--trust-root`/`--tuf-root` flags. `#196` keeps working
because TUF becomes *one more producer* of `ManualTrustRoot`, never a
replacement for the cache.

`ManualTrustRoot` has all three fields `pub`, and `Verifier::new(cfg, ManualTrustRoot{..})`
compiles on the workspace's **current** feature pin — so **verify against a
self-hosted stack works today**, and offline verify is a `ManualTrustRoot` built
from the cached file. Two constraints bound the TUF half:

- **TUF is public-good only.** `trust/sigstore/constants.rs` is `pub(crate)`, so
  `SigstoreTrustRoot::new()` cannot be pointed at a self-hosted TUF repo. A
  self-hosted operator uses `from_trusted_root_json_unchecked` /
  `from_client_trust_config` — the same static-file path the acceptance suite
  uses. This is why the test stack needs **no** TUF service, which resolves the
  second open question below.
- **`_unchecked` performs no validation**, by its own doc. Treat its input as
  operator-supplied trusted material and say so at the call site.

Temporal handling is inherited, not reimplemented: `fulcio_certs()` passes
`allow_expired = true` (a CA may have been valid when it signed) while tlog
keys must be currently valid.

**One precedence ladder, extending — not replacing —
[`adr_offline_verify_trust_cache.md`](./adr_offline_verify_trust_cache.md).**
Every rung produces the same `ManualTrustRoot`; nothing downstream branches on
which rung won:

| Rung | Source | Produces | Code |
|---|---|---|---|
| 1 | `--tuf-root <path>` / `OCX_SIGSTORE_TUF_ROOT` | `load_trusted_root_json` over a `trusted_root.json` — Fulcio CA **and** a pinned Rekor key. A directory resolves to `<dir>/trusted_root.json` | `trust_resolve.rs:50-56` |
| 2 | `--trust-root <path>` / `OCX_SIGSTORE_TRUST_ROOT` | `load_from_pem` over a Fulcio-CA PEM — no Rekor key | `trust_resolve.rs:59-63` |
| 3 | trust-root cache, **keyed by Rekor authority** | `TrustRootCacheRecord` → Fulcio CA + Rekor key | `trust_resolve.rs:69-71` |
| 4 | offline: hard refusal; online: embedded root + network refresh | `OfflineTrustMaterialUnavailable`, or `TrustRoot::load_embedded` | `trust_resolve.rs:75-81` |

Every rung passes through `enforce_offline_rekor_key` (`trust_resolve.rs:87-95`):
offline verification without a pinned Rekor key is a typed refusal, because the
SET cannot be checked and there is no network to fetch the key.

- **The CLI contract does not move at all.** An earlier draft of this ADR
  proposed widening `--trust-root` to accept a `trusted_root.json`. That proposal
  is **withdrawn as unnecessary**: `--tuf-root` already accepts exactly that file
  (`trust_resolve.rs:50-56`), and it is the rung whose documented purpose is
  "Fulcio CA + pinned Rekor key" — precisely what
  `test/sigstore/generate-trusted-root.py` emits. The two flags are already the
  two forms, and `fake_sigstore.py` confirms the existing tests know it: it
  exposes `trust_root_pem_path` *and* `trusted_root_json_path` as separate
  fixtures. The test stack and the self-hosting documentation therefore use
  `--tuf-root test/sigstore/trusted_root.json`, and **no flag, grammar, exit code
  or JSON shape changes anywhere in this milestone.**
- **The same correction fixes an inverted ladder.** That draft also listed
  `--trust-root` above `--tuf-root` and gave `OCX_SIGSTORE_TRUST_ROOT` the JSON
  handling. Both are wrong: `resolve_trust_root` tests `tuf_override` first, and
  `OCX_SIGSTORE_TRUST_ROOT` is the **PEM** variable while `OCX_SIGSTORE_TUF_ROOT`
  is the JSON one. The table above is transcribed from the function, not from
  memory of it.
- **Cache:** the record gains `ctfe_keys` behind a version bump
  (`TrustRootCacheVersion::V2`). A V1 record is a **miss**, never a partial read
  ([`data-and-formats.md`](../rules/rust-quality/data-and-formats.md)
  DATA-FMT-01/02).
- **Expired TUF metadata with no network:** refuse, with exit 78 and a reason
  naming the expiry and the offline rungs. `SigstoreTrustRoot` enforces this
  itself (`ExpirationEnforcement::Safe`); ocx must not downgrade it to
  `AllowExpired`. Stale trust material is not a fallback.
- **`TrustRoot::load_embedded`** (the `oci/verify/trust_root.rs:130-137` stub)
  is deleted, not filled in: `trust/sigstore/constants.rs` `include_bytes!`-embeds
  the production root, so rung 4 supersedes it. The untyped
  `serde_json::Value` walk at `oci/verify/trust_root.rs:194-270` is replaced by
  the crate's typed `TrustedRoot` parse.

**Why committed test key material cannot reach a production verification path
(S3).** Three independent structural reasons, not a convention:

1. Rungs 1 and 2 require a **caller-named path**. Nothing in the tree points at
   `test/sigstore/` unless a test or an operator writes it on the command line
   or in an environment variable.
2. Rung 3, the only rung that could serve material a caller did not name, is
   **keyed by Rekor authority** (`rekor_cache_key`). A record minted against
   `http://localhost:3000` is not a candidate for a production Rekor; the lookup
   misses and falls through.
3. Rung 4's embedded root is the public-good Sigstore root compiled into the
   `sigstore` crate, which the test material has no way to influence.

The residual risk is an operator who copies the documentation's `--tuf-root`
example verbatim against a production registry — which is a documentation
hazard, not a code one, and is why `test/sigstore/README.md` states the material
is worthless outside this repository.

**OIDC acquisition is ocx-owned.** `sigstore-rs` has **no ambient/CI credential
detection at all** (no `ACTIONS_ID_TOKEN`, no `detect_credential`; interactive
PKCE only, `DEFAULT_REDIRECT_PORT = 8080`). `#194`'s CI story is therefore ocx
code: read `ACTIONS_ID_TOKEN_REQUEST_URL` + `ACTIONS_ID_TOKEN_REQUEST_TOKEN`
(GitHub) or the `id_tokens` job claim (GitLab), exchange over HTTPS, hand the
JWT to the public `IdentityToken::try_from(&str)`. ~30 lines of ordinary HTTP
against a documented endpoint — no crypto, no wire format, and no maintained
Rust crate to delegate it to. This is what makes the GitHub Actions and GitLab
CI documentation examples executable rather than aspirational.

### D4 — SSRF dial-site guard

The sign and verify pipelines call `pull_manifest_raw`, `push_blob` (×2) and
`push_referrer_manifest` **directly on the transport** (`oci/sign/pipeline.rs`,
with a source comment at lines 113-117 conceding the SSRF floor is "never
re-checked here"), so they never reach `Index::guard_physical_dial` — whose
only production call site is `package_manager/tasks/pull.rs:903`. A hostile
local index answering NXDOMAIN at resolve time and a private address at dial
time is admitted for signature traffic.

**Decision: one shared transport seam, not a fourth/fifth/sixth call site.**
Adding the guard per call site is what produced the gap; the next call site
added would reopen it. The guard moves into the transport construction path so
that *every* dial is covered by construction, and the pipelines lose their
direct transport access.

**Exact type and function.** A decorator implementing the existing
`OciTransport` trait, so no call site changes and no pipeline learns about SSRF:

```rust
// NEW FILE crates/ocx_lib/src/oci/client/guarded_transport.rs
pub(in crate::oci) struct GuardedTransport {
    inner: Box<dyn OciTransport>,
    /// logical registry -> permitted physical hosts, from the resolved index
    trusted_hosts: BTreeMap<String, Vec<String>>,
    /// per-process (host, port) memo; a fully warm operation resolves nothing
    seen: Mutex<HashSet<(String, u16)>>,
}

impl OciTransport for GuardedTransport { /* guard, then delegate, per method */ }

// crates/ocx_lib/src/oci/client.rs — the ONE installation point
impl Client {
    pub(crate) fn with_dial_guard(
        self,
        trusted_hosts: BTreeMap<String, Vec<String>>,
    ) -> Self;
}
```

The guard body is the existing `oci::ssrf::resolve_and_validate`
(`oci/index.rs:383`) — no second implementation, no new crate. `Client::transport()`
(`oci/client.rs:182-191`) keeps its signature and now hands back the guarded
decorator, which is why the four direct pipeline calls are covered without
touching `oci/sign/pipeline.rs` or `oci/verify/pipeline.rs` at all.

**Why a decorator and not `ClientBuilder`/`reqwest::dns::Resolve`.** The
`GuardedResolver` hook sees a hostname and nothing else; it cannot know which
*logical* registry that host is standing in for, and `trusted_hosts` is
per-namespace. That asymmetry is exactly why the residual at
`oci/index.rs:370-372` stayed open rather than being fixed inside the resolver.

**Scope, stated so it is not over-claimed.** This closes the *missing-guard*
hole — signature traffic now gets the same check pull traffic has. It does
**not** close the resolve→connect rebinding window; that residual remains, and
the comment at `oci/index.rs:370-372` stays.

#### The Fulcio asymmetry — a constraint delegation *creates*

Registry traffic is the whole of the original finding, but delegating to
`sigstore-rs` opens a second, narrower hole and closes it unevenly:

| Client | Injectable HTTP client? | Consequence |
|---|---|---|
| Rekor | **Yes** — `rekor::apis::configuration::Configuration` has every field public, including `pub client: reqwest::Client` | ocx injects the hardened, timeout-bearing, SSRF-guarded-resolver client. House rules PKG-13/PKG-14/SEC-16/SEC-18 are satisfiable. |
| Fulcio | **No** — `request_cert_v2` constructs `reqwest::Client::new()` internally | No timeouts, no connect bound, and the guarded resolver **cannot reach Fulcio traffic**. Not fixable from outside the crate. Upstream [sigstore-rs#176](https://github.com/sigstore/sigstore-rs/issues/176) is open. |

**Decision: do not delegate the Fulcio client. Delegate Rekor.**

An earlier draft accepted the unguarded Fulcio dial behind three mitigations.
That is overturned, because the trade was mispriced in one direction: it treated
delegation as a simplification that costs a guard, when in fact it costs a guard
and buys nothing.

1. **The code being deleted already exists and already works.**
   `oci/sign/fulcio.rs` is 164 lines that make one JSON POST and parse the
   response. Migration step 6 proposed reducing it to "a thin adapter over
   `FulcioClient::request_cert_v2`" — i.e. replacing working, guarded,
   timeout-bearing code with unguarded, timeout-less code of comparable size.
   That is a net loss with no offsetting gain.
2. **It is not what the non-negotiable is about.** The constraint forbids
   hand-written ASN.1, X.509, Merkle, SCT, TUF and signature code.
   `oci/sign/fulcio.rs` writes none of that — it is an HTTP call against a
   documented JSON API, and the certificate it receives is parsed by `x509-cert`
   either way. Keeping it does not move ocx one line closer to owning crypto.
3. **The hang is worse than the SSRF exposure, and it is unconditional.**
   `reqwest::Client::new()` (`fulcio/mod.rs:210`) carries no request timeout, no
   connect timeout and no read timeout — reqwest's documented defaults. A wedged
   or blackholed Fulcio therefore hangs `ocx package sign` **forever, with no
   exit code**. That violates three MUST-severity house rules at once —
   [`async.md`](../rules/rust-quality/async.md) ASYNC-04,
   [`security.md`](../rules/rust-quality/security.md) SEC-16, and
   [`package-manager-domain.md`](../rules/rust-quality/package-manager-domain.md)
   PKG-13/PKG-14 — and unlike the SSRF exposure it needs no adversary at all. It
   is a flaky network away, for every user, on the tool's slowest command.

So the split is: **Rekor delegated** (its `Configuration.client` is public, so the
hardened client, its timeouts, its retry policy and its guarded resolver all
apply), **Fulcio kept** (one ocx-owned POST on the same hardened client). One
`sigstore-rs` client is used where it can be configured, and not used where it
cannot. Migration step 6 is amended accordingly: `oci/sign/rekor.rs` becomes a
thin adapter, `oci/sign/fulcio.rs` stays.

**What this does not change.** ocx still consumes `sigstore-rs` for everything
the constraint is actually about: bundle construction, `bundle::verify::Verifier`,
`InclusionProof::verify`, `CosignVerificationKey`, the trust-root types. The
delegation this ADR is named for is untouched; only the Fulcio *transport* stays
ocx's.

**Reopen this if** upstream [sigstore-rs#176](https://github.com/sigstore/sigstore-rs/issues/176)
lands and `FulcioClient` accepts an injected client. At that point delegating
costs nothing and the 164 lines can go.

The three mitigations below were written for the accept-and-mitigate path. Two of
them survive the reversal and are kept, because they are good regardless:

1. **Exploit the asymmetry rather than averaging it.** Rekor gets the hardened
   client. Do not weaken Rekor's configuration for symmetry with anything.
2. *(Superseded by the decision above — retained for the reasoning.)*
   **The threat models genuinely differ.** The registry host is *registry- and
   index-supplied* — attacker-influenced, which is what makes
   `guard_physical_dial` load-bearing. The Fulcio URL is **operator-supplied
   configuration** (a flag or config field), not content fetched from a
   registry. An unguarded dial to an operator-named host is a materially
   smaller exposure than an unguarded dial to an index-named one.
3. **Validate the Fulcio URL at config-parse time** — scheme, and the same
   private/loopback/link-local/metadata-range rejection the resolver applies —
   so a hostile *config* is still refused even though the dial itself is
   unguarded. This is a parse-time check on operator input, not a second
   resolver.

Mitigation 3 is now the belt to the decision's braces: the Fulcio dial is
guarded *and* the URL is rejected at parse time. And if a future ocx feature ever
lets a **registry or index** name the Fulcio endpoint, mitigation 2's reasoning
evaporates entirely — which the decision above no longer depends on, but which
the next reader should still know.

### D5 — Bundle correctness and cosign v3 interop (`#197`)

Populate `inclusion_proof` (proven available inline, D1) and keep the v0.3
single-leaf certificate form (oneof form 3) — intermediates come from the trust
root, never the bundle. This is what makes cosign v3 interop (`#197`) testable.

**Emitted shape, field by field.** Media type
`application/vnd.dev.sigstore.bundle.v0.3+json`, unchanged
(`oci/sign/bundle.rs:27`).

| Field | Emitted value | Change from today |
|---|---|---|
| `mediaType` | `…bundle.v0.3+json` | unchanged |
| `verificationMaterial.certificate.rawBytes` | Fulcio **leaf only**, base64 DER | unchanged shape; the leaf now carries a real embedded SCT |
| `verificationMaterial.x509CertificateChain` | **absent** | unchanged — v0.3 uses the single-certificate oneof arm |
| `verificationMaterial.publicKey` | **absent** | unchanged — keyless only |
| `…tlogEntries[0].logIndex` / `logId.keyId` / `integratedTime` | from the Rekor create response | unchanged |
| `…tlogEntries[0].kindVersion` | `{ kind: "hashedrekord", version: "0.0.1" }` | unchanged |
| `…tlogEntries[0].canonicalizedBody` | base64 of the Rekor entry body | unchanged |
| `…tlogEntries[0].inclusionPromise.signedEntryTimestamp` | the real SET from Rekor | value is now a **standard** SET, not `ocx-rekor-set-v1` |
| `…tlogEntries[0].inclusionProof` | `{ logIndex, rootHash, treeSize, hashes[], checkpoint.envelope }` | **newly non-null** (`oci/sign/bundle.rs:66` currently hardcodes `None`) |
| `messageSignature.messageDigest` | `{ algorithm: "SHA2_256", digest }` | unchanged |
| `messageSignature.signature` | ECDSA P-256 over the subject digest | unchanged |
| `dsseEnvelope` | **absent** | unchanged — S1-D, no DSSE in v1 |

**Read-path tolerance, for a cosign-produced bundle.** The verifier must accept
what cosign emits across its version range, and refuse only what it cannot
verify:

- **Media type:** accept `v0.1`, `v0.2` and `v0.3`. Reject an unknown version
  with `BundleParseFailed` — **exit 65**, not 79. `BundleParseFailed` classifies to
  `ExitCode::DataError` (`oci/verify/error.rs:269`), sharing an arm with
  `RekorSetInvalid` and `TransparencyBodyMismatch` precisely so retry logic does
  not fire on a data-integrity failure. 79 is `NotFound`, which is
  `NoSignaturesFound | NoUsableBundle` (`:265`). An earlier draft wrote 79 here;
  in a document whose whole premise is that the exit-code contract is frozen,
  that is the most expensive kind of typo.
- **Amends [`adr_oci_referrers_signing_v1.md`](./adr_oci_referrers_signing_v1.md)
  on the read path.** That ADR chose "bundle v0.3 only" (:386). It rejected
  dual-format on the **write** path (:388), and the write path stays v0.3-only
  here — but accepting v0.1/v0.2 on read is a genuine amendment and is recorded
  as one rather than left to look like an oversight.
- **Certificate arm:** accept **either** `certificate` (v0.3 single leaf) **or**
  `x509CertificateChain` (v0.1/v0.2). When a chain is present, use only its leaf
  and build the path from the trust root — never trust bundle-supplied
  intermediates.
- **Promise vs proof:** proof present, promise absent is the cosign v3 direction
  and **must verify** (Merkle only). Promise present, proof absent is older
  cosign and **must verify** (SET only). Both present: verify both. **Neither is
  a hard failure** — that is the case where nothing binds the entry to the log.
- **Unknown fields are ignored.** No `deny_unknown_fields` on the bundle read
  path; the bundle is a *foreign* producer's document (`data-and-formats.md`
  DATA-FMT-04, tolerant side).
- **`dsseEnvelope` present:** typed refusal `NoUsableBundle`, exit 79. Not a
  parse error — the bundle is well-formed and ocx cannot verify that shape.
- **Digest bytes:** hash what arrived; never re-serialize the bundle before
  verifying (DATA-DIG-04).

Interop is tested in both directions in one run against one trusted root:
`cosign verify` over an ocx bundle, and `ocx package verify` over a cosign
bundle.

### D6 — What stays open, and what closes

**Closes as already-done, both verified against the tree:**

- `#195` — zot is already the referrers-capable primary and `registry:2` the
  permanent negative fixture.
- `#206` — `oci/verify/identity.rs:26` already parses with `x509-cert`'s
  `Certificate::from_der`; the "structural TLV check" premise is stale.
  **But it does not close as-is:** `oci/verify/identity.rs:48-56` reads the
  **deprecated** Fulcio OID `1.3.6.1.4.1.57264.1.1` (issuer v1), while the
  empirical stack (D1) confirms a real Fulcio leaf carries
  `1.3.6.1.4.1.57264.1.8` (issuer v2). Against the fake this never showed;
  against real Fulcio it means the issuer pin in `[[trust.policy]]` reads an
  absent extension. Fix the OID **and add a unit test asserting `.1.8` is the
  one read**, then close.

**Stays open: `#107` (Rekor v2).** `rekor-tiles` exposes no REST surface, only
proto, and `sigstore-rs` 0.14 ships no v2 client — adopting it would mean
hand-rolling one, which is the banned category. That upstream constraint *is*
the issue.

**Discoverability contract for the deferral.** A single grep,
`rg 'ocx-sh/ocx#107'`, must find all three of:

1. the doc comment on `VerifyErrorKind::RekorSetAbsentTsaPresent`, stating that
   this variant is the Rekor-v2/TSA case and why it is a refusal today;
2. a `// TODO(ocx-sh/ocx#107):` at the **single** decision point that rejects an
   RFC 3161 timestamp in place of a SET — one site, not one per caller;
3. the `website/src/docs/in-depth/signing.md` § "Deferred to Future Work" line.

The exit code and message for that path do **not** change: `RekorSetAbsentTsaPresent`
keeps its number and its `error_kind` slug.

## Alternatives considered

| Option | Verdict |
|---|---|
| Keep the Python fake, add SCT minting to it | Rejected. Minting a valid SCT requires a real CT log; the fake would grow into one. It is the 909 lines we are deleting. |
| Legacy CTFE (`ct_server`) instead of TesseraCT | Rejected on hard constraints: no published `createtree` image, distroless CTFE image cannot template a runtime tree ID. |
| `rekor-tiles` (Rekor v2) | Rejected — no REST surface, no client in `sigstore-rs` 0.14. Tracked as `#107`. |
| Call `sigstore_rs::Verifier::verify()` and trust it | Rejected — it silently skips Merkle inclusion and SET verification (`verifier.rs:196-202`). |
| Mint `trusted_root.json` at runtime with `cosign` | Rejected — adds a Go toolchain dependency to the test path for material that can simply be committed. |

## Component contracts

Precise enough for a planner to decompose without re-deriving the design.

```rust
// crates/ocx_lib/src/oci/verify/tlog.rs — the ONE new ocx-owned verification
// module. It exists solely because sigstore-rs `bundle/verify/verifier.rs`
// lines 196-202 are TODOs (sigstore-rs#285). Delete it when they land.

/// Verifies the transparency-log half of a bundle: Merkle inclusion, then the
/// Signed Entry Timestamp. Every primitive is delegated; this function owns
/// ordering and error mapping and nothing else.
///
/// # Errors
/// [`VerifyErrorKind::RekorSetInvalid`] when the SET does not verify against the
/// pinned Rekor key; [`VerifyErrorKind::RekorSetAbsentTsaPresent`] when the entry
/// carries an RFC 3161 timestamp and no SET; [`VerifyErrorKind::BundleParseFailed`]
/// when neither an inclusion proof nor an inclusion promise is present.
pub(super) fn verify_tlog_entry(
    entry: &LogEntry,
    rekor_key: &CosignVerificationKey,
) -> Result<(), VerifyErrorKind>;
```

```rust
// crates/ocx_lib/src/oci/verify/trust_root.rs — reshaped, not deleted.

/// Converts any resolved trust source into the one type sigstore-rs consumes.
/// `ctfe_keys` is newly populated and is what makes SCT verification reachable.
fn into_manual_trust_root(source: TrustSource)
    -> Result<ManualTrustRoot<'static>, TrustRootLoadError>;

/// On-disk format: the offline trust-root cache record.
/// Strict — this binary wrote it and reads it back.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustRootCacheRecord {
    version: TrustRootCacheVersion, // first field, always; V2 adds ctfe_keys
    fulcio_certs: Vec<String>,      // PEM
    rekor_keys: BTreeMap<String, String>,
    ctfe_keys: BTreeMap<String, String>, // NEW in V2
    fetched_at: String,             // RFC 3339, literal Z
}
```

**Verification order is the client spec's and must not be reordered:** establish the
timestamp from Rekor's `integratedTime` **first**, then validate the certificate path
*against that timestamp*, then SCT, then policy, then the transparency-log invariants, then
the signature. `Verifier::verify_digest` owns steps 1–4 and 7; `verify_tlog_entry` owns 5
and 6.

## D2b — The error seam, which delegation does not come with

Delegating to `sigstore-rs` means ocx starts receiving `SigstoreError` values
where it previously produced its own `VerifyErrorKind`. Nothing about the
delegation supplies that mapping, and the default outcome is wrong in a way no
test would catch: an unmapped variant lands in a catch-all `Internal(_)` and
classifies to **exit 1**, so a Rekor transport failure that must be exit 83
(`RekorUnavailable` — the code a caller's `case $? in 83) retry` handler keys on)
would instead look like an ocx bug. Verification still *fails*, so every negative
test stays green while the contract silently breaks.

**Obligation: one exhaustive `From<SigstoreError> for VerifyErrorKind`**, no
wildcard arm, so a `sigstore` upgrade that adds a variant fails the build rather
than the contract. `kind_detail()` is already written this way — "Frozen contract
C-S1-1 … exhaustive match — no wildcard, so adding a variant forces a new arm"
(`oci/verify/error.rs:288-290`) — and the new seam matches it.

The four failure modes this milestone newly makes reachable, and what each maps
to. Two reuse an existing variant; two are appended:

| New failure | Source | Variant | Exit |
|---|---|---|---|
| SCT invalid or absent | `CertificateErrorKind::Sct` (`verifier.rs:167-168`) | **new** `SctInvalid` | 65 |
| Certificate not valid at `integratedTime` | `CertificateErrorKind::Expired` (`verifier.rs:205-219`) | reuse `CertChainInvalid` — it *is* a chain-validity failure | 65 |
| Merkle inclusion or checkpoint mismatch | `InclusionProof::verify` (`inclusion_proof.rs:63`) | **new** `InclusionProofInvalid` | 65 |
| TUF metadata expired, no network | `SigstoreTrustRoot` (`ExpirationEnforcement::Safe`) | reuse `TrustRootLoad(_)` with a new reason | 78 |

Two new variants, therefore two new `kind_detail()` slugs — `sct_invalid` and
`inclusion_proof_invalid`. **This is an addition to the `--format json` contract,
not a change to it**: existing slugs keep their spelling and their exit codes, and
no consumer that does not know the new slugs can observe a difference on any
input that worked before. Recording it here because "no JSON shape changes"
elsewhere in this ADR would otherwise read as "no new slugs", which is not true.

Both new variants classify to 65 rather than to a new code. They are
data-integrity failures on an artifact that arrived intact, which is exactly the
arm `RekorSetInvalid` and `TransparencyBodyMismatch` already share, with the
comment saying why: retry logic must not fire (`oci/verify/error.rs:268-274`).

**One value, not shape, does change.** D6 moves the Fulcio issuer OID from the
deprecated `.1.1` to `.1.8`, which changes what `certificate_oidc_issuer` reports
for certificates a real Fulcio issues. The field, its type and its presence are
unchanged. The move touches four places in one file — the constant
(`oci/verify/identity.rs:20`), two doc comments (`:7`, `:19`), and the assertion
at `:129` — and all four flip in the same commit or the test contradicts the code.

## Measured cost

Counted, not estimated. Production code in the subsystem being restructured:

| Area | Lines | Note |
|---|---|---|
| `oci/sign/` | 2 139 | across 10 files; `pipeline.rs` is 755 of it |
| `oci/verify/` | 3 234 | `pipeline.rs` is 1 583, of which **759 is production and 824 is its own test module** |
| `oci/referrer/` | 811 | `capability.rs` is 693 |
| **Total** | **6 184** | |

The delegation is far more surgical than "restructure sign/verify to delegate"
suggests. Mapping `verify/pipeline.rs` function by function, the functions that
`bundle::verify::Verifier` replaces are:

| Function | Lines | Fate |
|---|---|---|
| `verify_cert_chain` | 30 | dies (`#207`) |
| `verify_signature` | 28 | dies — `Verifier` does it |
| `verify_rekor_set` | 29 | replaced (`#209`) |
| `verify_transparency_body_binding` | 35 | dies — `Verifier` does it |
| `from_bundle` | 50 | shrinks — becomes `ManualTrustRoot` assembly |
| **Deleted or replaced** | **~172** | against roughly 40–60 lines of `Verifier` wiring |

Everything else in that file — `run_inner` (119), `verify_one_referrer` (90),
`list_signature_referrers` (54), `cache_trust_material` (69), the failure
merge/rank helpers (47) — is **ocx orchestration that sigstore-rs does not
do**: referrer discovery, the trust cache, offline policy, failure aggregation.
None of it is touched.

**So the production-code delta is a net deletion on the order of 150 lines**,
plus `oci-rekor-set-v1`'s ~10 lines in `sign/rekor.rs:189-198`, against new
wiring of similar size in `oci/verify/tlog.rs` and the two sign adapters.

**The weight of this milestone is not in the Rust.** It is in the test
infrastructure: seven compose services, four fixtures rebuilt, 79 tests
retargeted across 7 files, 909 lines of fake deleted, and D1a's factory. Any
plan that budgets this as a large production-code change has mis-read it, and
any plan that budgets the test migration as a swap has mis-read it the other
way.

## D1a — The negative tests, which a real stack cannot produce

A real Fulcio will not mint a certificate under a rogue CA on request, and a
real Rekor will not return a corrupted SET. The fake could do both, and **11
tests depend on exactly that**. Step 4 of the migration plan below deletes the
fake; steps 7 and 8 then *gate on* tampered-chain and tampered-SET tests going
red before green. Written naively those two facts contradict each other, so the
resolution is recorded here rather than discovered during implementation.

The 11 tests split into two groups, and only one of them needs new machinery:

**Group A — stack operations, no code (9 tests).**

| Fake control | Tests | Real-stack equivalent |
|---|---|---|
| `set_failure_mode` | `test_sign_rekor_unavailable_exits_83`, `test_verify_rekor_unavailable_exits_83`, `test_offline_auto_verify_with_pinned_material`, `test_offline_auto_verify_bad_policy_still_enforced`, `test_online_verify_populates_cache_then_offline_verify_succeeds`, `test_offline_verify_from_warm_cache_still_enforces_identity`, `test_tuf_root_override_pins_rekor_key_no_fetch`, `test_tuf_root_offline_air_gapped_verify` | **Front Rekor with the toxiproxy already in the tree** (`test/docker-compose.yml:78`, `ghcr.io/shopify/toxiproxy:2.12.0`) and add a per-test toxic. More faithful than the fake's canned 5xx — it exercises connect-timeout and DNS behaviour the fake never had |
| `foreign_oidc_token` | `test_sign_wrong_key_oidc_token_exits_80` | A second `staticClients` entry in `dex-config.yaml`, or a JWT from an issuer absent from `fulcio-config.json`. Fulcio rejects it on its own terms |

**Why toxiproxy and not `docker compose pause rekor`.** The obvious mechanism is
wrong here, and wrong in a way that only shows up under load. `pause` is
**session-global**: it takes Rekor away from every test in the run, and
`verify-basic.yml:168` runs the suite in parallel, so a Rekor-outage test would
fail unrelated signing tests running beside it. The failure is a flake, it is
load-dependent, and it would be diagnosed as a stack-stability problem rather
than as a test-isolation bug. toxiproxy gives a **per-connection** toxic instead,
it is already in `test/docker-compose.yml` under the `bench` profile with a REST
API the harness already drives, and adopting it here costs a profile flag rather
than a new dependency.

**Group B — an adversarial-artifact factory (2 tests).**

`test_verify_invalid_cert_chain_exits_65`,
`test_verify_detects_tampered_rekor_set` and `test_sign_wrong_key_oidc_token_exits_80`
need material no honest service will emit. The third is the cheapest: a second
`staticClients` entry in `dex-config.yaml`, or a JWT from an issuer absent from
`fulcio-config.json`, so Fulcio rejects it on its own terms. The other two What replaces the fake is **not a fake server** — it is a small factory
that takes a *real* bundle produced by the *real* stack and returns a corrupted
variant, plus one helper that mints a leaf under a throwaway untrusted CA:

- `tamper_set(bundle)` — flip a byte in the stored SET and reserialize.
- `bundle_under_rogue_ca(payload)` — mint a CA and a leaf with the `cryptography`
  library and assemble a bundle around them.

Roughly 80–120 lines of *test* code, against a maintained library. This does not
touch the no-hand-rolled-crypto constraint, which governs ocx's production
verification path; `fake_sigstore.py` already builds certificates the same way
(`_build_ca_cert`, `mint_leaf_cert`).

**These tests get stronger, not weaker.** Today they assert that ocx rejects a
*fake's* canned bad response. Afterwards they assert that ocx's real verifier —
now `bundle::verify::Verifier` — rejects a genuinely malformed artifact that is
byte-identical to a valid one everywhere except the corruption. That is the
threat being defended against, and the fake could never express it.

The factory is the *only* part of `fake_sigstore.py`'s 909 lines that survives,
and it survives in a different shape: a bad-artifact builder, not an HTTP server.

## Migration plan

Ordered so the tree is green at every step. **The stack lands before any deletion**, and no
deletion step is reachable until the tests that would catch its regression are running
against real services.

| Step | Change | Gate |
|---|---|---|
| 0 | Walk the [#195](https://github.com/ocx-sh/ocx/issues/195) and [#206](https://github.com/ocx-sh/ocx/issues/206) acceptance criteria against the tree; fix the deprecated Fulcio issuer OID (D6); close both | `task verify` green; a unit test asserts the `.1.8` OID is the one read |
| 1 | Add the seven services, key material, `trusted_root.json` and helper scripts under the `sigstore` profile. **No test consumes them yet** | `docker compose --profile sigstore up -d` reaches healthy for every service; `task test` unchanged and green |
| 2 | **Measure** cold and warm profile bring-up wall-clock and peak memory on an amd64 runner; record in the PR body | A number exists. If cold startup is unacceptable, D1 is reconsidered *here*, before anything depends on it |
| 3 | Teach the bring-up path to request the profile and add the zero-skip assertion (D1); add `test/tests/fixtures/sigstore_stack.py` and the session-scoped sign-once fixture. Fake and real coexist for exactly this one step | Both fixture sets importable; suite green; the zero-skip assertion is shown **red** by temporarily removing the profile flag |
| 4 | Swap the consuming test modules onto the real fixtures; build the **adversarial-artifact factory** and the Group A stack controls (D1a); delete `fake_sigstore.py` and `test_fake_sigstore.py`; update the `test/tests/conftest.py` re-export block | 64 signing tests green against real services; the 20 previously-skipped tests run; **all 11 fault-injection tests from D1a green without the fake** — this gate is what makes steps 7 and 8 reachable |
| 5 | Enable the additional `sigstore` crate features (`sign`, `verify`, `fulcio`, `rekor`, `sigstore-trust-root`, keeping `bundle` + `rustls-tls`); re-confirm the RUSTSEC-2023-0071 ignore with a DEP-08 removal condition | `cargo deny check` clean; `cargo tree -e features -i rustls` shows exactly one crypto provider |
| 6 | Reduce `oci/sign/rekor.rs` to a thin adapter over `entries_api::create_log_entry`, injecting the hardened client into `Configuration.client`. **`oci/sign/fulcio.rs` stays** (D4) — **not** a full delete, because `SigningContext` needs a `pub(crate)` `Keyring` ([sigstore-rs#562](https://github.com/sigstore/sigstore-rs/issues/562)) | Sign acceptance green; the emitted leaf carries an embedded SCT and the bundle a non-null `inclusionProof`; the Rekor adapter injects the shared hardened client (D4) |
| 7 | Delete `verify_cert_chain` and `cert_expired_but_tlog_valid` **only**, leaving `verify_rekor_set` and `verify_transparency_body_binding` untouched (see the ordering invariant below); route certificate verification through `Verifier::verify_digest`. Closes [#207](https://github.com/ocx-sh/ocx/issues/207), [#208](https://github.com/ocx-sh/ocx/issues/208) | Tampered-chain, missing-intermediate and expired-certificate tests shown **red** before green, using D1a's factory rather than the deleted fake |
| 8 | Add `oci/verify/tlog.rs`. Closes [#209](https://github.com/ocx-sh/ocx/issues/209) | Tampered-proof and tampered-SET tests shown **red** before green, using D1a's `tamper_set` rather than the deleted fake |
| 9 | Reshape `trust_root.rs`: typed `TrustedRoot` parse, `ctfe_keys`, cache version bump, `tough`-backed `load_embedded`. Closes [#210](https://github.com/ocx-sh/ocx/issues/210) | Offline verify green from `--tuf-root` and from a fresh cache; expired TUF with no network yields exit 78 |
| 10 | Install the dial guard (D4) | A test asserts a rewritten physical host resolving to loopback is refused on the **sign** path, not only the pull path |
| 11 | cosign interop pass (D5 read-path tolerance), then documentation | `cosign verify` accepts an ocx bundle and `ocx package verify` a cosign bundle, both against one trusted root |

**Ordering invariant between steps 7 and 8 — do not reorder, and do not merge.**
`Verifier` performs no Merkle-inclusion check and no SET check
(`verifier.rs:196-202`, `TODO`s against
[sigstore-rs#285](https://github.com/sigstore/sigstore-rs/issues/285)). Step 7
therefore deletes **only** `verify_cert_chain` and `cert_expired_but_tlog_valid`
— the two things `Verifier` genuinely replaces. It must **not** touch
`verify_rekor_set` or `verify_transparency_body_binding`, which stay live and
unmodified until step 8's `oci/verify/tlog.rs` replaces them in the same commit
that deletes them.

Read loosely, "route verification through `Verifier::verify_digest`" invites
replacing the whole pipeline at step 7, which would leave the tree in a state
where signature verification passes with **no transparency-log verification at
all** — and every acceptance test would stay green, because the tests assert
that valid artifacts verify and invalid ones do not, and a dropped SET check
breaks neither. That is a "green that never ran"
([`quality-core.md`](../rules/quality-core.md)) with a security consequence, and
it is invisible to every gate in the plan. The step-8 gate is what catches it,
which is why the tampered-SET test must be shown **red** first.

Steps 6–9 are individually revertible: each is a deletion plus a delegation, and step 3's
fixtures keep working across all of them.

## NFR coverage

- **Security.** Verification gains certificate-path building, embedded-SCT verification,
  certificate temporal validity against Rekor's integrated time, Merkle-inclusion
  verification and canonical SET verification — closing the omission class behind five of
  the six CVEs in semantics research §9. Signature traffic gains an SSRF dial guard for the
  first time. Trust material that cannot support a required check becomes a typed refusal,
  never a silent partial verify. The resolve→connect rebinding window is explicitly **not**
  closed and is not claimed to be (D4).
- **Operability.** Self-hosting adds no CLI surface: one `trusted_root.json` through the
  existing `--tuf-root`, and the committed acceptance file is the worked example. TUF
  metadata caches under `$OCX_HOME/state/tuf/`, consistent with the `state/` tier. Expired
  metadata with no network is a named reason inside exit 78, not a stale-trust accept and
  not a new exit code. The Rekor tree-churn constraint (D1) is a documented operational
  rule enforced by a suite-level guard.
- **CI cost and latency.** Image footprint is measured (~1.5 GB); bring-up wall-clock is
  **not**, and migration step 2 exists to make it measured before anything depends on it.
  Two structural controls are designed in: the opt-in profile keeps non-signing runs at
  today's cost, and a session-scoped sign-once fixture reduces the Fulcio + Rekor
  round-trip from per-test to per-session.
- **Offline.** Both offline paths survive: `--tuf-root` and the trust-root cache. The cache
  record gains one field behind a version bump, so an old record is a miss rather than a
  partial read. Offline verify continues to require material carrying a Rekor key, and now
  also a CTFE key.
- **Compatibility.** No flag, exit code or `--format json` shape changes; `--trust-root`
  widens its accepted input rather than changing grammar. The one user-visible behaviour
  change is that a **bare CA PEM** cannot support SCT verification against real Fulcio.

Silent by design on: install-path performance, storage layout, index routing semantics and
the package-tier lock — none is touched.

## Documentation surfaces

| Surface | Change |
|---|---|
| `website/src/docs/in-depth/signing.md` § "Current Limitations" | Nine of the eleven bullets are deleted outright; only Rekor v2 and DSSE survive |
| `website/src/docs/in-depth/signing.md` § "Deferred to Future Work" | Reduced to [#107](https://github.com/ocx-sh/ocx/issues/107) and DSSE; the "blocked, not merely unwired" note on [#197](https://github.com/ocx-sh/ocx/issues/197) is removed |
| `website/src/docs/in-depth/signing.md` § "Trust Root" | Rewritten for the ladder in D3 with a real embedded TUF root |
| **New:** self-hosting a Sigstore stack | The committed `test/sigstore/` stack as the executable example, fed to `--tuf-root` |
| **New:** signing against the public-good instance | The zero-configuration path; what the embedded TUF root does |
| `website/src/docs/in-depth/ci.md` | GitHub Actions (`permissions: id-token: write`) and GitLab CI (`id_tokens:` with `aud:`) short examples |
| **New asciicasts** (`task recordings:build`) | Sign against the public-good instance; verify with an identity pin; offline verify from a `trusted_root.json` |
| `.claude/rules/subsystem-oci.md` | Module-map rows for `oci/verify/tlog.rs` and the guarded transport; the `oci/sign/fulcio.rs` and `oci/sign/rekor.rs` rows removed |
| `.claude/rules/arch-principles.md` | `state/tuf/` added to the **State** row |
| `test/README` or `test/sigstore/README.md` | How to bring the profile up and down as a unit, and why (Rekor tree churn) |

`CHANGELOG.md` is **not** a surface: it is generated by git-cliff from commit subjects.

## Consequences

**Positive:**

- The hand-rolled chain walk, temporal check and `ocx-rekor-set-v1` payload are deleted
  outright; `oci/sign/{fulcio,rekor}.rs` collapse to thin adapters. ~1274 lines of Python
  fake deleted.
- 50 of the 64 swapped tests are constant substitutions; 11 need the D1a work. No test becomes newly runnable — there were never any skipped ones.
- Verification is strictly stronger on every axis it touches, and the strengthening is
  test-covered by cases that can be shown red.
- The stack doubles as the executable example for the self-hosting documentation.
- cosign interop becomes testable rather than blocked.

**Negative — what gets worse:**

- The acceptance suite gains a ~1.5 GB, **amd64-only** dependency. On arm64 the Sigstore
  profile does not run without emulation; that is a real regression for anyone on Apple
  Silicon.
- Deleting the fake removes the only Docker-free path to exercising sign or verify. A
  machine without Docker can no longer run those tests at all.
- The Rekor tree-churn rule (D1) makes `docker compose restart rekor` a footgun that
  silently invalidates signatures made earlier in the same run. It is guarded, but it is a
  new rule the harness did not previously have.
- `--trust-root <fulcio.pem>` stops being sufficient to verify a real Fulcio certificate:
  with no CTFE key there is nothing to check the leaf's embedded SCT against. Flags, exit
  codes and JSON shapes are unchanged, and the flag now also accepts a `trusted_root.json`
  — but an invocation that passed a bare PEM and succeeded against the fake now fails with
  exit 78 against real Fulcio.
- ocx owns `oci/verify/tlog.rs` until sigstore-rs#285 lands. Small, but the module a
  security review must read hardest.
- Two bring-up paths now exist for the compose file (`default` and `sigstore`), which is
  exactly how `bench` drifted. The zero-skip assertion in D1 is what keeps them honest.

**Risks:**

- *Risk:* the profile is never requested and the whole signing suite skips green.
  *Mitigation:* the zero-skip assertion (D1) is mandatory and must itself be shown red by
  temporarily removing the profile flag ([`testing.md`](../rules/rust-quality/testing.md)
  TEST-12).
- *Risk:* a silently weaker verify ships because `verify_digest` is assumed complete.
  *Mitigation:* `oci/verify/tlog.rs` is mandatory; tampered-proof and tampered-SET tests
  gate step 8.
- *Risk:* the trust-root cache reads an old record and yields a trust root with no CTFE key.
  *Mitigation:* version-first record, exhaustive match on a closed version enum, unknown
  version is a miss and never a partial read.
- *Risk:* the dial guard resolves on every request and slows the hot path.
  *Mitigation:* per-process `(host, port)` memo, mirroring the existing per-pull
  memoization in `extract_layers`; a fully warm operation resolves nothing.

## Validation

- [ ] `docker compose --profile sigstore up -d` reaches healthy for all seven services
- [ ] Bring-up wall-clock and peak memory measured on an amd64 runner; numbers in the PR body
- [ ] Zero Sigstore acceptance tests skip when the profile is requested; the assertion that
      enforces this has been shown red
- [ ] `test/tests/fixtures/fake_sigstore.py` and `test/tests/test_fake_sigstore.py` are
      absent, with no alias left behind
- [ ] `oci/sign/fulcio.rs` and `oci/sign/rekor.rs` contain no HTTP request
      construction and no signature or wire-format logic of their own
- [ ] `ocx-rekor-set-v1` appears nowhere in the tree
- [ ] Net line count across `crates/ocx_lib/src/oci/{sign,verify}/` is negative
- [ ] Tampered chain, tampered SCT, expired certificate, tampered inclusion proof and
      tampered SET each have a test shown **red** before green
- [ ] An ocx-produced bundle passes `cosign verify`; a cosign-produced bundle passes
      `ocx package verify`; same trusted root, same run
- [ ] A sign-path test asserts a physical host resolving to loopback is refused
- [ ] Offline verify green from `--tuf-root` and from a fresh cache; expired TUF with no
      network yields exit 78
- [ ] Exit codes 83 and 84 keep their numbers; no `--format json` field changes
- [ ] `cargo deny check` clean; exactly one TLS backend in `cargo tree`
- [ ] `CHANGELOG.md` untouched

## Links

- [`research_sigstore_rs_api_surface.md`](./research_sigstore_rs_api_surface.md) — the
  `verify_digest` trace, the `TODO` correction block, the `ManualTrustRoot` seam, feature flags
- [`research_sigstore_verification_semantics.md`](./research_sigstore_verification_semantics.md) —
  bundle v0.3 shape, Fulcio OID table, SET canonicalization, the six-CVE omission table
- [`research_sigstore_selfhost_stack.md`](./research_sigstore_selfhost_stack.md) — §8b the
  Rekor-v1-is-forced verdict, §8c the image table, §3 the `FULCIO_CONFIG` landmine
- [`research_sigstore_current_architecture.md`](./research_sigstore_current_architecture.md) —
  per-file findings and the frozen CLI contract tables
- [`research_sigstore_rs_spike.md`](./research_sigstore_rs_spike.md) — the feature set that
  builds, no Rekor v2, `to_bundle()` emits v0.2
- [`rebase_ledger_signing_and_trust.md`](./rebase_ledger_signing_and_trust.md) — the
  dial-site SSRF residual
- [`adr_oci_referrers_signing_v1.md`](./adr_oci_referrers_signing_v1.md) — amended: driver
  D9, Amendments 4 and 5
- [`adr_offline_verify_trust_cache.md`](./adr_offline_verify_trust_cache.md) — the
  precedence ladder this extends
- [sigstore-rs#285](https://github.com/sigstore/sigstore-rs/issues/285) — the upstream issue
  whose resolution deletes `oci/verify/tlog.rs`

## Open questions

Four questions were raised in drafting. All four are **resolved by evidence**
and recorded below as resolutions rather than deleted, so a reviewer can see
what was asked and what settled it. No `[NEEDS CLARIFICATION]` marker remains;
nothing in this ADR is blocked on an answer from the owner.

**Resolved — arm64.** The profile skips with a *stated reason* naming the
amd64-only Trillian/MySQL images; emulation is refused (Trillian under QEMU
against MySQL is exactly the slow, flaky combination the Decision Drivers price
at ×2). Because a bare skip is indistinguishable from "never ran", the suite
additionally asserts **zero Sigstore skips on amd64**, so the skip path is
reachable only where it is genuinely unavoidable.

**Resolved — `--trust-root`.** Turning a previously-working invocation into an
exit-78 refusal is a CLI **behaviour** change, which the frozen-contract driver
forbids. It is also unnecessary: `SigstoreTrustRoot::from_trusted_root_json_unchecked(&[u8])`
consumes a Sigstore `TrustedRoot` JSON directly. `--trust-root` therefore
**widens its accepted input** — a bare CA PEM (as today) *or* a
`trusted_root.json` — and only the PEM form, which structurally cannot carry CT
log keys, degrades. The flag, its name, and its exit codes are untouched; the
self-hosting documentation shows the JSON form. No grammar change, no new flag.

**Resolved — the `TransparencyLogEntry` reverse adapter is not needed.** The
concern was that `LogEntry::verify_inclusion` consumes the REST `LogEntry`
while a bundle carries the protobuf `TransparencyLogEntry`, and upstream ships
only the forward conversion (`bundle/models.rs:72`). Reading one level down
dissolves it: `rekor::models::InclusionProof` is **fully public**, with a
public `new(log_index, root_hash, tree_size, hashes, checkpoint)` and a public

```rust
pub fn verify(&self, entry: &[u8], rekor_key: &CosignVerificationKey) -> Result<(), SigstoreError>
```

(`rekor/models/inclusion_proof.rs:44,62`) that takes the canonicalised entry
bytes **directly** — which is exactly what the bundle already stores as
`canonicalized_body`. ocx never needs a `LogEntry` at all.

The one remaining seam is `SignedCheckpoint`, whose `decode` is `pub(crate)` —
but its `Deserialize` impl is public and delegates straight to it
(`checkpoint.rs:219-229`), so the checkpoint envelope string parses through
serde:

```rust
let checkpoint: SignedCheckpoint = serde_json::from_value(Value::String(envelope))?;
InclusionProof::new(log_index, root_hash, tree_size, hashes, Some(checkpoint))
    .verify(&canonicalized_body, &rekor_key)?;
```

Total: a `Vec<u8> → [u8; 32]` conversion for `root_hash` and each proof hash,
plus the two calls above — on the order of 15 lines, lossless, and no
hand-rolled Merkle arithmetic. **D2's "one wired call" estimate for #209's
Merkle half stands.**

`#210`'s TUF question is **not** open: D3 establishes that
`trust/sigstore/constants.rs` is `pub(crate)`, so a self-hosted TUF repository
cannot be targeted at all. Static `trusted_root.json` is the self-hosted path by
design, and the test stack needs no TUF service.

## Changelog

| Date | Author | Change |
|---|---|---|
| 2026-08-18 | architect | Initial proposal: D1 real stack behind an opt-in profile with a zero-skip guard, D2 delete-the-clients-own-the-orchestration, D3 one trust-material path with a real TUF root, D4 transport-seam dial guard, D5 bundle shape and cosign read-path tolerance, D6 deferrals and closures |
| 2026-08-18 | orchestrator | Empirical validation of the whole stack (table in Context); image digests pinned; measured Rekor tree churn recorded |
| 2026-08-18 | architect | Named the D1 bring-up mechanism (`test/src/helpers.py:63-69` passes no `--profile` today) and the zero-skip guard; named the D4 type and function (`GuardedTransport` decorator + `Client::with_dial_guard`) and bounded its claim; expanded D5 to a field-by-field emitted shape plus cosign read-path tolerance; added the D6 `#107` discoverability contract and the deprecated-OID sub-finding blocking `#206`; added the D3 precedence ladder; added component contracts, migration plan, NFR coverage, documentation surfaces and validation checklist |
| 2026-08-18 | orchestrator | Reconciled D2/D3/D4 against the **visibility wall** (`research_sigstore_rs_visibility_and_capabilities.md`): signing cannot be delegated (`Keyring` is `pub(crate)`, upstream #562), so `fulcio.rs`/`rekor.rs` shrink rather than delete; #207/#208 are obtained via `Verifier`, not by direct calls; #209's SET half composes two public halves instead of the `cosign`-feature door, which would drag a second `oci-client`; TUF is public-good-only; CI OIDC is ocx-owned. Added the **Fulcio SSRF asymmetry** — `request_cert_v2` builds its own `reqwest::Client`, so delegation removes a guard ocx could otherwise apply |
| 2026-08-18 | orchestrator | Resolved the arm64 and `--trust-root` open questions; `--trust-root` widens its accepted input rather than degrading to exit 78, preserving the frozen CLI contract |
| 2026-08-18 | orchestrator | Corrected D2's `#209` row, which still routed the Merkle half through `LogEntry::verify_inclusion` and an unwritten reverse adapter after the open question had already resolved to `InclusionProof`. Verified against crate source, not docs: `InclusionProof::new`/`::verify` public (`inclusion_proof.rs:45,63`), `verify` covers checkpoint signature + root binding + RFC 6269 inclusion in one call, `entry` takes `canonicalized_body` directly, and `impl Deserialize for SignedCheckpoint` (`checkpoint.rs:220`) is public. No adapter, no hand-rolled Merkle. Estimate stands. |
| 2026-08-18 | orchestrator | **Refuted own claim.** The `cosign`-feature refusal rested on "drags a second `oci-client`", which is false: `[patch.crates-io]` is graph-wide and the fork is `0.17.0`, satisfying sigstore's `^0.17`, so cargo unifies onto our fork. Decision unchanged, justification replaced — added **D2a** with the three real reasons (fork-coupling, duplicate registry layer, `native-tls` feature-pin hazard) and a `deny.toml` SEC-14 side-finding. |
| 2026-08-18 | orchestrator | Verified the visibility wall against crate source. **Holds**, but two citations were imprecise: `verify_sct` and `Keyring` are themselves `pub` and are gated by their enclosing `pub(crate) mod` (`crypto/mod.rs:150`, `:137`), not by their own visibility. Corrected both, and recorded that this makes the upstream unlock a one-word change — worth filing, not on the critical path. |
| 2026-08-18 | orchestrator | **Refined own claim.** "Signing cannot be delegated" was overstated: `SigningContext::async_production()`/`::production()` are public under `sigstore-trust-root`. Corrected to the precise form — delegable *only* against public-good infra, never against a self-hosted stack, because all three constructors either need an unconstructable `Keyring` or hardcode `FULCIO_ROOT`. Decision unchanged and now better supported: taking the delegated path would fork ocx into two signing implementations, so the suite would exercise the path most users do not run. Also noted `production()` `block_on`s its own runtime (ASYNC-08 panic risk). |
| 2026-08-18 | orchestrator | **Plan defect found and fixed.** Step 4 deleted the fake while steps 7 and 8 gated on tampered-chain and tampered-SET tests going red — a capability only the fake had. Measured the blast radius: exactly 11 tests across 4 files use `set_failure_mode`/`set_invalid_chain`/`set_tampered_set`/`foreign_oidc_token`. Added **D1a** splitting them into 9 that become stack operations and 2 that need an ~80-120 line adversarial-artifact factory, and made step 4's gate carry all 11 so steps 7-8 are reachable. |
| 2026-08-18 | orchestrator | Added **Measured cost** — counted rather than estimated. Subsystem is 6 184 production lines; the delegation deletes/replaces ~172 in `verify/pipeline.rs` against ~40-60 of `Verifier` wiring, a net deletion near 150 lines. The milestone's weight is test infrastructure, not Rust. |
| 2026-08-18 | orchestrator | **Withdrew a proposed CLI change and fixed an inverted ladder.** Read `trust_resolve.rs` instead of reasoning from the flag names: `--tuf-root` already accepts `trusted_root.json`, so the proposed `--trust-root` widening is unnecessary and is withdrawn — the CLI contract now moves **not at all**. The draft ladder also had `--trust-root` above `--tuf-root` and gave `OCX_SIGSTORE_TRUST_ROOT` the JSON handling; both inverted. Table retranscribed from the function. Answered **S3** structurally: the cache is keyed by Rekor authority, so the one rung a caller does not name cannot serve test material to a production Rekor. |
| 2026-08-18 | orchestrator | Closed a security-regression window in the migration plan. Step 7 read as "route verification through `Verifier`", which invites deleting `verify_rekor_set` before step 8 replaces it — leaving a tree that verifies signatures with no transparency-log check and stays green, because no existing test distinguishes it. Added an explicit ordering invariant and narrowed step 7's deletion list. |
| 2026-08-18 | orchestrator | Folded in `rev-spec`'s report after verifying all four checkable claims against source. **Retracted the "20 skipped tests become runnable" claim** — a marker scan finds two markers total, neither a skip, and `test_offline_verify.py` has 5 tests so "8 skipped" was impossible; it was the ADR's only unfalsifiable green. **Fixed an exit code**: unknown bundle version is `BundleParseFailed` → 65 (`error.rs:269`), not 79. Adopted the **in-tree toxiproxy** for D1a Group A after `rev-spec` showed `compose pause` is session-global and `verify-basic.yml:168` runs in parallel. Added **D2b**, the `SigstoreError` → `VerifyErrorKind` seam that delegation does not supply — without it a Rekor transport failure exits 1 instead of 83 and every test stays green. Declared the read-path amendment to `adr_oci_referrers_signing_v1.md`. |
| 2026-08-18 | orchestrator | **Overturned D4's Fulcio decision** after verifying both halves of the asymmetry at source (`fulcio/mod.rs:210` bare `Client::new()`; `rekor/apis/configuration.rs:17` `pub client`). Delegating Fulcio was mispriced: it would replace 164 lines of working guarded code with unguarded, timeout-less code of similar size, and `Client::new()` carries no timeout at all — an unconditional hang on `ocx package sign`, violating ASYNC-04, SEC-16 and PKG-13/14, needing no adversary. Rekor is delegated (injectable client); Fulcio's transport stays ocx's. Migration step 6 amended. |
