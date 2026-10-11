---
title: "Declare dependencies"
description: "Decide when a package should depend on another package, pin it by digest and choose the edge visibility."
---
<!-- doc_type: how-to -->
<!-- doc_tier: everyday -->

By the end of this page your `metadata.json` lists the packages yours needs at run time, each pinned and given the narrowest visibility that works. You need a bundle and a `metadata.json`. OCX resolves the graph at install time and composes each dependency's environment by the visibility you choose. The [dependencies reference][reference-dependencies] lists every field, and [dependencies in depth][in-depth-dependencies] owns the resolution model.

## Decide between bundling and depending {#when}

Bundling ships the dependency's bytes inside your archive, so every install carries them. Depending points at another package by digest. The consumer fetches it once and shares it across every package that references it.

Declare a dependency when at least one of these is true:

- **The dependency is large or commonly needed.** Bundling [Node.js][nodejs] in every [npm][npm] wrapper re-ships roughly 30 MB per package. A declared `ocx.sh/nodejs/node:24` dependency gives one cached install across all packages that pin the same digest.
- **You need a specific version of someone else's tool.** Wrapping [`terraform`][terraform] with organisation defaults means pinning the upstream build. A declared dependency records the pin in metadata, so consumers can audit it without unpacking your archive.
- **Your package runs the dependency at run time.** A wrapper that shells out to [`cmake`][cmake] needs it on disk in a known place. `${deps.cmake.installPath}` provides that.

Bundle instead when the dependency is tiny, single-use or coupled to your build, such as a vendored library you patched.

## Pin by digest {#pinning}

Every published dependency is pinned by [OCI digest][oci-digest], never a version range or a floating tag. Two consumers installing on different days get the same graph, because the digest is immutable. The digest must name a platform manifest and never an [OCI Image Index][oci-image-index]. See [Manifest Pins, Not Index Pins][reference-manifest-pins] for why an index pin returns 404 once the dependency's publisher pushes again.

You do not write the digest by hand. In the sidecar you author, an identifier needs only an explicit registry:

```json
{
  "dependencies": [
    { "identifier": "ocx.sh/kitware/cmake:3.28", "visibility": "public" }
  ]
}
```

[`ocx package create`][cmd-package-create] resolves every tag-only dependency against the selected index for the platform you pass with `-p`, and rewrites the sidecar with the pin. The full first-push flow is in [Build and push][authoring-building-pushing]:

```sh
ocx package create build -m metadata.json -o mytool-1.0.0.tar.xz -p linux/amd64 -i ghcr.io/acme/mytool:1.0.0
```

The rewritten sidecar then reads:

```json
{
  "dependencies": [
    { "identifier": "ocx.sh/kitware/cmake:3.28@sha256:a1b2c3d4e5f6...", "visibility": "public" }
  ]
}
```

`create` leaves your `metadata.json` unchanged and writes the rewritten sidecar as `mytool-1.0.0-metadata.json` beside the archive. Push that file, not the one you wrote. [`ocx package push`][cmd-package-push] finds it beside the archive without `-m`.

The push resolves nothing. It checks that every dependency carries a manifest pin and refuses to publish with exit 65 otherwise. Tag-only dependencies, index pins and a missing platform pin all fail that check.

Index selection in `create` follows the usual routing. [`--remote`][arg-remote] goes straight to the registry. [`--offline`][arg-offline] and [`--frozen`][arg-frozen] refuse to resolve a tag missing from the local index, with exit 81. The default tries the local index first and fetches on a miss. A tag that does not exist in the selected index fails with exit 79.

Every identifier needs an explicit registry, because the consumer's default registry may differ from yours. The digest is optional while you author and mandatory once published. A fully pinned `metadata.json` parses as authoring metadata unchanged, so `create` and `push` accept it as-is.

### Depend on a package with several platforms {#pinning-per-platform}

A package built with [`ocx package create --platform any`][cmd-package-create] can depend on a package that ships a manifest per platform. The dependency must also offer an `any` manifest. The `create` step then resolves a single manifest digest, bare on the identifier, as it does for a concrete platform. A dependency that offers only platform-specific manifests fails `create` with exit 65 and names the dependency. [Publish for several platforms][authoring-multi-platform] and [Platforms][reference-platforms] cover the `any` rule.

## Rename a dependency with `name` {#name-field}

The placeholder `${deps.NAME.installPath}` takes `NAME` from the last path segment of the repository. Both `ocx.sh/kitware/cmake` and `ocx.sh/myorg/cmake` become `cmake`, which collides when two dependencies share a final segment. The optional `name` field overrides the lookup key:

