---
outline: deep
---
# Building and Pushing

Every package starts the same way: a tar archive on disk, a `metadata.json` next to it, and a destination registry. This page covers the publisher workflow from local archive to published OCI image, including the cascade convention for rolling tags and the layer-reuse pattern that lets a single base archive back many releases.

## The First Push {#first-push}

[`ocx package push`][cmd-package-push] uploads zero or more layers as OCI blobs and records them under one image manifest for the single platform this invocation publishes. [`ocx package create`][cmd-package-create]'s `--platform` picks that platform and records it in the metadata sidecar; `push` reads it back and publishes under it by default, so the platform a package is published under always matches what its dependency pins were resolved against. Publishing more than one platform under the same tag means running `create`/`push` once per platform — see the [multi-platform guide][authoring-multi-platform] for how OCX assembles the resulting [OCI Image Index][oci-image-index] across those pushes.

```sh
ocx package create build -m metadata.json -o mytool-1.0.0.tar.xz -p linux/amd64
ocx package push -i mytool:1.0.0 mytool-1.0.0.tar.xz
```

`--new` (`-n`) is a `--cascade` modifier: it tells the cascade resolver to skip the existing-tag lookup on the first publish of a repository. Outside `--cascade` the flag is a no-op. Reach for it once you adopt rolling tags.

<Terminal src="/casts/authoring/package-push.cast" title="Publishing a package for the first time" collapsed />

::: tip Test before you push
Before pushing, verify the package works locally with [`ocx package test`][authoring-testing]. It runs the same install pipeline — dep resolution, extraction, env composition — in a temp directory with no registry round-trip. Exit code is forwarded from the command you run, so CI can gate on it. See [Testing locally][authoring-testing] for the full workflow.
:::

## Bring Your Own Archives {#byo-archives}

[`ocx package push`][cmd-package-push] does not bundle a directory for you. Every file layer must be a pre-built `.tar.gz` / `.tar.xz` / `.tar.zst` archive (the aliases `.tgz`, `.txz`, `.tzst`, and `.tar.zstd` are also accepted) — and that asymmetry is deliberate. Re-bundling the same content yields a non-deterministic digest (timestamps, compression entropy), and the registry treats every distinct digest as a fresh layer. Bundle once with [`ocx package create`][cmd-package-create] (see [Bundle Anatomy][authoring-bundle-anatomy]), then reference that archive across every subsequent push.

::: warning Zero-layer pushes need explicit `--metadata`
A push with zero file layers is valid: it produces a config-only OCI artefact (the same shape OCX uses internally for the `__ocx.desc` description tag written by [`ocx package describe`][cmd-package-describe]). With no file layer next to which to look for `<stem>-metadata.json`, `--metadata` is mandatory — the same rule applies whenever every layer is a `sha256:…` digest reference.
:::

## Resolving Dependency Pins {#dependency-pins}

A package that [declares dependencies][authoring-dependencies] needs each one pinned to a manifest digest before [`ocx package push`][cmd-package-push] will publish it — an unpinned dependency is rejected (exit 65). Writing that digest by hand does not scale past one dependency, and pinning the wrong kind of digest breaks silently later: it is tempting to pin a dependency's [OCI Image Index][oci-image-index] digest so a single identifier could resolve per-platform at install time, but a tag's index is rewritten on every platform push — the old index digest becomes untagged and the registry eventually garbage-collects it, so an index-pinned dependency 404s the moment that happens. See [Manifest Pins, Never Index Pins][reference-manifest-pins] for the full hazard.

[`ocx package create`][cmd-package-create] is the compiler for this problem. Write each dependency tag-only in the `metadata.json` sidecar, then resolve with `--platform`:

```sh
ocx package create build -i mytool:1.0.0 -p linux/amd64 -m metadata.json -o .
```

`create` resolves every unpinned dependency against the selected index and rewrites the sidecar in place with the resolved manifest pin — the rewritten file, not the one you hand-wrote, is what you push. Index selection follows the same [`--remote`][arg-remote] / [`--offline`][arg-offline] / [`--frozen`][arg-frozen] routing as every other resolution: the default checks the local index first and fetches on a miss; `--offline`/`--frozen` refuse to resolve a dependency tag that is not already cached (exit 81); a dependency tag absent from the selected index entirely fails with exit 79.

