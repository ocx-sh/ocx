# ADR: Public Package Index with Registry Indirection (ocx.rs)

<!--
Decision-record bundle, deliberately wider than one-decision-per-ADR:
captures the full 2026-07-10/11 design discussion so a later planning
phase (/swarm-plan) can start from this document alone.
-->

## Metadata

**Status:** Accepted (design level — implementation not started)
**Date:** 2026-07-11
**Deciders:** Michael Herwig + Claude (discussion record)
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Follows Golden Path: GitHub Actions, GitHub (repos/GHCR), Cloudflare edge (`product-tech-strategy.md` Infrastructure table)
**Domain Tags:** infrastructure | integration | security
**Supersedes:** current `_catalog`-based open catalog on self-hosted Artifactory

## Context

Today the open OCX catalog lives on a **self-hosted Artifactory** behind `ocx.sh`, and
package listing uses the OCI `_catalog` endpoint. Problems:

1. **`_catalog` is a dead end.** It is an optional extension, not core OCI distribution
   spec. GHCR and Docker Hub do not implement it at all. Where it exists it returns repo
   names only (no versions/descriptions → N+1 manifest fetches), has unstable pagination,
   and often needs registry-wide read scope. Any move to GHCR forces us off `_catalog`
   regardless of scale.
2. **Scale ambition.** ~42 packages today (some coworker usage, small blast radius),
   a few hundred midterm, aspirationally 30–50k for a public open-source catalog.
3. **Server exit (security-motivated).** The self-hosted server running Artifactory must
   go. Target: everything on managed cloud infra (GHCR for storage, Cloudflare for static
   hosting). Not a cost decision — an attack-surface decision.
4. **Corporate story must stay untouched.** OCX targets corporates running private
   registries; they use plain OCI and must never need the public index.

Precedent inside the family: the sibling project **Grimoire** (OCI-distributed AI
artifacts) hit the same wall and adopted a Bazel-style index repository indirection.

## Decision Drivers

- Zero production services owned by us (static hosting only, no own server)
- GHCR as physical storage; identifiers must survive storage migrations
- Homogeneous backend: everything under the index is OCI — unlike Bazel/cargo we can
  always `GET /v2/<repo>/tags/list` (this is a genuine differentiator; exploit it)
- Fast resolution on the critical install path
- PR-based governance for an open catalog (namespace ownership, squat defense)
- Boring tech, proven upgrade paths; design for hundreds now, don't build for 50k

## Industry Context & Research

Informal (discussion-level) survey; no separate research artifact.

| System | Lesson taken |
|---|---|
| Bazel Central Registry | Git repo as source of truth, PR governance, per-module metadata + source pointer, presubmit validation. Survives because clients lazily fetch single files over HTTP — never clone. Merge policy precedent: metadata/pointer change = human review, version bump from listed maintainer = auto-merge. |
| crates.io sparse index (RFC 2789) | Git-clone index died at scale; replaced by name-addressed static files over HTTPS (`/ca/rg/cargo`). Per-name lookup = O(1), CDN-cached, no pagination concept needed. `config.json` at index root carries format/endpoints. Proven to 150k+ crates. |
| Homebrew / winget / APT | Full-catalog snapshot download **is** state of the art for search/browse at package-manager scale (Homebrew JSON API blob, winget ships an SQLite db, APT package lists for 30 years). PyPI *removed* server-side search (operational burden). |
| Go module proxy / pkg.go.dev | "Announce = doorbell": requester asks the proxy to fetch; proxy derives truth from the source. Announcer payload can never lie. |
| Docker Hub / `docker.io` | Short logical names are client-side rewrites (`docker.io` → `registry-1.docker.io`). Flat claimed namespaces, not repo-bound. |
| registry.k8s.io | Server-side OCI redirect facade over backing stores. Named and **rejected** here (see D8) — it is the zero-client-change alternative at the cost of an always-on service. |

**Key insight:** two different query types scale differently. *Resolution* (name known →
where is it?) is name-addressed and needs no pagination ever. *Discovery/search* (what
exists?) needs a corpus — client-side over a snapshot at our scale, never a server.

## Decisions

### D1 — Index indirection; git repo is source of truth, clients read rendered static files

