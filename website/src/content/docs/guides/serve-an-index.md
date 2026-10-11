---
title: "Serve an index"
description: "Serve a local index snapshot over HTTPS or straight from disk, build an air-gap pipeline from existing ocx commands, and repair catalog drift with regenerate."
---
<!-- doc_type: how-to -->
<!-- doc_tier: everyday -->

Use this guide to hand a local index snapshot to other machines. You can serve it over HTTPS from any static file server. You can also read it straight off disk with no server. Both routes need only existing `ocx` commands, with no bespoke server and no `ocx-mirror` code.

You need an index subtree under `$OCX_HOME/index/<source>/` that holds every package you want to serve. The [local index][local-index] page explains how the subtree is laid out and updated. The [indices overview][indices] explains the format.

## Serve a local snapshot {#servable}

A local index subtree is not only a client-side cache. It can be grown incrementally by [`ocx index update`][cmd-index-update]. It can be snapshotted in bulk by [`ocx index sync`][cmd-index-sync]. It can be repaired by [`ocx index regenerate`][cmd-index-regenerate].

Once its [`config.json`][local-layout] exists, the subtree is a complete, self-describing copy of [the one wire format][format]. Any OCX client can read it back. It can come over HTTPS from any static file server, or straight off disk. A curated corporate mirror is a pipeline of existing `ocx` commands.

### Check that the tree has a config.json {#servable-defect}

`config.json` is the version marker of the format. [`ocx index update`][cmd-index-update] and [`ocx index sync`][cmd-index-sync] write the JSON object `{"format_version": 1}` the first time either refreshes a published source. They write it only if nothing exists at that path. It is never rewritten afterward. A copy of a site that publishes its own `config.json` is left untouched (see [Wire layout][local-layout]).

