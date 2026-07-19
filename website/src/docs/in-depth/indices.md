---
outline: deep
---
# Indices

OCI tags are mutable. The [OCI Distribution Specification][oci-dist-tag] defines tags as registry-side aliases that can be re-pointed at any moment — `cmake:3` today may resolve to a different digest after the next patch release. Choosing a version by tag — a devcontainer parameter, a Gradle property, a bare CLI argument — needs to resolve to the same digest every time you make that same choice, on your laptop, on CI, and on a machine that has never talked to the registry before.

OCX solves this with a local index: a directory holding a local copy of registry resolution data that only changes when explicitly refreshed. This page explains what an index actually is — one wire format, copied or generated in different places — how the local copy resolves without a network round trip, and how OCX resolves packages published through the [`index.ocx.sh`][index-ocx-sh] public index. The user-facing surface — when to refresh, offline mode, `--remote` — lives in the [Indices section of the user guide][user-indices].

:::info An index resolves versions, not `ocx.lock`
An index answers one question: which versions of a package exist, and for a chosen version, which platform-manifest digest and physical registry location it points to. It never appears in `ocx.lock` resolution — a lock already records the exact platform-manifest digest it pinned (see [Locking][in-depth-versioning-locking]), so resolving a lock reads that digest straight off disk and fetches it, without consulting any index. What an index buys is free *version choice*: something that names a tag rather than a digest — a devcontainer feature parameter, a Gradle property, a bare `ocx install cmake:3.28` — still needs to resolve deterministically and offline, which is exactly the gap a [shipped index copy](#bundled) closes.
:::

## One format, many copies {#format}

There is exactly one index format: a `config.json` version marker, a catalog `c/index.json`, per-package root documents `p/<ns>/<pkg>.json`, and per-package dispatch objects `p/<ns>/<pkg>/o/<algo>/<hex>.json`. Every copy of an index anywhere — the hosted [index.ocx.sh][index-ocx-sh] site, a corporate mirror of it, a directory shipped inside a [DevContainer Feature][devcontainer-features], the machine-local cache under `$OCX_HOME` — is that identical layout. Copying it with `rsync`, `wget --mirror`, or `git add` produces a valid index; there is nothing to convert.

Indices differ along three axes, never in format:

- **Location** — hosted (`index.ocx.sh` itself), mirrored (a corporate static-file proxy in front of it), shipped (a devcontainer feature, a CI artifact, a directory committed to a repository), or local to a machine (`$OCX_HOME/index/<source>/`).
- **Provenance** — *published*: a copy of a remote ocx-index, bytes copied from it and verifiable against it (see [index.ocx.sh](#public-index)). Or *derived*: OCX builds it itself from a plain OCI registry's tags, because an ordinary OCI registry publishes no index of its own.
- **Completeness** — a local copy is normally *partial*: [`ocx index update <pkg>`][cmd-index-update] grows it one package at a time. A full site mirror (`wget --mirror`) is just the degenerate *complete* case — same grammar, more entries.

## Remote Index {#remote}

The remote index is the live OCI registry (or, for a package identified under `ocx.sh`, the live [`index.ocx.sh`][index-ocx-sh] service). It answers metadata queries — which tags exist for a package, which digest a tag currently points to, which platforms a manifest declares — directly and authoritatively.

[`ocx index catalog`][cmd-index-catalog] browses available packages; [`ocx index list`][cmd-index-list] lists tags for a specific package. Both query the live registry when the global flag [`--remote`][arg-remote] is set.

The remote index is also the data source for [`ocx index update`][cmd-index-update]. Refreshing the local index means querying the remote for a tag's resolution data and writing a dispatch object, a root document, and a catalog entry to disk — never the platform manifest itself (see [Dispatch objects only](#local-dispatch)).

## Local Index {#local}

The local index reads from a self-contained collection under [`$OCX_HOME`][env-ocx-home] — one subtree per source, each independently resolvable with no network. Resolving `cmake:3` reads the `ocx.sh` (or whichever registry's) subtree and verifies a digest locally; nothing is fetched over the network unless the requested tag is missing.

The home is resolved in order:

| Precedence | Source | Note |
|---|---|---|
| 1 | [`--index PATH`][arg-index] | Highest precedence — points at a specific index collection |
| 2 | [`OCX_INDEX`][env-ocx-index] | Set by a deployment that ships its own copy — a devcontainer feature, a CI step. Never ambient; [`ocx direnv export`][cmd-direnv-export] does not touch it. |
| 3 | `$OCX_HOME/index/` | Default — the machine-local collection |

The home is a **collection**, not a single index: `$OCX_HOME/index/ocx.sh/`, `$OCX_HOME/index/ghcr.io/`, … one subtree per source, side by side. Pointing `--index` / `OCX_INDEX` elsewhere swaps the *whole* collection for a shipped one — never a partial overlay of the two. A shipped copy is not required to live at any particular path; a project conventionally commits one at `.ocx/index/`, but that convention is not enforced.

OCX never writes a lock file inside the collection itself, wherever it lives. Cross-process locks for an index update are always homed under the machine-global `$OCX_HOME/locks`, keyed by the guarded directory's own identity — so a redirected or git-committed `.ocx/index/` copy only ever changes when [`ocx index update`][cmd-index-update] actually writes new data into it, never picks up a stray lock sidecar `git status` would flag.

**The local index is never updated automatically.** You decide when a source's subtree changes. Until you explicitly refresh it, the same identifier always resolves to the same digest — on your laptop, on CI, and on every team member's machine. Rolling tags like `cmake:3` map to the digest current at last update, not whatever the registry serves today. That is true even for a tag you have never asked for before: resolving a brand-new identifier *grows* the copy with a new root and dispatch object, but re-resolving one already on disk never silently *refreshes* it — the only way to change what an already-known tag resolves to is an explicit [`ocx index update`][cmd-index-update].

::: info Similar to APT's package lists
[`apt-get update`][apt-repo-format] downloads a `Packages` index from each configured mirror, listing every `.deb`'s filename and checksum. `apt-get install` then reads that local index and verifies the downloaded file against the recorded checksum — the network is only involved during an explicit refresh and the file fetch itself, not on every dependency resolution. [`ocx index update <package>`][cmd-index-update] is the explicit-refresh equivalent of `apt-get update`.
:::

### Wire layout {#local-layout}

Every source's subtree under the home is the [index.ocx.sh][index-ocx-sh] wire grammar, byte-for-byte:

```
$OCX_HOME/index/ocx.sh/
├── config.json                published sources only — {"format_version": 1}
├── c/index.json (+ .etag)     published sources only — catalog + conditional-GET validator
└── p/<ns>/
    ├── <pkg>.json              root document — repository pointer, tags, publisher status
    └── <pkg>/o/sha256/
        └── <hex>.json          dispatch object — filename is the object's own SHA-256
```

A **published** source — `index.ocx.sh` or a mirror of it — copies these bytes verbatim from the hosted site; nothing is OCX-minted. Every object's filename is its own recomputed SHA-256, and every root document is verified on read against the digest its `c/index.json` catalog entry carries — a byte-tampered object fails to load rather than resolving silently ([CWE-345][cwe-345]). That is what makes "copy a mirror" work: `wget --mirror https://index.ocx.sh/` into `$OCX_HOME/index/ocx.sh/` produces a valid, *complete* copy that verifies against nothing but itself — object filenames and the catalog, no separate trust anchor.

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

`o/` holds **dispatch objects only** — an observation object for a published source, an [OCI image index][oci-image-index] for a derived one. A leaf platform manifest, the manifest that actually names a binary's layers, is never copied into the local index. The copy pins what a re-push can change — the tag→digest binding, the platform→digest map — and leaves everything below a digest to the [package store][in-depth-storage-packages], which fetches it on demand and is content-addressed anyway.

A tag's `content` digest in the root document disambiguates by its own absence:

- **Present in `o/`** — decode it, run [platform selection][reference-platforms-compatibility] over the per-platform digest list it carries, then fetch the resulting leaf digest from the physical registry.
- **Absent from `o/`** — it names a manifest directly, the shape a single-platform tag uses. OCX checks the [package store][in-depth-storage-packages] first — an already-installed tool's leaf manifest was cached there at install time, so re-resolving it needs no network at all — and only reaches the registry on a genuine cold miss.

A multi-platform package therefore holds exactly one dispatch object under `o/`; a single-platform one holds none. Compared to copying the whole manifest chain, that is roughly a sixth the size for a typical multi-platform package.

::: info An incomplete copy self-heals
Resolving a tag whose `content` is absent from `o/` fetches a manifest by digest from the registry — but the fetched bytes can turn out to decode as an image index instead: a multi-platform tag whose dispatch object simply had not been written to this particular copy yet. OCX writes it into `o/` and continues resolving from there, rather than failing. A partial copy heals a gap the moment it is asked to resolve it, with no separate repair step.
:::

### Crash-safe updates {#local-crash-safety}

[`ocx index update <pkg>`][cmd-index-update] writes three things per tag, in a fixed order, each its own atomic step: the dispatch object into `o/` (content-addressed — an orphan left by an aborted write is harmless, nothing points at it yet), the root document (tempfile plus rename), then the package's `c/index.json` catalog entry (tempfile plus rename). An interruption between any two steps — a crash, a kill signal — always recovers on the next read or update; there is no window where it leaves the copy looking corrupt.

The catalog entry is nothing more than the SHA-256 of the root document's own bytes — a cached derivation, not an independently trusted fact. A read that finds a stored root and its catalog entry disagree is therefore an **inconsistency**, not evidence of tampering: OCX recomputes the entry from the root actually on disk and rewrites the catalog to match, logged at info level. A hard error is reserved for corruption recomputation cannot fix — an unparseable root document, a dispatch object whose bytes disagree with its own `o/` filename, a failed `repository` cross-check.

A **later** catalog sync that finds the *remote* root digest has moved past the local one is a different case: that is staleness, reported as an update being available, never an error. Re-running [`ocx index update <pkg>`][cmd-index-update] re-snapshots it.

### Update modes {#update-modes}

[`ocx index update <package>`][cmd-index-update] syncs the local index for a specific package from its remote source:

- **Bare identifier** (e.g., `cmake`) — downloads every tag for the repository, each with its own dispatch object and root entry.
- **Tagged identifier** (e.g., `cmake:3.28`) — fetches only that single tag, merging it into the existing local subtree.

Tag-scoped mode is ideal for lockfile workflows where the local index should hold only explicitly requested tags. Packages not listed are not touched.

### Fresh-machine fallback {#fresh-machine}

On a fresh machine, [`ocx index update`][cmd-index-update] does not need to run before the first [`ocx package install cmake:3.28`][cmd-package-install]. When the local index has no entry for a requested tag, [`ocx package install`][cmd-package-install] transparently resolves that single tag against the configured remote, writes the dispatch object, root, and catalog entry, and proceeds with the install. Subsequent commands — including [`--offline`][arg-offline] — then work from the cached entry without touching the network.

Refreshing an already-cached tag or discovering every tag for a repository is still the job of [`ocx index update`][cmd-index-update]; the fallback only covers the specific tag being installed.

## Active Index {#active}

Every command that resolves a package identifier — [`ocx package install`][cmd-package-install], [`ocx package which`][cmd-which], [`ocx package exec`][cmd-exec], [`ocx index list`][cmd-index-list] — uses one working index for that invocation. By default, this is the local index. Two flags change which index is used:

| Mode | Flag | Source | Network? |
|---|---|---|---|
| Default | *(none)* | Local index | No (unless fetching a new binary) |
| Remote | [`--remote`][arg-remote] | OCI registry | Yes |
| Offline | [`--offline`][arg-offline] | Local index | Never |

**`--remote`** forces tag and catalog lookups to query the registry directly for a single command. The local index is **not** updated. Layer data fetched under `--remote` still writes through to the [package store][in-depth-storage-packages], so the command populates that cache while bypassing the index. Use it for a one-off check — seeing currently available tags, or resolving the latest digest — without committing the tag resolution result locally.

**`--offline`** prevents all network access for that command. If the local index does not have a requested package, the command fails immediately rather than attempting a registry query. Useful to verify that the current index and package store are self-sufficient before a build in a restricted or air-gapped environment.

[`--index`][arg-index] / [`OCX_INDEX`][env-ocx-index] do not change the active index *mode* — the local index remains active. They only change *which collection* is read. See [Shipped copies](#bundled).

The active index controls tag and manifest resolution only. The [package store][in-depth-storage-packages] is independent — installed binaries are accessible in all three modes regardless of which index is active.

## Shipped copies {#bundled}

A local index subtree is small — root documents and dispatch objects only, no layer archives, no binaries — small enough to ship *inside* a tool release. [Bazel Rules][bazel-rules], [GitHub Actions][github-actions-docs], and [DevContainer Features][devcontainer-features] can bundle a frozen copy at release time and set [`OCX_INDEX`][env-ocx-index] (or pass [`--index`][arg-index]) to point OCX at it. Consumers write `cmake:3` and the bundled copy resolves it deterministically — with zero network dependence and zero dependence on any other machine-global state — while the [package store][in-depth-storage-packages] and [install symlinks][in-depth-storage-symlinks] stay in `OCX_HOME` as usual.

This produces a two-level pin on *version choice*, not on `ocx.lock` — a devcontainer feature or a GitHub Action has no lock file of its own, so this index-level pin is the only determinism it gets: the tool version pins the bundled index copy, which pins the resolved binary. A version bump to the action or rule — proposed automatically by [Dependabot][dependabot] or [Renovate][renovate] — advances the bundled copy. Users get the updated binary with no config changes.

::: tip GitHub Action with a bundled index
```yaml
- uses: ocx-actions/setup-cmake@v2.1.0   # pins action → pins index → pins binary
  with:
    version: "3.28"                       # human-readable tag, no platform conditions
```

`@v2.1.0` pins everything end-to-end. `@v2` follows minor releases — as the maintainer ships updated index copies, `cmake:3.28` may resolve to a newer build when the action version changes. No SHA256 lists, no `if: runner.os == 'Linux'` conditionals.
:::

The contrast with maintaining a [hand-curated URL matrix][toolchains-llvm] — one `filename → checksum` entry per `version × os × arch` — is clear: a version bump means editing one rule version, not a dictionary.

::: warning Content still needs a network the first time — unless the layer cache is already warm
A shipped copy resolves the tag → platform-manifest digest offline, but the manifest bytes and layers themselves are content, fetched on demand from the registry the first time a given digest is installed. "First time" is scoped to the [layer store][in-depth-storage-layers] and the blob store, not to `packages/`: copying `blobs/`, `layers/`, and `index/` into a fresh `$OCX_HOME` — never `packages/` — is enough. A package's `packages/…/content/` directory is a hardlink assembly of its layers, built locally at install time; if every layer a package needs is already on disk, `ocx package install` reassembles `content/` from the local layer cache with no network at all, even though `packages/` itself started out empty. Only a genuinely cold layer needs egress.
:::

## index.ocx.sh {#public-index}

An OCI registry path is a physical detail — a hostname and repository path — but a package's *identity* is logical: "the `cmake` package published under `ocx.sh`". When a maintainer migrates the backing registry (say, from a self-hosted GHCR org to Docker Hub), every consumer that pinned the physical `ghcr.io/…` path directly breaks, even though the package itself did not change.

[`index.ocx.sh`][index-ocx-sh] is a pointer index, not a registry: it carries no `/v2` API and stores no blobs. It maps a stable logical identifier (`ocx.sh/<namespace>/<package>`) to the physical registry currently hosting it, plus the per-platform content digests observed there. OCX resolves `ocx.sh/kitware/cmake:3.28` by asking the index for the current physical location and digest, then fetching the actual manifest from that physical registry — the OCI image-index hop a direct registry resolve performs is skipped entirely, because the index already carries the per-platform digest list. The index HTTP client ships its own bundled CA root set, the same source the main OCI client uses, so root and observation-object fetches work on a minimal container with no system CA store installed.

For any `ocx.sh/<namespace>/<package>` identifier, the public index is consulted **before** the OCI registry — never the other way around — so a logical reference always resolves through the verified two-hop path below rather than a registry that happens to serve a repository under the same name. Identifiers on any other registry are unaffected; the index is never consulted for them. Until the index is generally available, an absent or unreachable `config.json` is an inert fall-through: OCX resolves straight against the registry, unchanged from today.

### Resolution pipeline {#public-index-pipeline}

```
logical id (ocx.sh/<ns>/<pkg>[:tag])
  → index resolve   : GET p/<ns>/<pkg>.json (root) → tags[tag].content (obs digest)
                      → GET p/<ns>/<pkg>/o/sha256/<obs-digest>.json (verify sha256 of bytes)
                      → platforms[]: select the platform matching this host
  → physical        : root.repository, e.g. "oci://ghcr.io/ocx-contrib/cmake"
  → mirror_map      : rewritten through [mirrors]'s registry role, if configured for this host
  → fetch           : GET physical-registry /v2/.../manifests/<leaf-digest> (verify OCI CAS)
```

`index.ocx.sh` yields **only pointers** — the platform manifest and its layers always come from the physical registry named by `repository`. The `repository` field carries an `oci://` scheme marker identifying it as a physical, transport-only reference; it is never used as a storage key. Locally, OCX keys everything — the local index path, `ocx.lock`, garbage-collection roots — on the *logical* identifier, so a registry migration never orphans a local copy or breaks a committed team lock.

::: warning A configured index that breaks fails loud, not silent
The "resolves straight against the registry" fall-through above applies only while `ocx.sh` is *not yet* configured as index-kind. Once a namespace names an index in [`[registries.<name>]`][config-registries] — the default once `index.ocx.sh` is generally available — that source is authoritative for it: a yanked tag, a tampered observation object, an unrecognized `config.json` version, or the endpoint being unreachable all surface as a hard error, never a silent drop to a registry that happens to serve a repository under the same name.
:::

### Two-hop fetch and caching {#public-index-caching}

| Object | Volatility | OCX behaviour |
|---|---|---|
| `p/<ns>/<pkg>.json` root | Volatile — the `repository` pointer can move by maintainer PR, tags are curated | Copy-first: never auto-refreshed under the default mode. A live re-fetch happens only on an explicit [`ocx index update`][cmd-index-update] or a [`--remote`][arg-remote] resolve, which rewrites the local copy and bumps `observed`. |
| `o/sha256/<hex>.json` observation object | Immutable | Fetched once, verified against its own SHA-256 filename, cached forever. |
| `c/index.json` catalog | Volatile | [`ocx index catalog`][cmd-index-catalog] against an `index.ocx.sh` source sends the ETag from a persisted `c/index.json.etag` sidecar as `If-None-Match`; an unchanged catalog answers `304` with no further work. On a real change, OCX re-snapshots only the packages whose root digest actually moved — one request amortized across the full catalog instead of one probe per package. |

::: warning Cached observation digests are not guaranteed stable across re-announces
An observation object's own digest can, in rare cases, change between two index re-announces of what is otherwise the same platform set, because of how the index canonicalizes platform ordering. OCX caches each observation object under the digest it was actually served with and never assumes a package's observation digest is a stable long-term identity — only the per-platform leaf digests it names are the durable pin.
:::

### Local layout for index.ocx.sh sources {#public-index-layout}

The local index uses the identical `p/<ns>/<pkg>.json` + `o/sha256/<hex>.json` shape for an `index.ocx.sh` source as for any other — it is [one wire format](#format), not a special case:

```
$OCX_HOME/index/ocx.sh/
├── config.json
├── c/index.json (+ .etag)
└── p/kitware/
    ├── cmake.json              root doc — copied verbatim from the hosted site
    └── cmake/o/sha256/
        └── <obs-digest>.json   verbatim observation object (immutable)
```

Unlike an OCI-registry source, there is no image-index digest to store here — the observation object's own `platforms[].digest` entries are already the per-platform manifest digests OCX needs, each independently verified against the physical registry's OCI content-addressed storage when actually fetched.

### Status surfacing {#public-index-status}

A package's root document carries publisher-set status fields, which OCX surfaces but never silently acts on:

| Field | Behavior |
|---|---|
| `yanked` (per tag) | A tag resolve against a yanked entry prints a warning and is refused by default — a yank is a publisher signal, not a delete. This surfaces identically whether the root was just fetched live or read from a [committed or shipped local copy](#bundled) with zero network: the same field, the same refusal. Set [`OCX_ALLOW_YANKED`][env-ocx-allow-yanked] to opt in and resolve it anyway. A digest-pinned resolve of the same content never needs the opt-in, since immutable content cannot itself be "yanked". |
| `deprecated` + message | Resolve prints a warning; the message is surfaced in [`ocx package info`][cmd-package-info]. |
| `superseded_by` | Shown as advisory information in [`ocx package info`][cmd-package-info] and resolve diagnostics; OCX never auto-follows it — that would silently substitute a different package than the one you asked for. |

::: info Open interop point — description assets
Observation objects share the same object store as `desc` blobs (README text, logo images) the index may carry for a package, but that part of the wire format is not yet frozen. [`ocx package info --save-readme`][cmd-package-info] / `--save-logo` stay driven by registry-side metadata for now, independent of `index.ocx.sh`.
:::

## Route index traffic through a mirror {#mirroring}

[Corporate mirrors][config-mirrors] redirect OCX's traffic host-by-host — and cover two different roles a host might serve. `index.ocx.sh`'s root, observation-object, and catalog fetches are plain HTTPS, not OCI distribution calls, so a mirror entry has to say which kind of traffic it is redirecting.

The [`[mirrors]`][config-mirrors] table value is either a plain string, which redirects both roles for a host, or an object that splits them:

```toml
[mirrors]
"index.ocx.sh" = { index = "https://artifactory.corp/ocx-index" }   # index role only
"ghcr.io" = "https://company.jfrog.io/ghcr-remote"                  # both roles → registry-only host
```

Same doctrine as the registry role: the value replaces the base URL wholesale, there is no fallback to the public origin, and the table merges through the [managed-config][config-managed] tier the same way, per role. Every root, observation-object, and catalog fetch still verifies its content against the recorded SHA-256 digest — the mirror changes only where the bytes come from, never whether they are trusted.

See [`[registries.<name>]`][config-registries] for the separate question of *which* namespaces resolve through the ocx-index protocol at all — a mirror only redirects an already-selected protocol's traffic, it does not select the protocol.

## Canonical tags {#canonical-tags}

A registry tag can be deleted by mistake, even when a digest it once pointed at is still pinned by someone's `ocx.lock`. Registry-side retention policies key on tags, not on which digests are "in use" somewhere — so a stray delete can leave a manifest orphaned and eligible for garbage collection on the registry side.

[`ocx package push`'s `--canonical-tag`/`--no-canonical-tag`][cmd-package-push] closes this gap on the registry side, on by default: for every platform manifest pushed **in that invocation**, OCX also pushes a digest-named `sha256.<hex>` tag alongside the version tag. As long as that canonical tag exists, the registry sees the manifest as referenced, regardless of what happens to the human-readable tags around it. There is no retroactive tagging — a push does not reach back and canonical-tag manifests published by earlier pushes.

Because the local index never copies a leaf platform manifest — only its [dispatch object](#local-dispatch) — an index-resolved install always fetches the leaf from the registry on demand, sometimes long after the index entry was written. Canonical tags are exactly the registry-side safety net that keeps that leaf tag-reachable in the meantime.

Canonical tags are a pure registry-side safety net scoped to `ocx package push` — [`ocx config push`][cmd-config-push] has no `--canonical-tag` flag, and [`index.ocx.sh`](#public-index) ignores canonical tags entirely; they carry no wire semantics on the index side.

## The `update` family {#update-family}

Three OCX commands share the `update` verb. Each refreshes exactly one record, and confusing them means refreshing the wrong one.

| Command | Refreshes |
|---|---|
| [`ocx index update`][cmd-index-update] | The local index at [`--index` ▸ `OCX_INDEX` ▸ `$OCX_HOME/index/`][arg-index] |
| [`ocx self update`][cmd-self-update] | The managed ocx install itself |
| [`ocx update`][cmd-update] | A project's `ocx.lock` |

`ocx update` never writes to the local index — `ocx.lock` is its only canonical record. Re-resolving a project's pinned tools does not change what `cmake:3` resolves to for any other command on the same machine; that stays [`ocx index update`][cmd-index-update]'s job.

## See Also

- [Indices section in the user guide][user-indices] — how-to: refresh, work offline, use `--remote`
- [Storage][in-depth-storage] — the local index's home relative to the other stores under `$OCX_HOME`
- [Versioning][in-depth-versioning] — tag mutability, locking by digest, `_` build suffix
- [Configuration][in-depth-configuration] — `[registries]` and `[mirrors]` config-driven defaults

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

<!-- security -->
[cwe-345]: https://cwe.mitre.org/data/definitions/345.html

<!-- commands -->
[cmd-package-install]: ../reference/command-line.md#package-install
[cmd-which]: ../reference/command-line.md#which
[cmd-exec]: ../reference/command-line.md#package-exec
[cmd-index-update]: ../reference/command-line.md#index-update
[cmd-index-catalog]: ../reference/command-line.md#index-catalog
[cmd-index-list]: ../reference/command-line.md#index-list
[cmd-package-push]: ../reference/command-line.md#package-push
[cmd-config-push]: ../reference/command-line.md#config-push
[cmd-package-info]: ../reference/command-line.md#package-info
[cmd-self-update]: ../reference/command-line.md#self-update
[cmd-update]: ../reference/command-line.md#update
[cmd-direnv-export]: ../reference/command-line.md#direnv-export
[arg-remote]: ../reference/command-line.md#arg-remote
[arg-offline]: ../reference/command-line.md#arg-offline
[arg-index]: ../reference/command-line.md#arg-index

<!-- environment -->
[env-ocx-home]: ../reference/environment.md#ocx-home
[env-ocx-index]: ../reference/environment.md#ocx-index
[env-ocx-allow-yanked]: ../reference/environment.md#ocx-allow-yanked

<!-- reference -->
[config-mirrors]: ../reference/configuration.md#keys-mirrors
[config-registries]: ../reference/configuration.md#keys-registries
[config-managed]: ../reference/configuration.md#keys-managed
[reference-platforms-compatibility]: ../reference/platforms.md#compatibility

<!-- internal -->
[user-indices]: ../user-guide.md#offline
[in-depth-storage]: ./storage.md
[in-depth-storage-packages]: ./storage.md#packages
[in-depth-storage-layers]: ./storage.md#layers
[in-depth-storage-symlinks]: ./storage.md#symlinks
[in-depth-versioning]: ./versioning.md
[in-depth-versioning-locking]: ./versioning.md#locking
[in-depth-configuration]: ./configuration.md
