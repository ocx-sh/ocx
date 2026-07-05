# Research: OCI Config Artifact Distribution & Corporate Managed-Config Patterns

**Date:** 2026-07-04
**Sources:** worker-researcher ×2 (`oci-researcher`, `rev-sota`), session `474359d1`
**Domain:** oci, configuration, package-manager, enterprise/managed config
**Triggered by:** `/swarm-plan` for "Corporate Managed Configuration (managed config tier)" — distribute a corporate `config.toml` as an OCI artifact from operator-controlled registries; clients fetch by tag/digest, cache snapshot locally.
**Consumed by:** `.claude/state/plans/plan_managed_config.md` + `.claude/artifacts/adr_managed_config_tier.md`

This artifact persists the verbatim findings of two researcher passes that covered different axes: (1) OCI artifact conventions for distributing a config payload, and (2) a SOTA-gap review of the resulting managed-config plan against current managed/remote-config practice. Preserved as separate sections.

---

## Section 1 — OCI Artifact Conventions for Distributing Configuration Payloads (`oci-researcher`)

**Research axis (question posed):** OCI artifact conventions for distributing configuration payloads. Context: ocx (Rust OCI-based package manager) will distribute a corporate `config.toml` as an OCI artifact from operator-controlled registries (Artifactory/Nexus/Harbor/GHCR compatible); clients fetch by tag or digest, cache snapshot locally. ocx already publishes patch descriptors as OCI artifacts. Report requested on: (1) current best practice 2025-2026 for packaging a single small config file — OCI 1.1 artifact manifest with `artifactType` vs image manifest with custom `config.mediaType` + single layer, and registry compatibility reality; (2) media-type naming conventions + annotations; (3) prior art (Flux OCIRepository, OPA/policy bundles, Helm OCI charts, cosign/sigstore, WASM-to-OCI, ORAS guidance, plus package-manager managed-config prior art: Cachix, Chocolatey, JFrog CLI config import, Renovate presets); (4) pitfalls (tag mutability + caching, digest pinning UX, multi-arch irrelevance, size limits, MIME sniffing).

### Grounding in ocx's existing code

ocx already has two working instances of this exact pattern: `push_description` (README+logo) and `push_patch_descriptor` (`crates/ocx_lib/src/oci/client.rs:1063`, backed by `crates/ocx_lib/src/patch/persistence.rs` and `patch/descriptor.rs`). Both use: a plain OCI *image* manifest (`schemaVersion: 2`, `mediaType: application/vnd.oci.image.manifest.v1+json`), a manifest-level `artifactType` (e.g. `application/vnd.sh.ocx.patch.v1`), an **empty config blob** (`application/vnd.oci.empty.v1+json`, literal `{}`), and exactly one layer with the real payload under a custom vendor media type. Pushed by tag to a reserved internal tag (`__ocx.patch`), never wrapped in an image index, validated on pull by re-hashing before parsing. A config artifact should be a third instance of this, not a new pattern.

### Recommended manifest shape

```json
{
  "schemaVersion": 2,
  "mediaType": "application/vnd.oci.image.manifest.v1+json",
  "artifactType": "application/vnd.sh.ocx.config.v1",
  "config": {
    "mediaType": "application/vnd.oci.empty.v1+json",
    "digest": "sha256:<hash of {}>",
    "size": 2
  },
  "layers": [
    { "mediaType": "application/vnd.sh.ocx.config.v1+toml", "digest": "sha256:<hash of config.toml>", "size": <n> }
  ],
  "annotations": { "org.opencontainers.image.created": "<RFC3339 push timestamp>" }
}
```

