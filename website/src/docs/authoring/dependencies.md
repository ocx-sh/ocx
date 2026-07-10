---
outline: deep
---
# Declaring Dependencies

When your tool needs to find another tool on disk at runtime — a runtime, a shared toolchain, a configuration generator — declare it as a dependency. OCX resolves the graph at install time, hardlinks every dependency's content into the consumer's environment, and composes their env surfaces according to the visibility you choose. This page covers the publisher decisions: when to declare a dependency at all, how to pin it, and how visibility changes what propagates to consumers.

## When to Declare {#when}

Bundle vs. depend is the first question. Bundling means shipping the dependency's bytes inside your own archive — every install carries them. Depending means pointing at another package by digest — the consumer fetches it once and shares it across every package that references it.

Reach for a declared dependency when at least one of these is true:

- **The dependency is large or commonly needed.** Bundling [Node.js][nodejs] inside every [npm][npm]-tool wrapper would mean re-shipping the same ~30 MB `node-vXX.x.x-linux-x64.tar.xz` (or ~57 MB Gzip equivalent) per tool. A declared `ocx.sh/nodejs:24` dependency means one cached install across all tools that pin the same digest.
- **You need a specific version of someone else's tool.** Wrapping [`terraform`][terraform] to add organisation defaults means pinning the upstream `terraform` build — a declared dependency captures that pin in metadata, so consumers can audit it without unpacking your archive.
- **Your tool genuinely runs the dependency at runtime.** A wrapper that shells out to [`cmake`][cmake] needs `cmake` on disk in a known place — `${deps.cmake.installPath}` provides that.

Bundling stays the right call when the dependency is tiny, single-use, or version-coupled to your build (a vendored library you patched, for example).

## Pinning by Digest {#pinning}

Every published dependency is pinned by [OCI digest][oci-digest] — never a version range, never a floating tag. Two consumers installing the same package on different days, against a registry whose tags have moved, get the same dependency graph because the digest is immutable. The digest must reference a platform manifest, never an [OCI Image Index][oci-image-index] — see [Manifest Pins, Never Index Pins][reference-manifest-pins] for why an index-pinned dependency eventually 404s once the dependency's publisher pushes again.

You do not write that digest by hand. In the `metadata.json` sidecar you author, a dependency identifier needs only an explicit registry — the digest is optional:

```json
{
  "dependencies": [
    { "identifier": "ocx.sh/cmake:3.28", "visibility": "public" }
  ]
}
```

[`ocx package create --platform <PLATFORM>`][cmd-package-create] resolves every tag-only dependency against the selected index and rewrites the sidecar in place with the resolved pin:

```json
{
  "dependencies": [
    { "identifier": "ocx.sh/cmake:3.28@sha256:a1b2c3d4e5f6...", "visibility": "public" }
  ]
}
```

Commit the rewritten sidecar — the one `create` produced, not the one you hand-wrote — alongside the archive, and hand both to [`ocx package push`][cmd-package-push]. `push` makes no resolution decisions of its own: it reads the sidecar, verifies every dependency already carries a manifest pin, and refuses to publish (exit 65) anything still tag-only, still carrying an index pin, or missing a platform's pin.

`create`'s index selection follows the same routing as every other resolution: [`--remote`][arg-remote] goes straight to the registry, [`--offline`][arg-offline] and [`--frozen`][arg-frozen] refuse to resolve a tag that is not already in the local index (exit 81), and the default resolves locally first, falling back to a fetch when the dependency is not yet cached. A dependency tag that does not exist in the selected index fails with exit 79.

The full identifier rules — required registry component, identifier syntax, the "no version ranges" decision — live in the [dependencies reference][reference-dependencies]. The short version: every dependency identifier carries an explicit registry; the digest is optional while you author, mandatory once published.

### When a Dependency Ships Multiple Platforms {#pinning-per-platform}

A package built with [`ocx package create --platform any`][cmd-package-create] — platform-agnostic content, like a script — can still depend on a package that ships different manifests per platform (the native binary the script wraps, for example). `create` resolves that case into a per-dependency `platforms` pin map instead of a single digest, and narrows your own package's target-platform set to whatever every dependency actually covers. The map shape, the lookup rule for libc-tagged platforms, and the fan-out this produces at push time are covered in [Multi-Platform Packages][authoring-multi-platform] and the [Per-Platform Pins reference][reference-per-platform-pins].

## When You Need a `name` Override {#name-field}

By default, the placeholder `${deps.NAME.installPath}` derives `NAME` from the last path segment of the OCI repository — `ocx.sh/cmake` becomes `cmake`, `ocx.sh/myorg/cmake` becomes `cmake`. That collides whenever two dependencies share that final segment, or when the segment is awkward to type (`my-very-long-tool-name`). The optional `name` field overrides the lookup key:

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

