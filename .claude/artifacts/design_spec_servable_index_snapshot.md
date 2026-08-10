# Design Spec: Servable Index Snapshot

## Overview

**Status:** Draft
**Date:** 2026-08-09
**Author:** architect
**ADR:** [`adr_servable_index_snapshot.md`](./adr_servable_index_snapshot.md)
**Scope (four items + one pulled in):** `config.json` writer + local reader; `ocx index sync`;
`ocx index regenerate`; a `file://` `IndexTransport`; and byte-exact serialization
for `c/index.json` + `config.json` (ADR decision F). *(The advisory `min_ocx_version` field was a
fifth item, withdrawn by owner decision — C-006.)*

Every contract is `C-NNN`, every scenario `S-NNN`. These are join keys for a later coverage check and
are **stable**: IDs withdrawn during the 2026-08-09 scope churn are kept as tombstones rather than
renumbered. Every contract is stated so it can be tested **without reading implementation code** —
type, signature, inputs, expected output, and for each failure mode the error variant and exit code.

Decision A is **settled** (uniform rule). There is no owner-pending contract.

### Exit codes used

| Code | Name | Reached by |
|---|---|---|
| 64 | `UsageError` | clap argument faults |
| 65 | `DataError` | `UnsupportedIndexFormat`, `MalformedIndexDocument`, `MalformedRootDocument`, `MalformedCatalogKey` |
| 69 | `Unavailable` | `IndexHttpFailed` — including every non-ENOENT I/O failure of the `file://` transport — and `CatalogDocumentAbsent` |
| 78 | `ConfigError` | `InvalidIndexUrl`, `PlainHttpIndexNotAllowed` |
| 79 | `NotFound` | `RemoteManifestNotFound` |
| 81 | `PolicyBlocked` | `--frozen` / `--offline` refusals |

### Tombstones

| ID | Was | Status |
|---|---|---|
| C-002 | public `IndexStore::write_source_config` | Withdrawn — one private writer, inside `commit` (C-023) |
| C-009 | `config.json` field-wise write over a `Value` | Withdrawn — write-if-absent needs no mutation surface (C-023) |
| C-011 | *(never allocated)* | **Intentionally unused.** A numbering slip in the first draft — no contract was ever written or dropped here, and coverage has no gap: the CLI contracts either side are C-010 (`regenerate`) and C-012 (`index sync`). IDs are never renumbered, so the hole stays documented |

---

## 1. Decision A — one version rule over bytes

### C-001 — `IndexFormatConfig` is the shared `config.json` shape

