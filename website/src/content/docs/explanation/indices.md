---
title: "Indices"
description: "What an OCX index is, the one wire format every copy shares, which index a command uses, and how index data reaches a repository."
---
<!-- doc_type: explanation -->

This page explains what an OCX index is, which index a command reads and how a copy ships inside a tool. It also covers how an index is written. A tag that resolves differently on two machines breaks builds.

OCI tags are mutable. The [OCI Distribution Specification][oci-dist-tag] defines tags as registry-side aliases that can be re-pointed at any moment. `kitware/cmake:3` today may resolve to a different digest after the next patch release.

Choosing a version by tag must give the same digest every time. That holds for a devcontainer parameter, a Gradle property or a bare CLI argument. It must hold on your laptop, on CI and on a machine that has never talked to the registry.

OCX solves this with a local index. It is a directory that holds a local copy of registry resolution data. The copy changes only when you refresh it explicitly.

The [local index][local-index] page explains how the copy resolves without a network round trip. The [index.ocx.sh][public-index] page explains how OCX resolves packages from the public index. [Serve an index][serve-an-index] shows how to publish a copy. The user-facing surface lives in the [Indices section of the user guide][user-indices]: when to refresh, offline mode and `--remote`.

:::note[An index resolves versions, not `ocx.lock`]
An index answers one question. Which versions of a package exist, and for a chosen version, which platform-manifest digest and physical registry location does it point to?

An index never appears in `ocx.lock` resolution. A lock already records the exact platform-manifest digest it pinned (see [Locking][in-depth-versioning-locking]). Resolving a lock reads that digest straight off disk and fetches it, with no index consulted.

