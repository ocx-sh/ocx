---
title: "index.ocx.sh"
description: "How OCX resolves a logical package identifier through the public index, why a configured index owns its whole registry, and how yanked and deprecated tags surface."
---
<!-- doc_type: explanation -->

This page explains how OCX resolves packages that are published through the [`index.ocx.sh`][index-ocx-sh] public index. It matters because the index, not the physical registry, decides which tags exist.

## The public index {#public-index}

An OCI registry path is a physical detail: a hostname and a repository path. The *identity* of a package is logical, for example "the `cmake` package published under `ocx.sh`". A maintainer may migrate the backing registry, say from a self-hosted GHCR org to Docker Hub. Every consumer that pinned the physical `ghcr.io/…` path directly then breaks, although the package itself did not change.

[`index.ocx.sh`][index-ocx-sh] is a pointer index, not a registry. It carries no `/v2` API and stores no blobs. It maps a stable logical identifier (`ocx.sh/<namespace>/<package>`) to the physical registry that hosts the package. It also records the per-platform content digests held there.

OCX resolves `ocx.sh/kitware/cmake:3.28` in two steps. It asks the index for the current physical location and digest. It then fetches the actual manifest from that physical registry.

The OCI image-index hop of a direct registry resolve is served by the index instead of the registry. It is not skipped. The hop count is the same, and only the side that answers it changes.

The HTTP client of the index ships its own bundled CA root set, the same source the main OCI client uses. Root and index-object fetches therefore work on a minimal container with no system CA store.

For any `ocx.sh/<namespace>/<package>` identifier, the public index is consulted **before** the OCI registry. It is never the other way around. A logical reference always resolves through the verified two-hop path below. It never resolves through a registry that happens to serve a repository under the same name. Identifiers on any other registry are unaffected, and the index is never consulted for them.

This wiring ships in the binary. `ocx.sh` names `https://index.ocx.sh` in the [compiled-defaults tier][config-precedence]. Any config tier can point it elsewhere. Setting [`index = ""`][config-registries-index] resolves `ocx.sh` as a plain OCI registry instead.

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

`index.ocx.sh` yields **only pointers**. The platform manifest and its layers always come from the physical registry named by `repository`. The `repository` field carries an `oci://` scheme marker. The marker identifies a physical, transport-only reference. It is never used as a storage key.

Locally, OCX keys everything on the *logical* identifier. That covers the local index path, `ocx.lock` and the garbage-collection roots. A registry migration therefore never orphans a local copy or breaks a committed team lock.

### A configured index owns its whole registry {#public-index-declared-names}

`config.json` carries the wire-format version, and nothing the client reads decides scope:

```json
{ "format_version": 1 }
```

A namespace may name an index in [`[registries.<name>]`][config-registries]. Then **only what that index holds is discoverable through it**. A reference for which the index has no root is a hard miss. OCX does not fall back to the plain OCI registry that the index points at. That holds even when the registry serves a repository under the same name.

The refusal names the index that was consulted. A package that has not been announced yet therefore reads as exactly that:

```
'ocx.sh/go-task:3.44' is not in the index at https://index.ocx.sh, which is authoritative
for every name in registry 'ocx.sh'; announce it there with `ocx package announce`, or take
the namespace off the index with `[registries."ocx.sh"] index = ""`
```

The scope is the registry, not a name shape. The root schema of `index.ocx.sh` pins a logical name to `ocx.sh/<namespace>/<package>`. A flat `ocx.sh/<name>` can therefore never hold a root there.

That is a constraint of the index operator, enforced when a package is announced. It is not a rule the client applies. An index that serves a root for a flat name resolves it normally.

This is what makes the yank and deprecation gate reachable at all. A tag that the index yanks cannot be obtained by asking the registry underneath instead. It also means **completeness matters**. A package missing from the index is unresolvable through it, by design.

An absent `config.json` is the same case, not a different one. It is read as `{"format_version": 1}` at every reader (see [Serve an index][serve-an-index-defect]). The `name_segments` field plays no part either way, whether present or absent. It never decides whether a name is in the jurisdiction of the index.

:::caution[An index that breaks fails loud, not silent]
The same authority makes every failure terminal. A yanked tag, a tampered index object, an unrecognized `config.json` version and an unreachable endpoint all surface as a hard error. None of them drops silently to a registry that happens to serve a repository under the same name.

An `index.ocx.sh` outage therefore blocks `ocx.sh/…` resolution. It does not quietly resolve it somewhere else. An outage is never reported as an absent package, and the error names the endpoint that failed. Namespaces on other registries are untouched.

Two things opt `ocx.sh` out. One is [`index = ""`][config-registries-index]. The other is pinning the namespace at a [`[mirrors]` registry endpoint][corporate-mirror]. That pin suppresses the compiled-in default, so a site that already routes `ocx.sh` elsewhere never gains a host it did not allow-list.

A mirror keyed on the *index* host is the opposite move. It redirects the index endpoint rather than replacing it. The verified two-hop path stays, served by that host.

Only a config file that the operator controls suppresses the default. Neither the [managed tier][config-managed] nor a forwarded `OCX_MIRRORS` can revoke the verified path. Both still redirect traffic.
:::

### Two-hop fetch and caching {#public-index-caching}

