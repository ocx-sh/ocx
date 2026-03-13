# ADR: SBOM Generation Strategy

## Metadata

**Status:** Proposed
**Date:** 2026-03-13
**Deciders:** mherwig
**Beads Issue:** N/A
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/tech-strategy.md`
**Domain Tags:** security | devops
**Supersedes:** N/A
**Superseded By:** N/A

## Context

OCX distributes pre-built Rust binaries across 8 platform targets via GitHub Releases and an OCI registry (`ocx.sh`). There is currently no Software Bill of Materials (SBOM) generated at any stage of the build or release pipeline. SBOMs are increasingly required by:

- **Regulatory mandates**: US Executive Order 14028, EU Cyber Resilience Act (CRA), CISA 2025 Minimum Elements
- **Enterprise adoption**: Organizations evaluating third-party tools require SBOMs for vulnerability tracking
- **Supply chain trust**: Consumers of OCX-distributed binaries need to verify what dependencies are embedded

Two characteristics of this project make SBOM generation non-trivial:

1. **Platform-specific dependencies**: Windows builds include `junction` (symlink handling); the actual dependency tree varies by target triple. An aggregate SBOM would be inaccurate.
2. **Patched dependency**: `oci-client` is overridden via `[patch.crates-io]` to a local git submodule (`external/rust-oci-client` v0.16.1). SBOMs must reflect the patched version, not the upstream crate-index version.
3. **Two distribution channels**: Binaries are published both as GitHub Release assets (via cargo-dist) and as OCI packages (via `ocx package push`). SBOMs should accompany both.

## Decision Drivers

- **Accuracy**: SBOM must reflect the *exact* dependencies compiled into each platform-specific binary
- **Automation**: Zero manual steps — generation must be part of the existing cargo-dist + post-release pipeline
- **Minimal friction**: Prefer cargo-dist native integration over custom CI steps
- **Tooling compatibility**: Downstream consumers should be able to scan binaries with standard tools (Trivy, Grype, `cargo audit`)
- **Future-proofing**: Native Cargo SBOM support (RFC 3553) is landing; avoid over-investing in tools that will be superseded

## Considered Options

### Option 1: cargo-auditable Only (Binary-Embedded)

**Description:** Enable `cargo-auditable = true` in `dist-workspace.toml`. This replaces `cargo build` with `cargo auditable build`, embedding a compressed dependency manifest into a `.dep-v0` linker section in the compiled binary. No standalone SBOM file is produced.

| Pros | Cons |
|------|------|
| Exact per-platform accuracy (compiled deps only) | No standalone SBOM file for compliance submissions |
| Under 4 KB overhead per binary | Incompatible with `cargo-zigbuild` (not currently used) |
| Trivy, Grype, `cargo audit` can scan binaries directly | Consumers must use tooling that understands `.dep-v0` |
| One-line config change in `dist-workspace.toml` | Not a standard SBOM format (CycloneDX/SPDX) |
| Reproducible (no timestamps, sorted JSON) | Does not include build environment metadata (rustc version) |
| Adopted by Alpine Linux, NixOS, openSUSE | — |

### Option 2: cargo-cyclonedx Only (Standalone SBOM File)

**Description:** Enable `cargo-cyclonedx = true` in `dist-workspace.toml`. cargo-dist runs `cargo cyclonedx` during the build phase, producing a `bom.xml` (CycloneDX format) that is distributed alongside each platform's release archive.

| Pros | Cons |
|------|------|
| Standard CycloneDX format — universally accepted | Generated from `cargo metadata`, not actual compilation |
| Standalone file for compliance submissions | May include dependencies not actually compiled for the target |
| cargo-dist handles generation and distribution | CycloneDX XML (not JSON) by default — less tooling-friendly |
| Actively maintained (STF-funded revival, v0.5.8) | Does not handle `--filter-platform` automatically |
| Includes license metadata | Extra build step time |

### Option 3: Both cargo-auditable + cargo-cyclonedx (Recommended)

**Description:** Enable both options in `dist-workspace.toml`. cargo-auditable provides exact per-binary dependency truth for scanner tools; cargo-cyclonedx provides a standard-format standalone SBOM file for compliance and human review.

| Pros | Cons |
|------|------|
| Best of both worlds: scanner support + compliance file | Two tools to maintain/update |
| cargo-auditable = ground truth; cyclonedx = human-readable | Slight build time increase |
| Both are one-line cargo-dist config changes | Minor: CycloneDX SBOM may not perfectly match embedded data |
| Per-platform builds mean per-platform SBOMs | — |
| Future: replace cyclonedx with native Cargo SBOM when RFC 3553 stabilizes | — |

### Option 4: External Tools (Syft/Trivy) in CI

**Description:** Add custom CI steps using Syft or Trivy to generate SBOMs from Cargo.lock or built binaries, independent of cargo-dist.

| Pros | Cons |
|------|------|
| Multi-format support (CycloneDX + SPDX) | Custom CI steps outside cargo-dist — fragile, must be maintained |
| Can combine SBOM generation with vulnerability scanning | Cannot be added to `dist-workspace.toml` — requires editing generated `release.yml` (violates project rule) |
| Trivy can scan cargo-auditable binaries post-build | Additional tool installation in CI |
| — | Syft/Trivy Cargo.lock parsing doesn't resolve per-platform deps |

## Decision Outcome

**Chosen Option:** Option 3 — Both cargo-auditable + cargo-cyclonedx

**Rationale:**

1. **Accuracy**: cargo-auditable embeds the *exact* compiled dependency tree per binary. Since cargo-dist builds each target separately, each binary's `.dep-v0` section reflects only that platform's dependencies (e.g., `junction` appears only in Windows binaries).

2. **Compliance**: cargo-cyclonedx produces a standard CycloneDX `bom.xml` that can be submitted to procurement teams, attached to GitHub Releases, and processed by any SBOM-aware tool.

3. **Minimal effort**: Both are single-line additions to `dist-workspace.toml`. No custom CI steps, no editing of the generated `release.yml` (which is forbidden per project rules).

4. **Forward-compatible**: When native Cargo SBOM support (RFC 3553) stabilizes (expected ~Cargo 1.87+), the `cargo-cyclonedx` step can be replaced with the native `*.cargo-sbom.json` output. cargo-auditable remains valuable regardless, as binary-embedded data enables post-distribution scanning.

### Consequences

**Positive:**
- Every OCX release binary is scannable by Trivy/Grype without a separate SBOM file
- Every release includes a standard-format CycloneDX SBOM per platform
- GitHub Release assets include both binary archives and their SBOMs
- No changes to the generated `release.yml` — all configuration via `dist-workspace.toml`

**Negative:**
- Build time increases slightly (cargo-auditable wraps the build; cargo-cyclonedx runs post-build)
- CycloneDX `bom.xml` may include dependencies that are conditionally compiled out for a given target (cargo-cyclonedx uses `cargo metadata`, not the actual build graph)

**Risks:**
- **cargo-auditable + cargo-zigbuild incompatibility**: Not a current risk (OCX doesn't use zigbuild), but would block future adoption of zigbuild for cross-compilation. Mitigation: cargo-auditable is working on zigbuild compatibility.
- **Patched oci-client in SBOM**: cargo-auditable will correctly embed the patched version (it runs during the actual build). cargo-cyclonedx *should* respect `[patch.crates-io]` via `cargo metadata`, but this should be verified. Mitigation: spot-check the first generated `bom.xml` to confirm the patched version appears.

## Technical Details

### Architecture

```
dist-workspace.toml
  ├── cargo-auditable = true    ──→  cargo auditable build
  │                                    └── Binary with .dep-v0 section
  │                                         └── Scannable by: trivy, grype, cargo audit
  │
  └── cargo-cyclonedx = true    ──→  cargo cyclonedx
                                       └── bom.xml (CycloneDX)
                                            └── Attached to GitHub Release