What an index buys is free *version choice*. Something that names a tag rather than a digest still needs to resolve deterministically and offline. A devcontainer feature parameter, a Gradle property and a bare `ocx package install kitware/cmake:3.28` are all examples. A [shipped index copy](#bundled) closes exactly that gap.
:::

## One format, many copies {#format}

There is exactly one index format. It has four parts:

- a `config.json` version marker,
- a catalog `c/index.json`,
- per-package root documents `p/<ns>/<pkg>.json`,
- per-package dispatch objects `p/<ns>/<pkg>/o/<algo>/<hex>.json`.

Every **published** copy of an index uses this identical layout. That covers the hosted [index.ocx.sh][index-ocx-sh] site and a corporate mirror of it. It also covers a directory shipped inside a [DevContainer Feature][devcontainer-features] and the machine-local cache under `$OCX_HOME`. A *derived* copy, described on the [local index][local-index] page, carries no `config.json` or `c/index.json` of its own.

Copying a published index with `rsync`, `wget --mirror` or `git add` produces a valid index. There is nothing to convert.

Indices differ along three axes, never in format:

- **Location.** An index is hosted (`index.ocx.sh` itself) or mirrored (a corporate static-file proxy in front of it). It can be shipped (a devcontainer feature, a CI artifact, a directory committed to a repository). It can also be local to a machine (`$OCX_HOME/index/<source>/`).
- **Provenance.** A *published* copy is a copy of a remote ocx-index. Its bytes are copied from it and verify against themselves (see [index.ocx.sh][public-index]). A *derived* copy is built by OCX itself from a plain OCI registry's tags, because an ordinary OCI registry publishes no index of its own.
- **Completeness.** A local copy is normally *partial*. [`ocx index update <pkg>`][cmd-index-update] grows it one package at a time. A full site mirror (`wget --mirror`) is the degenerate *complete* case, with the same grammar and more entries.

## Remote index {#remote}

The remote index is the live OCI registry. For a package identified under `ocx.sh`, it is the live [`index.ocx.sh`][index-ocx-sh] service. It answers metadata queries directly and authoritatively. Examples are which tags exist for a package, which digest a tag points to and which platforms a manifest declares.

[`ocx index catalog`][cmd-index-catalog] browses available packages. [`ocx index list`][cmd-index-list] lists the tags of one package. Both query the live registry when the global flag [`--remote`][arg-remote] is set.

The remote index is also the data source for [`ocx index update`][cmd-index-update]. Refreshing the local index means querying the remote for the resolution data of a tag. OCX then writes a dispatch object, a root document and a catalog entry to disk. It never writes the platform manifest itself (see [Dispatch objects only][local-dispatch]).

## Active index {#active}

Every command that resolves a package identifier uses one working index for that invocation. Examples are [`ocx package install`][cmd-package-install], [`ocx package which`][cmd-which], [`ocx package exec`][cmd-exec] and [`ocx index list`][cmd-index-list]. By default this is the local index. Two flags change which index is used:

| Mode | Flag | Source | Network? |
|---|---|---|---|
| Default | *(none)* | Local index | No (unless fetching a new binary) |
| Remote | [`--remote`][arg-remote] | OCI registry | Yes |
| Offline | [`--offline`][arg-offline] | Local index | Never |

**`--remote`** forces tag and catalog lookups to query the registry directly for a single command. The two shapes that can take it differ in what happens to the local index.

A **query** reads the registry and reports. The local index is **not** updated. Queries are [`ocx --remote index list`][cmd-index-list], [`ocx --remote index catalog`][cmd-index-catalog] and [`ocx --remote package description pull`][cmd-package-info].

A **resolve** still writes, despite the flag's name. Resolves are `ocx --remote package install`, `ocx --remote package exec` and similar. A resolve re-fetches the tag it resolved and rewrites the local copy for that one tag. This is exactly what an [`ocx index update`][cmd-index-update] scoped to the tag does (see [Two-hop fetch and caching][public-index-caching]). `--remote` skips only the *local-first* check, not the write.

Layer data fetched under either shape still writes through to the [package store][in-depth-storage-packages]. Use a query for a one-off check that leaves the index alone. Use a `--remote` resolve to force-refresh one tag without typing `ocx index update <pkg>:<tag>` first.

**`--offline`** prevents all network access for that command. If the local index does not have a requested package, the command fails immediately. It does not attempt a registry query. This verifies that the current index and package store are self-sufficient before a build in a restricted or air-gapped environment.

[`--index`][arg-index] and [`OCX_INDEX`][env-ocx-index] do not change the active index *mode*. The local index stays active. They change only *which collection* is read. See [Shipped copies](#bundled).

The active index controls tag and manifest resolution only. The [package store][in-depth-storage-packages] is independent. Installed binaries are accessible in all three modes, whichever index is active.

## Shipped copies {#bundled}

A local index subtree is small. It holds root documents and dispatch objects only, with no layer archives and no binaries. That is small enough to ship *inside* a tool release.

[Bazel Rules][bazel-rules], [GitHub Actions][github-actions-docs] and [DevContainer Features][devcontainer-features] can bundle a frozen copy at release time. They set [`OCX_INDEX`][env-ocx-index] (or pass [`--index`][arg-index]) to point OCX at it. Consumers write `kitware/cmake:3` and the bundled copy resolves it deterministically. That needs no network and no other machine-global state. The [package store][in-depth-storage-packages] and [install symlinks][in-depth-storage-symlinks] stay in `OCX_HOME` as usual.

`OCX_INDEX` swaps the *whole* collection for the shipped one, never a partial overlay of the two. Without it, OCX reads `$OCX_HOME/index/`. A project that commits its own `.ocx/index/` copy exports the variable for the project's shell, pointing at that directory.

`ocx direnv export` never sets the variable ambiently. An explicit `--index` or a deployment-set `OCX_INDEX` are the only channels that redirect the collection. `--index` wins over the variable.

Under [`--remote`][arg-remote] or `OCX_REMOTE`, tag-addressed and catalog-addressed lookups bypass the redirected collection. They query the registry directly, so the variable has no effect on them. Digest-addressed lookups consult the redirected collection first in every mode.

This produces a two-level pin on *version choice*, not on `ocx.lock`. A devcontainer feature or a GitHub Action has no lock file of its own, so this index-level pin is the only determinism it gets. The tool version pins the bundled index copy, and the copy pins the resolved binary. A version bump to the action or rule advances the bundled copy. [Dependabot][dependabot] or [Renovate][renovate] can propose that bump automatically.

Users get the updated binary with no config changes.

:::tip[GitHub Action with a bundled index]
```yaml
- uses: ocx-actions/setup-cmake@v2.1.0   # pins action → pins index → pins binary
  with:
    version: "3.28"                       # human-readable tag, no platform conditions
```

`@v2.1.0` pins everything end to end. `@v2` follows minor releases. When the maintainer ships updated index copies, `kitware/cmake:3.28` may resolve to a later build when the action version changes. There are no SHA256 lists and no `if: runner.os == 'Linux'` conditionals.
:::

Compare this with a [hand-curated URL matrix][toolchains-llvm]. That matrix needs one `filename → checksum` entry per `version × os × arch`. With a shipped index, a version bump means editing one rule version, not a dictionary.

:::caution[Content needs a network the first time, unless the layer cache is warm]
A shipped copy resolves the tag to a platform-manifest digest offline. The manifest bytes and layers are content. They are fetched on demand from the registry the first time a given digest is installed.

"First time" is scoped to the [layer store][in-depth-storage-layers] and the blob store, not to `packages/`. Copying `blobs/`, `layers/` and `index/` into a fresh `$OCX_HOME` is enough. Never copy `packages/`.

A package's `packages/…/content/` directory is a hardlink assembly of its layers, built locally at install time. If every layer a package needs is already on disk, `ocx package install` reassembles `content/` from the local layer cache with no network. This holds even though `packages/` started out empty. Only a genuinely cold layer needs egress.
:::

## Writing to an index {#writing}

Two commands write an index. [`ocx package claim`][cmd-package-claim] writes a package's first entry. [`ocx package announce`][cmd-package-announce] writes every tag update after it. Each arrives as a pull request (GitHub) or a merge request (GitLab).

Neither ever commits to the default branch of the index. The history of the index is always something a human or a bot decided to merge.

How that request gets *written* is `--transport`. The two values are not stylistic. The default, `api`, opens the request through the REST API of the forge. `git` clones the index repository into a temporary directory and builds the commit there. It then creates the request from a single authenticated push that carries [GitLab's merge-request push options][gitlab-push-options].

The `git` transport is GitLab-only and exists for one reason. A GitLab CI job token can push to a repository and read the API. It is read-only for merge requests, so the REST route is closed to the one credential a pipeline gets for free.

That leaves four credential postures, and the posture decides the transport:

| Posture | What is set | Transport |
|---|---|---|
| GitHub, access token | [`OCX_ANNOUNCE_TOKEN`][env-ocx-announce-token] holds a personal access token | `api` |
| GitLab, access token | [`OCX_ANNOUNCE_TOKEN`][env-ocx-announce-token] holds a personal, project or group token | `api` |
| GitLab CI job token | The runner's `GITLAB_CI` and `CI_JOB_TOKEN`, with nothing stored | `git` |
| GitLab, split pair | [`OCX_ANNOUNCE_GIT_TOKEN`][env-ocx-announce-git-token] carries the push, [`OCX_ANNOUNCE_TOKEN`][env-ocx-announce-token] the API reads | `git` |

The split pair is not redundancy. The two halves are asked for different things, and a real deployment often answers with different identities. A deploy token can write the repository but has no API surface to call. A job token can read the API but cannot open the merge request. Setting both lets each carry only what it can do.

Under `git`, ocx checks two things on the **index** project before it writes. The project must accept job-token pushes, and its job-token allowlist must admit the publishing project. ocx exits 82 when job-token pushes are off. It exits 77 when the allowlist omits the publisher. Only an administrator there can grant either. ocx also refuses to run on a `git` before 2.31.0, and checks this before the forge is constructed.

[Announcing a package][authoring-announcing] walks a real pipeline through each posture.

**Every write also removes what it stops referencing, in the same commit.** [`ocx package announce`][cmd-package-announce] deletes an index object the moment nothing in the rebuilt root references it. Examples are the old dispatch object of a moved tag, a replaced readme and a replaced logo. The request diff shows removals beside additions, so old bytes do not accumulate.

This sweep is always on and has no flag. It mirrors the [local copy's own sweep][local-update-modes] one layer down. Only an object that the *previous* committed root named is ever a candidate.

A concurrent announce may have an open request that references an object a merged run just deleted. This is accepted because it self-heals. Every curated tag's bytes are rewritten on the next announce of that package.

[`ocx package claim`][cmd-package-claim] runs the same sweep on every claim. That includes a re-claim that only adds an owner. A claim never writes tags, so the only object it orphans is the payload of the description. A claim that refreshes `desc` drops the readme or logo blob that the previous entry named.

## Keep tags {#keep-tags}

A registry tag can be deleted by mistake, even when someone's `ocx.lock` still pins a digest it once pointed at. Registry-side retention policies key on tags, not on which digests are in use somewhere. A stray delete can leave a manifest orphaned and eligible for garbage collection on the registry side.

[`ocx package push`'s `--keep-tag` and `--no-keep-tag`][cmd-package-push] close this gap on the registry side. [`ocx package copy`][cmd-package-copy] takes the same flags. They are on by default. For every platform manifest pushed **in that invocation**, OCX also pushes a digest-named `__ocx.keep.<algorithm>-<hex>` tag beside the version tag. While that keep tag exists, the registry sees the manifest as referenced, whatever happens to the human-readable tags around it. 

There is no retroactive tagging. A push does not reach back and keep-tag manifests published by earlier pushes.

A digest whose algorithm makes the tag longer than the OCI limit of 128 characters gets no keep tag at all. `sha512` is one, at 146 characters. The digest is never truncated to fit. Two digests sharing one truncated tag would silently drop the protection of a manifest. That is worse than leaving it unprotected in the open.

The local index never copies a leaf platform manifest, only its [dispatch object][local-dispatch]. An index-resolved install therefore always fetches the leaf from the registry on demand, sometimes long after the index entry was written.

A manifest stays reachable as long as *anything* still references it. A version tag does, and so does an image index that still lists it in its platform map. Most manifests never need a keep tag to stay alive.

Keep tags are a safeguard for the case that falls through the cracks. A tag moves or a patch is released, and nothing else points at the old digest any more. It would become eligible for garbage collection before an index-resolved install ever fetches it.

Keep tags are a pure registry-side safety net scoped to `ocx package push` and `ocx package copy`. [`ocx config push`][cmd-config-push] has no `--keep-tag` flag. [`index.ocx.sh`][public-index] ignores keep tags entirely, and they carry no wire semantics on the index side.

## The `update` family {#update-family}

Four OCX commands share the `update` verb. Each refreshes exactly one record, and confusing them means refreshing the wrong one.

| Command | Refreshes |
|---|---|
| [`ocx index update`][cmd-index-update] | The local index at [`--index` ▸ `OCX_INDEX` ▸ `$OCX_HOME/index/`][arg-index] |
| [`ocx self update`][cmd-self-update] | The managed ocx installation itself |
| [`ocx config update`][cmd-config-update] | The managed-config snapshot |
| [`ocx update`][cmd-update] | A project's `ocx.lock` |

A fifth command belongs to the family without carrying the verb. [`ocx index sync`][cmd-index-sync] refreshes the same record that `ocx index update` does. It works over the catalog of a whole registry rather than a named list of packages.

`ocx update` never writes a **tag pointer** into the local index. `ocx.lock` is its only canonical record. It can still persist a resolved dispatch object into `o/`. That object is content-addressed and pins nothing, so the write moves no tag.

Re-resolving the pinned packages of a project therefore does not change what `kitware/cmake:3` resolves to for any other command on the same machine. That stays the job of [`ocx index update`][cmd-index-update].

## See also {#see-also}

- [Local index][local-index]: the layout, crash safety and update modes of the machine-local collection.
- [index.ocx.sh][public-index]: how OCX resolves the public index and surfaces yanked and deprecated packages.
- [Serve an index][serve-an-index]: publish a local snapshot over HTTPS or from disk, and build an air-gap pipeline.
- [Indices section in the user guide][user-indices]: how to refresh, work offline and use `--remote`.
- [Storage][in-depth-storage]: the home of the local index relative to the other stores under `$OCX_HOME`.
- [Versioning][in-depth-versioning]: tag mutability, locking by digest and the `_` build suffix.
- [Configuration][in-depth-configuration]: the `[registries]` and `[mirrors]` config-driven defaults.

<!-- external -->
[oci-dist-tag]: https://github.com/opencontainers/distribution-spec/blob/main/spec.md#pulling-manifests
[bazel-rules]: https://bazel.build/extending/rules
[github-actions-docs]: https://docs.github.com/en/actions/writing-workflows/choosing-what-your-workflow-does/using-pre-written-building-blocks-in-your-workflow
[devcontainer-features]: https://containers.dev/implementors/features/
[dependabot]: https://docs.github.com/en/code-security/dependabot/working-with-dependabot/keeping-your-actions-up-to-date-with-dependabot
[renovate]: https://docs.renovatebot.com/
[toolchains-llvm]: https://github.com/bazel-contrib/toolchains_llvm/blob/master/toolchain/internal/llvm_distributions.bzl
[index-ocx-sh]: https://index.ocx.sh
[gitlab-push-options]: https://docs.gitlab.com/user/project/push_options/

<!-- commands -->
[cmd-package-install]: ../reference/command-line/package-consuming.md#package-install
[cmd-which]: ../reference/command-line/package-consuming.md#which
[cmd-exec]: ../reference/command-line/package-consuming.md#package-exec
[cmd-index-update]: ../reference/command-line/index-commands.md#index-update
[cmd-index-sync]: ../reference/command-line/index-commands.md#index-sync
[cmd-index-catalog]: ../reference/command-line/index-commands.md#index-catalog
[cmd-index-list]: ../reference/command-line/index-commands.md#index-list
[cmd-package-push]: ../reference/command-line/package-publishing.md#package-push
[cmd-package-copy]: ../reference/command-line/package-publishing.md#package-copy
[cmd-package-announce]: ../reference/command-line/package-publishing.md#package-announce
[cmd-package-claim]: ../reference/command-line/package-publishing.md#package-claim
[cmd-config-push]: ../reference/command-line/config.md#config-push
[cmd-package-info]: ../reference/command-line/package-publishing.md#package-description-pull
[cmd-self-update]: ../reference/command-line/self.md#self-update
[cmd-config-update]: ../reference/command-line/config.md#config-update
[cmd-update]: ../reference/command-line/commands.md#update
[arg-remote]: ../reference/command-line/index.md#arg-remote
[arg-offline]: ../reference/command-line/index.md#arg-offline
[arg-index]: ../reference/command-line/index.md#arg-index

<!-- environment -->
[env-ocx-index]: ../reference/environment.md#ocx-index
[env-ocx-announce-token]: ../reference/environment.md#ocx-announce-token
[env-ocx-announce-git-token]: ../reference/environment.md#ocx-announce-git-token

<!-- internal -->
[local-index]: ./local-index.md
[local-dispatch]: ./local-index.md#local-dispatch
[local-update-modes]: ./local-index.md#update-modes
[public-index]: ./public-index.md
[public-index-caching]: ./public-index.md#public-index-caching
[serve-an-index]: ../guides/serve-an-index.md
[user-indices]: ../guides/work-offline.mdx#offline
[authoring-announcing]: ../publishing/announce.md
[in-depth-storage]: ./storage.mdx
[in-depth-storage-packages]: ./storage.mdx#packages
[in-depth-storage-layers]: ./storage.mdx#layers
[in-depth-storage-symlinks]: ./storage.mdx#symlinks
[in-depth-versioning]: ./versioning.mdx
[in-depth-versioning-locking]: ./versioning.mdx#locking
[in-depth-configuration]: ./configuration.mdx
