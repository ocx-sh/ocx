---
title: "ocx index"
---
<!-- doc_type: reference -->

## `ocx index` {#index}

Inspect the package index and refresh the local copy.

None of these subcommands writes to a forge: proposing a namespace or a new version into the shared index is `ocx package claim` and `ocx package announce`.

```text
ocx index <COMMAND>
```

## `ocx index catalog` {#index-catalog}

List available repositories in the registry.

Uses the local index by default; with `--remote` it queries the registry directly without writing through to the local index. Repository names are always prefixed with their registry (for example `ocx.sh/kitware/cmake`), and the registry defaults to `OCX_DEFAULT_REGISTRY` (or `ocx.sh`) when none is given.

```text
ocx index catalog [OPTIONS] [REGISTRY]...
```

| Flag | Value | Description |
| --- | --- | --- |
| `--with-tags` |  | List tags for each repository in the catalog |

## `ocx index list` {#index-list}

List available versions of a package.

A pure query: under `--remote` it contacts the registry without writing the local index; run `ocx index update` to refresh the persistent snapshot. An identifier carrying a digest (`@sha256:...`) is rejected, because a digest narrows nothing in a tag listing; use `ocx package description pull <pkg>@<digest>` for a single artifact, or drop the digest.

A tag-only identifier (`<pkg>:<tag>`) filters the returned list. With `--platforms`, a digest-bearing identifier is accepted and resolves directly to that artifact's platform set, offline when already cached locally.

- Alias: `ls`

```text
ocx index list [OPTIONS] <PACKAGE>...
```

| Flag | Value | Description |
| --- | --- | --- |
| `--platforms` |  | Shows which platforms are available for each package. Uses the tag from the identifier, or `latest` if none specified |
| `--variants` |  | Lists unique variant names found in the tags |

## `ocx index regenerate` {#index-regenerate}

Rebuild a local index source's catalog from the root documents on disk.

`c/index.json` is derived data: every entry restates a digest the root document beside it already carries.

Every other writer only ever adds to it, which leaves one drift nothing can repair: an entry naming a package whose root document is gone. This re-derives the whole map from the tree, so such an entry is dropped.

It reports, per registry, the number of root documents found and every package it added, corrected or removed. A run that changed nothing says so in one line.

It consults no source and moves no pin, so `--frozen` and `--offline` both permit it. It writes exactly one file per registry, that registry's `c/index.json`, and never `config.json`, whose `name_segments` is an operator declaration no tree can be read for.

It also clears a stale `c/index.json.etag` beside it if one is present, which matters if your served tree tracks that file in version control.

Each `<REGISTRY>` must be a published index source (one configured with `[registries."<ns>"] index`). A derived, plain-OCI namespace has no catalog document by grammar (its catalog is the package enumeration itself) and is refused with exit 78.

The tree may come from another implementation or have no prior `c/index.json`; a tree whose catalog already matches is left byte-identical.

It enumerates neither symlinked root documents nor symlinked directories under `p/`. Because the catalog is replaced wholesale, running it against a symlink-deduplicated layout drops every package reached through a link.

A symlinked directory takes everything beneath it in one step.

```text
ocx index regenerate <REGISTRY>...
```

## `ocx index sync` {#index-sync}

Refresh every package one or more registries' catalogs list.

The whole-registry form of `ocx index update`: instead of naming packages, name the registries, and each one's own catalog names the packages. This is how a whole mirror is snapshotted in one command.

The set is read from the source live, never from the local copy, which is the set you already have. For a published source that is its own `c/index.json`, for a plain-OCI registry its repository listing.

Each package is refreshed as if named bare, so it adopts every tag the source lists and keeps any tag only the local copy holds.

It is a union of snapshots, never a replica. A merge never deletes a durable pin.

It drops only an ephemeral row the source no longer lists. A package that has disappeared upstream therefore keeps the tags this machine already recorded.

`ocx index regenerate` is the only command that removes a durable entry or a catalog entry. It removes only a catalog entry whose root document is already gone.

It cannot retract a package, for which the answer is a fresh index home.

Every `<REGISTRY>` is enumerated before any is refused, so one unreachable source does not cost the others their snapshot. The command still fails afterwards, reporting each failure.

A source that answers but serves no catalog document at all is a failure too, never an empty set. A catalog listing zero packages is the only clean way to snapshot nothing.

The refresh is bounded at the same in-flight ceiling `ocx index update` uses, across all registries together rather than per registry.

Recording a new tag-to-digest mapping is the discovery a freeze exists to refuse, so `--frozen` rejects this command with exit 81. `--offline` rejects it for contacting the source, including with `--dry-run`, which still enumerates.

A plain-OCI registry's repository listing needs the registry's `_catalog` endpoint. Docker Hub disables it, and GHCR allows it only with an authenticated, correctly scoped token.

A registry that refuses it fails that registry's snapshot. A catalog that cannot be read fails the command (exit 80 for an auth refusal, 69 for a published source serving no `c/index.json`).

A catalog listing zero packages exits 0 with a warning, since a token without catalog scope often answers with an empty list. A catalog carrying a yanked tag is refused with exit 65 unless the operator sets `OCX_ALLOW_YANKED`.

The first successful sync for a published source also writes its `config.json` when it is absent.

Details: <https://ocx.sh/docs/reference/command-line/index-commands/#index-sync>

```text
ocx index sync [OPTIONS] <REGISTRY>...
```

| Flag | Value | Description |
| --- | --- | --- |
| `--dry-run` |  | Print the packages this would refresh and refresh none; nothing is written, not even `config.json`, and no `[patches]` sync runs |

## `ocx index update` {#index-update}

Refresh the local index for one or more packages.

Fetches the requested packages' tags from the registry (or a configured index source) and records tag-to-digest mappings in the local index. Afterward `--offline` and `--frozen` resolution works without contacting the network.

Does not download the package itself - use `ocx package install` or `ocx package pull` for that.

Run it without `--frozen`. Recording a new tag-to-digest mapping is the discovery a freeze exists to refuse.

So `--frozen` rejects this command with exit 81 and moves no pin.

A tagged identifier (`cmake:3.28`) records only that tag; a bare identifier (`cmake`) records every tag.

Only the packages you name are touched: every other package keeps the version it already resolves to. The local copy is the set of snapshots you asked for, never a mirror of a floating remote.

`ocx index sync <REGISTRY>` is the same work over a registry's whole catalog, and is equally explicit.

A merge never deletes a durable pin. It does drop an ephemeral local row the source's root no longer lists.

If any package fails to refresh, the whole command fails; packages that refresh successfully keep their updated tags. A failed tag withholds only itself (and any tag sharing its digest); the exit code follows the first failure in request order.

Per tag it writes the tag's dispatch object and root document into the local index, never a leaf platform manifest. Then it upserts the package's catalog entry.

A committed index is therefore self-contained for version choice, and a locked package's platform-manifest digest resolves with no other store or network access. Manifests and layers are content and stay on the registry until installed, so `ocx --offline package install` after an update resolves the digest but still fails to install.

The first successful update for a published source also writes that source's `config.json` when it is absent. Naming no package is a usage error (exit 64).

Details: <https://ocx.sh/docs/reference/command-line/index-commands/#index-update>

```text
ocx index update <PACKAGE>...
```
