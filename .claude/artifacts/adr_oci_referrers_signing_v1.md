# ADR: OCI Referrers Signing v1 (Slice 1 — Sign + Verify)

## Metadata

- **Status:** Approved (supersedes verify-only MVI design; amendment to parent ADR)
- **Date:** 2026-04-19
- **Deciders:** OCX core maintainers
- **Issue:** [#24 — OCI referrers API for signature and SBOM discovery](https://github.com/ocx-sh/ocx/issues/24)
- **Supersedes:** n/a (new decision; related `adr_oci_referrers_discovery.md` deferred to Slice 2)
- **Superseded-by:** n/a

### Amendment Log

- **2026-04-19 (R1 fix pass):** Incorporates R1 review feedback (Architect F1-F8, Spec-Compliance A1-A10, Researcher A1-A5). Key changes: swap archived `ci-id` for `ambient-id` + inline fallback (Researcher A1); document Rekor v2 SET-absent risk and pin `sigstore = "=0.13"` with v2 incompatibility note (Researcher A2); correct Fulcio endpoint to `/api/v2/signingCert` (Researcher A4); correct DSSE v2 rationale (Researcher A5); tighten fallback-tag defence (Architect F1); introduce `Signer` trait abstraction for OIDC vs. bundle push separation (Architect F2); add `ReferrersUnsupported = 83` and remap network 5xx to 69 per quality-rust-exit_codes.md canonical enum (Architect F4 + resolve flagged 81 question); collapse `signing_context()`/`verify_context()` into a single `online_context()` accessor (Architect F5); justify each new `Sign/VerifyErrorKind` variant (Architect F6); introduce `ClassifyErrorKind` trait to route kinds to exit codes (Architect F7); record sigstore-rs 0.14 upgrade risk (Architect F8); document JSON envelope shape stability + `context` fields (Spec A1, A7); add expanded error-kind inventory (Spec A4); cosign interop pin `>=3.0.6` (Researcher A3).
- **2026-05-14 (R3 fix pass — RCA Cluster B + D):** Adds Amendments 1–7 (see "Amendments — Round-3 Review" section below): B1 transport-accessor pattern (Amendment 1), F1 endpoint cross-subsystem leak (Amendment 2), G1 `state/` fourth tier (Amendment 3), BLOCK-1 Rekor v2 client tracking (Amendment 4), BLOCK-2 TUF rotation SLA (Amendment 5), capability-cache TTL drift (Amendment 6), G3 `Signer::sign` return shape (Amendment 7). Each amendment is append-only — superseded sections remain in place with back-links from the amendment text. Decision log: `.claude/artifacts/review_r3_adr_decisions.md`.
- **2026-05-14 (R3 fix pass — RCA Cluster B follow-up):** Adds Amendment 8 — explicit defer of architect findings F2 (`AmbientProvider::detect()` static-trait blocks heterogeneous dispatch) and F3 (`DispatchingTokenProvider::new(override, no_tty)` constructor narrowness) to v2; v1 token-provider machinery ships unchanged with named v2 design seam (instance-method `AmbientProvider` + `DispatchingTokenProvider::builder()` pattern). Adds a back-ref banner above the §"`Signer` trait abstraction" code block pointing readers to Amendment 7 as the canonical signature for `Signer::sign` (kind-only return); original code block preserved per append-only convention. Decision log: `.claude/artifacts/review_r3_adr_decisions.md` (Amendment 8 entry).

## Amendments — Round-3 Review (2026-05-14)

This section records all changes from the round-3 max-tier review's RCA Cluster B (one-way-door architectural decisions deferred past stub phase) and Cluster D (docs and ADR drift from code without amendment trail). Amendments are append-only: original sections referenced below remain in place; this section is the source of truth for any conflict.

Decision log: `.claude/artifacts/review_r3_adr_decisions.md`.

### Amendment 1 — `Client::transport()` accessor pattern (round-3 review)

**Supersedes:** §"Context injection (Codex finding #2 + Architect F5)" — partial. The `online_context()` accessor returning `&Client` stays. The leak surface is the layer below: `SignContext::transport: &dyn OciTransport`, populated by reading `Client::transport()`.

**Background.** Phase 5b shipped `oci::Client::transport()` as a `pub` accessor returning `&dyn OciTransport`. `SignContext` then stores that reference as a `pub` field of a `pub struct`. Architect B1 finding: this is a one-way door — once any external crate reads `Client::transport()`, removing the accessor is a breaking API change. The accessor exists for one reason: the sign-pipeline state machine needs raw transport calls (PUT manifest, PUT blob, GET referrers) at a layer below `Client`'s facade methods. Three options on the table:

| Option | Shape | Pros | Cons |
|---|---|---|---|
| **(1) Keep `Client::transport()` public** | `pub fn transport(&self) -> &dyn OciTransport`; `SignContext::transport: &dyn OciTransport` public field | Smallest diff; works as written | Locks `OciTransport` shape into public surface; bypasses `Client` facade; verify pipeline would inherit same anti-pattern |
| **(2) `SignContext::new_from_client(&Client)`** | `Client::transport()` becomes `pub(crate)`; `SignContext::new(client)` builds the context, owns transport access internally | Keeps transport private to `oci` module; one entry point per pipeline | Forces `Client` to know about `SignContext` shape — backward dependency (`oci::client` depending on `oci::sign`) |
| **(3) `SignPipeline::run(&Client, options)`** (recommended) | Pipeline takes `&Client` as argument; never constructs a `SignContext` struct; transport access happens inside pipeline as method calls on `&Client` via existing facade or new `pub(crate)` helpers | Boundary clarity: pipeline is the single consumer; symmetric with future `VerifyPipeline::run(&Client, options)`; testable via `Client` mock without exposing transport; aligns with command pattern in `arch-principles.md` | Requires deleting `SignContext` struct (the `pub` field); larger diff than Option 1 |

**Decision: Option (3) — `SignPipeline::run(&Client, options)` pattern. Recommended; awaiting stakeholder confirmation before Phase 5c branch.**

Rationale:

- **Boundary clarity.** `Client` is already the facade in `arch-principles.md`'s Design Principles table. Adding a second public accessor on `Client` that punches through the facade contradicts the rationale for having a facade.
- **Future-symmetry.** Slice 2 verify against external (non-OCX-signed) bundles needs the same shape. Picking the pipeline-as-function pattern now means verify just adds `VerifyPipeline::run(&Client, options)` alongside.
- **Test mockability.** `Client` is already constructed in tests via `ClientBuilder` with `TestTransport`. Pipelines that take `&Client` get the test transport for free without exposing the trait object.
- **YAGNI on `SignContext`.** The struct exists only to bundle five fields the pipeline reads sequentially. A function with named parameters or a private builder achieves the same with no public surface.

**Open question (stakeholder confirmation requested):** Option (3) is recommended on architectural grounds; Options (1) and (2) are smaller diffs. If schedule pressure on Phase 5c trumps boundary purity, Option (2) is the second-best choice — `SignContext` stays but its `transport` field becomes `pub(crate)` and `Client::transport()` is demoted to `pub(crate)` in the same commit. Option (1) is **not** recommended; it locks the trait shape into public surface permanently.

**Phase 5c impact.** With Option (3): delete `pub struct SignContext`; rename `sign/pipeline.rs::Pipeline` to `SignPipeline`; expose `pub async fn SignPipeline::run(client: &Client, options: SignOptions) -> Result<SignedReferrer, SignError>`; demote `Client::transport()` to `pub(crate)`. With Option (2): keep `SignContext` but flip both `transport` field and `Client::transport()` to `pub(crate)`. With Option (1): no code change, document the public surface in `oci/client.rs` doc-comment as "this is a one-way door, see ADR Amendment 1 before adding callers."

### Amendment 2 — `UrlRejection` cross-subsystem import (round-3 review)

**Supersedes:** §"Architecture Decisions" — module map under `oci/sign/` and `oci/verify/`. The `endpoint.rs` module moves up one level.

**Background.** Architect F1 finding: `oci::verify::error` currently imports `UrlRejection` from `oci::sign::endpoint`. This creates a sign → verify dependency on a shared primitive (URL validation rejection type) that is conceptually neither sign-specific nor verify-specific. Both pipelines validate Fulcio / Rekor / TSA URLs identically. Verify also validates `--rekor-url` and (in v2) `--tsa-url`. Leaving the type under `oci::sign::endpoint` means verify must depend on sign, and any reorganisation of sign breaks verify.

**Decision: lift `endpoint` to `oci::endpoint` as a peer module of `sign` and `verify`. Recommended.**

| Option | Shape | Verdict |
|---|---|---|
| **(A) Lift to `oci::endpoint`** (chosen) | New `crates/ocx_lib/src/oci/endpoint.rs` (or `endpoint/` directory if it grows) hosts `UrlRejection`, `validate_sigstore_url`, URL constants. Both `oci::sign::*` and `oci::verify::*` import from `oci::endpoint`. | Single source of truth; no cross-subsystem coupling; future TSA endpoint validation lands in the same module |
| (B) Keep under `oci::sign::endpoint`, document direction | Add a doc-comment "this primitive is shared with `oci::verify`; do not promote `oci::sign::endpoint` into private modules without lifting first." | Documentation drift risk; the next reorg of sign will not consult the doc-comment |
| (C) Duplicate the type | `oci::sign::endpoint::UrlRejection` and `oci::verify::endpoint::UrlRejection` as siblings | Violates DRY for a primitive both pipelines actually share byte-for-byte |

Rationale: Option (A) is the standard "lift shared primitive to peer module" refactor. The doc-comment fix (B) does not address the structural defect — it just describes it. Slice 2 needs the same URL validator for TSA endpoint URLs; pre-emptive lift avoids a second cross-subsystem leak.

**Phase 5c impact.**
- Create `crates/ocx_lib/src/oci/endpoint.rs` (or `endpoint/mod.rs` if it splits per OCX convention of named module files — see `arch-principles.md` Code Style Conventions).
- Move `UrlRejection`, `validate_sigstore_url`, URL constants from `oci/sign/endpoint.rs`.
- Update imports in `oci/sign/*` and `oci/verify/*`.
- Delete `oci/sign/endpoint.rs`.
- Re-export the public types from `oci::endpoint` if external crates (none today) ever need them.

### Amendment 3 — `state/` tier in three-store architecture (round-3 review)

**Supersedes:** §"Capability cache contract (Codex finding #3)" — path location. The TTL contract stays; only the storage path moves out of `~/.ocx/blobs/{registry}/.capabilities.json` into `~/.ocx/state/referrers/<registry>.json`.

**Background.** Architect G1 finding: the capability cache (whether the registry implements OCI Referrers API) is **not** content-addressed and **not** GC-relevant. Storing it under `blobs/` (a content-addressed tier) silently introduces a fourth concept into the three-tier-CAS architecture documented in `arch-principles.md` and `subsystem-file-structure.md`. The fourth tier already exists (the code writes to `state/`); the ADR and the rules need to acknowledge it.

**Decision: document `state/` as a legitimate fourth tier; update `arch-principles.md` + `subsystem-file-structure.md`. Recommended; lowest churn.**

| Option | Verdict |
|---|---|
| **(A) Document `state/` as a fourth tier** (chosen) | Lowest churn; matches what code does today; aligns with the conceptual distinction that capability probes are *registry-scoped runtime state*, not content |
| (B) Relocate capability cache under existing tier (e.g. `blobs/.cache/`) | Stretches `blobs/` semantics to cover non-blob data; reviewers and GC code must learn the exception; the cache is per-registry-host metadata, not per-digest content |

Rationale: capability cache is **not content-addressed** (no digest), **not GC-eligible** (the GC walker scans `packages/refs/blobs/` for live references; capability JSON has no refs), and **not user-visible** (no `ocx` command lists it). Treating it as content while it has none of content's properties is the misclassification. Adding a fourth tier name (`state/`) for ephemeral, registry-scoped runtime state is the correct fix.

**Definitional contract for `state/`:**

- **Layout:** `~/.ocx/state/{subsystem}/{key}.json`. Slice 1 uses `state/referrers/<registry-slug>.json`.
- **Lifetime:** TTL-bound (default 6h — see Amendment 6). Stale entries safe to delete.
- **GC:** not walked. `ocx clean` may add a `--state` flag in v2 to truncate `state/`; v1 ignores.
- **Atomicity:** writes via `tempfile::NamedTempFile::persist` followed by `std::fs::rename` (Windows-safe — see Codex finding #3 fix in same review pass).
- **Not for:** content (use `blobs/`), extracted files (use `layers/`), assembled packages (use `packages/`), persistent metadata mirror (use `tags/`), or install pointers (use `symlinks/`).

**Doc-cross-ref text to add to `arch-principles.md` (Key Concepts table):**

```markdown
| **State** | Ephemeral, registry-scoped or subsystem-scoped runtime state at `state/{subsystem}/{key}.json`; TTL-bound; not GC-walked. Example: `state/referrers/<registry>.json` for OCI Referrers API capability cache. See `adr_oci_referrers_signing_v1.md` Amendment 3. |
```

**Doc-cross-ref text to add to `subsystem-file-structure.md`:**

Under §"Module Map" add a row:

```markdown
| `state_store.rs` (planned, Slice 1) | Ephemeral runtime state per subsystem | `StateStore`, `StateKey` |
```

Under §"FileStructure (composite root)" replace the `pub struct FileStructure` listing with the seven-store form:

```rust
pub struct FileStructure {
    pub blobs: BlobStore,
    pub layers: LayerStore,
    pub packages: PackageStore,
    pub tags: TagStore,
    pub symlinks: SymlinkStore,
    pub temp: TempStore,
    pub state: StateStore,   // NEW — Slice 1; see adr_oci_referrers_signing_v1.md Amendment 3
}
```

Add a new §"StateStore — Ephemeral subsystem state" section between TempStore and "Path Construction" with the definitional contract above.

**Phase 5c impact.** Code already writes to `~/.ocx/state/referrers/<registry>.json` per Codex finding #3 path (the Windows-safe atomic rename fix). Phase 5c adds (a) a thin `StateStore` wrapper under `file_structure/state_store.rs`, (b) the `state` field on `FileStructure`, (c) updates the two rules. No data migration needed (nothing released yet).

### Amendment 4 — Rekor v2 client tracking (round-3 review)

**Supersedes:** §"Risks & Mitigations" — row labelled "Rekor v2 TUF distribution imminent". Original Risks row is retained but augmented by this amendment.

**Background.** Researcher BLOCK-1 finding: ADR Risks row acknowledges Rekor v2 went GA October 2025 and v1 OCX cannot verify v2-only bundles. But the mitigation reads "document this in release notes" with no concrete tracking mechanism. Reviewer's evidence: no open issue on `sigstore/sigstore-rs` tracking Rekor v2 client support exists with OCX named, no internal OCX tracking issue exists referencing the upstream gap, and the ADR Risks text does not name an owner.

**Acknowledgement.** sigstore-rs Rekor v2 support is **not yet released**. The latest sigstore-rs release at ADR amendment time (2026-05-14) is 0.13.0 (October 2024); no 0.14 release is in progress; no Rekor v2 client work is visible on `sigstore/sigstore-rs` issue tracker. OCX v1 cannot verify Rekor-v2-only bundles. The existing `VerifyErrorKind::RekorSetAbsentTsaPresent → ExitCode::RekorUnavailable = 82` mapping is the operator's signal that they've hit this transition.

**Decision: track upstream via a dedicated GitHub issue on both `sigstore/sigstore-rs` and OCX repo. Required action before Phase 5c finalize.**

| Tracking action | Owner | Acceptance |
|---|---|---|
| File / find issue on `sigstore/sigstore-rs` requesting Rekor v2 client surface — link this ADR; subscribe OCX maintainer GitHub handle | OCX core maintainer | Issue URL recorded in this ADR (replace `<TBD-upstream-rekor-v2-issue>` below) |
| File internal OCX issue "Track sigstore-rs Rekor v2 client release" referencing the upstream issue and this ADR Amendment 4 | OCX core maintainer | Issue URL recorded in this ADR (replace `<TBD-internal-issue>` below) |
| Add release-notes copy for OCX v1.x stating "OCX v1 verifies Rekor v1 bundles only; bundles produced by cosign ≥3.0.6 against Rekor v2 require OCX v2" | release workflow | Release notes template updated |
| When sigstore-rs Rekor v2 client lands, reopen this ADR via a v2 amendment | OCX core maintainer | n/a — gated on upstream |

**Placeholders pending stakeholder action:**
- Upstream issue: `<TBD — file at https://github.com/sigstore/sigstore-rs/issues with title "Rekor v2 client support — tracking issue (used by OCX)" and record URL here>`
- Internal issue: `<TBD — file in ocx-sh/ocx with title "Track sigstore-rs Rekor v2 client release for OCX v2" and record URL here>`

**Phase 5c impact.** None to code. Filing the two tracking issues and updating release notes is a non-code task that must be checked off before `/finalize` of Phase 5c. The verification fixture pinning `cosign ≥3.0.6` against Rekor v1 (staging Rekor) remains the v1 interop test boundary.

### Amendment 5 — TUF rotation SLA (round-3 review)

**Supersedes:** §"Risks & Mitigations" — row "TUF root rotation forces forced-upgrade flows." Original Risks row is retained but augmented by this amendment.

**Background.** Researcher BLOCK-2 finding: v1 ADR currently states "v1 embeds TUF root via `sigstore-trust-root = "0.6.4"`; upgrade cadence follows sigstore-rs point releases." This is the trust-root *acquisition* policy, but it does not define an **SLA** for what happens when sigstore rotates a key in the trust root — does OCX v1 silently fail to verify until users upgrade, or does it refresh from TUF at runtime?

**Decision: Option (A) — embedded-only trust root with documented 90-day forced-upgrade window + CI parity check. Recommended for v1 (no network at verify-time = aligns with offline-first principle). Document v2 migration path to Option (B).**

| Option | Shape | Verdict |
|---|---|---|
| **(A) Embedded-only + 90-day SLA + CI parity check** (chosen, v1) | TUF root statically embedded at build time via `sigstore-trust-root = "=0.6.4"`. Documented policy: "OCX must release a point version within 90 days of any sigstore upstream trust-root change containing a root key rotation." CI nightly job verifies the embedded root matches latest published upstream root; opens a tracking issue if drift detected | Zero network at verify-time (offline-first principle, Product Principle 4); deterministic behaviour; failure mode is "old OCX rejects signatures by new keys" — observable and actionable, never silent acceptance |
| (B) Embedded + `from_tuf()` runtime refresh | Embedded root as bootstrap; `verify/trust_root.rs::load()` calls `sigstore::trust::sigstore::SigstoreTrustRoot::from_tuf()` to refresh from `tuf-repo-cdn.sigstore.dev` at first verify call per session; cache refreshed root under `state/trust_root/`; fall back to embedded on TUF unavailability | Self-healing across key rotations; but breaks the offline-first principle for verify; adds TUF dependency surface; cache invalidation is non-trivial; embedded root must still pass the CI parity check (no escape from A's burden) |

Rationale:

- **Offline-first.** Product Principle 4 says "local index never auto-update; populated index + cached object = zero network." Verify reads a referrer manifest (network call already), then the bundle blob (network call already). Adding a third network call to `tuf-repo-cdn.sigstore.dev` is a *new* trusted host, not a recapture of an existing one.
- **Failure mode honesty.** With Option (A), an OCX user verifying a freshly-signed bundle against a rotated key gets `VerifyErrorKind::CertChainInvalid` → exit 65 with the remediation "TUF root out of date." This is correct: the failure is observable, mappable to action, and never silently degrades to "accept anything."
- **CI parity check closes the silent-drift hole.** A nightly OCX CI job re-fetches the upstream sigstore trust-root and compares it to the embedded asset. Drift opens a tracking issue. This is what makes the 90-day SLA enforceable, not aspirational.
- **`EmbeddedAssetMissing` is **not** the production state.** Reviewer flagged this as a current-production concern. Phase 5c must wire the embedded asset before v1 release; this amendment makes that gating explicit.

**90-day SLA contract:**

1. When sigstore upstream publishes a `sigstore-trust-root` release containing a root-key rotation (`root.json` `version` field increments and a key in the active key set changes), OCX maintainers have 90 days to release a point version of OCX containing the updated embedded asset.
2. The nightly CI parity check (described below) opens a tracking issue on day 0 of upstream release.
3. If the 90-day window lapses, the next OCX release notes carry a Block-tier "TUF root out of date" warning.
4. The 90-day clock can be shortened to 7 days if upstream publishes a key rotation as a security advisory (key compromised).

**CI parity check:**

- New GitHub Actions workflow `.github/workflows/sigstore-trust-root-drift.yml` runs on a daily cron.
- Step 1: `cargo run --package ocx_cli -- internal print-embedded-trust-root --json` (new hidden subcommand returning the embedded asset's hash + version).
- Step 2: fetch upstream via `sigstore_root_signing` CLI or equivalent.
- Step 3: diff. If different, file an issue using `gh issue create` with label `sigstore-trust-root-drift`.

**v2 migration path to Option (B):** the `Signer` trait split in §"`Signer` trait abstraction" provides the seam. A v2 `KeylessSigner` can take a `TrustRoot` argument (today implicit, from embedded asset; v2 explicit, with `from_tuf()` constructor). v1 surface unchanged; v2 adds `pub fn TrustRoot::from_tuf(cache: &StateStore) -> Result<TrustRoot, ...>` and a `--refresh-trust-root` flag.

**Phase 5c impact.** Wire the embedded asset (`sigstore-trust-root = "=0.6.4"`). Replace `EmbeddedAssetMissing` runtime check (currently the production state per reviewer's evidence) with a build-time assertion (compile-time include of the trust-root bytes). Add the nightly drift workflow under `.github/workflows/`. Update release notes template with the 90-day SLA line.

### Amendment 6 — Capability-cache TTL (round-3 review)

**Supersedes:** §"Capability cache contract (Codex finding #3)" — TTL paragraph (the line stating "24h in CI mode (detected via `ci-id`); 1h interactive"). Original §"Forward-Compat Hooks for v2" stays unchanged.

**Background.** Reviewer-flagged drift: code currently sets a single flat 6-hour TTL on the capability cache. The ADR / FR-17 currently mandate 24h in CI / 1h interactive. The split was specified before the code was written; it was never implemented; the discrepancy has been Warn-tier across two review rounds.

**Decision: reconcile to code reality. Adopt flat 6h TTL for v1. CI/interactive split deferred to v2.**

Rationale:

- **6h flat is a reasonable middle.** Long enough that CI cold-starts don't reprobe constantly (typical CI cycle is 1-2h, so a 6h cache survives back-to-back runs in the same Actions runner). Short enough that a registry's Referrers API support change (capability acquired or lost) reflects within the same business day.
- **Interactive 1h was over-engineered.** The original 1h interactive ceiling was justified by "interactive users want a fresh probe per session." But the cache is bypassable via the `--no-cache` global flag, and probing on every interactive command is wasteful for a value that changes on the order of months (registries either implement OCI 1.1 Referrers or they don't, and a flip is a feature release on the registry side).
- **Defer split, not delete.** v2 may still split if telemetry shows different access patterns. The deferred state is captured in the v2 forward-compat hooks.

**Updated FR-17 text** (replaces the existing 24h/1h split language wherever it appears in the PRD / FRs / acceptance criteria):

> **FR-17 (revised, v1):** The `/v2/<name>/referrers/` capability probe result is cached at `~/.ocx/state/referrers/<registry-slug>.json` with a flat TTL of 6 hours. The cache is bypassed by the `--no-cache` global flag (which invalidates the capability cache and, when introduced in Slice 2, the referrer-index cache for the invocation). Future work (v2): split the flat TTL into context-aware TTLs (longer CI, shorter interactive) if access-pattern telemetry shows benefit.

**Phase 5c impact.** None to code (code already implements 6h flat). Update PRD `prd_oci_referrers_signing_v1.md` FR-17. Update test that asserts TTL value (currently `6 * 3600` seconds). Update §"Capability cache contract" in this ADR (mark TTL paragraph as superseded with back-link to this amendment).

### Amendment 7 — `Signer::sign` return shape (round-3 review)

**Supersedes:** §"`Signer` trait abstraction (Architect F2)" — the `sign` method signature in the code block.

**Background.** Architect G3 finding: ADR §"`Signer` trait abstraction" specifies `async fn sign(&self, target_digest: &Digest) -> Result<SignedBundle, SignError>`. Phase 5b code shipped `Result<SignedBundle, SignErrorKind>` (kind-only, no `SignError` wrapper). The mismatch is a Warn-tier ADR-code drift item that has been visible across two review rounds.

**Decision: kind-only wins for v1 (current code stays as written). Full error wrapper deferred until first multi-kind context arises.**

| Option | Shape | Verdict |
|---|---|---|
| **(A) Kind-only `Result<SignedBundle, SignErrorKind>`** (chosen, matches code) | The pipeline composes the leaf kind into a richer `SignError` (with identifier + context) at the boundary where it bubbles out of the signer | YAGNI-aligned (only one caller composes); cheaper diff; matches the existing pattern of `IndexImpl` returning leaf errors that the manager wraps |
| (B) Full `Result<SignedBundle, SignError>` (matches ADR) | Signer returns the wrapped error; pipeline forwards as-is | Saves one composition site at the cost of every `Signer` impl having to carry identifier-context plumbing it doesn't otherwise need |

Rationale:

- **Code wins on convention.** The OCX codebase already follows the leaf-kind pattern in `IndexImpl` and `OciTransport` — the leaf trait returns a kind enum, and the surrounding facade composes it into the three-layer error. Forcing `Signer` to return `SignError` would make it the odd one out.
- **No information lost.** Composing identifier-context happens unconditionally one level up (in the pipeline that consumes the `Signer`). The pipeline already has the identifier; the signer never needs to know it.
- **Forward path is additive.** If a v2 multi-signer dispatch (`SignerSet`) needs to bubble pre-composed errors, it can wrap any `Signer` and compose at that point; the trait signature doesn't change.

**Updated method signature** (replaces the corresponding line in §"`Signer` trait abstraction"):

```rust
async fn sign(&self, target_digest: &Digest) -> Result<SignedBundle, SignErrorKind>;
```

The doc-comment for `Signer::sign` adds: "Returns the leaf error kind; callers compose into `SignError` with identifier and context at the pipeline boundary. See `quality-rust-errors.md` three-layer error pattern."

**Phase 5c impact.** None to code (code already returns `SignErrorKind`). Update the §"`Signer` trait abstraction" code block in this ADR to match. Add a doc-comment on `Signer::sign` referencing this amendment.

### Amendment 8 — Token-provider boundary concerns (F2, F3): deferred to v2

**Supersedes:** nothing in v1 surface — this amendment is the explicit defer record for two round-1 architect findings (F2, F3) that round-3 review re-surfaced as undocumented "silent defers" not addressed by Amendments 1–7. The current v1 token-provider machinery (`AmbientProvider::detect()` static-trait method; `DispatchingTokenProvider::new(override_token, no_tty)` constructor) ships as written. This amendment records the design-debt acknowledgement and names the v2 seam where the concerns will be addressed.

**Background.** Round-1 (and re-flagged in round-3 as F2 + F3) architect findings on the token-provider boundary:

- **F2 — `AmbientProvider::detect()` as static-trait method blocks heterogeneous dispatch.** The current `AmbientProvider` trait exposes `detect()` as an associated function (no `self` receiver). This makes the trait useful only as a namespace for implementing types: callers must name each concrete impl (e.g. `AmbientIdProvider::detect()`, `InlineFallbackProvider::detect()`) and cannot iterate over a `Vec<Box<dyn AmbientProvider>>` to dispatch across heterogeneous detection backends. Slice 1 has exactly two backends (`ambient-id` crate + inline env-inspection fallback) and a fixed dispatch order, so the static shape works in practice. The defect surfaces when a third backend lands (e.g., a v2 provider for a CI platform not covered by either current backend) or when test code wants to inject a deterministic detector stub: today both cases require a code change to the dispatch site, not just a new `impl AmbientProvider`.

- **F3 — `DispatchingTokenProvider::new(override_token, no_tty)` constructor is narrow for the state machine the docs claim it owns.** The dispatcher's two-argument constructor encodes the entire dispatch policy positionally: presence of an explicit override token and the `--no-tty` flag. The ADR's S1-C state machine, however, decides among four paths (explicit flag → ambient detect → browser PKCE → hard error) with future v2 extensions implied (device-code flow; `--issuer` / `--audience` overrides; token-file path). Every new dispatch input grows the positional argument list and forces all call sites to update. A builder-shaped constructor (`DispatchingTokenProvider::builder().override_token(...).no_tty(...).build()`) or a configuration struct would absorb future inputs without breaking call sites. Slice 1 has only two inputs so the narrow constructor compiles and works; the constraint shows up at the first v2 extension.

**Decision: defer both F2 and F3 to v2. v1 ships the current shape as written. Phase 5c does not refactor the trait or the constructor.**

Rationale:

- **v1 machinery works.** Two backends, fixed dispatch order, two constructor inputs — the static-trait and positional-constructor shapes are not actively blocking any v1 contract. No v1 test, no v1 caller, and no v1 acceptance criterion requires heterogeneous dispatch or builder ergonomics.
- **Refactor cost is non-trivial.** Flipping `AmbientProvider::detect()` from associated function to instance method is a trait-surface change that touches every impl, every test stub, and the `DispatchingTokenProvider` body. Converting `DispatchingTokenProvider::new(override, no_tty)` to a builder is an API-shape change with the same blast radius. Doing both refactors mid-Phase-5c, while wiring keyless sign + verify and the trust-root embed, multiplies the change set without unlocking a v1 deliverable.
- **Concrete callers haven't surfaced yet.** The architectural defects are real (static dispatch closes the trait; positional constructor closes the call site), but neither has been exercised by a second consumer in v1. The standard YAGNI argument applies: extracting the abstraction now bakes in a shape chosen without a real second use case. v2 brings the second use case (device-code flow, additional CI providers, token-file path) and the refactor will be informed by that surface rather than speculated against it.
- **Decision is explicit, not silent.** The round-3 review's specific complaint was "F2 + F3 not addressed in Amendments 1–7 — silent defer." This amendment converts the silent defer into a documented, dated, stakeholder-visible defer with a named v2 seam (below).

**Migration path — v2 design seam:**

v2 introduces `AmbientProvider` **instance dispatch** + `DispatchingTokenProvider::builder()` pattern with provider injection points. Sketch:

```rust
// v2 sketch — not in v1 surface
pub trait AmbientProvider: Send + Sync {
    async fn detect(&self) -> Result<Option<IdentityToken>, OidcError>;
    fn provider_name(&self) -> &'static str;
}

impl DispatchingTokenProvider {
    pub fn builder() -> DispatchingTokenProviderBuilder { /* ... */ }
}

pub struct DispatchingTokenProviderBuilder {
    override_token: Option<IdentityToken>,
    no_tty: bool,
    ambient_providers: Vec<Box<dyn AmbientProvider>>,  // injectable
    browser_provider: Option<Box<dyn TokenProvider>>,  // injectable
    // v2 additions: device_code_provider, issuer_override, audience_override, token_file_path...
}
```

The v2 seam is additive on top of the v1 surface: `DispatchingTokenProvider::new(override, no_tty)` continues to compile as a thin wrapper over the builder (`Self::builder().override_token(...).no_tty(...).build()`), preserving v1 call sites until v2 deprecates them.

**Phase 5c impact:** **none.** The current `DispatchingTokenProvider::new(override_token, no_tty)` constructor and the current `AmbientProvider::detect()` static-trait shape continue to work unchanged for the v1 surface. No Phase 5c code change. No ADR §"`Signer` trait abstraction" change (the F2 defer applies to `AmbientProvider`, not `Signer`; the §"`Signer` trait abstraction" code block is independently corrected by Amendment 7's back-ref banner). The v2 design work is tracked separately and will be opened as a v2 ADR amendment when a concrete second caller or third backend materialises.

## Relationship to Parent ADR

This ADR implements the Amendment 2026-04-19 to [`adr_oci_artifact_enrichment.md`](./adr_oci_artifact_enrichment.md) which pulled "Phase 5 — Signature Support" into v1 scope. It inherits from the parent ADR:

- **Media type registry** — `application/vnd.dev.sigstore.bundle.v0.3+json` for sign output; `application/vnd.oci.image.manifest.v1+json` with `artifactType` for referrer manifests.
- **Subject-targeting rules** — signatures target the exact per-platform image manifest digest, never the index.
- **Referrers API contract** — `POST /v2/<name>/manifests/<tag-or-digest>` with `subject` set on the manifest body per OCI distribution spec v1.1.
- **Fallback-tag stance** — the parent ADR forbids writing `sha256-<digest>.sig` fallback tags from OCX. This ADR restates and enforces the rule on the push side (S1-F).

This ADR **does not** re-open decisions locked by the parent ADR (artifact enrichment, media-type ownership, subject scope). New decisions here are strictly about signing-side operations.

## Context

OCX produces reproducible binary distributions across OCI registries. Users building CI pipelines on OCX have asked for a supply-chain guarantee: "the binary my `ocx install` pulls is the same binary the publisher pushed, and the publisher's identity is attestable." The industry-standard answer is cosign keyless signing producing Sigstore bundles, discovered via the OCI Referrers API.

The previous iteration of this feature (rejected by the user on 2026-04-18) shipped a verify-only MVI whose only trust-policy level was `skip`. The user labelled it a "half-product": nothing was actually enforced, and users who wanted real signing still had to install `cosign` as a sibling tool. The rejection instructions were explicit: *"every iteration must be a deliverable feature."*

Slice 1 therefore ships the complete signing loop end-to-end:

- `ocx package sign <REFERENCE>` — cosign-compatible keyless signing producing Sigstore bundle v0.3, pushed as an OCI referrer.
- `ocx verify <REFERENCE> --certificate-identity <VAL> --certificate-oidc-issuer <VAL>` — real cosign-compatible verification (Fulcio cert chain + Rekor SET + subject identity match). No `skip` level exists.

Slice 1 explicitly does **not** ship:

- External signature discovery from other tooling (deferred to Slice 2).
- `ocx sbom` or SBOM enrichment (Slice 2).
- DSSE / attest flows (sigstore-rs 0.13 gap — waiting on 0.14+).
- TOML trust-policy file (v1 uses CLI flags; v2 adds TOML with a forward-compat stub reserved in exit code 78 and file-path validation in 79).
- HSM / KMS signing (v2+).
- Notation support (no maintained Rust library).

## Decision Drivers

| Driver | Description |
|---|---|
| **D1 — Complete signing loop** | "Deliverable feature" means: user signs, user verifies, without external tooling. Verify-only or sign-only are rejected. |
| **D2 — Cosign-compatible wire format** | Interoperability: artifacts signed by `ocx package sign` must be verifiable by `cosign verify`, and vice versa. Non-negotiable for adoption in mixed tool-chains. |
| **D3 — CI-first ergonomics** | Primary user is a GitHub Actions job publishing releases. Ambient OIDC detection must work on at least GHA, GitLab, CircleCI, Buildkite, and GCP without manual token wrangling. |
| **D4 — Enforcing verify only** | No `--insecure`/`skip` escape hatch in v1. If a registry lacks Referrers API, verification is a hard error, not a silent pass. Users who need the escape hatch can continue not using `ocx verify`. |
| **D5 — Typed exit codes** | Backend consumers (CI scripts, Bazel rules) need programmatic failure discrimination. Every distinct failure class maps to a distinct sysexits-aligned exit code with a concrete remediation. |
| **D6 — Machine-readable errors** | `--format json` produces a typed error envelope with `schema_version: 1`. No human prose fallback. |
| **D7 — Forward-compat with v2** | v2 will ship TOML trust-policy files and SBOM discovery. v1 must not paint v2 into a corner: schema fields, exit codes, and flag names are reserved now even if unused. |
| **D8 — Three-layer error pattern** | Every new failure mode flows through `Error → PackageError → PackageErrorKind`. No ad-hoc String errors. No `anyhow` in `ocx_lib`. |
| **D9 — Testability without live Sigstore** | CI must not depend on live Fulcio/Rekor. We test against Sigstore staging (`fulcio.sigstage.dev`, `rekor.sigstage.dev`) and pre-generated deterministic fixtures committed under `test/fixtures/signing/`. |

## Industry Context & Research

The decisions below were grounded in six research artifacts already on disk:

| Artifact | Role in this ADR |
|---|---|
| `research_cosign_signing_flow.md` | 15-step signing state machine; sigstore-rs 0.13 surface (Fulcio + Rekor clients present; DSSE unimplemented); ECDSA P-256 + SHA-256 algo choice; referrer manifest shape. |
| `research_cosign_sigstore_notation.md` | Bundle v0.3 vs legacy format; no Rust Notation library; SPDX 2.3 vs 3.0 framing. |
| `research_oidc_cli_flows.md` | Ambient OIDC dispatch (`ambient-id` crate primary + inline fallback after `jku/ci-id` archival on 2026-01-27); browser PKCE via sigstore-rs `OauthTokenProvider`; pre-check error taxonomy. |
| `research_verify_cli_patterns.md` | cosign NDJSON wart (don't repeat); notation trust-policy shape; oras `--distribution-spec` pattern; `schema_version: 1` mandate. |
| `research_oci_referrers_2026.md` | Registry compat matrix (GHCR/Docker Hub lack Referrers API as of 2026-04); per-platform descent mandate; threat model L0–L4. |
| `codex_review_plan_oci_referrers.md` | 9 actionable findings baked in: correct repo paths, Context injection of both `default_index` + `Client`, expanded `ClientError` variants, separate trust-policy parse errors, no `trybuild` as unit test, concrete test fixtures, no `cosign sign` CI dep, explicit JSON error envelope, cache-contract TTL policy. |

Peer-tool references:

- **cosign** (>=2.4) — reference implementation; its CLI shape (`--certificate-identity`, `--certificate-oidc-issuer`) is adopted verbatim for identity-match flags because CI authors already know them.
- **sigstore-rs v0.13** (pinned `=0.13`) — the only maintained Rust library with Fulcio + Rekor client coverage. DSSE gap means we cannot ship attest in v1.
- **oras** — `--distribution-spec` flag pattern influences our spec-version surfacing (we defer to `discover`'s capability cache instead).
- **notation (CNCF)** — inspired our trust-policy shape (deferred to v2 per S1-G).

## Considered Options

### Decision S1-A — Signing algorithm

**Chosen:** ECDSA P-256 + SHA-256 (Fulcio default).

| Option | Pros | Cons |
|---|---|---|
| **ECDSA P-256 + SHA-256** (chosen) | Fulcio issues these certs by default; maximum interoperability with cosign verify; sigstore-rs ergonomic path | Locked to one curve for v1 (v2 can expand) |
| Ed25519 | Faster sign, simpler key handling | Fulcio does **not** issue Ed25519 certs in the public Good instance (2026-04); would force private-CA deployments |
| RSA-2048 / RSA-3072 | Universal HW support | Larger signatures; cosign prefers ECDSA; deprecated in new ecosystems |

Rationale: D2 (cosign-compatible) forces the Fulcio default. No other option survives the interoperability constraint.

### Decision S1-B — Bundle format written on push

**Chosen:** Sigstore bundle v0.3 only (`application/vnd.dev.sigstore.bundle.v0.3+json`).

| Option | Pros | Cons |
|---|---|---|
| **Bundle v0.3 only** (chosen) | Current state-of-the-art; carries cert, signature, and Rekor SET in one blob; cosign ≥2.0 verifies natively | Older cosign (<2.0) cannot verify; forces callers to upgrade |
| Legacy cosign (sha256-\<digest\>.sig tag + tlog lookup) | Universal tooling compatibility | Tag-based fallback contradicts S1-F; adds GC / concurrency complexity |
| Both (legacy + v0.3) | Maximum compat | Doubles the push artifact count; contradicts single-source-of-truth principle |

Rationale: D2 plus parent-ADR fallback-tag ban (see S1-F). Legacy format discovery on *read* is Slice 2's problem (S2-E); on *write*, only v0.3 is correct.

### Decision S1-C — OIDC token acquisition dispatch

**Chosen:** Ambient (`ambient-id` crate — primary; inline ~80-line env-inspection fallback — secondary) → explicit `--identity-token` → Browser PKCE → hard error.

| Option | Pros | Cons |
|---|---|---|
| **Ambient (`ambient-id` + inline fallback) → flag → browser → error** (chosen) | CI just works; interactive laptop flow works; actionable pre-check errors per `research_oidc_cli_flows.md`; survives upstream crate drift | Four-way dispatch needs thorough unit coverage |
| Flag-only (`--identity-token`) | Minimal dispatch logic | CI authors must wire `id-token: write` + token fetch manually on every platform |
| Browser-only | Simplest | Doesn't work in CI |
| Ambient via archived `ci-id` | No extra engineering | **`jku/ci-id` was archived 2026-01-27** — read-only, no CVE response path, non-starter for a security-sensitive OIDC dependency |
| Ambient → flag → device code → error | More interactive surfaces | Device code adds dependency on second crate; Browser PKCE covers the case |

Rationale: D3 plus dependency hygiene. The previously-selected `jku/ci-id` crate was archived on **2026-01-27** (permanently read-only; 3 open issues + 1 open PR will never be merged); depending on an archived crate in a security-sensitive OIDC path is unacceptable. Replacement: `ambient-id` (active; Fedora packaging review underway, RHBZ#2396331) as the primary impl of a local `AmbientProvider` trait, with an inline ~80-line env-inspection fallback impl (covering GHA, GitLab, CircleCI, Buildkite, GCP metadata server) so OCX retains an independent escape hatch if `ambient-id` regresses or diverges from Sigstore ecosystem conventions. See `research_oidc_cli_flows.md` D-OIDC-1 for full trade-off. `sigstore-rs` still ships `OauthTokenProvider` for browser PKCE; they compose cleanly via the `TokenProvider` trait we define.

**State machine:**

```
  ┌────────────────────┐   yes  ┌────────────────────┐
  │  --identity-token  │ ─────► │ use flag token     │
  │     provided?      │        └──────────┬─────────┘
  └─────────┬──────────┘                   │
            │ no                           │
            ▼                              ▼
  ┌─────────────────────┐       ┌────────────────────┐
  │ ambient-id OR       │  yes  │ ambient token ok   │
  │ inline fallback     │ ────► └──────────┬─────────┘
  │ detects CI?         │                  │
  └─────────┬───────────┘                  │
            │ no                           │
            ▼                              ▼
  ┌─────────────────────┐       ┌────────────────────┐
  │  stdin is a TTY &   │  yes  │ browser PKCE flow  │
  │  not in --no-tty?   │ ────► └──────────┬─────────┘
  └─────────┬───────────┘                  │
            │ no                           │
            ▼                              ▼
  ┌──────────────────────┐       ┌──────────────────┐
  │ actionable error     │       │ Fulcio CSR  ...  │
  │ (exit 77)            │       └──────────────────┘
  └──────────────────────┘
```

### Decision S1-D — DSSE attestation signing

**Chosen:** Not in v1. Document v2 path.

| Option | Pros | Cons |
|---|---|---|
| **Not in v1** (chosen) | Ships what sigstore-rs 0.13 supports; honest scope | No `ocx package attest` in v1 |
| Ship DSSE via sigstore-rs fork | Feature parity with cosign attest | Maintaining a fork is a tax; no upstream signing PR exists — a fork would have no convergence path |
| Wait for sigstore-rs 0.14 before shipping any signing | Future-proof | Misses D1's "ship something" — indefinite delay |

Rationale: DSSE signing is not implemented in sigstore-rs 0.13; there is **no upstream tracking issue or signing PR on the sigstore-rs tracker as of 2026-04-19** (latest release is 0.13.0, October 2024 — there is no 0.14 in progress). v2 must re-evaluate DSSE support when and if sigstore-rs 0.14+ ships signing support; until then, shipping signing without DSSE is a real feature, and forking for DSSE is a maintenance trap with no upstream convergence path.

### Decision S1-E — Offline signing

**Chosen:** Rejected. `ocx package sign --offline` exits with a typed error at code 77 (local validation / offline rejected).

| Option | Pros | Cons |
|---|---|---|
| **Reject offline sign** (chosen) | Matches cosign (which also requires Fulcio/Rekor); single code path | Can't pre-stage signatures air-gapped |
| Write sig blob offline, push on next online invocation | Enables air-gap workflow | Doubles state; requires disk-staging protocol; non-goal for v1 |
| Stub sign-offline to produce unsigned bundle | Compiles clean | Produces a bundle the user thinks is signed but isn't — security smell |

Rationale: Offline air-gap signing is a specialized workflow. Users requesting it can re-open in v2; v1 priority is completing the default loop.

### Decision S1-F — Fallback tag on push

**Chosen:** Never written on push. No `.sig` tag, no index stub. Enforced by both the push code path (`oci/sign/pipeline.rs`) and a unit test that asserts the push sequence emits exactly one referrer manifest and zero `sha256-*.sig` tag writes; the `TestTransport` records every `PUT /manifests/<tag>` call, and the test fails if a tag of shape `sha256-<hex>.sig` or `sha256-<hex>.att` appears in the recorded tape.

| Option | Pros | Cons |
|---|---|---|
| **Never write fallback tag** (chosen) | Single source of truth (referrer API); matches parent-ADR stance; no GC ambiguity; enforced by test tape inspection | Registries without Referrers API can't have signatures pushed by OCX (hard error: `ReferrersUnsupported` → exit 83) |
| Always write fallback tag | Works on legacy registries | Creates concurrent-write races; requires GC protocol for orphan .sig tags; contradicts parent ADR |
| Write fallback tag only when registry lacks Referrers API | Compat with legacy | Push path bifurcates; capability-cache drift creates silent mode switches |

Rationale: Parent-ADR ruling (single-source-of-truth) plus the fact that GHCR and Docker Hub, despite lacking Referrers API, are the primary adoption targets — we need the hard-error path to force registries to adopt Referrers API rather than papering over the gap. Slice 2 will still *read* legacy tag-based signatures other tools have written; `ocx package sign` only ever produces v0.3 referrers. **Enforcement:** Architect F1 demands the test-tape assertion above (no tag of shape `sha256-<hex>.sig|.att` in the recorded writes) to lock this down as a structural invariant, not a reviewer-checked convention.

### Decision S1-G — Trust-policy shape for verify (v1)

**Chosen:** CLI flags only: `--certificate-identity <VAL>` and `--certificate-oidc-issuer <VAL>`. Both required. No TOML file. No `--insecure-ignore-tlog`. No `skip` level.

| Option | Pros | Cons |
|---|---|---|
| **Flags only, both required** (chosen) | Zero config surface; impossible to silently accept anything; matches cosign's flag names | Can't express "accept any of N identities"; that's a v2 concern |
| TOML policy file (v1) | Composable, matches notation | Doubles v1 scope; user already asked us to ship signing in v1, not policy-management |
| Allowlist of OIDC issuers, no identity check | Quick to ship | Defeats the purpose — any GitHub user could impersonate anyone else |
| `skip` level (previous half-product) | Low implementation cost | **Rejected by user on 2026-04-18** as "half-product" — nothing enforced |

Rationale: The v1 interaction is: "sign with my CI identity, verify that exact identity." A single flag pair covers 95% of CI use cases. v2 can layer TOML on top without breaking the flag surface (flags override file). Exit code 78 is reserved now for "trust-policy parse error"; exit code 79 is reserved for "trust-policy file path not found" (see exit-code table below).

### Decision S1-H — Verify re-enforcement level

**Chosen:** Full keyless verification. Every verify invocation:
1. Fetches referrers for the target image manifest.
2. Selects the Sigstore bundle v0.3 referrer by `artifactType`.
3. Validates the Fulcio cert chain against the TUF root (via `sigstore-trust-root` 0.6.4).
4. Validates the Rekor SET (Signed Entry Timestamp) against the Rekor public key.
5. Checks the cert SAN matches `--certificate-identity` (exact match).
6. Checks the cert issuer matches `--certificate-oidc-issuer` (exact match).
7. Verifies the signature over the per-platform image manifest digest.

| Option | Pros | Cons |
|---|---|---|
| **Full keyless verification** (chosen) | No escape hatch; matches cosign strict mode | Fails on registries without Referrers API (hard error by design) |
| Verify signature + cert chain, skip Rekor SET | Works during Rekor outages | SET is the non-repudiation anchor; skipping it degrades the security property to "PKI-valid at signing time" without temporal proof |
| `--insecure-ignore-tlog` flag | Cosign has one | Rejected — this ADR explicitly opposes escape hatches (D4); users who want ignore-tlog can use cosign itself |

Rationale: D4. Rekor availability is a hard dependency (exit code 82 reserved for Rekor-specific unavailability, distinct from registry 5xx at 81).

### Decision S1-I — Re-sign idempotency

**Chosen:** Each invocation writes a new signature as an additional referrer.

| Option | Pros | Cons |
|---|---|---|
| **New referrer each time** (chosen) | Matches cosign behavior; verify just picks the first valid one; no overwrite hazard | Referrer list grows over re-signs; cleanup is GC's job, not sign's |
| Replace existing referrer | Single artifact | Race conditions; which signature "wins" is ill-defined; no precedent |
| Append only if no existing valid signature | Conservative | Defines "valid" at push time when we shouldn't — that's verify's job |

Rationale: Signing is an additive, append-only operation. Historic signatures remain discoverable; operators concerned about proliferation can run registry-side GC.

## Amendment: CLI Placement of `verify` {#cli-placement}

The original proposal placed `verify` as a top-level command (`ocx verify …`). During implementation the command was moved under the `package` subcommand group, becoming `ocx package verify`. The sign command was always `ocx package sign`.

Rationale: both `sign` and `verify` operate at the OCI-tier layer (they consume OCI identifiers directly and never consult `ocx.toml`). Per the layering rule in `subsystem-cli-commands.md`, OCI-tier commands that are naturally paired with another OCI-tier command should share a subcommand group. `package sign` and `package verify` are symmetric in purpose — one attaches a signature, the other checks it — and both target package manifests. Placing them in the same `package` group signals that symmetry to users and keeps the top-level namespace from growing with paired commands.

The asymmetry in the original design (sign under `package`, verify at top-level) was a pre-rebase artifact: `verify` was stubbed at top-level because it felt analogous to `ocx install` (a user-facing safety step) rather than to `ocx package sign` (a publisher step). In practice both are publisher/automation commands operating on specific manifest digests, and the OCI-tier classification governs placement. Moving `verify` to `ocx package verify` aligns with that principle and with `subsystem-cli-commands.md`'s low-level registry command row.

## Exit Code Taxonomy

Extends `quality-rust-exit_codes.md`'s `ExitCode` enum. Values below 64 are shell-reserved; 128+ are signal-derived. New variants required for Slice 1 marked **NEW**; existing variants referenced by number when semantics unchanged.

| Code | Variant | Sysexits name | Semantics (sign / verify) | Actionable advice |
|---|---|---|---|---|
| 0 | `Success` | — | Command succeeded | — |
| 64 | `UsageError` | `EX_USAGE` | Bad CLI invocation (missing required flag, bad identifier format, mutually exclusive flags) | Check `--help`; fix flags |
| 65 | `DataError` | `EX_DATAERR` | Corrupted referrer manifest; malformed bundle; cert chain parse failure | Re-sign source; escalate to publisher |
| 69 | `Unavailable` | `EX_UNAVAILABLE` | Registry unreachable (DNS / connection refused); catch-all for "not online" | Check network / registry URL |
| 74 | `IoError` | `EX_IOERR` | Filesystem I/O during bundle staging; disk full | Check disk / permissions |
| 75 | `TempFail` | `EX_TEMPFAIL` | Registry 429 (rate-limited, honor Retry-After); transient retry-worthy | Retry with backoff (honor Retry-After header) |
| 77 | `PermissionDenied` | `EX_NOPERM` | Registry 403; `--offline` rejected on sign; OIDC pre-check failure (no ambient, no TTY) | Check registry ACL; drop `--offline`; set `ACTIONS_ID_TOKEN_REQUEST_TOKEN` / equivalent |
| 78 | `ConfigError` | `EX_CONFIG` | **NEW-SEMANTIC:** Fulcio 4xx non-401/403 (malformed CSR etc.) **OR** trust-policy parse error (reserved for v2 TOML). Exit 78 used for "the config we built is bad before even hitting the wire" | File a bug if Fulcio rejects; fix trust-policy TOML (v2) |
| 79 | `NotFound` | — | Referrer list empty (no signatures found for target); reserved for v2 "trust-policy file path not found" | Publisher hasn't signed yet; specify correct path |
| 80 | `AuthError` | — | Registry 401; Fulcio 401 (OIDC token rejected) | Refresh registry creds; refresh OIDC token; check issuer URL |
| 81 | `OfflineBlocked` | — | Deliberate `--offline` policy denial on read-side ops. Never used for sign (sign with `--offline` routes to `PermissionDenied = 77` per S1-E). Network 5xx routes to `Unavailable = 69`, not this code. | Drop `--offline` or run online |
| 82 | `RekorUnavailable` | — | **NEW:** Rekor (transparency log) unavailable OR `VerifyErrorKind::RekorSetAbsentTsaPresent` (Rekor v2 transition — see Risks). Distinct from registry 5xx. Fulcio succeeded but Rekor could not be reached or SET could not be validated. | Retry later (Rekor outage); check sigstore status page; if persistent, bundle may be Rekor-v2-only (see Risks — full v2 support deferred until sigstore-rs ships a v2 client) |
| 83 | `ReferrersUnsupported` | — | **NEW:** Registry does not implement the OCI Referrers API and has no fallback-tag referrers index. `ocx package sign` refuses to write (S1-F ban on fallback tags on push); `ocx verify` cannot discover the referrer to verify. Hard error by design (D4) — no silent degradation. | Use a registry implementing OCI Distribution Spec v1.1 Referrers API (ocx.sh default; ghcr.io; Harbor 2.5+; etc.). GHCR/Docker Hub status tracked in `research_oci_referrers_2026.md`. |
| 1 | `Failure` | — | Fall-through for unclassified errors | File a bug with `--log-level=debug` output |

**Rule**: Every new `PackageErrorKind::*` variant added in Slice 1 ships with a test asserting its exit-code classification.

### Exit code 81 conflict resolution (Architect F4)

An earlier draft of this ADR flagged a conflict: the initial design brief had assigned exit 81 to "registry 5xx / network," while `.claude/rules/quality-rust-exit_codes.md` pre-existing `ExitCode::OfflineBlocked = 81` preserves the semantic distinction "user asked for offline, policy denied."

**Resolution (locked):** The canonical `ExitCode` enum wins. Keep `OfflineBlocked = 81` unchanged (it is part of the public exit-code contract documented in `man ocx` and consumed by shell scripts). Route network 5xx to existing `Unavailable = 69`. Add `RekorUnavailable = 82` (new, Rekor-specific) and `ReferrersUnsupported = 83` (new, distinct from generic `Unavailable` because the registry *is* reachable, just missing a capability — the operator's fix is "change registry," not "retry later"). Both new variants are added to `quality-rust-exit_codes.md` in the same R1 pass as this ADR so the canonical enum and this table remain in lockstep.

## Architecture Decisions

### New crate / module shape

Slice 1 adds these modules to `ocx_lib`:

```
crates/ocx_lib/src/
  oci/
    client/
      error.rs                      ← EXPAND (add Unauthorized/Forbidden/RateLimited/ServiceUnavailable/ReferrersUnsupported)
    referrer/                       ← NEW module
      manifest.rs                   ← referrer manifest push/pull shape
      media_types.rs                ← constant table for accepted/written artifactTypes
      capability.rs                 ← 24h TTL cache over `/v2/<name>/referrers/*` probe
    sign/                           ← NEW module
      mod.rs                        ← re-exports; defines Signer trait (Architect F2)
      signer.rs                     ← Signer trait (separates OIDC acquisition from bundle assembly + push)
      oidc.rs                       ← TokenProvider trait + dispatch (AmbientProvider trait with ambient-id primary + inline fallback)
      oidc_ambient.rs               ← ambient-id wrapper (replaces archived ci-id)
      oidc_ambient_inline.rs        ← inline env-inspection fallback (~80 lines: GHA, GitLab, CircleCI, Buildkite, GCP)
      oidc_browser.rs               ← sigstore-rs OauthTokenProvider wrapper
      fulcio.rs                     ← Fulcio client (wraps sigstore-rs FulcioClient::request_cert_v2 → /api/v2/signingCert)
      rekor.rs                      ← Rekor v1 upload client (wraps sigstore-rs); v2 support pending sigstore-rs (see Risks)
      bundle.rs                     ← Sigstore bundle v0.3 construction
      pipeline.rs                   ← push-side state machine (15 steps); implements Signer
    verify/                         ← NEW module
      mod.rs                        ← re-exports
      trust_root.rs                 ← TUF root loader (sigstore-trust-root 0.6.4)
      identity.rs                   ← cert SAN / issuer match
      pipeline.rs                   ← verify-side state machine
      error.rs                      ← VerifyErrorKind taxonomy
```

Slice 1 adds these modules to `ocx_cli`:

```
crates/ocx_cli/src/
  app/
    context.rs                      ← MODIFY (add online_context() accessor — Architect F5 collapses signing_context + verify_context into one; inject both default_index + remote_client)
  command/
    package_sign.rs                 ← NEW (package sign subcommand)
    package.rs                      ← MODIFY (add Sign variant)
    verify.rs                       ← NEW (top-level verify subcommand)
    mod.rs                          ← flat aggregator, no mod.rs (but existing command.rs imports)
  api/
    data/
      signature.rs                  ← NEW (Printable for signature push result)
      verification.rs               ← NEW (Printable for verification result)
    data.rs                         ← MODIFY (add pub mod statements)
  error_envelope.rs                 ← NEW (JSON error envelope with schema_version: 1)
```

### `Signer` trait abstraction (Architect F2)

OIDC token acquisition and bundle assembly/push are orthogonal concerns that must not be hard-wired into a single pipeline function. Separating them now (v1) unlocks HSM/KMS/private-CA signers in v2 without touching the push-side state machine.

> **Note:** the `Signer::sign` signature in this section is **superseded by [Amendment 7](#amendment-7--signersign-return-shape-round-3-review)** — the canonical signature returns `SignErrorKind` (kind-only). The code block below is preserved for historical context per append-only amendment convention.

```rust
// crates/ocx_lib/src/oci/sign/signer.rs (signature only — no body in this ADR)

/// A `Signer` produces a Sigstore-compatible bundle for a given target digest.
/// Implementations encapsulate how a signing identity is established
/// (OIDC keyless, HSM, KMS, long-lived key) and how the Rekor entry is produced.
///
/// Variants:
/// - `KeylessSigner` (v1) — Fulcio keyless flow via sigstore-rs; consumes a TokenProvider.
/// - `KmsSigner`      (v2) — cloud KMS or HSM long-lived key; skips Fulcio.
/// - `PrivateCaSigner`(v2+) — enterprise CA + private TUF root.
///
/// The push-side pipeline (`sign/pipeline.rs`) consumes `&dyn Signer` — never a
/// concrete type. Swapping signers does not require re-plumbing the push code.
pub trait Signer: Send + Sync {
    /// Produces a Sigstore bundle v0.3 for the given target manifest digest.
    async fn sign(&self, target_digest: &Digest) -> Result<SignedBundle, SignError>;

    /// Stable identifier used in telemetry, JSON envelope `context.signer`,
    /// and test-tape assertions. Example values: "keyless-fulcio", "kms-aws-kms",
    /// "private-ca".
    fn signer_kind(&self) -> &'static str;
}

/// OIDC token acquisition is a separate trait: a `Signer` that wants it
/// consumes one, but a KMS signer does not.
pub trait TokenProvider: Send + Sync {
    async fn token(&self, audience: &str) -> Result<IdentityToken, OidcError>;
    fn provider_name(&self) -> &'static str;
}
```

**Why an Option-A trait split (vs. a single `SigningBackend` trait with optional OIDC):** The two responsibilities have incompatible lifetimes — a `TokenProvider` is per-invocation (tokens are short-lived, 10 min) while a signer identity (KMS key, private CA cert) is long-lived. A single trait forces `Option<TokenProvider>` fields in non-OIDC signers, leaking the OIDC abstraction. Split traits keep each impl narrow; the v1 `KeylessSigner` composes them explicitly.

### Context injection (Codex finding #2 + Architect F5)

`Context` must expose a single `online_context()` accessor that injects **both** `default_index` (for tag→digest resolution on the *target* image) **and** `remote_client` (for the referrer push or verify discovery). Previous draft exposed only the client, forcing commands to re-resolve from the identifier — that bypassed the local index cache and would silently ignore `--offline` fallback rules.

**Architect F5 consolidation:** an earlier draft introduced two accessors, `signing_context()` and `verify_context()`. These had identical signatures and identical failure modes (both error on offline). Having two names suggests a distinction that does not exist in v1 and invites divergence. Slice 1 therefore exposes one accessor, `online_context()`, used by both sign and verify commands. Any future divergence (e.g., different offline policy for verify reading a cached bundle) will introduce distinct accessors at that point, not speculatively now (YAGNI).

```rust
// NEW accessor shape (signature only, no body in this ADR)
impl Context {
    /// Returns (index, client) for any flow requiring network. Errors if offline.
    /// Used by both `ocx package sign` and `ocx verify`.
    pub fn online_context(&self) -> ocx_lib::Result<(&oci::index::Index, &oci::Client)>;
}
```

### Expanded `ClientError` variants (Codex finding #4)

`oci/client/error.rs` currently has: `Authentication, DigestMismatch, UnexpectedManifestType, InvalidManifest, ManifestNotFound, BlobNotFound, Registry, Io, Serialization, InvalidEncoding, Internal`.

Slice 1 adds:

```rust
#[non_exhaustive]
pub enum ClientError {
    // existing variants ...

    /// HTTP 401 from registry — credentials missing or invalid.
    Unauthorized { registry: String, source: ...},

    /// HTTP 403 from registry — creds valid but ACL denies.
    Forbidden { registry: String, source: ... },

    /// HTTP 429 from registry — honor Retry-After.
    RateLimited { registry: String, retry_after: Option<Duration>, source: ... },

    /// HTTP 5xx from registry or network error — retry-worthy but not our config.
    ServiceUnavailable { registry: String, source: ... },

    /// Registry returned 404 on /v2/<name>/referrers/ — Referrers API unsupported.
    ReferrersUnsupported { registry: String },
}
```

Each variant has an `impl ClassifyExitCode` returning the code from the taxonomy above. `native_transport.rs` updates its error mapping to emit these variants; `test_transport.rs` gains builder methods to inject each for negative testing.

### `SignErrorKind` and `VerifyErrorKind` — variant inventory & justification (Architect F6 + Spec A4)

Every new kind below is justified by a distinct user-facing remediation *and* a distinct exit code. Variants that would map to identical remediation + exit code are merged. Kinds are `#[non_exhaustive]`.

```rust
// crates/ocx_lib/src/oci/sign/error.rs

#[non_exhaustive]
pub enum SignErrorKind {
    /// Fulcio rejected the CSR (non-401/403) — config-side defect we built a bad
    /// request. Exit 78 (ConfigError). Remediation: file a bug.
    FulcioBadRequest,

    /// Fulcio rejected the OIDC token — issuer mismatch, audience wrong, expired.
    /// Exit 80 (AuthError). Remediation: refresh token, check issuer URL.
    OidcTokenRejected,

    /// Rekor unavailable at time of signing. Exit 82 (RekorUnavailable).
    /// Remediation: retry later; check sigstore status page.
    RekorUnavailable,

    /// Rekor returned the entry but SET could not be extracted or parsed
    /// (e.g., sigstore-rs serialization glitch). Distinct from RekorUnavailable
    /// because the remediation is "file a bug," not "retry." Exit 65 (DataError).
    RekorSetMalformed,

    /// Registry returned 404 on /v2/<name>/referrers/. Exit 83 (ReferrersUnsupported).
    /// Remediation: use a registry with OCI 1.1 referrers.
    ReferrersUnsupported,

    /// OIDC pre-check (expiry, audience) failed client-side — token never sent
    /// to Fulcio. Exit 77 (PermissionDenied). Remediation: per-platform hint
    /// table in oidc.rs (missing GHA id-token:write, GitLab id_tokens, etc).
    OidcPreCheckFailed,

    /// --offline was supplied to `ocx package sign`. Exit 77 (PermissionDenied)
    /// per S1-E (offline sign is rejected outright; mapped to PermissionDenied,
    /// not OfflineBlocked, because the policy rejection is on the *action*, not
    /// a passive network access).
    OfflineSignRefused,

    /// Catch-all for Fulcio/Rekor HTTP errors outside the codes above. Exit 1.
    /// Forced to be a leaf variant — no nested anyhow.
    SigningPipelineInternal,
}

// crates/ocx_lib/src/oci/verify/error.rs

#[non_exhaustive]
pub enum VerifyErrorKind {
    /// No referrers found for target manifest. Exit 79 (NotFound).
    /// Remediation: publisher hasn't signed, or signed a different platform.
    NoSignaturesFound,

    /// Referrer(s) found but none has a recognized Sigstore bundle artifactType.
    /// Exit 79. Remediation: might be a legacy tag-based sig (Slice 2) or a
    /// non-Sigstore attestation.
    NoUsableBundle,

    /// Cert SAN does not match --certificate-identity. Exit 77.
    /// Remediation: verify against the right signer, or the signer is
    /// impersonating.
    IdentityMismatch,

    /// Cert issuer does not match --certificate-oidc-issuer. Exit 77.
    IssuerMismatch,

    /// Cert chain does not verify against TUF root. Exit 65 (DataError).
    /// Remediation: TUF root out of date, or cert is forged.
    CertChainInvalid,

    /// Signature does not verify over subject digest. Exit 65.
    /// Strongest possible failure — bundle contents were tampered with.
    SignatureInvalid,

    /// Rekor SET does not verify against Rekor public key. Exit 82.
    /// Remediation: TUF root out of date, or Rekor key rotated.
    RekorSetInvalid,

    /// NEW (Researcher A2 / Risks): Rekor v2 transition. Bundle has no SET
    /// but has an RFC 3161 TSA timestamp. Exit 82. v1 cannot verify TSA;
    /// full Rekor v2 support deferred until sigstore-rs ships a v2 client.
    /// Remediation: pin cosign ≥3.0.6 or wait for OCX v2 with TSA support.
    RekorSetAbsentTsaPresent,

    /// Registry returned 404 on referrers. Exit 83 (ReferrersUnsupported).
    ReferrersUnsupported,

    /// Rekor unavailable during verify. Exit 82. Distinct from RekorSetInvalid
    /// because retry is appropriate. Remediation: retry later.
    RekorUnavailable,

    /// Bundle parse failed (not v0.3, corrupted JSON). Exit 65.
    BundleParseFailed,
}
```

**Mergers rejected:** `IdentityMismatch` and `IssuerMismatch` share exit code 77 but have distinct remediation ("check who signed it" vs. "check which Fulcio instance issued the cert") — keep both. `RekorSetInvalid` and `RekorSetAbsentTsaPresent` share exit 82 but the first means "Rekor said no" and the second means "we can't ask Rekor v1 and v1 OCX can't ask Rekor v2" — keep both.

### `ClassifyErrorKind` trait (Architect F7)

Routing a kind to an exit code is a single-responsibility operation: the kind *knows* its mapping (it's an invariant of the kind's definition), and the CLI just dispatches. A free function `classify_error(&anyhow::Error)` (per `quality-rust-exit_codes.md`) still owns the outermost dispatch walking `.chain()`, but the leaf-kind lookup lives on the kind itself via a tiny trait:

```rust
// crates/ocx_lib/src/cli/classify.rs  (name subject to builder layout)

pub trait ClassifyErrorKind {
    fn exit_code(&self) -> crate::cli::ExitCode;
}

impl ClassifyErrorKind for SignErrorKind {
    fn exit_code(&self) -> ExitCode {
        match self {
            Self::FulcioBadRequest          => ExitCode::ConfigError,
            Self::OidcTokenRejected         => ExitCode::AuthError,
            Self::RekorUnavailable          => ExitCode::RekorUnavailable,
            Self::RekorSetMalformed         => ExitCode::DataError,
            Self::ReferrersUnsupported      => ExitCode::ReferrersUnsupported,
            Self::OidcPreCheckFailed
            | Self::OfflineSignRefused      => ExitCode::PermissionDenied,
            Self::SigningPipelineInternal   => ExitCode::Failure,
        }
    }
}

impl ClassifyErrorKind for VerifyErrorKind { /* analogous exhaustive match */ }
```

**Why a trait, not a free function per kind:** unit tests assert every kind has a mapping by exercising the trait generically; adding a new kind forces the match to be updated (exhaustive match compile error). This keeps the exit-code contract in lockstep with the kind enum without a separate classification table going stale. The top-level `classify_error(&anyhow::Error)` walks the error chain, downcasts to `SignError` / `VerifyError`, and calls `.kind().exit_code()`.

### JSON error envelope (Codex finding #9 + Spec A1, A4, A7 + C-S1-1 frozen v1 shape)

All `--format json` errors in signing / verify commands produce exactly this shape. The `error` object is nested under the envelope root; `context` is nested under `error`. Top-level keys are strictly `schema_version`, `command`, `exit_code`, `error` (error path) or `schema_version`, `command`, `exit_code`, `data` (success path). No flattening; no alternate top-level arrays like `signed: [...]` or `verified: [...]`.

```json
{
  "schema_version": 1,
  "command": "package sign",
  "exit_code": 80,
  "error": {
    "kind": "auth_error",
    "detail": "oidc_token_rejected",
    "message": "Fulcio rejected OIDC token: issuer not in trust root",
    "remediation": "Verify --certificate-oidc-issuer matches a Fulcio-trusted issuer",
    "context": {
      "identifier": "ocx.sh/cmake:3.28",
      "registry": "ocx.sh",
      "signer": "keyless-fulcio",
      "subject_digest": "sha256:7a9f...",
      "bundle_digest": null,
      "fulcio_url": "https://fulcio.sigstore.dev/api/v2/signingCert",
      "rekor_url": "https://rekor.sigstore.dev"
    }
  }
}
```

**Stability contract (frozen v1 — Spec A1 + C-S1-1).** `schema_version = 1` means the following fields are present on every error envelope written by v1: `schema_version`, `command`, `exit_code`, `error.kind`, `error.message`, `error.context`. `error.detail` and `error.remediation` are optional. The set of legal `error.kind` values is closed and listed in the variant inventory above. Adding a new top-level field is a minor-version bump (consumers tolerant of extra fields continue to work); adding a new `kind` value is a minor-version bump (`schema_version` → `2`); removing or renaming a field or kind is major. v1 is frozen at this shape; Slice 2 reuses the same envelope.

**`error_kind` inventory (stable v1 — Spec A4).** The `error_kind` string column below is the serialized form of the `SignErrorKind` / `VerifyErrorKind` Rust enums from the variant inventory. `error_kind_detail` is a snake_case string derived from the variant name (e.g., `OidcTokenRejected` → `oidc_token_rejected`). Consumers match on `error_kind_detail` for programmatic decisions; `error_kind` is a coarser category for humans.

| Stage | `error_kind` (category) | `error_kind_detail` values (frozen) |
|---|---|---|
| sign | `usage_error` | `missing_required_flag`, `bad_identifier` |
| sign | `config_error` | `fulcio_bad_request`, `trust_policy_parse_error` (v2) |
| sign | `data_error` | `rekor_set_malformed`, `csr_build_failed` |
| sign | `auth_error` | `registry_unauthorized`, `oidc_token_rejected` |
| sign | `permission_denied` | `registry_forbidden`, `oidc_pre_check_failed`, `offline_sign_refused` |
| sign | `not_found` | `target_manifest_not_found` |
| sign | `unavailable` | `registry_unreachable`, `registry_service_unavailable` |
| sign | `temp_fail` | `registry_rate_limited` |
| sign | `rekor_unavailable` | `rekor_down`, `rekor_rate_limited` |
| sign | `referrers_unsupported` | `registry_no_referrers_api` |
| sign | `io_error` | `bundle_write_failed` |
| sign | `internal` | `signing_pipeline_internal` |
| verify | `usage_error` | `missing_required_flag`, `mutually_exclusive_flags` |
| verify | `data_error` | `cert_chain_invalid`, `signature_invalid`, `bundle_parse_failed` |
| verify | `permission_denied` | `identity_mismatch`, `issuer_mismatch` |
| verify | `not_found` | `no_signatures_found`, `no_usable_bundle` |
| verify | `unavailable` | `registry_unreachable` |
| verify | `rekor_unavailable` | `rekor_down`, `rekor_set_absent_tsa_present`, `rekor_set_invalid` |
| verify | `referrers_unsupported` | `registry_no_referrers_api` |
| verify | `internal` | `verify_pipeline_internal` |

**`context` field catalog (Spec A7).** `context` is a JSON object; the set of keys varies by command. Every key is optional; consumers must not assume presence. v1 ships these keys (additive in v2):

| Key | Type | Populated when | Purpose |
|---|---|---|---|
| `identifier` | string | always | `<registry>/<repo>:<tag>` or digest reference the user passed |
| `registry` | string | always | hostname only (e.g., `ghcr.io`) |
| `signer` | string | sign only | Value from `Signer::signer_kind()` (e.g., `keyless-fulcio`) |
| `subject_digest` | string | after step 8 of push pipeline | `sha256:<target-manifest-digest>` being signed over |
| `bundle_digest` | string | after step 13 | `sha256:<bundle-digest>` — null before step 13 |
| `fulcio_url` | string | sign only, after step 6 attempt | Canonical URL used for CSR POST |
| `rekor_url` | string | sign only, after step 10 attempt | Canonical URL used for Rekor upload |
| `referrer_digest` | string | verify only, after referrer selection | Digest of the selected referrer manifest |
| `cert_identity` | string | verify failures on identity match | Actual SAN observed in the cert |
| `cert_issuer` | string | verify failures on issuer match | Actual issuer observed in the cert |

**Printable success shape (Spec A8).** Success responses use the same envelope base (`schema_version=1`, `command`, `exit_code=0`) with the error fields omitted and replaced by `data`:

```json
{
  "schema_version": 1,
  "command": "package sign",
  "exit_code": 0,
  "data": {
    "identifier": "ocx.sh/cmake:3.28",
    "subject_digest": "sha256:7a9f...",
    "bundle_digest": "sha256:c2d0...",
    "referrer_digest": "sha256:e4a1...",
    "signer": "keyless-fulcio",
    "fulcio_cert_serial": "3bd02e...",
    "rekor_log_index": 98765432
  }
}
```

```json
{
  "schema_version": 1,
  "command": "verify",
  "exit_code": 0,
  "data": {
    "subject": "ocx.sh/cmake:3.28",
    "bundle_digest": "sha256:c2d0...",
    "signature_count": 1,
    "signatures": [{
      "signature_format": "sigstore-bundle-v0.3",
      "discovery_method": "referrers-api",
      "certificate": {
        "issuer": "https://token.actions.githubusercontent.com",
        "san": "https://github.com/my-org/my-repo/.github/workflows/release.yml@refs/heads/main",
        "not_before": "2026-04-19T11:55:00Z",
        "not_after": "2026-04-19T12:05:00Z"
      },
      "rekor": {
        "log_index": 98765432,
        "integrated_time": "2026-04-19T12:00:00Z",
        "log_id": "..."
      }
    }]
  }
}
```

**Frozen v1 success contract (C-S1-1).** For `ocx verify` of an OCX bundle, `data` contains: `subject`, `bundle_digest`, `signature_count`, and a `signatures` array. Each `signatures[]` element contains `signature_format`, `discovery_method`, `certificate` (with `issuer`, `san`, `not_before`, `not_after`), and `rekor` (with `log_index`, `integrated_time`, `log_id`). Slice 1 emits `signature_count: 1` with a single-element `signatures` array (OCX never emits mixed format on sign). Slice 2 extends this to multi-element arrays when both v0.3 and legacy `.sig` signatures are discovered during external verify.

### Capability cache contract (Codex finding #3)

The `/v2/<name>/referrers/` probe result (supported / unsupported) is cached at `~/.ocx/blobs/{registry}/.capabilities.json` with:

- **TTL:** 24h in CI mode (detected via `ci-id`); 1h interactive.
- **Bypass:** `--no-cache` global flag invalidates **both** the capability cache and the referrer-index cache for the invocation (Slice 2 adds the referrer-index cache; contract is reserved now).
- **Write:** atomic rename from `.capabilities.json.tmp`.
- **Read-on-start:** fail-open (missing file = "unknown, probe").

Exit-code implications: cache returning "unsupported" for this registry on `ocx package sign` (or `ocx verify`) produces `ReferrersUnsupported` → exit **83** (distinct from `Unavailable = 69`; the registry is reachable but does not implement the capability — the remediation is "change registry," not "retry later"). See exit-code table above.

## Referrer manifest shape on push

```json
{
  "schemaVersion": 2,
  "mediaType": "application/vnd.oci.image.manifest.v1+json",
  "artifactType": "application/vnd.dev.sigstore.bundle.v0.3+json",
  "config": {
    "mediaType": "application/vnd.oci.empty.v1+json",
    "digest": "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a",
    "size": 2
  },
  "layers": [
    {
      "mediaType": "application/vnd.dev.sigstore.bundle.v0.3+json",
      "digest": "sha256:<bundle-digest>",
      "size": <bundle-size>
    }
  ],
  "subject": {
    "mediaType": "application/vnd.oci.image.manifest.v1+json",
    "digest": "sha256:<target-image-manifest-digest>",
    "size": <target-size>
  },
  "annotations": {
    "org.opencontainers.image.created": "2026-04-19T12:00:00Z",
    "dev.ocx.sign.tool-version": "ocx 0.X.Y"
  }
}
```

Push sequence (`sign/pipeline.rs`):

1. Resolve identifier via `default_index` (respecting `--offline` = hard error on sign per S1-E).
2. Fetch per-platform image manifest digest (sign targets a single platform per invocation; multi-platform sign is iteration over this flow).
3. Acquire OIDC token per S1-C dispatch.
4. Generate ephemeral ECDSA P-256 keypair.
5. Build CSR (X.509 PKCS#10) over the OIDC subject.
6. POST to Fulcio (`https://fulcio.sigstore.dev/api/v2/signingCert` via sigstore-rs `FulcioClient::request_cert_v2`, or staging `https://fulcio.sigstage.dev/api/v2/signingCert` per S1-J / test mode). Note: Fulcio v1beta is deprecated; sigstore-rs 0.13 routes to v2 at runtime. ADR documents v2 explicitly to prevent future builders from hand-rolling the v1 URL.
7. Receive short-lived cert chain.
8. Compute `subject-digest = sha256(target-image-manifest bytes)`.
9. Sign `subject-digest` with the ephemeral key.
10. Build Rekor `hashedrekord` entry and POST to Rekor.
11. Extract Rekor SET.
12. Assemble Sigstore bundle v0.3 blob (cert + sig + SET + subject-digest).
13. Compute bundle digest.
14. PUT bundle as blob to `{registry}/{repo}/blobs/`.
15. PUT referrer manifest to `{registry}/{repo}/manifests/<sha256-of-manifest>` with `subject` set.

Failure at any step short-circuits with the appropriate typed error → exit code per table above.

## Not Doing (v1 scope guardrails)

- **`ocx package attest` (DSSE)** — sigstore-rs 0.13 gap. v2.
- **`ocx sbom` read/discovery** — Slice 2.
- **External signature discovery** (cosign legacy `.sig` tag parse, other tools' bundles) — Slice 2 (S2-E).
- **TOML trust policy file** — v2. Exit codes 78 (parse error) and 79 (file not found) reserved.
- **HSM / KMS signing** — v2+. Will layer over the Fulcio path without breaking S1-A.
- **Notation support** — indefinite. No Rust library exists.
- **`--insecure-ignore-tlog`** — explicitly rejected (D4).
- **Offline signing** — explicitly rejected (S1-E).
- **Fallback tag on push** — explicitly rejected (S1-F).
- **Private CA / BYO-trust-root** — TUF-root update flow is Slice-2-or-later; v1 uses the stock `sigstore-trust-root` 0.6.4 TUF root.
- **Signature GC** — registry-side concern; OCX does not prune old referrers.

## Risks & Mitigations

| Risk | Severity | Mitigation |
|---|---|---|
| sigstore-rs 0.13 API churn during v1 development | High | Pin `sigstore = "=0.13"`; wrap all sigstore-rs calls in `oci/sign/fulcio.rs` and `oci/sign/rekor.rs` so the upgrade path is localized |
| Live Fulcio/Rekor unavailable in CI for acceptance tests | High | Use Sigstore staging (`fulcio.sigstage.dev`, `rekor.sigstage.dev`) via env-gated config; pre-generate deterministic bundle fixtures for pure offline paths; **never** invoke `cosign sign` in CI (Codex finding #8) |
| `ci-id` crate doesn't support a CI platform our users need | Medium | Flag `--identity-token` is always available as explicit override; document per-platform token fetch in command-line reference |
| Exit-code conflict (81) with existing `OfflineBlocked` | Medium | Resolution A in this ADR (keep existing semantics, use 69 for network 5xx) — flagged for human review |
| TUF root rotation forces forced-upgrade flows | Low | v1 embeds TUF root via `sigstore-trust-root = "0.6.4"`; upgrade cadence follows sigstore-rs point releases |
| Rekor non-determinism (timestamps) makes bundle fixtures hard to match byte-for-byte | Medium | Fixture strategy is "fields present and well-formed," not bytewise equality; hashing is applied to the bundle *input*, not the Rekor SET |
| Users store credentials in env vars that the ambient detector might leak | High | `ambient-id` and the inline fallback do not log tokens; we assert this in a unit test (negative grep over tracing events in a fixture) covering both the `ambient-id` impl and the inline fallback impl |
| **Rekor v2 TUF distribution imminent (Researcher A2)** — Rekor v2 went GA October 2025 (tiled-log architecture); **v2 entries carry no SET** (integrated_time is 0 and MUST be ignored), clients must use RFC 3161 TSA timestamps from `timestamp.sigstore.dev`. If Sigstore distributes the v2 log URL via TUF before OCX ships, newly-signed bundles will contain no SET and the S1-H verify pipeline (step 4: "Validates Rekor SET") fails. sigstore-rs 0.13 has no Rekor v2 client and no tracking issue exists on sigstore-rs for v2 support as of 2026-04-19. | High | Pin `sigstore = "=0.13"`; for v1 verify pipeline, treat SET as required when present; if a bundle has no SET **and** no RFC 3161 TSA timestamp, fail hard. If a bundle has no SET but **does** have a TSA timestamp, emit a warning and fail (v1 does not ship TSA verification) — reserve distinct `VerifyErrorKind::RekorSetAbsentTsaPresent` for this transition state, mapped to `ExitCode::RekorUnavailable = 82`. Full v2 sign/verify loop is deferred until sigstore-rs gains a v2 client; document this in release notes. Signed bundles produced by OCX v1 continue to target the v1 log instance. Sources: https://blog.sigstore.dev/rekor-v2-ga/ ; https://github.com/sigstore/sigstore-rs/issues/539 |
| **sigstore-rs 0.14 upgrade path (Architect F8)** — when sigstore-rs 0.14 (or later) ships with DSSE signing and/or Rekor v2 client support, OCX needs a deliberate upgrade plan, not a passive `cargo update`. | Medium | Lock `sigstore = "=0.13"` with a `# pinned — see adr_oci_referrers_signing_v1.md Risks` comment in `Cargo.toml`. When a 0.14+ release lands with relevant capability, open a tracking issue referencing this ADR; re-evaluate DSSE (S1-D) and Rekor v2 (Risks row above) as a single coordinated bump. The `Signer` trait (Architect F2) lets us layer new signer impls over the existing Fulcio/Rekor pipeline without breaking S1-A. |

## Forward-Compat Hooks for v2

- **TOML trust policy** — Exit codes 78 (parse) and 79 (file-not-found) reserved.
- **SBOM discovery** — Referrer `artifactType` table (`oci/referrer/media_types.rs`) is a `const` table; Slice 2 adds SPDX/CycloneDX media types without refactoring the push-side.
- **`--insecure-ignore-tlog`** — deliberately absent; adding later is additive, not breaking.
- **Dual-format external-signature verify** — Slice 2 extends `verify/pipeline.rs` with a legacy-tag parse pass; the v0.3 path in v1 is unchanged.
- **HSM / KMS signer** — KMS / HSM signers in v2 implement the `Signer` trait (introduced for v1; see §Architecture Decisions) directly — they own their private key and produce a signature without calling Fulcio, so they are not a sibling to the Fulcio step but an alternative `Signer` implementation alongside `KeylessSigner`.

## References

- `.claude/artifacts/adr_oci_artifact_enrichment.md` §Amendment 2026-04-19 — parent ADR
- `.claude/artifacts/prd_oci_referrers_signing_v1.md` — user-facing scenarios this ADR enables
- `.claude/artifacts/pr_faq_oci_referrers_signing_v1.md` — working-backwards press release
- `.claude/state/plans/plan_slice1_sign_and_verify.md` — runnable implementation plan
- `.claude/artifacts/research_cosign_signing_flow.md`
- `.claude/artifacts/research_cosign_sigstore_notation.md`
- `.claude/artifacts/research_oidc_cli_flows.md`
- `.claude/artifacts/research_verify_cli_patterns.md`
- `.claude/artifacts/research_oci_referrers_2026.md`
- `.claude/artifacts/codex_review_plan_oci_referrers.md`
- `.claude/rules/quality-rust-errors.md` — three-layer error pattern enforcement
- `.claude/rules/quality-rust-exit_codes.md` — sysexits-aligned `ExitCode` enum
- `.claude/rules/subsystem-oci.md` — OciTransport trait, ChainMode, Result\<Option\<T\>\> convention
- `.claude/rules/subsystem-cli.md` + `subsystem-cli-api.md` + `subsystem-cli-commands.md` — Printable trait, single-table rule, typed enums
- [OCI Distribution Spec v1.1](https://github.com/opencontainers/distribution-spec/blob/main/spec.md) — Referrers API
- [Sigstore Bundle v0.3 protobuf](https://github.com/sigstore/protobuf-specs/blob/main/protos/sigstore_bundle.proto)
- [Cosign](https://github.com/sigstore/cosign) — reference implementation for wire-format interoperability
- [sigstore-rs 0.13](https://docs.rs/sigstore/0.13.0/sigstore/) — pinned signing library
- [`ci-id` crate](https://docs.rs/ci-id/) — ambient OIDC detection
