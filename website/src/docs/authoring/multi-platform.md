---
outline: deep
---
# Multi-Platform Packages

Most binary tools ship per-platform builds — different bytes for Linux/amd64, Linux/arm64, Darwin/arm64, Windows/amd64. The naive distribution approach is one tag per platform: `mytool:1.0.0-linux-amd64`, `mytool:1.0.0-darwin-arm64`. That works, but it pushes the platform-resolution problem onto every consumer's install script. OCX uses [OCI Image Indexes][oci-image-index] instead — one tag, multiple manifests, OCX picks the right one at install time based on the consumer's platform.

This page covers the publisher view: how to assemble a multi-platform package, how `ocx package push` builds the index, and the digest-stability properties you can rely on.

## One Tag, Many Manifests {#concept}

An [OCI Image Index][oci-image-index] is a manifest of manifests — a single descriptor that points at one image manifest per platform. When a consumer runs `ocx package install mytool:1.0.0`, OCX fetches the index, finds the manifest matching the consumer's platform, and pulls only that manifest's layers. No conditional logic in install scripts, no platform-suffixed tags to keep in sync.

The publisher equivalent of "build for amd64, then arm64, then push the index" collapses to: push each platform separately under the same tag, and OCX assembles the index for you. When [`ocx package push`][cmd-package-push] sees a tag that already has a manifest, it merges the new platform into the existing index rather than replacing it. (`--new` on the first push tells the cascade resolver to skip that lookup; everywhere else it is a no-op.)

## The Per-Platform Push Pattern {#pattern}

The hand-publishing flow (the [`ocx_mirror`][mirror-pipeline] tool runs the same pattern from a YAML spec):

1. **Bundle each platform's content with [`ocx package create`][cmd-package-create].** Pass `-i mytool:1.0.0 -p <platform>` and let `-o .` infer the output name. Each create call drops a `<name>-<tag>-<os>-<arch>.tar.xz` archive (`<name>` is the OCI repository's last segment) plus a sibling `<…>-metadata.json` sidecar — the pair [`ocx package push`][cmd-package-push] picks up automatically. The full sidecar/inferred-name convention lives in [Bundle Anatomy → sidecars][authoring-bundle-sidecars].
2. **Push the first platform with `--new --cascade`.** `--cascade` keeps your rolling-tag aliases (`1.0`, `1`, `latest`) in sync; `--new` skips the existence lookup that cascade would otherwise issue against a tag that does not yet exist. Omit `-m`: push reads the sidecar next to the layer.
3. **Push subsequent platforms with `--cascade`** (no `--new`). OCX detects the existing manifest, merges the new platform into the image index, re-points the tag at the index digest, and updates each rolling alias.

```sh
ocx package create build -i mytool:1.0.0 -p linux/amd64 -m metadata.json -o .
ocx package create build -i mytool:1.0.0 -p linux/arm64 -m metadata.json -o .

ocx package push -n -c -p linux/amd64 mytool:1.0.0 mytool-1.0.0-linux-amd64.tar.xz
ocx package push    -c -p linux/arm64 mytool:1.0.0 mytool-1.0.0-linux-arm64.tar.xz
```

After both pushes, `mytool:1.0.0` resolves to an image index with two platform descriptors, and any rolling aliases (`1.0`, `1`, `latest`) point at the same index digest. A consumer on Linux/arm64 fetches only the Linux/arm64 manifest's layers; an amd64 Linux runner fetches only the amd64 manifest's layers. Add `-p darwin/arm64`, `-p darwin/amd64`, or `-p windows/amd64` the same way for the rest of the matrix.

<Terminal src="/casts/authoring/package-multi-platform.cast" title="Publishing a multi-platform package" collapsed />

The recording also runs [`ocx index update`][cmd-index-update] and [`ocx package install`][cmd-install] after the second push so you can see the consumer side: a single tag, the right platform's layers fetched, the binary on the candidate symlink ready for `ocx package exec`.

## Use the Same Metadata Across Platforms {#metadata}

The default and recommended pattern: ship one `metadata.json` that covers every platform. Env entries, dependencies, entrypoints — all of them apply uniformly. The platform-specific bits live in the archive, not in the metadata.

When platforms genuinely diverge — say, Windows needs different env keys, or a platform has different entry-point names — the [`ocx_mirror`][in-tree-mirror-spec] spec accepts a per-platform metadata override. Hand-driven publishers can pass `--metadata <path>` per push and use a different file each time. See [`mirrors/cmake/mirror.yml`][mirror-cmake]'s `metadata.platforms` block for the canonical example.

## The Image Index Is Stable {#stability}

Each per-platform manifest's digest depends only on its own bytes. Push the *same archive bytes* twice and the manifest digest is identical — no platform-side rebuild churn. (Re-running `ocx package create` produces a different archive on each invocation; see [Bundle anatomy → stable archives][authoring-bundle-anatomy-stable] for why.) The index manifest's digest is a function of its descriptors, so it changes when (and only when) you add a platform or push a new build for an existing platform. That stability is what lets [`--cascade`][authoring-building-pushing-cascade] work cleanly across multi-platform releases — the rolling tag points at the index, the index points at per-platform manifests, every layer caches independently.

## See Also {#see-also}

- [`ocx package push` reference][cmd-package-push]
- [Storage in depth — multi-layer packages][in-depth-storage-multi-layer]
- [Versioning in depth — platforms][in-depth-versioning-platforms]
- [Building & pushing][authoring-building-pushing] — cascade, layer reuse, BYO archives
- [Migration patterns][authoring-migration] — `ocx_mirror` per-platform spec

<!-- external -->
[oci-image-index]: https://github.com/opencontainers/image-spec/blob/main/image-index.md
[mirror-cmake]: https://github.com/ocx-sh/ocx/blob/main/mirrors/cmake/mirror.yml
[in-tree-mirror-spec]: https://github.com/ocx-sh/ocx/tree/main/mirrors

<!-- commands -->
[cmd-package-create]: ../reference/command-line.md#package-create
[cmd-package-push]: ../reference/command-line.md#package-push
[cmd-index-update]: ../reference/command-line.md#index-update
[cmd-install]: ../reference/command-line.md#package-install

<!-- in-depth -->
[in-depth-storage-multi-layer]: ../in-depth/storage.md#multi-layer
[in-depth-versioning-platforms]: ../in-depth/versioning.md#platforms

<!-- authoring -->
[authoring-building-pushing]: ./building-pushing.md
[authoring-building-pushing-cascade]: ./building-pushing.md#cascade
[authoring-bundle-anatomy-stable]: ./bundle-anatomy.md#stable
[authoring-migration]: ./migration.md
[authoring-bundle-sidecars]: ./bundle-anatomy.md#sidecars

<!-- mirror pipeline -->
[mirror-pipeline]: https://github.com/ocx-sh/ocx/tree/main/mirrors