The `name` override must itself satisfy `^[a-z0-9][a-z0-9_-]*$` and stay at most 64 characters — same rule as entry-point names. Identifiers always carry an explicit registry (the [pinning rules][reference-dependencies] reject `myorg/cmake@sha256:…` without one).

## Choosing Edge Visibility {#edge-visibility}

Each dependency entry carries a `visibility` field, distinct from the [entry visibility on env entries][authoring-env-surface]. This one controls how the dependency's environment propagates through the chain. Four values map onto two axes (private = the package's own runtime sees it; interface = consumers see it):

| Value | Private surface | Interface surface | Use case |
|---|---|---|---|
| `sealed` (default) | No | No | Structural dependency — content accessed only via `${deps.NAME.installPath}`, no env propagation. |
| `private` | Yes | No | Package's own shims need the dep's env; consumers don't. |
| `public` | Yes | Yes | Both the package and consumers need the dep's env. |
| `interface` | No | Yes | Meta-packages that forward env to consumers without using it themselves. |

The default — `sealed` — is the right pick for most dependencies. A wrapper that hardcodes `${deps.cmake.installPath}/bin/cmake` in an entry-point target doesn't need cmake's env to leak; it accesses cmake by path. Promote to `private` when your own launchers need to invoke the dep with its env applied. Promote to `public` when consumers should also see it (a [`mise`-style][mise] meta-package that exposes its components directly).

The composition mechanic — what happens at diamond dependencies, how `through_edge` propagates visibility transitively — lives in [dependencies in depth][in-depth-dependencies]; the runtime artifact OCX writes during install (`resolve.json`, capturing the resolved env) is documented in [environments in depth][in-depth-environments]. The publisher decision compresses to one rule: pick the *narrowest* visibility that still covers your runtime need.

::: info Inspired by CMake's `target_link_libraries`
The vocabulary maps cleanly to [CMake's PUBLIC/PRIVATE/INTERFACE link visibility][cmake-tll] — what a target *publishes to its build consumers*. OCX applies the same intuition to the runtime env propagation graph. Use the analogy as a memory aid; the behaviours are not identical.
:::

## Ordering Matters {#ordering}

The order of entries in the `dependencies` array defines the canonical order for env composition. The first entry's environment is applied first, then each subsequent dependency layers on. `path`-type entries stack from later dependencies prepended onto earlier ones; `constant`-type entries follow the [last-wins rule][in-depth-environments-last-wins] within the composition.

When composing a non-trivial graph, put dependencies whose env should "win" closer to the end of the array — they overwrite earlier constants and prepend to earlier path lists. Transitive deps preserve topological order (deps before dependents), and the importing package's own env always emits last, so its prepends end up highest priority on PATH. The full ordering rule lives in [`#dependencies-ordering`][reference-deps-ordering].

## See Also {#see-also}

- [Dependencies reference][reference-dependencies] — every field, every constraint
- [Dependencies in depth][in-depth-dependencies] — resolution, diamond dedup, garbage collection
- [Building and pushing][authoring-building-pushing] — the `create` resolves / `push` gates split, gate errors
- [Multi-platform packages][authoring-multi-platform] — per-platform pin maps, target-set intersection
- [Env surface][authoring-env-surface] — entry visibility (distinct from edge visibility)
- [Migration patterns][authoring-migration] — when wrapping upstream tools introduces dep declarations

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
[reference-dependencies]: ../reference/metadata.md#dependencies
[reference-deps-ordering]: ../reference/metadata.md#dependencies-ordering
[reference-manifest-pins]: ../reference/metadata.md#dependencies-manifest-pins
[reference-per-platform-pins]: ../reference/metadata.md#dependencies-per-platform-pins

<!-- in-depth -->
[in-depth-dependencies]: ../in-depth/dependencies.md
[in-depth-environments]: ../in-depth/environments.md
[in-depth-environments-last-wins]: ../in-depth/environments.md#last-wins

<!-- commands -->
[cmd-package-create]: ../reference/command-line.md#package-create
[cmd-package-push]: ../reference/command-line.md#package-push
[arg-remote]: ../reference/command-line.md#arg-remote
[arg-offline]: ../reference/command-line.md#arg-offline
[arg-frozen]: ../reference/command-line.md#arg-frozen

<!-- authoring -->
[authoring-env-surface]: ./env-surface.md
[authoring-building-pushing]: ./building-pushing.md
[authoring-multi-platform]: ./multi-platform.md
[authoring-migration]: ./migration.md
