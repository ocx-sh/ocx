---
outline: deep
---
# Indices

OCI tags are mutable. The [OCI Distribution Specification][oci-dist-tag] defines tags as registry-side aliases that can be re-pointed at any moment — `kitware/cmake:3` today may resolve to a different digest after the next patch release. Choosing a version by tag — a devcontainer parameter, a Gradle property, a bare CLI argument — needs to resolve to the same digest every time you make that same choice, on your laptop, on CI, and on a machine that has never talked to the registry before.

OCX solves this with a local index: a directory holding a local copy of registry resolution data that only changes when explicitly refreshed. This page explains what an index actually is — one wire format, copied or generated in different places — how the local copy resolves without a network round trip, and how OCX resolves packages published through the [`index.ocx.sh`][index-ocx-sh] public index. The user-facing surface — when to refresh, offline mode, `--remote` — lives in the [Indices section of the user guide][user-indices].

:::info An index resolves versions, not `ocx.lock`
An index answers one question: which versions of a package exist, and for a chosen version, which platform-manifest digest and physical registry location it points to. It never appears in `ocx.lock` resolution — a lock already records the exact platform-manifest digest it pinned (see [Locking][in-depth-versioning-locking]), so resolving a lock reads that digest straight off disk and fetches it, without consulting any index. What an index buys is free *version choice*: something that names a tag rather than a digest — a devcontainer feature parameter, a Gradle property, a bare `ocx package install kitware/cmake:3.28` — still needs to resolve deterministically and offline, which is exactly the gap a [shipped index copy](#bundled) closes.
:::

## One format, many copies {#format}

There is exactly one index format: a `config.json` version marker, a catalog `c/index.json`, per-package root documents `p/<ns>/<pkg>.json`, and per-package dispatch objects `p/<ns>/<pkg>/o/<algo>/<hex>.json`. Every **published** copy of an index anywhere — the hosted [index.ocx.sh][index-ocx-sh] site, a corporate mirror of it, a directory shipped inside a [DevContainer Feature][devcontainer-features], the machine-local cache under `$OCX_HOME` — is that identical layout (a *derived* copy, covered below, carries no `config.json` or `c/index.json` of its own). Copying it with `rsync`, `wget --mirror`, or `git add` produces a valid index; there is nothing to convert.

Indices differ along three axes, never in format:

- **Location** — hosted (`index.ocx.sh` itself), mirrored (a corporate static-file proxy in front of it), shipped (a devcontainer feature, a CI artifact, a directory committed to a repository), or local to a machine (`$OCX_HOME/index/<source>/`).
- **Provenance** — *published*: a copy of a remote ocx-index, bytes copied from it and verifiable against itself (see [index.ocx.sh](#public-index)). Or *derived*: OCX builds it itself from a plain OCI registry's tags, because an ordinary OCI registry publishes no index of its own.
- **Completeness** — a local copy is normally *partial*: [`ocx index update <pkg>`][cmd-index-update] grows it one package at a time. A full site mirror (`wget --mirror`) is just the degenerate *complete* case — same grammar, more entries.

## Remote Index {#remote}

The remote index is the live OCI registry (or, for a package identified under `ocx.sh`, the live [`index.ocx.sh`][index-ocx-sh] service). It answers metadata queries — which tags exist for a package, which digest a tag currently points to, which platforms a manifest declares — directly and authoritatively.

[`ocx index catalog`][cmd-index-catalog] browses available packages; [`ocx index list`][cmd-index-list] lists tags for a specific package. Both query the live registry when the global flag [`--remote`][arg-remote] is set.

The remote index is also the data source for [`ocx index update`][cmd-index-update]. Refreshing the local index means querying the remote for a tag's resolution data and writing a dispatch object, a root document, and a catalog entry to disk — never the platform manifest itself (see [Dispatch objects only](#local-dispatch)).

## Local Index {#local}

The local index reads from a self-contained collection under [`$OCX_HOME`][env-ocx-home] — one subtree per source, each independently resolvable with no network. Resolving `kitware/cmake:3` reads the `ocx.sh` (or whichever registry's) subtree and verifies a digest locally; nothing is fetched over the network unless the requested tag is missing.

The home is resolved in order:

| Precedence | Source | Note |
|---|---|---|
| 1 | [`--index PATH`][arg-index] | Highest precedence — points at a specific index collection |
| 2 | [`OCX_INDEX`][env-ocx-index] | Set by a deployment that ships its own copy — a devcontainer feature, a CI step. Never ambient; [`ocx direnv export`][cmd-direnv-export] does not touch it. |
| 3 | `$OCX_HOME/index/` | Default — the machine-local collection |

The home is a **collection**, not a single index: `$OCX_HOME/index/ocx.sh/`, `$OCX_HOME/index/ghcr.io/`, … one subtree per source, side by side. Pointing `--index` / `OCX_INDEX` elsewhere swaps the *whole* collection for a shipped one — never a partial overlay of the two. A shipped copy is not required to live at any particular path; a project conventionally commits one at `.ocx/index/`, but that convention is not enforced.

OCX never writes a lock file inside the collection itself, wherever it lives. Cross-process locks for an index update are always homed under the machine-global `$OCX_HOME/locks`, keyed by the guarded directory's own identity — so a redirected or git-committed `.ocx/index/` copy only ever changes when `ocx` actually writes new data into it, never picks up a stray lock sidecar `git status` would flag.

**The local index is never updated automatically.** You decide when a source's subtree changes. Until you explicitly refresh it, the same identifier always resolves to the same digest — on your laptop, on CI, and on every team member's machine. Rolling tags like `kitware/cmake:3` map to the digest current at last update, not whatever the registry serves today. That is true even for a tag you have never asked for before: resolving a brand-new identifier *grows* the copy with a new root and dispatch object, but re-resolving one already on disk never silently *refreshes* it under the default mode. An explicit [`ocx index update`][cmd-index-update] is the deliberate way to move it; a [`--remote`][arg-remote] resolve of that tag is the other, since — unlike a `--remote` *query* — it re-fetches and rewrites the local copy for the tag it touches (see [Active Index](#active)).

"Explicit" means *named*. An update moves the packages you list and nothing else — never a package as a side effect of moving another, and there is no *implicit* whole-index sync to move them all at once ([`ocx index sync`][cmd-index-sync] is the explicit one — see [Update modes](#update-modes)). That covers the `repository` field too — the pointer deciding which physical registry a package is fetched from is part of what a copy pins, not a detail refreshed in passing. And resolution stays silent about all of it: once something is committed locally, resolving it makes no network request and tells you nothing about the remote, because there is nothing it could act on. Updates surface where you asked for them — [`ocx index catalog --remote`][cmd-index-catalog] — never as a warning from a command that was doing something else.

::: info Similar to APT's package lists
[`apt-get update`][apt-repo-format] downloads a `Packages` index from each configured mirror, listing every `.deb`'s filename and checksum. `apt-get install` then reads that local index and verifies the downloaded file against the recorded checksum — the network is only involved during an explicit refresh and the file fetch itself, not on every dependency resolution. [`ocx index update <package>`][cmd-index-update] is the explicit-refresh equivalent of `apt-get update`.
:::

### Wire layout {#local-layout}

Every source's subtree under the home is the [index.ocx.sh][index-ocx-sh] wire grammar, byte-for-byte:

```
$OCX_HOME/index/ocx.sh/
├── config.json                published sources only — {"format_version": 1, "name_segments": 2}
├── c/index.json               published sources only — the package catalog
└── p/<ns>/
    ├── <pkg>.json              root document — repository pointer, tags, publisher status
    └── <pkg>/o/sha256/
        └── <hex>.json          dispatch object — filename is the object's own SHA-256
```

A **published** source — `index.ocx.sh` or a mirror of it — reaches this layout two ways, and
"verbatim" means something different for each. A full copy (`wget --mirror`, `rsync`, a shipped
tarball) reproduces the site byte-for-byte — every root document, every dispatch object, and
`config.json` itself are exactly the bytes the site served. That is what makes "copy a mirror" work:
`wget --mirror https://index.ocx.sh/` into `$OCX_HOME/index/ocx.sh/` produces a valid, *complete*
copy that verifies against nothing but itself — object filenames and the catalog, no separate trust
anchor.

[`ocx index update`][cmd-index-update] instead grows the tree package by package, and only its
dispatch objects are verbatim: each object's filename is its own recomputed SHA-256, decoded and
digest-reverified but never re-encoded. Its root documents are assembled **locally** — the merge
folds the tag(s) an invocation named, or every tag the site currently lists for a bare identifier,
into whatever this machine already committed, then re-serializes the result. A tag-scoped
`ocx index update cmake:3.28` therefore writes a root carrying that one tag, not the site's whole tag
list — a valid *partial* copy in the same grammar, not a byte-for-byte one. This is also what makes
the never-deletes rule below possible: a tag the site has since dropped survives locally because the
local root was never a replacement for the site's, only ever a merge into it. Every dispatch object
is verified on read against its own `o/` filename — a byte-tampered object fails to load rather than
resolving silently ([CWE-345][cwe-345]). A root document is cross-checked against its `c/index.json`
entry too, but that entry is a cached derivation rather than a trust anchor: a disagreement is
repaired from the root on disk, not refused (see [Crash-safe updates](#local-crash-safety)).

`config.json` follows the same split. A full copy of a site that already publishes one
(`index.ocx.sh` does) carries it verbatim. An `ocx index update`-grown tree gets one from OCX itself:
on the **first** successful update for a published source, OCX writes `{"format_version": 1}` if —
and only if — nothing already exists at that path. Written locally, it is never rewritten afterward
either — a wrong or stale `config.json` is repaired by deleting it and running `ocx index update`
again, never by any command editing it in place. See
[Serving a local index snapshot](#servable) for why this file existing, or not, matters.

A **derived** source — any plain OCI registry, `ghcr.io` or `docker.io` or a private one — has no site to copy, so OCX writes the root document itself, in the same grammar:

```json
// $OCX_HOME/index/ghcr.io/p/ocx-contrib/cmake.json
{
  "repository": "oci://ghcr.io/ocx-contrib/cmake",
  "tags": {
    "3.28": { "content": "sha256:<dispatch-digest>", "observed": "2026-07-18T09:00:00Z" }
  }
}
```

A derived source carries no `config.json` or `c/index.json` of its own — its catalog is simply the directory listing under `p/`. `observed` is the timestamp the pointer was last confirmed against its source — a "when was this taken" datum, not a freshness gate (the local index is [never auto-refreshed](#local)).

### Dispatch objects only {#local-dispatch}

`o/` holds **dispatch objects only** — the [OCI image index][oci-image-index] a tag resolved to, verbatim, for either provenance kind. A leaf platform manifest, the manifest that actually names a binary's layers, is never copied into the local index. The copy pins what a re-push can change — the tag→digest binding, the platform→digest map — and leaves everything below a digest to the [package store][in-depth-storage-packages], which fetches it on demand and is content-addressed anyway.

Every tag's `content` digest in the root document names an image index present in `o/`: OCX decodes it, runs [platform selection][reference-platforms-compatibility] over the per-platform digest list it carries, then fetches the resulting leaf digest from the physical registry, checking the [package store][in-depth-storage-packages] first — an already-installed tool's leaf manifest was cached there at install time, so re-resolving it needs no network at all. A tag that resolves to a bare platform manifest instead of an index is refused when the source announces it and is never recorded; `ocx package push` always publishes an index, so this can only happen for a repository ocx did not publish.

A digest-pinned reference (`pkg@sha256:…`) is content addressing, not dispatch — it is fetched directly by digest and never touches `o/`. A multi-platform package therefore holds exactly one dispatch object under `o/` per tag. Compared to copying the whole manifest chain, that is roughly a sixth the size for a typical multi-platform package.

::: info An incomplete copy self-heals
A dispatch object missing from a partially-synced copy — a package the copy has not fully cached yet — is, by construction, an OCI image index: OCX fetches it by digest, verifies it, and writes it into `o/` before dispatch continues, rather than failing. A partial copy heals a gap the moment it is asked to resolve it, with no separate repair step. This is unrelated to the digest-pinned references above, which never touch `o/` at all.
:::

### Crash-safe updates {#local-crash-safety}

[`ocx index update <pkg>`][cmd-index-update] writes three things per tag, in a fixed order, each its own atomic step: the dispatch object into `o/` (content-addressed — an orphan left by an aborted write is harmless, nothing points at it yet), the root document (tempfile plus rename), then the package's `c/index.json` catalog entry (tempfile plus rename). An interruption between any two steps — a crash, a kill signal — always recovers on the next read or update; there is no window where it leaves the copy looking corrupt.

The catalog entry is nothing more than the SHA-256 of the root document's own bytes — a cached derivation, not an independently trusted fact. A read that finds a stored root and its catalog entry disagree is therefore an **inconsistency**, not evidence of tampering: OCX recomputes the entry from the root actually on disk and rewrites the catalog to match, logged at info level. A hard error is reserved for corruption recomputation cannot fix — an unparseable root document, a dispatch object whose bytes disagree with its own `o/` filename, a failed `repository` cross-check.

A **later** catalog sync that finds the *remote* root digest has moved past the local one is a different case: that is staleness, reported as an update being available, never an error. Re-running [`ocx index update <pkg>`][cmd-index-update] re-snapshots it.

### Update modes {#update-modes}

[`ocx index update <package>`][cmd-index-update] syncs the local index for a specific package from its remote source:

- **Tagged identifier** (e.g., `kitware/cmake:3.28`) — adopts that one tag. Every sibling pin, and the `repository` pointer, stay exactly as committed. A tagged update is a statement about one version.
- **Bare identifier** (e.g., `cmake`) — adopts every tag the source currently lists, plus the package-level fields. Naming the package with no tag is the sanctioned point to take a routing migration: you asked about the package, so the package's own pointer moves.

**An update never deletes.** A tag the source has stopped listing stays in the local copy, with the digest it was pinned to, on both source kinds. The copy is not a mirror of the remote's current tag list — it is the record of what this machine snapshotted, so a publisher retiring a version cannot silently break a machine still pinned to it. Merge is the only write verb: local entries outside the scope of the update are never touched, and entries the remote dropped are never removed.

**Naming is the only mode, and there is no *implicit* whole-index sync — that is deliberate.** A
remote index floats by definition: packages appear, platforms get added to existing versions, tags
move. A local copy is not a mirror of it — it is the set of snapshots you asked for. Naming a package
directly is one way to ask; [`ocx index sync <REGISTRY>`][cmd-index-sync] is the other —
it reads that source's own catalog **at that instant** to choose the set, then does exactly the same
per-package work as if each were named bare. Nothing else moves a pin: not a resolve, not a listing,
not an update of a different package, and there is no spelling of "sync everything, kept in sync" —
`index sync` is a single explicit read, not a standing subscription, and repeated runs still only
ever add.

"Sync everything, automatically" has no well-defined meaning against a partially materialized copy: it
would either clone a floating remote (making the copy stop being a lock) or re-snapshot whatever
subset happens to be present (an arbitrary set nobody chose). `index sync` sidesteps that by being
a one-shot, operator-named act against one source's catalog, not a background sync — see
[Serving a local index snapshot](#servable) for the case it exists to serve.

Naming is also *who*: the index pins only packages the user named directly, so a [patch companion][user-patches-pins] — a package a descriptor names on the operator's behalf — pins in its own patch-tier state (`state/patch-companions/`) instead, and never as a local-index entry.

Tag-scoped mode is ideal for lockfile workflows where the local index should hold only explicitly requested tags. Packages not listed are not touched — not their tag pins, and not the `repository` pointer that decides which registry the package is fetched from — and nothing is fetched about them either: an update requests the named packages' roots and their dispatch objects, and stops.

To ask what the source has now, ask the source: [`ocx index catalog --remote`][cmd-index-catalog] lists what it publishes, [`ocx index list --remote`][cmd-index-list] the tags of one package. Without `--remote` both answer from the local copy — the index you maintain. "Am I behind?" is a question about the remote, so it is asked out loud rather than tracked in local shadow state.

### Fresh-machine fallback {#fresh-machine}

On a fresh machine, [`ocx index update`][cmd-index-update] does not need to run before the first [`ocx package install kitware/cmake:3.28`][cmd-package-install]. When the local index has no entry for a requested tag, [`ocx package install`][cmd-package-install] transparently resolves that single tag against the configured remote, writes the dispatch object, root, and catalog entry, and proceeds with the install. Subsequent commands — including [`--offline`][arg-offline] — then work from the cached entry without touching the network.

Refreshing an already-cached tag or discovering every tag for a repository is still the job of [`ocx index update`][cmd-index-update]; the fallback only covers the specific tag being installed.

## Serving a local index snapshot {#servable}

A local index subtree — grown incrementally by [`ocx index update`][cmd-index-update], snapshotted in
bulk by [`ocx index sync`][cmd-index-sync], or repaired by
[`ocx index regenerate`][cmd-index-regenerate] — is not only a client-side cache. Once its
[`config.json`](#local-layout) exists, it is a complete, self-describing copy of [the one wire
format](#format) that any OCX client can read back: over HTTPS from any static file server, or
straight off disk with no server at all. A curated corporate mirror is a pipeline of existing `ocx`
commands, with no bespoke server and no `ocx-mirror` code required.

### Why an OCX-grown tree could not be served before {#servable-defect}

`config.json` is the format's version marker, and until it exists, serving a tree grown by
[`ocx index update`][cmd-index-update] back to another OCX client silently failed: a `config.json`
fetch that 404s made the client treat the whole source as **not an index** and stop before ever
requesting a package's root document — a tree containing every package asked about reported
not-found for all of them, with no diagnostic. `ocx index update` never wrote this file; only the
hosted [`index.ocx.sh`][index-ocx-sh] renderer did, so a locally-grown tree never had one to begin
with.

Two changes close the gap:

1. **`ocx index update` and `ocx index sync` now write `config.json`** — exactly
   `{"format_version": 1}` — the first time either successfully refreshes a published source, if
   nothing already exists at that path. Write-if-absent, never rewritten afterward: a copy of a site
   that already publishes its own `config.json` is left untouched (see [Wire layout](#local-layout)).
2. **An absent `config.json` now means "version 1", at every reader alike** — the local reader, the
   HTTPS reader, and the [`file://`](#servable-consuming) reader. A tree with no `config.json` at
   all — grown by an `ocx` predating this change, or hand-assembled — now resolves instead of failing
   closed. A `config.json` that names an unsupported `format_version` is still a hard error
   ([exit 65][exit-codes]), identically on every reader: only what *absence* means changed, never
   what a served, unsupported version means.

### Serving and consuming the tree {#servable-consuming}

Once a source subtree holds every package needed — grown one package at a time, snapshotted in bulk,
or repaired — copy it wherever it needs to go and point a consuming `ocx` at the copy two ways:

- **Over HTTPS**, from any static file server — `python3 -m http.server`, nginx, an internal artifact
  bucket. Configure [`[registries."<ns>"] index = "https://…"`][config-registries-index] as usual.
- **Straight off disk, with no server at all**, via a `file://` base:

  ```toml
  [registries."corp"]
  index = "file:///srv/ocx-index/corp"
  ```

  See [`file://` bases][config-registries-index-file] for the exact requirements — empty authority,
  absolute path — and its read-only, bounded, symlink-contained guarantees. This is the shape an
  air-gapped machine uses: no HTTP server, no TLS, and no network stack at all between the copy and
  the consumer.

Either way the served tree is read through the identical [ocx-index protocol](#public-index-pipeline)
`index.ocx.sh` itself uses — object digests still verify, and `yanked` / `deprecated` status still
surfaces (see [Status surfacing](#public-index-status)) — so a consuming `ocx` cannot distinguish a
hand-run static file server, a `file://` checkout, and the hosted site.

### The air-gap pipeline, end to end {#servable-air-gap}

1. On a connected machine, snapshot the packages needed — named directly
   (`ocx index update cmake ninja`), or in bulk from a source's own catalog
   (`ocx index sync ocx.sh`).
2. Copy `$OCX_HOME/index/<source>/` — the whole subtree, `config.json` included — onto media, into a
   git repository, or onto a host that can reach the air-gapped network.
3. On the air-gapped machine, point `ocx` at the copy:
   [`[registries."<ns>"] index = "https://mirror.corp/…"`][config-registries-index] for a served
   copy, or `index = "file:///…"` for one staged directly on disk.
4. `ocx package install <pkg>:<tag>` resolves to the exact platform-manifest digest the connected
   machine pinned — the copy is byte-identical to the source subtree, so nothing about resolution
   differs from resolving against the original.
5. That resolve only names a digest; the manifest and layer bytes behind it are content, and the
   index never carries them (see [Dispatch objects only](#local-dispatch)). Fetching them still needs
   a registry the air-gapped host can reach — the root document's `repository` field still points at
   the upstream host, so route that traffic through a [`[mirrors]`][config-mirrors] **registry** role
   pointed at a registry inside the air-gapped network, the same way any other disconnected OCI pull
   is served. `ocx package install` then materializes the package from there.

Nothing in this pipeline is `ocx-mirror`-specific. It is existing `ocx` commands, a copy step, and,
optionally, a static file server — the index half of an air-gapped install; step 5 is the registry
half every disconnected OCI setup already needs.

### Repairing drift with `regenerate` {#servable-regenerate}

`c/index.json` is derived data — every entry restates a digest the root document beside it already
carries — and every writer except one only ever *adds* to it (see [Crash-safe updates](#local-crash-safety)).
That leaves one drift nothing else repairs: an entry naming a package whose root document is gone,
deleted by hand or pruned by an external tool. [`ocx index regenerate <REGISTRY>`][cmd-index-regenerate]
re-derives the whole catalog from the `p/` tree on disk and replaces it wholesale — the one operation
that clears such an entry. It never contacts a source and moves no pin, so [`--frozen`][arg-frozen]
and [`--offline`][arg-offline] both permit it: [`ocx index update`][cmd-index-update] and
[`ocx index sync`][cmd-index-sync] are the commands both flags refuse; `regenerate` is the only one
whose *purpose* is to write, rewriting the
catalog deliberately; [`catalog`][cmd-index-catalog] and [`list`][cmd-index-list] are permitted
because they read the local copy — though `list` can still trigger the same read-path self-heal
described below on an already-drifted tree, which is a write neither flag gates. It never writes
`config.json` either — `name_segments` is an operator declaration no tree can be read for, so
guessing one while repairing a foreign tree would be wrong.

::: warning `regenerate` does not follow symlinks under `p/`
Because the derivation walks the filesystem and the catalog it writes replaces the previous one
**wholesale**, a symlink under `p/` is not a missing entry — it is silent removal from
`c/index.json`. A symlinked root **document** is skipped by the walk. A symlinked **directory** is
never queued at all, which takes every root beneath it with it in one step — a whole namespace or
package tree can vanish from the catalog in a single `regenerate` run. The packages still resolve by
tag, since tag resolution reads root documents directly and never through the catalog, but they
disappear from [`ocx index catalog`][cmd-index-catalog], from [`ocx index sync`][cmd-index-sync] enumeration elsewhere,
and from anything else that reads `c/index.json`. If a served tree deduplicates a shared package
across locations with symlinks, `regenerate` needs the real files underneath, not links to them —
hardlinks are unaffected, since a hard-linked file *is* a regular file to the walk.

The removal is not always permanent: on a tree this machine also *resolves* — not merely serves — a
resolve's own catalog self-heal re-adds a dropped entry the next time the package is resolved, since
the root read behind it follows symlinks directly and the catalog is only ever a cache of what roots
are on disk. It sticks precisely for the case this warning is written for: a tree that is served and
never locally resolved.
:::

## Active Index {#active}

Every command that resolves a package identifier — [`ocx package install`][cmd-package-install], [`ocx package which`][cmd-which], [`ocx package exec`][cmd-exec], [`ocx index list`][cmd-index-list] — uses one working index for that invocation. By default, this is the local index. Two flags change which index is used:

| Mode | Flag | Source | Network? |
|---|---|---|---|
| Default | *(none)* | Local index | No (unless fetching a new binary) |
| Remote | [`--remote`][arg-remote] | OCI registry | Yes |
| Offline | [`--offline`][arg-offline] | Local index | Never |

**`--remote`** forces tag and catalog lookups to query the registry directly for a single command,
and the two shapes that can take it differ in what happens to the local index. A **query**
([`index list --remote`](#index-list), [`index catalog --remote`](#index-catalog),
[`package description pull --remote`][cmd-package-info]) reads the registry and reports — the local index is
**not** updated. A **resolve** (`package install --remote`, `package exec --remote`, and similar)
still writes, despite the flag's name: it re-fetches the tag it resolved and rewrites the local copy
for that one tag, exactly like an [`ocx index update`][cmd-index-update] scoped to it (see
[Two-hop fetch and caching](#public-index-caching)) — `--remote` only skips the *local-first* check,
not the write. Layer data fetched under either shape still writes through to the
[package store][in-depth-storage-packages]. Use a query for a one-off check without touching the
index; a `--remote` resolve is a way to force-refresh one tag without typing
`ocx index update <pkg>:<tag>` first.

**`--offline`** prevents all network access for that command. If the local index does not have a requested package, the command fails immediately rather than attempting a registry query. Useful to verify that the current index and package store are self-sufficient before a build in a restricted or air-gapped environment.

[`--index`][arg-index] / [`OCX_INDEX`][env-ocx-index] do not change the active index *mode* — the local index remains active. They only change *which collection* is read. See [Shipped copies](#bundled).

The active index controls tag and manifest resolution only. The [package store][in-depth-storage-packages] is independent — installed binaries are accessible in all three modes regardless of which index is active.

## Shipped copies {#bundled}

A local index subtree is small — root documents and dispatch objects only, no layer archives, no binaries — small enough to ship *inside* a tool release. [Bazel Rules][bazel-rules], [GitHub Actions][github-actions-docs], and [DevContainer Features][devcontainer-features] can bundle a frozen copy at release time and set [`OCX_INDEX`][env-ocx-index] (or pass [`--index`][arg-index]) to point OCX at it. Consumers write `kitware/cmake:3` and the bundled copy resolves it deterministically — with zero network dependence and zero dependence on any other machine-global state — while the [package store][in-depth-storage-packages] and [install symlinks][in-depth-storage-symlinks] stay in `OCX_HOME` as usual.

This produces a two-level pin on *version choice*, not on `ocx.lock` — a devcontainer feature or a GitHub Action has no lock file of its own, so this index-level pin is the only determinism it gets: the tool version pins the bundled index copy, which pins the resolved binary. A version bump to the action or rule — proposed automatically by [Dependabot][dependabot] or [Renovate][renovate] — advances the bundled copy. Users get the updated binary with no config changes.

::: tip GitHub Action with a bundled index
```yaml
- uses: ocx-actions/setup-cmake@v2.1.0   # pins action → pins index → pins binary
  with:
    version: "3.28"                       # human-readable tag, no platform conditions
```

`@v2.1.0` pins everything end-to-end. `@v2` follows minor releases — as the maintainer ships updated index copies, `kitware/cmake:3.28` may resolve to a newer build when the action version changes. No SHA256 lists, no `if: runner.os == 'Linux'` conditionals.
:::

The contrast with maintaining a [hand-curated URL matrix][toolchains-llvm] — one `filename → checksum` entry per `version × os × arch` — is clear: a version bump means editing one rule version, not a dictionary.

::: warning Content still needs a network the first time — unless the layer cache is already warm
A shipped copy resolves the tag → platform-manifest digest offline, but the manifest bytes and layers themselves are content, fetched on demand from the registry the first time a given digest is installed. "First time" is scoped to the [layer store][in-depth-storage-layers] and the blob store, not to `packages/`: copying `blobs/`, `layers/`, and `index/` into a fresh `$OCX_HOME` — never `packages/` — is enough. A package's `packages/…/content/` directory is a hardlink assembly of its layers, built locally at install time; if every layer a package needs is already on disk, `ocx package install` reassembles `content/` from the local layer cache with no network at all, even though `packages/` itself started out empty. Only a genuinely cold layer needs egress.
:::

## index.ocx.sh {#public-index}

An OCI registry path is a physical detail — a hostname and repository path — but a package's *identity* is logical: "the `cmake` package published under `ocx.sh`". When a maintainer migrates the backing registry (say, from a self-hosted GHCR org to Docker Hub), every consumer that pinned the physical `ghcr.io/…` path directly breaks, even though the package itself did not change.

[`index.ocx.sh`][index-ocx-sh] is a pointer index, not a registry: it carries no `/v2` API and stores no blobs. It maps a stable logical identifier (`ocx.sh/<namespace>/<package>`) to the physical registry currently hosting it, plus the per-platform content digests recorded there. OCX resolves `ocx.sh/kitware/cmake:3.28` by asking the index for the current physical location and digest, then fetching the actual manifest from that physical registry — the OCI image-index hop a direct registry resolve performs is served by the index instead of the registry, not skipped: the hop count is the same, only which side answers it changes. The index HTTP client ships its own bundled CA root set, the same source the main OCI client uses, so root and index-object fetches work on a minimal container with no system CA store installed.

For any `ocx.sh/<namespace>/<package>` identifier, the public index is consulted **before** the OCI registry — never the other way around — so a logical reference always resolves through the verified two-hop path below rather than a registry that happens to serve a repository under the same name. Identifiers on any other registry are unaffected; the index is never consulted for them. This wiring ships in the binary: `ocx.sh` names `https://index.ocx.sh` in the [compiled-defaults tier][config-precedence], and any config tier can point it elsewhere or set [`index = ""`][config-registries-index] to resolve `ocx.sh` as a plain OCI registry instead.

### Resolution pipeline {#public-index-pipeline}

```
logical id (ocx.sh/<ns>/<pkg>[:tag])
  → index resolve   : GET p/<ns>/<pkg>.json (root) → tags[tag].content (image-index digest)
                      → GET p/<ns>/<pkg>/o/<algo>/<hex>.json (verify sha256 of bytes)
                      → manifests[]: select the platform matching this host
  → physical        : root.repository, e.g. "oci://ghcr.io/ocx-contrib/cmake"
  → mirror_map      : rewritten through [mirrors]'s registry role, if configured for this host
  → fetch           : GET physical-registry /v2/.../manifests/<leaf-digest> (verify OCI CAS)
```

`index.ocx.sh` yields **only pointers** — the platform manifest and its layers always come from the physical registry named by `repository`. The `repository` field carries an `oci://` scheme marker identifying it as a physical, transport-only reference; it is never used as a storage key. Locally, OCX keys everything — the local index path, `ocx.lock`, garbage-collection roots — on the *logical* identifier, so a registry migration never orphans a local copy or breaks a committed team lock.

### A configured index owns its whole registry {#public-index-declared-names}

`config.json` carries the wire-format version, and nothing the client reads decides scope:

```json
{ "format_version": 1 }
```

If a namespace names an index in [`[registries.<name>]`][config-registries], **only what that index holds is discoverable through it**. A reference the index has no root for is a hard miss: OCX does not fall back to the plain OCI registry the index points at, even when that registry serves a repository under the same name. The refusal names the index that was consulted, so a package that has not been announced yet reads as exactly that:

```
'ocx.sh/go-task:3.44' is not in the index at https://index.ocx.sh, which is authoritative
for every name in registry 'ocx.sh'; announce it there with `ocx package announce`, or take
the namespace off the index with `[registries."ocx.sh"] index = ""`
```

The scope is the registry, not a name shape. `index.ocx.sh`'s own root schema pins a logical name to `ocx.sh/<namespace>/<package>`, so a flat `ocx.sh/<tool>` can never hold a root there — but that is the index operator's constraint, enforced when a package is announced, not a rule the client applies. An index that does serve a root for a flat name resolves it normally.

This is what makes the yank and deprecation gate reachable at all: a tag the index yanks cannot be obtained by asking the registry underneath instead. It also means **completeness matters** — a package missing from the index is unresolvable through it, by design.

`config.json` itself being absent is the same case, not a different one: it is read as `{"format_version": 1}` (see [Serving a local index snapshot](#servable) for what this replaced — a missing `config.json` used to make the whole source unreadable rather than resolving as version 1). Its `name_segments` field, present or absent, plays no part in this either way: it never decides whether a name is in the index's jurisdiction.

::: warning An index that breaks fails loud, not silent
The same authority makes every failure terminal: a yanked tag, a tampered index object, an unrecognized `config.json` version, or the endpoint being unreachable all surface as a hard error, never a silent drop to a registry that happens to serve a repository under the same name. So an `index.ocx.sh` outage blocks `ocx.sh/…` resolution rather than quietly resolving it somewhere else. An outage is never reported as an absent package — the error names the endpoint that failed. Namespaces on other registries are untouched.

Two things opt `ocx.sh` out: [`index = ""`][config-registries-index], and pinning the namespace at a [`[mirrors]` registry endpoint](#mirroring), which suppresses the compiled-in default so a site that already routes `ocx.sh` elsewhere never gains a host it did not allow-list. A mirror keyed on the *index* host is the opposite move — it redirects the index endpoint rather than replacing it, so the verified two-hop path stays, served by that host. Only a config file the operator controls suppresses: neither the [managed tier][config-managed] nor a forwarded `OCX_MIRRORS` can revoke the verified path, though both still redirect traffic.
:::

### Two-hop fetch and caching {#public-index-caching}

| Object | Volatility | OCX behaviour |
|---|---|---|
| `p/<ns>/<pkg>.json` root | Volatile — the `repository` pointer can move by maintainer PR, tags are curated | Copy-first: never auto-refreshed under the default mode. A live re-fetch happens only on an explicit [`ocx index update`][cmd-index-update] or a [`--remote`][arg-remote] resolve, which rewrites the local copy and bumps `observed`. |
| `o/<algo>/<hex>.json` — the OCI image index this tag resolved to, verbatim | Immutable | Fetched once, verified against its own SHA-256 filename, cached forever. |
| `c/index.json` catalog | Volatile | **Never copied.** The local `c/index.json` is authored: each entry is the hash of the local root document beside it, written in the same step that writes that root. The site's own catalog is fetched live only when you ask for it (`ocx index catalog --remote`) and nothing from it is stored. That is what keeps the local tree free of per-machine bookkeeping — there is no record of remote state in it to go stale. |

A registry alias that drifted between announces — the case the first row above is built to tolerate, where the registry-side digest moves past what the index still has committed — is exactly what [`ocx package cascade check`][cmd-package-cascade-check] flags from the publisher's side: it compares a package's live registry tags against the namespace's committed root and reports the mismatch as index staleness. [`ocx package cascade repair`][cmd-package-cascade-repair] fixes a drifted registry alias itself; the follow-up [`ocx package announce --tags-file`][cmd-package-announce] then re-observes the repaired tags and re-publishes the moved digests into the index. Neither command touches any machine's local copy — that hop is still [`ocx index update`][cmd-index-update], run whenever a particular machine wants the newly announced root.

### Local layout for index.ocx.sh sources {#public-index-layout}

The local index uses the identical `p/<ns>/<pkg>.json` + `o/<algo>/<hex>.json` shape for an `index.ocx.sh` source as for any other — it is [one wire format](#format), not a special case:

```
$OCX_HOME/index/ocx.sh/
├── config.json
├── c/index.json
└── p/kitware/
    ├── cmake.json              root doc — assembled locally from snapshotted tags, not a verbatim copy
    └── cmake/o/sha256/
        └── <index-digest>.json   OCI image index, verbatim (immutable)
```

The stored object *is* the registry's OCI image index — its `manifests[].digest` entries are the per-platform manifest digests OCX needs, each independently verified against the physical registry's OCI content-addressed storage when actually fetched.

The index's file format is specified by the index repository: [wire format][index-wire-format].

### Status surfacing {#public-index-status}

A package's root document carries publisher-set status fields, which OCX surfaces but never silently acts on:

| Field | Behavior |
|---|---|
| `yanked` (per tag) | A tag resolve against a yanked entry prints a warning and is refused by default — a yank is a publisher signal, not a delete. This surfaces identically whether the root was just fetched live or read from a [committed or shipped local copy](#bundled) with zero network: the same field, the same refusal. Set [`OCX_ALLOW_YANKED`][env-ocx-allow-yanked] to opt in and resolve it anyway. A digest-pinned resolve of the same content never needs the opt-in, since immutable content cannot itself be "yanked". |
| `deprecated` + message | Resolve prints a warning; the message is surfaced in [`ocx package description pull`][cmd-package-info]. |
| `superseded_by` | Shown as advisory information in [`ocx package description pull`][cmd-package-info] and resolve diagnostics; OCX never auto-follows it — that would silently substitute a different package than the one you asked for. |

::: info Open interop point — description assets
Index objects share the same object store as `desc` blobs (README text, logo images) the index may carry for a package, but that part of the wire format is not yet frozen. [`ocx package description pull --save-readme`][cmd-package-info] / `--save-logo` stay driven by registry-side metadata for now, independent of `index.ocx.sh`.
:::

## Route index traffic through a mirror {#mirroring}

[Corporate mirrors][config-mirrors] redirect OCX's traffic host-by-host — and cover two different roles a host might serve. `index.ocx.sh`'s root, index-object, and catalog fetches are plain HTTPS, not OCI distribution calls, so a mirror entry has to say which kind of traffic it is redirecting.

The [`[mirrors]`][config-mirrors] table value is either a plain string, which redirects both roles for a host, or an object that splits them:

```toml
[mirrors]
"index.ocx.sh" = { index = "https://artifactory.corp/ocx-index" }   # index role only
"ghcr.io" = "https://artifactory.example.com/ghcr-remote"           # both roles → registry-only host
```

Same doctrine as the registry role: the value replaces the base URL wholesale, there is no fallback to the public origin, and the table merges through the [managed-config][config-managed] tier the same way, per role. Every root, index-object, and catalog fetch still verifies its content against the recorded SHA-256 digest — the mirror changes only where the bytes come from, never whether they are trusted.

See [`[registries.<name>]`][config-registries] for the separate question of *which* namespaces resolve through the ocx-index protocol at all — a mirror only redirects an already-selected protocol's traffic, it does not select the protocol.

## Keep tags {#keep-tags}

A registry tag can be deleted by mistake, even when a digest it once pointed at is still pinned by someone's `ocx.lock`. Registry-side retention policies key on tags, not on which digests are "in use" somewhere — so a stray delete can leave a manifest orphaned and eligible for garbage collection on the registry side.

[`ocx package push`'s `--keep-tag`/`--no-keep-tag`][cmd-package-push] closes this gap on the registry side, on by default: for every platform manifest pushed **in that invocation**, OCX also pushes a digest-named `__ocx.keep.<algorithm>-<hex>` tag alongside the version tag. As long as that keep tag exists, the registry sees the manifest as referenced, regardless of what happens to the human-readable tags around it. There is no retroactive tagging — a push does not reach back and keep-tag manifests published by earlier pushes.

A digest whose algorithm makes the tag longer than the OCI limit of 128 characters — `sha512`, at 146 — gets no keep tag at all. The digest is never truncated to fit: two digests sharing one truncated tag would silently drop a manifest's protection, which is worse than leaving it unprotected in the open.

Because the local index never copies a leaf platform manifest — only its [dispatch object](#local-dispatch) — an index-resolved install always fetches the leaf from the registry on demand, sometimes long after the index entry was written. A manifest stays reachable as long as *anything* still references it — a version tag, or an image index that still lists it in its platform map — so most manifests never need a keep tag to stay alive. Keep tags are a safeguard OCX offers for the case that falls through the cracks: a tag moved or a patch released such that nothing else points at the old digest any more, and it would otherwise become garbage-collection-eligible before an index-resolved install ever fetches it.

Keep tags are a pure registry-side safety net scoped to `ocx package push` — [`ocx config push`][cmd-config-push] has no `--keep-tag` flag, and [`index.ocx.sh`](#public-index) ignores keep tags entirely; they carry no wire semantics on the index side.

## The `update` family {#update-family}

Four OCX commands share the `update` verb. Each refreshes exactly one record, and confusing them means refreshing the wrong one.

| Command | Refreshes |
|---|---|
| [`ocx index update`][cmd-index-update] | The local index at [`--index` ▸ `OCX_INDEX` ▸ `$OCX_HOME/index/`][arg-index] |
| [`ocx self update`][cmd-self-update] | The managed ocx installation itself |
| [`ocx config update`][cmd-config-update] | The managed-config snapshot |
| [`ocx update`][cmd-update] | A project's `ocx.lock` |

A fifth command belongs to the family without carrying the verb: [`ocx index sync`][cmd-index-sync] refreshes the same record `ocx index update` does, over a whole registry's catalog rather than a named list of packages.

`ocx update` never writes a **tag pointer** into the local index — `ocx.lock` is its only canonical record. It can still persist a resolved dispatch object into `o/`, content-addressed and pinning nothing, so that write moves no tag. Re-resolving a project's pinned tools therefore does not change what `kitware/cmake:3` resolves to for any other command on the same machine; that stays [`ocx index update`][cmd-index-update]'s job.

## See Also

- [Indices section in the user guide][user-indices] — how-to: refresh, work offline, use `--remote`
- [Storage][in-depth-storage] — the local index's home relative to the other stores under `$OCX_HOME`
- [Versioning][in-depth-versioning] — tag mutability, locking by digest, `_` build suffix
- [Configuration][in-depth-configuration] — `[registries]` and `[mirrors]` config-driven defaults
- [`file://` bases][config-registries-index-file] — the exact requirements for consuming a
  [servable snapshot](#servable) with no server at all

<!-- external -->
[oci-dist-tag]: https://github.com/opencontainers/distribution-spec/blob/main/spec.md#pulling-manifests
[oci-image-index]: https://github.com/opencontainers/image-spec/blob/main/image-index.md
[apt-repo-format]: https://wiki.debian.org/DebianRepository/Format
[bazel-rules]: https://bazel.build/extending/rules
[github-actions-docs]: https://docs.github.com/en/actions/writing-workflows/choosing-what-your-workflow-does/using-pre-written-building-blocks-in-your-workflow
[devcontainer-features]: https://containers.dev/implementors/features/
[dependabot]: https://docs.github.com/en/code-security/dependabot/working-with-dependabot/keeping-your-actions-up-to-date-with-dependabot
[renovate]: https://docs.renovatebot.com/
[toolchains-llvm]: https://github.com/bazel-contrib/toolchains_llvm/blob/master/toolchain/internal/llvm_distributions.bzl
[index-ocx-sh]: https://index.ocx.sh
[index-wire-format]: https://index.ocx.sh/docs/reference/wire-format

<!-- security -->
[cwe-345]: https://cwe.mitre.org/data/definitions/345.html

<!-- commands -->
[cmd-package-install]: ../reference/command-line.md#package-install
[cmd-which]: ../reference/command-line.md#which
[cmd-exec]: ../reference/command-line.md#package-exec
[cmd-index-update]: ../reference/command-line.md#index-update
[cmd-index-sync]: ../reference/command-line.md#index-sync
[cmd-index-regenerate]: ../reference/command-line.md#index-regenerate
[cmd-index-catalog]: ../reference/command-line.md#index-catalog
[cmd-index-list]: ../reference/command-line.md#index-list
[cmd-package-push]: ../reference/command-line.md#package-push
[cmd-package-announce]: ../reference/command-line.md#package-announce
[cmd-package-cascade-check]: ../reference/command-line.md#package-cascade-check
[cmd-package-cascade-repair]: ../reference/command-line.md#package-cascade-repair
[cmd-config-push]: ../reference/command-line.md#config-push
[cmd-package-info]: ../reference/command-line.md#package-description-pull
[cmd-self-update]: ../reference/command-line.md#self-update
[cmd-config-update]: ../reference/command-line.md#config-update
[cmd-update]: ../reference/command-line.md#update
[cmd-direnv-export]: ../reference/command-line.md#direnv-export
[arg-remote]: ../reference/command-line.md#arg-remote
[arg-offline]: ../reference/command-line.md#arg-offline
[arg-frozen]: ../reference/command-line.md#arg-frozen
[arg-index]: ../reference/command-line.md#arg-index
[exit-codes]: ../reference/command-line.md#exit-codes

<!-- environment -->
[env-ocx-home]: ../reference/environment.md#ocx-home
[env-ocx-index]: ../reference/environment.md#ocx-index
[env-ocx-allow-yanked]: ../reference/environment.md#ocx-allow-yanked

<!-- reference -->
[config-mirrors]: ../reference/configuration.md#keys-mirrors
[config-registries]: ../reference/configuration.md#keys-registries
[config-registries-index]: ../reference/configuration.md#keys-registries-index
[config-registries-index-file]: ../reference/configuration.md#keys-registries-index-file
[config-precedence]: ../reference/configuration.md#precedence
[config-managed]: ../reference/configuration.md#keys-managed
[reference-platforms-compatibility]: ../reference/platforms.md#compatibility

<!-- internal -->
[user-indices]: ../user-guide.md#offline
[user-patches-pins]: ../user-guide/patches.md#patches-pins
[in-depth-storage]: ./storage.md
[in-depth-storage-packages]: ./storage.md#packages
[in-depth-storage-layers]: ./storage.md#layers
[in-depth-storage-symlinks]: ./storage.md#symlinks
[in-depth-versioning]: ./versioning.md
[in-depth-versioning-locking]: ./versioning.md#locking
[in-depth-configuration]: ./configuration.md
