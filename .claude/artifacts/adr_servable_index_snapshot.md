# ADR: A Local Index Snapshot Is Directly Servable

## Metadata

**Status:** Accepted — decision A ratified by the owner 2026-08-09 (uniform rule), reversing an
earlier ratification; decisions B–F settled.
**Date:** 2026-08-09
**Deciders:** mherwig, architect
**Beads Issue:** N/A
**Related issues:** [#215](https://github.com/ocx-sh/ocx/issues/215) (the local copy must be
self-contained)
**Tech Strategy Alignment:**
- [x] Golden Path: Rust 2024 / Tokio, no new dependency. One `IndexTransport` impl over `tokio::fs`,
  two verbs, one write inside an existing transaction, and a **net deletion** in the
  version-gate path.
**Domain Tags:** oci, index, wire-format, security, devops, cli
**Supersedes:** N/A
**Superseded By:** N/A
**Amends:** [`adr_index_indirection.md`](./adr_index_indirection.md) F1 (`config.json` gains a writer,
a local reader, and a **changed absent-semantics**), F5a/F5c (the index base URL admits a `file://`
scheme), and the 2026-08-05 amendment's "there is no whole-index sync" clause (narrowed, not deleted).
**Depends on:** [`adr_oci_index_only_dispatch.md`](./adr_oci_index_only_dispatch.md) (D3's invariant
and D6's clause map — the authority for the distributable-tree property),
[`adr_index_indirection.md`](./adr_index_indirection.md) (wire grammar; A1/A4/B/C2/C3/E/G/H untouched
per `adr_oci_index_only_dispatch.md:328`)

---

## Context

### What is already settled, and what is genuinely new

Two properties this design needs are **existing doctrine**:

- **A copied subtree resolves without knowing where it came from.** `adr_oci_index_only_dispatch.md`
  D3 (`:214-216`): *"Every tag in the index points at an OCI image index; that index is present in
  `o/`; and its bytes are byte-identical to what the registry served… no bot-authored content sits in
  the resolution path."* Consequences (`:535`): *"**Decode is unconditional**, so a copied subtree
  resolves without knowing where it came from."* Validation (`:749-750`): a hosted subtree copied
  verbatim into `$OCX_HOME/index/<any-source>/` resolves offline with no source-kind configuration.
- **A partial copy is the blessed normal case** (`adr_index_indirection.md:48-51`). This lives in
  `## Context`, so no lettered decision — and no D6 row — reaches it. A curated mirror holding 42 of
  4000 packages needs no new blessing.

**Cite D3/D6 for this, not A2.** D6's A2 row (`adr_oci_index_only_dispatch.md:314`) supersedes only
the *"self-verifying, no OCX-minted metadata"* claim **about `o/` objects**, overstated while
published sources stored a bot-synthesized object; post-D6 it *"Holds unconditionally"*. A2's decision
sentence (`adr_index_indirection.md:225-226`) is not in the superseded list.

> **Hygiene, in scope.** `adr_index_indirection.md:223` carries a bare
> `> superseded — see adr_oci_index_only_dispatch.md D6` under the A2 heading with no qualifier — more
> than D6's own table row (`:314`) and the metadata line (`:22`) claim, and the reason A2 reads as
> wholly retired. Scope it to the published-index bullet (`:239-245`). Not needed at `:268` (A3),
> which D6 does supersede at the headline level.

**Genuinely new:** neither ADR addresses **OCX serving its own tree over HTTP to other OCX clients**.
Both describe OCX as the *consumer* of someone else's hosted index. The Location axis
(`adr_index_indirection.md:37-41`) covers copying a tree anywhere it can be *read* — never a machine
turning around and serving one. That is this ADR's new ground.

### The defect: the equivalence already committed to stops one directory short

`adr_oci_index_only_dispatch.md:761-762` already asserts:

> `ocx index update` output for a published package is byte-identical to `wget --mirror` of the same
> subtree.

Scoped to the `p/<ns>/<pkg>` subtree. **`config.json` sits at the source root, outside that scope** —
which is why the gap survived. Nothing writes one: `IndexStore::source_config_path`
(`index_store.rs:303`) has zero non-test callers; `IndexFormatConfig` (`ocx_index.rs:86-104`) derives
`Deserialize` only.

Serve that tree and it reports **not found for every package it contains**, silently: `config.json`
404s → `NotAnIndex` (`ocx_index.rs:660`) → `jurisdiction` returns `Authoritative` without probing
(`:496-509`) → `resolve_root` returns `Ok(None)` **before** the root GET (`:681-683`). `Authoritative`
makes that miss terminal. **The change completes an equivalence the project already committed to.**

### What makes a tree resolvable versus enumerable

A tag-addressed resolve fetches, in order: `config.json` (`:645`), `p/<repository>.json` (`:687`),
`p/<repository>/o/<algo>/<hex>.json` (`:726-732`). `c/index.json` (`:834`) is fetched **only** by
`fetch_catalog`, reached only from `IndexImpl::list_repositories`.

> `config.json` makes a tree **resolvable**. `c/index.json` only makes it **enumerable**.

### The transport seam is already the right shape

`IndexTransport` (`:153-160`) is two methods. `StubIndexTransport` (`:1088-1144`) is the worked
example of a non-`reqwest` impl. **The `file://` blocker is the URL parser:**
`OcxIndex::resolve_base_url` (`:577-616`) routes through `config::mirror::parse_url`
(`mirror.rs:453-489`); `file:///srv/x` has an **empty authority** and fails `MissingHost`
(`mirror.rs:477-479`) → 78. That parser is shared with `[mirrors]`. Worse, the plain-HTTP gate is
`target.protocol == "http" && !insecure_hosts.contains(host)` (`:602`) — a `file` scheme matches
**neither arm**, so no gate fires. Go's `GOPROXY=file://` set this precedent and it bypassed GOSUMDB.

---

## Scope

**In (five items):**

1. `config.json` written at source-subtree creation; read on the local path.
2. The bulk snapshot — decided as `ocx index update --from-catalog <REGISTRY>`, shipped as
   `ocx index sync <REGISTRY>...` (Decision B).
3. `ocx index regenerate` — drift repair (Decision C).
4. `file://` `IndexTransport` (read-only) + the closed scheme gate.
5. ~~Advisory `min_ocx_version` field in `config.json`.~~ **Withdrawn** — owner decision, see
   Decision E.

Plus, pulled in by item 3's second consumer: **byte-exact serialization for `c/index.json` and
`config.json`** (Decision F). Treated as in scope on its own merits — a second implementation of a
frozen wire document with no parity test is a latent divergence whether or not `regenerate` ever runs
against the public index.

**Out:** signing; auth for private index mirrors; `[mirrors]` `file://` support; the later ocx-mirror
blob-copy + `repository`-rewrite phase (nothing here forecloses it — no storage path, lock, or GC root
gains a physical-location key); **and actually replacing `indexbot render`'s catalog step, which is
the index repo's decision, not ours** — this ADR only makes it possible.

---

## Decision Drivers

- **DR1.** Complete the `wget --mirror` equivalence at the source root.
- **DR2.** No check may be selected by which caller asked. Trust rules are uniform over bytes.
- **DR3.** Zero bytes added to any existing tree except the one file that was always meant to be there.
- **DR4.** No pin moves except under a command the user invoked naming what to move.
- **DR5.** Pre-1.0 clean break, no shims.

## Industry Context & Research

**Research artifacts:** [`research_index_wire_versioning_trust.md`](./research_index_wire_versioning_trust.md)
(decision A — full citations for the CVEs, the six-format survey, and the PEP 629/691 precedent),
[`research_servable_index_snapshot.md`](./research_servable_index_snapshot.md) (decisions B and D).

---

## Decision A — Absent `config.json` means version 1, for every reader

**Owner-ratified 2026-08-09, reversing the earlier provenance-gated ratification.**

> **Absent `config.json` ⇒ assume `format_version: 1` for EVERY reader — local and fetched, no
> provenance split. An unrecognized version ⇒ hard error (65) for every reader.**

### Considered options

#### A1: Provenance-gated leniency (previously ratified — rejected)

Absent ⇒ assume 1 **only** inside `LocalIndex`, because OCX authored those bytes; any fetched tree
stays fail-closed. Unrecognized ⇒ hard error on both.

| Pros | Cons |
|---|---|
| Preserves today's remote gate exactly | **CWE-501 Trust Boundary Violation by construction**: same bytes, same path, two readers with different checks selected by caller, safety resting on a guarantee the type system does not encode |
| Zero migration | The provenance premise is **false for the case this ADR ships**: `--index` / `OCX_INDEX` point at copies OCX did not author (`adr_index_indirection.md` A1; `subsystem-oci.md` calls the tree *"trusted deployment-managed input"* — an **operator's** act) |
| Matches "our own state" intuitively | Two 2026 CVEs are this exact shape — CVE-2026-5223 (Cargo's local extraction cache trusted as "state we wrote"; fixed by closing the **write** path unconditionally, not by a read-path check) and CVE-2025-36852 "CREEP", CVSS 9.4 (trusted and untrusted producers resolving to one cache key; fixed by **structural** namespace isolation, not caller-based distinction) |

#### A2: Uniform lenient — PEP 629/691 (**chosen**)

| Pros | Cons |
|---|---|
| One rule over bytes; **deletes** a code path rather than adding one | Loses today's refusal to consume roots from an endpoint that never declared itself an index (analysed below) |
| No surveyed sparse-index format splits trust by provenance — checked directly: crates.io, PEP 691, Debian, Alpine `apk`, Nix `nix-cache-info`, Go sumdb | The 404 arm changes meaning, so five match sites change at once |
| PEP 629/691 sets the precedent **for the untrusted network case**: *"if that data does not exist clients MUST assume that it is version 1.0"*, major mismatch ⇒ hard fail | |
| **OCX's own code already argues for it** — `check_format_version`'s doc (`ocx_index.rs:622-624`): *"Config-driven construction (`[registries."<ns>"].index` presence) already decided this host serves an ocx-index, so there is nothing left to probe for — this only guards the wire-format version."* | |

#### A3: Uniform strict — `config.json` required everywhere (rejected)

| Pros | Cons |
|---|---|
| Maximum integrity; one rule, no exceptions | **Flag day.** Every existing `$OCX_HOME/index/<source>/` unreadable until rewritten |
| Every subtree self-describing | Breaks **derived** sources by construction (`adr_index_indirection.md:249-250`: no `config.json`, catalog is directory enumeration) |
| | Refuses a pin the machine already committed — the opposite of the package-tier-lock doctrine |

### Weighted evaluation

| Criterion | W | A1 | **A2** | A3 |
|---|---|---|---|---|
| Trust-boundary integrity; no caller-selected check | 5 | 2 | **5** | 5 |
| Zero action for existing homes and derived sources | 4 | 5 | **5** | 1 |
| Net code paths (deletion preferred) | 3 | 2 | **5** | 3 |
| Ecosystem conformance | 3 | 2 | **5** | 3 |
| Preserves the "undeclared endpoint" refusal | 2 | 5 | **1** | 5 |
| **Total** | | **51** | **77** | **58** |

### What changes in the code — every site, named

`FormatVersionState` has two variants; under the uniform rule `NotAnIndex` has **no remaining
producer**. The enum is deleted and `check_format_version` returns `Result<Arc<IndexFormatConfig>>`.

| Site | Today | After |
|---|---|---|
| `ocx_index.rs:378-385` | `enum FormatVersionState { Confirmed(..), NotAnIndex }` | **Deleted.** Return type becomes `Result<Arc<IndexFormatConfig>>` |
| `:657-660` (producer) | `IndexFetch::NotFound => return Ok(NotAnIndex)` | Returns an assumed `IndexFormatConfig { format_version: 1, name_segments: None }` |
| `:494-506` (`jurisdiction`) | Three arms: `Confirmed`, `NotAnIndex => None`, `Err` | Two arms. The assumed config's `name_segments: None` already means "no declaration", so the deleted arm's behaviour is preserved exactly |
| `:678-683` (`resolve_root`) | Early `return Ok(None)` on `NotAnIndex` | **Deleted** — this is the behaviour change |
| `:829-833` (`fetch_catalog`) | Early `return Ok(CatalogIndex::new())` | **Deleted.** The existing `IndexFetch::NotFound` arm at `:836` already yields an empty catalog, so behaviour is unchanged |
| `:1017-1021` (`fetch_root_document`, the `index update` path) | Early `return Ok(None)` | **Deleted** |

Caching is unchanged: the assumed default is **not** memoized, preserving today's "re-checked every
call so a later-deployed `config.json` is picked up" property with no new reasoning
(`:657-659`).

> **Do not confuse enums.** `AliasState::NotAnIndex` (`package/cascade/graph.rs:236`) is an unrelated
> type meaning "this tag resolves to a bare image manifest". It is untouched.

The `AbsentConfig` parameter carried by an earlier draft of this ADR is **deleted** — with one rule
there is no question to parameterize. That is the "deletes a path" claim made concrete.

### What is lost, stated plainly

Today an endpoint that never declared itself an OCX index cannot have its roots consumed. After this
change, **any endpoint the user configured as an index and that serves a parseable root will
resolve.**

**What still distinguishes "misconfigured base URL pointing at an unrelated static site" from "index
at version 1"? Nothing — and that is acceptable because the user typed the URL.** Config-driven
construction *is* the declaration: an `OcxIndex` exists for a namespace only because
`[registries."<ns>"] index` names that base (F5a, `context.rs:706-726`), merged through the managed
tier with `system_locked` applying. There is no probing and never was.

What the removed check was **not** doing is worth stating, because it is what makes the loss small.
An unrelated static site must still, in sequence: serve a document at exactly `p/<ns>/<pkg>.json` that
deserializes as `IndexRoot` (requiring a `repository` field that passes the `oci://` parse via
`repository_check`), whose `tags[t].content` names a dispatch object it also serves, **whose bytes
hash to that digest** (`ocx_index.rs:738-746`). The `config.json` probe was a typo-catcher, not a
security control — and it caught the typo by breaking silently, which is the defect this ADR exists
to fix.

> **Correction — the digest verify is not a trust anchor.** An earlier revision of this ADR called
> `ocx_index.rs:738-746` "the index path's actual trust anchor". That is **false** and the claim is
> withdrawn. The `digest` fed to that comparison is read from `root.tags[t].content` — the *same*
> document whose bytes are being checked. It is a self-consistency check (it catches a truncated or
> corrupted dispatch object, and a server that swaps the object without rewriting the root), **not**
> provenance. On a fresh resolve no user-pinned digest enters the comparison; a hostile index that
> authors both documents passes it trivially. The index path's real trust boundary is the transport:
> TLS to a host the user configured, plus `[registries."<ns>"] index` being config-driven and
> `system_locked`-gated. Nothing in this ADR weakens or strengthens that. The loss is small for the
> reason stated above — the user typed the URL — and for no cryptographic reason.

### Acceptance-test impact: none

`test/tests/test_index_ocx_sh.py:525` `test_unsupported_format_version_fails_closed_registry_only_unaffected`
asserts exit **65** for `format_version=2` (`static_index.write_config(..., format_version=2)`,
`:532`). That is the *unsupported* path, which the uniform rule **keeps unchanged**. No acceptance
test asserts fail-closed behaviour for an **absent** `config.json`. **No security-shaped test
changes.** The suite's `config.json`-was-requested assertions (`:218`, `:297`, `:822`) also still
hold — the fetch still happens; only the meaning of its 404 changes.

### The unrecognized-version path, both readers

`format_version != SUPPORTED_FORMAT_VERSION` ⇒ `Error::UnsupportedIndexFormat` → **65
`DataError`**, on the fetched reader (unchanged) and on the local reader (**new** — `LocalIndex`
reads no `config.json` today, so a local tree declaring version 2 is currently mis-parsed as version
1). The comparison stays `!=`; `<=` is deferred to v2 (ratified). Contracts C-004/C-005.

---

## Decision B — ~~The bulk snapshot is a flag on `index update`~~ **Superseded: `ocx index sync`**

**Chosen at the time: `ocx index update --from-catalog <REGISTRY>`.** The `mirror` rename stays
rejected.

> **Superseded by the verb split.** `--from-catalog` shipped, then was promoted to
> **`ocx index sync <REGISTRY>...`** — variadic, with `--dry-run`, in
> `crates/ocx_cli/src/command/index_sync.rs`. The flag exists nowhere; `ocx index update` is again
> only `<PACKAGE>...`. The authority for the framing is C-012's post-merge amendment in
> [`design_spec_servable_index_snapshot.md`](./design_spec_servable_index_snapshot.md), which records
> two grounds. The first is grammatical: a flag that has to exclude its own verb's positionals costs
> three declarations — `conflicts_with = "packages"`, an `ArgGroup`, and `--dry-run`'s own
> `requires`/`conflicts_with` pair — whose entire job is keeping two commands apart inside one name.
> The second is ergonomic and recorded as such: a registry *list* had nowhere natural to sit while the
> positional slot belonged to packages, so multi-registry runs were expressible only by repeating the
> flag. Ergonomics is a sufficient reason for a grammar; it needed saying plainly. What did
> **not** motivate the promotion is parallelism. What keeps the ≤ 512 ceiling a property of the run
> rather than of the argument count is the **flatten** — every named registry's packages enter one
> bounded loop. Enumeration itself stays sequential, one request in flight, so the ceiling is neither
> the reason for that nor threatened by changing it; serialized enumeration is accepted because N is
> operator-sized and the run is dominated by the refresh, and its cost is that each dead host's
> timeout is paid in full, in series.
>
> B2 and B3 are not vindicated by this. The shipped verb is `sync`, not `mirror` or `snapshot`, and
> the grounds for rejecting those two names survive intact — a merge-never-deletes operation is not a
> mirror, and "mirror" already carries two other meanings in this project. `sync` carries its own
> naming debt instead, recorded in C-012: it overpromises to an rsync-shaped ear, so every
> user-facing surface that says `sync` also says the store is merge-only-never-delete.

| Option | Verdict |
|---|---|
| **B1** `--from-catalog <REGISTRY>` | **Chosen** |
| **B2** `ocx index mirror <REGISTRY>` (research recommendation) | Rejected — its case rested mainly on leaving `subsystem-oci.md` consequence 2 untouched; the owner has granted that amendment, so the benefit is void. Also: a merge-never-deletes operation is not a mirror, and it would be a third meaning of "mirror" beside `[mirrors]` and the `ocx-mirror` binary |
| **B3** `ocx index snapshot <REGISTRY>` | Rejected on the same grounds as B2 |
| **B0** `ocx --remote index catalog … \| jq \| xargs ocx index update` | Rejected: unbounded fan-out, no Windows story, requires `jq`, argv limits at fleet scale |

Three concrete costs sink the separate verb:

1. **`--frozen` refusal is inherited free** from the gate at `index_update.rs:50-56`, which runs
   before any fetch and before any source is constructed. A separate verb needs a duplicated policy
   gate — a place to forget it.
2. **One mental model.** `update <pkg>…` names the set explicitly; `--from-catalog <REG>` names it by
   enumeration. Same operation, same write path, same `RootScope::Package`.
3. **Identical fan-out** — the same `refresh_tags`-per-package loop. A separate verb is an alias or a
   duplicate.

> **What became of the three costs.** Cost 1 materialised and was paid rather than avoided: `ocx
> index sync` carries its own `--frozen` gate (`index_sync.rs:53-59`) beside `index update`'s
> (`index_update.rs:50-56`), so there really are two places to forget it. Each file pins its gate
> with a test named `exactly_one_frozen_gate` (`index_sync.rs:359`, `index_update.rs:139`) asserting
> the module's `Error::PolicyBlocked` count is **exactly** one — an equality, so it fails at zero as
> loudly as at two, which is what makes it a guard against the gate being deleted and not merely
> against a second one appearing. Cost 3 does not survive the shared loop: neither verb owns a
> fan-out, both call `index_common::refresh_packages` (`index_common.rs:87`), whose
> `buffer_unordered(INDEX_REFRESH_CONCURRENCY)` (`index_common.rs:116`, the constant is 8 at `:31`) is
> the one loop, so `sync` is neither an alias nor a duplicate but that loop fed from a catalog rather
> than from argv. Cost 2 is what the split traded away deliberately: two names now describe one
> operation, and an operator has to know which takes packages and which takes registries. The half
> that survives is the implementation's single model — same write path, same `RootScope::Package`,
> same bounded loop — not the surface's. The ceiling itself is unchanged and its outer half is now
> `pub` at `local_index.rs:34`, so the two citations in *Concurrency* below (`index_update.rs:65-85`
> for the `JoinSet`, `local_index.rs:26` for the constant) describe where that code stood when the
> decision was taken, not where it stands now.

**This is not the removed `--all`.** That flag's referent would have been the local copy's current
contents, an arbitrary set nobody chose. `--from-catalog <REGISTRY>` takes a **required registry
argument**; its referent is that source's catalog as enumerated at this instant — an explicit operator
act naming a set by naming the registry. Record the distinction in the amendment so a future review
does not re-flag it.

**Concurrency.** Argv is bounded in practice; a catalog is not. The flag expands to the same package
list and feeds **one** loop, bounded by `buffer_unordered(8)` replacing the unbounded `JoinSet` at
`index_update.rs:65-85`. Nested inside `TAG_REFRESH_CONCURRENCY = 64` (`local_index.rs:26`) the stated
ceiling is **≤ 512 in-flight requests**. One loop for both inputs is the smaller diff *and* the
correct one; it caps the pre-existing argv fan-out as a side effect (C-024).

---

## Decision C — `ocx index regenerate` exists for drift repair

**Not** promote-to-servable. The earlier cut argued `c/index.json` is incrementally correct because
`commit_published_root` upserts through `CatalogTransaction` — true only for a tree written
exclusively by one OCX through the normal path. A **served mirror tree** is exactly where that fails:
manual curation, `rsync`, a second person's OCX writing into a shared home, a git merge of two mirror
branches.

The drift is one-directional and currently unrecoverable:

| Drift | Recoverable today? |
|---|---|
| Catalog entry stale or missing, root present | **Yes.** `IndexStore::read_root` self-heals by re-deriving from root bytes (`CatalogEntryStatus::Recovered`, `index_store.rs:924-928`) |
| **Catalog lists a package whose root is gone** | **No — never.** `CatalogTransaction::write_root` only upserts (`:993-995`); `commit` writes the merged map (`:1029`); `read_root`'s recovery only adds. That entry is permanent and the catalog lies about the tree's contents from then on |

**Why this does not violate merge-only-never-delete — address this first, because a reviewer will
raise it.** `regenerate` is a **pure re-derivation of `c/index.json` from the `p/` walk**, not a
deletion. The tree is the source of truth; the catalog returns to being derived data rather than
persisted state masquerading as authoritative. No root document and no `o/` object is removed, ever.
The no-delete rule governs roots and objects — the things that carry pins.

Precedent inside the project: `ocx-sh/index`'s `indexbot render` already derives the served tree's
catalog from the available packages (`render.py:296-309`). This is the same operation, client-side.

### A verb, not a flag on `update`

Folding it into `update` would couple a purely local repair to a network fetch, with different failure
modes and a different policy posture. Rejected explicitly.

**`--frozen` permits `regenerate`. `--offline` is irrelevant to it.** The asymmetry with `index
update`'s exit-81 refusal looks wrong at a glance, so the reasoning is stated: `--frozen` freezes
**pin movement**. `regenerate` consults no source, moves no `tags[].content`, and touches no root's
`repository` — it rewrites one derived document from documents already on disk. Refusing it would
freeze a *repair* that cannot change what any tag resolves to. C-021.

### Naming: `regenerate`, not `generate`

`generate`'s precedent (`apt-ftparchive generate`, `createrepo`) describes **first-time derivation
from a pool**. This operation re-derives a catalog that already exists, from a tree that already
exists — a prior generation being regenerated. Recorded as considered-and-rejected with that
distinction.

### It does **not** write `config.json`

`regenerate` derives what is derivable. `c/index.json` is a function of the `p/` walk. `config.json`
is not: `name_segments` is an operator declaration OCX cannot derive from a tree (the index repo
supplies its own `NAME_SEGMENTS`, `render.py:335`). Writing `config.json` is solely the creation
hook's job (Decision E).

This also removes a real tension: `regenerate` must not fabricate an operator declaration for a
foreign tree it is merely repairing.

### Report — new design, not `index update`'s shape

There is no "report of changes" exemplar in the `index` family (`index_update.rs:67-73` explicitly
emits none), so this is new. An operator running it against a served mirror needs to know what changed:
entries **removed** (catalog listed a package whose root is absent), **corrected** (digest disagreed
with the root on disk), **added** (root present but unlisted). C-010.

`CatalogTransaction::commit` already no-ops on an unchanged map (`:1025`), so a clean tree costs
nothing and leaves mtimes untouched — which is what makes this safe to run on a schedule against a
served tree.

---

## Decision D — `file://` wiring

| Option | Verdict |
|---|---|
| **D1** Teach `parse_url` an empty authority | Rejected: shared with `[mirrors]`; a `file://` in the `registry` role would then parse and be dialled |
| **D2** Branch **before** `parse_url` in `resolve_base_url` (**chosen**) | Blast radius is the index role only; `parse_url` untouched, so `[mirrors]` keeps rejecting `file://` (C-020) |
| **D3** A separate `index_path` field | Rejected: F5a's "field presence is the kind marker" becomes ambiguous |

**The scheme set becomes closed and exhaustive** (C-018) — the fix for the missing gate arm: `https`
allowed; `http` gated by `OCX_INSECURE_REGISTRIES` as today; `file` allowed only with an **empty
authority and an absolute path**; anything else ⇒ `InvalidIndexUrl` (**78**) at context init rather
than a transport error (75) at first fetch.

## Decision E — `config.json` creation hook (and why there is no advisory version field)

**Hook: in `LocalIndex::commit_published_root`, after the transaction commits, write-if-absent.**

Two seams were rejected before this one, and the second was rejected only in review:

- **`lock_source` (`index_store.rs:332`)** does the `create_dir_all`, but fires on **every** catalog
  transaction and on the derived-root lock — a hook there would write into derived subtrees, which
  `adr_index_indirection.md:249-250` defines as having none, and would bury a wire-document write
  inside a generic locking primitive.
- **`CatalogTransaction::commit`** — this ADR's original choice, on the reasoning that `commit`
  already publishes the *other* source-root document under the already-held lock. **Withdrawn.**
  `commit` is a shared primitive, not the update path: its production callers are
  `commit_published_root` (`local_index.rs:722`) **and** `IndexStore::persist_recovered_catalog_entry`
  (`index_store.rs:553`, the `read_root` catalog self-heal), and Decision C's `regenerate_catalog`
  would have been a third. A hook there therefore contradicts Decision C directly — `regenerate`
  would inject `config.json` into the foreign tree it is merely repairing — and makes a *resolve*
  create a wire document as a side effect. The reasoning "commit already writes a source-root
  document" was sound; the premise "commit means an update happened" was not.

So the write sits one level up, in the update path itself, through a new `pub(crate)
IndexStore::ensure_source_config`. Verified reachable: `commit_published_root` calls
`transaction.commit()` **unconditionally** (`local_index.rs:722`) even when the merge is a no-op, so
the first `index update` for a published source always reaches the following line. It runs **after**
the commit, not before: a crash between the two leaves content without a config — the pre-change
status quo, repaired by the next update — whereas the reverse leaves a tree declaring itself an index
while holding nothing. C-023, and S-019 asserts the containment.

**Write-if-absent, never update.** An existing `config.json` may be a verbatim copy of a hosted one
carrying fields OCX does not model. Leaving it alone keeps A2's "no local re-encoding" true and drops
all preserve-unknown-fields machinery. Accepted consequence: **no command rewrites a `config.json`**
— the repair path for a wrong one is to delete it and re-run `update`.

**The advisory `min_ocx_version` field is WITHDRAWN** (owner decision, 2026-08-09; C-006 tombstoned).
The field was to be written at creation and surfaced only in the message of `UnsupportedIndexFormat`,
a path that already refuses. Enforcing it was never on the table — it would make an unsigned,
CDN-cacheable field load-bearing, the identical argument that made `name_segments` explicitly *not* a
security control (`ocx_index.rs:93-98`), and would let an index operator brick clients. What killed
the advisory form is the interaction with write-if-absent: **nothing can ever correct it.** The value
would record the binary that created the tree and stay wrong for the life of a long-lived mirror —
which is precisely the deployment this ADR exists to enable, and staleness defeats the only benefit
the field had. Two gains from dropping it:

- An OCX-written `config.json` is exactly `{"format_version": 1}`, and `IndexFormatConfig` now models
  only the two keys the Python renderer knows. Decision F's parity story gets simpler, not weaker.
  *(Round-2 correction: this bullet originally claimed the result is **byte-identical** to the Python
  renderer's output. It is not — `render.py:335` emits `name_segments` from the module constant
  `NAME_SEGMENTS = 2` unconditionally, so the reference never produces the one-key form. The two
  agree on **form** — indent, separators, field order, `ensure_ascii`, trailing newline — and differ
  on **content**, because `name_segments` is an operator declaration OCX cannot derive. No churn
  follows: C-023 is write-if-absent and `regenerate` never writes one, so the two producers never
  write the same file. See the Correction under C-001 in the design spec.)*
- `ocx_lib` gains no `env!("CARGO_PKG_VERSION")` on this path, no `Version::parse`, no comparison.
  `UnsupportedIndexFormat` keeps its first clause only.

What is given up, honestly: a user whose binary is too old for the index they configured still gets
`index format_version 2 is not supported (this ocx understands 1)` and no hint what to install. That
problem is **unsolved** — but it was never solved by this field either, since a client old enough to
need the warning predates the field and ignores it (the invisibility recorded in Known tension 2).
Solving it needs a channel an old client already reads, which is a separate design.

---

## Decision F — Byte-exact serialization for `c/index.json` and `config.json`

`regenerate`'s second consumer is **`ocx-sh/index`'s publication step**. That repo takes publisher
fork-PRs writing only `p/<ns>/<pkg>.json` + CAS objects; `c/index.json` is generated at render time by
`indexbot render`, never edited in a PR — which is what keeps every PR from conflicting on one shared
file. Making `ocx index regenerate` able to do that job puts the "derive the catalog from `p/`"
knowledge in one place.

**There is a real divergence today, and it is wider than reported.** Verified in both repos:

| Document | Rust | Python | Verdict |
|---|---|---|---|
| **root** `p/<ns>/<pkg>.json` | `serialize_root` via the hand-rolled `PythonJson` formatter, trailing `\n` at `wire_writer.rs:49` | `serialize_package_root`, `json.dumps(indent=2, sort_keys=False, ensure_ascii=True)` + `"\n"` | **Parity holds.** Proven by `crates/ocx_lib/tests/index_wire_conformance.rs` against fixtures vendored from the Python serializer, pinned by `SOURCE_COMMIT`. **No latent newline gap on roots** — the question asked, answered |
| **catalog** `c/index.json` | `serde_json::to_vec_pretty` (`index_store.rs:1029`) — **no trailing newline**, non-ASCII emitted raw as UTF-8 | `json.dumps({...}, indent=2) + "\n"` (`render.py:309`), `ensure_ascii` defaults **True** | **Diverges** — by exactly one byte, the trailing newline, which is a full-file diff on every render. *(Round 2: the "plus a genuine divergence on any non-ASCII key" clause is withdrawn. `PACKAGE_ID_RE` (`validate_entry.py:70-72`) restricts keys to `[a-z0-9]` plus separators, so a non-ASCII catalog key cannot be produced upstream. `ensure_ascii` is vacuous for the catalog and for `config.json`; it still matters for roots, which carry free-form strings.)* |
| **`config.json`** | *(no writer today — item 1 adds one)* | `json.dumps({"format_version": …, "name_segments": NAME_SEGMENTS}, indent=2) + "\n"` (`render.py:334-338`) | **A Python reference form exists.** Not in the brief. Item 1's writer must match it, or it ships a second divergence on day one |

**Fix: one formatter, three documents.** Generalize `wire_writer`'s private body over `T: Serialize`
through the existing `PythonJson` formatter and add `serialize_catalog` / `serialize_config` beside
`serialize_root`. This is a **reuse, not a second serializer** — `PythonJson` already owns the one
rule the JSON ecosystem does not implement (`ensure_ascii`), and the trailing newline is already
`out.push(b'\n')`. Add catalog and config conformance fixtures beside the existing root vectors.
C-025.

Field order is load-bearing: Python's `json.dumps` defaults `sort_keys=False`, so `config.json` is
`format_version` then `name_segments`. Rust struct field order must match, and `name_segments` needs
`skip_serializing_if = "Option::is_none"`. With `min_ocx_version` withdrawn, `IndexFormatConfig`
carries no OCX-only field at all, so an OCX-written config is byte-identical to a Python-rendered one
rather than a superset of it.

**Consumers, and who does what:** the index repo is a named downstream consumer, and **adopting this
is a separate change in a different repo**. This ADR enables it; it does not perform it.

---

## Encoded constraints (ratified; not re-litigated)

- `format_version` comparison stays `!=`.
- **The catalog is authored, never mirrored.** Nothing copies an upstream `c/index.json` verbatim —
  `regenerate` derives it from the roots on disk.
- **No typed root-mutation API.** `serde_json::Value` + `serialize_root` stays the root rewrite
  surface. Write-if-absent means `config.json` needs no mutation surface at all.
- **No delete verb on `IndexStore`.** `regenerate` removes no root and no `o/` object.
- No `index catalog --filter`. `--format json` exists.
- **Yank markers ride in the root bytes and inherit for free.** `surface_root_status`
  (`ocx_index.rs:851`) is shared verbatim by the live resolve and the offline committed-root resolve.
  `index sync` merges roots through `commit_published_root`, so a yank in a snapshotted root is
  present in the served tree and enforced by every reader (S-014).
- `ocx package announce` is public-index governance, not this path. No forge dependency.

---

## Consequences

**Positive**

- The `wget --mirror` equivalence of `adr_oci_index_only_dispatch.md:761-762` holds at the **source
  root**, not just per package.
- An air-gapped mirror is one command, then a static file server. Zero `ocx-mirror` code.
- A **local** tree declaring an unsupported `format_version` is refused for the first time.
- The version-gate path is a **net deletion**: one enum, five match arms, one parameter.
- The catalog document gains cross-language byte parity it never had, and `config.json` gets it before
  its first divergence exists.
- `index update`'s pre-existing unbounded argv fan-out is capped (C-024).
- **`ocx-sh/index` gains a supported path to retire its own catalog renderer** — its decision, later.

**Negative / accepted**

- An endpoint the user configured as an index that serves parseable roots will resolve without ever
  declaring itself. Analysed above; accepted as the price of one rule.
- One `open()` per index source per process on the resolve path, memoized; a permanent miss for
  derived sources.
- A **derived** (plain-OCI) subtree stays non-servable — no `config.json`, no `c/index.json`, by
  grammar.
- No command rewrites an existing `config.json`; a wrong one is repaired by deleting it and re-running
  `update`.
- `--offline` builds **no** index sources (`context.rs:687-690`), so `file://` is unavailable under
  `--offline` though it needs no network. See OQ2.

**Risks**

- *A hostile `file://` tree.* The path comes from `[registries]`, which merges through the managed
  tier. Mitigations: absolute-path + empty-authority requirement (C-018/C-019), containment via
  `utility::fs::path::join_under_root` (C-016), the `MAX_INDEX_DOCUMENT_BYTES` cap (C-017),
  `system_locked`. Residual: no TLS and no plain-HTTP gate apply — by definition — and the SSRF floor
  on the `repository` pointer is unchanged and still applies.
- *Silent not-found via unreadable files.* A transport mapping EACCES to `NotFound` would reproduce
  this ADR's own defect one directory deeper. C-015 makes ENOENT (and `NotADirectory`) the **only**
  miss.

---

## Boundary: what `wire_writer` now guarantees

**`wire_writer.rs:13-17` today:** *"One document is ever serialized by OCX: the human-diffable
`p/<ns>/<pkg>.json` root ([`serialize_root`]). … the index writes no object shapes of its own, so
there is nothing else here to emit."*

Two items now cross it (`config.json` and `c/index.json`), and the sentence needs replacing rather
than being left contradicted. Separate what it protects:

1. *OCX defines no object shapes of its own* — `adr_oci_index_only_dispatch.md` D1, the reason the
   invented observation object was deleted. **Untouched.** `config.json` and `c/index.json` are frozen
   ● shapes of the hosted grammar (`adr_index_indirection.md:787-788`), both already **read** by OCX.
2. *One document has a canonical byte-exact form* — **this is the part that changes, and it gets
   stronger.** The literal claim was already false: `commit` has serialized `CatalogDocument` via
   `to_vec_pretty` (`index_store.rs:1029`) for as long as the local catalog has existed, with no
   parity test. Decision F brings it, and `config.json`, under the same regime.

**New invariant, stated as the replacement:**

> Every document OCX writes into an index tree goes through the one canonical formatter
> ([`PythonJson`], matching `json.dumps(indent=2, sort_keys=False, ensure_ascii=True)` + a trailing
> `\n`) and has a vendored cross-language parity fixture: the `p/<ns>/<pkg>.json` root
> ([`serialize_root`], `CONTRACTS.md` §14), `c/index.json` ([`serialize_catalog`]), and `config.json`
> ([`serialize_config`]). What a tag points at is a registry's own OCI image index, stored
> byte-for-byte as served — the index writes no object shapes of its own.

Strictly stronger than the sentence it replaces: it covers three documents where the old one covered
one and quietly excluded a second.

### Two hygiene items, in scope

| Location | Current | Fix |
|---|---|---|
| `adr_index_indirection.md:223` | Unscoped `> superseded — see … D6` under the A2 heading | Scope it to the published-index bullet (`:239-245`), matching D6's table row (`:314`) and the metadata line (`:22`) |
| `subsystem-oci.md:352` | *"the yank gate, **obs-digest verify** and terminal stop"* | *"dispatch-object digest verify"* — pre-D6 vocabulary retired at `adr_oci_index_only_dispatch.md:168-170`. `subsystem-oci.md:379-381` needs **no** fix |

### Out of scope, recorded

`CatalogDocument` drops unknown fields on the round trip (`wire.rs:134-139`) — a forward-compat hazard
TUF forbids. Pre-existing; nothing unknown legitimately arrives in a local catalog (authored, never
mirrored), and `regenerate` re-derives the map wholesale, so a dropped unknown field could not survive
anyway. Worth fixing separately if the hosted catalog ever grows a sibling field.

---

## Constitution deviations

`arch-principles.md` carries **named** conventions rather than a numbered list; the table keys on the
actual rule names. No deviation is required.

| Convention | Status | Note |
|---|---|---|
| Fleet forward-compat — no `deny_unknown_fields` in the `Config` tree | **Compliant** | `IndexFormatConfig` gains `Serialize` but no `deny_unknown_fields` |
| Utility-catalog-first | **Compliant** | `join_under_root`, `path_exists_lossy`, the existing `write_bytes_atomic` → `persist_temp_file`, `lock_scoped` via `begin_catalog_transaction`, and `PythonJson` reused rather than a second formatter. No new helper |
| Locking policy: atomic-rename-replaced data ⇒ `lock_scoped` into `$OCX_HOME/locks`, never a sidecar | **Compliant** | Both the `config.json` creation write and `regenerate` run inside the `"index-catalog"` transaction |
| Type names: full descriptive names | **Compliant** | `RegenerateOutcome`, `FileIndexTransport` |
| Internal enum exhaustiveness | **Compliant** | `FormatVersionState` is deleted, not made `#[non_exhaustive]` |
| Test-only seams | **Compliant** | No new seam |
| Core vs plugin boundary | **Compliant** | `regenerate` is `ocx_lib` surface taking an `IndexStore` because the logic belongs in the library and the CLI stays a thin wrapper — the rule every other command follows. *(Round 2: the stronger claim that `ocx-sh/index` is "a real second caller and not a CLI caller" is **withdrawn** — that repo is pure Python, no `Cargo.toml`, so its only route is shelling out to `ocx index regenerate`. The placement is right; the justification was not.)* ocx-mirror's path stays the CLI; its four symbols are untouched |

---

## Migration / rollout

| Population | What happens |
|---|---|
| An existing `$OCX_HOME/index/<published-source>/` | `config.json` appears on the **next** `ocx index update` for that source. Resolution is unaffected either way |
| An existing derived subtree | Unchanged. Not servable — by grammar |
| A tree produced by an **older** ocx | Read unchanged; gains `config.json` on the next update |
| A tree produced by a **newer** ocx, read by an older one | The older ocx ignores the local `config.json`, so it reads as today. A `format_version: 2` tree is mis-parsed by pre-A2 binaries — the hole this closes going forward, not retroactively |
| An index tree served over HTTPS with no `config.json` at all | **Now resolves** rather than reporting empty. The behaviour change users will actually notice, and the fix they wanted |
| `[mirrors]` entries | `parse_url` unchanged; a `file://` index-role override is refused by C-018's post-override gate (C-020, corrected) |
| A tree whose `config.json` is wrong or stale | **No command repairs it.** `update` is write-if-absent, `regenerate` never touches it. The repair path is a hand-edit or deleting the file and re-running `update`. Named here because "write-if-absent, never update" makes this permanent — see Known tension 4 |
| `ocx-sh/index` | Nothing, until that repo chooses to adopt `regenerate` |

**Rollout order:** (1) decision A's deletion + the local reader; (2) Decision F's serializer + fixtures;
(3) the `commit` creation write; (4) `file://` + closed scheme set; (5) `regenerate`; (6)
the bulk snapshot + the bounded loop; (7) rule and ADR amendments in the same commit as the code they
describe. (1), (2) and (4) are independently shippable.

### Rule and ADR amendments

Prescribed against the flag grammar and landed in the verb's; the blocks below are quoted from the
rule as it now reads, so the ADR and the rule cannot be read against each other. One clause in
consequence 2 is not this section's — the explicit-`--remote`-resolve sentence came in with the
implementation commit, from the pin-movement rule rather than from the bulk snapshot.

**`subsystem-oci.md` consequence 2** (`:20-31`):

> 2. **A pin moves only under a command the user invoked naming what to move.** `ocx index update
>    <pkg>...` moves the packages listed and nothing else. `ocx index sync <REGISTRY>...`
>    moves the set the user named by naming the registries, by enumerating **each source's catalog at
>    that instant** — an explicit operator act, never a default. Nothing else moves a pin: not a
>    listing, not an update of a *different* package, and there is no implicit whole-index sync in any
>    spelling. The one resolve that does move a pin is an explicit `--remote` one — it re-fetches and
>    rewrites the tag it touches, the same write an `ocx index update` scoped to that tag would make;
>    a **default**-mode resolve never does. `index sync` moves nothing under `oci/index/**` beyond tag
>    pins, dispatch objects and the source's `config.json`; no patch-companion and no managed-config
>    binding is recorded there, and nothing under `oci/index/**` gains the ability to record either.
>    `ocx index regenerate` moves no pin at all — it re-derives
>    `c/index.json` from the roots on disk.

**`subsystem-oci.md:368-372`:**

> **There is no *implicit* catalog sync.** `ocx index update <pkg>...` fetches the named packages'
> roots and nothing else. `ocx index sync <REGISTRY>...` reads each source's catalog **to choose the
> set**, then performs exactly the same per-package work through the same bounded loop. `ocx index
> regenerate` fetches nothing: it rebuilds the local catalog from the local `p/` tree. Neither form
> reports on a package it was not asked about.

**`subsystem-oci.md` "The local `c/index.json` is AUTHORED, not mirrored"** (`:374-382`) — add: the
catalog is **derived** data, and `ocx index regenerate` is the operation that restores it to a pure
derivation of `p/`, which is the only way an entry naming a removed root is ever cleared.

**Downstream citations, audited:**

| Citation | Verdict |
|---|---|
| `subsystem-oci.md:28-34` (patch companions / managed config never pin here) | **Holds verbatim.** Naming a *registry* names neither; the added sentence makes the non-licensing explicit |
| `subsystem-oci.md:56` ("…the `ocx index update` report, `ocx index catalog`") | **Does not hold, and did not before this ADR** — there is no `index update` report (`index_update.rs:67-73`). Correct to `ocx index catalog --remote` |
| `adr_index_indirection.md` F1 row for `config.json` ("Read once; reject unknown `format_version` (fail-closed, `DataError`)") | **Amended by decision A.** Rejection on unknown version stands; *absence* no longer fails closed |
| `adr_index_indirection.md` 2026-08-05 amendment, *"do not re-propose"* `--all` | **Holds.** Different referent, required argument |
| `adr_oci_registry_mirror.md` | Set `**Superseded By:**` to this ADR **for the index-tree half only**. Not deleted: it carries R1–R6 and the replace-never-fallback rationale, and predates the shipped `[mirrors]` index role, the `{registry,index}` object form, the mirror-suppresses-compiled-index rule, per-role `system_locked`, and field-wise merge |

**In scope, one line:** delete *"Packages with an update waiting are reported afterward"* from the
`Index::Update` clap help (`crates/ocx_cli/src/command/index.rs:33`). Block-tier doc/code mismatch per
`quality-cli-help.md`, in the help block this change edits anyway. `regenerate`'s help must not copy
it — `regenerate` genuinely does report, and its help says only what it produces.

---

## One-way doors

| Door | Reversibility | Note |
|---|---|---|
| Absent `config.json` ⇒ version 1 | **Low.** Trees will ship without one and keep working | The behaviour users depend on after this lands |
| `regenerate` verb grammar | **Medium.** Pre-1.0 | A verb is costlier to retire than a flag |
| `ocx index sync <REGISTRY>...` verb grammar | **Medium.** Pre-1.0 | Decided as a flag, shipped as a verb (Decision B), which is the costlier half of that door — the same note as `regenerate`'s row |
| `file://` as an index base scheme | **Low.** Config files in the wild carry it | Bounded by the closed scheme set — adding a scheme later is additive; removing `file` is not |
| Catalog / config byte form | **Low once fixtures are vendored** — that is the point | Fixing it now is cheaper than after a third implementation exists |
| ~~`min_ocx_version` field name~~ | *n/a — field withdrawn, so no door is opened* | Dropping it before shipping is the cheapest moment; adding it later stays additive |

---

## Known tensions, recorded not resolved

Three consequences that are real, accepted, and not fixed by this ADR. Recorded so a later change
does not rediscover them as surprises.

**1. `ensure_ascii` conflicts with any future canonical-JSON signing.** Decision F matches
`ocx-sh/index`'s Python renderer, which emits `\uXXXX` escapes for non-ASCII (`render.py:309`,
`json.dumps` default `ensure_ascii=True`). RFC 8785 (JCS) — the canonicalization every practical
JSON-signing scheme uses, and the one TUF-adjacent tooling expects — mandates the **opposite**: raw
UTF-8, no escaping. So the moment index documents get signed, the signing input cannot be the served
bytes; it must be a re-canonicalized form, and the two encodings must both be produced and kept
consistent. Choosing parity with the existing implementation over forward-compatibility with signing
is the right call today — a second implementation already exists and an unsigned divergence is a
present bug, while signing is not designed — but the cost is named here, not discovered later.

**2. There is no way to tell an old client it is too old — and that stays true.** A client predating
any compatibility field is exactly the client such a field would need to reach, so an in-band marker
cannot work: it only ever helps a client new enough to have shipped after the marker did. This
observation is what led to withdrawing `min_ocx_version` outright (Decision E) rather than shipping
it as a diagnostic. The gap is real and **unsolved**: a user on an old binary against a v2 index gets
`index format_version 2 is not supported (this ocx understands 1)` with no hint what to install.
Closing it needs a channel an old client already reads — the update-check path, or the error's own
prose carrying a stable URL — which is a separate design, not a config field.

**3. Resumability is unsolved for `ocx index sync`.** A bulk mirror of a large source is a long,
network-bound, all-or-nothing loop: interrupt it and the local tree holds an arbitrary prefix of the
catalog with no record of where it stopped. The verb split widened the window rather than moving it —
one invocation now names any number of registries and flattens them into that single loop, so the
prefix an interrupt leaves can straddle sources. This is *safe* — merge-only-never-delete means the
partial tree is a valid union of snapshots, and re-running converges — but it is not *cheap*: the
re-run refetches everything. No checkpoint, no `--continue`, no manifest of intent is designed here.
Accepted for the first cut because correctness does not depend on it and the fix (a resume journal)
is additive. If mirror runs turn out to be measured in hours, this is the first thing to revisit.

**4. Nothing can correct a `config.json`.** Three rules combine: absent ⇒ version 1,
write-if-absent-never-update, and `regenerate` never touches the file. Once written it cannot be
changed by any ocx command. With `min_ocx_version` withdrawn the file holds only `format_version`
(plus `name_segments` when an operator authored it), so there is far less to go stale — but the
consequence survives for the v2 transition: **when v2 ships, its first task is not the `!=` → `<=`
widening this ADR defers, but adding the config *rewrite* path C-023 refuses.** The interim repair
path is manual — delete the file, re-run `update` — and is named in the migration table.

---

## Open Questions

- **OQ1 — How does the library primitive address a served-tree root, where the layout is
  `config.json` / `c/` / `p/` at the repo root with no `<home>/<source>/` wrapper?**
  **Recommended: no new API — construct `IndexStore::new(<parent-of-checkout>).with_locks_root(<tmp>)`
  with `source = <checkout-dir-name>`.** `wire_source_dir` is `root.join(slugify(source))`
  (`index_store.rs:297-299`), so this addresses the checkout exactly, and `to_relaxed_slug` preserves
  `[a-zA-Z0-9._-]` so an ordinary directory name is identity. Two caveats to document rather than
  engineer around: the checkout directory name must be slug-identical to itself, and `locks_root`
  must be redirected because it defaults to `root/locks` (`:62-66`), which would otherwise litter the
  parent. If the index repo finds this awkward in practice, a root-is-the-source constructor is a
  later, additive change — do not build it speculatively.

- **OQ2 — Should `--offline` build `file://` index sources?**
  **Recommended: no, in v1.** `build_index_sources` returns empty when the remote client is absent
  (`context.rs:687-690`), and an `OcxIndex` needs a physical-fetch client for the leaves its pins
  name. An index source that resolves a pin nothing can fetch is a half-answer. Revisit on a concrete
  `--offline` + `file://` + warm-blob-store case; the change is local to `build_index_sources`.

- ~~**OQ3 — Who owns the `min_ocx_version` field name in `config.json`?**~~ **Closed by withdrawing
  the field** (owner decision, 2026-08-09). No cross-repo name to align, and `IndexFormatConfig` now
  models exactly the two keys the Python renderer emits.

---

## Validation (contract, not implementation)

- [ ] A tree produced by one `ocx index update` for a published source, served over HTTPS, resolves
      every package it contains (S-002).
- [ ] A tree with **no** `config.json`, served over HTTPS, also resolves — the decision-A behaviour
      change (S-016).
- [ ] `ocx index update` output is byte-identical to `wget --mirror` **including the source root**
      (S-001).
- [ ] A subtree declaring `format_version: 2` exits **65** with the **same message** on local,
      `file://` and `https://` — no field of the config participates in the diagnostic (S-006).
- [ ] `test_unsupported_format_version_fails_closed_registry_only_unaffected` passes **unmodified**.
- [ ] `serialize_catalog` and `serialize_config` are byte-identical to vendored fixtures generated by
      `render.py:309` and `:334-338` (S-017).
- [ ] `regenerate` on a catalog listing a removed root drops that entry, and every `p/**` and `o/**`
      file is byte-identical afterwards (S-009).
- [ ] `ocx --frozen index regenerate <REG>` exits **0**; `ocx --frozen index sync <REG>` exits **81**
      (S-005).
- [ ] A `file://` tree with `chmod 000 config.json` exits **69**, not a silent not-found (S-007).
- [ ] `[registries."<ns>"] index = "ftp://x"` exits **78** at context init (S-011).
- [ ] A `[mirrors]`/`OCX_MIRRORS` `file://` index-role override is refused by C-018's post-override
      gate (78); `parse_url` itself still accepts it, by design (C-020).
- [ ] A second `index update` on an unchanged tree leaves it byte- and mtime-identical (S-008); an
      existing `config.json` is never rewritten (S-015).
- [ ] The summed peak in-flight requests across two registries under `ocx index sync` stays at or
      below `INDEX_REFRESH_CONCURRENCY`, measured with a fixture that holds each request open
      (C-024).

## Links

- [`adr_oci_index_only_dispatch.md`](./adr_oci_index_only_dispatch.md) — D3 invariant, D6 clause map
- [`adr_index_indirection.md`](./adr_index_indirection.md) — wire grammar; A1/A4/B/C2/C3/E/G/H stand
- [`adr_oci_registry_mirror.md`](./adr_oci_registry_mirror.md) — superseded for the index-tree half only
- [`design_spec_servable_index_snapshot.md`](./design_spec_servable_index_snapshot.md) — numbered
  contracts and scenarios
- [`research_index_wire_versioning_trust.md`](./research_index_wire_versioning_trust.md) ·
  [`research_servable_index_snapshot.md`](./research_servable_index_snapshot.md)
- `ocx-sh/index`: `bot/src/indexbot/core/render.py:296-338` (the Python reference forms)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-08-10 | review round 1 | **Decision B superseded by the verb split.** `--from-catalog` shipped and was promoted to `ocx index sync <REGISTRY>...`; the flag exists nowhere and `index update` is again only `<PACKAGE>...`. Framing follows C-012's post-merge amendment in the design spec — two grounds, grammar and ergonomics, each recorded as what it is: a flag excluding its own verb's positionals costs three clap declarations, and a registry list had nowhere natural to sit while the positional slot belonged to packages. The rejected options B2/B3 and the three costs that were said to sink a separate verb are kept, with each cost's outcome recorded: cost 1 (a duplicated policy gate) materialised and is paid for by `exactly_one_frozen_gate` in both `index_sync.rs` and `index_update.rs`, cost 3 (alias-or-duplicate) is answered by the shared `index_common::refresh_packages`, and cost 2 (one mental model) is the trade the split made deliberately — one operation now has two names. The trailing `--from-catalog` references (one-way door, known tension 3, the prescribed `subsystem-oci.md` amendment, scope, rollout, validation) name the shipped grammar. |
| 2026-08-09 | owner | **`min_ocx_version` withdrawn** (C-006 tombstoned, OQ3 closed, one-way-door row retired). The round-2 finding stands: write-if-absent plus never-update means nothing can ever correct the value, so it would record the creating binary and stay wrong for a long-lived mirror's life — defeating the diagnostic value that was its only justification. Consequence: an OCX-written `config.json` is exactly `{"format_version": 1}`, **byte-identical** to `render.py:334-338`'s form rather than a superset, and `UnsupportedIndexFormat` keeps its first clause only. The old-client-warning gap is acknowledged as unsolved (Known tension 2) and needs a channel an old client already reads, not a config field. |
| 2026-08-09 | review round 2 | **Decision E's hook moved out of `CatalogTransaction::commit`.** The cross-model gate showed `commit` is a shared primitive with two production callers (`commit_published_root`, and `persist_recovered_catalog_entry` — a read-path self-heal) plus Decision C's `regenerate_catalog` as a would-be third, so the hook contradicted Decision C and let a resolve write a wire document. Hook now sits in `commit_published_root` after the commit, via `pub(crate) IndexStore::ensure_source_config`; S-019 asserts it. Also: the "actual trust anchor" claim about `ocx_index.rs:738-746` withdrawn as false (self-consistency, not provenance); C-020 corrected (`parse_url` accepts any scheme — enforcement moved into C-018's post-override gate); *Known tensions* section added for the `ensure_ascii`/JCS conflict, advisory-field invisibility, and unsolved `--from-catalog` resumability. |
| 2026-08-09 | architect (opus) | Initial ADR. Defect traced to `config.json` 404 → `NotAnIndex` → `resolve_root` `Ok(None)` before the root GET. |
| 2026-08-09 | architect (opus) | Scope cut (later partly reversed): `regenerate` removed, separate `mirror`/`snapshot` verb rejected, `config.json` hook placed in `CatalogTransaction::commit`. Framing corrected to cite D3/D6 rather than A2. |
| 2026-08-09 | architect (opus) | **Owner ratification of decision A: the uniform rule**, reversing the earlier provenance-gated ratification. All five `FormatVersionState::NotAnIndex` sites enumerated (`ocx_index.rs:384, 497, 660, 681, 831, 1019`); the enum is **deleted**, `check_format_version` returns `Result<Arc<IndexFormatConfig>>`, and the `AbsentConfig` parameter of the prior draft is deleted with it. What is lost is stated plainly — nothing distinguishes a misconfigured base URL from a v1 index, and that is acceptable because the user typed the URL (**superseded**: this row originally cited the dispatch-object digest verify at `:738-746` as "the actual trust anchor" — withdrawn as false, see the Correction blockquote in *What is lost*). **Acceptance-test impact: none** — `test_index_ocx_sh.py:525` tests `format_version=2`, which the rule keeps; no test covers the absent case. Noted that `AliasState::NotAnIndex` (`package/cascade/graph.rs:236`) is an unrelated enum. |
| 2026-08-09 | architect (opus) | **`regenerate` reinstated as Decision C — drift repair, not promote-to-servable.** The unrecoverable drift is precisely identified: a catalog entry naming a removed root is never cleared, because `write_root` only upserts (`index_store.rs:993-995`) and `read_root`'s recovery only adds. Framed as pure re-derivation of derived data, which is why it does not violate merge-only-never-delete. `--frozen` **permits** it, with the asymmetry reasoned. It does **not** write `config.json` — `name_segments` is an operator declaration OCX cannot derive — which also removes the risk of injecting fabricated metadata into a foreign tree. `generate` rejected on the first-time-derivation-vs-re-derivation distinction. |
| 2026-08-09 | architect (opus) | **Decision F added: byte-exact serialization**, driven by `ocx-sh/index` as `regenerate`'s second consumer. Verified against both repos — roots have parity (`wire_writer.rs:49` + `index_wire_conformance.rs`), so **there is no latent newline gap on roots**; `c/index.json` diverges (`index_store.rs:1029` `to_vec_pretty` vs `render.py:309` `json.dumps(indent=2) + "\n"` with `ensure_ascii=True`). **New finding beyond the brief:** `config.json` also has a Python reference form (`render.py:334-338`), so item 1's writer must match it or ship a divergence on day one. Fix reuses `PythonJson` for all three documents rather than adding a formatter. `wire_writer.rs:13-17`'s invariant replaced with a strictly stronger one covering three documents. OQ1 answers the served-tree-root addressing question with **no new API**. |
