---
outline: deep
---
# Storage

Most package managers keep everything in a single mutable tree. Installing a new version silently replaces the old one, breaking anything that referenced the old path. They also hit the network on every operation, making offline use and reproducible CI awkward.

OCX takes the opposite approach. The data directory under `~/.ocx/` (configurable via [`OCX_HOME`][env-ocx-home]) is split into independent stores — each owns one concern, each is content-addressed where it matters, each can be reasoned about in isolation. This page explains the layout and *why* each store exists. The user-facing surface — what `ocx install` produces, where to find binaries, which paths are safe to embed — lives in the [Storage section of the user guide][user-storage].

## Stores {#stores}

Five stores plus a download staging area sit under `$OCX_HOME`:

<Tree>
  <Node name="~/.ocx/" icon="🏠" open>
    <Node name="packages/" icon="📦">
      <Description>immutable, content-addressed assembled packages</Description>
    </Node>
    <Node name="layers/" icon="🗂️">
      <Description>extracted OCI layers — shared across packages that reference the same layer</Description>
    </Node>
    <Node name="blobs/" icon="📋">
      <Description>raw OCI blobs — manifests, image indexes, referrers</Description>
    </Node>
    <Node name="tags/" icon="🏷️">
      <Description>local mirror of registry tag-to-digest mappings — no binaries</Description>
    </Node>
    <Node name="symlinks/" icon="🔀">
      <Description>stable symlinks safe to embed in shell profiles and configs</Description>
    </Node>
    <Node name="temp/" icon="🧪">
      <Description>download staging — cleaned on successful install</Description>
    </Node>
  </Node>
</Tree>

Each store has a single responsibility. Upgrading a package updates symlinks in `symlinks/` and adds an entry to `packages/`, but never touches `tags/`. The stores can be understood — and reasoned about — one at a time.

::: details Path component encoding
Registry names, repository names, and tags that appear as directory or file names are <Tooltip term="slugified">Characters outside `[a-zA-Z0-9._-]` are replaced with underscores. For example, `localhost:5000` becomes `localhost_5000`. This prevents the POSIX `PATH` separator (`:`) from corrupting environment variables that embed these paths.</Tooltip> before they become filesystem path components. Dots and hyphens are preserved so domain names (`ghcr.io`) and semantic versions (`3.28.1`) stay readable on disk.

Digest-addressed directories use a two-level shard (`sha256/{2hex}/{30hex}/`). Only 32 hex characters are encoded in the path; the full digest is written to a sibling `digest` file inside each entry and is the source of truth for identity.
:::

## Packages {#packages}

When OCX installs a package, the actual files land in `packages/`. The critical design decision: the storage path is derived from the content's <Tooltip term="SHA-256 digest">A 64-character hex fingerprint of the exact bytes in a file or archive. The same bytes always produce the same digest; any change — even a single bit — produces a completely different one. Used here as a content address: the path is uniquely determined by what the files contain.</Tooltip>, not from the package name or tag.

<Tree>
  <Node name="~/.ocx/packages/" icon="📦" open>
    <Node name="{registry}/" icon="📁" open-icon="📂" open>
      <Node name="sha256/ab/c123…/" icon="📁" open-icon="📂" open>
        <Description>one directory per unique package build</Description>
        <Node name="content/" icon="📂">
          <Description>package files — binaries, libraries, headers</Description>
        </Node>
        <Node name="entrypoints/" icon="🚀">
          <Description>generated launchers for declared entry points — one script per name</Description>
        </Node>
        <Node name="metadata.json" icon="📋">
          <Description>declared env vars and extraction options</Description>
        </Node>
        <Node name="refs/" icon="🔗">
          <Description>back-references to install symlinks — guards against deletion</Description>
        </Node>
      </Node>
    </Node>
  </Node>
</Tree>

This <Tooltip term="content-addressed layout">A storage scheme where every item's address is derived from a cryptographic hash of its contents. The same bytes always map to the same address — accidental duplication is impossible and silent corruption is detectable.</Tooltip> has two consequences:

- **Automatic deduplication.** If `cmake:3.28` and `cmake:latest` resolve to the same binary build, they share one directory on disk. Storage scales with the number of *distinct builds*, not the number of tags that reference them.
- **Immutability.** A path under `sha256/<shard>/` never changes its contents — safe to reference directly from scripts or cache layers that need a known, stable binary.