- **Set `artifactType`, but keep config empty (not a real custom config blob).** `artifactType` is free to set — every registry checked stores/returns the manifest JSON verbatim. Only read-side tooling lags (Harbor's `/accessories` API and UI don't yet surface real `artifactType`: [goharbor/harbor#21344](https://github.com/goharbor/harbor/issues/21344), [#21345](https://github.com/goharbor/harbor/issues/21345)) — never blocks push/pull. The empty-config marker is the OCI spec's own registered convention ([media-types.md](https://github.com/opencontainers/image-spec/blob/main/media-types.md)), so it can't trip a config-type allow-list the way a custom config type sometimes does (Red Hat Quay historically gated custom OCI config types behind `ALLOWED_OCI_ARTIFACT_TYPES`).
- **Never wrap in an image index** — config has no platform axis. Single manifest at the tag, matching existing ocx artifacts.
- **Don't use `subject`/Referrers API.** ocx resolves by explicit tag already, so it never needs graph-linking. This sidesteps the one genuinely patchy OCI 1.1 feature: Artifactory only added Referrers API support in **7.90.1** ([docs.jfrog.com/artifactory/docs/oci-repositories](https://docs.jfrog.com/artifactory/docs/oci-repositories)); Harbor/Nexus lag similarly. cosign's own fallback for registries lacking Referrers support is a tag-suffix scheme (`sha256-<digest>.sig`) — evidence this graph-linking machinery is exactly the part still worth avoiding.

### Media types & naming

Extend ocx's existing `application/vnd.sh.ocx.<name>.v1` (artifactType) / `application/vnd.sh.ocx.<name>.v1+json` (blob) convention: add `MEDIA_TYPE_CONFIG_V1 = "application/vnd.sh.ocx.config.v1"` and `MEDIA_TYPE_CONFIG_TOML_V1 = "application/vnd.sh.ocx.config.v1+toml"`. Keep raw TOML bytes verbatim in the layer (no TOML→JSON round-trip — preserves operator comments/formatting). Annotate `org.opencontainers.image.created` always; optionally `.source`/`.revision` if a CI pipeline produces the config (mirrors Flux's convention below). `ManifestBuilder::annotations()` already supports a passthrough map for this.

### Prior art

| Project | Shape | Right | Wrong / limiting |
|---|---|---|---|
| [Flux OCIRepository](https://fluxcd.io/flux/cheatsheets/oci-artifacts/) | image manifest + custom config/content media types, no OCI 1.1 features | Broadest documented registry list (Docker Hub, GHCR, GitLab, ACR, ECR, GAR, Harbor, self-hosted); `org.opencontainers.image.*` annotations | No digest-drift signal beyond polling |
| [Helm OCI charts](https://helm.sh/blog/helm-oci-mediatypes/) | image manifest + `cncf.helm.config.v1+json` | GA since 3.8, huge registry support | Quay needed explicit config-type allow-listing historically — the trap the empty-config choice avoids |
| [OPA bundles](https://github.com/open-policy-agent/opa/issues/1413) | 3-layer manifest, real config+manifest+tarball | Clean separation of concerns | Media type family never got one canonical spec — fragmentation risk of rolling your own multi-part scheme |
| [cosign/sigstore](https://docs.sigstore.dev/cosign/signing/other_types/) | `artifactType` + empty config + layer, or legacy `sha256-<digest>.sig` tag fallback | Explicit modern+legacy compatibility strategy | Only needed because it links artifact-to-artifact (subject-graph); ocx's tag-addressed standalone case doesn't need this |
| [WASM-to-OCI](https://github.com/engineerd/wasm-to-oci) / [CNCF Wasm OCI layout](https://tag-runtime.cncf.io/wgs/wasm/deliverables/wasm-oci-artifact/) | custom config+layer media types | Simple, matches ocx's own package-push shape | JFrog needed a dedicated support doc to make Artifactory accept it — even protocol-legal media types sometimes need enterprise-registry reconfiguration |
| [ORAS guidance](https://oras.land/docs/concepts/artifact/) | recommends `artifactType` as the modern preferred path, calls plain custom-config the "prior art" | — | In practice Flux/Helm/OPA still ship the "prior art" pattern because it's broader-compatible; ocx's hybrid (set both) gets both benefits |

**Package-manager config-onboarding UX**: [Cachix](https://docs.cachix.org/getting-started) collapses onboarding to one command (`cachix use mycache`) plus a self-diagnosis command (`cachix doctor`) — worth an `ocx config doctor`-equivalent someday. [Renovate presets](https://docs.renovatebot.com/config-presets/) auto-discover an org-wide config by naming convention (a repo literally named `renovate-config`, or `.github/renovate-config.json`) without the user typing a URL — the closest structural analog to ocx's reserved `global` repo under `[patches] registry`. Could not verify specifics on Chocolatey Central Management or a `jfrog config import` equivalent (searches returned no substantive content) — flagging the gap rather than guessing.

### Pitfalls

- **Tag mutability + caching**: OCI tags are always mutable (ocx's own docs already say this for `[mirrors]`/`[patches]`). Cache by tag but store the resolved digest alongside so a re-fetch can detect drift and log it, same as `ocx index update`.
- **Digest pinning UX**: `Identifier` already handles `@sha256:...` uniformly — zero new parsing needed — but pinning defeats the point of "central corporate config" (a pinned CI runner never gets an emergency CA rotation). Worth a docs warning, possibly an analog to `PatchConfig`'s system-locked/`required=true` non-overridability.
- **Multi-arch irrelevance**: single image manifest at the tag, never an image index — config has no platform axis.
- **Size limits**: reuse existing constants rather than inventing new ones — `patch/persistence.rs` already caps descriptor layers at 1 MiB client-side (CWE-400 guard), and the local config loader already caps `config.toml` at 64 KiB (`MAX_CONFIG_SIZE`, per `configuration.md`). Reuse the 64 KiB local cap for the OCI-fetched variant so "config file" means one size ceiling everywhere.
- **MIME sniffing**: OCI spec frames media type as untrusted until digest-verified ([media-types.md](https://github.com/opencontainers/image-spec/blob/main/media-types.md)) — exactly what `persist_patch_descriptor` already does; carry it unchanged. Never use a browser-executable media type (`text/html`, bare `text/*`) for the layer — some registries echo the declared media type as the blob's HTTP `Content-Type` on GET, and a registry UI blob-preview feature could reflect operator-authored content as renderable HTML. The vendor `+toml` type closes this by construction.

### Bottom line recommendation

Third instance of ocx's existing artifact-push pattern: `artifactType = application/vnd.sh.ocx.config.v1`, empty config, single layer `application/vnd.sh.ocx.config.v1+toml` with raw bytes, pushed by reserved tag, no `subject`/index. Works identically on a Distribution Spec v1.0-only registry (nothing depends on 1.1 features to function) while still being OCI-1.1-aware for tooling that looks. Reuse `MAX_CONFIG_SIZE` (64 KiB) as the fetch-side cap and the existing digest-reverify-before-parse discipline unchanged.

Files read for grounding: `crates/ocx_lib/src/config/patch.rs`, `crates/ocx_lib/src/patch/descriptor.rs`, `crates/ocx_lib/src/patch/persistence.rs`, `crates/ocx_lib/src/package_manager/tasks/patch_publish.rs`, `crates/ocx_lib/src/oci/client.rs` (~960-1180, 3826+), `crates/ocx_lib/src/oci/manifest_builder.rs`, `crates/ocx_lib/src/media_type.rs`, `website/src/docs/reference/configuration.md`.

---

## Section 2 — SOTA-Gap Review of the Managed-Config Plan/ADR (`rev-sota`)

**Research axis (question posed):** SOTA-gap reviewer, read-only, reviewing the managed-config plan+ADR (`parsed-greeting-pancake.md` — corporate managed ocx config as OCI artifact; seed in `config.toml`; notify/apply refresh; CI via env var) against current practice 2025-2026. Report requested on: (1) known pitfalls in managed/remote client-config systems the plan misses (Chrome enterprise policy, VS Code settings sync conflicts, Renovate preset resolution, Cachix, nix substituter trust, Docker MDM configs): rollback story, kill-switch abuse, config poisoning post-compromise, staged rollout / canary tags, telemetry-free drift detection; (2) OCI artifact consumption — anything since the research pass contradicting the chosen shape (image manifest + `artifactType` + empty config + single `+toml` layer, no referrers); (3) security-relevant SOTA — signed config (cosign/sigstore); is unsigned-but-digest-verified acceptable v1 posture given ocx has open sigstore issues (#98-#103); (4) onboarding UX gaps vs best-in-class (`cachix use`, `renovate-config` convention). Classified Actionable vs Deferred.

### Actionable (cheap plan-text fix or addition, no scope change)

**A1 — No local diagnostic/status verb (biggest gap).** Plan ships `ocx config update` as the *only* v1 verb. Every SOTA precedent checked (Chrome's `chrome://policy` page, Docker Desktop's Admin Console reporting, VS Code Settings Sync status) exposes a local, read-only introspection surface. This plan has none — and it's also the direct answer to the "telemetry-free drift detection" question: without a status verb, an ops team has no way to audit fleet compliance (stale snapshot, disabled refresh, wrong source) except by scraping raw files under `state/managed-config/`. Recommend adding `ocx config status` (read-only, no network): print resolved effective source (flag>env>seed), snapshot digest + `fetched_at` from `provenance.json`, effective refresh policy, and whether `OCX_NO_CONFIG`/`OCX_NO_CONFIG_REFRESH` is currently suppressing anything. This is cheap — it only reads already-persisted local state, no new wire format, no new network path — and gives ops a scriptable compliance signal via existing fleet/EDR tooling without ocx phoning home. Recommend revising "one verb v1" to "two verbs v1 (`update`, `status`)" rather than deferring — the debugging gap is real from day one (silent `notify` mode + no status = an admin can't tell if a host ever adopted the tier).

**A2 — Staged rollout needs zero new code, just a docs recipe.** Plan has no canary/phased-rollout mechanism (flagged as a gap vs Docker Desktop's "targeted group before global" and Chrome's atomic policy groups), but `source` already accepts any OCI tag/digest, and OCX already teaches a tag-cascade promotion convention for exactly this shape (`adr_cascade_platform_aware_push.md` — push `3.28.1`, cascade `3.28`→`3`→`latest`). Recommend the user-guide "Centrally managing ocx configuration" section document a `:canary`→`:stable` tag-promotion recipe (subset of fleet seeds `source=...:canary`, promote to `:stable` once validated). Zero new mechanism, reuses an idiom the project already has.

**A3 — Blast radius + signing deferral isn't written down.** Decision I only covers the one-hop `[managed]` strip. It doesn't state the actual blast radius: a compromised managed-config registry/credential lets an attacker push arbitrary `[mirrors]`/`[patches]` to the whole fleet — mirror rewrite is a MITM surface, and `[patches] required=true` forces companion/CA-overlay installs — with only registry auth + content-digest verification as the barrier, no signature. Checked the sibling `adr_infrastructure_patches.md`: it has **zero** mentions of cosign/sigstore/signing today, so this plan is consistent with existing project posture, not introducing a new weaker one — but that consistency should be *stated*, not silent. Recommend one paragraph in the ADR's Security/Decision-Drivers section naming the blast radius explicitly and committing both `[patches]` and `[managed]` to become `[trust.policy]` (#98) / auto-verify (#99) consumers once those land. This directly answers the signing question: **unsigned-but-digest-verified is acceptable v1 posture**, provided it's an explicit, cited deferral rather than an implicit omission — scoping in signing now would mean inventing a bespoke trust path ahead of #98's general mechanism, which is the kind of duplicated-effort the project's "extend existing mechanisms" bar exists to prevent.

**A4 — Make the "can't self-brick" property explicit.** The plan already has the right design: managed-config fetch always uses `canonical_reference()`, bypassing whatever `[mirrors]` the payload itself sets — so a malicious or broken payload can never break its own future re-fetch. That's a real strength but currently reads as incidental. Recommend one sentence in the ADR component contracts making it explicit (future readers/reviewers shouldn't have to re-derive it).

### Deferred (v2 / user decision — correctly out of v1 scope)

**D1 — Cosign/sigstore signing of the artifact itself.** Defer, contingent on A3 being written up as an explicit deferral rather than silent.

**D2 — True rollback/history** (kept `.previous/` generation, `ocx config update --rollback`). Plan's fix-forward story is adequate for v1: operator republishes corrected TOML at the same ref, next tick or explicit `ocx config update` re-syncs automatically, and `--managed-config ""` already gives a full manual escape hatch (clears fence + deletes snapshot). Building real history now is YAGNI — no evidence yet that fix-forward is too slow, and it matches the plan's own CAS-avoidance reasoning in Decision D. Revisit if incident response experience says otherwise.

**D3 — Registry HEAD/digest-probe portability** on non-compliant legacy on-prem registries. Low-value edge case; not new to this feature (every existing `fetch_manifest_digest` call site in the OCI client already assumes distribution-spec HEAD support). No action needed.

### No gaps found

- OCI artifact shape (image manifest, `artifactType` set, empty-config descriptor via existing `MEDIA_TYPE_OCI_EMPTY_CONFIG`, single typed layer, no subject/referrers) matches current OCI Image Spec 1.1 guidance exactly — empty-config + mandatory `artifactType` is the documented pattern, and skipping `subject`/referrers is correct since this is a standalone top-level artifact, not an attachment to another manifest. Also confirmed: if cosign signing is added later (D1), cosign attaches signatures as separate referrer artifacts pointing *at* this manifest's digest — no reshaping of the config manifest itself needed later.
- Dirty-fence detection (exit 82) for the seed block already matches the VS Code Settings Sync conflict-detection pattern (detect local edits before overwrite, require explicit force) — no gap.
- Decision H (no `ocx config publish`, document `oras push` recipe) is consistent with real precedent — Cachix, Renovate presets, and Docker's admin-settings.json are all plain-artifact/config distribution with no dedicated publish CLI either. Affirmed, not a gap.

Sources checked: OCI image-spec manifest.md / ORAS artifact docs (empty-config + artifactType requirement), Docker Desktop Admin Console docs (signing + targeted rollout), Chrome Enterprise Policy docs, Renovate config-presets docs, NixOS/Cachix substituter trust docs, Sigstore/cosign docs, plus local: `crates/ocx_lib/src/media_type.rs`, `crates/ocx_lib/src/oci/manifest_builder.rs`, `.claude/artifacts/adr_infrastructure_patches.md`, `.claude/rules/arch-principles.md`.
