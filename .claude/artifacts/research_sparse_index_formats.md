# Research: Sparse HTTP Package-Index Formats (for `index.ocx.rs`)

**Date:** 2026-07-12
**Scope:** Entry schema + layout precedents for OCX's static name-addressed index (`p/<namespace>/<package>.json` on Cloudflare Pages, source-of-truth in `ocx-rs/index` git repo, per `adr_public_index_registry_indirection.md` D1–D3, D12–D13).

## Direct Answer

Four precedent systems examined:

1. **crates.io sparse index** — `config.json` root (`dl`, `api`, `auth-required`), path sharding by name length (`1/`, `2/`, `3/<c>/`, `<cc>/<cc>/`), line-delimited JSON per version (`name`, `vers`, `deps[]`, `cksum`, `features`, `features2`, `yanked`, `links`, `rust_version`, `v`, `pubtime` — only `yanked` is mutable), ETag/`If-Modified-Since` conditional GET.
2. **Bazel Central Registry** — `metadata.json` (versions, yanked_versions, maintainers, homepage, repository allowlist) is the pointer/governance layer; `source.json` (url, integrity, strip_prefix, patches) is the fetch-recipe payload; `MODULE.bazel` is fetched lazily only when a version is actually resolved.
3. **Homebrew `formulae.brew.sh`** — one JSON file per formula/cask (`/api/formula/<name>.json`) *and* a monolithic `formula.jws.json`/`formula.json` full-catalog blob; the monolithic file is now an actively-complained-about scaling liability (31 MB, all-or-nothing, GitHub issue open since Feb 2026 requesting a batch endpoint) — direct validation of OCX ADR's D2 decision to defer `all.json` and never make it authoritative for resolution.
4. **winget-pkgs** — the git repo (`microsoft/winget-pkgs`) is *only* the human-facing YAML source; the actual client never touches it. The client resolves against a **prebuilt SQLite index (`index.db`) bundled inside a signed MSIX (`source.msix`/`index.msix`)**, rebuilt server-side from the repo and served from a CDN (`cdn.winget.microsoft.com`) — i.e., winget already does the "git repo → rendered artifact → CDN" split OCX is designing, just with SQLite instead of flat JSON.

## Industry Context & Trends