```json
{
  "dependencies": [
    { "identifier": "ocx.sh/myorg/cmake@sha256:...",    "name": "myorg_cmake" },
    { "identifier": "ocx.sh/upstream/cmake@sha256:...", "name": "cmake" }
  ],
  "env": [
    { "key": "BUILD_TOOL",   "type": "constant", "value": "${deps.cmake.installPath}/bin/cmake",  "visibility": "public" },
    { "key": "PATCH_SCRIPT", "type": "constant", "value": "${deps.myorg_cmake.installPath}/bin/patch.sh", "visibility": "private" }
  ]
}
```

A `name` must match `^[a-z0-9][a-z0-9_-]*$` and stay within 64 characters, the same rule as entry-point names. Use it too when a segment is awkward to type, such as `my-very-long-tool-name`.

## Choose the edge visibility {#edge-visibility}

Each dependency carries a `visibility` field, distinct from the [entry visibility on env entries][authoring-env-surface]. It controls how the dependency's environment propagates. Four values map onto two axes: private means the package's own runtime sees it, and interface means consumers see it.

| Value | Private surface | Interface surface | Use case |
|---|---|---|---|
| `sealed` (default) | No | No | Structural dependency, reached only through `${deps.NAME.installPath}` with no env propagation. |
| `private` | Yes | No | The package's own launchers need the dependency's env, and consumers do not. |
| `public` | Yes | Yes | Both the package and its consumers need the dependency's env. |
| `interface` | No | Yes | Meta-packages that forward env to consumers without using it. |

Most dependencies should stay `sealed`. A wrapper that hardcodes `${deps.cmake.installPath}/bin/cmake` accesses cmake by path, so cmake's env need not leak. Use `private` when your own launchers invoke the dependency with its env applied. Use `public` when consumers should see it too, as in a [`mise`-style][mise] meta-package.

Pick the narrowest visibility that covers your run-time need. Diamond dependencies and `through_edge` propagation are in [dependencies in depth][in-depth-dependencies]. The resolved env that OCX writes at install time is in [environments in depth][in-depth-environments].

:::note[Inspired by CMake's `target_link_libraries`]
The vocabulary follows [CMake's PUBLIC/PRIVATE/INTERFACE link visibility][cmake-tll]. OCX applies the same intuition to run-time env propagation. Treat the analogy as a memory aid, because the behaviours are not identical.
:::

## Order the dependencies {#ordering}

The order of the `dependencies` array is the canonical order for env composition. The first entry applies first, and each later dependency layers on top. Later `path` entries prepend onto earlier ones, and `constant` entries follow the [last-wins rule][in-depth-environments-last-wins].

Put dependencies whose env should win near the end of the array. Transitive dependencies keep topological order, and the importing package's own env always emits last, so its prepends have the highest priority on PATH. The full rule is [`#dependencies-ordering`][reference-deps-ordering].

## See also {#see-also}

- [Dependencies reference][reference-dependencies] for every field and constraint
- [Dependencies in depth][in-depth-dependencies] for resolution, diamond dedup and garbage collection
- [Publish a package to a registry][authoring-building-pushing] for the `create` resolves and `push` gates split
- [Publish for several platforms][authoring-multi-platform] for dependencies on multi-platform builds
- [Declare the environment][authoring-env-surface] for entry visibility, which is distinct from edge visibility

<!-- external -->
[oci-digest]: https://github.com/opencontainers/image-spec/blob/main/descriptor.md#digests
[oci-image-index]: https://github.com/opencontainers/image-spec/blob/main/image-index.md
[cmake]: https://cmake.org/
[cmake-tll]: https://cmake.org/cmake/help/latest/command/target_link_libraries.html
[mise]: https://mise.jdx.dev/
[nodejs]: https://nodejs.org/
[npm]: https://www.npmjs.com/
[terraform]: https://developer.hashicorp.com/terraform

<!-- reference -->
[reference-dependencies]: ../reference/metadata/dependencies.md#dependencies
[reference-deps-ordering]: ../reference/metadata/dependencies.md#dependencies-ordering
[reference-manifest-pins]: ../explanation/dependencies.mdx#manifest-pins
[reference-platforms]: ../reference/platforms.md

<!-- in-depth -->
[in-depth-dependencies]: ../explanation/dependencies.mdx
[in-depth-environments]: ../explanation/environments.mdx
[in-depth-environments-last-wins]: ../explanation/environments.mdx#last-wins

<!-- commands -->
[cmd-package-create]: ../reference/command-line/package-publishing.md#package-create
[cmd-package-push]: ../reference/command-line/package-publishing.md#package-push
[arg-remote]: ../reference/command-line/index.md#arg-remote
[arg-offline]: ../reference/command-line/index.md#arg-offline
[arg-frozen]: ../reference/command-line/index.md#arg-frozen

<!-- authoring -->
[authoring-env-surface]: ./env-surface.md
[authoring-building-pushing]: ./build-and-push.mdx#first-push
[authoring-multi-platform]: ./multi-platform.mdx
