# ADR: The Lock Records the Physical Address — Resolution Output, Never Input

## Metadata

**Status:** **Rejected** (2026-07-30) — its founding premise is false. This ADR treats the lock's
logical `repository` as the defect. It is not: `adr_index_indirection.md` Decision C deliberately keys
storage, `ocx.lock` and GC roots on logical identity, C2 makes physical **transport-only**, and the code
implements exactly that (`ResolvedChain { pinned, transport_pinned }`, `resolve.rs:360-416`; chain blobs
written under the logical key at `common.rs:453-465`).

The observed offline break has a different cause and a far smaller fix: `LocalIndex` never implements
`physical_reference` (falls to the `index_impl.rs:113-116` default `Ok(None)`) despite every root
document carrying `repository` (`wire.rs:54`), and `ChainedIndex::physical_reference`
(`chained_index.rs:1051-1060`) reads no local copy. Nothing in the lock format, the index layout, `o/`,
the wire format, or the mirror fleet needs to change. Owner decision 2026-07-30: the transport address
is re-derived locally, never recorded — recording it would pin routing and stop a locked project
following a registry migration, where re-deriving follows it and the digest still pins the bytes.

Kept as a record of the reasoning, not as a plan. Superseding work: `ocx-sh/ocx#159`.
**Date:** 2026-07-29
**Deciders:** mherwig (S1–S11 settled prior to this ADR)
**Beads Issue:** N/A
**Related issues:** [ocx-sh/ocx#42](https://github.com/ocx-sh/ocx/issues/42) (unified freshness/update-check
strategy — home for §U), [ocx-sh/ocx#159](https://github.com/ocx-sh/ocx/issues/159) (audit direct
`Context::remote_client()` call sites — the update check is one), [ocx-sh/ocx#251](https://github.com/ocx-sh/ocx/issues/251)
(configured index authoritative), [ocx-sh/ocx#33](https://github.com/ocx-sh/ocx/issues/33) (project
toolchain config — origin of `ocx.lock`)
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Follows Golden Path in `.claude/rules/product-tech-strategy.md` — Rust 2024 / Tokio, no new
  dependency; the change is a field-semantics flip plus a path map.
**Domain Tags:** architecture | oci | index | data | devops
**Supersedes:** N/A
**Amends:** [`adr_index_indirection.md`](./adr_index_indirection.md) §Decision C (identity is logical,
location is routing) — still true of `ocx.toml` and of the index; **no longer true of `ocx.lock`**.
**Depends on:** [`adr_oci_index_only_dispatch.md`](./adr_oci_index_only_dispatch.md) (dispatch-only
object store — S7 restates it, does not change it), [`adr_platform_model_unification.md`](./adr_platform_model_unification.md)
(per-platform leaf-digest map in the lock)

---

## Context

### The defect (recorded, not re-derived)

`crates/ocx_lib/src/project/resolve.rs:516` writes `repository: identifier.without_specifiers()`, where
`identifier` is the **`ocx.toml`-declared (logical)** name obtained from `declared_identifier` (`:463`).
The physical target the resolve just produced is discarded. `ocx.lock` therefore records a logical name
for a package the index redirects — logical `ocx.sh/astral-sh/python-build-standalone`, physical
`oci://ghcr.io/ocx-contrib/astral-sh/python-build-standalone`.

Consequence, proven empirically on a warm store with the network cut: `ocx run` / `ocx env` / `ocx pull`
exit **69**. The install path calls
`package_manager/tasks/resolve.rs:472 → resolve_transport_pinned → physical_reference →
ocx_index.rs:859 physical_identifier → :730 resolve_root → :695 check_format_version`, and
`check_format_version` is a bare `GET <index base>/config.json` propagated with `?`, with no local
fallback — unlike `chained_index.rs:1030-1048 fetch_manifest_raw_bytes`, which tolerates source failure.

Red/green, same warm store, same cut network: stock config → exit 69; `[registries."ocx.sh"] index = ""`
→ exit 0. `ocx.sh` carries a compiled-in default index (`config/loader.rs:195-207`), so this is every
stock install. Only `--offline` and `ocx direnv export` (`find_plain`, a pure store read) survive.

This violates Product Principle 2 (**offline-first**) and Differentiator 4 (offline-first indexing) on
the most common command in the product.

### Two breaks, one root cause — and why they separate

The settled decisions bundle two changes that look like one:

1. **Lock semantics.** `LockedTool.repository` becomes physical; install stops dereferencing.
   Touches `ocx` and the fleet's `ocx.lock` files. Fixes the offline defect.
2. **Index wire layout.** `p/<logical-registry>/<ns>/<pkg>.json`, `o/<host>/sha256/<hex>.json`,
   `c/<logical-registry>/index.json`. Touches `ocx`, `ocx-sh/index`, and `index.ocx.sh`. Fixes nothing
   about offline — it makes the index copyable and its objects shareable.

They are separable, and **S5 is what separates them**: once install never consults the index, the lock's
contents are entirely independent of the index's layout. A lock written before the layout change stays
valid after it, because a physical address is not an index path. This ADR's ordering leans on that fact
throughout — it is the single most load-bearing observation here.

### What the lock looks like, before and after

```toml
# before — logical; install must ask the index what this means
[[tool]]
name = "python"
group = "default"
repository = "ocx.sh/astral-sh/python-build-standalone"

# after — physical; install has nothing left to ask
[[tool]]
name = "python"
group = "default"
repository = "ghcr.io/ocx-contrib/astral-sh/python-build-standalone"
```

`[tool.platforms]` (canonical-grammar platform → leaf digest) is unchanged. The `oci://` scheme marker
carried by the index root's `repository` field is stripped on parse (`parse_physical_repository`); the
lock stores the bare `Identifier`, as it does today. The logical name is not written anywhere in the
lock — S2: it is recoverable from `ocx.toml` via `(group, name)`, which `declared_identifier` already
does.

---

## Decision Drivers

- **Offline-first is a product principle, not a feature.** A warm store plus zero network must serve
  `run`/`env`/`pull`. Today it does not.
- **A lock is a record of a resolution, not a restatement of a request.** Anything install re-derives is
  not locked, and anything not locked is not reproducible.
- **Two writers, one artifact.** `ocx announce` and the Python indexbot both write the index's source
  tree. A path-map disagreement produces a rejected PR per package, fleet-wide.
- **The bootstrap is a released binary.** 43 mirror repos install ocx through a SHA-pinned `setup-ocx`.
  Any ordering that requires an unreleased ocx to un-break CI is a deadlock.
- **Published packages must keep resolving** (CLAUDE.md hard exception). An index root is on that read
  path.
- **Fleet migration cost is paid in PRs.** 64 lock files, ~53 repos. Every ordering must be scriptable.
- **Pre-1.0 breaks are announced in `CHANGELOG.md` and nowhere else** — no dual-form parsing, no warning
  schedule, no migration prose in user docs.

---

## Industry Context & Research

**Research artifact:** N/A (surveyed inline; primary sources are each tool's lock-file documentation).

| Tool | Lock records | Indirection resolved… | Physical location in the lock? |
|---|---|---|---|
| **Cargo** | `name`, `version`, `checksum` (sha256 of the `.crate`), `source` = registry *source id* (`registry+https://github.com/rust-lang/crates.io-index`) | at fetch time, from the registry index's `config.json` `dl` template | **No** — logical source id only |
| **npm** | `resolved` = fully-qualified tarball URL, `integrity` = SRI hash | at lock time | **Yes** — install fetches `resolved` directly |
| **Go** | `go.mod` module paths (logical); `go.sum` = `h1:` hashes of module zip + `.mod` | never — `GOPROXY` is ambient config, never recorded | **No** — location is entirely environmental |
| **Nix flakes** | `locked` node: `{type, owner, repo, rev, narHash}` — the *dereferenced* form of the symbolic input in `flake.nix` | at lock time; flake-registry indirection (`nixpkgs` → `github:NixOS/nixpkgs`) is baked into the lock | **Yes** — the lock stores resolution output by construction |
| **Bazel bzlmod** | `MODULE.bazel.lock`: registry URLs + hashes per module | at lock time | **Yes** |

**Key insight.** The split is not "logical vs physical" — it is **what the client must do at install time**.
Go and Cargo can lock logical names because they re-derive the location from ambient config (`GOPROXY`,
`[source]` replacement) *without a network round trip that can fail*: Cargo reads the local registry
cache, Go reads an env var. OCX cannot: its re-derivation is an HTTPS `GET <index>/config.json` with no
local fallback. Locking a logical name is only safe when dereferencing is free and offline. OCX's is
neither, so OCX must lock the dereferenced form.

**What OCX takes:**

- **Nix flakes** as the governing principle — the lock stores the *output* of registry resolution, and
  the symbolic name stays in the input file (`flake.nix` ↔ `ocx.toml`). This is S1 verbatim, and it is
  the closest structural precedent: flake-registry indirection is exactly OCX's index indirection.
- **npm** as the concrete field shape — one resolved, physical address per entry, installed without
  further metadata lookup.
- **Go** as the trust model — the content hash, not the address, is the integrity anchor. OCX already
  digest-pins per platform, so a wrong `repository` yields a 404, never wrong bytes. This is what makes
  the physical address safe to record: it is a routing hint the CAS verify keeps honest.

**What OCX rejects, and why:** Cargo's "lock the source id, re-derive the URL". It is the current design
and it is the defect.

**The npm counter-argument, and OCX's answer.** npm's `resolved` URLs are the canonical complaint against
physical locks: a lock generated against a private mirror carries mirror hostnames and is not portable,
which is why pnpm/yarn strip or rewrite the host. OCX does not inherit this. `[mirrors]` rewriting
happens **below** the lock, at transport time — the documented resolution pipeline is
`root.repository → mirror_map → fetch` (`website/src/docs/in-depth/indices.md:174-176`). A physical
address in the lock is still rewritten to a corporate mirror without re-locking, so the lock stays
portable across sites in the one dimension npm's is not. This is Differentiator 9 paying for
Decision S2.

---

## Settled Decisions (owner-locked inputs — recorded, not re-opened)

| # | Decision | Rationale of record |
|---|---|---|
| **S1** | The lock stores resolution **output**, never input. Anything install re-derives is not locked. | Re-derivation is the failure surface; a lock that requires it is not a lock. Nix-flakes precedent. |
| **S2** | `LockedTool.repository` becomes the **physical** address. Physical only — the logical name is recoverable from `ocx.toml` via `(group, name)`. "Lock without a toml" is YAGNI. | Storing both invites divergence and a third parse path for zero use case. `declared_identifier` already does the recovery. |
| **S3** | `ocx.toml` keeps the logical name. | Identity is logical (`adr_index_indirection` C); only the *lock* moves. Migrating a package's backing registry stays a zero-diff event for `ocx.toml`. |
| **S4** | `lock_version` → **2**, not 4. No compat for unreleased formats. Renumbering *down* is deliberate. | See **§P — premise correction** below. The renumber target needs one owner word before Phase A. |
| **S5** | **Install never consults the index.** `physical_reference` leaves the install path entirely. | A digest-pinned identifier has nothing left to redirect. This is the offline fix *and* the decoupling that makes the migration orderable. |
| **S6** | Yanks surface at `ocx update`, never enforced at install. | Reproducibility outranks yank enforcement. A later advisory check is fine; a hard install gate is not — it would reintroduce a network dependency on the install path by the back door. |
| **S7** | The index is an indirection, not a content store: `logical name + tag → physical address + digest`. Image indexes (dispatch objects) only, never leaf manifests. | Restates `adr_oci_index_only_dispatch`; no change. Keeps a copied index ~6× smaller and keeps leaf manifests in the blob store where GC accounts for them. |
| **S8** | Target tree: `p/<logical-registry>/<ns>/<pkg>.json` (mapping only), `o/<host>/sha256/<hex>.json` (objects, keyed by origin), `c/<logical-registry>/index.json` (catalog). | `o/` deliberately mirrors `~/.ocx/blobs/<host>/sha256/<hex>` so copying an index object into the blob store is a move with no path translation. |
| **S9** | **One-way rule:** content-addressed index objects may be copied *into* the blob store, never the reverse. | GC lives on the blobs. See §NFR-GC. |
| **S10** | Invariants: (a) copy the whole tree → a valid locked index, resolvable offline; (b) copy `o/` alone → shareable across every index pointing at the same physical repo; (c) a plain/derived index and an indirected index differ **only** in `p/` and `c/`; their `o/` trees are byte-identical. | (c) is what makes a derived index and a published index the same artifact format — the spine of `adr_index_indirection`. |
| **S11** | Breaking is accepted. `index.ocx.sh`, the bot's schema and renderer, and the fleet all get rewired. CHANGELOG line, no compat window, no dual-form parsing. | Pre-1.0 policy. Note the scope limit in M below: *no dual-form parsing* is not *no dual publishing*. |

### §P — Premise correction on S4 (verified, needs one owner word)

S4's stated premise is **"only v1 ever shipped (ocx 0.3.7 writes it); 2 and 3 were never released."**
That is false for v2, and the error is load-bearing.

Evidence:

- `v0.4.3` is a released tag. `git show v0.4.3:crates/ocx_lib/src/project/lock.rs` documents
  `LockVersion::V2` as *"the only written shape"* and `LockMetadata.lock_version` as *"Written locks are
  always `LockVersion::V2`; `V1` is accepted on read"*.
- The census finds **24 v2 locks on disk**, 9 stamped `generated_by = "ocx 0.4.3"`, one written
  **2026-07-29** (`ocx-contrib/mirror-kitware/ocx.lock`).
- v3 is the unreleased shape on `main` (`SUPPORTED_LOCK_VERSION: u8 = 3`), reached in
  `9978cc22 feat!: unified platform model — one relation, one grammar, lock V3`. **v3** is the version
  that was never released; **v2 shipped and is the fleet's current format.**

Why it matters: released-v2 and proposed-new-v2 have the **identical on-disk shape** — `repository`
plus `[tool.platforms]`. Only the *meaning* of `repository` differs (logical vs physical). Numbering
both `2` makes them indistinguishable, and both parse. A new ocx reading a 0.4.3-written lock would
treat a logical name as physical and, per S5, has no index deref left to rescue it → 404.

Blast radius **today is zero**: the census found no locked `repository` matching any redirecting root in
`/home/mherwig/dev/index/p/*/*.json`. Every on-disk lock names a package that resolves identically
logical or physical. So the collision is latent, not live — but it becomes live the first time any repo
locks a redirected package while still on a released 0.4.3, which is the normal state of the fleet
during Phase A.

Two safe resolutions, both one line:

1. **`lock_version = 4`.** Monotone, no collision, costs nothing but the aesthetic S4 was buying.
2. **Keep `2`, and gate on `generated_by`** — rejected as fragile: `generated_by` is a free-text
   string, and a dev build of `main` today already stamps `ocx 0.4.3`.

Recommendation: **4**. Recorded here rather than decided, because S4 is owner-settled and the premise —
not the preference — is what changed.

---

## Decision M — Migration ordering

**Three phases, in this order. No step leaves a published package unresolvable.**

### Phase 0 — Prerequisite: the update check (see §U)

**Ships:** timeout + `--index`/`OCX_INDEX` respect + removal of the `is_terminal()` gate on
`update_check::check_for_update` (`app.rs:182-187`). Tracked under
[#42](https://github.com/ocx-sh/ocx/issues/42), not this ADR.

**Still broken:** everything else. Nothing is fixed by this step alone.

**What makes the next step safe:** without it, Phase A's acceptance criterion cannot go red. The check is
gated on `is_terminal()`, so a piped test never executes it; a warm-store/no-network test would pass
whether or not the lock defect were fixed. That is the "unchecked green" pattern from `quality-core.md`
— a check whose passing state is indistinguishable from it never having run. Phase A must be verifiable
before it is shipped.

### Phase A — Lock semantics (`ocx` only, then the fleet)

**Ships:** `resolve.rs:516` writes the physical target; `LockedTool.repository` is physical; `lock_version`
bumped (§P); `resolve_transport_pinned`'s `physical_reference` call removed from the install path
(`package_manager/tasks/resolve.rs:409`); CHANGELOG line. Release as `ocx 0.5.0`. Then `setup-ocx` cuts a
release defaulting to it, then repos re-lock in the order of **Decision R**.

**Still broken at this point:** the index wire layout is unchanged — `o/` is still package-local, `p/`
still has no `<logical-registry>` segment, index objects are not yet blob-store-shaped. A repo that has
not re-locked still holds a logical `repository`, and once it bumps its ocx pin it must re-lock in the
same commit (its old lock_version is rejected outright).

**What makes the next step safe:** after Phase A the lock contains **no index-derived indirection at all**
(S5). The index's layout becomes invisible to every locked project, so Phases B and C cannot break a
locked build no matter how they land. This is why Phase A goes first even though the layout reshape is
the larger change.

**Why the fleet only re-locks once across the whole migration:** Phases B and C change index *paths*, not
physical *addresses*. A lock records addresses. Nothing in `ocx.lock` is a function of the index layout
after S5.

### Phase B — Index **source**-tree reshape (`ocx-sh/index` + `ocx announce`, lockstep)

**Ships:** the committed source tree moves to `p/<logical-registry>/<ns>/<pkg>.json` +
`o/<host>/sha256/<hex>.json` (a `git mv` plus the path-builder updates); the renderer keeps emitting the
**current served layout** unchanged. ocx's announce path map (`announce/pipeline.rs build_files`) flips in
the release preceding the reshape; the six index-side path-construction sites converge on the single
`cas_relpath` fix point (`validate_entry.py:418-426`) — `announce.py:102,107`, `reconcile.py:82,125,162`,
`classify_pr.py:104`, `validate.py:165-180`, `render.py:118-128` currently duplicate it by hand.

**Still broken:** nothing client-visible. Served URLs are byte-identical before and after; every released
ocx, including the fleet's, is unaffected.

**What makes the next step safe:** the two-writer risk is fully discharged here, in a window where no
client can observe a mistake. The byte-agreement contract on root *documents* is untouched — S7 restates
existing behaviour, so `wire_writer.rs`'s `PythonJson` serializer and the vendored conformance fixtures
need no change. Only the **path map** moves. Per the census, exactly one repo (`mirror-bazelbuild`)
currently runs announce, so the coordination surface is one pin bump and a same-day window.

### Phase C — Index **served**-tree flip (`ocx-sh/index` renderer + `ocx` reader)

**Ships:** the renderer emits the new served shape under a new base — `https://index.ocx.sh/v2` — while
continuing to render the current tree at the current base. ocx changes one constant
(`DEFAULT_INDEX_BASE_URL`) to the new base and ships the new reader. Both trees are rendered from the
same source tree, so they cannot diverge.

**Still broken:** an old ocx on the old base cannot see the new served shape and vice versa — by design,
each client parses exactly one form. Dual *rendering* is not dual *parsing*; S11 forbids the latter, and
the former is ~688 KB of static files.

**Why a new base rather than an in-place flip:** only one file genuinely collides — `config.json`, whose
`format_version` is read by `check_format_version` at a fixed `<base>/config.json` and refused if
unrecognised. Every other path already differs under S8. A single path prefix is cheaper than special-
casing one file, and it makes the eventual retirement a directory delete rather than a surgical one.

**Retirement:** once the fleet and `ocx` are wholly off the old base, freeze the old tree, then delete it,
with the CHANGELOG line. Until then the old tree keeps being rendered so newly announced versions remain
visible to un-migrated repos — the CLAUDE.md exception is "already-published packages keep resolving",
and dual rendering discharges it with room to spare.

### Orderings considered and rejected

#### Loser 1 — "Flip `index.ocx.sh` in place first, ocx follows"

Rewrite the served tree to the new layout at the current base; ship the new ocx afterwards.

| Pros | Cons |
|---|---|
| One tree, no prefix, no dual render | **Deadlock.** Every released ocx — 0.4.3, installed by a SHA-pinned `setup-ocx` in 43 repos — 404s on `p/<ns>/<pkg>.json` and hard-fails `check_format_version` (an unrecognised `config.json` is a fail-loud error by design, `indices.md:198`). The fleet's CI breaks *before* it can bump its pin, and bumping the pin in 43 repos requires CI that works. |
| | Violates the CLAUDE.md read-path exception outright — published packages stop resolving for every un-migrated client simultaneously. |

Loses on the bootstrap constraint. There is no recovery ordering from inside the deadlock.

#### Loser 2 — "ocx first, index later"

Ship an ocx that reads the new layout before the index serves it.

| Pros | Cons |
|---|---|
| Single ocx release carries everything | New ocx is dead on arrival — nothing serves what it reads. |
| | Announce writes new-shape paths into a repo whose bot validates old-shape paths → one rejected PR per package, exactly the two-writer failure the drivers name. |
| | Forces the lock fix to wait behind a three-repo wire migration, leaving `exit 69` live on every stock install for the duration. |

Loses on the two-writer constraint and on time-to-fix for the actual defect.

#### Loser 3 — "one release, both breaks"

Bundle lock semantics and the layout reshape into a single ocx release and a single index deploy.

| Pros | Cons |
|---|---|
| One fleet re-lock, one coordination window | The re-lock count is the *same* either way — S5 means the layout change never touches lock contents. The bundle buys nothing. |
| | Couples an urgent two-file fix to a three-repo migration; every schedule slip in the index repo keeps `exit 69` shipped. |
| | Collapses two independently verifiable steps into one, so a regression in either is diagnosed against a diff spanning both. |

Loses because the claimed benefit does not exist.

---

## Decision C — `c/` stays logical-keyed

**Decision:** `c/<logical-registry>/index.json`, keyed by logical `<ns>/<pkg>`. Sharded by logical
registry, never by physical host.

**Rationale, tied to who reads it:**

- `ocx index catalog` and the website catalog view are **discovery** surfaces. A user or a script asks
  "what can I install from `ocx.sh`?" and types a logical name. A physical-keyed catalog answers a
  question nobody asks, and cannot answer the one they do.
- `ocx index update`'s change detection diffs the catalog's per-package validator (today: sha256 of the
  root's own bytes) to decide which roots to re-snapshot. The unit of re-snapshot is a root, and roots
  are logical-keyed. Physical keying would require a reverse map to use the catalog at all.
- **S10(c) requires it.** A plain/derived index and an indirected index must differ *only* in `p/` and
  `c/`, with byte-identical `o/`. That invariant is only expressible if `c/` is the logical view — it is
  precisely the part that differs *because* it is logical. A physical-keyed `c/` would be identical
  across both index kinds and the invariant would have nothing to say.
- One physical repository may back **two** logical names (a rename, an alias, a shared upstream mirror).
  Physical keying makes that unrepresentable; logical keying makes it two entries pointing at one `o/`
  object — which is the shareability S10(b) is for.

`o/` is the only host-keyed tree, and it is host-keyed for exactly one reason: byte-compatibility with
`~/.ocx/blobs/<host>/sha256/<hex>` (S8).

---

## Decision R — Who re-locks, in what order

Governing rule: **the ocx pin bump and the re-lock ride the same commit.** A new ocx rejects an old
`lock_version` outright, so a repo that bumps without re-locking is broken at step one. A repo that bumps
neither is fine indefinitely — its lock is digest-pinned and install never derefs (S5).

Order is producers-before-consumers:

| Wave | Repos | Why here |
|---|---|---|
| **1** | `ocx` / `ocx-sion` / `ocx-soraka` (worktrees, `lock_version 1`), `test/` | Dogfood. OCX's own toolchain (`go-task`, `shellcheck`, `shfmt`, `lychee`, `bun`, `uv`, `git-cliff`) must survive the format it is shipping. `direnv exec . ocx …` exercises the new lock on every subsequent task run. |
| **2** | `setup-ocx` (v2), `www-setup` (v2) | Every other repo bootstraps ocx through `setup-ocx`. It must default to the new release before anyone can pin it. Cut `setup-ocx` v1.4.0 here. |
| **3** | Pilots: `mirror-astral-sh`, `mirror-bazelbuild`, `mirror-kitware` and their `ocx-contrib` twins (all v2, `generated_by ocx 0.4.3`) | Already on the current released ocx, so the diff isolates the format change. `mirror-bazelbuild` is the only announce-runner in the fleet — it doubles as the Phase B canary. |
| **4** | `ocx-mirror` (+ its `external/ocx` submodule bump), `ocx-mcp`, `ocx-mirror-sdk`, `rules_ocx`, `find_ocx`, `grimoire-index`, `mirror-pypi`, `kate-middlechild` | Consumers with their own CI and their own opinions about the ocx version. `find_ocx`/`ocx-mirror-sdk`/`rules_ocx` float on `setup-ocx@v1` and pick the new release up automatically once wave 2 lands — verify, do not assume. |
| **5** | The remaining ~40 `ocx-contrib/mirror-*` (33 at `lock_version 1`, `generated_by ocx 0.3.6`) | Passive, digest-pinned, all pinning the same `setup-ocx` SHA (`de8e3366…`). One scripted commit shape: bump the pin, `ocx update`, commit. No urgency — a stale lock keeps working. |

Waves 4 and 5 may lag indefinitely. Waves 1–3 are the migration.

**Cost:** ~53 pull requests, of which ~40 are one scripted commit shape. Two known traps, both already
recorded: a lock written by a newer ocx is rejected by the bootstrap ocx `setup-ocx` installs (so wave 2
strictly precedes waves 3–5), and a breaking spec change must ride the relock commit rather than split
from it.

---

## Decision E — `[registries."<ns>"].index = ""` stays

**Decision:** keep it, and stop describing it as an escape hatch. It is the **source-kind selector** for a
namespace, and always was:

- `index = "<url>"` → the namespace is served by an **index source**: names are logical, roots are
  dereferenced, the index is authoritative and fails loud.
- `index = ""` → the namespace is a **plain OCI source**: names are physical, resolution goes to the
  registry's tags API, no root deref exists.

What changes is only its *reach*. After S5, `index = ""` has no effect on install at all — install has
nothing to deref either way. It continues to govern `ocx update` and unpinned `ocx install <name>:<tag>`
(tag → digest), `ocx index catalog` / `ocx index update`, `ocx announce`, and `ocx describe`.

**Why not delete it.** Removing it leaves no way to say "`ocx.sh` is a plain registry for me". Two real
populations need that sentence: an air-gapped site that mirrors the *registry* but not the *index*, and
anyone who wants `ocx.sh` names resolved without a second host in the trust path. The only other opt-out
is pinning the namespace at a `[mirrors]` registry endpoint, which is a different intent (redirect, not
replace) and is not always available. Deleting a documented config key to remove a workaround for a bug
that is now fixed would be trading a real capability for a cosmetic one.

**Documentation follow-up:** any prose that frames `index = ""` as a remedy for hangs or resolution
failures is describing the defect, not the feature. Rewrite it as source selection. Affected files:
`website/src/docs/reference/configuration.md`, `website/src/docs/user-guide.md`,
`website/src/docs/in-depth/indices.md`.

---

## Decision U — The update-check defect is a separate issue, sequenced as Phase 0

**The defect.** `app.rs:182-187` runs `update_check::check_for_update` before dispatch on
`run`/`env`/`direnv`/`launcher`/`pull`. It builds a fresh remote index that bypasses `--index` /
`OCX_INDEX`, and it carries **no HTTP timeout** — `OcxIndex` bounds itself at 30 s / 60 s
(`ocx_index.rs:110,116`), but this path does not go through it. It is gated on `is_terminal()`, so piped
tests never execute it. Against a blackholed network, `ocx direnv export` was killed at 25 s without
returning.

**Decision:** **out of scope for this ADR's decision set; in scope as Phase 0 of its sequence.** Fold it
into [#42](https://github.com/ocx-sh/ocx/issues/42) (unified freshness/update-check strategy with TTL
caching), cross-referencing [#159](https://github.com/ocx-sh/ocx/issues/159) for the direct
`remote_client()` construction.

**Justification for separating it:**

- **Different mechanism, different fix.** The lock defect is a *resolution* dependency on the network,
  fixed by changing what a persisted format records — a wire-contract change with a three-repo blast
  radius. The update check is an *advisory* background call, fixed by a timeout, a routing correction,
  and deleting a terminal gate. Nothing about it is a format question.
- **Different lifecycle.** Its fix ships standalone, needs no coordination with `ocx-sh/index` or the
  fleet, and is not gated on any decision here. Bundling it would put a three-line fix behind a
  multi-repo migration.
- **Same property, though.** Both are violations of "offline must work". Recording them as one ADR would
  imply one fix; recording the property once and the mechanisms separately is the accurate shape.

**Justification for sequencing it first anyway:** Phase A's acceptance criterion is a warm store with a
blackholed network exiting 0. With the update check unfixed that test either hangs or — under `pytest`,
which pipes stdout — never runs the offending code, so it goes green regardless of whether Phase A
landed. Phase A would ship with a check that cannot go red. Fix the gate before writing the test.

Three sub-defects, all worth naming in the issue: (1) no timeout; (2) bypasses `--index`/`OCX_INDEX`;
(3) `is_terminal()` gating makes the whole path untested and untestable in CI.

---

## Consequences

**Positive:**

- `ocx run` / `ocx env` / `ocx pull` work on a warm store with zero network, on a stock install. Product
  Principle 2 becomes true rather than aspirational.
- The install path loses an entire network dependency and an entire failure mode. `physical_reference`
  is no longer reachable from install at all — the strongest form of "cannot regress".
- A lock becomes self-contained: it can be read, audited, and executed without an index, a config file,
  or a network. `ocx.toml` + `ocx.lock` fully determine what gets fetched from where.
- The index's job shrinks to what S7 says it is, which makes S8's tree — and the copy/share invariants of
  S10 — expressible.
- Index objects become movable into the blob store with no path translation (S8), so a shipped index and
  a warm blob store stop being two separate provisioning steps.

**Negative:**

- The lock is tied to the physical host. Mitigated by `[mirrors]` rewriting below the lock (§Industry
  Context), but a package whose backing registry *migrates* now requires a re-lock, where before it did
  not. That is the price of S1 and it is charged knowingly: a migration should be a visible diff, not a
  silent redirect on someone's next CI run.
- ~53 repos re-lock. Mechanical, but real.
- Two index trees are served for the duration of Phase C.
- Yank enforcement weakens to advisory at install (S6). Accepted: reproducibility outranks it, and a hard
  gate would smuggle the network dependency back onto the install path.

**Risks:**

- **`lock_version` collision (§P).** Highest-severity item in this ADR. Mitigation: renumber to 4, or
  confirm the collision is acceptable given zero live blast radius. Needs an owner word before Phase A.
- **Two-writer PR storm in Phase B.** Mitigation: the reshape is one commit containing both the `git mv`
  and every path-builder update, with the byte gate and `schema:validate:rendered` running on the new
  paths in the same commit. Converge the six duplicated path builders on `cas_relpath` first, so there is
  one place to be wrong.
- **A repo bumps its ocx pin without re-locking.** Fails closed with `UnsupportedLockVersion` naming the
  found value — loud, actionable, no silent misresolution.
- **A repo re-locks while the index root's `repository` is mid-migration.** The lock captures whatever the
  root said at that moment. Acceptable: that is what a lock is, and `ocx update` re-captures.
- **Fixture drift between the two serializers.** `crates/ocx_lib/tests/fixtures/index_wire/**` is vendored
  from `ocx-sh/index@<SOURCE_COMMIT>` and only re-synced weekly by `test:index-conformance-drift`. Phase B
  must re-vendor in the same window, and the known-ahead fixture `root/with-variants.json` must be
  reconciled rather than assumed correct.

---

## Non-Functional Requirements

### NFR-Offline — the property this ADR exists for

**Requirement:** warm store, zero network, stock config → `ocx run`, `ocx env`, `ocx pull`,
`ocx direnv export` all exit 0.

**Acceptance, red and green both demonstrated** (`quality-core.md` "Unchecked Green"):

| | Before Phase A | After Phase A |
|---|---|---|
| Warm store, network blackholed, stock config | exit **69** | exit **0** |
| Warm store, network blackholed, `index = ""` | exit 0 | exit 0 (unchanged) |
| Warm store, network blackholed, `--offline` | exit 0 | exit 0 (unchanged) |

The test must run the code it claims to test: the `is_terminal()` gate on the update check (Phase 0)
means a piped harness executes a different program than a user does. Either the gate goes, or the test
runs under a pty. A green obtained by not reaching the code is not a green.

**Mutation proof required:** revert `resolve.rs:516` to write the logical identifier, rerun, observe 69.
If it stays green, the test is not discriminating and a second guard is hiding the defect — keep mutating
until one reds.

### NFR-GC — correctness of the reachability graph

The local index tree is **outside** the GC reachability graph today: `ReachabilityGraph` lists only
`packages`, `layers`, `blobs` (three `CasTier` variants); `IndexStore` is not a tier and is never walked
(`adr_index_indirection` B1). S8/S9 must not change that.

- **S9's one-way rule is the invariant that preserves it.** Copying an index object *into* the blob store
  creates a blob the graph already knows how to account for — it enters as an ordinary
  `blobs/<host>/sha256/<hex>` entry with the existing retention edges from leaf blobs to parent image
  indexes. Copying *out* would create an index-tree object with no accounting owner and no tier, i.e. a
  leak the graph is structurally unable to see.
- **S8's shard shape is a real delta, not a cosmetic one.** `IndexStore::dispatch_object_path` builds
  `o/{algo}/{hex}.json` inline (`index_store.rs:367-374`) — one flat hex directory plus a `.json`
  suffix — while `BlobStore::path` uses `cas_path::cas_shard_path`'s three-component
  `{algo}/{hex[0..2]}/{hex[2..32]}` (`cas_path.rs:46-52`). S8's "a straight move with no path
  translation" is only true once the index side calls `cas_shard_path` too. Phase C must converge them,
  or S8's stated benefit does not exist. Flagged as an implementation gate, not an open question.
- `chained_index.rs:315-355 recover_absent_dispatch` already crosses from an index-shaped lookup into the
  blob store's physical, digest-keyed space, verifies the digest, and removes the blob on mismatch. It is
  the existing precedent and needs no new mechanism — only the path convergence above.

### NFR-Fleet — migration cost

| Metric | Value | Source |
|---|---|---|
| Lock files to migrate | 64 (40 at v1, 24 at v2) | census |
| Repos requiring a PR | ~53 | census |
| Repos on one scripted commit shape | ~40 (`ocx-contrib/mirror-*`, all pinning `setup-ocx@de8e3366…`) | census |
| Repos running `announce` (Phase B coordination surface) | 1 (`mirror-bazelbuild`) | census |
| Re-locks per repo across the whole migration | **1** | S5 — Phases B/C do not touch lock contents |
| Repos that may lag indefinitely | waves 4–5 (~48) | digest-pinned; install never derefs |

The dominating cost is wave 5, and it is scriptable to a single commit shape. The dominating *risk* is
wave 2 ordering: a repo whose bootstrap ocx predates the format cannot read a lock written by the new
one, and fails before step one.

---

## Technical Details

### Target index tree (S8)

```
<index root>/
  config.json                                  format_version, name_segments
  p/<logical-registry>/<ns>/<pkg>.json         mapping only: repository (physical) + tags{tag→digest}
  o/<host>/sha256/<hex>.json                   dispatch objects, keyed by ORIGIN host
  c/<logical-registry>/index.json              tag/catalog overview, LOGICAL-keyed (Decision C)
```

`o/` mirrors `~/.ocx/blobs/<host>/sha256/<hex>` so copying an object into the blob store is a move.
Requires the shard-shape convergence noted in NFR-GC.

### Resolution pipeline, before and after

```
BEFORE — install dereferences (the defect)
  ocx.lock repository (LOGICAL)
    → physical_reference → resolve_root → check_format_version   GET <index>/config.json   ← network, no fallback
    → physical address
    → mirror_map
    → GET <physical>/v2/.../manifests/<leaf-digest>

AFTER — install has nothing to dereference (S5)
  ocx.lock repository (PHYSICAL)
    → mirror_map
    → GET <physical>/v2/.../manifests/<leaf-digest>

`ocx update` / unpinned install — unchanged, and the ONLY place indirection is resolved
  ocx.toml identifier (LOGICAL)
    → index: GET p/<logical-registry>/<ns>/<pkg>.json → tags[tag].content
    → GET o/<host>/sha256/<hex>.json (verify sha256 of bytes) → select platform
    → write ocx.lock: repository = root.repository (PHYSICAL), platforms = leaf digests
```

### Lock data model

```toml
[metadata]
lock_version = 2            # or 4 — see §P
declaration_hash_version = 1
declaration_hash = "sha256:…"
generated_by = "ocx 0.5.0"
generated_at = "…Z"

[[tool]]
name = "python"             # ocx.toml binding key — joins back to the LOGICAL name
group = "default"           # ocx.toml table — the other half of the join key
repository = "ghcr.io/ocx-contrib/astral-sh/python-build-standalone"   # PHYSICAL, scheme stripped

[tool.platforms]            # unchanged
"linux/amd64" = "sha256:…"
"darwin/arm64" = "sha256:…"
```

---

## Implementation Plan

**Phase 0** — [#42](https://github.com/ocx-sh/ocx/issues/42), independently shippable

1. [ ] Bound `update_check::check_for_update` with an explicit timeout; route it through the context's
       index so `--index` / `OCX_INDEX` are honoured; drop the `is_terminal()` gate (or make it
       test-overridable) so CI executes the path.

**Phase A** — `ocx`, then the fleet

2. [ ] `resolve.rs:516` writes the resolved physical target; drop the discard of the resolve output.
3. [ ] Remove `physical_reference` from the install path (`package_manager/tasks/resolve.rs:409`); assert
       structurally that install cannot reach it.
4. [ ] Bump `SUPPORTED_LOCK_VERSION` (§P) and the `LockVersion` enum; delete the unreleased-format read
       paths per S11 — no migration code.
5. [ ] Offline acceptance test per NFR-Offline, red and green both demonstrated, plus the mutation proof.
6. [ ] CHANGELOG line. Release `ocx 0.5.0`. Cut `setup-ocx` v1.4.0.
7. [ ] Re-lock in the wave order of Decision R; pin bump and re-lock in the same commit.

**Phase B** — `ocx-sh/index` + `ocx announce`, lockstep

8. [ ] Converge the six index-side path builders on `cas_relpath` (`announce.py`, `reconcile.py`,
       `classify_pr.py`, `validate.py`, `render.py` → `validate_entry.py:418-426`).
9. [ ] Ship ocx's new announce path map (`announce/pipeline.rs build_files`) in the release preceding the
       reshape.
10. [ ] One commit in `ocx-sh/index`: `git mv` the source tree to S8's shape, update `cas_relpath`, keep
        the renderer emitting the current served layout, update `taskfile.yml`'s depth-bound `find`
        invocations and the golden render fixtures.
11. [ ] Bump `mirror-bazelbuild`'s ocx pin in the same window; accept a short announce stall.

**Phase C** — served flip

12. [ ] Renderer emits the new served shape under `/v2` alongside the current tree; update
        `schema/root.schema.json`, `image-index.schema.json`, `c-index.schema.json` docs/titles and
        `format_version`.
13. [ ] Converge `IndexStore::dispatch_object_path` on `cas_path::cas_shard_path` (NFR-GC), then point
        `DEFAULT_INDEX_BASE_URL` at the new base and ship the new reader.
14. [ ] Re-vendor `crates/ocx_lib/tests/fixtures/index_wire/**` and reconcile `root/with-variants.json`.
15. [ ] Rewrite `bot/CONTRACTS.md`, `site/src/docs/reference/wire-format.md` ("The Four Frozen URL
        Shapes"), `entry-schema.md`; update `site/src/[ns]/[pkg].paths.ts` walk depth + watch glob and
        `theme/utils/cas.ts`'s runtime CAS URL.
16. [ ] Rewrite the `index = ""` framing in `website/src/docs/{reference/configuration.md,user-guide.md,
        in-depth/indices.md}` per Decision E; update the resolution-pipeline diagram in `indices.md`.
17. [ ] After the fleet is off the old base: freeze, then delete it. CHANGELOG line.

---

## Validation

- [ ] Offline acceptance test passes **and has been observed red** with `resolve.rs:516` reverted.
- [ ] The offline test executes the update-check code path (no `is_terminal()` short-circuit hiding it).
- [ ] Structural assertion: no install-path call reaches `physical_reference`.
- [ ] `UnsupportedLockVersion` fires with the found value for every unsupported number, including the
      chosen renumber target's neighbours.
- [ ] Phase B: index-repo byte gate and `schema:validate:rendered` green on the reshaped tree; served
      URLs byte-identical before and after.
- [ ] Phase C: `o/<host>/sha256/<hex>.json` copies into `~/.ocx/blobs/<host>/sha256/<hex>` with **no path
      translation** — demonstrated by an actual move, not by inspection.
- [ ] S10 invariants demonstrated: whole-tree copy resolves offline; `o/`-only copy is shared across two
      indices; a derived and an indirected index have byte-identical `o/`.
- [ ] GC accounts for a copied-in index object as an ordinary blob; no index-tree object is ever created
      from a blob (S9).
- [ ] Security review of the physical-address path: SSRF floor still enforced where `physical_identifier`
      enforced it, now that install no longer passes through it.

---

## Links

- [`adr_index_indirection.md`](./adr_index_indirection.md) — the index's format and the logical/physical
  split this ADR amends for `ocx.lock`
- [`adr_oci_index_only_dispatch.md`](./adr_oci_index_only_dispatch.md) — dispatch-only objects (S7)
- [`adr_platform_model_unification.md`](./adr_platform_model_unification.md) — per-platform lock map
- [`adr_index_routing_semantics.md`](./adr_index_routing_semantics.md) — `IndexOperation` × `ChainMode`;
  the pinned-id contract this ADR completes
- [`adr_public_index_registry_indirection.md`](./adr_public_index_registry_indirection.md)
- `.claude/rules/subsystem-oci.md`, `.claude/rules/subsystem-file-structure.md`
- `website/src/docs/in-depth/indices.md` — resolution pipeline, `index = ""`, two-hop caching
- [Nix flake references and the flake registry](https://nix.dev/manual/nix/stable/command-ref/new-cli/nix3-flake) —
  locked vs unlocked flake references
- [npm `package-lock.json` — `resolved` / `integrity`](https://docs.npmjs.com/cli/v10/configuring-npm/package-lock-json)
- [Cargo — registry index format and `Cargo.lock`](https://doc.rust-lang.org/cargo/reference/registry-index.html)
- [Go modules — `go.sum` and `GOPROXY`](https://go.dev/ref/mod)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-07-29 | Claude (architect) | Initial draft — records S1–S11, adds M/C/R/E/U, flags §P |
