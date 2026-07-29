# ADR: Record the Variant Set on the Index Root

## Metadata

**Status:** Accepted
**Date:** 2026-07-29
**Deciders:** mherwig
**Beads Issue:** N/A
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md`
**Domain Tags:** api | data | integration
**Supersedes:** N/A (amends [`adr_variants.md`](./adr_variants.md) § "Index Changes")
**Superseded By:** N/A

## Context

A **variant** is a build of the same version with different software-level
characteristics — an optimisation profile, a feature set, a libc.
[`adr_variants.md`](./adr_variants.md) decided the encoding: a tag prefix.
`slim-3.13.1` is the `slim` variant of `3.13.1`; an unprefixed `3.13.1` is the
*default* variant. By the time a variant reaches ocx or the public index it is
nothing but a hyphen-prefixed tag string.

That ADR's § "Index Changes" concluded:

> The index lists all tags including variant-prefixed ones. No structural
> changes to `IndexImpl`. A future `list_variants()` convenience method could
> parse tags to extract unique variant names, but this is not required for the
> initial implementation.

and its Phase 4 notes recorded "variant info is fully recoverable from tag
names, **no consumer exists yet**". Both were written 2026-03-22, before
`ocx-sh/index` existed. Three consumers exist now, and each re-implements the
version grammar to answer "does this package ship variants":

| Consumer | Implementation | Note |
|---|---|---|
| `ocx index list --variants` | `Version::parse` (Rust) | the reference derivation |
| `ocx-sh/index` package page | `site/.vitepress/theme/utils/version.ts` | a hand port; its own docblock records "this port isn't mechanically checked against the Rust source (no shared test-vector fixture, no CI cross-check)" |
| `ocx-sh/index` bot | `core/version_order.py::_VERSION_RE` | a *narrower* grammar — no prerelease/build — used only to *exclude* variant tags, never to enumerate them |

The observable gap is not the package detail page: `buildVersionTable` already
groups tags into one `VariantRow` per variant, default first, and
`VersionTree.vue` renders the label. It is everywhere else. The index root's
`tags` object is a flat `{tag: {content, observed, yanked?}}` map with no
variant-aware field, and the catalog grid's view-model has no variant concept
at all — its `latestVersion` comes from `find_latest_version`, which
deliberately skips variant-prefixed tags. So a package that ships `slim` is
indistinguishable, in the listing and on the wire, from one that does not.

## Decision Drivers

- A consumer of a root should not have to re-implement the version grammar to
  learn that a package ships variants.
- The index root is a **published wire format**. Additive and optional only;
  every already-published root must keep validating and keep resolving, and a
  consumer that ignores the new field must see no behaviour change.
- Two writers author roots — ocx's `package announce` and the index bot's
  `announce`/`reconcile` — and the index CI byte-compares the committed root
  against its own re-serialization. Whatever is added must be spelled
  identically by both.
- No second source of truth. If the set is derivable from `tags`, anything
  recorded is a projection, and a test must hold the two together.

## Industry Context & Research

**Research artifact:** N/A — the relevant prior art is inside this repo.
**Trending approaches:** Registries that model variants at all (Docker Hub's
`-slim`/`-alpine` convention, conda's build strings) leave them in the tag
string and let clients parse. None publish a derived variant index.
**Key insight:** The closest precedent is one field away in the same document.
`source` (added 2026-07) is bot-derived, optional, omitted-when-absent, and
re-derived on every `regenerate` rather than carried over — it is a projection
of the observed manifests exactly as `variants` is a projection of `tags`. Its
schema description states the contract that made it safe: "never in `required`,
so every root published before this field existed stays valid." Following an
established shape in the same schema costs no innovation token.

## Considered Options

### Option 1: Record nothing; leave every consumer deriving

**Description:** Keep `adr_variants.md`'s original decision. Fix only the
visible gap by having the catalog view-model derive variants at render time
(the site already owns a version parser).

| Pros | Cons |
|------|------|
| Zero wire change, zero schema change, zero cross-language drift surface on a new field | The stated goal — a root consumer that need not re-implement the grammar — is not met |
| No fourth parser; the three that exist already have one | The catalog derivation would be a *fourth* call site of the drifting TS/Python ports, adding to the problem it works around |
| Cannot go stale — nothing is cached | A consumer outside these three repos still has to port the grammar |

### Option 2: `variants` array of names on the root (chosen)

**Description:** An optional `variants: ["musl", "slim"]` on the package root,
sorted and deduplicated, derived from the tag names, written by both writers in
one fixed slot, omitted entirely when empty.

| Pros | Cons |
|------|------|
| Answers the question in one field read, in the document the consumer already has | Adds a second Python derivation and a cross-language agreement to maintain |
| Follows `source`'s established additive/optional/re-derived shape | A projection can go stale if a writer forgets to re-derive |
| Makes the catalog grid possible without any parsing at render time | Costs a schema change and a coordinated two-repo merge order |

### Option 3: Structured variant objects (`{name, latest, tagCount}`)

**Description:** Record per-variant metadata, not just names.

| Pros | Cons |
|------|------|
| A richer page could render without touching `tags` | Every added field is another thing to keep in sync with `tags`, and none of them is needed today (YAGNI) |
| | `latest`-per-variant duplicates what the alias-chain logic already computes correctly, and would be the *second* place a "which tag is newest" rule lives |

### Option 4: Declare variants in `ocx.toml`/mirror spec and carry the declaration through

**Description:** Treat the variant set as publisher intent, authored upstream,
carried into the root rather than derived.

| Pros | Cons |
|------|------|
| Could name a variant that ships only a bare rolling tag, which no derivation can see | A genuine second source of truth: the declaration and the tags can disagree, and the index has no way to adjudicate |
| | The mirror spec's `variants` block is an *authoring* concept that does not survive to the registry; reconnecting it would be new plumbing on the announce path |

## Decision Outcome

**Chosen Option:** Option 2.

**Rationale:** The recorded field is a *projection*, and the whole design hangs
on that word. It is derived, never declared (rules out Option 4's disagreement
class); it carries names only, nothing a second rule could compute differently
(rules out Option 3); and it is re-derived on every regeneration by both
writers, so it cannot outlive the tags that justified it.

Option 1 is the honest competitor and deserves the record: the variant set *is*
derivable, the three consumers that exist today all already own a parser, and a
fourth-party consumer is speculative. The case against it is that the derivation
is not one rule — it is three implementations of one rule, one of which
(`version.ts`) documents its own unverified-port risk and one of which
(`_VERSION_RE`) is already narrower than the others. Recording the answer does
not remove those parsers, but it does give them a shared, byte-pinned oracle:
the vendored `with-variants.json` vector now fails if the Rust and Python
derivations disagree, which nothing checked before.

**Shape:**

```jsonc
{
  // … name, repository, owners, status, deprecated_message, created,
  //    desc, upstream?, superseded_by?, source?
  "variants": ["musl", "slim"],   // optional; omitted when empty, never []
  "tags": { /* … */ }             // always last (CONTRACTS §14)
}
```

Four sub-decisions, each load-bearing:

1. **Named variants only; the default is never listed.** The default variant is
   the *absence* of a prefix, so it has no name. `ocx index list --variants`
   renders it with an empty-string placeholder; that placeholder is a display
   artifact and the schema rejects it on the wire (`pattern` refuses `""`).
2. **Omitted when empty, never `[]`.** One state, one spelling. Decisively:
   an unconditional `"variants": []` changes the bytes of every root published
   before this field existed, so the next announce of each would miss the C6
   unchanged short-circuit and open a pull request per package. The schema's
   `minItems: 1` refuses the other spelling outright.
3. **Slot: after `source`, immediately before `tags`.** CONTRACTS §14 fixes key
   order and keeps `tags` last; the index CI byte-compares, so any other
   position fails the gate.
4. **Re-derived, never carried over.** Same rule as `source`: a variant whose
   last tag disappears upstream leaves the root in the same run.

### Quantified Impact

| Metric | Before | After | Notes |
|--------|--------|-------|-------|
| Reads to learn a package's variant set | 1 root fetch + a full version-grammar port | 1 root fetch | The port stays for ordering/grouping; only this question stops needing it |
| Bytes added to a root with no variants | — | 0 | Key omitted; every existing root is byte-identical |
| Bytes added to a root with variants | — | ~20/variant | Once, then stable |
| Implementations of the variant rule | 3, mutually unchecked | 3, two of them byte-pinned to each other | `version.ts` remains unpinned — see Risks |

### Consequences

**Positive:**
- The catalog grid can show variants by reading a field, and does.
- `ocx index list --variants` and `package announce` now call one function
  (`ocx_lib::package::version::variant_names`); they were separate before.
- The Rust and Python derivations are pinned to each other by a shared golden
  vector whose tag set is chosen to make disagreement fail
  (`musl-3.13.1-rc1` alone reds the narrower `_VERSION_RE`).

**Negative:**
- A second Python derivation exists (`version_order.variant_names`),
  deliberately not reusing `_VERSION_RE` — that pattern predates
  prerelease/build support and would drop `slim` from a package whose slim tags
  carry `-rc1`; widening it instead would change what `core/anomaly.py` treats
  as a pinned release.
- Two repos must merge in order (see Risks).

**Risks:**
- **Cross-repo merge order.** ocx writing `variants` before the index repo
  accepts it fails the byte gate on every announce PR. Mitigation: the index
  change merges first; the vendored-fixture README records the dependency, and
  no fleet package ships a variant tag today, so nothing is blocked meanwhile.
- **`version.ts` stays unpinned.** The site's port is still checked by nothing.
  This ADR does not fix that; it narrows the blast radius (the port no longer
  owns the "does this ship variants" answer) and leaves a shared vector that a
  future TS contract test can consume.
- **A bare rolling tag is invisible.** A package publishing only `slim` (no
  `slim-<version>`) records no variant, because `slim` is not a version. The
  site infers it in a second pass from a versioned sibling; the recorded field
  does not, and the schema description says so. Accepted: inferring publisher
  intent from a non-version tag is exactly the declaration Option 4 was
  rejected for.

## Amendment (2026-07-29): one writer, not two — ocx records nothing

The "Cross-repo merge order" risk above was resolved in the other direction. Two
`ocx-sh/index` pull requests changed what the field is for:

- **#110** — the bot **derives** the catalog's variant set from `root.tags`
  instead of reading the stored field. Nothing downstream consumes what is
  written any more.
- **#112** — the pull-request gate accepts an **absent or empty** `variants`
  unconditionally, while a **present** one is still held to the derivation in
  both directions.

That makes the field vestigial on the ocx side, and the two-writer byte-parity
requirement — the only reason ocx wrote it — no longer exists. ocx therefore
records none. Everything else stands: the field is still in the schema, the
bot still writes it, and `variant_names` is still the one Rust derivation that
`ocx index list --variants` and the cross-language golden vector pin.

The change is a **removal, not a silence.** `regenerate` clones the committed
root, so merely dropping the write would carry a stored set through verbatim —
and the first time a variant's last tag left upstream, the carried-over set
would stop matching the derivation and #112's gate would reject the announce,
which is the exact failure the staging was designed to avoid. `regenerate`
unconditionally `shift_remove`s the key.

Blast radius: exactly one live root carries it today
(`p/astral-sh/python-build-standalone.json` → `["slim"]`, verified by scanning
all 11 roots). It self-heals on its next announce — one pull request, no sweep.

Sub-decisions 1-4 of the Decision above now describe the **bot's** contract
only. Sub-decision 4 ("re-derived, never carried over") is what ocx's
unconditional removal preserves from the other side.

## Technical Details

### Architecture

```
tag names ──► package::version::variant_names   (Rust, ONE derivation)
                 │
                 └─► ocx index list --variants   (+ "" placeholder for default)
                                                     ┆ must agree