A public **index repository** (GitHub, org `ocx-rs`) holds one small JSON file per
package. CI renders it to **static sparse-index files** on Cloudflare Pages. Clients
fetch per-package files over HTTPS; clients never clone git (cargo's hard lesson).
Physical package storage: **GHCR** under `ghcr.io/ocx-contrib/<pkg>` (mirrors already
publish via the ocx-contrib org).

### D2 — Resolution vs discovery split

- **Resolution** (CLI critical path): name-addressed per-package file, <1 KB, CDN-cached.
- **Discovery/search**: website feature, CI-rendered from the index (+ registry
  enrichment offline). Optional `all.json` catalog snapshot **deferred** until website
  search wants it. At 42–400 packages even the full catalog is 10–50 KB — sharding
  (crates.io path-prefix scheme) is a documented upgrade path, not v1 work.
- `ocx search` in the CLI: not planned for v1; discovery lives on the website.

### D3 — Entry content: pointer + governance, NOT a metadata mirror

Per-package entry holds: logical name, physical repository pointer, owners, status
(deprecation / yanked versions). It does **not** mirror tags/versions/descriptions.

Rationale: homogeneous OCI backend means version listing is always available live via
`tags/list`; pointer-only entries mean the index changes only when a *new package*
appears — release publishing needs **no index write**, killing the dual-write/staleness
problem for the common case.

**Staleness rule (invariant):** mutable pointers (cascade tags `3`, `3.28`, `latest`)
are never resolved from index data. If tag data is ever snapshot into the index later,
it is advisory/display-only. Resolution = index for *location*, live OCI (or lockfile
digest) for *versions/content*.

The one per-version fact that cannot live in the registry: **yank** ("exists but do not
resolve") — that is index data, part of `status`.

Consciously accepted trade-off: pointer-only = no index-level digest pinning, so a
registry(-namespace) compromise alone suffices to serve bad content to first-time
installs (lockfiles pin digests after that — TOFU). BCR pins digests in `source.json`;
we defer this to a separate **signing/provenance ADR** (see Deferred).

### D4 — Announce protocol: doorbell, not data

Publisher signals only "refresh package X". An index bot regenerates the entry from
**registry truth** (`tags/list` + manifests) and commits one file change.

- Cascade-safe: OCX push cascades tags (`3.28.1` → `3.28`, `3`, `latest`); regeneration
  sees post-cascade state — publisher never enumerates what changed.
- Idempotent: N announces = same result; coalescing trivial; no delta bookkeeping,
  no "which do we have to re-announce" tracking. Payloads cannot lie (Go-proxy model).
- Drift self-heal: nightly reconcile cron re-scans all entries (catches pushes that
  bypassed announce). Matches OCX's existing self-heal-on-next-clean pattern.
- Transport ladder: v0 = human edits entry file + PR (fine at 42 packages);
  v1 = `repository_dispatch`/`workflow_dispatch` from publisher CI;
  v2 = `ocx package push --announce` sugar over the same dispatch. CLI stays thin.

### D5 — Bot merge policy

| Event | Action | Precedent |
|---|---|---|
| New package (new entry file) | PR + human review — namespace gate | BCR new modules, winget moderators |
| Existing entry refresh, regen matches registry, CI green | Auto-merge | winget verified publishers, Homebrew autobump, Scoop excavator |
| Yank / deprecate / transfer / pointer change | Human review always | BCR `metadata.json` policy |

### D6 — Ownership proof without accounts

OCX manifests embed their canonical identifier. Rule: index entry name must equal the
identifier embedded in the manifest at the claimed physical repo → **registry write
access = ownership proof**. Squatting requires GHCR namespace control, not a fast PR.
`owners` field (GitHub usernames) exists for transfers/human overrides.

### D7 — Naming model: Docker-Hub-style flat claimed namespaces

Not repo-bound (Grimoire binds name to repo+host; wrong here — OCX packages are tools,
not repos; one repo may ship many tools). Flat namespaces (`ocx/cli`, `kitware/cmake`),
first-come with human-reviewed new-namespace PRs. Index maps logical → physical, N:1
allowed.

### D8 — Client architecture: early identifier rewrite

Logical `ocx.rs/cmake` → physical `ghcr.io/ocx-contrib/cmake` resolved **once, early**,
before the existing pipeline. Everything downstream (LocalIndex, lockfile, layer store)
operates on physical identifiers, unchanged.

Rejected alternatives:
- **RedirectingIndex as a fourth `IndexImpl`**: catalog/discovery ops fit the trait, but
  resolution indirection buried inside the chain hides physical identity from cache,
  lock, and storage layers. Honest assessment: this is a resolver stage, not a data
  source — "just one more trait impl" was oversold.
- **Server-side OCI facade** (registry.k8s.io style — Cloudflare Worker implementing the
  distribution-spec read path, 302 → GHCR): zero client changes, but an always-on
  service to operate, token-dance handling, redirect hop per request — and the index
  repo is still needed for governance/catalog. Killed by the "zero prod moving parts"
  driver.

### D9 — Dual-name identifiers for correct reporting

Carry both names through the pipeline (`requested` logical + `physical` resolved).
Display rules: user-facing output shows what the user typed (logical); physical appears
in verbose output, **error messages** (registry errors reference physical — needed for
debugging), and as an explicit report/API field (e.g. `resolved_from`). Lockfile stores
logical name + physical ref + digest.

### D10 — Known-registries config: declared type, exact match, no probing

Named registry entries in config; single `url` key typed by scheme prefix:
`index+https://…` or `oci+https://…` (cargo `sparse+` precedent; equivalent to two
mutually-exclusive keys, prefix form chosen — one key, parse once).

Resolution rule (predictability invariant):

```
registry component of identifier
  → exact match against known-registries keys?
      yes → behavior per entry's url prefix (index-resolve | direct OCI)
      no  → literal OCI host
```

- Type is declared **locally in config**, never inferred from the network — nothing can
  be "mistaken" for the other.
- Rewrite runs **once**; index-resolved physical refs are literal, no recursive alias.
- No fuzzy matching, no well-known probing (latency + ambiguity).
- Residual risk (documented): typo of an alias falls through to literal DNS. Mitigated
  for our names by shipping both `ocx.rs` and `ocx.sh` in the default map.
- Default `ocx.rs` → index endpoint is baked into shipped defaults.

### D11 — Storage keying: logical names never reach the storage layer

Layer store, lock physical refs, and local index are keyed by **physical** registry.
Precedent already in-tree: `mirror_map` rewrites host before storage keying (mirrors are
the primary storage target). Index rewrite is the same transformation class and runs in
the same pre-storage stage. Consequences: layer-sharing boundary = actual origin; two
aliases of one physical registry share correctly; one logical name can never span two
physical registries' layer pools (cache-poisoning shape avoided). Order:
`logical → index resolve → physical → mirror_map → actual host`.

### D12 — Index format versioning: root `config.json`

Index root carries `config.json`: `format_version` + endpoints (cargo precedent).
Client rule: unknown fields ignored (additive evolution); `format_version` bump = hard
error ("upgrade ocx"). Kept separate from any future `all.json` snapshot — config is
tiny + stable, snapshot is bulk + regenerated.

### D13 — Freshness: boring HTTP

Conditional GET (`If-None-Match`/ETag → 304) on index files; CDN respects it. No custom
update protocol. Root-pointer + digest-named immutable shards (TUF/OCI-style layout) is
the documented upgrade, not v1.

### D14 — Domain: ocx.rs canonical, migrate NOW

`ocx.rs` becomes the canonical logical registry (org `ocx-rs` exists; ~$30/yr vs ~$80).
Timing argument: manifests embed canonical identifiers and the ownership check (D6)
compares against them — rename cost grows with every publish. At 42 packages the blast
radius is the smallest it will ever be; "mid-term" was the trap. `ocx.sh` stays a
permanent-ish alias in the default known-registries map during transition (owned this
year; renewal optional). One canonical name, forever, decided before public launch.
No hard preference existed between the two; ocx.rs chosen for org match + momentum.

### D15 — Hosting: Cloudflare Pages for index AND website; two projects, two surfaces

Both surfaces on Cloudflare Pages (security-motivated server exit + one stack; GitHub
Pages rejected to avoid split infra). **Two separate Pages projects** — one project =
one deploy artifact; combining would couple index refreshes to website rebuilds:

| Surface | Pages project | Domain | Deployed by |
|---|---|---|---|
| Website (human) | `ocx-website` | apex `ocx.rs` | ocx repo `deploy-website.yml` (`deploy-cloudflare` job; rsync-to-server job retired at migration step 6) |
| Index (machine) | `ocx-index` | `index.ocx.rs` | `ocx-rs/index` `deploy.yml` |

Plane separation (clears the "index.ocx.rs is long" confusion): the **logical name**
users type stays `ocx.rs/<pkg>` — a config key. The **endpoint URL** the client fetches
is `https://index.ocx.rs/...` — plumbing users never see (docker.io →
registry-1.docker.io pattern). Endpoint chosen final *before* CLI defaults are written
= zero churn. Website dev target = Pages branch alias `dev.ocx-website.pages.dev`
(replaces dev.ocx.sh at server exit). Apex flip (evict bootstrap attachment from
ocx-index, attach to ocx-website, repoint CNAME) runs idempotently in the prod deploy.

## Technical Details

### Index repo layout (proposal — planning phase refines)

```
config.json                     # format_version, endpoints
p/<namespace>/<package>.json    # one entry per package
```

Entry schema starting point (planning phase finalizes):

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

### Resolution flow

```
user: ocx install ocx.rs/cmake
  1. registry component "ocx.rs" → known-registries hit, type=index
     (default map: ocx.rs → index+https://index.ocx.rs)
  2. GET https://index.ocx.rs/p/.../cmake.json   (CDN, ETag/304, local cache)
  3. rewrite → ghcr.io/ocx-contrib/cmake   (physical; done once)
  4. existing pipeline: tags/list + manifests against GHCR,
     mirror_map, LocalIndex, lockfile (logical + physical + digest),
     layer store keyed by physical host
```

Cold path = 1 tiny CDN fetch + normal OCI flow. Warm = 304 or local cache.
Lockfile-pinned installs skip the index entirely.

### Announce flow

```
publisher CI: push to GHCR (cascade tags mutate)
  → doorbell: repository_dispatch {package: "kitware/cmake"} to index repo
  → bot job: regenerate p/kitware/cmake.json from registry truth
  → diff? → PR
      new file        → label new-package, await human review
      refresh + green → auto-merge
nightly: reconcile cron regenerates all entries (drift self-heal)
```

## Migration Plan (42 packages)

1. [ ] Ground Setup complete (below — human actions)
2. [ ] Create index repo under `ocx-rs`; seed `config.json` + 42 entries pointing at
       `ghcr.io/ocx-contrib/<pkg>`; CI renders to Cloudflare Pages at `ocx.rs`
3. [ ] Re-run mirror pipelines republishing to GHCR with **`ocx.rs/...` embedded
       identifiers** (required for D6 ownership check; clean republish, no
       dual-identifier compat per pre-1.0 no-backward-compat rule)
4. [ ] CLI work lands (see Planning Inputs): known-registries config, early rewrite,
       dual-name reporting, storage-keying audit
5. [ ] Coworkers: one-line registry switch to `ocx.rs`; lockfiles re-resolve;
       `ocx.sh` alias covers stragglers
6. [ ] Retire Artifactory + own server
7. [ ] Side-effect if repos move org (e.g. `rules_ocx` to `ocx-rs`): BCR
       `metadata.json` `repository` field update = **one manual-review PR** at BCR
       (pointer changes are never auto-merged there); version bumps auto-merge again
       afterwards since maintainer usernames are unaffected. Reinstall "Publish to BCR"
       app on the new repo location. GitHub redirects keep old `source.json` archive
       URLs alive — never let a new repo claim the old name.

## Ground Setup (human actions before autonomous work)

### Cloudflare (DONE — 2026-07-11)

- [x] Zone `ocx.rs` added; nameservers switched at registrar (istanco.net); zone **Active**.
      Zone id `e7e05c1d7edc241e4a7fde08e423483f` (hardcoded in deploy workflow).
- [~] DNSSEC: **skipped** — registrar (istanco.net, `.rs`) exposes no DS-record field.
      Deferred; optionally file a registrar support ticket later. Disable the half-enabled
      DNSSEC in Cloudflare (DNS → Settings) so no orphan-DS signal lingers.
- [x] Pages project `ocx-index` created via wrangler in CI; custom domain `ocx.rs` attached.
- [x] Deploy mode: **GitHub Actions + wrangler** (config-as-code) — `.github/workflows/deploy.yml`
      in `ocx-rs/index`. Bootstraps project, deploys `public/`, ensures domain + apex CNAME.
- [x] API token created: *Account · Cloudflare Pages · Edit* + *Zone · DNS · Edit · ocx.rs*.
      Stored as repo secrets `CLOUDFLARE_API_TOKEN` + `CLOUDFLARE_ACCOUNT_ID`.

### DNS (in the Cloudflare zone)

- [x] `index.ocx.rs` → proxied CNAME to `ocx-index.pages.dev`, canonical index endpoint
      (D15 layout). Provisioned check-then-act by the index deploy workflow.
      **Learning:** Pages custom-domain attach does NOT auto-create the apex record —
      records are provisioned explicitly in both deploy workflows.
- [x] Apex `ocx.rs` → currently CNAME to `ocx-index.pages.dev` (bootstrap placeholder).
      Repointed to `ocx-website.pages.dev` by the website's first prod deploy
      ("Point ocx.rs at the website" step: evict from ocx-index → attach → repoint).
- [ ] Pending human: set `CLOUDFLARE_API_TOKEN` + `CLOUDFLARE_ACCOUNT_ID` secrets on the
      **ocx repo** (same values as ocx-rs/index), merge `feat/website-cloudflare-deploy`,
      then dispatch `deploy-website.yml` with target=prod to flip the apex.
- [ ] Optional: `www.ocx.rs` redirect to apex (deferred)
- [x] `ocx.sh` zone/hosting left untouched until migration step 6 (rsync deploy job
      kept alongside the new `deploy-cloudflare` job until server retirement)

### GitHub

- [x] Org `ocx-rs`: index repo created — **`ocx-rs/index`** (public, auto-merge enabled).
      Name `index` chosen over `registry` (registry = GHCR's job; this is the sparse index).
      Seeded with `public/config.json` (format_version 1 placeholder), `public/index.html`
      landing, and `.github/workflows/deploy.yml` (Cloudflare Pages deploy substrate).
      Deploy verified green; `config.json` served at `ocx-index.pages.dev` and `ocx.rs`.
- [ ] Index repo settings: branch protection on `main` (required status checks),
      "Allow auto-merge" enabled
- [ ] Index repo Actions secrets: `CLOUDFLARE_API_TOKEN`, `CLOUDFLARE_ACCOUNT_ID`
- [ ] Announce doorbell credential: publishers (mirror CI in `ocx-contrib`) need a token
      able to fire `repository_dispatch` on the index repo — fine-grained PAT on the
      index repo (contents:read + metadata + actions/dispatch) stored as org-level
      secret in `ocx-contrib`, or a small GitHub App later. v0 (manual PRs) needs none.
- [ ] `ocx-contrib`: confirm GHCR publishing path — mirror workflows publishing with
      repo-scoped `GITHUB_TOKEN` (`packages: write`) if mirror repos live in
      `ocx-contrib`; otherwise org-level PAT with `write:packages`
- [ ] GHCR package visibility: public; org setting must allow public packages
- [ ] Domain: keep `ocx.sh` registration through transition (alias in default map)

## Deferred / Out of Scope (each needs its own ADR or spec before public launch)

- **Signing / provenance / index-level digest pinning** — the D3 trade-off consciously
  accepts TOFU-until-lockfile; supply-chain hardening is a separate ADR
- Yank semantics spec (CLI behavior on yanked version; resolve vs install vs lock)
- Namespace transfer / dispute process
- `all.json` search snapshot + website search
- Index published as OCI artifact for air-gapped mirrors (ocx-mirror consumes catalog
  the same way it mirrors packages)
- Sparse-index sharding (crates.io prefix scheme) — trigger: per-directory file counts
  or snapshot size hurt, not before
- `ocx search` CLI command

## Planning Inputs (for /swarm-plan later)

CLI-side work identified but not designed:

1. Known-registries config schema (`index+`/`oci+` url prefixes, default map with
   `ocx.rs` + `ocx.sh` aliases) — touches config + docs
2. Early rewrite stage: resolver fetching per-package index files, HTTP cache
   (ETag/local TTL), placement before LocalIndex/lock/storage keying
3. Dual-name identifier (`requested`/`physical`) — touches Identifier or a wrapping
   ResolvedIdentifier, display/error/report surfaces, lockfile schema (logical +
   physical + digest)
4. Storage-keying audit: verify no logical name reaches `file_structure`/layer store
   (mirror_map stage is the pattern to co-locate with)
5. `config.json` format_version handling (ignore-additive / hard-fail-major)
6. Index repo: entry schema, render CI, bot (regen + merge policy), reconcile cron
7. Announce dispatch (v1) + `--announce` sugar (v2)

Open questions for planning: index repo name; exact entry schema (owners format,
status enum); announce transport details; whether website moves to Cloudflare too or
stays on GitHub Pages; ocx.sh sunset criteria.

## Links

- `.claude/rules/subsystem-oci.md` — Index/ChainedIndex/mirror_map current state
- `adr_oci_registry_mirror.md` — mirror rewrite precedent (D11)
- `adr_custom_oci_identifier.md` — identifier semantics (D9 touches this)
- `.claude/rules/product-context.md` — corporate vs open-catalog positioning
- Bazel Central Registry, crates.io sparse index RFC 2789, registry.k8s.io — external precedents

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-07-11 | Michael + Claude | Initial record from design discussion (2026-07-10/11) |