Pipeline flow (per platform):
  ┌─────────────────────────────────────────────────────────────────┐
  │  cargo-dist build (release.yml, matrix per target)              │
  │                                                                 │
  │  1. cargo auditable build --release -p ocx --target <triple>    │
  │     → binary with embedded .dep-v0 dependency manifest          │
  │                                                                 │
  │  2. cargo cyclonedx -p ocx                                     │
  │     → bom.xml alongside binary in release archive               │
  │                                                                 │
  │  3. Upload archive (binary + bom.xml) to GitHub Release         │
  └─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
  ┌─────────────────────────────────────────────────────────────────┐
  │  post-release-oci-publish.yml (per platform)                    │
  │                                                                 │
  │  1. Extract binary from cargo-dist archive                      │
  │  2. ocx package create → repackage as OCX tarball               │
  │  3. ocx package push → publish to ocx.sh registry               │
  │                                                                 │
  │  Note: The binary retains its .dep-v0 section through           │
  │  repackaging. The bom.xml could optionally be included          │
  │  in the OCX package or pushed as an OCI referrer artifact.      │
  └─────────────────────────────────────────────────────────────────┘
```

### Per-Platform Dependency Differences

| Target | Extra Dependencies | Notes |
|--------|-------------------|-------|
| `x86_64-pc-windows-msvc` | `junction` | Windows symlink handling |
| `aarch64-pc-windows-msvc` | `junction` | Windows symlink handling |
| `*-unknown-linux-musl` | statically linked libc | Different linking model |
| `*-unknown-linux-gnu` | dynamically linked glibc | Standard linking |
| `*-apple-darwin` | — | Standard dependencies |

cargo-auditable handles this automatically: each binary's `.dep-v0` section contains only the crates actually compiled for that target. cargo-cyclonedx's `bom.xml` may be less precise here.

### NTIA/CISA 2025 Minimum Elements Coverage

| Element | cargo-auditable | cargo-cyclonedx |
|---------|----------------|-----------------|
| Supplier Name | Partial (crate authors) | Yes |
| Component Name | Yes | Yes |
| Version | Yes | Yes |
| Unique Identifiers | SHA-256 (crate checksum) | purl, cpe |
| Dependency Relationships | Yes (tree) | Yes (tree) |
| SBOM Author | No (embedded) | Yes |
| Timestamp | No (reproducible) | Yes |
| Component Hash | Implicit (Cargo.lock) | Partial |
| License | No | Yes |
| Tool Name | No | Yes |

The combination of both tools covers all CISA 2025 minimum elements.

### Future Enhancement: Signed Attestations

Once the basic SBOM generation is in place, a follow-up phase can add signed attestations:

```yaml
# Future: add to post-build step in release pipeline
- name: Attest SBOM
  uses: actions/attest-sbom@v2
  with:
    subject-path: target/release/ocx
    sbom-path: bom.xml
