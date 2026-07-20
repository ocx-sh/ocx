# Research: Metadata-Only Dependency-Closure Computation & Presentation

> Axis: design patterns. Produced by worker-researcher (sonnet) for the inspect-binaries-closure plan, 2026-07-20. Persisted by orchestrator (researcher session has no Write tool).

## Metadata

**Date:** 2026-07-20
**Domain:** packaging | CLI UX
**Triggered by:** `package inspect` design (OCX — Rust package manager over OCI registries)
**Expires:** 2027-07-20

## Direct Answer

Every mature package manager separates **metadata resolution** (cheap, cacheable, network-optional) from **content fetch** (expensive, the actual install). OCX already has this split at the code level — `inspect()`/`inspect_all()` in `tasks/inspect.rs` are explicitly read-only. This research is about hardening that boundary and shaping its output, not inventing it.

One line: treat local `tags/` + `blobs/` as a **local-first resolution cache keyed by digest**, fall back to registry only for tag→digest lookups or missing metadata blobs, and present the result as a **dedup'd tree with an explicit `(*)`-style truncation marker** plus a **flat JSON node-array mirroring `cargo metadata`'s `resolve.nodes`** for machine consumers — not a naive recursive JSON tree (cannot represent diamonds or cycles without duplication or infinite output).

## Q1 — Metadata-only computation across package managers

| System | Metadata source | What's cached | What's fetched | Offline failure mode |
|---|---|---|---|---|
| **Cargo** | Sparse HTTP index (one JSON-lines file per crate) + `cargo metadata` | Index files + `.crate` + extracted `src/` | Uncached index entries via conditional HTTP (ETag) | `--offline`/`--frozen` error if lockfile needs unsatisfiable update; populated lock+cache resolves zero-network |
| **npm/pnpm** | Registry "packument" (one JSON doc per name, all versions + deps) | Packuments briefly; abbreviated packument (`vnd.npm.install-v1+json`) for install path | Whole packument per new name; tarballs dedup'd via CAS (pnpm) | `--offline` = cache only; unresolvable range = hard error |
| **apt** | `Packages` index per repo/arch, cached in `/var/lib/apt/lists/` | Full dep graph — `apt-cache depends/rdepends` fully offline | Nothing for dry resolve; `.deb` fetch separate | Stale-but-present index still resolves; absent index = impossible |
| **Nix** | `nix-instantiate` eval → `.drv` store derivations (no build) | `.drv` content-addressed, cached forever | Only what eval needs (IFD anti-pattern breaks offline) | Pure eval fully offline |
| **Go modules** | Proxy protocol: `@v/list`, `@<v>.info`, `@<v>.mod` — graph without `.zip` | `GOMODCACHE`; proxy responses "strongly consistent" | `.info`/`.mod` per new version; `.zip` deferred | `GOPROXY=off` = cache-only |

**Repeating pattern:** graph-shape query and payload fetch hit *different endpoints* with *different cache lifetimes*; a published version's metadata is immutable → cacheable forever. OCX maps directly: `tags/` = mutable pointer, digest-addressed manifest/config blob = immutable metadata.

Sources: [RFC 2789 sparse-index](https://rust-lang.github.io/rfcs/2789-sparse-index.html), [cargo metadata](https://doc.rust-lang.org/cargo/commands/cargo-metadata.html), [packument](https://github.com/vweevers/packument), [npm offline](https://addyosmani.com/blog/using-npm-offline/), [apt-cache](https://manpages.debian.org/testing/apt/apt-cache.8.en.html), [Go module proxy](https://proxy.golang.org/), [Nix IFD](https://nix.dev/manual/nix/2.34/language/import-from-derivation), [pip #12184](https://github.com/pypa/pip/issues/12184).

## Q2 — Presenting transitive trees with edge qualifiers

**`cargo tree`** — strongest prior art:
- `-e/--edges` filters edge kind (`normal`/`build`/`dev`/…).
- Diamonds **deduplicated in display**: repeat-visit node truncated + suffixed `(*)`; `--no-dedupe` opts out. The single most important UX decision — truncation marker, not silent omission, not unbounded repetition.
- `--depth` control; `-i/--invert` for reverse "why" queries (same job as `pnpm why`).

**`cargo metadata` JSON** — flat node array, not nested tree:
```json
"resolve": { "nodes": [ { "id": "pkgA 1.0.0", "deps": [{"name":"pkgB","pkg":"pkgB 2.0.0","dep_kinds":[{"kind":null}]}], "dependencies": ["pkgB 2.0.0"] } ] }
```
Cycle-safe and diamond-safe by construction: diamond = shared entry, cycle = back-edge to a listed ID. A naive nested `{name, deps:[…]}` JSON tree **cannot represent a cycle** and duplicates diamond subtrees (npm ls / pnpm list --json weakness — not a pattern to copy).

**pnpm `why`**: reverse-tree with `deduped` markers — precedent for a future `--why` mode, not day-one.
**`ldd`**: flat list baseline; not a DAG model.

**Recommendation for OCX `package inspect`:**
1. Human mode: cargo-tree-style indentation, `(*)` on repeat-visit **keyed by content digest** (stronger identity than cargo's string IDs), depth via flag only if needed.
2. `--format json`: flat node array keyed by digest, edges reference digests. Backend-first (product principle #1).
3. Cycles: flat shape forecloses the failure mode regardless of whether OCX's model permits them.

Sources: [cargo tree](https://doc.rust-lang.org/cargo/commands/cargo-tree.html), [Krycho — Using cargo tree](https://v5.chriskrycho.com/journal/using-cargo-tree/), [pnpm why](https://pnpm.io/cli/why), [pnpm list](https://pnpm.io/cli/list), [npm ls](https://docs.npmjs.com/cli/v11/commands/npm-ls/).

## Q3 — Cache-first content-addressed lookup invariant

> **Digest lookups are always local-first and never go stale; tag lookups are never cache-opaque.**

- **Digest → content is immutable.** Nix `.drv`, Cargo `.crate` cache, Go proxy "strongly consistent" responses, pip's per-`(name,version,checksum)` cache: resolved immutable identity = cacheable forever, zero revalidation.
- **Tag → digest is the only staleness surface.** Cargo's ETag'd sparse index revalidates the *index entry*; the `.crate` it points to never changes. apt's `Packages` file is the mutable pointer.

**Pitfall prevented:** conflating the two into one revalidation policy — re-checking a digest-pinned blob "just in case" (defeats offline-first) or treating a tag as cache-forever (serves stale data after republish). Any registry round-trip in the closure walk must be attributable to a specific unresolved tag or missing metadata blob, never "revalidation."

No canonical industry name; closest explicit statements: Go proxy strong-consistency guarantee, pip/Cargo immutable-release-metadata assumption.

## Recommendation

1. Confirm inspect's walk is phase-split: (a) tag→digest (network-optional, ChainMode-gated); (b) digest→metadata (always local-first, never revalidated).
2. Flat digest-keyed node-array for `--format json` (cargo `resolve.nodes` pattern).
3. `(*)`-truncation dedup for the human tree. Skip `--no-dedupe`/`-e`/`-i` flags until a second real use case (YAGNI — cargo grew these over years).
4. Don't copy npm/pnpm nested-tree JSON for machine output.

## Metadata Footer

**Confidence:** High Q1/Q2; Medium Q3 (synthesis, not a named industry term).
**Staleness:** Re-check in 12 months.