[`ocx package push`][cmd-package-push] then makes no resolution decisions of its own — it is a gate. It reads the sidecar `create` wrote, verifies every dependency carries a manifest pin covering the single platform being published, and verifies each unique pin actually resolves in its registry:

| Failure | Exit code | Meaning |
|---|---|---|
| Sidecar has no recorded platform | 65 | The sidecar predates `create`, or was hand-authored — run `ocx package create --platform` to establish one. |
| An explicit `--platform` disagrees with the sidecar's recorded platform | 65 | Drop `--platform` to publish under the recorded platform, or re-run `create` for the one you passed. |
| Dependency still tag-only | 65 | Re-run `ocx package create --platform` to pin it. |
| Pin map has no entry covering the platform being published | 65 | The dependency has no pin compatible with this push's platform. |
| Pin resolves to an image index | 65 | The dependency was pinned by hand against an index digest — re-run `create`. |
| Pinned manifest not found in the registry | 79 | The dependency's manifest was deleted, or the digest is wrong. |
| Registry authentication failed | 80 | Refresh credentials for the dependency's registry. |

A package with no dependencies, or one whose sidecar is already fully pinned, skips all of this — `create` does not touch the network when `--platform` is omitted and every dependency already carries a digest.

Building with `--platform any` is the one case where `create` writes a per-dependency pin map instead of a single digest: an `any`-targeted package performs no platform-specific resolution, so it can only depend on dependencies that themselves offer an `any` build — a dependency with no `any` manifest fails `create` outright, naming it. See [Multi-Platform Packages][authoring-multi-platform].

## Cascading Rolling Tags {#cascade}

Most publishers ship versioned releases (`1.0.0`, `1.0.1`) and want users to pin against rolling aliases (`1.0`, `1`, `latest`) without maintaining the alias graph by hand. The `--cascade` flag does that bookkeeping at push time: when you push `mytool:1.0.1`, OCX consults the existing tags and re-points each ancestor (`1.0`, `1`, `latest`) to the new digest — but only when the new tag is genuinely the latest at that specificity level. Push a backport `0.9.5` after `1.0.1` is live and `--cascade` won't touch `latest`, because `1.0.1` is still the newer release.

Cascade is a publisher convention, not a registry-enforced rule. The registry sees only tag-to-digest writes; OCX synthesises the alias semantics on top. Cascade decisions are evaluated per platform — a backport that is the latest for `linux/amd64` but trails the head on `darwin/arm64` will only re-point the rolling tags it actually leads on. The full alias model lives in [versioning in depth → cascades][in-depth-versioning-cascades].

<Terminal src="/casts/authoring/package-cascade.cast" title="Cascading rolling tags across releases" collapsed />

## Reusing Layers Across Packages {#layer-reuse}

A package's content is the union of its layers — a base, optional middle layers, a top layer with the binary. Re-pushing a layer that already exists in the target registry is wasteful: the registry GC will dedupe in the background, but the publisher already spent the upload bandwidth and the consumer pays the download cost on first install.

OCX lets you reference any layer that is already in the target registry by digest:

```
<algo>:<hex>.<ext>
```

`<algo>` is one of `sha256`, `sha384`, or `sha512`, with the matching hex length (64, 96, or 128 chars). The extension (`tar.gz` / `tar.xz` / `tar.zst` / aliases `tgz`/`txz`/`tzst`/`tar.zstd`) is mandatory — OCI blob HEADs do not carry the original media type, so the publisher must declare it. A bare `<algo>:<hex>` without an extension is rejected. OCX HEADs the registry to verify the digest exists, then records the existing layer in the new manifest without re-uploading. This is the foundation of the [three-tier storage model][in-depth-storage-layers]: shared base archives are written once, hardlinked into every package that references them, and never re-downloaded.

The hand-publishing pattern (the [`ocx_mirror`][mirror-pipeline] tool currently always uploads file layers; cross-release digest reuse is something hand-driven publishers compose manually):

