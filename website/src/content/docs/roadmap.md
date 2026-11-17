---
title: "Roadmap"
---
<!-- doc_type: explanation -->

What is shipped, in progress and planned for OCX, by theme. Each row links to its issue or pull request on [GitHub](https://github.com/ocx-sh/ocx) where one exists.

## Composable Packages

Multi-layer OCI artifacts that let publishers compose packages from reusable building blocks. Share stable internals across releases and apply infrastructure-specific patches without rebuilding from scratch.

| Feature | Status | Link |
|---|---|---|
| Multi-layer artifacts | In progress | [#20](https://github.com/ocx-sh/ocx/issues/20), [#22](https://github.com/ocx-sh/ocx/pull/22) |
| Infrastructure patches | Shipped | [#21](https://github.com/ocx-sh/ocx/pull/21) |
| Shared internals | Planned | — |
| Delta downloads | Planned | — |

## Dependencies

Packages declare dependencies on other packages, enabling automatic resolution for composable toolchains. A Java application can require a specific JRE version, and OCX resolves the full graph.

| Feature | Status | Link |
|---|---|---|
| Dependency resolution | Shipped | [#13](https://github.com/ocx-sh/ocx/pull/13) |
| Dependency mounting | Planned | [#31](https://github.com/ocx-sh/ocx/issues/31) |
| Path interpolation | Planned | [#32](https://github.com/ocx-sh/ocx/issues/32) |
| Parallel resolution | Planned | [#28](https://github.com/ocx-sh/ocx/issues/28) |
| Toolchain config | Shipped | [#33](https://github.com/ocx-sh/ocx/issues/33), [Project toolchain][guide-project-toolchain] |
| Transitive constraints | Planned | — |

## System Requirements

Packages declare required host capabilities like glibc version, musl compatibility, or specific CPU features. OCX validates the host before installation and selects the best matching variant.

| Feature | Status | Link |
|---|---|---|
| Package variants | Shipped | [#14](https://github.com/ocx-sh/ocx/pull/14) |
| Host capabilities | Planned | — |
| Pre-install validation | Planned | — |
| Variant fallbacks | Planned | — |

## Referrer API

Attach SBOMs, signatures, and attestations to existing releases using the OCI Referrers API. No need to re-publish a package to add supply-chain metadata after the fact.

| Feature | Status | Link |
|---|---|---|
| OCI referrers | Shipped | [#24](https://github.com/ocx-sh/ocx/issues/24), [Verify what you install][guide-verify-installs] |
| SBOM attachment | Shipped | [Attach attestations and SBOMs][guide-attest-sbom] |
| Post-publish attestations | Shipped | [Attach attestations and SBOMs][guide-attest-sbom] |
| Signature discovery | Planned | — |

## Interoperability

First-class integrations with the tools and platforms where developers already work. From build systems to CI pipelines to local development environments, OCX meets you where you are.

| Feature | Status | Link |
|---|---|---|
| Bazel module | In progress | [#12](https://github.com/ocx-sh/ocx/pull/12) |
| GitHub Actions | In progress | [ocx-sh/setup-ocx](https://github.com/ocx-sh/setup-ocx) |
| DevContainer features | Shipped | [Dev Container Feature][p-integrations-devcontainer] |
| mise backend plugin | Planned | [#34](https://github.com/ocx-sh/ocx/issues/34) |
| Shims & lazy loading | Shipped | [Deferred packages][explanation-deferred-packages] |
| Air-gap export/import | Planned | [#25](https://github.com/ocx-sh/ocx/issues/25) |

## Hardening

Stabilizing the CLI interface and package metadata format for long-term reliability. Improved error messages, better documentation, and a commitment to backwards compatibility.

| Feature | Status | Link |
|---|---|---|
| Stable CLI semver | Planned | — |
| Schema validation | Planned | — |
| Idempotent PATH setup | Planned | [#26](https://github.com/ocx-sh/ocx/issues/26) |
| Relocatable symlinks | Planned | [#23](https://github.com/ocx-sh/ocx/issues/23) |
| Content dedup fix | Planned | [#27](https://github.com/ocx-sh/ocx/issues/27) |
| API documentation | Planned | — |

<!-- internal -->
[guide-project-toolchain]: ./guides/project-toolchain.mdx
[guide-verify-installs]: ./guides/verify-installs.mdx
[guide-attest-sbom]: ./publishing/attest-and-sbom.mdx
[explanation-deferred-packages]: ./explanation/deferred-packages.mdx
[p-integrations-devcontainer]: /integrations/devcontainer/