- **Trending**: "Git-as-database is dead, sparse HTTP + CDN is the convergent design." Every registry that scaled past ~10⁴–10⁵ entries independently arrived at the same shape: small immutable per-name files, name-addressed HTTP, CDN-cached, conditional GET. Cargo (RFC 2789, sparse now 99%+ of requests as of April 2025), Homebrew (JSON API since 4.0.0), CocoaPods (CDN podspecs), Go modules (module proxy, 18min→12s), vcpkg and Nixpkgs all hit the same git-repo-as-database wall and converged on the same fix. This is a fully validated, boring-tech pattern — not a bet.
- **Trending**: **OCI as generic package-registry backend**, independent of OCX. Andrew Nesbitt (ecosyste.ms/Libraries.io) published "What Package Registries Could Borrow from OCI" (Feb 2026) arguing rubygems.org/PyPI-style registries should adopt OCI content-addressing, the Referrers API for signing/attestation, and OCI-native mirroring (Harbor/Zot) as a backend — without changing client protocols. External validation of OCX's founding thesis from a well-known package-ecosystem researcher; worth citing in `product-context.md` positioning.
- **Established**: pointer/payload split (BCR `metadata.json`/`source.json`; OCX D3) and doorbell/announce-not-data (Go proxy; OCX D4) are both proven, multi-year patterns, not novel.
- **Established**: conditional GET (ETag preferred over `Last-Modified` when both present) as the entire "freshness protocol" — no custom polling/versioning layer needed. Cargo's own spec explicitly recommends CDN cache-invalidation on index-file update, exactly OCX D13.
- **Emerging**: OCI Referrers API (Distribution Spec v1.1, GA since March 2024, now supported by Quay.io, ACR, ghcr.io partially) as the unified signing/SBOM/provenance-attachment mechanism — directly the substrate for OCX's own **Deferred** signing/provenance ADR. Scope that future ADR against Referrers + Sigstore/cosign rather than inventing OCX-specific signing.
- **Emerging**: JSR (jsr.io, Deno's registry, 2024-era greenfield design) separates immutable version metadata (cacheable forever) from package-level mutable status (yanked lives at the package level, not baked into each version) — the same shape OCX D3 already lands on by having exactly one small mutable file per package.
- **Declining**: monolithic full-catalog JSON blobs as the *primary* client-facing API. Homebrew's `formula.jws.json` (31 MB, all-or-nothing) is presently the subject of an open Homebrew/brew issue (#21622, Feb 2026) proposing a batch/filtered endpoint precisely because the monolith doesn't scale to CI/metered/Docker use — happening in real time to a peer system. Reinforces OCX's D2 decision to keep `all.json` **out of the resolution path** even after it exists for search.
- **Declining**: npm's "corgi" abbreviated-packument content-negotiation trick (`Accept: application/vnd.npm.install-v1+json`, 410 kB → 21 kB) is clever but depends on dynamic content negotiation server-side (`Vary: Accept`), which doesn't map cleanly onto a static-file CDN host like Cloudflare Pages. Not recommended for OCX — static, separately-named files are simpler and boring-tech-compatible.

## Key Findings

### 1. crates.io sparse index protocol

- `config.json` at index root: `dl` (download URL template, markers `{crate}` `{version}` `{prefix}` `{lowerprefix}` `{sha256-checksum}`), `api` (optional, base URL for publish/other API calls), `auth-required` (bool, gates `Authorization` header on all index/API/download requests). Real-world root file is minimal: `{"dl": "https://static.crates.io/crates", "api": "https://crates.io"}`. [config.json](https://github.com/rust-lang/crates.io-index/blob/master/config.json)
- Path sharding (name-length-driven, case-normalized to lowercase in the path, original case preserved in JSON content):
  - 1-char name → `1/<name>`
  - 2-char name → `2/<name>`
  - 3-char name → `3/<first-char>/<name>`
  - 4+-char name → `<first-two>/<next-two>/<name>` (e.g. `cargo` → `ca/rg/cargo`)
- Per-version line-delimited JSON fields: `name`, `vers`, `deps[]` (`name`, `req`, `features`, `optional`, `default_features`, `target`, `kind`, `registry`, `package`), `cksum` (sha256 of the artifact — content pinning **at the index layer**, something OCX's D3 explicitly forgoes), `features`, `features2` (namespaced/weak features, requires `v: 2`), `links`, `rust_version`, `v` (schema version, unknown values ignored by Cargo ≥1.51), `pubtime`. **Only `yanked` is ever mutated post-publish** — every other field is append-only/immutable.
- Yank = flip `yanked: true` in place; entry stays in the file forever (never deleted), excluded from resolution.
- Caching: conditional GET via `ETag`/`If-None-Match` (preferred) or `Last-Modified`/`If-Modified-Since`; registries running behind a CDN are told explicitly to invalidate the CDN cache on index-file update, not just rely on client TTLs.
[Cargo Registry Index reference](https://doc.rust-lang.org/cargo/reference/registry-index.html), [rust-lang/rfcs 2789](https://rust-lang.github.io/rfcs/2789-sparse-index.html)

### 2. Bazel Central Registry

- `bazel_registry.json` (registry root, optional): `mirrors[]`, `module_base_path`.
- `modules/<name>/metadata.json`: `versions[]`, `yanked_versions{version: reason}`, `maintainers[]` (github username **and** `github_user_id` — numeric ID tracked precisely because usernames can be renamed/transferred), `homepage`, `repository` (allowlist that `source.json`'s URL must match — anti-squat guard), `deprecated`.
- `modules/<name>/<version>/source.json`: fetch recipe — `url`/`mirror_urls`, `integrity` (SRI checksum, i.e. **digest pinned at the pointer layer**), `strip_prefix`, `patches{filename: integrity}`, `patch_strip`, `archive_type`.
- `MODULE.bazel` per version is the actual heavyweight payload — fetched lazily, only for versions actually resolved into a build; registries never cloned wholesale.
- Governance precedent already captured in the ADR (D5): pointer/`metadata.json` changes always human-reviewed; version bumps auto-mergeable.
[bazel.build/external/registry](https://bazel.build/external/registry), [metadata.schema.json](https://github.com/bazelbuild/bazel-central-registry/blob/main/metadata.schema.json)

### 3. Homebrew `formulae.brew.sh` / winget-pkgs

- Homebrew endpoints: `/api/formula.json` + `/api/cask.json` (full catalog blobs), `/api/formula/<name>.json` + `/api/cask/<token>.json` (per-package, includes analytics + generation timestamp), `/api/analytics/<category>/<days>.json` (30d/90d/365d installs). [docs/api.md](https://github.com/Homebrew/formulae.brew.sh/blob/main/docs/api.md)
- The monolithic `formula.jws.json` is presently a live scaling pain point — 31 MB, all-or-nothing, no filtering, open issue since Feb 2026 requesting a batch endpoint. [Homebrew/brew#21622](https://github.com/homebrew/brew/issues/21622)
- `homebrew-core`'s **source** repo had to be sharded into `Formula/<letter>/<name>.rb` directories after GitHub itself flagged the flat layout as an expensive-operations problem (331 MB just to unshallow, ~1 GB `.git`). [nesbitt.io retrospective](https://nesbitt.io/2025/12/24/package-managers-keep-using-git-as-a-database.html) The **served** JSON API stayed flat, one file per package — sharding was a git/source-repo concern, not a CDN-serving concern. Maps directly onto OCX: shard `ocx-rs/index`'s git tree for repo hygiene reasons, independent of whether the deployed static files need sharding at all.
- winget: the git repo (`microsoft/winget-pkgs`) is YAML source only; clients never touch it. The default `winget` source resolves against a **prebuilt SQLite index bundled in a signed MSIX (`index.msix`/`source.msix`)**, generated server-side from the repo and served via CDN; manifest YAML fetched from CDN only on actual install. Architecturally the closest peer to OCX's "git repo (governance) → rendered artifact (resolution) → CDN" split — winget just picked SQLite-in-a-signed-package over flat JSON files. [winget sources discussion](https://github.com/microsoft/winget-cli/issues/6255)

### 4. OCX entry-schema recommendation — what we'd regret omitting

Current ADR starting point:
```json
{
  "name": "ocx.rs/kitware/cmake",
  "repository": "oci://ghcr.io/ocx-contrib/cmake",
  "owners": ["github:..."],
  "status": "active",
  "yanked": [],
  "created": "2026-07-11"
}
```

Gaps against peer systems, each tied to a concrete regret scenario:

- **`owners` as bare strings vs structured objects.** BCR tracks `github_user_id` (numeric, immutable) alongside the username specifically because GitHub usernames can be renamed or recycled — a bare `"github:alice"` string breaks silently if `alice` renames. **Recommend**: `owners: [{ "github": "alice", "github_id": 123456 }]`. Cheap now (42 packages), expensive to backfill later (exactly the D14 timing argument the ADR already made for the domain rename).
- **No deprecation message field.** BCR's `deprecated` (string, human-readable reason) and npm's `deprecated` field are both precedent for a free-text explanation shown to users, distinct from the binary `status` enum. **Recommend**: add optional `deprecated_message: string | null` alongside `status`.
- **No advisory digest hint — the ADR's own conscious trade-off.** D3 explicitly accepts TOFU-until-lockfile as a risk (registry-namespace compromise can serve bad content to first installs before a lockfile pins a digest). BCR's `source.json` pins an `integrity` checksum at exactly this layer for exactly this reason. OCX's homogeneous-OCI-backend advantage means an advisory field is nearly free to add: `latest_digest_hint: string | null`, populated by the same regen bot that already re-derives everything else from `tags/list`, **explicitly non-authoritative** (resolution still uses live `tags/list`, this is display/defense-in-depth only — a "does what I resolved match what the index last saw" client-side sanity check, not a trust anchor). Low-cost hedge against the one gap the ADR already flagged as deferred, without pulling in the full signing ADR.
- **What NOT to add** (confirmed by researching what peers deliberately avoid at pointer layer): version lists, dependency graphs, checksums-per-version, feature flags. crates.io's `cksum`/`deps[]` exist because crates.io's backend (a blob store) has no live "ask for the version list" API — cargo has to pre-bake it. OCX's homogeneous-backend advantage ("`tags/list` always live") is a genuine structural advantage over both crates.io and BCR here; re-adding that data would be regression to *their* constraint, not an improvement.

### 5. Path sharding — when it's needed and what actually drives it

Key finding: sharding in every peer system is driven by **git-repo/filesystem mechanics** (working-tree checkout size, GitHub UI/API directory-listing truncation, `git status`/`diff` cost), **not** by CDN/HTTP lookup cost — a name-addressed HTTP GET is O(1) regardless of how many sibling files exist in the same logical path. Concretely:

- GitHub truncates directory listings in the web UI and API at **1,000 entries**; official guidance recommends staying under **~3,000** per directory for repo health. [GitHub community discussion](https://github.com/orgs/community/discussions/136892), [GitHub repository limits](https://docs.github.com/en/repositories/creating-and-managing-repositories/repository-limits)
- Homebrew hit this concretely: flat `Formula/` forced GitHub to ask them to shard by first letter (331 MB unshallow, ~1 GB `.git`). BCR (~1,000 modules today) has not needed to shard `modules/` at all.
- OCX's own proposed layout (`p/<namespace>/<package>.json`) **already provides one level of natural sharding** for free, the same way npm scopes or Docker Hub namespaces fan out — as long as no single namespace itself accumulates thousands of packages. `ocx-contrib` (the community catch-all namespace) is the one namespace that could realistically hit that; that's the one to watch.

**Recommendation**: no crates.io-style prefix-hashing needed for the *deployed CDN files* at any package count OCX plans for. The trigger to actually add sharding is specifically **the `ocx-rs/index` git repo's own working-tree ergonomics**, specifically **inside `p/ocx-contrib/`** if that directory approaches ~1,000–3,000 entries. Concretely: if/when `p/ocx-contrib/` alone exceeds a few hundred files, shard it internally as `p/ocx-contrib/<first-char>/<package>.json`, leave every other namespace flat. Strictly a later, narrow, namespace-scoped change.

## Design Patterns Worth Considering

- **Pointer/payload separation with a lazy-fetch client** (BCR, Go module proxy, OCX D3/D4) — proven at BCR's and Go's scale; OCX's version already correctly identifies this as the right shape.
- **Announce = doorbell, not data** (Go proxy precedent, OCX D4) — payload-can't-lie property is the single strongest anti-supply-chain-tampering property available without full signing; *the* reason OCX's announce design is safer than a publisher-supplied metadata push (which crates.io still partially relies on via its publish API for the `cksum`/`deps` fields it must accept from the publisher).
- **Rendered-artifact indirection between git source-of-truth and client-facing format** (winget's SQLite-in-MSIX, Homebrew's git→JSON) — OCX's plan (git repo → CI render → static JSON on Pages) is the direct, simpler-tech analogue; no need for a binary index format at OCX's scale — flat JSON + CDN is the boring/appropriate choice; reserve SQLite-style prebuilt indexes for if/when `ocx search` becomes a real CLI feature (currently explicitly out of scope per D2).
- **Digest pinning at the pointer layer as a *cheap* hedge, not a full solution** (BCR `source.json.integrity`) — see the `latest_digest_hint` recommendation above; "do the 80/20 now" pattern distinct from committing to the full deferred signing ADR.
- **OCI Referrers API as the substrate for the eventual signing/provenance ADR** — scope that future ADR against Distribution Spec v1.1 Referrers + Sigstore/cosign, where the ecosystem is already converging.

## Sources

- [Cargo Registry Index reference (doc.rust-lang.org)](https://doc.rust-lang.org/cargo/reference/registry-index.html) — config.json fields, path sharding, per-version JSON schema, yank semantics, ETag/If-Modified-Since caching
- [RFC 2789: Sparse registry index (rust-lang/rfcs)](https://rust-lang.github.io/rfcs/2789-sparse-index.html) — original sparse-index proposal, caching rationale
- [rust-lang/crates.io-index `config.json`](https://github.com/rust-lang/crates.io-index/blob/master/config.json) — real-world minimal root file
- [Bazel registries (bazel.build/external/registry)](https://bazel.build/external/registry) — bazel_registry.json, metadata.json, source.json layout
- [bazel-central-registry `metadata.schema.json`](https://github.com/bazelbuild/bazel-central-registry/blob/main/metadata.schema.json) — exact metadata fields incl. `github_user_id`, `deprecated`
- [Homebrew formulae.brew.sh `docs/api.md`](https://github.com/Homebrew/formulae.brew.sh/blob/main/docs/api.md) — full/per-package/analytics endpoint catalog
- [Homebrew/brew issue #21622 — formula.jws.json 31MB monolith](https://github.com/homebrew/brew/issues/21622) — live scaling pain, Feb 2026
- [microsoft/winget-cli #6255 — index.db inside source2.msix](https://github.com/microsoft/winget-cli/issues/6255) — winget prebuilt-index architecture
- [Package managers keep using git as a database, it never works out — Andrew Nesbitt, Dec 2025](https://nesbitt.io/2025/12/24/package-managers-keep-using-git-as-a-database.html) — cross-ecosystem retrospective (cargo, homebrew, cocoapods, vcpkg, go modules, nixpkgs)
- [What Package Registries Could Borrow from OCI — Andrew Nesbitt, Feb 2026](https://nesbitt.io/2026/02/18/what-package-registries-could-borrow-from-oci.html) — OCI-as-backend thesis, external validation of OCX's approach
- [OCI Image and Distribution Specs v1.1 Releases — opencontainers.org](https://opencontainers.org/posts/blog/2024-03-13-image-and-distribution-1-1/) — Referrers API GA
- [GitHub repository limits docs](https://docs.github.com/en/repositories/creating-and-managing-repositories/repository-limits) — directory-width guidance (~3,000 recommended, 1,000 UI/API cap)

## Recommendation

Keep the ADR's plan as-is on the big structural decisions (pointer-only entries, static JSON over HTTP, no git-clone client, `config.json` root, conditional-GET freshness) — every one is independently convergent across crates.io, BCR, Homebrew, and winget, and the Dec-2025/Feb-2026 industry commentary (Nesbitt, ×2) confirms this isn't a bet, it's catching up to consensus. Two concrete, cheap additions before implementation, both extending the *already-planned* per-package regen bot rather than adding new machinery:

1. Change `owners` to structured `{github, github_id}` objects now, while the blast radius is 42 packages (same "do it before publish cost compounds" logic the ADR already applied to the `ocx.rs` domain rename in D14).
2. Add `deprecated_message: string | null` and `latest_digest_hint: string | null` (advisory-only, non-authoritative) to the entry schema — both populated for free by the regen bot from data it already reads off the registry, and both directly close gaps the ADR itself flagged (no deprecation UX, no defense-in-depth against the accepted TOFU trade-off) without pulling forward the full signing ADR.

On sharding: don't build it. The trigger isn't total package count (30–50k flat JSON files is a non-event for Cloudflare Pages) — it's `p/ocx-contrib/` specifically approaching GitHub's own ~1,000–3,000-file-per-directory comfort zone in the *source* repo. Leave a one-line note in the index-repo README pointing at that specific threshold, move on.

**Product-context flag** (per workflow-swarm.md rule 6): Nesbitt's "What Package Registries Could Borrow from OCI" (Feb 2026) is independent, credible validation of OCX's founding thesis — candidate citation for `product-context.md` positioning.
