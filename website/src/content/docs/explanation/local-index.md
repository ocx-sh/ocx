---
title: "Local index"
description: "How the machine-local index collection under OCX_HOME is found, laid out, kept crash-safe and updated, and why it never refreshes by itself."
---
<!-- doc_type: explanation -->

This page explains how the local index is found, laid out on disk and updated. It matters because the local index decides which digest a tag resolves to, with no network.

The [indices overview][indices] describes the one wire format that every index copy shares. The [Serve an index][serve-an-index] guide shows how to publish a local copy to other machines.

## The local collection {#local}

The local index reads from a self-contained collection under [`$OCX_HOME`][env-ocx-home]. The collection holds one subtree per source, and each subtree resolves with no network. Resolving `kitware/cmake:3` reads the `ocx.sh` subtree, or the subtree of whichever registry applies, and verifies a digest locally. Nothing is fetched over the network unless the requested tag is missing.

The home is resolved in order:

| Precedence | Source | Note |
|---|---|---|
| 1 | [`--index PATH`][arg-index] | Highest precedence. Points at a specific index collection. |
| 2 | [`OCX_INDEX`][env-ocx-index] | Set by a deployment that ships its own copy, such as a devcontainer feature or a CI step. Never ambient. [`ocx direnv export`][cmd-direnv-export] does not touch it. |
| 3 | `$OCX_HOME/index/` | The default, machine-local collection. |

The home is a **collection**, not a single index. `$OCX_HOME/index/ocx.sh/` and `$OCX_HOME/index/ghcr.io/` sit side by side, one subtree per source. Pointing `--index` or `OCX_INDEX` elsewhere swaps the *whole* collection for a shipped one. It is never a partial overlay of the two.

A shipped copy is not required to live at any particular path. A project conventionally commits one at `.ocx/index/`, but that convention is not enforced.

OCX never writes a lock file inside the collection itself, wherever it lives. Cross-process locks for an index update are homed under the machine-global `$OCX_HOME/locks`. They are keyed by the identity of the guarded directory. A redirected or git-committed `.ocx/index/` copy therefore changes only when `ocx` writes new data into it. It never picks up a stray lock sidecar that `git status` would flag.

**The local index is never updated automatically.** You decide when the subtree of a source changes. Until you refresh it explicitly, the same identifier always resolves to the same digest. That holds on your laptop, on CI and on every team member's machine. Rolling tags like `kitware/cmake:3` map to the digest that was current at the last update, not whatever the registry serves today.

That holds even for a tag you have never asked for before. Resolving a brand-new identifier *grows* the copy with a new root and dispatch object. Re-resolving one that is already on disk never silently *refreshes* it under the default mode.

An explicit [`ocx index update`][cmd-index-update] is the deliberate way to move a tag. A [`--remote`][arg-remote] resolve of that tag is the other. Unlike a `--remote` *query*, it re-fetches and rewrites the local copy for the tag it touches (see [Active index][active]).

