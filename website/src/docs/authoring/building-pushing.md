---
outline: deep
---
# Building and Pushing

Every package starts the same way: a tar archive on disk, a `metadata.json` next to it, and a destination registry. This page covers the publisher workflow from local archive to published OCI image, including the cascade convention for rolling tags and the layer-reuse pattern that lets a single base archive back many releases.

## The First Push {#first-push}

[`ocx package push`][cmd-package-push] uploads zero or more layers as OCI blobs and records them under one image manifest in the order you give. `--platform` is required for every push: a single-platform push lands a single manifest under the tag. To attach more platforms to the same tag, push them one at a time — see the [multi-platform guide][authoring-multi-platform] for how OCX assembles the resulting [OCI Image Index][oci-image-index].

```sh
ocx package create build -m metadata.json -o mytool-1.0.0.tar.xz
ocx package push -p linux/amd64 -m metadata.json -i mytool:1.0.0 mytool-1.0.0.tar.xz
```

`--new` (`-n`) is a `--cascade` modifier: it tells the cascade resolver to skip the existing-tag lookup on the first publish of a repository. Outside `--cascade` the flag is a no-op. Reach for it once you adopt rolling tags.

<Terminal src="/casts/authoring/package-push.cast" title="Publishing a package for the first time" collapsed />

::: tip Test before you push
Before pushing, verify the package works locally with [`ocx package test`][authoring-testing]. It runs the same install pipeline — dep resolution, extraction, env composition — in a temp directory with no registry round-trip. Exit code is forwarded from the command you run, so CI can gate on it. See [Testing locally][authoring-testing] for the full workflow.
:::

## Bring Your Own Archives {#byo-archives}

[`ocx package push`][cmd-package-push] does not bundle a directory for you. Every file layer must be a pre-built `.tar.gz` / `.tar.xz` archive (the aliases `.tgz` and `.txz` are also accepted) — and that asymmetry is deliberate. Re-bundling the same content yields a non-deterministic digest (timestamps, compression entropy), and the registry treats every distinct digest as a fresh layer. Bundle once with [`ocx package create`][cmd-package-create] (see [Bundle Anatomy][authoring-bundle-anatomy]), then reference that archive across every subsequent push.

::: warning Zero-layer pushes need explicit `--metadata`
A push with zero file layers is valid: it produces a config-only OCI artefact (the same shape OCX uses internally for the `__ocx.desc` description tag written by [`ocx package describe`][cmd-package-describe]). With no file layer next to which to look for `<stem>-metadata.json`, `--metadata` is mandatory — the same rule applies whenever every layer is a `sha256:…` digest reference.
:::

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

`<algo>` is one of `sha256`, `sha384`, or `sha512`, with the matching hex length (64, 96, or 128 chars). The extension (`tar.gz` / `tar.xz` / aliases `tgz`/`txz`) is mandatory — OCI blob HEADs do not carry the original media type, so the publisher must declare it. A bare `<algo>:<hex>` without an extension is rejected. OCX HEADs the registry to verify the digest exists, then records the existing layer in the new manifest without re-uploading. This is the foundation of the [three-tier storage model][in-depth-storage-layers]: shared base archives are written once, hardlinked into every package that references them, and never re-downloaded.

The hand-publishing pattern (the [`ocx_mirror`][mirror-pipeline] tool currently always uploads file layers; cross-release digest reuse is something hand-driven publishers compose manually):

1. **Bundle the base archive once and capture its digest.** Bundles are not byte-reproducible across runs (mtimes vary; see [Bundle anatomy → stable archives][authoring-bundle-anatomy-stable]), so the digest is a property of the run that produced the archive, not of the source tree. Record it after creating the file (`sha256sum base.tar.xz`) — the local file digest equals the layer digest the registry stores.
2. **Push the first release with the file layer.** OCX uploads it under the digest captured in step 1.
3. **Push later releases by digest.** Re-reference the same blob via `sha256:<hex>.<ext>` — no re-bundle, no re-upload, no extra storage, no consumer re-download.

<Terminal src="/casts/authoring/package-layer-reuse.cast" title="Reusing a base layer across two releases" collapsed />

::: warning Pathological filenames
If a file in your working directory is literally named `sha256:abc….tar.gz`, prefix it with `./` to force file interpretation. Bare `<algo>:<hex>.<ext>` tokens are always parsed as digest references.
:::

## See Also {#see-also}

- [`ocx package push` reference][cmd-package-push]
- [Versioning in depth — cascades][in-depth-versioning-cascades]
- [Storage in depth — layers][in-depth-storage-layers]
- [Multi-platform packages][authoring-multi-platform]
- [Bundle anatomy][authoring-bundle-anatomy]

<!-- external -->
[oci-image-index]: https://github.com/opencontainers/image-spec/blob/main/image-index.md
[mirror-pipeline]: https://github.com/ocx-sh/ocx/tree/main/crates/ocx_mirror

<!-- commands -->
[cmd-package-create]: ../reference/command-line.md#package-create
[cmd-package-push]: ../reference/command-line.md#package-push
[cmd-package-describe]: ../reference/command-line.md#package-describe

<!-- in-depth -->
[in-depth-storage-layers]: ../in-depth/storage.md#layers
[in-depth-versioning-cascades]: ../in-depth/versioning.md#cascades

<!-- authoring -->
[authoring-bundle-anatomy]: ./bundle-anatomy.md
[authoring-bundle-anatomy-stable]: ./bundle-anatomy.md#stable
[authoring-multi-platform]: ./multi-platform.md
[authoring-testing]: ./testing.md
