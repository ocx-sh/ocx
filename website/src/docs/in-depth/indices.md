---
outline: deep
---
# Indices

OCI tags are mutable. The [OCI Distribution Specification][oci-dist-tag] defines tags as registry-side aliases that can be re-pointed at any moment — `cmake:3` today may resolve to a different digest after the next patch release. For a tool that installs binaries reproducibly, this is a problem: `ocx install cmake:3` run twice on different days could silently resolve different builds.

OCX solves this by keeping a local snapshot of tag-to-digest mappings. That snapshot only changes when explicitly refreshed. The same snapshot always resolves `cmake:3` to the same digest — regardless of what the registry serves today. This page explains *how* the three index modes (local, remote, active) interact, *what* the snapshot bundle pattern enables for Actions/Bazel/DevContainer authors, and *why* OCX never auto-updates the local index. The user-facing surface — when to refresh, offline mode, `--remote` — lives in the [Indices section of the user guide][user-indices].

## Remote Index {#remote}

The remote index is the live OCI registry. It answers metadata queries — which tags exist for a package, which digest a tag currently points to, which platforms a manifest declares — directly and authoritatively.

[`ocx index catalog`][cmd-index-catalog] browses available packages; [`ocx index list`][cmd-index-list] lists tags for a specific package. Both query the live registry when the global flag [`--remote`][arg-remote] is set.

The remote index is also the data source for [`ocx index update`][cmd-index-update]. Refreshing the local snapshot means querying the remote index and writing the results to disk.

## Local Index {#local}

The local index reads from [`~/.ocx/tags/`][in-depth-storage-tags] — a <Tooltip term="snapshot">A point-in-time copy of the remote registry's tag-to-digest mappings. No binaries — only the small JSON metadata files needed to resolve package identifiers.</Tooltip> of OCI tag-to-digest mappings. Resolving `cmake:3` is a file read; no network request is made.

**The local index is never updated automatically.** You decide when your snapshot changes. Until you explicitly refresh it, the same identifier always resolves to the same digest — on your laptop, on CI, and on every team member's machine. Rolling tags like `cmake:3` map to the digest current at last update, not whatever the registry serves today.

::: info Similar to APT's package lists
`apt-get update` downloads package metadata from configured sources and caches it in `/var/lib/apt/lists/`. All subsequent `apt-get install` calls resolve packages from that local snapshot — the network is only involved during an explicit refresh, not on every install. [`ocx index update <package>`][cmd-index-update] is the per-package equivalent: you control when the snapshot changes, and the rest of the time you work from the local cache.
:::

### Update modes {#update-modes}

[`ocx index update <package>`][cmd-index-update] syncs the local index for a specific package from the remote registry:

- **Bare identifier** (e.g., `cmake`) — downloads all tag-to-digest mappings for the repository.
- **Tagged identifier** (e.g., `cmake:3.28`) — fetches only that single tag's digest and manifest, merging it into the existing local tags file.

Tag-scoped mode is ideal for lockfile workflows where the index should contain only explicitly requested tags. Packages not listed are not touched.

### Fresh-machine fallback {#fresh-machine}

On a fresh machine, [`ocx index update`][cmd-index-update] does not need to run before the first [`ocx install cmake:3.28`][cmd-install]. When the local tag store has no entry for a requested tag, [`ocx install`][cmd-install] transparently resolves that single tag against the configured remote, persists it to the tag store, and proceeds with the install. Subsequent commands — including [`--offline`][arg-offline] — then work from the cached entry without touching the network.

Refreshing a cached tag or discovering every tag for a repository is still the job of [`ocx index update`][cmd-index-update]; the fallback only covers the specific tag being installed.

## Active Index {#active}

Every command that resolves a package identifier — [`ocx install`][cmd-install], [`ocx find`][cmd-find], [`ocx exec`][cmd-exec], [`ocx index list`][cmd-index-list] — uses one working index for that invocation. By default, this is the local index. Two flags change which index is used:

| Mode | Flag | Source | Network? |
|---|---|---|---|
| Default | *(none)* | Local snapshot | No (unless fetching a new binary) |
| Remote | [`--remote`][arg-remote] | OCI registry | Yes |
| Offline | [`--offline`][arg-offline] | Local snapshot | Never |

**`--remote`** forces tag and catalog lookups to query the registry directly for a single command. The persistent local tag store (`$OCX_HOME/tags/`) is **not** updated. Blob data fetched under `--remote` still writes through to `$OCX_HOME/blobs/`, so the command populates the blob cache while bypassing the tag snapshot. Use it for a one-off check — seeing currently available tags, or resolving the latest digest — without committing the tag resolution result to the local snapshot.

