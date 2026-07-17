---
outline: deep
---
# Dependencies

A binary tool rarely runs in isolation. A web application needs a JavaScript runtime; a build tool needs a compiler; a Maven build needs both Java and Maven on `PATH`. Most package managers solve this with version-range resolvers and project-level lockfiles — useful when source compilation produces unique builds, but heavyweight when the upstream is already a content-addressed binary in an OCI registry.

OCX takes a deliberately narrow approach. Every dependency is pinned to an exact <Tooltip term="OCI digest">A SHA-256 fingerprint that identifies a specific build of a package. The publisher records the exact digest they tested against — not a version range, not a "latest" tag. This means the dependency graph is fully determined by the package metadata alone.</Tooltip> by the publisher; there are no version ranges, no resolution algorithm, no auto-updates. The dependency graph is a flat list of digests baked into each package's metadata. This page explains *why* the surface is so small, *how* transitive resolution works, and where the related design — visibility, environment composition, GC — lives. The user-facing surface — auto-fetch, `ocx exec`, `ocx package deps` — lives in the [Dependencies section of the user guide][user-deps].

## Manifest Pins, Not Index Pins {#manifest-pins}

A dependency's digest is not free to identify anything with the right bytes — it must reference a platform [OCI manifest][oci-image-manifest], never an [OCI Image Index][oci-image-index]. Pinning a dependency's index digest looks appealing at first: a single identifier could then resolve to whichever platform an installing host needs, the same way an ordinary package reference does at install time.

That reasoning breaks the moment the dependency's publisher pushes again. [Cascade publishing][in-depth-versioning-cascades] rewrites a tag's index on every platform push — the previous index digest becomes untagged and the registry's garbage collector reclaims it on its next sweep. A dependency pinned to that now-untagged index digest starts 404ing, permanently, the first time its publisher adds a platform or re-releases an existing one. The child platform manifests have no such problem: every successor index still references them, so they survive indefinitely.

::: info The same rule governs the project lock
[`ocx.lock`][in-depth-project-lock] applies the identical rule to project toolchains: it records each tool's **per-platform leaf manifest digest**, deliberately never the index digest — see [Lock format][in-depth-project-lock-format]. Package dependencies follow the same rule, for the same reason: the index digest is a moving target across a publisher's release history, the leaf manifest digest is not.
:::

## Create Resolves, Push Gates {#create-push-split}

Enforcing the manifest-pin rule by hand-editing digests does not scale, so OCX splits the publishing pipeline into two responsibilities. [`ocx package create --platform`][cmd-package-create] is the compiler: the publisher writes each dependency tag-only in the `metadata.json` sidecar, and `create` resolves it against the selected index into a manifest pin, rewriting the sidecar in canonical form. [`ocx package push`][cmd-package-push] is a pure gate: it makes no resolution decisions, verifying only that every declared dependency already carries a manifest pin covering the single platform the invocation publishes — and refusing to publish otherwise.

The split keeps the published guarantee cheap to check. `push` never decides *which* manifest a dependency should resolve to; it confirms a pin already exists and that it actually resolves, in its registry, to a manifest rather than an index. Resolution — the part that needs index access, `--offline`/`--remote`/`--frozen` routing, and disambiguation between candidate manifests — happens once, at `create` time, where the publisher can inspect and commit the result before it ever reaches a registry.

A package built with `--platform any` compounds this: it performs no platform-specific resolution of its own, so it can only depend on dependencies that themselves offer an `any` build — a dependency with no `any` manifest blocks `create` outright, naming the offending dependency, rather than narrowing an installable set. See [Multi-Platform Packages][authoring-multi-platform] for the publisher workflow and the [Per-Platform Pins reference][reference-per-platform-pins] for the pin-map shape and the compatibility rule that projects it.