**Type:** `ocx_lib::oci::index::wire::IndexFormatConfig` (moved from `ocx_index.rs`, which is now one
of two readers plus a writer; amend `wire.rs:24-27`'s "stays next to its one reader" sentence).

```rust
pub struct IndexFormatConfig {
    pub format_version: u64,                                    // required, no serde default
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_segments: Option<NonZeroU32>,
}
```

- Derives `Deserialize` **and** `Serialize`. No `deny_unknown_fields`.
- **Field order is load-bearing** (C-025): Python's `json.dumps` defaults `sort_keys=False`, and
  `render.py:334-338` emits `format_version` then `name_segments`. Declaration order must match.
- `skip_serializing_if` on `name_segments` omits the key when the value is absent.
- **The type adds no field of its own.** The advisory `min_ocx_version` of an earlier revision is
  **dropped** (C-006, tombstoned) — so `IndexFormatConfig` models exactly the two keys the Python
  renderer knows, and no producer-specific third.

> **Correction — "byte-identical to the Python form" was overstated, and is now scoped.** Earlier
> revisions of this spec and of the ADR said dropping `min_ocx_version` makes an OCX-written
> `config.json` byte-identical to a Python-rendered one. That is **false as a statement about
> content**: `render.py:335` emits `name_segments` from a module constant (`NAME_SEGMENTS = 2`,
> `:46`) **unconditionally** — there is no branch that omits it — so the reference never produces
> `{"format_version": 1}`, which is exactly what C-023 writes. What is true, and all that C-025
> needs, is **form**: `serialize_config` emits the Python *shape* — two-space indent, `": "` and
> `","` separators, `sort_keys=False` field order, `ensure_ascii`, one trailing `\n` — for whatever
> fields the struct carries. The two producers agree on encoding and differ on content, by design:
> `name_segments` is an operator declaration OCX cannot derive from a tree (C-008), so OCX omits it
> rather than guessing.
>
> **No churn follows**, because the two never write the same file: C-023 is write-if-absent, so a
> tree rsync'd from a hosted index keeps the renderer's two-key config untouched, and `regenerate`
> never writes one at all. The one real consequence is semantic, not byte-level: an OCX-authored
> config declares **no** name-shape constraint where a hosted one declares `2`. That is a widening,
> and it is safe precisely because `name_segments` is documented as *not* a security control
> (`ocx_index.rs:93-98`) — it only ever narrows what a client asks for.
- `IndexFormatConfig::assumed_v1()` — `{ format_version: 1, name_segments: None }`, the value
  substituted for an absent document (C-005).

**Test:** `from_str(r#"{"format_version":1,"future_key":[]}"#)` succeeds, ignoring `future_key`;
`from_str(r#"{}"#)` fails; `from_str(r#"{"format_version":1,"name_segments":0}"#)` fails (the
`NonZeroU32` validator).

### C-003 — `IndexStore::read_source_config`

```rust
pub async fn read_source_config(&self, source: &str) -> Result<Option<IndexFormatConfig>>
```

| Input | Output |
|---|---|
| No file at `<home>/<slug(source)>/config.json` | `Ok(None)` |
| Valid JSON matching C-001 | `Ok(Some(config))` |
| Present but unparseable | `Err(Error::MalformedIndexDocument { url: <path>, source })` → **65** |
| Present but unreadable (EACCES, EISDIR, …) | `Err(file_error(path, io))` — **never** `Ok(None)` |

The last row is the local-filesystem twin of C-015: a permission error read as absence would silently
disable the version gate.

**Test:** all four rows on a `tempfile::TempDir` home; the EACCES row is `#[cfg(unix)]`, `chmod 000`.

### C-004 — One version gate, no caller parameter

```rust
// ocx_lib::oci::index::wire — beside SUPPORTED_FORMAT_VERSION
pub(crate) fn gate_format_version(version: u64) -> Result<()>
```

| Input | Result |
|---|---|
| `version == SUPPORTED_FORMAT_VERSION` | `Ok(())` |
| otherwise | `Err(UnsupportedIndexFormat { version })` → **65** |

There is **no** `absent` parameter and no caller-selected behaviour: absence is resolved to
`IndexFormatConfig::assumed_v1()` *before* the gate runs (C-005), so the gate only ever sees a
version. `SUPPORTED_FORMAT_VERSION` stays `1`; the comparison stays `!=`.

> **Corrected — the gate lives in `wire.rs` and takes a `u64`, not `&IndexFormatConfig`.** An earlier
> revision put it in `ocx_index.rs` taking the config struct, and paired it with a structural test
> asserting "no call site compares against `SUPPORTED_FORMAT_VERSION` itself". That test **fails on
> `main` today**: `CatalogDocument::into_packages` (`wire.rs:236`) makes exactly that comparison, for
> the `c/index.json` envelope, and it is outside WP6's file set. The choice was to scope the test down
> until it passed — which would have made the "one gate" claim true only by narrowing what it claims —
> or to make it true. The second is correct and is also the smaller diff: same constant, same
> `UnsupportedIndexFormat` error, same policy, and `wire.rs:225-229`'s own doc already states the
> intent — *"One version pin, one policy, one error, whichever document carries it."* So the gate is a
> free function in `wire.rs` beside the constant, taking the version; the config path calls
> `gate_format_version(config.format_version)` and `into_packages` calls it with its envelope's. No
> behaviour changes at either site. **WP6 gains `wire.rs`** (WP1 is merged; no conflict).

**Test:** two rows. Plus the structural test above, now literally true: exactly one comparison
against `SUPPORTED_FORMAT_VERSION`, inside `gate_format_version`. *(Scoped, not "crate-wide" as an
earlier revision said — the test walks `src/oci/index`. That is the right root, since every reader
that could drift lives under it, but the assertion message must say so rather than claim the wider
scope.)* The operator set it scans must include `<` and `>`, not just `==`/`!=`: the deferred v2
change is `!=` → `<=`, and a **second** reader added with `<=` while the gate keeps `!=` leaves the
count at 1 and passes — which is precisely the CWE-501 drift the test exists to catch.

### C-005 — Absent means version 1, at both readers

| Reader | Absent `config.json` | Present |
|---|---|---|
| `OcxIndex::check_format_version` (`ocx_index.rs:641`) — every fetched base, `https://` and `file://` | `IndexFormatConfig::assumed_v1()`, **not memoized** (preserving today's re-check-every-call property, `:657-659`) | Parsed, gated (C-004), memoized |
| `LocalIndex`, once per source per instance | `assumed_v1()`, memoized including the absent outcome | Parsed, gated (C-004), memoized |

**Signature change:** `check_format_version` returns `Result<Arc<IndexFormatConfig>>`.
`FormatVersionState` is **deleted**. *(Corrected: **four** consumer sites, not five. The ADR's five
line numbers count `:472` and `:474` as two arms of one `match` in `jurisdiction`, and two of the
remaining citations are constructions inside `check_format_version` itself. The other three —
`resolve_root`, `fetch_catalog`, `fetch_root_document` — are the "pure deletions of an early return",
with one qualification: the early return goes, **the call stays** as
`self.check_format_version().await?;`. Dropping the call would let a served `format_version: 2`
resolve roots, which C-005's own test forbids.)*

> `AliasState::NotAnIndex` (`package/cascade/graph.rs:236`) is an unrelated enum and is untouched.

**Test:** serve a tree with **no** `config.json` through the stub transport and assert the package
**resolves**, with a `p/<ns>/<pkg>.json` request recorded (`StubIndexTransport::request_urls`) — the
inverse of today's behaviour. Then set `format_version: 2` and assert **65**. Repeat both against the
same tree read as a local subtree.

### C-006 — ~~The advisory minimum-ocx-version diagnostic~~ — **WITHDRAWN**

`min_ocx_version` is **not** written, not read, and not part of `IndexFormatConfig`. Owner decision,
2026-08-09, on the round-2 finding that the field is unfixable once written: `update` is
write-if-absent, `regenerate` never touches `config.json`, so the value would record the binary that
*created* the tree and stay wrong for the life of a long-running mirror — defeating the diagnostic
value that was its only justification.

Two things follow, and they are the reason the withdrawal is an improvement rather than a loss:

1. `IndexFormatConfig` now models exactly the two keys the Python renderer knows and no
   producer-specific third, so C-025's parity fixtures have nothing to special-case. *(An earlier
   revision said this made the two producers' output byte-identical. It does not — see the Correction
   under C-001: `render.py:335` always emits `name_segments`. They agree on **form**, and differ on
   **content**, by design.)*
2. `Error::UnsupportedIndexFormat`'s message keeps its first clause only —
   `index format_version 2 is not supported (this ocx understands 1)`. No version comparison, no
   `Version::parse`, no `env!("CARGO_PKG_VERSION")` in this path.

The problem the field was meant to address — a user holding a binary too old for the index they
configured — is unsolved and stays unsolved. It was never solved by this field either: a client old
enough to need the warning predates the field and would ignore it. See ADR *Known tensions*.

---

## 2. Byte-exact serialization (ADR decision F)

### C-025 — One formatter, three documents, three fixtures

`wire_writer.rs`'s private body is generalized over `T: Serialize` through the existing `PythonJson`
formatter; `serialize_root` keeps its signature and two siblings are added:

```rust
pub fn serialize_root(root: &serde_json::Value) -> Vec<u8>;      // unchanged
pub fn serialize_catalog(catalog: &CatalogDocument) -> Vec<u8>;  // NEW
pub fn serialize_config(config: &IndexFormatConfig) -> Vec<u8>;  // NEW
```

All three emit `json.dumps(indent=2, sort_keys=False, ensure_ascii=True)` form plus a single trailing
`\n`. **No second formatter is introduced** — `PythonJson` already owns the one rule the JSON
ecosystem does not implement (`ensure_ascii`) and the trailing newline is already `out.push(b'\n')`
(`wire_writer.rs:49`).

`CatalogTransaction::commit` (`index_store.rs:1029`) switches from `serde_json::to_vec_pretty` to
`serialize_catalog`. That call is the current divergence.

| Document | Python reference | Current Rust | After |
|---|---|---|---|
| root | `serialize_package_root` | `serialize_root` | unchanged — **parity already holds**, proven by `index_wire_conformance.rs` |
| `c/index.json` | `render.py:309` — `json.dumps({...}, indent=2) + "\n"` | `to_vec_pretty`: **no trailing newline**, non-ASCII raw UTF-8 | `serialize_catalog` |
| `config.json` | `render.py:334-338` — `json.dumps({"format_version":…,"name_segments":…}, indent=2) + "\n"` | *(no writer)* | `serialize_config` |

**Test:** extend `crates/ocx_lib/tests/index_wire_conformance.rs` with `catalog_fixtures_round_trip_
byte_exact` and `config_fixtures_round_trip_byte_exact`, reading vendored fixtures under
`tests/fixtures/index_wire/{catalog,config}/` generated by the Python renderer and pinned by the same
`SOURCE_COMMIT` discipline as the root vectors.

> **Corrected — `ensure_ascii` is vacuous for these two shapes.** An earlier revision mandated "at
> least one catalog fixture carrying a non-ASCII package key". That fixture **cannot be generated**:
> the Python renderer validates every key against `PACKAGE_ID_RE`
> (`validate_entry.py:70-72`, `[a-z0-9]` plus separators), so a non-ASCII catalog key is unreachable
> upstream, and `config.json`'s Python form (`render.py:334-338`) has no string field at all. A
> hand-written fixture would prove nothing — the plan's own risk row says so. The mandate is dropped.
> **The divergence actually being closed for `c/index.json` is the trailing newline: one byte.**
> `ensure_ascii` still comes free with `PythonJson` and still matters for *roots*, which do carry
> free-form strings; it is simply not testable on these two documents.

**Drift-gate extension — required, not optional.** `test/scripts/sync_index_conformance.sh` compares
the vendored tree with `diff -r` against a scratch dir built **only** from its `dir_leaves` /
`file_leaves` lists (`:33-40`, `:193`). New `tests/fixtures/index_wire/{catalog,config}/` dirs appear
in neither list nor in `ocx_authored`, so `--check` would report "Only in dest" and **fail
permanently from the day the fixtures land**. Worse, there is no upstream `golden/catalog|config`
family to vendor from: the only Python-emitted `config.json` / `c/index.json` bytes live under
`golden/render/*/expected/dist/`, which the script lists as `unvendored` (`:46`). So this contract
also requires: add `render/<case>/expected/dist/config.json` and `render/<case>/expected/dist/c/index.json`
as `file_leaves`, and treat the script as part of the same work package as the fixtures.

> **Corrected — do *not* narrow the `render` entry in `unvendored`.** An earlier revision required
> that, and it is both unexpressible and unnecessary. `assert_every_upstream_path_is_claimed` **OR**s
> a `dir_leaves`/`file_leaves` match against an `unvendored` match; there is no negation primitive, so
> "`render` except these two files" has no syntax in that list. It is also moot: the two new
> `file_leaves` entries claim those paths on their own, redundantly with the `render` catch-all, and
> `place_leaves` — what `diff -r` actually compares — never consults `unvendored` at all. `--check`
> passes either way. Update the `render` entry's **comment** for accuracy; leave the pattern alone.
>
> **The corpus pin must move.** `SOURCE_COMMIT` is `405c44ab`, at which the Python `config.json` is
> `{"format_version": 1}` — `name_segments` arrives one commit later (`44b087d`, upstream PR #75),
> which is the renderer C-001's Correction describes. Vendoring at the current pin would freeze a
> fixture that contradicts C-001. Re-pin to a commit at or after `44b087d`; the diff between
> `405c44ab` and local `a115289` touches exactly one other vendored leaf,
> `serializer/root/with-variants.json`, which is already vendored byte-identical (the README's
> standing "pending re-pin" note), so the re-pin has no effect on WP1's root vectors.
>
> **One case is the right number.** `config.json` is byte-identical across all eight `render/<case>`
> dirs and `c/index.json` differs only in its single package key and digest, so a second case
> duplicates an assertion rather than adding coverage. Vendor `normal`. The genuine gap — no upstream
> case renders a multi-package or empty catalog — is out of scope here and is recorded as such.

**Second test, covering the switch itself.** The fixture tests above call `serialize_catalog`
directly, so **nothing would fail if the one-line switch at `index_store.rs:1029` were forgotten** —
the divergence would survive behind a passing parity suite. So: drive a real
`CatalogTransaction` to `commit()` over a temp `IndexStore`, then byte-compare the resulting
`c/index.json` **on disk** against the vendored fixture. The end-to-end assertion is the one that
pins the switch; the direct-call assertions localize the failure when it breaks.

---

## 3. The `config.json` creation hook

### C-023 — `LocalIndex::commit_published_root` writes `config.json` when absent

> **Corrected — the hook is not in `CatalogTransaction::commit`.** An earlier revision placed the
> write-if-absent step inside `commit`'s body. That is **wrong**, and the cross-model gate caught it:
> `commit` is a shared primitive with **two** production callers today —
> `LocalIndex::commit_published_root` (`local_index.rs:722`) and
> `IndexStore::persist_recovered_catalog_entry` (`index_store.rs:553`, a **read-path** self-heal) —
> and C-007 would have made `regenerate_catalog` a third. A hook in `commit` therefore fires (a) on
> `regenerate`, contradicting C-008/C-022 and ADR Decision C's "never inject metadata into a foreign
> tree", and (b) as a side effect of `read_root`, making a *resolve* create `config.json` in a tree
> OCX may not own. (`local_index.rs:756` is the third `commit()` call but is `#[cfg(test)]`
> `seed_root_document` — not production.)

The write lives in the **update path only**, in `commit_published_root`, after the transaction
commits:

1. `transaction.commit().await?` — unchanged, including its `c/index.json.etag` cleanup
   (`index_store.rs:1019-1023`), its `catalog == original` early return (`:1025`), and its catalog
   write, now via `serialize_catalog` (C-025).
2. Then, **if `source_config_path(source)` does not exist**, write
   `serialize_config(&IndexFormatConfig { format_version: SUPPORTED_FORMAT_VERSION, name_segments:
   None })` — i.e. exactly `{"format_version": 1}` — through a new
   `IndexStore::ensure_source_config(source)` wrapping the existing private `write_bytes_atomic`
   (`:682`). If it exists, do nothing — **write-if-absent, never update**.

Ordering rationale: the config declares "this tree is an OCX index at version 1". Writing it *after*
the catalog commit means a crash between the two leaves a tree with content but no config — the
pre-change status quo, which the next update repairs. Writing it first would leave a config-only
tree claiming to be an index with nothing in it.

**Reachability, verified:** `commit_published_root` calls `transaction.commit()` unconditionally
(`local_index.rs:722`) even when `merge_root` returns `None`, so the first `index update` for a
published source always reaches step 2.

**Scope:** published sources only, by construction — a derived source writes through
`write_root_document` (`index_store.rs:656`) and never reaches `commit_published_root`. Matches the
grammar (`adr_index_indirection.md:249-250`).

**Locking:** `ensure_source_config` takes the same `lock_source("index-catalog", …)` guard the
transaction used, re-acquired for the write. It is no longer inherited, because the write now sits
outside the transaction's scope.

| Failure | Result |
|---|---|
| The write fails (I/O) | `file_error(path, io)` propagates; the catalog write has already committed, so the tree is content-complete but config-less — the pre-change status quo, repaired by the next update |
| **The lock re-acquire times out** | *(Row added during execution — the re-acquire introduces it and the table did not have it.)* Because the write re-acquires rather than inherits, `commit_published_root` takes a **second** lock **after** its catalog work committed. Under contention — a concurrent `regenerate` holds `index-catalog`/`c/index.json` across its whole run — that acquisition can exhaust `SOURCE_LOCK_TIMEOUT` (60s) and error, so `ocx index update` exits non-zero for a run whose catalog write **succeeded**. Nothing is corrupted: the tree is left content-complete and config-less, the documented status quo. The defect is the report, not the state, so the call site (WP11) **logs the timeout and returns `Ok(())`** — the next update repairs the config by construction, which is the same argument the crash row already makes |

**Test:** a fresh published subtree gets `config.json` after one `index update` (S-001); a second
update leaves it byte- and mtime-identical (S-008); a pre-seeded config is untouched (S-015); a
`read_root` self-heal against a config-less tree leaves it config-less (S-019).

### C-022 — Wire-layout containment

No code path writes `config.json` except C-023. Specifically **not**: `regenerate` (C-008), the
`read_root` catalog self-heal (`persist_recovered_catalog_entry`), `index list`, `index catalog`, or
any **read-only** resolve path — each leaves an index home byte-identical.

> **Corrected — "any resolve path" was too broad, found by WP11.** `ChainedIndex` calls
> `commit_published_root` from a **write-through** resolve (`chained_index.rs:758`,
> `LocalWritePolicy::Full` + published + tag-only), so a grow-on-resolve does create `config.json` —
> correctly, since that path is an update in all but name. C-022's load-bearing claims are unharmed:
> no **read-only** path writes it, and the `read_root` self-heal specifically does not, which is what
> S-019 pins and what made C-023's hook placement outside `CatalogTransaction::commit` necessary.
> **Binding on WP10:** do not byte-compare `config.json` across a write-through resolve — it would
> fail legitimately.

**Accepted limit, recorded rather than fixed:** the **write** path is ungated. A local tree declaring
`format_version: 2` does not stop `index update` writing v1-shaped documents into it. C-005 scopes the
uniform rule to *readers*, so this is spec-conformant — but it is the same family of asymmetry the
ADR exists to delete, and no contract named it until WP11 did. Out of scope here; a writer-side gate
is a v2 question alongside the deferred `!=` → `<=`.

**Test:** a structural test asserting exactly one `ensure_source_config` call site and that it is in
`commit_published_root`; an acceptance test byte-comparing an index home before and after `ocx index
list`, `ocx index catalog`, and a resolve that triggers the catalog self-heal.

> **Corrected twice, by the WP5 panel.**
>
> 1. **The acceptance test must be scoped to `config.json`, not the whole home.** A self-heal by
>    definition changes the catalog map, so `commit`'s `catalog == original` early return does not
>    fire and `c/index.json` **is** rewritten — and its opportunistic `remove_file` unlinks a stale
>    `c/index.json.etag` if one is present. Byte-comparing the whole index home across a self-heal
>    therefore fails, while the thing C-022 actually claims — that **`config.json`** is untouched —
>    holds. Binding on the acceptance WP (WP10).
> 2. **The structural test as prescribed is evadable, and the evasion was demonstrated.** Scanning
>    for `.ensure_source_config(` misses the UFCS form `IndexStore::ensure_source_config(self, …)`,
>    and misses entirely a writer that bypasses the wrapper —
>    `Self::write_bytes_atomic(&self.source_config_path(source), …)`. Both were added as production
>    methods and the test stayed **green**; the dotted form fails as designed, so the guard catches
>    one of three shapes. `source_config_path` is `pub`, so the bypass is reachable crate-wide. The
>    scan must match the bare identifier (excluding the `fn` declaration and doc-link forms) **and**
>    any non-test line putting `source_config_path` in a write position. The real containment is
>    `ensure_source_config`'s `pub(crate)`; the test is the tripwire, and a tripwire that green-lights
>    two of three shapes is worse than none.

---

## 4. `ocx index regenerate` — drift repair

### C-007 — `regenerate_catalog`

```rust
// ocx_lib::oci::index::regenerate
pub async fn regenerate_catalog(store: &IndexStore, source: &str) -> Result<RegenerateOutcome>

pub struct RegenerateOutcome {
    pub source: String,
    pub roots: usize,               // roots found under p/
    pub added: Vec<String>,         // root on disk, absent from the catalog
    pub corrected: Vec<String>,     // entry digest disagreed with the root on disk
    pub removed: Vec<String>,       // catalog named a package with no root on disk
}
```

**Preconditions:** `source` is contained, **is a published source**, and its subtree already exists.
It makes **no assumption that the tree was OCX-authored**: no prior `c/index.json`, no `config.json`,
roots possibly written by another implementation.

Two of those three are new, and both close holes review found:

- **Published-only, enforced at the CLI seam (C-010), stated here.** A *derived* (plain-OCI) subtree
  has no `c/index.json` **by grammar** — its catalog *is* the `p/` enumeration
  (`adr_index_indirection.md` A2; ADR Consequences). Point this function at a derived slug and it
  creates one, and under Decision A (absent `config.json` ⇒ v1) that subtree becomes both resolvable
  and enumerable when served — precisely what the grammar denies it. The library **cannot** make this
  call: the published/derived split lives in configuration (`[registries."<ns>"] index`), which
  `IndexStore` has no access to, and "has no `c/index.json`" cannot serve as the test because
  accepting exactly that is a precondition above. So the guard belongs to C-010, and this line exists
  so it is a stated obligation rather than an unwritten one.
- **The subtree must already exist, checked before `begin_catalog_transaction`.** `lock_source`
  `create_dir_all`s the source directory (`index_store.rs:341`), so a mistyped source currently
  *creates* it, walks nothing, reports `roots: 0`, and exits **0** — indistinguishable from a clean
  tree. Under C-026's addressing (store rooted at the checkout's *parent*) that also drops a stray
  empty directory beside a served checkout. A repair verb pointed at nothing is a user error, not a
  no-op: absent subtree ⇒ `file_error` on the missing path.

**Performs, inside one `store.begin_catalog_transaction(source)` critical section:**

1. `store.list_wire_repositories(source)` (`index_store.rs:722`) — the `p/` BFS with `o/` pruning.
2. For each repository, read verbatim root bytes and derive `IndexStore::root_catalog_entry` (`:428`).
   **The `repository_check` closure is `|_| Ok(())`.** `read_root_uncatalogued`
   (`index_store.rs:568-573`) takes one, and its failure propagates as a hard error through
   `read_root_inner` (`:613`). Its only existing caller passes `LocalIndex`'s `oci://`-scheme
   validator (`local_index.rs:587`) — reusing that here would hard-fail exactly the foreign trees the
   Preconditions promise to accept, and no variant in this function's error set admits that failure.
   `regenerate` validates nothing about `repository`: it never reads that field and C-021 turns on it
   touching no root's `repository`. Pinned here because the closure is easy to fill in by copying the
   neighbouring call site, and the copy is silently wrong.
3. Diff against the transaction's freshly-read map to populate `added` / `corrected` / `removed`,
   then **replace** the map with the derivation (`CatalogTransaction::catalog()`, `:962`).
4. `transaction.commit()` — which writes nothing when the resulting map equals the one read (`:1025`),
   so a clean tree costs nothing and leaves mtimes untouched.

**Network:** zero. No `IndexTransport`, no `oci::Client`, no source is constructible from this
signature.

**Idempotence:** a second call returns three empty vectors and leaves the tree byte- and
mtime-identical.

| Failure | Error | Exit |
|---|---|---|
| A root under `p/` does not parse | `Error::MalformedRootDocument { index_source, repository, cause }` | 65 |
| Source directory cannot be created or locked | `file_error` / lock timeout | I/O class |

### C-008 — Derivation semantics, and the no-delete rule

- The written `c/index.json` is exactly `{ <repository>: sha256(root bytes on disk) }` for every root
  the `p/` walk found, wrapped in `CatalogDocument` at `SUPPORTED_FORMAT_VERSION`, emitted by
  `serialize_catalog` (C-025).
- **Wholesale replacement.** An entry naming a root that is not on disk is dropped — the one drift
  nothing else can repair (`write_root` only upserts, `:993-995`; `read_root`'s recovery only adds).
- **Never replace a non-empty catalog with an empty one.** `derived.is_empty() && !previous.is_empty()`
  ⇒ hard error, before the commit. `list_wire_repositories` returns `Ok(vec![])` when the source's
  `p/` directory does not exist (`index_store.rs:729-731`), which is **indistinguishable** from a tree
  that genuinely holds zero packages — so a sparse or partial checkout, a CI job that regenerates
  before `p/` is materialized, or a C-026 store rooted one level off would each write
  `{"format_version":1,"packages":{}}` over a live served catalog and **exit 0**. The `removed` list
  names every dropped package, so the damage is loud in the report and silent in the exit code, and a
  CI script that checks only the status commits the wipe. Wholesale replacement must never be
  reachable from "I could not find the tree". Combined with the existence pre-flight above, the two
  guards close both halves: the pre-flight catches a missing *source*, this catches a missing `p/`.
- **This is derivation, not deletion.** `regenerate_catalog` removes **no** root document and **no**
  `o/` object, ever. It writes exactly one path — `c/index.json` — and **inherits `commit`'s
  stale-etag cleanup**: `commit` unconditionally `remove_file`s `c/index.json.etag` *before* its
  `catalog == original` early return (`index_store.rs:1013-1026`, failure ignored on purpose). So the
  idempotence claim below is exact from the second run onward; a **first** run against a tree written
  by an older ocx also drops that one stray file. Neither a root nor an `o/` object, so
  merge-only-never-delete is intact and S-009 holds — but "changes nothing on a clean tree" is not
  literally true on that first pass, and a test asserting byte-identity must seed no `.etag`.
- **It does not write `config.json`.** `name_segments` is an operator declaration OCX cannot derive
  from a tree; injecting a guess into a foreign tree under repair would be wrong. Creation is
  C-023's job alone — and C-023's *placement* in
  `commit_published_root` rather than in the shared `CatalogTransaction::commit` is what makes this
  claim true rather than aspirational. Asserted by S-019.
- **Neither a symlinked root document nor any symlinked directory under `p/` is enumerated.**
  `list_wire_repositories` branches on `file_type.is_dir()` and then `!file_type.is_file()`
  (`index_store.rs:772-782`); a symlink is **neither**, so a symlinked `p/**.json` is skipped — and a
  symlinked *directory* is never queued, taking **every root beneath it** with it in one step.
  Because this contract replaces the catalog wholesale, that is not a missing entry but silent **bulk
  removal** from `c/index.json`. `regenerate` is specified for trees whose roots and intermediate
  directories are real — which is every tree OCX produces. An operator running it against a
  symlink-deduplicated layout must know this; the CLI help and the docs (S-013) say so, and they say
  the directory case explicitly, not just the file case.
- Nothing from a remote catalog is ever persisted.

**Test:** seed a catalog with a fabricated `ns/ghost` entry and a corrupted digest for a real one;
assert `removed == ["ns/ghost"]`, `corrected == [<real>]`; assert `p/`'s file list, every `o/` object,
and `config.json` are byte-identical before and after (S-009) — and that a tree with **no**
`config.json` still has none afterwards (S-019).

### C-028 — A non-UTF-8 name under `p/` is an error, not a silent omission

*Added after the WP2 security panel found it and the WP5 architecture pass established the error home.
It was carried in the plan's WP5b row with no C-number, which is why its exit code was about to be
chosen silently by whoever implemented it.*

`list_wire_repositories` builds catalog keys from raw filesystem components: directory components
through `to_string_lossy()` and file stems through `file_stem().and_then(to_str)` with a bare
`continue`. A `p/` name that is not valid UTF-8 is therefore mangled to U+FFFD or dropped. Because
C-007 replaces the catalog **wholesale**, the dropped package is then **deleted from `c/index.json`
while its root document sits on disk, at exit 0**, reported in none of `added` / `corrected` /
`removed`. C-007 explicitly admits trees "written by another implementation", so this is reachable
input, not a hypothetical.

| Input | Result |
|---|---|
| Any path component under `p/` that is not valid UTF-8 — file stem or directory | `Err(…)` → **65** |

**The error is a new variant in `crates/ocx_lib/src/file_structure/error.rs`, classifying to
`DataError` (65).** Not `crate::error::file_error` → `Error::InternalFile` → **74**, which was the
first proposal: 74 is `IoError`, the generic fallback, and `crates/ocx_lib/src/error.rs`'s own doc
comments (`:42`, `:55`) record variants being added specifically to escape *"the generic `IoError`
(74) that wrapping in `InternalFile` forced"*. *(Citation corrected — an earlier revision attributed
those two lines to `file_structure/error.rs`, where `:42`/`:55` are merely the bounds of
`RepositoryEscapesIndexHome`. The quoted text is in the crate-root `error.rs`, about `crate::Error`'s
own variants. The load-bearing half of the argument is the next sentence and it was verified against
the file.)* All four existing variants of that enum — `MissingDigest`, `DigestMismatch`,
`MalformedRootDocument`, `RepositoryEscapesIndexHome` — classify to `DataError`, because the enum is
the home for "this tree's structure is malformed". A foreign tree's un-decodable name is that, and
labelling an operator's tree an internal fault (74) misreports whose problem it is.

**Scope:** `crates/ocx_lib/src/file_structure/error.rs` joins WP5's file set. No other wave-2 package
touches it.

**Test:** `#[cfg(unix)]`, via `std::os::unix::ffi::OsStrExt`, for **both** shapes — a non-UTF-8 package
file stem and a non-UTF-8 directory component. Assert `Err` and `classify() == ExitCode::DataError`.
Only once this holds is `regenerate`'s "raced away between walk and read" `continue` provably harmless.

### C-010 — `ocx index regenerate <REGISTRY>...` CLI contract

| Aspect | Contract |
|---|---|
| Grammar | `ocx [--index PATH] index regenerate <REGISTRY>...`; at least one registry required (clap `required = true`) |
| **Published-only guard** | Each `<REGISTRY>` must resolve to a **published** source — one carrying `[registries."<ns>"] index`. A derived (plain-OCI) namespace ⇒ **78** (`ConfigError`), naming the registry and saying a derived index has no `c/index.json` by grammar. This is C-007's published-only precondition, enforced here because only the CLI can see configuration; the library cannot. Without it `regenerate` would mint a `c/index.json` in a subtree the grammar says has none, and Decision A would then make that subtree resolvable and enumerable when served |
| Home selection | Existing root `--index` ▸ `OCX_INDEX` ▸ `$OCX_HOME/index`. **No new global flag** |
| Wrapper depth | `ocx_cli` builds no index source and opens no client; it calls `regenerate_catalog` per registry and reports |
| Report | **New payload type** — not `index update`'s (which emits none, `index_update.rs:67-73`). Per registry: `roots`, and the three name lists. Honours the root `--format json`. Plain output names every changed package; a clean run says so in one line |
| Help text | States exactly what it reports. It must **not** copy the `Index::Update` variant's stale "Packages with an update waiting are reported afterward" line, which is being deleted (the former `command/index.rs:33`, a line number since reused by live text) |
| Aggregation | Per-registry failures aggregate in input order; the lowest-index error is the process error |

### C-021 — `--frozen` and `--offline` both permit `regenerate`

`--frozen` freezes pin movement. `regenerate` consults no source, changes no `tags[].content`, and
touches no root's `repository`. `--offline` is irrelevant for the same reason. Neither gate is added.

**Test:** `ocx --frozen index regenerate <REG>` exits **0**; `ocx --offline index regenerate <REG>`
exits **0**; byte-compare every `p/**` and `o/**` file before and after to prove no pin moved.

### C-026 — Addressing a served-tree root

> **Corrected — `ocx-sh/index` is not a library caller and cannot be.** An earlier revision framed
> this contract around `ocx-sh/index` as "a real second caller, and not a CLI caller", and used that
> to justify the `ocx_lib` placement. The index repo is **pure Python** — there is no `Cargo.toml`
> anywhere in it — so it can only ever shell out to `ocx index regenerate`, i.e. exactly the CLI the
> justification said it was not. **The second-caller argument is withdrawn.** `regenerate_catalog`
> stays in `ocx_lib` on the plain ground that the logic belongs in the library and the CLI stays a
> thin wrapper — the same rule every other command follows. C-026 survives as a *capability* note
> (the shape is expressible, and `ocx-mirror` is a plausible future Rust caller), not as a
> requirement anything depends on today.

`regenerate_catalog` takes `(&IndexStore, source)` like every other store operation. A served tree
whose root **is** the source directory (`config.json` / `c/` / `p/` at a repo checkout root) is
addressed with **no new API**:

```rust
let store = IndexStore::new(checkout.parent().unwrap()).with_locks_root(&scratch_dir);
regenerate_catalog(&store, checkout.file_name().unwrap().to_str().unwrap()).await?;
```

`wire_source_dir` is `root.join(slugify(source))` (`index_store.rs:297-299`), so this addresses the
checkout exactly. Two documented caveats, not engineered around:

1. The checkout directory name must be slug-identical to itself. `to_relaxed_slug` preserves
   `[a-zA-Z0-9._-]`, so any ordinary name is.

   > **And that preservation is why the pre-flight needs a containment guard** — found by WP8.
   > `regenerate_catalog`'s existence check calls `store.source_config_path(source)`, a **pure path
   > builder** with no `ensure_source_contained`, unlike the nine other `IndexStore` entry points.
   > `to_relaxed_slug` preserves `.`, so a source of `".."` survives and the pre-flight stats the index
   > home's **parent**. Nothing reads or writes out of bounds — `begin_catalog_transaction` →
   > `lock_source` guards on the very next line — and the CLI's published-only guard (C-010) means the
   > value must be a configured namespace first, so through `ocx` it requires an operator to configure
   > a namespace literally named `..` *(corrected: an earlier revision said "unreachable through `ocx`",
   > and WP8's security review demonstrated `[registries[".."]] index = …` reaching it — the guard
   > catches it at 65, so the claim was over-stated, not the code wrong)*. It is reachable
   > through the **library**, which is the entire reason `regenerate_catalog` lives in `ocx_lib`:
   > `ocx-mirror` is the intended second caller. Reorder the guard ahead of the pre-flight.
2. `locks_root` **must** be redirected — it defaults to `root/locks` (`:62-66`), which would otherwise
   be created beside the checkout. A CI checkout is exclusive, so a scratch dir is fine.

C-026 also promotes `CatalogTransaction::catalog()` to production API. Its doc currently reads "a test
seam for putting a catalog into a chosen state" (`index_store.rs:958-962`) — re-word it in the same
work package, or the code documents a constraint the design has dropped.

**Test:** a unit test constructing the store this way against a fixture tree laid out at the root and
asserting the catalog lands at `<checkout>/c/index.json`.

---

## 5. The bulk snapshot — `ocx index sync`

### C-012 — CLI contract

> **Post-merge amendment: the flag became a verb.** `--from-catalog` shipped as a flag on
> `ocx index update` and was promoted to **`ocx index sync <REGISTRY>...`** after review of the
> shipped surface. Two grounds:
>
> 1. **The flag had to exclude its own verb's positionals.** `conflicts_with = "packages"` plus an
>    `ArgGroup` plus `--dry-run`'s own `requires`/`conflicts_with` pair is three declarations whose
>    only job is to keep two commands apart inside one name. A verb needs none of them, and the
>    `index_sync.rs` grammar tests are three lines each because there is nothing left to exclude.
> 2. **A registry list had nowhere natural to sit.** The flag was repeatable, so multi-registry runs
>    were *expressible* — `--from-catalog a --from-catalog b` — but only because the positional slot
>    was already spoken for by packages. Under a verb they are the positionals. This one is
>    ergonomics, which is a sufficient reason for a grammar and is recorded as what it is.
>
> What did **not** motivate it: parallelism. Enumeration remains **sequential**, one registry at a
> time, and the packages every named registry lists are flattened into the single bounded refresh
> loop — which is what keeps the ≤ 512 ceiling a property of the run rather than of the argument
> count. Concurrent *enumeration* would be a separate change with its own justification: during that
> phase exactly one request is in flight, so the refresh ceiling is not the reason for it either way.
> N is operator-sized and the run is dominated by the refresh, so serialized enumeration is accepted
> and its cost is recorded in the Batch rule row below.
>
> The naming caveat is recorded because `sync` overpromises to an rsync-shaped ear: the store is
> merge-only-never-delete, and nothing in this command removes anything. Every user-facing surface
> that says `sync` says that in the same breath.

| Aspect | Contract |
|---|---|
| Grammar | `ocx index sync <REGISTRY>...` — variadic, at least one required (**64** otherwise). `ocx index update <PACKAGE>...` goes back to an unconditionally `required` positional; the `ArgGroup` and both `conflicts_with` declarations are gone with the flag |
| `--frozen` | Refused by each verb's own gate, before any fetch and before any source is constructed → **81**. **One gate per command**; a test in each asserts there is exactly one |
| `--offline` | Refused by the existing `context.oci_index()?` accessor, checked first so `--offline --frozen` reports the stricter posture → **81** |
| Write scope | Each enumerated repository refreshed with a **bare** identifier ⇒ `RootScope::Package` (C-014) |
| Report | No stdout payload on the wet path; the aggregated error on stderr is the batch signal. `--dry-run` reports `CatalogPreview` and nothing else |
| Batch rule (S-020) | **Every registry is enumerated before any is refused.** One unreachable source does not cost the others their snapshot; the command still fails afterwards, and each failure is reported at the enumeration site through the shared funnel. This is why `index sync` takes a list at all — an early return on the first failure makes an N-registry run as fragile as its worst source. The cost is that a dead host's connect timeout is now paid in full for **each** dead host, in series, rather than once |
| Aggregation | An **enumeration** failure outranks a **refresh** failure whatever their argument positions: that registry contributed no work at all, while a refresh failure means the set was read and one member could not be fetched. Within each kind the lowest input index wins, so the exit is deterministic however the fan-out completed. Determinism needs an ordered input, which neither enumeration branch supplies — a published catalog is drained from a map and a registry listing is ordered by nothing this end controls — so `enumerate_catalog` sorts before either the report or the refresh sees the vector. Successful packages keep their tags |
| Duplicate registries | The **registry list** is deduplicated before enumeration, preserving argument order, so `ocx index sync a a` enumerates `a` once — one network round trip, one line under `--dry-run`, each of its packages refreshed once. Deduplicating the argument list rather than the flattened package set is what keeps the preview and the wet path agreeing about the set the command would touch. Identity is the whole identifier only in the other direction: two *different* registries serving the same repository name stay two packages |
| Empty enumeration | A registry whose catalog lists zero packages is warned about by name on stderr and does not fail the run |
| Catalog keys | Every enumerated key is validated **verbatim** against the repository grammar (`Identifier::validate_repository`) before any refresh, and a malformed key refuses that registry's enumeration fail-closed → **65**, never a filtered key list. Parsing a key and discarding the result is *not* this check: the parser splits the tag and digest off before its character-class, uppercase and length guards run, so `ns/pkg:<anything>` passes as the repository `ns/pkg` while the raw key is what becomes the identifier |
| Patch descriptors | The piggyback (C-024's shared module) runs only when the whole command succeeded, so a nine-of-ten-registry run leaves patch descriptors untouched. Consistent with `index update`, and stated because it is not obvious: the nine that worked did refresh their tags |
| Shared loop | Both verbs call `command::index_common::refresh_packages`; neither owns a fan-out. C-024's guard moved with it, and each command now also asserts it grew none of its own |

### C-013 — Enumeration source

- Enumerated **from the source, live**: `OcxIndex::fetch_catalog_strict` (`ocx_index.rs:970`,
  persists nothing) for a published source; the registry's listing via `IndexImpl::list_repositories`
  for a derived one. Never from the local copy.
- Source selection per registry reuses the existing routing at the granularity the question has:
  `OcxIndex::serves_registry` (`index_sync.rs:180`), which is `jurisdiction`'s own first arm — the one
  that answers `Outside` with no I/O — asked without a package, because there is no package yet.
  Falls back to the plain-OCI index.
- A registry whose listing endpoint refuses surfaces that source's error under the authoritative-stop
  rule — no fall-through, no empty-set success.

> **An absent `c/index.json` is not an empty catalog — added after the cross-model gate measured the
> fifth instance of this plan's recurring silent-empty-success shape.** `fetch_catalog` maps
> `IndexFetch::NotFound` to `Ok(CatalogIndex::new())` (`ocx_index.rs:957-960`), and under Decision A an
> absent `config.json` is `assumed_v1()`, so no version gate stands ahead of it either. Measured under
> the retired `--from-catalog` flag, before the verb split: a published base whose directory exists
> but holds no `c/` made `ocx index update --from-catalog <reg>` exit **0** having written nothing,
> with **zero bytes on stdout and stderr** — the operator asked to snapshot a mirror and was answered
> by silence. Reachable
> from a one-line config typo, a wrong path component in the base URL, a tree deployed before `c/` was
> published, or a CDN 404.
>
> The two facts must be distinguished at the enumeration seam: **absent catalog document ⇒ the C-013
> authoritative stop** (an error, non-zero); **served catalog with zero packages ⇒ exit 0**. The
> tolerant reading stays correct for `ocx index catalog --remote`, which is a *listing* command — so
> the smaller fix is a strict variant used only by the enumeration seam, not a change to
> `fetch_catalog`'s existing callers.
>
> **The structural guard cannot see this.** C-013's only pin until WP10 is scoped to
> `enumerate_catalog`'s own source text (`index_sync.rs`), and the swallow happens one call down
> inside `fetch_catalog`. The guard is not weak — it is watching the wrong function. That is a general
> lesson for source-text guards in this plan, and the acceptance test beside it (S-012) is what
> actually holds the rule.

**Test:** a stub fixture whose source catalog lists 3 packages while the local home holds 1 — assert 3
are refreshed. Plus the absent-catalog row above: assert a **non-zero** exit and a diagnostic, not
silence.

### C-014 — Scope per package

Bare identifier ⇒ `RootScope::Package`: adopts every tag the source lists plus package-level fields,
keeps any tag only the local copy holds. Merge never deletes (`subsystem-oci.md:248-260`). Running it
twice against a source that removed a package leaves that package's root and pins in place — a union
of snapshots, not a replica.

### C-024 — One bounded loop for both input shapes

The `JoinSet` at `index_update.rs:65-85` is replaced by a single `buffer_unordered(CONCURRENCY)` over
the resolved package list, however that list was produced.

> **Amended with C-012's verb split.** The loop moved out of `index_update.rs` into
> `command/index_common.rs`, which both `update` and `sync` call; the constant is
> `INDEX_REFRESH_CONCURRENCY` and is stated once. `index sync` flattens every named registry's
> packages into that single call rather than looping per registry — a *concurrent* per-registry
> fan-out would have multiplied the ceiling by the argument count, which is precisely what the
> structural guards in each command forbid. (A sequential per-registry loop would hold the ceiling
> fine; it is simply not what the flatten needed to be.) Each command carries a structural guard that
> it contains no fan-out of its own, and the guard is a directory scan rather than a per-file needle
> list, because a helper module hosting the fan-out satisfied every per-file form.

- `pub(super) const INDEX_REFRESH_CONCURRENCY: usize = 8` (`index_common.rs`). Nested inside
  `TAG_REFRESH_CONCURRENCY = 64` (`local_index.rs:34`, `pub` so the product can be asserted from both
  real constants rather than restated) ⇒ stated ceiling **≤ 512 in-flight requests**.
- Aggregation preserved exactly: each item yields `(input_index, Result)`, sorted, lowest returned —
  `index_common::first_failure`, shared with `index_regenerate`'s identical C-010 rule.
- The `JoinSet` panic-propagation arm disappears with the `JoinSet`; `buffer_unordered` does not
  spawn, so a panic unwinds the caller directly.
- Every failure is rendered through one funnel (`index_common::log_failure`), which neutralizes the
  subject and the error chain together; both verbs assert they emit no operator-facing failure prose
  of their own. A count of sanitizer calls against a count of log macros was the earlier form, and it
  is satisfiable by putting two sanitizer calls in one macro and paying for a second macro with none.

Also caps the pre-existing unbounded argv fan-out — a side effect of using one loop.

**Test:** a counting stub asserts peak concurrent in-flight `get` calls never exceeds 512 over a
200-package catalog; an existing-behaviour test asserts a 3-package argv run failing at input index 1
still returns index 1's error.

> **Amended: the bound is measured over two registries.** The 512 assertion is vacuous — S-004
> records why and what replaced it — and a single-registry measurement, even one taken against the
> loop's own constant, bounds only the loop's width rather than the claim this contract actually
> makes: that naming a second registry does not open a second loop. So the measurement sums peak
> in-flight across **two** index servers in one `index sync` run, where eight shared slots stay a
> summed eight while a per-registry fan-out reaches eight per source. That sum is also the only guard
> the structural denylist cannot be spelled around — a reviewer defeated every needle in it with
> `tokio::join!` over two `refresh_packages` calls, 1024 in flight.

### C-027 — `ocx index sync --dry-run`

> **Amended with C-012's verb split**, which moved every row of this contract. The flag was
> `--dry-run` on `ocx index update`, valid only alongside `--from-catalog`; it is now `index sync`'s
> own flag, and the exclusion machinery it needed is gone with the flag it excluded. The reasoning
> below is unchanged — a catalog-sized work set is the one an operator cannot read off the command
> line — and the WP8 clap finding is kept at the end as a recorded measurement, not as a live row.

`ocx index sync` is the refresh shape whose work set the operator cannot see before it runs: argv
names `index update`'s packages, a catalog does not. Mirroring a large source is the intended use, so
"what would this pull" must be answerable without pulling it.

| Aspect | Contract |
|---|---|
| Grammar | `ocx index sync <REGISTRY>... --dry-run`. No companion flag and no exclusion: the verb's only positionals are registries, so there is nothing for `--dry-run` to be incompatible with. `ocx index update --dry-run` is an **unknown argument** → **64** |
| Effect | Enumeration (C-013) runs; the per-package refresh loop (C-024) does **not**. Nothing under `$OCX_HOME/index` is opened for write, and no `CatalogTransaction` is begun — so C-023's `config.json` write does not fire either. **The command returns before the patch-descriptor piggyback** (`index_common::sync_patch_descriptors`), which runs after aggregation, does network I/O, and writes *outside* the index home — an "index home untouched" assertion alone would not catch it |
| Report | The enumerated set as a two-column registry-and-package table on **stdout**, one package per line, sorted within each registry; `--format json` emits `{"registry": "<r>", "packages": ["<ns>/<pkg>", …]}` per registry, in argument order. This is the only *refresh* shape with a stdout payload, which is why it is a flag rather than default behaviour |
| Exit | **0** on a clean enumeration, even when the served catalog lists zero packages. An enumeration failure keeps C-013's authoritative-stop error and its exit code — and per C-013's added row, an **absent** `c/index.json` is such a failure, not an empty set. A multi-registry preview with one failed registry prints **no** partial listing: half an answer presented as the answer is the same empty-set success, one registry at a time |
| `--frozen` / `--offline` | Still refused → **81**. A dry run writes nothing but still performs network I/O, and the gate is ahead of enumeration, so a frozen preview does not even contact the source |

> **The retired grammar's clap finding, kept because it was measured.** Under the flag form, the
> natural reading — `--dry-run` declared `requires = "from_catalog"` — **parsed `--dry-run cmake`
> cleanly**: clap treats a `requires` target that shares a *satisfied* `ArgGroup` with the present
> argument as already satisfied, and `from_catalog` shared the required `selection` group with
> `packages`. The "only with `--from-catalog`" half silently evaporated and the flag became
> permissive; it needed an explicit `conflicts_with = "packages"`, yielding `ArgumentConflict`. The
> verb split retired the grammar and with it the finding's application here, but the clap behaviour is
> real and worth carrying: `requires` is not a guard when an `ArgGroup` can satisfy it on the target's
> behalf.

**Test:** against the C-013 3-package stub, `index sync --dry-run` prints the 3 identifiers and a
filesystem-mtime assertion over the whole index home shows **zero** writes; the same invocation
without `--dry-run` writes 3 roots. The four refused grammars (`--dry-run` on `index update`,
`index sync --dry-run` with no registry, and both bare forms) each exit **64**, and `--offline` and
`--frozen` each exit **81** with no listing printed.

---

## 6. The `file://` transport

### C-015 — `FileIndexTransport` implements `IndexTransport`

```rust
pub struct FileIndexTransport { base_url: String, root: PathBuf }
```

Read-only; the trait has no write path.

| Condition | Result |
|---|---|
| Exists and readable | `Ok(IndexFetch::Found { bytes })` |
| `ErrorKind::NotFound` | `Ok(IndexFetch::NotFound)` |
| `ErrorKind::NotADirectory` (a path component is a file) | `Ok(IndexFetch::NotFound)` |
| `ErrorKind::IsADirectory` | `Err(IndexHttpFailed)` → **69** |
| `PermissionDenied` | `Err(IndexHttpFailed)` → **69** |
| Any other I/O error | `Err(IndexHttpFailed)` → **69** |
| **`self.root` is not an existing *directory*** | `Err(IndexHttpFailed)` → **69** — checked **before** the per-file read |

**ENOENT and NotADirectory are the only misses.** Under decision A a missing `config.json` now means
"version 1", so an EACCES mapped to `NotFound` would silently promote an unreadable tree to a valid
v1 index — a *worse* failure than before the uniform rule, which makes this row load-bearing.

**The root row is why the ENOENT row needs a qualifier.** Read literally, "ENOENT ⇒ `NotFound`"
swallows a mistyped or absent base directory: every read below a non-existent root surfaces
`ErrorKind::NotFound`, so the whole index would report as empty rather than misconfigured — this
ADR's own defect, reproduced at a new layer. ENOENT means `NotFound` **only below a root that
exists**.

> **It must test directory-ness, not mere existence.** An earlier revision said "does not exist",
> which an implementation satisfies with `fs::try_exists` — and that returns **true for a regular
> file**. Point a base at `/srv/ocx-index.json`: `join_under_root` is purely lexical and happily
> yields `/srv/ocx-index.json/config.json`, the root check passes, and every subsequent read fails
> `ENOTDIR`, which the table maps to `Ok(NotFound)`. Under Decision A that is a valid, **empty v1
> index** — the exact silent-empty-mirror this row exists to prevent, reached through a different
> door. Nothing upstream closes it: C-018 gates scheme, authority and absolute-path, never
> directory-ness. So the check is `metadata(&root).await` and refuse unless `is_dir()`.

Amend the trait doc (`ocx_index.rs:124-137` after wave 1; the ADR's `147-157` predates it):
*"Plain-HTTPS transport"* and *"a `304` is a protocol violation"* are not transport-neutral. Keep the no-conditional-GET rule. `IndexHttpFailed`'s own doc
(`oci/index/error.rs:142-145`) likewise still reads *"An **HTTP request** … (connection, TLS,
unexpected status)"*, which this transport falsifies — that amendment belongs to whichever WP owns
`error.rs`, not WP3.

**Test:** a table over all seven rows on a `TempDir` (`#[cfg(unix)]` for EACCES), asserting the
`IndexFetch` variant or `classify() == ExitCode::Unavailable` (**69**, not 75 — 75 is `TempFail`,
which nothing on this path returns).

### C-016 — Containment, no percent-decoding

- `get(url)` requires `url.starts_with(&self.base_url)` **and** that the remainder is either empty or
  begins with `/`; otherwise `IndexHttpFailed` → **69** (never a silent miss). The boundary check is
  not pedantry: with base `file:///srv/index`, the URL `file:///srv/index2/p/ns/x.json` passes a bare
  prefix test, yields the tail `2/p/ns/x.json`, and would be read as `/srv/index/2/p/ns/x.json` —
  contained, so not an escape, but a foreign-base URL silently becomes a wrong-path miss instead of a
  refusal. That is the refused-vs-absent line this whole plan defends.
- **The single leading `/` is stripped before the join.** `base_url` is stored trailing-slash-trimmed
  (`ocx_index.rs:428`) and every URL is built as `format!("{base_url}/…")` (`:645`, `:687`, `:728`,
  `:834`, `:1022`), so the remainder **always** starts with `/`. `join_under_root` rejects a leading
  separator as `PathEscapeError::Absolute` *before* it normalizes anything
  (`utility/fs/path.rs:137-139`) — so a transport that passed the tail through unmodified would refuse
  every legitimate fetch, `config.json` included, and report the entire index as refused.
- The remainder is a **literal** relative path, **not** percent-decoded: OCX builds these URLs by
  `format!` from a validated lowercase repository, an algorithm prefix, and hex (`ocx_index.rs:687,
  726-732`), so no escape is legitimately produced and decoding would create a `%2e%2e` vector.
- Joined through `utility::fs::path::join_under_root(&self.root, tail)` (utility catalog). A `..`
  sequence **that escapes the root**, an absolute component, or a Windows drive/UNC form ⇒
  `IndexHttpFailed` → **69**. Not *any* `..`: `join_under_root` folds `.`/`..` lexically and errors
  only on a residual escaping `..` (`utility/fs/path.rs:142-153`), so `a/../b` resolves to `root/b`
  and returns `Ok` — pinned by the existing test at `utility/fs/path.rs:485`. A test written from the
  older "any `..`" wording would assert a refusal the mandated primitive does not produce.
- **Symlinks are permitted only while they stay inside the tree.** A shipped copy is
  operator-managed and `rsync`/hardlink/symlink layouts are legitimate ways to stage one, so a link
  is not refused for being a link. But the resolved target is canonicalized and must remain under a
  canonicalized `root`; one that escapes ⇒ `IndexHttpFailed` → **69**.

> **Corrected — the earlier wording permitted an arbitrary-file read.** This bullet used to say only
> "symlinks inside the tree are not refused", and the transport enforced containment *lexically on
> the URL tail*. `metadata()` and `File::open` both follow links, and nothing compared the resolved
> path to the root — so a link **out** of the tree had a fully contained tail and was served.
> Measured during review: `p/kitware/cmake.json → /etc/passwd` returned `Found { 1792 bytes }`, and
> those bytes become index-document content that `persist_published_root` writes into the local
> store. `get` could read any path the process can.
>
> The trust argument ("it is the operator's own directory tree") is weaker than the workflow this
> ADR exists to enable: the air-gap story is *copy the tree to another machine*, so it arrives as a
> tarball or a git checkout — both of which carry attacker-authorable symlinks. Canonicalizing keeps
> every legitimate staging layout working, because those links stay inside the tree. And by the same
> reasoning C-017's regular-file rule already invokes, a narrowing on a Low-reversibility scheme has
> to land **now**: widening later is additive, narrowing later is not.
>
> The lexical fold still runs first and is still the primary defence — `join_under_root` normalizes
> `.`/`..` *before* the OS sees the path, so a symlinked directory component cannot be re-expanded by
> a later `..`. Canonicalization is the second gate, not a replacement for the first.
- **Scope limit, and the divergence it leaves.** That permission covers *reading* through the
  transport. It does **not** extend to `regenerate`'s enumeration: `list_wire_repositories`
  (`index_store.rs:762-798`) walks with `tokio::fs::read_dir` + `DirEntry::file_type()`, which
  reports the link's own type — `is_dir()` and `is_file()` are both false for a symlink, so the
  `if !file_type.is_file() … continue` arm at `:785` silently skips a symlinked `p/**.json` root.
  A tree that symlinks *root documents* would therefore have those packages dropped from a
  regenerated `c/index.json`. Blast radius is enumeration only — tag resolution reads roots directly,
  never through the catalog — but it is real. **Decision: leave the walk as-is and state the limit.**
  `regenerate` is specified for OCX-authored and served trees, where roots are regular files;
  symlinking a root is not a layout OCX produces. C-008 carries the operator-facing warning, and
  S-009's fixture uses regular files so the plan does not accidentally certify the symlink case.

**Test:** `<base>/p/../../etc/passwd` and `https://elsewhere/config.json` each return `Err`, never
`Ok(NotFound)`.

> **Corrected.** This line previously also demanded `Err` for `<base>/p/%2e%2e/x.json`. That is
> **unproducible given the clause above it**: without percent-decoding, `p/%2e%2e/x.json` is an
> ordinary contained relative path — `join_under_root` returns `Ok` and the outcome is `Found` or a
> clean miss. It was a leftover from the pre-correction "any `..` or encoded `..` is refused"
> reading, and left standing it invites someone to "fix" the code to match by decoding first and
> refusing after — reintroducing the very `%2e%2e` vector the no-decoding rule exists to close.
> The correct assertion is that `%2e%2e` stays **literal**: a directory literally named `%2e%2e`
> resolves, and a decoy planted at the decoded location is *not* served.

### C-017 — Size cap

Bodies larger than `MAX_INDEX_DOCUMENT_BYTES` (`ocx_index.rs:106`) ⇒ `IndexHttpFailed` → **69**.
Applied by reading at most `cap + 1` bytes, never by trusting file metadata.

**Only a regular file is read.** A directory entry that is not a regular file — FIFO, device node,
socket — ⇒ `IndexHttpFailed` → **69**, refused on a cheap `file_type().is_file()` check *before* the
counted read. The byte cap bounds memory; it does not bound **time**, and a FIFO or a stalled network
mount makes `get` block with no cancellation. The HTTPS sibling bounds time explicitly on CWE-400
grounds (`INDEX_CONNECT_TIMEOUT` / `INDEX_REQUEST_TIMEOUT`, `ocx_index.rs:108-116`); the `file://`
path has no equivalent, and C-016 deliberately permits symlinks, so an operator-staged tree reaches
such a node without any traversal. This rule is stated **now** because adding it later is a
*narrowing* — the non-additive direction on a scheme the ADR grades Low-reversibility.

**The type check must hold on the handle that is read, not on a path stat'd earlier.** `metadata(&path)`
followed by `File::open(&path)` is two independent lookups, and anything that replaces the path
between them — a concurrent `rsync` refreshing a staged tree, any local user with write access to the
index directory — passes the `is_file()` gate and then blocks in `open()`. Reproduced during review:
the pre-stat reported `is_file = true`, and after the swap `File::open` had not returned three seconds
later, with nothing to cancel it. So: keep the pre-stat (it avoids `open()`ing device nodes, which can
have side effects), open with `O_NONBLOCK` (`OpenOptionsExt::custom_flags`, a no-op on regular files),
and **re-check `file.metadata()?.file_type().is_file()` on the open handle** before reading.

**`get` is bounded in time.** Wrap it in a `tokio::time::timeout` mirroring the HTTPS sibling's
`INDEX_REQUEST_TIMEOUT` (`ocx_index.rs:108-116`); elapsing ⇒ `IndexHttpFailed` → **69**. The
regular-file rule alone does **not** bound time, and an earlier revision wrongly credited it with
covering stalled network mounts: on a stalled mount the *first* blocking call is the `metadata` stat
itself, upstream of any type check. Concretely — index root on an NFS/CIFS mount whose server
disappears — `ocx` would hang indefinitely instead of exiting 69, and each retry pins another
`spawn_blocking` thread against tokio's 512-thread pool, degrading the whole runtime rather than the
one fetch.

**Test:** `mkfifo` under the root, `#[cfg(unix)]`, asserting refusal rather than a hang; a
swap-after-stat case for the handle re-check; and a timeout case that does not depend on a real
stalled mount.

### C-018 — Closed scheme set in `resolve_base_url`, enforced on the **post-override** target

Two checks, and the order between them is the contract:

1. **Configured-base branch**, before `config::mirror::parse_url`. A `file` base is diverted here
   because it must never be host-keyed.
2. **Post-override gate.** After the `[mirrors]` index-role override has been applied and the
   effective `{protocol, host, path_prefix}` is final, the resulting scheme is re-checked against the
   same closed set. This is the arm that closes C-020's hole (below) and it is **not** redundant with
   check 1: the override replaces the scheme, so a check that only ran on the configured base is
   bypassed by any `[mirrors]` entry — including one injected through `OCX_MIRRORS`.

| Scheme | Behaviour |
|---|---|
| absent / `https` | Unchanged: `parse_url`, `[mirrors]` index-role override, https path |
| `http` | Unchanged: allowed only when the final host is in `OCX_INSECURE_REGISTRIES`; else `PlainHttpIndexNotAllowed` → **78** |
| `file` | Permitted **only** as a configured base (check 1), never as an override result. Requires **empty authority** and **absolute path**. Yields base `file://<abs>` + a `FileIndexTransport`. `[mirrors]` index-role overrides do not apply to it (host-keyed; a `file` base has none) |
| `file:///C:/` — a bare Windows drive | `InvalidIndexUrl` → **78**, the same refusal as the row below. Found by two independent reviewers on WP6: the trim leaves `/C:`, which is **non-empty**, so the filesystem-root row does not catch it, and `file_root` yields the bare designator `C:`. `Path::new("C:")` carries a `Prefix(Disk)` component with **no** `RootDir`, so `is_absolute()` is false and Win32 resolves it against the **per-drive current directory** — `metadata` then succeeds on whatever directory `ocx` was launched from, passes the transport's root-is-a-directory check, and every index document is read from the CWD. A checkout containing `p/<ns>/<pkg>.json` supplies the root document, which is the trust anchor of the whole resolve path. The OS-independent drive-letter test cannot see this: it pins the *tail-stripping* rule, not the *absoluteness* rule it was introduced to satisfy. Pin it with `#[cfg(windows)] assert!(!Path::new("C:").is_absolute())` |
| `file:///` — the filesystem root | `InvalidIndexUrl` → **78**. Every base gets a trailing-slash trim (`ocx_index.rs:405`); at the filesystem root that trim yields an **empty** path and a `file://` base that refuses every subsequent fetch. Refused as a bad base rather than accepted into a silently empty index — the same silent-empty-mirror failure C-015's root row closes at the transport, closed here at the gate *(row added during execution: WP6 hit it, the spec had not stated it)* |
| `file` on Windows | `file:///C:/srv/x` — empty authority, and the drive-letter tail is stripped of its leading `/` to yield the absolute path `C:\srv\x`. Without this row the "absolute path" requirement is undefined on Windows, where the URL tail `/C:/srv/x` is *not* an absolute path; ocx ships `x86_64-pc-windows-msvc` and `aarch64-pc-windows-msvc` (`post-release-oci-publish.yml:58`), so freezing the Unix-only reading would need a breaking change to widen. UNC stays refused by C-019 |
| anything else | `InvalidIndexUrl` → **78**, at whichever check sees it first |

**Signature: the gate and the transport are one value.** `resolve_base_url` returns `Result<String>`
today, and the transport is constructed independently at the call site
(`context.rs:750`, `Box::new(ReqwestIndexTransport::new())`). Leaving it that way would make the
caller **re-derive** the scheme by string-prefixing the returned base in order to pick a transport —
a second, independent scheme parse, which is precisely the shape of the bug this contract exists to
close. So `resolve_base_url` returns a typed `IndexBase { url: String, transport: Box<dyn
IndexTransport> }` (or equivalent), decided once. The gate and the dispatch are then the same
decision, and no WP can implement half of it.

The last table row is the gate arm that does not exist today: an unknown scheme currently flows
through `parse_url` and fails later as a transport error (75) — wrong class, wrong moment.

**Test:** with `[registries."x"] index = "https://up.example"` and `[mirrors]` (and, separately,
`OCX_MIRRORS`) mapping `up.example` to `file://localhost/srv/x`, resolution fails
`InvalidIndexUrl` → **78** and **no** filesystem read is attempted.

### C-019 — `file://` with a non-empty authority is refused

`file://host/srv/x`, `file://localhost/srv/x` ⇒ `InvalidIndexUrl` → **78**. A non-empty authority is a
UNC/remote form.

### C-020 — `[mirrors]` may not route index traffic to `file://`

> **Corrected.** An earlier revision asserted "`parse_url` is unchanged, so a `[mirrors]` `file://`
> value still fails `MissingHost`". That is **false**. `parse_url` (`mirror.rs:453-489`) splits on the
> first `://`, lower-cases whatever precedes it into `protocol`, and rejects **only an empty host**.
> `file:///srv/x` is rejected (empty authority ⇒ `MissingHost`), but `file://localhost/srv/x` parses
> **today** as `{protocol: "file", host: "localhost", path_prefix: "srv/x"}`. No scheme allowlist
> exists anywhere in that parser.

The hole is inert on `main` — nothing consumes a non-`http(s)` protocol — and **this work is what
makes it reachable**, because C-015 introduces a transport that acts on `file`. Therefore:

- `parse_url` stays **unchanged** (it is shared with the registry role and is not the right layer for
  an index-role policy).
- The refusal lives in the **post-override gate of C-018 check 2**, on the effective index target.
  Both `[mirrors]` table entries and `OCX_MIRRORS` environment entries flow through
  `resolve_mirror_map` into that same target, so one gate covers both.
- Exit **78** (`InvalidIndexUrl`), and the diagnostic names the mirror entry, not the configured base
  — the operator's mistake is in `[mirrors]`.

**Test:** a regression guard asserting `parse_url("file://localhost/srv/x")` still returns `Ok`
(pinning that the shared parser was deliberately *not* changed) **paired with** the C-018 test
asserting the resolution it feeds is refused. The pair is the contract; either alone misleads.

---

## 7. Scenarios

### S-001 — The air-gap pipeline, end to end

Connected machine: `ocx index sync ocx.sh`. Copy `$OCX_HOME/index/ocx.sh/` to a host
serving static files. On a clean air-gapped machine with `[registries."ocx.sh"] index =
"https://mirror.corp/ocx-index"` and `[mirrors]` pointing artifact traffic at the corp registry,
`ocx package install cmake:3.28` resolves to **the same platform-manifest digest** the connected
machine pinned. Also assert the copied subtree is byte-identical to `wget --mirror` of the served tree
**including the source root** — the criterion `adr_oci_index_only_dispatch.md:761-762` states at
package level.

### S-002 — The defect, and its fix, asserted as a pair

Serve a tree built by a **pre-change** ocx: every resolve reports not-found, silently, for packages
the tree contains — assert the `p/<ns>/<pkg>.json` file exists on the server's disk while the client
reports absence, and that the access log records `config.json` and **no** `p/…` request. Rebuild with
a post-change `ocx index update`; re-serve; the same commands resolve. One test, both halves.

### S-016 — A tree with no `config.json` at all now resolves

The decision-A behaviour change, isolated from S-002 because it is the one users may notice as a
*semantic* change rather than a fix: serve a tree from which `config.json` has been deleted. Every
package resolves, and the access log shows the `config.json` 404 followed by the `p/…` GET that today
is never issued. Pair it with the negative: the same tree with `format_version: 2` exits **65**.

### S-003 — Serving with no server at all

`[registries."corp"] index = "file:///srv/ocx-index/corp"`. `ocx package install corp/tool:1.2`
resolves the tag through the index and fetches the leaf from the corp registry. No HTTP server, no
TLS, no `--index` redirection.

### S-004 — Fleet snapshot

`ocx index sync ocx.sh` over a 42-package catalog: 42 roots + dispatch objects land,
plus one `config.json` at the source root. Peak in-flight requests stay at or below 512.

Then the same ceiling over **two** registries in one invocation, the same 42 packages of work split
between them, asserting the **summed** peak across both sources rather than either one's. That sum is
the number a per-registry fan-out actually moves, and the number no spelling of the fan-out can
evade — which is what makes this half, and not the source-text denylists beside it, the thing holding
C-024's multi-registry claim up.

> **The `≤ 512` bound is vacuous, here and in C-024's own prescribed test.** 42 single-tag packages
> cannot reach 512 even with the fan-out removed entirely, so the assertion passes against an
> unbounded implementation — WP10 measured exactly that. C-024's 200-package version is vacuous for
> the same reason. Two things are needed for a bound that bites: assert against
> `INDEX_REFRESH_CONCURRENCY`, not 512, and make the fixture **hold** each request long enough for
> overlap to exist — without a hold the peak is 1 however wide the loop is. WP10's measurements, kept
> in the test so the numbers are reproducible rather than folklore: 7 real / 15 mutated at a 20 ms
> hold, 8 / 26 at 200 ms. Reaching 512 at all would need multi-tag roots to exercise the nested
> 64-wide fan-out.

### S-005 — Policy asymmetry, asserted together

`ocx --frozen index sync ocx.sh` exits **81** and the home is byte-identical
afterwards, with no network request (the gate precedes source construction). `ocx --frozen index
regenerate ocx.sh` exits **0** and rewrites the catalog. Asserting both in one test is what documents
the asymmetry as intentional.

### S-006 — An unsupported wire version is refused identically on three paths

A subtree declaring `{"format_version": 2}` exits **65** read as `$OCX_HOME/index/<src>/`, via
`--index`, over `https://`, and via a `file://` base — with the **same** message on all four. Same
code on all four is what proves no reader has its own rule. (A fifth variant carries an unmodelled
sibling key alongside `format_version: 2`, asserting the message does not change: no field of the
config participates in the diagnostic.)

### S-007 — Unreadable is not absent

A `file://` tree with `chmod 000 config.json` exits **69** naming the path. It must **not** exit 0,
must not report not-found, and — post-decision-A — must not be treated as an absent config and
silently promoted to v1. Repeat with `chmod 000` on `p/<ns>/<pkg>.json`.

> **Corrected twice by WP10, which could not write this test as worded.** The exit code was **75**;
> C-015's table and its own test note both say **69**, and 75 is `TempFail`, which nothing on this
> path returns — the same defect already fixed in C-015 and missed here. And the command was
> `ocx index list <pkg>`, which **cannot observe the failure**: it reads the local index only, so for
> a package the home lacks it warns and exits **0** without ever contacting the source. Verified by
> experiment before the test was written. Use `ocx index update`.

### S-008 — A repeat update is byte- and mtime-stable

Run `ocx index update <pkg>` twice against an unchanged source. Every file under the source subtree —
`config.json` included — has unchanged bytes **and** mtime.

### S-009 — `regenerate` repairs a catalog without touching content

Hand-edit `c/index.json` to add `ns/ghost` (no root on disk) and corrupt one real entry's digest.
`regenerate` reports `removed: ["ns/ghost"]`, `corrected: [<real>]`; afterwards every `p/**` file,
every `o/**` object, and `config.json` are byte-identical.

### S-010 — Old and new binaries share a tree

A tree carrying `config.json` is read by a binary predating this change: it ignores the file and
resolves as before. A tree produced by an old ocx gains `config.json` on its next update.

### S-011 — A bad scheme fails at the right moment, in the right class

`[registries."ns"] index = "ftp://x"` makes every ocx command that **builds index sources** exit **78**
at context initialisation naming `ns` — not 75 at the first index fetch, and not a silent fall-through
to plain OCI.

> **"Every ocx command" overreaches** — WP10's correction. Under `--offline` no index sources are
> built, so the gate is never reached and the command exits **0**. A test written from the original
> wording would exercise three commands and claim all of them. Assert the qualified rule and record
> the `--offline` case as the boundary, rather than quietly narrowing the sample.

### S-012 — A registry that refuses enumeration

`ocx index sync <registry whose catalog endpoint requires auth>` surfaces that
registry's error and a nonzero exit; it does not report success over zero packages. Packages refreshed
before the failure keep their pins.

### S-013 — Help text tells the truth

`ocx index update --help` contains no sentence claiming updates are reported afterward (the former
`command/index.rs:33`, deleted — the number now carries live text). `ocx index regenerate --help`
describes exactly the report it produces.

### S-014 — Yank markers ride the snapshot for free

A source root marking `tags["1.2.3"].yanked` is snapshotted and served. A client resolving `pkg:1.2.3`
against the served tree is refused unless `OCX_ALLOW_YANKED` is set — identical to resolving against
the origin. No yank-specific code exists in this change; `surface_root_status` (`ocx_index.rs:851`) is
the one shared gate.

> **Unrunnable as worded — the yank gate fires on the *snapshotting* machine.** WP10 found that
> `ocx index sync` over a catalog holding a yanked tag exits **65** and snapshots nothing, so a test
> written from the scenario text snapshots nothing and then asserts about a copy that does not exist.
> Mirroring a yanked tag requires the **mirror operator's own** `OCX_ALLOW_YANKED=1` at snapshot time;
> the client-side refusal this scenario is about only becomes observable after that. That is a real
> operator-facing consequence — a corporate mirror cannot faithfully mirror a yanked tag without
> opting in — and it belongs in the docs, not just the test.

### S-015 — An existing `config.json` is never rewritten

Seed a subtree with a hand-written `config.json` carrying an unmodelled key and a `name_segments`
value OCX would not have chosen. Run `ocx index update <pkg>` and then `ocx index regenerate`. The
file is byte-identical after both. This is what makes a verbatim `rsync` of a hosted tree survive
contact with a local update, and what keeps `regenerate` safe to point at a foreign repo.

### S-017 — Cross-language byte parity for all three documents

Vendored fixtures generated by `ocx-sh/index`'s renderer round-trip byte-exactly through
`serialize_root`, `serialize_catalog`, and `serialize_config`. At least one catalog fixture carries a
non-ASCII package key (proving `ensure_ascii`) and the suite fails against the old `to_vec_pretty`
emitter on the trailing newline alone.

### S-018 — Previewing a catalog mirror before committing to it

An operator about to mirror an unfamiliar source runs
`ocx index sync ocx.sh --dry-run --format json`, reads the package list, and decides.
Assert: the JSON lists every package the source catalog holds, the index home is untouched
(no new files, no mtime change on existing ones, no `config.json` created), **`sync_patches` never
ran** (assert with `[patches]` configured and online, so the piggyback would otherwise fire), and
exit is **0**.
Re-running without `--dry-run` writes exactly the previewed set. `--offline` with `--dry-run` is
**81**. *(Amended with C-012's verb split: the old "`--dry-run` with a positional package is 64" row
described the flag's exclusion rules, which no longer exist — `index sync`'s positionals are
registries and `index update` no longer carries the flag. The grammar tests in each command file
pin what replaced it.)*

### S-020 — Several registries are one run, and one failure does not void it

`ocx index sync <healthy> <unreachable>` — a second registry whose published base serves no
`c/index.json`. Assert, in **both argument orders**: exit **69**, the healthy registry's roots on
disk afterwards, and the failing registry named on stderr. Both orders, because a run that only works
when the healthy registry comes first is passing by accident of the loop rather than by contract.

The same invocation with `--dry-run` also exits **69** and prints **no partial listing**: half an
answer presented as the answer is the empty-set success C-013 forbids, one registry at a time.

### S-019 — Only a refresh creates `config.json`

Against a served/foreign tree that has roots and a catalog but **no** `config.json`, assert it is
still absent after each of: `ocx index regenerate <r>`, `ocx index list`, `ocx index catalog <r>`, and
a resolve that triggers the `read_root` catalog self-heal (`persist_recovered_catalog_entry` —
provoked by deleting one catalog entry while its root stays on disk). Then run `ocx index update` on
the same tree and assert it appears. This is C-022's containment claim made executable, and it is the
scenario that would have failed the pre-correction design.

The verb is `update` there only because refreshing one named package is the cheapest refresh to run.
`ocx index sync` creates the file on identical terms and by the same route — C-023's hook sits in
`commit_published_root`, which both verbs reach through the shared refresh loop — so what this
scenario separates is a **refresh** from a read or a repair, never `update` from `sync`.

---

## 8. Coverage map

| Change | Contracts | Scenarios |
|---|---|---|
| Decision A — uniform version rule | C-001, C-003, C-004, C-005 | S-002, S-006, S-007, S-016 |
| Byte-exact serialization (F) | C-025 | S-017 |
| `config.json` creation hook | C-022, C-023 | S-001, S-008, S-010, S-015, S-019 |
| `regenerate` | C-007, C-008, C-010, C-021, C-026 | S-005, S-009, S-013, S-015 |
| `ocx index sync` | C-012, C-013, C-014, C-024, C-027 | S-004, S-005, S-012, S-018, S-020 |
| `file://` transport | C-015, C-016, C-017, C-018, C-019, C-020 | S-003, S-007, S-011 |
| ~~Advisory version field~~ | *withdrawn — C-006 tombstoned* | — |
| Cross-cutting | C-022 | S-013, S-014 |

## 9. Explicit non-contracts

- No provenance-gated version check, and no `AbsentConfig` parameter — one rule over bytes.
- `regenerate` never writes `config.json` and never removes a root or an `o/` object.
- No public `IndexStore::write_source_config` (C-002 withdrawn). The one writer is
  `IndexStore::ensure_source_config` — `pub(crate)`, write-if-absent only, no update path, and one
  production call site (`commit_published_root`). It is crate-visible rather than private because
  C-023's correction moved its caller into a sibling module.
- A present `config.json` is never updated — no command rewrites one, including `regenerate`. The
  repair path for a wrong `config.json` is manual: delete it and re-run `update`.
- **No `min_ocx_version` field** (C-006 withdrawn). OCX writes exactly `{"format_version": 1}`, and
  an unmodelled `min_ocx_version` in a foreign config is ignored like any other unknown key.
- No `generate` verb, no separate `mirror`/`snapshot` verb, no `--all` flag.
- No new `IndexStore` constructor for served-tree roots (C-026 uses the existing one).
- No write path on `IndexTransport`, and no `file://` write transport.
- No `index catalog --filter`. `--format json` exists.
- No typed root-mutation API; `serde_json::Value` + `serialize_root` remains the root rewrite surface.
- No all-or-nothing transaction across packages under `ocx index sync`.
- No `file://` support in `[mirrors]`.
- No change to `SUPPORTED_FORMAT_VERSION` or the `!=` comparison.
- This ADR does not change `ocx-sh/index`; adopting `regenerate` there is that repo's decision.