tag names ──► version_order.variant_names (Python) ──┘
                 └─► regenerate() ──► PackageRoot.variants   (the ONE writer)
                                          │
                                          └─► render._catalog_entry (derives from tags)
                                                └─► /data/catalog/catalog.json ──► PackageCard.vue

ocx announce::pipeline::regenerate ──► shift_remove("variants")   (never writes)
```

### API Contract

```rust
// crates/ocx_lib/src/package/version.rs
pub fn variant_names<'a>(tags: impl IntoIterator<Item = &'a str>) -> Vec<String>;
```

```python
# bot/src/indexbot/core/version_order.py
def variant_names(tags: Iterable[str]) -> tuple[str, ...]: ...
```

### Data Model

`schema/root.schema.json`: `variants` — `array`, `minItems: 1`,
`uniqueItems: true`, items `pattern: ^[a-z][a-z0-9.]*$` with
`not: {const: "latest"}`. Not in `required`.

## Implementation Plan

1. [x] `ocx_lib::package::version::variant_names` — the one Rust derivation.
2. [x] `ocx index list --variants` routed through it (was a private copy).
3. [x] ~~`announce::pipeline::apply_variants` — record it in the `tags`-adjacent slot.~~
   Superseded by the Amendment: `regenerate` removes the key and never writes it.
4. [x] `schema/root.schema.json` + 1 valid / 5 invalid fixtures with recorded reasons.
5. [x] `model.PackageRoot.variants`, codec (`parse`/`serialize_package_root`), `regenerate`.
6. [x] `core/render.py::_catalog_entry` reads the field; `PackageCard.vue` renders it.
7. [x] Golden vector `with-variants.json`, vendored into ocx as a cross-language pin.
8. [x] `CONTRACTS.md` §14 key order; `entry-schema.md` reference page.
9. [ ] Re-pin `SOURCE_COMMIT` once the `ocx-sh/index` PR merges.
10. [x] Amendment: drop ocx's write; `regenerate` removes the key unconditionally.

## Validation

- [x] Old roots still validate: the whole pre-existing valid-fixture corpus
      (no `variants` key) passes `task schema:validate` unchanged.
- [x] Malformed new field rejected *for the recorded reason*: `[]`, `""`,
      `latest`, `Slim`, and a duplicate each fail with their own asserted
      message, not merely "fails".
- [x] Projection == derivation, asserted in Rust, Python, and across languages
      through the vendored vector.
- [x] Every behaviour shown red before green by source mutation with grep proof
      that the mutation landed.
- [ ] Security review — not applicable; no new input is trusted (the field is
      derived from tags the index already validates, and is never read back to
      make a resolution decision).

## Links

- [adr_variants.md](./adr_variants.md) — the tag-prefix encoding; this ADR
  amends its § "Index Changes"
- [adr_announce_publisher_surface.md](./adr_announce_publisher_surface.md) —
  the announce pipeline this field is written by
- [adr_index_indirection.md](./adr_index_indirection.md) — the root wire format
- `ocx-sh/index` `bot/CONTRACTS.md` §14 — the byte-exact root serializer spec

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-07-29 | mherwig | Initial decision |
| 2026-07-29 | mherwig | Amendment: `ocx-sh/index` #110 derives the set and #112 always accepts an absent one, so ocx stops recording `variants` and unconditionally removes the key |