1. **Bundle the base archive once and capture its digest.** Bundles are not byte-reproducible across runs (mtimes vary; see [Bundle anatomy → stable archives][authoring-bundle-anatomy-stable]), so the digest is a property of the run that produced the archive, not of the source tree. Record it after creating the file (`sha256sum base.tar.xz`) — the local file digest equals the layer digest the registry stores.
2. **Push the first release with the file layer.** OCX uploads it under the digest captured in step 1.
3. **Push later releases by digest.** Re-reference the same blob via `sha256:<hex>.<ext>` — no re-bundle, no re-upload, no extra storage, no consumer re-download.

<Terminal src="/casts/authoring/package-layer-reuse.cast" title="Reusing a base layer across two releases" collapsed />

::: warning Pathological filenames
If a file in your working directory is literally named `sha256:abc….tar.gz`, prefix it with `./` to force file interpretation. Bare `<algo>:<hex>.<ext>` tokens are always parsed as digest references.
:::

## Signing after push {#signing-after-push}

A pushed manifest is identified by its digest, but nothing prevents a registry operator or network
attacker from substituting a different binary under the same tag. [Sigstore][sigstore] keyless
signing closes that gap: it binds the manifest digest to your OIDC identity (GitHub Actions
workflow, Google account, email) via a short-lived certificate, logs the entry in
[Rekor][rekor]'s append-only transparency log, and attaches the resulting bundle to the manifest
as an [OCI Referrers][oci-referrers-spec] artifact. No long-lived signing keys, no key management.

After pushing, sign the platform manifest:

```sh
ocx package sign -p linux/amd64 my/cmake:3.28
```

Consumers verify by supplying the expected signer identity — verification fails loudly rather than
silently if the bundle is absent or the identity does not match.

For implementation detail (TUF trust root loading, referrers-capability cache, OCI 1.1 hard-fail
policy, bundle storage paths, and slice boundaries) see [Signing In Depth][in-depth-signing].

For command flags, token-source precedence, and exit codes see the
[`package sign` reference][cmd-package-sign] and [`package verify` reference][cmd-package-verify].

## See Also {#see-also}

- [`ocx package push` reference][cmd-package-push]
- [Versioning in depth — cascades][in-depth-versioning-cascades]
- [Storage in depth — layers][in-depth-storage-layers]
- [Declaring dependencies][authoring-dependencies] — when to depend, visibility, `name` overrides
- [Multi-platform packages][authoring-multi-platform]
- [Bundle anatomy][authoring-bundle-anatomy]

<!-- external -->
[oci-image-index]: https://github.com/opencontainers/image-spec/blob/main/image-index.md
[mirror-pipeline]: https://github.com/ocx-sh/ocx/tree/main/crates/ocx_mirror
[oci-referrers-spec]: https://github.com/opencontainers/distribution-spec/blob/main/spec.md#listing-referrers
[sigstore]: https://www.sigstore.dev/
[rekor]: https://github.com/sigstore/rekor

<!-- commands -->
[cmd-package-create]: ../reference/command-line.md#package-create
[cmd-package-push]: ../reference/command-line.md#package-push
[cmd-package-describe]: ../reference/command-line.md#package-describe
[cmd-package-sign]: ../reference/command-line.md#package-sign
[cmd-package-verify]: ../reference/command-line.md#package-verify
[arg-remote]: ../reference/command-line.md#arg-remote
[arg-offline]: ../reference/command-line.md#arg-offline
[arg-frozen]: ../reference/command-line.md#arg-frozen

<!-- reference -->
[reference-manifest-pins]: ../reference/metadata.md#dependencies-manifest-pins

<!-- in-depth -->
[in-depth-storage-layers]: ../in-depth/storage.md#layers
[in-depth-versioning-cascades]: ../in-depth/versioning.md#cascades
[in-depth-signing]: ../in-depth/signing.md

<!-- authoring -->
[authoring-bundle-anatomy]: ./bundle-anatomy.md
[authoring-bundle-anatomy-stable]: ./bundle-anatomy.md#stable
[authoring-dependencies]: ./dependencies.md
[authoring-multi-platform]: ./multi-platform.md
[authoring-testing]: ./testing.md