```

This produces a Sigstore-signed in-toto attestation, verifiable with `gh attestation verify`. Provides SLSA Build Level 2 provenance. **Not in scope for the initial implementation** — evaluate after the base SBOM generation is validated.

## Implementation Plan

1. [ ] Add `cargo-auditable = true` to `dist-workspace.toml` under `[dist]`
2. [ ] Add `cargo-cyclonedx = true` to `dist-workspace.toml` under `[dist]`
3. [ ] Run `dist generate-ci` to regenerate `release.yml`
4. [ ] Commit both config and regenerated workflow
5. [ ] Test with a pre-release tag to verify:
   - Binary contains `.dep-v0` section (check with `cargo audit bin target/release/ocx`)
   - `bom.xml` is included in release archives
   - `bom.xml` reflects patched `oci-client` version (0.16.1, not 0.16.0)
   - Windows `bom.xml` includes `junction`, non-Windows does not (or verify via `.dep-v0`)
6. [ ] Verify Trivy can scan the released binary: `trivy fs --scanners vuln target/release/ocx`
7. [ ] Add `task sbom:check` to `taskfile.yml` for local SBOM generation during development
8. [ ] Document SBOM availability in release notes template

### Future Phases (Out of Scope)

- **Phase 2**: GitHub Artifact Attestations via `actions/attest-sbom`
- **Phase 3**: Embed CycloneDX SBOM as OCI referrer artifact when pushing to ocx.sh registry
- **Phase 4**: Replace cargo-cyclonedx with native Cargo SBOM (RFC 3553) when stabilized

## Validation

- [ ] Pre-release build produces binary with `.dep-v0` section
- [ ] Pre-release archive contains `bom.xml`
- [ ] `bom.xml` lists patched `oci-client` v0.16.1
- [ ] Windows binary's embedded deps include `junction`; Linux/macOS do not
- [ ] Trivy scan of released binary reports zero false negatives vs Cargo.lock audit

## Links

- [cargo-auditable](https://github.com/rust-secure-code/cargo-auditable) — Binary-embedded dependency manifests
- [cargo-cyclonedx](https://github.com/CycloneDX/cyclonedx-rust-cargo) — CycloneDX SBOM generator for Cargo
- [cargo-dist SBOM config](https://axodotdev.github.io/cargo-dist/book/reference/config.html) — `cargo-auditable` and `cargo-cyclonedx` options
- [RFC 3553 (native Cargo SBOM)](https://github.com/rust-lang/cargo/pull/13709) — Future native support
- [actions/attest-sbom](https://github.com/actions/attest-sbom) — GitHub signed attestations
- [CISA 2025 SBOM Minimum Elements](https://www.cisa.gov/resources-tools/resources/2025-minimum-elements-software-bill-materials-sbom)
- [CycloneDX specification](https://cyclonedx.org/specification/overview/)
- [EU Cyber Resilience Act](https://digital-strategy.ec.europa.eu/en/policies/cyber-resilience-act)
- [adr_release_install_strategy.md](./adr_release_install_strategy.md) — Related: release pipeline architecture

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-03-13 | Claude (Architect) | Initial draft |
