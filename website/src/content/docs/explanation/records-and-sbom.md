---
title: "Records and SBOMs"
description: "What the pkg:oci identity in an execution record buys, and how a record chains to an SBOM or attestation by digest."
---
<!-- doc_type: explanation -->

This page explains the package identity inside an execution record and how a record chains to an SBOM or an attestation. It is a sibling of [Execution Records][records], which covers switching recording on and what a record promises.

## What `pkg:oci` buys {#purl}

`packages[].uri` is a [purl][purl-spec] of type `pkg:oci`. The version is the sha256 digest and `tag` is advisory, matching OCX's digest-is-identity model.

The `packages[]` entries are field-for-field an [in-toto][in-toto-attestation] `ResourceDescriptor`. They drop into an [SLSA][slsa] `resolvedDependencies` list without reshaping if an attestation flow is ever built on top. [Sigstore][sigstore], [Tekton Chains][tekton-chains] and [GUAC][guac] all consume in-toto shapes.

The claim is narrow, so nothing is promised that is not delivered. A purl gives a stable, standardized identity string, the same shape [Trivy][trivy] emits. It joins across tools and stays forward-compatible if records ever feed an SBOM or attestation pipeline.

It does not give vulnerability lookup. [OSV][osv] has no OCI ecosystem and treats `purl` as informational rather than a query key. [Grype][grype] decomposes an image into its constituent packages and matches those, so a whole-artifact `pkg:oci` purl gets zero CVE matches.

CVE visibility into a packaged binary would require the package's upstream ecosystem purl carried alongside. That is a separate feature, not a side effect of this one.

## Chaining to an SBOM or attestation {#sbom-chain}

A record answers "which exact digests ran" and says nothing about what is inside those digests. [`ocx package sbom`][cmd-package-sbom] answers that second question. The two chain through a key the record already carries, with no format change on either side.

```text
execution record  →  "these exact digests ran"
        │ packages[].digest.sha256 is the join key
        ▼
ocx package sbom <digest>  →  "and here is what is inside them"
```

The join is deliberately a digest, never a name or a purl. [`ocx package sign`][cmd-package-sign] and [`ocx package attest`][cmd-package-attest] bind every signature and attestation to the manifest digest alone. The verification pipeline hardcodes the `sha256` algorithm and ignores the subject's `name` field entirely. A name-keyed or purl-keyed join would match on a field the pipeline never treats as binding. `packages[].digest.sha256` in a record is that same digest.

To confirm that what ran was also what was approved, read the digest out of a record. Hand it to [`ocx package sbom`][cmd-package-sbom] or [`ocx package verify --attestation`][cmd-package-verify-attestations].

```sh
digest=$(jq -r '.packages[0].digest.sha256' /var/log/ocx/records/*.json)
ocx package sbom "registry.example/cmake@sha256:${digest}" --summary
ocx package verify --attestation -p linux/amd64 "registry.example/cmake@sha256:${digest}"
```

Both commands need a package that carries an SBOM attestation, which [`ocx package attest`][cmd-package-attest] attaches (see [Attest and SBOM](../publishing/attest-and-sbom.mdx)). With `--summary`, the `sbom` listing gains a detail such as `, CycloneDX 1.5 with 42 component(s) under cmake`.

<!-- internal -->
[records]: execution-records.md

<!-- external -->
[purl-spec]: https://github.com/package-url/purl-spec
[in-toto-attestation]: https://github.com/in-toto/attestation
[slsa]: https://slsa.dev/spec/v1.0/provenance
[sigstore]: https://www.sigstore.dev/
[tekton-chains]: https://tekton.dev/docs/chains/
[guac]: https://github.com/guacsec/guac
[trivy]: https://trivy.dev/
[osv]: https://osv.dev/
[grype]: https://github.com/anchore/grype

<!-- commands -->
[cmd-package-sbom]: ../reference/command-line/package-consuming.md#package-sbom
[cmd-package-sign]: ../reference/command-line/package-publishing.md#package-sign
[cmd-package-attest]: ../reference/command-line/package-publishing.md#package-attest
[cmd-package-verify-attestations]: ../publishing/attest-and-sbom.mdx#attestations-verification
