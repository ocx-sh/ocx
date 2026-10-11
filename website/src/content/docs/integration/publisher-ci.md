---
title: "Publish packages from CI"
description: "A release pipeline that pushes a package with source annotations and an SBOM, signs it, attests provenance and promotes it."
---
<!-- doc_type: how-to -->
<!-- doc_tier: integration -->

This page shows a release pipeline for an ocx package. It pushes the package with provenance annotations and an SBOM, signs it with the runner's own identity, attests the build and promotes the result. It assembles commands that the publishing pages explain one by one.

Every step runs unattended, so read [Run ocx unattended][p-unattended-ci] first for the settings a job should make. The pipeline needs write access to the registry. [Registry authentication recipes][p-registry-auth-recipes] show how to give a job that credential.

## The pipeline in one job {#publisher-pipeline}

The job below runs on [GitHub Actions][github-actions-docs]. It builds the archive, pushes one platform, signs the manifest and attests the build. `id-token: write` is what provisions the OIDC identity that ocx detects, so there is no signing key to store.

```yaml
permissions:
  contents: read
  packages: write
  id-token: write

jobs:
  publish:
    runs-on: ubuntu-latest
    env:
      PKG: ghcr.io/acme/tools/widget
      OCX_AUTH_ghcr_io_TYPE: basic
      OCX_AUTH_ghcr_io_USER: ${{ github.actor }}
      OCX_AUTH_ghcr_io_TOKEN: ${{ secrets.GITHUB_TOKEN }}
    steps:
      - uses: actions/checkout@v4
      - uses: ocx-sh/setup-ocx@v1
      - run: ocx package create build -m metadata.json -o widget.tar.xz -p linux/amd64 -i "$PKG:1.2.3"
      - run: ocx package push --cascade --ci-annotations=github --sbom sbom.json widget.tar.xz
      - run: ocx package sign -p linux/amd64 "$PKG:1.2.3"
      - run: ocx package attest -p linux/amd64 --type slsaprovenance1 --predicate provenance.json "$PKG:1.2.3"
```

In `package create`, `build` is the content directory and `metadata.json` is the [package metadata][reference-metadata] your repository commits. The `sbom.json` and `provenance.json` files come from your build. ocx wraps them and does not generate them. Each step is a section below.

## Stamp where the package came from {#publisher-annotations}

`--ci-annotations=github` writes the source repository, the commit and a creation time onto the image index of every tag the push writes. GHCR reads the source annotation to link the package page to the repository and to inherit its permissions. A bare `--ci-annotations` detects the provider from the runner, and `=gitlab` does the same for GitLab. A set `SOURCE_DATE_EPOCH` decides the creation time, so a reproducible build stamps one instant. [Stamping from CI][publishing-ci-annotations] has the table of sources.

`--cascade` also moves the rolling tags `1.2`, `1` and `latest` to the new build. Cascade is a convention that the publisher maintains, and the registry does not enforce it.

## Attach the SBOM {#publisher-sbom}

`push --sbom` attaches a CycloneDX document to the manifest it just wrote. With a signing identity visible, which a GitHub or GitLab job provides, the SBOM is signed. Without one, the document is attached unsigned and the command says so. If an identity is visible but cannot be acquired, the step fails instead of falling back. For an SPDX document or a second predicate, use [`ocx package attest`][cmd-package-attest] with `--type spdx`.

[Attach an SBOM][publishing-sbom] has the trust rules.

## Sign and attest {#publisher-sign}

[`ocx package sign`][cmd-package-sign] binds the manifest digest to the pipeline's identity through [Sigstore][sigstore]. It finds the identity on its own, so the step needs no credential wiring. Run it once and read the `Certificate identity` and `Certificate OIDC issuer` lines. Those two strings are what a consumer's trust policy has to match. [Signing from CI][signing-ci] covers GitLab and other issuers.

Pass `--no-tty` to `sign` and `attest` in a pipeline. A missing token then fails the step and does not wait for a browser login. SLSA provenance needs a signing identity, and `attest --type slsaprovenance1` without one exits 64.

## Promote without rebuilding {#publisher-promote}

Build and test once, then move the same bytes through your registries:

```sh
ocx package copy --to prod.example.com --cascade ghcr.io/acme/tools/widget:1.2.3
```

The second registry needs its own credential. Add `OCX_AUTH_prod_example_com_*` variables to the job, as [Registry authentication recipes][p-registry-auth-recipes] describe. [`ocx package copy`][cmd-package-copy] copies the manifests, blobs and referrers verbatim, so the digest never changes and the signature and SBOM travel with it. A rerun is safe. [Promote packages][publishing-promote] describes the result rows and the dry run.

## Pre-release builds {#publisher-snapshots}

A change request needs a build that is not a release. [Snapshot tracks][publishing-snapshots] push a build per change, announce it to an index and tear it down when the request closes. Its CI section has a GitLab and a GitHub workflow. Retry exit 75 there and never retry 69 or 82, as the [exit code table][p-unattended-ci-unattended-exit] explains.

<!-- external -->
[github-actions-docs]: https://docs.github.com/en/actions
[sigstore]: https://www.sigstore.dev/

<!-- commands -->
[cmd-package-attest]: ../reference/command-line/package-publishing.md#package-attest
[cmd-package-sign]: ../reference/command-line/package-publishing.md#package-sign
[cmd-package-copy]: ../reference/command-line/package-publishing.md#package-copy

<!-- reference -->
[reference-metadata]: ../reference/metadata/index.md

<!-- guides -->
[publishing-ci-annotations]: ../publishing/build-and-push.mdx#source-annotation-ci
[publishing-sbom]: ../publishing/attest-and-sbom.mdx#attestations-attach
[publishing-promote]: ../publishing/promote-packages.mdx
[publishing-snapshots]: ../publishing/snapshot-tracks.md#snapshot-tracks-ci
[signing-ci]: ../publishing/sign-a-release.md#ci

<!-- pages -->
[p-unattended-ci]: ./unattended-ci.md
[p-registry-auth-recipes]: ./registry-auth-recipes.md
[p-unattended-ci-unattended-exit]: ./unattended-ci.md#unattended-exit