| Object | Volatility | OCX behaviour |
|---|---|---|
| `p/<ns>/<pkg>.json` root | Volatile. The `repository` pointer can move by maintainer PR and tags are curated. | Copy-first. Never auto-refreshed under the default mode. A live re-fetch happens only on an explicit [`ocx index update`][cmd-index-update] or a [`--remote`][arg-remote] resolve. That re-fetch rewrites the local copy and bumps `observed`. |
| `o/<algo>/<hex>.json`, the OCI image index this tag resolved to, verbatim | Immutable | Fetched once, verified against its own SHA-256 filename and cached forever. |
| `c/index.json` catalog | Volatile | **Never copied.** The local `c/index.json` is authored. Each entry is the hash of the local root document beside it, written in the same step that writes that root. The catalog of the site is fetched live only when you ask for it (`ocx index catalog --remote`), and nothing from it is stored. The local tree stays free of per-machine bookkeeping, so there is no record of remote state in it to go stale. |

A registry alias can drift between announces. The registry-side digest then moves past what the index still has committed. The first row above is built to tolerate this case.

[`ocx package cascade check`][cmd-package-cascade-check] flags that drift from the publisher's side. It compares the live registry tags of a package against its committed root and reports the mismatch as index staleness. [`ocx package cascade repair`][cmd-package-cascade-repair] fixes a drifted registry alias itself. The follow-up [`ocx package announce --tags-file`][cmd-package-announce] re-observes the repaired tags. It then re-publishes the moved digests into the index.

Neither command touches the local copy of any machine. That hop is still [`ocx index update`][cmd-index-update], run whenever a particular machine wants the newly announced root.

### Local layout for index.ocx.sh sources {#public-index-layout}

The local index uses the identical `p/<ns>/<pkg>.json` and `o/<algo>/<hex>.json` shape for an `index.ocx.sh` source as for any other. It is [one wire format][indices-format], not a special case:

```
$OCX_HOME/index/ocx.sh/
├── config.json
├── c/index.json
└── p/kitware/
    ├── cmake.json              root doc, assembled locally from snapshotted tags, not a verbatim copy
    └── cmake/o/sha256/
        └── <index-digest>.json   OCI image index, verbatim (immutable)
```

The stored object *is* the OCI image index of the registry. Its `manifests[].digest` entries are the per-platform manifest digests that OCX needs. Each one is verified independently against the OCI content-addressed storage of the physical registry when it is fetched.

The index repository specifies the file format of the index: [wire format][index-wire-format].

### Status surfacing {#public-index-status}

The root document of a package carries status fields set by the publisher. OCX surfaces them but never silently acts on them:

| Field | Behavior |
|---|---|
| `yanked` (per tag) | A tag resolve against a yanked entry prints a warning and is refused by default. A yank is a publisher signal, not a delete. It surfaces identically whether the root was just fetched live or read from a [committed or shipped local copy][indices-bundled] with zero network. It is the same field and the same refusal. Set [`OCX_ALLOW_YANKED`][env-ocx-allow-yanked] to opt in and resolve it anyway. A digest-pinned resolve of the same content never needs the opt-in, because immutable content cannot itself be "yanked". |
| `deprecated` + message | Resolve prints a warning. The message is surfaced in [`ocx package description pull`][cmd-package-info]. |
| `superseded_by` | Shown as advisory information in [`ocx package description pull`][cmd-package-info] and in resolve diagnostics. OCX never auto-follows it, because that would silently substitute a different package than the one you asked for. |

:::note[Open interop point: description assets]
Index objects share one object store with the `desc` blobs (README text and logo images) that the index may carry for a package. That part of the wire format is not frozen yet. [`ocx package description pull --save-readme`][cmd-package-info] and `--save-logo` stay driven by registry-side metadata. They are independent of `index.ocx.sh`.
:::

## See also {#see-also}

- [Indices][indices]: the shared wire format, the active index and shipped copies.
- [Local index][local-index]: the layout of the local collection and how updates grow it.
- [Serve an index][serve-an-index]: serve a snapshot over HTTPS or from disk.
- [Corporate mirror][corporate-mirror]: route index traffic through a mirror.

<!-- external -->
[index-ocx-sh]: https://index.ocx.sh
[index-wire-format]: https://index.ocx.sh/docs/reference/wire-format

<!-- commands -->
[cmd-index-update]: ../reference/command-line/index-commands.md#index-update
[cmd-package-announce]: ../reference/command-line/package-publishing.md#package-announce
[cmd-package-cascade-check]: ../reference/command-line/package-publishing.md#package-cascade-check
[cmd-package-cascade-repair]: ../reference/command-line/package-publishing.md#package-cascade-repair
[cmd-package-info]: ../reference/command-line/package-publishing.md#package-description-pull
[arg-remote]: ../reference/command-line/index.md#arg-remote

<!-- environment -->
[env-ocx-allow-yanked]: ../reference/environment.md#ocx-allow-yanked

<!-- reference -->
[config-registries]: ../reference/configuration.md#keys-registries
[config-registries-index]: ../reference/configuration.md#keys-registries-index
[config-managed]: ../reference/configuration.md#keys-managed
[config-precedence]: configuration.mdx#precedence

<!-- internal -->
[indices]: ./indices.md
[indices-format]: ./indices.md#format
[indices-bundled]: ./indices.md#bundled
[local-index]: ./local-index.md
[serve-an-index]: ../guides/serve-an-index.md
[serve-an-index-defect]: ../guides/serve-an-index.md#servable-defect
[corporate-mirror]: ../guides/corporate-mirror.mdx
