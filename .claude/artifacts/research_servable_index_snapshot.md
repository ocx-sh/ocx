# Research: Serving an index snapshot — OCI ecosystem and CLI conventions

## Metadata

**Date:** 2026-08-09
**Domain:** packaging, devops, cli
**Triggered by:** making an OCX local index snapshot directly servable (write
`config.json`, derive `c/index.json`, add a `file://` index source), and naming the
two new verbs.
**Expires:** 2027-08-09

## Direct Answer

Two independent axes, both answered.

**Ecosystem (OCI):** the ecosystem has *not* converged on "a directory that answers
the OCI Distribution protocol." It has converged on exactly what OCX already does —
a static, crates.io-style sparse index decoupled from the Distribution API, served
by any static host. A [Feb 2026 supply-chain analysis](https://nesbitt.io/2026/02/18/what-package-registries-could-borrow-from-oci.html)
argues OCI's real gap is that registries have *no equivalent to a package index
API*, and that the fix is a light metadata index in front of OCI-backed bulk
storage — direct, recent validation of the split OCX already has.

**CLI conventions:** no core package manager keeps a whole-catalog sync verb in the
everyday tool; it is always a separate, operator-facing role, and the universal
name for it is **`mirror`**. The metadata-derivation step is universally called
**`generate`** (`apt-ftparchive generate`, `createrepo`); "regenerate" has zero
precedent in any tool surveyed.

## Technology Landscape

### Established (proven, widely accepted)

| Tool/Pattern | Status | Notes |
|---|---|---|
| `oci-layout` (`blobs/` + `index.json` + `oci-layout`) | Standard | The one standardized directory-as-registry-*content* format, explicitly transport-agnostic. Note: it has **no** URL routing, so "directory of oci-layout" ≠ "directory that answers Distribution API calls" |
| Client-side local-layout flags | Standard | `oras --oci-layout`, skopeo `oci:`, regclient "OCI layouts as local-disk equivalent of a repository". Every tool invented its **own** local-path flag rather than waiting for a spec |
| Air-gap bulk copy | Standard | `oras cp` / `oras backup`, `skopeo sync` (registry↔dir↔registry, YAML-filtered), crane/gcrane, zot sync extension, Harbor replication. All copy *content*; none add a query/index API |
| Pool + generate-metadata split | Standard | `apt-ftparchive generate`, `createrepo` — "make a raw content pool servable" |
| `file://` as a first-class index source | Standard | Go `GOPROXY=file://` (protocol is pure static-file GETs), apt `deb file:///`, Maven `file://`, Nix local binary caches, cargo `directory`/`local-registry` |
| Lockfile-scoped fetch decoupled from install | Trending | `pnpm fetch` → `install --offline`; the shape modern tools converge on for CI/Docker layering |

### Declining / never matured

| Tool/Pattern | Signal | Avoid because |
|---|---|---|
| "Static registry over S3/CDN" (precompute Distribution-API paths as static files) | Self-described experimental, unmoved in 2 years | Solves the wrong layer, no catalog/auth/push. Cited as evidence the pattern stalled, **not** as a recommendation |
| `_catalog` as an enumeration API | Docker Hub refuses it; GHCR requires auth with undocumented limits; no registry supports filtered/subset queries | Never converged. Confirms the index must be the enumerator, never the registry |
| Non-atomic full-mirror tools | `apt-mirror` leaves broken half-synced state ("hash sum mismatch"); superseded by `apt-mirror2`, which guarantees never leaving a broken mirror on exit 0 | Directly validates the ordering/atomicity invariant |

## Design Patterns Worth Considering

- **Index as the enumerator, registry as bulk storage** — validated independently
  and recently. [Nesbitt, Feb 2026](https://nesbitt.io/2026/02/18/what-package-registries-could-borrow-from-oci.html)
- **`mirror` as a distinct operator verb, not a flag on the everyday fetch** —
  apt-mirror, debmirror, panamax, `cargo-fetcher mirror`, Nexus proxy repos. Cargo
  deliberately has no whole-index sync verb at all; it pushes that role out to
  third-party tools. [panamax](https://github.com/panamax-rs/panamax)
- **`generate` for deriving servable metadata from a pool** —
  [apt-ftparchive](https://manpages.debian.org/testing/apt-utils/apt-ftparchive.1.en.html),
  [createrepo_c](https://www.systutorials.com/docs/linux/man/8-createrepo_c/)
- **Strict-subset rule for a local replacement source** — cargo requires a
  `directory`/`local-registry` source to be a strict subset of what it replaces; it
  cannot add crates absent upstream. A good integrity rule for a curated mirror.
  [source-replacement](https://doc.rust-lang.org/cargo/reference/source-replacement.html)

## Key Findings

1. **Cargo has no `sparse+file://`.** The sparse index protocol is documented
   https/http only; `file://` in cargo covers the crate-*download* endpoint, not the
   index. OCX would be establishing its own convention rather than copying one.
   [registry-index](https://doc.rust-lang.org/cargo/reference/registry-index.html)
2. **Go's `GOPROXY=file://` is the closest 1:1 precedent** — the module-proxy
   protocol is pure static-file GETs, so a local directory is a first-class proxy
   source with no special-casing. **Caveat:** Go explicitly carves out an exception
   bypassing GOSUMDB checksum verification for `file://` proxies — a local source
   traded integrity for convenience.
   [sumdb.go](https://tip.golang.org/src/cmd/go/internal/modfetch/sumdb.go?m=text)
3. **pip `--find-links` is the cautionary tale**: without `--no-index`, pip merges
   local and PyPI by "best version wins" — a live dependency-confusion class. Never
   silently blend a local source with a remote one.
   [writeup](https://dev.to/brabster/how-to-get-pwned-with-extra-index-url-462g)
4. **cargo's `.cargo-checksum.json` is documented as explicitly *not* a security
   mechanism** — it guards accidental modification only. A checksum manifest is not
   a substitute for signing.
5. **`sync` is an actively dangerous name here** — asdf/mise trained users to expect
   a cheap periodic metadata refresh; `update` is the apt/apk convention for
   "refresh metadata only, don't fetch content". Either name undersells what a
   full-catalog pull costs.
6. **OCI `artifactType` is a content-type discriminator, not a version gate** —
   closer to a MIME type. Do not reuse it for "client too old" signalling.
7. **Nix has no bulk-copy verb in core**, and an issue asking for one has sat open
   for years — signal that unbounded full-cache-copy is not wanted in an everyday
   tool. [nix#3336](https://github.com/NixOS/nix/issues/3336)

## Recommendation

1. **Do not build a Distribution-API-over-`file://` transport.** That road stayed
   experimental and solves the wrong layer. Serve the same static tree from any file
   host and add a local-path branch to the *index client*. In OCX that branch is
   precisely an `IndexTransport` impl — the trait is the client's read seam
   (`get(url) -> Found{bytes} | NotFound`), not a Distribution-API emulation.
2. **Name the bulk verb `mirror`, not a flag on `update`.** Every ecosystem
   surveyed treats full-catalog replication as a distinct, heavier,
   operator-triggered operation. As a side effect this leaves `ocx index update`'s
   "only what you name" contract untouched, which is independently valuable.
3. **Name the promote step `generate`, not `regenerate`.**
4. **Ship `file://` as first-class**, with two non-negotiables drawn from the
   failure histories: never blend a local source with a remote one by best-version-
   wins, and do not present a checksum manifest as an integrity boundary.
5. **Atomicity is table stakes** — `apt-mirror2`'s guarantee ("never leaves a broken
   mirror if it exits 0") is the bar; write content before the pointer that names it.

## Sources

| Source | Type | Date | Relevance |
|---|---|---|---|
| [Nesbitt, "What package registries could borrow from OCI"](https://nesbitt.io/2026/02/18/what-package-registries-could-borrow-from-oci.html) | Blog | 2026-02 | Index/storage split validation; "no package-index-API equivalent" gap |
| [image-spec image-layout](https://github.com/opencontainers/image-spec/blob/main/image-layout.md) | Spec | current | `oci-layout` structure, transport-agnostic framing |
| [distribution-spec](https://github.com/opencontainers/distribution-spec/blob/main/spec.md) | Spec | current | Referrers API; no enumeration standard |
| [ORAS OCI layouts](https://oras.land/docs/how_to_guides/distributing_oci_layouts/) · [backup/restore](https://oras.land/docs/how_to_guides/backup-restore/) | Docs | current | `--oci-layout` as local source; tarball air-gap transport |
| [skopeo-sync](https://man.archlinux.org/man/extra/skopeo/skopeo-sync.1.en) | Man | current | registry↔dir↔registry sync semantics |
| [zot mirroring](https://zotregistry.dev/v1.4.3/articles/mirroring/) · [Harbor replication](https://goharbor.io/docs/1.10/administration/configuring-replication/create-replication-rules/) | Docs | current / 2020 | Sync extensions, filter-based selective replication |
| [regclient](https://github.com/regclient/regclient/blob/main/README.md) | Repo | current | regsync filtering; OCI layouts as local-disk repository |
| [cargo registry-index](https://doc.rust-lang.org/cargo/reference/registry-index.html) | Docs | current | No `sparse+file://` precedent |
| [cargo source-replacement](https://doc.rust-lang.org/cargo/reference/source-replacement.html) | Docs | current | Strict-subset rule; checksum-is-not-security |
| [Go private module proxy](https://www.gofaq.org/en/how-to-set-up-a-private-go-module-proxy/) · [sumdb.go](https://tip.golang.org/src/cmd/go/internal/modfetch/sumdb.go?m=text) | Docs / source | current | `file://` as first-class proxy; GOSUMDB bypass caveat |
| [panamax](https://github.com/panamax-rs/panamax) | Repo | current | Full crates.io mirror as a separate tool |
| [apt-ftparchive](https://manpages.debian.org/testing/apt-utils/apt-ftparchive.1.en.html) · [createrepo_c](https://www.systutorials.com/docs/linux/man/8-createrepo_c/) | Man | current | `generate` verb precedent |
| [apt-mirror#102](https://github.com/apt-mirror/apt-mirror/issues/102) · [apt-mirror2](https://hub.docker.com/r/aptmirror/apt-mirror2) | Repo | current | Non-atomic sync failure mode; atomic rewrite guarantee |
| [pip dependency-confusion writeup](https://dev.to/brabster/how-to-get-pwned-with-extra-index-url-462g) | Blog | current | `--find-links` shadowing |
| [nix#3336](https://github.com/NixOS/nix/issues/3336) · [Nix binary cache](https://nixos.wiki/wiki/Binary_Cache) | Repo / Wiki | current | No bulk-copy verb; local cache = same protocol, signature is the valve |
| [Ochagavía, S3 as a container registry](https://ochagavia.nl/blog/using-s3-as-a-container-registry/) | Blog | 2024-07 | **Flagged >18mo, self-described experimental** — cited as evidence the pattern stalled |