A pin — direct digest or the `any`-keyed map entry — is a snapshot of the publisher's platform coverage at the moment `create` ran, not a live query. If a dependency's own publisher later adds an `any` build where none existed before, packages that depend on it do not retroactively gain it; re-running `create` and `push` is what picks it up. This mirrors [the project lock's pin-preservation guarantee][in-depth-project-lock-pin-preservation]: neither layer silently advances a pin nobody asked to re-resolve.

## Resolution {#resolution}

When you pull or install a package that declares dependencies, OCX fetches every required package transitively — dependencies of dependencies, and so on — and stores them in the [package store][in-depth-storage-packages]. The process is fully automatic and requires no extra flags:

```shell
ocx package pull webapp:2.0
```

If `webapp:2.0` declares dependencies on `nodejs:24` and `bun:1.3`, all three packages end up in the package store. Only `webapp:2.0` is the package you explicitly requested — the dependencies are implementation details, fetched and stored but not surfaced as top-level installs.

Each declared dependency is identified by digest, so the publisher's recorded graph is the complete truth. Two builds of `webapp:2.0` that pin different `nodejs` digests resolve to *different* dependency closures even if the surface tag (`nodejs:24`) is the same.

::: info Resolution is publisher-provided, not solved
[Nix derivations][nix] take the same approach: every input is pinned by hash, and the store is content-addressed. There are no version ranges — the exact input is determined at build time. [Go modules][go-modules] use Minimum Version Selection with a `go.sum` integrity file — deterministic, but with a resolution algorithm. [Homebrew][homebrew] uses floating name-only dependencies with no pinning at all.

OCX sits closest to Nix in philosophy — exact pins, no resolution — but without the functional language and the build system. The dependency declaration is a flat list of digests in a JSON file.
:::

## Composition {#composition}

To actually *run* a package with its dependency environments configured, use [`ocx exec`][cmd-exec] (or [`ocx env`][cmd-env] to export the composed environment into a shell). [`ocx exec`][cmd-exec] composes the environments of all dependencies in <Tooltip term="topological order">Dependencies are applied before their dependents. If A depends on B, and B depends on C, the order is C → B → A. Among packages at the same level, alphabetical order (by identifier) is used as a tiebreaker so the result is deterministic.</Tooltip> before launching the command. Dependencies come first, then the package you requested.

Scalar variables (like `JAVA_HOME`) follow last-writer-wins; accumulator variables (like `PATH`) merge naturally — each dependency's `bin/` directory is prepended in order.

The full algorithm — TC walk, edge filter, scalar vs. accumulator semantics, conflict reporting — is documented in [Environments][in-depth-environments] under [Composition Order][env-composition] and [Last-Wins Scalar Semantics][env-last-wins]. There is one canonical implementation; the design lives there.

## Visibility {#visibility}

Every package owns two environment surfaces: an **interface surface** (what consumers see by default) and a **private surface** (what the package's own launchers see at runtime). A build tool that wraps a compiler might need `CC` and `LD_LIBRARY_PATH` internally — those belong on the private surface. A shared Java runtime that every consumer needs goes on the interface surface.

Each dependency declares a `visibility` value (`sealed` / `private` / `public` / `interface`) controlling which surface it reaches. When dependencies form chains, visibility propagates inductively along edges; diamond resolution applies the most-open value per axis.

The complete model — including the per-axis truth table, edge filter, propagation rules, and worked examples — is documented in [Environments][in-depth-environments] under [Two Surfaces][env-two-surfaces], [Visibility Views][env-visibility], and [Edge Filter][env-edge-filter]. The `--self` flag selects which surface [`ocx exec`][cmd-exec] emits; the [Visibility Views section][env-visibility] holds the full truth table.

## Garbage Collection {#gc}

Dependencies are protected from garbage collection as long as any package that depends on them is still referenced. When you uninstall a package, its dependencies do not disappear immediately — they remain in the package store until no other installed package depends on them. [`ocx clean`][cmd-clean] removes only packages that have no references at all: no install symlinks and no dependent packages.

The mechanism is forward-only: when OCX installs a package with dependencies, it records each dependency as a forward-ref inside the dependent package's `refs/deps/` directory, pointing at the dependency's `content/`. Nothing is written into the dependency's own `refs/symlinks/` — the dependency is protected by the *liveness of its dependents*, not by a back-reference inside itself.

The full reachability walk — how `refs/symlinks/`, `refs/deps/`, `refs/layers/`, and `refs/blobs/` compose into one BFS sweep — lives in [Storage → Garbage Collection][in-depth-storage-gc].

## Scope {#scope}

OCX dependencies are deliberately simple — they solve a specific problem without introducing the complexity of a full-featured dependency resolver.

**No version ranges.** A dependency is pinned to an exact digest. There is no "find me any Java >= 21" logic. The publisher chose a specific build, tested against it, and recorded it. This is what makes the dependency graph fully reproducible: the metadata is the complete truth, regardless of what the registry contains today.

**No automatic updates.** When a dependency gets a security patch, the publisher must release a new version of their package with the updated digest. This is a deliberate tradeoff — reproducibility over convenience. Future tooling will help publishers detect when their pinned dependencies have newer builds available.

**Not a project-level lockfile.** Dependencies live in package metadata. They describe what a *single package* needs, not what a *project* needs. A project-level configuration file with version resolution and lock semantics is a separate concept for a future release.

The result: the dependency surface is a strict subset of what a general-purpose package manager exposes. OCX deliberately leaves resolution and lockfile composition to the publisher and to higher-level tooling — the runtime guarantee is that *whatever the publisher pinned is what the consumer gets*.

## See Also

- [Dependencies section in the user guide][user-deps] — how-to: pull, exec, deps, conflict warnings
- [Environments][in-depth-environments] — composition algorithm, visibility model, last-wins semantics
- [Storage → Garbage Collection][in-depth-storage-gc] — forward-ref reachability walk
- [Entry Points][in-depth-entry-points] — generated launchers that embed the private surface
- [Declaring dependencies][authoring-dependencies] — the publisher workflow: write tag-only, `create` resolves
- [Building and pushing][authoring-building-pushing] — the `create`/`push` split, gate errors, exit codes
- [Multi-Platform Packages][authoring-multi-platform] — per-platform pin maps, target-set intersection
- [Dependencies reference][reference-dependencies] — sidecar field shapes, manifest-pin rule

<!-- external -->
[nix]: https://nixos.org/
[go-modules]: https://go.dev/ref/mod
[homebrew]: https://brew.sh/
[oci-image-index]: https://github.com/opencontainers/image-spec/blob/main/image-index.md
[oci-image-manifest]: https://github.com/opencontainers/image-spec/blob/main/manifest.md#image-manifest

<!-- commands -->
[cmd-exec]: ../reference/command-line.md#exec
[cmd-env]: ../reference/command-line.md#env
[cmd-clean]: ../reference/command-line.md#clean
[cmd-package-create]: ../reference/command-line.md#package-create
[cmd-package-push]: ../reference/command-line.md#package-push

<!-- reference -->
[reference-dependencies]: ../reference/metadata.md#dependencies
[reference-per-platform-pins]: ../reference/metadata.md#dependencies-per-platform-pins

<!-- authoring -->
[authoring-dependencies]: ../authoring/dependencies.md
[authoring-building-pushing]: ../authoring/building-pushing.md
[authoring-multi-platform]: ../authoring/multi-platform.md

<!-- internal -->
[user-deps]: ../user-guide.md#dependencies
[in-depth-environments]: ./environments.md
[in-depth-entry-points]: ./entry-points.md
[in-depth-storage-packages]: ./storage.md#packages
[in-depth-storage-gc]: ./storage.md#gc
[in-depth-versioning-cascades]: ./versioning.md#cascades
[in-depth-project-lock]: ./project.md#lock
[in-depth-project-lock-format]: ./project.md#lock-format
[in-depth-project-lock-pin-preservation]: ./project.md#pin-preservation
[env-composition]: ./environments.md#composition-order
[env-last-wins]: ./environments.md#last-wins
[env-two-surfaces]: ./environments.md#two-surfaces
[env-visibility]: ./environments.md#visibility-views
[env-edge-filter]: ./environments.md#edge-filter
