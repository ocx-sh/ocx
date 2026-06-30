---
outline: deep
---
# Bundle Anatomy

A package on the registry is two artefacts that travel together: a tar archive holding the files your tool needs at runtime, and a sidecar `metadata.json` describing how to install and configure them. This page covers the publisher decisions that shape both — what to put in the archive, where to declare those choices, and how `ocx package create` produces a stable bundle ready to push.

## What Goes in the Archive {#what-goes-in}

Most upstream toolchains ship a single root directory inside their archive — `cmake-3.28.1-linux-x86_64/bin/cmake`, `node-v20.0.0-linux-x64/bin/node`. That layout keeps the archive self-describing for humans, but a package manager needs a flat surface so the package's declared `PATH` entry (`${installPath}/bin`) lands consumers' `bin/` without knowing the upstream's naming convention.

OCX leaves the choice in your hands. Either pre-flatten the tree before bundling, or keep the upstream layout and tell OCX to strip the wrapper at install time via [`strip_components`][strip-components]. Repackaging upstream archives unchanged — the [migration patterns guide][authoring-migration] walks the most common transformations — avoids re-bundling cost and keeps your release pipeline as a thin wrapper over the upstream artifact.

OCX assembles every installed package under `content/` ([three-tier storage][in-depth-storage]) — that is the post-install layout, not a publisher-side archive requirement. What goes inside the archive is up to you. The convention most archive-based mirrors follow is `bin/` (executables) plus any data the tool reads at runtime; pair that with `strip_components: 1` for upstreams that ship a single wrapper directory. Pre-built single binaries can ship without any wrapper at all — set `strip_components: 0` so the binary lands at the archive root, or use [`type: binary`][reference-bundle-binary] for sidecar-style downloads.

## Stable Archives {#stable}

Archive creation is sensitive to inputs that vary between runs. [`ocx package create`][cmd-package-create] sorts entries by filename so directory iteration order does not leak in, but file mtimes are read from the filesystem and embedded in the tar headers — re-running the bundle on a fresh checkout, on a different host, or after `touch` will produce different bytes and a different digest. Treat the archive digest as **a property of one specific run**, not of the source tree.

That matters because [layer reuse][in-depth-storage-layers] hinges on digest equality: bundle the same content twice and you get two layers in the registry, two downloads at install time, and zero dedup. The fix is not byte-reproducibility — it is to capture the digest after the first push and reference it explicitly on every subsequent push:

```sh
ocx package create build -m metadata.json -o mytool-1.0.0.tar.xz
ocx package push -n -p linux/amd64 -m metadata.json mytool:1.0.0 mytool-1.0.0.tar.xz

# Record the layer digest. The on-disk archive is the same blob the registry stores,
# so a local sha256 matches the layer digest the registry will hold.
BASE_DIGEST=$(sha256sum mytool-1.0.0.tar.xz | awk '{print $1}')

# Later releases reuse the layer by digest — no re-bundle, no re-upload.
ocx package push -p linux/amd64 -m metadata.json mytool:1.0.1 sha256:${BASE_DIGEST}.tar.xz
```

The `sha256:<hex>.<ext>` form on [`ocx package push`][cmd-package-push] is the supported way to point a new manifest at a layer that already lives in the target registry. See [Reusing layers across packages][authoring-layer-reuse] for the full pattern, including multi-layer packages with a stable base and a per-release top layer.

<Terminal src="/casts/authoring/package-create.cast" title="Bundling a package with ocx package create" collapsed />

## Choosing the Compression {#compression}

The archive extension picks the codec: `.tar.xz` (LZMA, default), `.tar.gz` (Gzip), or `.tar.zst` (Zstandard). LZMA typically produces noticeably smaller archives for binary payloads, while Gzip decompresses faster on tiny archives where CPU time dominates. Zstandard ([OCI 1.1][oci-media-types]) sits between them: archives close to LZMA's size at its lower presets, but decompresses several times faster — the win when consumers install on cold caches. Layers are downloaded once and cached locally per digest ([three-tier storage][in-depth-storage]), so the size win usually outweighs the speed difference at registry scale.

Compression level (`fast`, `default`, `best`) trades publish-time CPU for archive size. The threads flag (`-j`) parallelises LZMA and Zstandard compression (Gzip is always single-threaded); `0` auto-detects up to 16 cores.