"Explicit" means *named*. An update moves the packages you list and nothing else. It never moves a package as a side effect of moving another. There is no *implicit* whole-index sync that moves them all at once. [`ocx index sync`][cmd-index-sync] is the explicit one (see [Update modes](#update-modes)).

The same holds for the `repository` field. The pointer that decides which physical registry a package is fetched from is part of what a copy pins. It is not refreshed in passing.

Resolution stays silent about all of this. Once something is committed locally, resolving it makes no network request and says nothing about the remote, because there is nothing it could act on. Updates surface where you asked for them, in [`ocx index catalog --remote`][cmd-index-catalog]. They never appear as a warning from a command that was doing something else.

:::note[Similar to APT's package lists]
[`apt-get update`][apt-repo-format] downloads a `Packages` index from each configured mirror. The index lists the filename and checksum of every `.deb`. `apt-get install` then reads that local index and verifies the downloaded file against the recorded checksum.

The network is involved only during an explicit refresh and the file fetch itself. It is not involved in every dependency resolution. [`ocx index update <package>`][cmd-index-update] is the explicit-refresh equivalent of `apt-get update`.
:::

### Wire layout {#local-layout}

The subtree of every source under the home uses the [index.ocx.sh][index-ocx-sh] wire grammar, byte for byte:

```
$OCX_HOME/index/ocx.sh/
├── config.json                published sources only, {"format_version": 1, "name_segments": 2}
├── c/index.json               published sources only, the package catalog
└── p/<ns>/
    ├── <pkg>.json              root document: repository pointer, tags, publisher status
    └── <pkg>/o/sha256/
        └── <hex>.json          dispatch object, the filename is the SHA-256 of the object itself
```

A **published** source reaches this layout in two ways. These are `index.ocx.sh` and a mirror of it. "Verbatim" means something different for each way.

A full copy reproduces the site byte for byte. Examples are `wget --mirror`, `rsync` and a shipped tarball. Every root document, every dispatch object and `config.json` itself are exactly the bytes the site served.

That is what makes "copy a mirror" work. `wget --mirror https://index.ocx.sh/` into `$OCX_HOME/index/ocx.sh/` produces a valid, *complete* copy. It verifies against nothing but itself, through object filenames and the catalog, with no separate trust anchor.

[`ocx index update`][cmd-index-update] instead grows the tree package by package. Only its dispatch objects are verbatim. The filename of each object is its own recomputed SHA-256. The object is decoded and digest-reverified but never re-encoded.

Its root documents are assembled **locally**. The merge folds the tags an invocation named into whatever this machine already committed. For a bare identifier it folds every tag the site lists. It then re-serializes the result.

A tag-scoped `ocx index update cmake:3.28` therefore writes a root that carries that one tag. It does not write the whole tag list of the site. The result is a valid *partial* copy in the same grammar, not a byte-for-byte one.

This is also what makes the never-deletes rule below possible. A tag the site has dropped survives locally, because the local root was never a replacement for the root of the site. It was only ever a merge into it.

Every dispatch object is verified on read against its own `o/` filename. A byte-tampered object fails to load rather than resolving silently ([CWE-345][cwe-345]). A root document is cross-checked against its `c/index.json` entry too. That entry is a cached derivation rather than a trust anchor. A disagreement is repaired from the root on disk, not refused (see [Crash-safe updates](#local-crash-safety)).

`config.json` follows the same split. A full copy of a site that publishes one carries it verbatim, and `index.ocx.sh` does publish one. An `ocx index update`-grown tree gets one from OCX itself.

On the **first** successful update for a published source, OCX writes `{"format_version": 1}`, but only if nothing exists at that path. A file written locally is never rewritten afterward. A wrong or stale `config.json` is repaired by deleting it and running `ocx index update` again. No command edits it in place.

See [Serve an index][serve-an-index-servable] for why it matters whether this file exists.

A **derived** source is any plain OCI registry, such as `ghcr.io`, `docker.io` or a private one. It has no site to copy, so OCX writes the root document itself, in the same grammar:

```json
// $OCX_HOME/index/ghcr.io/p/ocx-contrib/cmake.json
{
  "repository": "oci://ghcr.io/ocx-contrib/cmake",
  "tags": {
    "3.28": { "content": "sha256:<dispatch-digest>", "observed": "2026-07-18T09:00:00Z" }
  }
}
```

A derived source carries no `config.json` or `c/index.json` of its own. Its catalog is the directory listing under `p/`. `observed` is the timestamp at which the pointer was last confirmed against its source. It records when the snapshot was taken and is not a freshness gate. The local index is [never auto-refreshed](#local).

### Dispatch objects only {#local-dispatch}

`o/` holds **dispatch objects only**. A dispatch object is the [OCI image index][oci-image-index] that a tag resolved to, verbatim, for either provenance kind. A leaf platform manifest names the layers of a binary. It is never copied into the local index.

The copy pins what a re-push can change: the binding from tag to digest and the map from platform to digest. Everything below a digest is left to the [package store][in-depth-storage-packages]. The store fetches it on demand and is content-addressed anyway.

The `content` digest of every tag in the root document names an image index present in `o/`. OCX decodes that index and runs [platform selection][reference-platforms-compatibility] over its per-platform digest list. It then fetches the resulting leaf digest from the physical registry, checking the [package store][in-depth-storage-packages] first.

The leaf manifest of an installed package was cached there at install time. Re-resolving it therefore needs no network at all.

A tag that resolves to a bare platform manifest instead of an index is refused when the source announces it. It is never recorded. `ocx package push` always publishes an index, so this can only happen for a repository that ocx did not publish.

A digest-pinned reference (`pkg@sha256:…`) is content addressing, not dispatch. It is fetched directly by digest and never touches `o/`. A multi-platform package holds exactly one dispatch object under `o/` per tag. That is roughly a sixth of the size of copying the whole manifest chain, for a typical multi-platform package.

:::note[An incomplete copy self-heals]
A dispatch object may be missing from a partially synced copy. This happens when a package is not fully cached yet. By construction, the missing object is an OCI image index. OCX fetches it by digest, verifies it and writes it into `o/` before dispatch continues. It does not fail.

A partial copy heals a gap the moment it is asked to resolve it, with no separate repair step. This is unrelated to digest-pinned references, which never touch `o/` at all.
:::

### Crash-safe updates {#local-crash-safety}

[`ocx index update <pkg>`][cmd-index-update] writes three things per tag, in a fixed order. Each is its own atomic step:

- the dispatch object into `o/`, which is content-addressed, so an orphan left by an aborted write is harmless because nothing points at it yet,
- the root document, written as a tempfile plus a rename,
- the `c/index.json` catalog entry of the package, written as a tempfile plus a rename.

An interruption between any two steps always recovers on the next read or update. A crash or a kill signal never leaves the copy looking corrupt.

The catalog entry is the SHA-256 of the bytes of the root document, and nothing more. It is a cached derivation, not an independently trusted fact. A read that finds a stored root and its catalog entry in disagreement has found an **inconsistency**, not evidence of tampering. OCX recomputes the entry from the root on disk and rewrites the catalog to match. It logs this at info level.

A hard error is reserved for corruption that recomputation cannot fix. Examples are an unparseable root document, a dispatch object whose bytes disagree with its own `o/` filename and a failed `repository` cross-check.

A **later** catalog sync may find that the *remote* root digest has moved past the local one. That is a different case. It is staleness, reported as an update being available, never an error. Re-running [`ocx index update <pkg>`][cmd-index-update] re-snapshots it.

### Update modes {#update-modes}

[`ocx index update <package>`][cmd-index-update] syncs the local index for one package from its remote source. The identifier decides the scope:

- **Tagged identifier**, such as `kitware/cmake:3.28`. The update adopts that one tag. Every sibling pin and the `repository` pointer stay exactly as committed. A tagged update is a statement about one version.
- **Bare identifier**, such as `cmake`. The update adopts every tag the source lists, plus the package-level fields. Naming the package with no tag is the sanctioned point to take a routing migration. You asked about the package, so the pointer of the package moves.

**An update never deletes a durable pin.** A tag that the source has stopped listing stays in the local copy, with the digest it was pinned to. This holds for both source kinds.

The copy is not a mirror of the current tag list of the remote. It is the record of what this machine snapshotted. A publisher retiring a version therefore cannot silently break a machine that is still pinned to it.

Merge is the only write verb. Local entries outside the scope of the update are never touched. Durable entries that the remote dropped are never removed.

**The one exception is an ephemeral row.** A row marked `"ephemeral": true` is announced with [`ocx package announce --ephemeral`][cmd-package-announce]. It is a snapshot meant to disappear. When the served root no longer lists it, a bare update drops it. A tagged update drops it only when it names that tag.

A durable pin that the root also lacks survives. `ocx --remote` resolving a tag that the root lacks also drops a local ephemeral row of that tag before it exits 79.

**An update always cleans up after itself.** This runs after every [`ocx index update`][cmd-index-update] and [`ocx index sync`][cmd-index-sync], for published and plain-registry sources alike. The local copy removes every dispatch object under `o/` that no surviving pin of the root of that package references any more. The object left behind by the *old* digest of a moved tag is one example. An object that a pin still names is never removed.

This is a referenced-set diff computed from the on-disk root of the package, not a directory walk. An object orphaned before this existed is not collected retroactively. A sibling package refresh's just-written object is never mistaken for an orphan, because its own root references it by construction.

The sweep runs silently, at `debug!` level only, with no stdout or stderr line. Description blobs are not modelled in the local copy at all and are left untouched either way. [`ocx index regenerate`][cmd-index-regenerate] is unaffected. It still removes no root document and no dispatch object.

**Naming is the only mode, and there is no *implicit* whole-index sync. That is deliberate.** A remote index floats by definition. Packages appear, platforms get added to existing versions and tags move.

A local copy is not a mirror of it. It is the set of snapshots you asked for.

Naming a package directly is one way to ask. [`ocx index sync <REGISTRY>`][cmd-index-sync] is the other. It reads the own catalog of that source **at that instant** to choose the set. It then does exactly the same per-package work as if each package were named bare.

Nothing else moves a pin: not a resolve, not a listing and not an update of a different package. There is no spelling of "sync everything, kept in sync". `index sync` is a single explicit read, not a standing subscription. Repeated runs never remove a durable pin.

"Sync everything, automatically" has no well-defined meaning against a partially materialized copy. It would clone a floating remote, so the copy stops being a lock. Or it would re-snapshot whatever subset happens to be present, an arbitrary set nobody chose.

`index sync` avoids both. It is a one-shot act, named by the operator, against the catalog of one source. It is not a background sync. See [Serve an index][serve-an-index-servable] for the case it exists to serve.

Naming is also about *who*. The index pins only packages that the user named directly. A [patch companion][user-patches-pins] is a package that a descriptor names on behalf of the operator. It pins in its own patch-tier state under `state/patch-companions/` and never as a local-index entry.

Tag-scoped mode is ideal for lockfile workflows where the local index should hold only explicitly requested tags. Packages not listed are not touched. This covers their tag pins and their `repository` pointer. Nothing is fetched about them either. An update requests the roots and dispatch objects of the named packages, and stops.

To ask what the source has today, ask the source. [`ocx index catalog --remote`][cmd-index-catalog] lists what it publishes. [`ocx index list --remote`][cmd-index-list] lists the tags of one package. Without `--remote`, both answer from the local copy, which is the index you maintain.

"Am I behind?" is a question about the remote, so it is asked out loud. It is not tracked in local shadow state.

### Fresh-machine fallback {#fresh-machine}

On a fresh machine, [`ocx index update`][cmd-index-update] does not need to run before the first [`ocx package install kitware/cmake:3.28`][cmd-package-install]. The local index may have no entry for the requested tag. [`ocx package install`][cmd-package-install] then resolves that single tag against the configured remote. It writes the dispatch object, the root and the catalog entry, and proceeds with the install.

Later commands work from the cached entry without touching the network. That includes [`--offline`][arg-offline].

Refreshing an already-cached tag, or discovering every tag of a repository, is still the job of [`ocx index update`][cmd-index-update]. The fallback covers only the specific tag being installed.

## See also {#see-also}

- [Indices][indices]: the shared wire format, the active index and shipped copies.
- [index.ocx.sh][public-index]: how OCX resolves packages through the public index.
- [Serve an index][serve-an-index]: publish a local copy and repair catalog drift.
- [Storage][in-depth-storage]: the home of the local index relative to the other stores under `$OCX_HOME`.

<!-- external -->
[apt-repo-format]: https://wiki.debian.org/DebianRepository/Format
[oci-image-index]: https://github.com/opencontainers/image-spec/blob/main/image-index.md
[index-ocx-sh]: https://index.ocx.sh

<!-- security -->
[cwe-345]: https://cwe.mitre.org/data/definitions/345.html

<!-- commands -->
[cmd-package-install]: ../reference/command-line/package-consuming.md#package-install
[cmd-index-update]: ../reference/command-line/index-commands.md#index-update
[cmd-index-sync]: ../reference/command-line/index-commands.md#index-sync
[cmd-index-regenerate]: ../reference/command-line/index-commands.md#index-regenerate
[cmd-index-catalog]: ../reference/command-line/index-commands.md#index-catalog
[cmd-index-list]: ../reference/command-line/index-commands.md#index-list
[cmd-package-announce]: ../reference/command-line/package-publishing.md#package-announce
[cmd-direnv-export]: ../reference/command-line/direnv.md#direnv-export
[arg-remote]: ../reference/command-line/index.md#arg-remote
[arg-offline]: ../reference/command-line/index.md#arg-offline
[arg-index]: ../reference/command-line/index.md#arg-index

<!-- environment -->
[env-ocx-home]: ../reference/environment.md#ocx-home
[env-ocx-index]: ../reference/environment.md#ocx-index

<!-- reference -->
[reference-platforms-compatibility]: ../reference/platforms.md#compatibility

<!-- internal -->
[indices]: ./indices.md
[active]: ./indices.md#active
[public-index]: ./public-index.md
[serve-an-index]: ../guides/serve-an-index.md
[serve-an-index-servable]: ../guides/serve-an-index.md#servable
[user-patches-pins]: patches.mdx#patches-pins
[in-depth-storage]: ./storage.mdx
[in-depth-storage-packages]: ./storage.mdx#packages