::: info Similar to the Nix store and Git objects
The [Nix package manager][nix] stores every package at `/nix/store/{hash}-name/` using the same principle: the path is a function of the content. This is what makes Nix derivations reproducible across machines — the same hash always means the same files. Git's internal object store (`.git/objects/`) works identically. OCX applies this model to OCI-distributed binaries.
:::

### Generated Launchers {#generated-launchers}

When a package's [`metadata.json`][metadata-ref] declares an [`entrypoints`][metadata-entry-points] array, OCX materializes a sibling `entrypoints/` directory at install time with one script per entry — a POSIX `.sh` launcher for Unix shells and a `.cmd` launcher for Windows. Each launcher bakes the digest-addressed `content/` path and re-enters via [`ocx launcher exec`][cmd-launcher-exec], so every invocation runs under the same clean-environment guarantee as [`ocx exec <package>`][cmd-exec].

Packages that declare no entrypoints never get an `entrypoints/` directory. See the [entry points guide][in-depth-entry-points] for the publisher workflow.

### Garbage Collection {#gc}

The `refs/symlinks/` subdirectory inside each package tracks every install symlink that currently points to it. That directory is the GC root signal — [`ocx clean`][cmd-clean] starts a reachability walk from every package with a live `refs/symlinks/` entry and follows forward-refs through all three tiers.

::: details How back-references work
When `ocx install cmake:3.28` creates the symlink `symlinks/…/cmake/candidates/3.28 → packages/…/sha256/ab/c123…/content`, it simultaneously writes a back-reference entry inside the package's `refs/symlinks/` directory. Removing the symlink via [`ocx uninstall`][cmd-uninstall] removes that back-reference entry. [`ocx clean`][cmd-clean] then builds a reachability graph across all three tiers: packages with live `refs/symlinks/` entries (and any profile content-mode references) are roots, and a single BFS pass follows each package's forward-refs in `refs/deps/`, `refs/layers/`, and `refs/blobs/`. Packages, layers, and blobs that remain unreachable across all three tiers are deleted in one sweep.
:::

A dependency is protected by the liveness of its dependents, not by a back-reference inside itself: when OCX installs a package with dependencies, it records each dependency as a forward-ref inside the dependent package's `refs/deps/` directory, pointing at the dependency's `content/`. Nothing is written into the dependency's own `refs/symlinks/`. The same `ocx clean` sweep that removes the last dependent therefore also collects the now-unreachable dependency.

## Layers {#layers}

A package on disk looks monolithic — one `content/` directory under one digest — but the bytes inside can come from more than one upstream archive. Each archive is a <Tooltip term="layer">An OCI image layer: a single tar archive (gzip or xz compressed) addressed in the registry by its own SHA-256 digest. The OCI image manifest lists all layers that compose a package; on pull, OCX extracts each layer once into the shared layer store and assembles them into the package's `content/` directory via hardlinks.</Tooltip>, stored once in `~/.ocx/layers/` and shared across every package that references it.

<Tree>
  <Node name="~/.ocx/layers/" icon="🗂️" open>
    <Node name="{registry}/" icon="📁" open-icon="📂" open>
      <Node name="sha256/ab/c123…/" icon="📁" open-icon="📂" open>
        <Description>one directory per unique layer blob</Description>
        <Node name="content/" icon="📂">
          <Description>extracted layer files — hardlink source for assembled packages</Description>
        </Node>
      </Node>
    </Node>
  </Node>
</Tree>

The layer store enables a different kind of dedup than the package store. The package store dedups whole builds — two tags pointing at the exact same binary share one directory. The layer store dedups *parts* of builds: a 200 MB shared base layer used by ten packages is downloaded, extracted, and stored exactly once. Each package's `content/` is then assembled by hardlinking files from one or more layer directories, so the package looks complete even though no bytes were copied.

::: info Similar to Docker image layers and pnpm's content store
[Docker image layers][docker-layers] are the same concept at the registry level: an image is a stack of layers, each addressed by digest, downloaded only when missing from the local cache. OCX applies this to binary packages instead of containers. [pnpm][pnpm] uses a comparable trick on the install side, storing every package version once in a content-addressed store and hardlinking them into individual `node_modules/` trees.
:::

### Multi-Layer Packages {#multi-layer}

When you call [`ocx package push`][cmd-package-push], every positional argument after the identifier is a layer — zero or more. Each layer is either a path to a local archive file or a digest reference to a layer that already exists in the target registry. A push with zero layers is valid too: it produces a config-only OCI artifact (useful for referrer-only / description-only manifests) and requires `--metadata` since there is no file layer to sniff a sibling metadata path from.