::: tip Stick with the defaults
`.tar.xz` at level `default` is the right call for almost every payload. Move to `best` only after a measurement shows the savings justify the publish-time cost.
:::

## Sidecar Metadata and Inferred Names {#sidecars}

[`ocx package create`][cmd-package-create] follows a sidecar convention so the metadata travels with the archive. Pass `-m metadata.json` and the command copies that file to a sibling next to the output archive, named after the archive itself:

```sh
ocx package create build -m metadata.json -o mytool-1.0.0.tar.xz
```

Produces:

<Tree :collapsible="false">
  <Node name="mytool-1.0.0.tar.xz" />
  <Node name="mytool-1.0.0-metadata.json">
    <Description>sidecar — same bytes as the input metadata.json</Description>
  </Node>
</Tree>

[`ocx package push`][cmd-package-push] uses the same convention in reverse. Omit `-m` from the push and OCX looks next to the first file layer for `<stem>-metadata.json`. So the round-trip stays declarative — declare metadata once at create time, reference the archive at push time, and the metadata follows along automatically.

The convention extends to the output filename when `-o` is a directory and `-i` / `-p` provide the identifier and platform:

```sh
ocx package create build -i mytool:1.0.0 -p linux/amd64 -m metadata.json -o .
```

Produces:

<Tree :collapsible="false">
  <Node name="mytool-1.0.0-linux-amd64.tar.xz" />
  <Node name="mytool-1.0.0-linux-amd64-metadata.json">
    <Description>sidecar — picked up automatically by per-platform pushes</Description>
  </Node>
</Tree>

The pattern is `<name>-<tag>-<os>-<arch>.tar.xz`, where `<name>` is the last segment of the OCI repository (so `myorg/mytool:1.0.0` infers `mytool-1.0.0-…`, not `myorg-mytool-1.0.0-…`). When `-o .` triggers inference, the codec is always LZMA (`.tar.xz`); to pick a different codec, pass an explicit filename to `-o` (e.g. `-o mytool-1.0.0.tar.gz`). That naming is what every per-platform push picks up automatically — see [Multi-Platform Packages][authoring-multi-platform] for the worked example. The same `-o .` convention is what the [`ocx_mirror`][in-tree-mirrors] pipeline runs in production.

## Stripping Upstream Wrappers {#strip-components}

When repackaging upstream archives, [`strip_components`][strip-components] mirrors [`tar --strip-components`][gnu-tar-strip]: declare it in `metadata.json`, and OCX removes that many leading path segments while extracting each layer. A value of `1` collapses `cmake-3.28/bin/cmake` to `bin/cmake`. Default when omitted is `0` (no stripping). See the [metadata reference][strip-components] for every constraint.

```json
{
  "type": "bundle",
  "version": 1,
  "strip_components": 1
}
```

## See Also {#see-also}

- [`ocx package create` reference][cmd-package-create]
- [Storage in depth — packages and layers][in-depth-storage]
- [Building and pushing][authoring-building-pushing] — taking the bundle to the registry
- [Migration patterns][authoring-migration] — repackaging Homebrew, GitHub Releases, raw tarballs

<!-- external -->
[in-tree-mirrors]: https://github.com/ocx-sh/ocx/tree/main/crates/ocx_mirror
[gnu-tar-strip]: https://www.gnu.org/software/tar/manual/html_section/transform.html
[oci-media-types]: https://github.com/opencontainers/image-spec/blob/v1.1.0/media-types.md

<!-- commands -->
[cmd-package-create]: ../reference/command-line.md#package-create
[cmd-package-push]: ../reference/command-line.md#package-push

<!-- reference -->
[strip-components]: ../reference/metadata.md#extraction-strip-components
[reference-bundle-binary]: ../reference/metadata.md#format-top-level

<!-- in-depth -->
[in-depth-storage]: ../in-depth/storage.md
[in-depth-storage-layers]: ../in-depth/storage.md#layers

<!-- authoring -->
[authoring-building-pushing]: ./building-pushing.md
[authoring-layer-reuse]: ./building-pushing.md#layer-reuse
[authoring-migration]: ./migration.md
[authoring-multi-platform]: ./multi-platform.md