**`--offline`** prevents all network access for that command. If the local index does not have a requested package, the command fails immediately rather than attempting a registry query. Useful to verify that the current index and object store are self-sufficient before a build in a restricted or air-gapped environment.

[`--index`][arg-index] / [`OCX_INDEX`][env-ocx-index] do not change the active index *mode* — the local snapshot remains active. They only change *where* that snapshot is read from. See [Bundled Snapshots](#bundled).

The active index controls tag and manifest resolution only. The [package store][in-depth-storage-packages] is independent — installed binaries are accessible in all three modes regardless of which index is active.

## Bundled Snapshots {#bundled}

The local snapshot is small enough — JSON metadata only, no binaries — to ship *inside* a tool release. [Bazel Rules][bazel-rules], [GitHub Actions][github-actions-docs], and [DevContainer Features][devcontainer-features] can bundle a frozen snapshot at release time and set [`OCX_INDEX`][env-ocx-index] (or pass [`--index`][arg-index]) to point OCX at it. Consumers write `cmake:3` and the bundled snapshot resolves it deterministically, with the [package store][in-depth-storage-packages] and [install symlinks][in-depth-storage-symlinks] remaining in `OCX_HOME` as usual.

This produces a two-level lock: the tool version locks the bundled snapshot, which locks the resolved binary. A version bump to the action or rule — proposed automatically by [Dependabot][dependabot] or [Renovate][renovate] — advances the bundled index. Users get the updated binary with no config changes.

::: tip GitHub Action with bundled index
```yaml
- uses: ocx-actions/setup-cmake@v2.1.0   # pins action → pins index → pins binary
  with:
    version: "3.28"                       # human-readable tag, no platform conditions
```

`@v2.1.0` locks everything end-to-end. `@v2` follows minor releases — as the maintainer ships updated index snapshots, `cmake:3.28` may resolve to a newer build when the action version changes. No SHA256 lists, no `if: runner.os == 'Linux'` conditionals.
:::

The contrast with maintaining a [hand-curated URL matrix][toolchains-llvm] — one `filename → checksum` entry per `version × os × arch` — is clear: a version bump means editing one rule version, not a dictionary.

## See Also

- [Indices section in the user guide][user-indices] — how-to: refresh, work offline, use `--remote`
- [Storage][in-depth-storage] — `~/.ocx/tags/` layout, tag-store as a store
- [Versioning][in-depth-versioning] — tag mutability, locking by digest, `_` build suffix
- [Configuration][in-depth-configuration] — config-driven defaults that pair with bundled snapshots

<!-- external -->
[oci-dist-tag]: https://github.com/opencontainers/distribution-spec/blob/main/spec.md#pulling-manifests
[bazel-rules]: https://bazel.build/extending/rules
[github-actions-docs]: https://docs.github.com/en/actions/writing-workflows/choosing-what-your-workflow-does/using-pre-written-building-blocks-in-your-workflow
[devcontainer-features]: https://containers.dev/implementors/features/
[dependabot]: https://docs.github.com/en/code-security/dependabot/working-with-dependabot/keeping-your-actions-up-to-date-with-dependabot
[renovate]: https://docs.renovatebot.com/
[toolchains-llvm]: https://github.com/bazel-contrib/toolchains_llvm/blob/master/toolchain/internal/llvm_distributions.bzl

<!-- commands -->
[cmd-install]: ../reference/command-line.md#install
[cmd-find]: ../reference/command-line.md#find
[cmd-exec]: ../reference/command-line.md#exec
[cmd-index-update]: ../reference/command-line.md#index-update
[cmd-index-catalog]: ../reference/command-line.md#index-catalog
[cmd-index-list]: ../reference/command-line.md#index-list
[arg-remote]: ../reference/command-line.md#arg-remote
[arg-offline]: ../reference/command-line.md#arg-offline
[arg-index]: ../reference/command-line.md#arg-index

<!-- environment -->
[env-ocx-index]: ../reference/environment.md#ocx-index

<!-- internal -->
[user-indices]: ../user-guide.md#indices
[in-depth-storage]: ./storage.md
[in-depth-storage-packages]: ./storage.md#packages
[in-depth-storage-symlinks]: ./storage.md#symlinks
[in-depth-storage-tags]: ./storage.md#tags
[in-depth-versioning]: ./versioning.md
[in-depth-configuration]: ./configuration.md
