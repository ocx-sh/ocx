# Research: Non-tar OCI layer media types — precedent for a zip layer type

**Axis 1/3** for ocx-sh/ocx#183 (zip layer media type). Produced 2026-08-09 during
`/hex-plan` Discover+Research. Findings decay — re-verify before citing in an ADR
older than ~6 months.

## Bottom line

Minting `application/vnd.sh.ocx.layer.v1.zip` is squarely in line with ecosystem
practice. The OCI project's own artifact-authoring guidance names exactly this
pattern with a non-tar worked example, and production precedent for non-tar
custom layers already exists. No registry inspects layer bytes against the
declared media type.

**But** the digest-identity goal #183 states as its motivation may not require a
new media type at all — see §5.

## 1. What the spec requires

Registered layer types are all tar-based (`tar`, `+gzip` MUST, `+zstd` SHOULD) —
[image-spec/layer.md](https://github.com/opencontainers/image-spec/blob/main/layer.md).

Critically, the tar-changeset rule in
[manifest.md](https://github.com/opencontainers/image-spec/blob/main/manifest.md)
is explicitly scoped to `config.mediaType == application/vnd.oci.image.config.v1+json`
— real container images an OCI runtime unpacks into a rootfs. For an **artifact
manifest** (any other config, including the registered empty-config marker) there
is **no format requirement on layers at all**. A zip layer is not "unregistered";
it is outside that contract's scope entirely. ocx's existing artifact-push shape
(empty config + custom-media-type layer) already relies on this.

[artifact-authors.md](https://github.com/opencontainers/artifacts/blob/main/artifact-authors.md)
states outright: *"content layer format is up to the artifact author... standard
or custom"*, with recommended template
`[tree].[org].[layerType].[subtype].layer.[version].[fileFormat]+[compression]`
and a **non-tar worked example**: `application/vnd.sylabs.sif.layer.v1.sif`.
`application/vnd.sh.ocx.layer.v1.zip` fits exactly and matches ocx's own
`vnd.sh.ocx.*` convention. IANA registration is optional (Helm did it;
Sylabs and WASM did not — both work).

## 2. Real precedents

| Project | Media type | Tar-wrapped | Consumption |
|---|---|---|---|
| Sylabs SIF/SquashFS | `application/vnd.sylabs.image.layer.v1.squashfs` | **No** | Sylabs client reads directly; opaque to generic runtimes |
| WASM-to-OCI | `application/vnd.wasm.content.layer.v1+wasm` | **No** | Wasm runtime executes directly |
| CNCF Wasm OCI Artifact spec | `application/vnd.w3c.wasm.component.v1+wasm` | **No** | Same, by design |
| Helm charts | `application/vnd.cncf.helm.chart.content.v1.tar+gzip` | Yes | Canonical naming/IANA example, not a non-tar precedent |
| ORAS generic files | caller-chosen (`application/pdf`, …) | No | Registry stores bytes, no interpretation |

**Sylabs lesson (closest analog).** SquashFS-layer images push and pull fine
everywhere, but generic OCI runtimes (`docker run`, containerd) cannot unpack them
as a rootfs, so Sylabs ships a `--layer-format tar` conversion flag. This friction
is runtime-interop, not registry rejection, and **does not transfer to ocx** — ocx
never relies on `docker run` to unpack its own layers.

Sources: [docs.sylabs.io](https://docs.sylabs.io/guides/latest/user-guide/cloud_library.html),
[engineerd/wasm-to-oci](https://github.com/engineerd/wasm-to-oci),
[CNCF TAG Runtime](https://tag-runtime.cncf.io/wgs/wasm/deliverables/wasm-oci-artifact/),
[Helm blog](https://helm.sh/blog/helm-oci-mediatypes/).

## 3. Registry compatibility — no evidence of rejection

Docker Hub, GHCR, Harbor, Artifactory, and Zot all support arbitrary OCI artifact
media types. No documented content-type sniffing, size gate, or rejection found.
GHCR via ORAS accepts PDFs, JPEGs, and custom vendor types per two independent
writeups ([Ken Muse](https://www.kenmuse.com/blog/universal-packages-on-github-with-oras/),
[aahlenst.dev](https://www.aahlenst.dev/blog/storing-blobs-on-github-container-registry/)).
Artifactory 7.74 built its OCI repo type to be permissive across mediaTypes
([JFrog](https://jfrog.com/blog/oci-support-in-jfrog-artifactory/)).
Distribution-spec blob PUT/GET is digest+size keyed; `mediaType` is caller
metadata, not an enforced content type. Historical enforcement friction was
`config.mediaType` allow-listing in older Quay/Harbor code — never a layer-type
rejection.

## 4. Unpacked vs opaque

The majority pattern for non-container OCI content: unpacked by the
domain-specific client, never by generic OCI tooling (Sylabs, WASM runtimes, Helm
client). ocx unpacking its own zip layer is the norm, not the exception.

## 5. Cheaper alternative for the motivating problem

#183's real ask is "upstream file hash == OCI layer digest", not "we need a zip
verb". cosign's `sha256-<digest>.sig` tag-fallback scheme
([sigstore docs](https://docs.sigstore.dev/cosign/signing/other_types/)) already
solves hash-identity linking via a canonical tag string with no manifest schema
change — structurally identical to **ocx's own existing `sha256.<upstream-hash>`
mirror storage tag**.

Consequence: pushing raw upstream bytes as a single-layer blob and recording the
upstream hash in the tag already achieves digest identity **today**, independent
of media type. A zip-specific media type is only needed to skip an unpack step or
avoid re-wrapping in tar — an **unpack-mechanics** decision, not an
identity-preservation requirement. No shared library does OCI-layer-aware zip
extraction, so the real cost of #183 is writing that unpack code, not minting the
media-type string.

## 6. Direct answer

**In line with ecosystem practice**, not rare-and-discouraged. No project was found
that tried a non-tar layer type and walked it back; Sylabs' precedent is still live
in production years later. What is genuinely rare is non-tar layers *in general* —
most stick to tar+gzip for tooling universality, not because zip was tried and
rejected. ocx would join a legitimate minority (Sylabs, WASM), not a cautionary
tale.

The plan should separate three questions that #183 conflates:

- **(a) media-type legality** — settled, cheap;
- **(b) unpack mechanics** — the real new work;
- **(c) whether a new media type is needed for the stated digest-identity goal** —
  per §5, likely not.

> **Speculation flag:** §6's "no abandoned attempts" claim is inference from an
> absence of counter-evidence in searches, not a documented retrospective.