```shell
# Two-layer package: shared base + package-specific top
ocx package push -p linux/amd64 mytool:1.2.3 base.tar.gz tool.tar.gz

# Re-publish with the base layer reused by digest — no re-upload.
# The digest ref must spell out the original archive extension
# (`.tar.gz` / `.tgz` / `.tar.xz` / `.txz`) — OCI blob HEADs do not
# carry the media type, so OCX refuses to guess.
ocx package push -p linux/amd64 mytool:1.2.4 sha256:<hex>.tar.gz newtool.tar.gz
```

The order matters for the manifest descriptor list, but assembled content must not overlap — two layers cannot contain the same file path. Overlap is rejected at install time with a clear error.

::: info Digest verification on pull
Every layer blob downloaded by `ocx install` or `ocx package pull` is streamed through SHA-256 on the way to disk and compared against the digest declared in the manifest before extraction. A mismatch — the registry serving different bytes for the same digest ([CWE-345](https://cwe.mitre.org/data/definitions/345.html)) — deletes the tampered file and fails the command. Zero-layer pulls are valid: a config-only package (produced by `ocx package push` with no file layers and `--metadata`) installs into an empty `content/` directory, which is the expected shape for referrer-only or description-only artifacts.
:::

::: warning Bring your own archives
[`ocx package push`][cmd-package-push] does **not** bundle a directory for you. Each layer must be a pre-built archive (`.tar.gz` / `.tgz` or `.tar.xz` / `.txz`). This is intentional: archive creation is non-deterministic (timestamps, compression entropy, file ordering), so re-bundling the same content yields a different digest and defeats layer reuse. Use [`ocx package create`][cmd-package-create] if you need to bundle a directory — that command produces a stable archive once, which you can then push and reference by digest from any number of subsequent packages.

If a file in your current directory is literally named like a digest reference (e.g. `sha256:abc….tar.gz`), prefix it with `./` to force file interpretation — bare `sha256:…` tokens are always parsed as digest refs.
:::

::: tip Designing for reuse
Layer reuse is most valuable when many packages share a large common base — a runtime library, a vendored toolchain, or a fixed dataset. Push the base once, record its digest, and reference it from every dependent package's push command. The registry stores it once; OCX downloads it once; every package gets a fresh `content/` view assembled from the same shared layer.
:::

## Tags {#tags}

When you run `ocx install cmake:3.28`, how does OCX know which binary to fetch? It looks up the tag `3.28` in the local tag store and finds the corresponding <Tooltip term="digest">The content fingerprint stored in the OCI registry manifest. A tag like `3.28` is just a human-readable alias; the digest is the canonical, immutable identifier that pinpoints the exact binary build behind that tag at index-update time.</Tooltip>. The tag store is a local copy of that mapping — no network required.

<Tree>
  <Node name="~/.ocx/tags/" icon="🏷️" open>
    <Node name="{registry}/" icon="📁" open-icon="📂" open>
      <Node name="{repo}.json" icon="📄">
        <Description>{ "3.28": "sha256:abc…", "3.30": "sha256:def…" }</Description>
      </Node>
    </Node>
  </Node>
</Tree>

The tag store is a *snapshot*: it reflects the state of the remote registry at the last time you refreshed it. The full design — when the snapshot is updated, how `--remote` and `--offline` interact with it, how snapshots travel inside GitHub Actions or Bazel rules — lives in [Indices][in-depth-indices].

::: info Similar to APT's package lists
`apt-get update` downloads package metadata from configured sources and caches it in `/var/lib/apt/lists/`. All subsequent `apt-get install` calls resolve packages from that local snapshot — the network is only involved during an explicit refresh, not on every install. `ocx index update <package>` is the per-package equivalent: you control when the snapshot changes, and the rest of the time you work from the local cache.
:::

## Symlinks {#symlinks}

Package paths embed the digest: `~/.ocx/packages/ocx.sh/sha256/ab/c123…/content`. That path changes on every upgrade. You cannot put it in a shell profile, an IDE config, or a build file and expect it to still work next month.

`symlinks/` solves this with <Tooltip term="stable symlinks">A symbolic link (symlink) is a filesystem entry that points to another path. The link's own path never changes — only its target can be updated. Tools that reference the link path continue to work transparently after the target is re-pointed to a new version.</Tooltip> whose paths never change — only their targets are re-pointed when you install or select a new version.

<Tree>
  <Node name="~/.ocx/symlinks/" icon="🔀" open>
    <Node name="{registry}/" icon="📁" open-icon="📂" open>
      <Node name="{repo}/" icon="📁" open-icon="📂" open>
        <Node name="current" icon="➡️" open>
          <Description>active package root — set by ocx select</Description>
          <Node name="content/" icon="📂" />
          <Node name="entrypoints/" icon="🚀" />
          <Node name="metadata.json" icon="📋" />
        </Node>
        <Node name="candidates/" icon="📁" open-icon="📂" open>
          <Node name="3.28" icon="➡️">
            <Description>pinned package root — created by ocx install cmake:3.28</Description>
          </Node>
          <Node name="3.30" icon="➡️">
            <Description>pinned package root — created by ocx install cmake:3.30</Description>
          </Node>
        </Node>
      </Node>
    </Node>
  </Node>
</Tree>

Two symlink entries cover every use case. Both target the **package root** (`packages/{registry}/{algorithm}/{2hex}/{30hex}/`) rather than the `content/` subdirectory; consumers traverse into `…/content/` for files, `…/entrypoints/` for launcher scripts, or read `…/metadata.json` directly:

**`candidates/{tag}`** — pinned to a specific version. Created by [`ocx install`][cmd-install] and pointed at the exact digest that tag resolved to at install time. cmake 3.28 and 3.30 can coexist; both candidates remain until you explicitly uninstall one. Even if the registry later re-pushes the `3.28` tag with a different binary, your candidate still points to the build you originally installed.

**`current`** — a floating pointer to whichever candidate you last declared active. Set by [`ocx select`][cmd-select] (or `ocx install --select` in one step). It is never updated automatically — not when you install a newer version, not when you update the tag store. This is intentional: tools referencing `current` should only change behavior when *you* decide they should. When the selected package declares [`entrypoints`][metadata-entry-points], [`ocx shell profile load`][cmd-shell-profile] adds `{repo}/current/entrypoints` to `$PATH` so every declared launcher becomes a top-level command. See [Entry Points][in-depth-entry-points] for how launchers, PATH, and clean-env execution compose.

::: info Inspired by SDKMAN and Homebrew
[SDKMAN][sdkman] (the Java SDK manager) uses the same two-level pattern: `~/.sdkman/candidates/{tool}/{version}/` for pinned installs and a `current` symlink updated by `sdk default {version}`. [Homebrew][homebrew] does the same with its `Cellar/{formula}/{version}/` store and a stable `opt/{formula}` symlink pointing at the active version. Linux's `update-alternatives` is the system-level equivalent, managing tools like `java` and `python3` via a layer of stable symlinks in `/etc/alternatives/`.
:::

## See Also

- [Storage section in the user guide][user-storage] — how-to: install, switch versions, embed stable paths
- [Indices][in-depth-indices] — tag-store snapshots, locking, offline behavior
- [Entry Points][in-depth-entry-points] — generated launchers, synth-PATH, clean-env execution
- [Dependencies][in-depth-dependencies] — `refs/deps/` forward-refs and reachability across the GC walk
- [`ocx clean`][cmd-clean] — reference for the GC command

<!-- external -->
[nix]: https://nixos.org/
[docker-layers]: https://docs.docker.com/get-started/docker-concepts/building-images/understanding-image-layers/
[pnpm]: https://pnpm.io/motivation#saving-disk-space
[sdkman]: https://sdkman.io/
[homebrew]: https://brew.sh/

<!-- commands -->
[cmd-install]: ../reference/command-line.md#install
[cmd-select]: ../reference/command-line.md#select
[cmd-uninstall]: ../reference/command-line.md#uninstall
[cmd-clean]: ../reference/command-line.md#clean
[cmd-package-push]: ../reference/command-line.md#package-push
[cmd-package-create]: ../reference/command-line.md#package-create
[cmd-launcher-exec]: ../reference/command-line.md#exec
[cmd-exec]: ../reference/command-line.md#exec
[cmd-shell-profile]: ../reference/command-line.md#shell-profile

<!-- environment -->
[env-ocx-home]: ../reference/environment.md#ocx-home

<!-- reference -->
[metadata-ref]: ../reference/metadata.md
[metadata-entry-points]: ../reference/metadata.md#entry-points

<!-- internal -->
[user-storage]: ../user-guide.md#storage
[in-depth-indices]: ./indices.md
[in-depth-entry-points]: ./entry-points.md
[in-depth-dependencies]: ./dependencies.md