An absent `config.json` means "version 1" at every reader. That covers the local reader, the HTTPS reader and the [`file://`](#servable-consuming) reader. A tree with no `config.json` at all resolves. This includes a hand-assembled tree and a tree grown by an `ocx` that never wrote the file.

A `config.json` that names an unsupported `format_version` is still a hard error ([exit 65][exit-codes]). It is the same on every reader. Only what *absence* means is relaxed. What a served, unsupported version means is not.

### Serve the tree and point a consumer at it {#servable-consuming}

Copy the source subtree wherever it needs to go. Then point a consuming `ocx` at the copy in one of two ways.

**Over HTTPS**, from any static file server. Examples are `python3 -m http.server`, nginx and an internal artifact bucket. Configure [`[registries."<ns>"] index = "https://…"`][config-registries-index] as usual.

**Straight off disk**, with no server at all, through a `file://` base:

```toml
[registries."corp"]
index = "file:///srv/ocx-index/corp"
```

This is the shape an air-gapped machine uses. It needs no HTTP server, no TLS and no network stack between the copy and the consumer.

The base has two requirements. `ocx` checks them at startup rather than at first fetch. The exception is [`--offline`][arg-offline]. Under `--offline`, no index source is built, so neither check runs and the command exits 0.

- **Empty authority.** `file:///srv/ocx-index/corp` (three slashes) is a local path. `file://host/…` and `file://localhost/…` are UNC or remote forms. They are refused, because a `file` base never dials a network.
- **Absolute path.** A relative tail is refused. So are the bare filesystem root (`file:///`) and a bare Windows drive designator (`file:///C:/`). They are refused rather than silently resolved against the directory `ocx` was launched from.

A `file` base is never keyed by host. It is therefore **not a valid `[mirrors]` override target**. The index role there redirects the traffic of a *host*, and a `file://` base has no host to key on. Point `index` itself at the path.

Every fetch through a `file://` base carries the guarantees of the HTTPS transport, adapted to a filesystem:

- **Read-only.** Nothing under the index base of the namespace is created, modified or deleted through the transport.
- **Size-bounded.** A document over the size cap of the HTTPS transport is refused, not truncated. The read is bounded by the bytes actually consumed, never by file metadata.
- **Symlink-contained.** A tree staged with symlinks is followed. Examples are an `rsync`, hardlink or symlink layout. The resolved target must stay under the configured root once both are canonicalized.
- **Regular files only.** A directory, device node, FIFO or any other file that is not plain is refused. This includes a mid-read swap that would otherwise slip one past the initial check.

Either way, the served tree is read through the identical [ocx-index protocol][public-index-pipeline] that `index.ocx.sh` itself uses. Object digests still verify. The `yanked` and `deprecated` status still surfaces (see [Status surfacing][public-index-status]). A consuming `ocx` cannot tell a hand-run static file server, a `file://` checkout and the hosted site apart.

To ship a copy inside a tool instead of serving it, set [`OCX_INDEX`][env-ocx-index] or pass [`--index`][arg-index] (see [Shipped copies][bundled]).

### Build the air-gap pipeline {#servable-air-gap}

1. On a connected machine, snapshot the packages you need. Name them directly, as in `ocx index update cmake ninja`. Or take them in bulk from the own catalog of a source, as in `ocx index sync ocx.sh`.
2. Copy `$OCX_HOME/index/<source>/` onto media, into a git repository, or onto a host that can reach the air-gapped network. Copy the whole subtree, `config.json` included.
3. On the air-gapped machine, point `ocx` at the copy. Use [`[registries."<ns>"] index = "https://mirror.corp/…"`][config-registries-index] for a served copy. Use `index = "file:///…"` for one staged directly on disk.
4. Run `ocx package install <pkg>:<tag>`. It resolves to the exact platform-manifest digest that the connected machine pinned. The copy is byte-identical to the source subtree, so resolution differs in nothing from resolving against the original.
5. Know what the resolve leaves out. It only names a digest. The manifest and layer bytes behind it are content, and the index never carries them (see [Dispatch objects only][local-dispatch]). Fetching them still needs a registry that the air-gapped host can reach.
6. Route the content fetch. The `repository` field of the root document still points at the upstream host. Route that traffic through a [`[mirrors]`][config-mirrors] **registry** role. Point it at a registry inside the air-gapped network, as for any other disconnected OCI pull. `ocx package install` then materializes the package from there.

Nothing in this pipeline is specific to `ocx-mirror`. It uses existing `ocx` commands, a copy step and, optionally, a static file server. That is the index half of an air-gapped install. Steps 5 and 6 are the registry half that every disconnected OCI setup already needs. The [corporate mirror][corporate-mirror] guide covers the registry half.

### Repair drift with regenerate {#servable-regenerate}

`c/index.json` is derived data. Every entry restates a digest that the root document beside it already carries. Every writer except one only ever *adds* to it (see [Crash-safe updates][local-crash-safety]). That leaves one drift that nothing else repairs. An entry may name a package whose root document is gone, deleted by hand or pruned by an external tool.

[`ocx index regenerate <REGISTRY>`][cmd-index-regenerate] re-derives the whole catalog from the `p/` tree on disk. It replaces the catalog wholesale. It is the one operation that clears such an entry.

It never contacts a source and moves no pin. [`--frozen`][arg-frozen] and [`--offline`][arg-offline] therefore both permit it. They refuse [`ocx index update`][cmd-index-update] and [`ocx index sync`][cmd-index-sync]. `regenerate` is the only command whose *purpose* is to write while they are set, and it rewrites the catalog deliberately.

[`catalog`][cmd-index-catalog] and [`list`][cmd-index-list] are permitted because they read the local copy. `list` can still trigger the read-path self-heal described in [Crash-safe updates][local-crash-safety] on an already-drifted tree. Neither flag gates that write.

`regenerate` never writes `config.json`. The `name_segments` field is an operator declaration that no tree can be read for. Guessing one while repairing a foreign tree would be wrong.

:::caution[`regenerate` does not follow symlinks under `p/`]
The derivation walks the filesystem, and the catalog it writes replaces the previous one **wholesale**. A symlink under `p/` is therefore not a missing entry. It is silent removal from `c/index.json`.

A symlinked root **document** is skipped by the walk. A symlinked **directory** is never queued at all. That takes every root beneath it with it in one step. A whole namespace or package tree can vanish from the catalog in a single `regenerate` run.

The packages still resolve by tag, because tag resolution reads root documents directly and never through the catalog. They disappear from [`ocx index catalog`][cmd-index-catalog], from [`ocx index sync`][cmd-index-sync] enumeration elsewhere and from anything else that reads `c/index.json`.

A served tree may deduplicate a shared package across locations with symlinks. Then `regenerate` needs the real files underneath, not links to them. Hardlinks are unaffected, because a hard-linked file *is* a regular file to the walk.

The removal is not always permanent. Take a tree that this machine also *resolves*, not merely serves. The catalog self-heal of a resolve re-adds a dropped entry the next time the package is resolved. The root read behind it follows symlinks directly, and the catalog is only a cache of the roots on disk. The removal sticks in exactly the case this warning is written for: a tree that is served and never locally resolved.
:::

## See also {#see-also}

- [Indices][indices]: the shared wire format, the active index and shipped copies.
- [Local index][local-index]: how the subtree you serve is laid out and updated.
- [index.ocx.sh][public-index]: the protocol a served tree is read through.
- [Corporate mirror][corporate-mirror]: route registry and index traffic through a mirror.

<!-- commands -->
[cmd-index-update]: ../reference/command-line/index-commands.md#index-update
[cmd-index-sync]: ../reference/command-line/index-commands.md#index-sync
[cmd-index-regenerate]: ../reference/command-line/index-commands.md#index-regenerate
[cmd-index-catalog]: ../reference/command-line/index-commands.md#index-catalog
[cmd-index-list]: ../reference/command-line/index-commands.md#index-list
[arg-offline]: ../reference/command-line/index.md#arg-offline
[arg-frozen]: ../reference/command-line/index.md#arg-frozen
[arg-index]: ../reference/command-line/index.md#arg-index
[exit-codes]: ../reference/exit-codes.md

<!-- environment -->
[env-ocx-index]: ../reference/environment.md#ocx-index

<!-- reference -->
[config-mirrors]: ../reference/configuration.md#keys-mirrors
[config-registries-index]: ../reference/configuration.md#keys-registries-index

<!-- internal -->
[indices]: ../explanation/indices.md
[format]: ../explanation/indices.md#format
[bundled]: ../explanation/indices.md#bundled
[local-index]: ../explanation/local-index.md
[local-layout]: ../explanation/local-index.md#local-layout
[local-dispatch]: ../explanation/local-index.md#local-dispatch
[local-crash-safety]: ../explanation/local-index.md#local-crash-safety
[public-index]: ../explanation/public-index.md
[public-index-pipeline]: ../explanation/public-index.md#public-index-pipeline
[public-index-status]: ../explanation/public-index.md#public-index-status
[corporate-mirror]: corporate-mirror.mdx
