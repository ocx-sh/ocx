# ADR: Platform Model Unification — One Compatibility Relation, One String Grammar, One Lock Version

## Metadata

**Status:** Accepted
**Date:** 2026-07-17
**Deciders:** mherwig (decisions locked and user-approved; this ADR specifies them)
**Beads Issue:** N/A
**Related issues:** [#215](https://github.com/ocx-sh/ocx/issues/215) (platform inconsistencies / lock unit), [#212](https://github.com/ocx-sh/ocx/issues/212) (list of platforms)
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Follows Golden Path in `.claude/rules/product-tech-strategy.md` (Rust 2024 / Tokio; no new external dependency — collapses existing code paths)
**Domain Tags:** oci, platform, resolution, project, cli
**Supersedes:** N/A (amends `adr_platform_libc_os_features.md` §180 and cross-checks `adr_cascade_platform_aware_push.md`)
**Superseded By:** N/A

---

## Context

The `Platform` type (`crates/ocx_lib/src/oci/platform.rs`) accreted **three independent string
encodings** and **two independent compatibility truths** as libc/`os.features` support, the V2
per-platform lock, and dependency pinning each landed. The result is the inconsistency class
issue #215 names and the multi-platform authoring surface issue #212 wants removed.

### The three encodings (all describe the same `Platform`)

| Encoding | Where | Shape | Problem |
|---|---|---|---|
| `Display` / `FromStr` / CLI `--platform` | `platform.rs:714-832` | `os/arch[/variant][+feat[+feat…]]` | **Repeated-`+` ambiguity**: a single feature value containing a literal `,` cannot be distinguished from a two-value feature list under repeated `+feat` syntax. |
| `lock_key` / `base_lock_key` / `from_lock_key` | `platform.rs:372-531`, escape helpers `641-712` | `os/arch[/variant][;osf=…][;feat=…]` with percent-escaping | A *second* lossless encoding invented solely because `Display` was lossy (dropped `os_features` + CPU `features`). |
| OCI JSON `platform` object | `From<&Platform> for native::Platform` `852-897` | `{os, architecture, variant, os.features:[…]}` | Correct and spec-bound; stays. |

### The two compatibility truths

1. **`Index::select`** (`oci/index.rs:286-376`) — walks the host priority list
   (`supported_set()` = `[current(), Any]`), and within the first matching tier keeps the
   **most specific** candidates by `os_features` count, ties → `Ambiguous`. This already
   implements #212's intended selection formula on a *fresh resolve*.
2. **`lookup_host_leaf`** (`project/resolve.rs:680-708`) — a **bare exact→base→"any" string
   lookup** over a V2 platform map on the *lock-read* path. It does **not** run the relation:
   a dual-libc host keyed `linux/amd64+libc.glibc,libc.musl` matches neither a
   `linux/amd64+libc.glibc` entry (exact key differs) nor the `base_lock_key` tier (which
   strips *all* features to `linux/amd64`), so it silently returns `None` — the **dual-libc
   silent-`None` gap**.

Two encodings of the compatibility answer, two encodings of the string, and `Any` enumerated as
a pseudo-host in `supported_set()`/`all_supported()` (`platform.rs:542,576`): the model is
internally redundant and, at the seams, inconsistent.

### #212 — remove platform lists from authoring

Authoring today can target a *set* of platforms (`--platform any` derives a coverage
intersection across all dependencies via `dependency_pinning.rs:155-297`, embedding a
`TargetPlatforms` set). #212 rules: **one platform per manifest**, `Any` lowest priority,
selection formula "Matching Platform × Num Matching OS Feature > Matching Platform > Any".

---

## Decision Drivers

- **D1. One compatibility relation.** Fresh-resolve (`Index::select`) and lock-read
  (`lookup_host_leaf`) must give identical answers. One relation, applied in both places,
  closes the dual-libc gap by construction.
- **D2. One string grammar.** `Display`, `FromStr`, CLI args, and lock/pin map keys must share
  a single lossless, injective, round-tripping encoding — killing the positional ambiguity and
  the parallel `lock_key` family.
- **D3. Single-platform mental model.** A manifest describes exactly one platform; `Any` is the
  lowest-priority fallback inherent in the relation, never an enumerated host tier or an
  authoring set.
- **D4. Predictable, falsifiable selection.** Selection is a total order on a small
  lexicographic score; genuine ties surface as an error, never a silent pick.
- **D5. Pre-1.0 clean break.** No migration shims. Legacy lock and authoring paths are deleted,
  not bridged (policy: `project_breaking_compat_next_version`). Only the OCI manifest + metadata
  wire that published packages already depend on stays backward compatible.

---

## Scope

**In scope (this ADR specifies all five as locked decisions):** the compatibility relation
(D1), the canonical grammar (D2), `ocx.lock` V3 + digest-uniqueness (D3), single-platform
resolution (D4), single-platform authoring (D5).

**Out of scope (explicitly deferred, do not fold in):**

- **Index indirection / public-index registry indirection / index locking** — parked; see
  [`handover_index_indirection.md`](./handover_index_indirection.md). Waiting on the
  `ocx-sh/index` M-1 gate and structural changes. The platform-model track (this ADR) runs
  first because representation feeds everything; the index-lock ADR (its work item #1) depends
  on this being merged.
- **Signing** — deferred entirely to a future ADR (records the unsigned-index tampering threat
  when written).
- **`host_can_run` / `same_os_arch`** (`platform.rs:257-300`) — the host-only symlink gate for
  issue #179. os+arch-only comparison, a separate concern; unaffected by this ADR and retained
  verbatim.
- **libc *version* ranges, CPU *microarch* variant matching** — remain deferred exactly as
  `adr_platform_libc_os_features.md` left them (strict `variant` equality, no ranges).

---

## Decision D1 — Directed compatibility relation `is_compatible(required, offered)`

The tier walk over `supported_set()` and the `lookup_host_leaf` string tiers are **both
replaced** by a single directed relation plus a lexicographic score, applied identically on
fresh-resolve and lock-read.

### Naming and direction

`is_compatible(required, offered)` — read "does `offered` satisfy the requirement `required`?".

- **`required`** = what the caller needs satisfied: the host (`Platform::current()`), or an
  explicit `--platform` value, or a project-lock host lookup key.
- **`offered`** = a candidate: an OCI image-index child platform, or a lock/pin map key.

The relation is **not symmetric**. Two asymmetries are load-bearing:

1. **`Any` asymmetry.** An `Any` *offer* satisfies every requirement (a platform-agnostic
   artifact runs anywhere). An `Any` *requirement* is satisfied **only** by an `Any` offer (an
   unknown/undetected host can run only platform-agnostic content, never a `Specific` binary).
2. **`os_features` inversion.** `os_features` are *mandatory host capabilities the offered
   binary demands*. So inside the relation the direction inverts relative to the
   `required`/`offered` naming: **`offered.os_features ⊆ required.os_features`** (the host's
   capabilities must be a superset of what the binary demands). `variant` does **not** invert —
   it is strict equality, gated on the *offer* declaring it.

### Rules (evaluated in order)

```
is_compatible(R = required, O = offered) -> bool:
  if O is Any:            return true          # Any offer satisfies every requirement
  if R is Any:            return false         # Any requirement: only an Any offer (handled above)
  # both Specific:
  if R.os   != O.os:      return false
  if R.arch != O.arch:    return false
  if O.variant.is_some() && O.variant != R.variant: return false   # strict, offer-gated
  return O.os_features ⊆ R.os_features                              # subset, inverted
```

`variant` is offer-gated: an offer that leaves it `None` imposes no constraint.

### Truth table

`R` = required, `O` = offered. `feat` abbreviates `os_features`. `⊆` is set subset.

| # | R | O | Result | Reason |
|---|---|---|---|---|
| 1 | `Any` | `Any` | ✅ compatible | Any offer |
| 2 | `Any` | `linux/amd64` | ❌ | Any requirement, Specific offer |
| 3 | `linux/amd64` | `Any` | ✅ | Any offer satisfies every requirement |
| 4 | `linux/amd64` | `linux/amd64` | ✅ | os+arch match, no refinements |
| 5 | `linux/amd64` | `linux/arm64` | ❌ | arch mismatch |
| 6 | `linux/amd64` | `windows/amd64` | ❌ | os mismatch |
| 7 | `linux/arm64` | `linux/arm64/v8` | ❌ | offer declares `variant`, required has none |
| 8 | `linux/arm64/v8` | `linux/arm64/v8` | ✅ | variant equal |
| 9 | `linux/arm64/v8` | `linux/arm64` | ✅ | offer leaves variant unset → unconstrained |
| 10 | `linux/arm64/v8` | `linux/arm64/v7` | ❌ | variant mismatch |
| 11 | `linux/amd64+libc.glibc` | `linux/amd64` | ✅ | `{} ⊆ {glibc}` (bare offer runs on libc host) |
| 12 | `linux/amd64+libc.glibc` | `linux/amd64+libc.glibc` | ✅ | `{glibc} ⊆ {glibc}` |
| 13 | `linux/amd64+libc.glibc` | `linux/amd64+libc.musl` | ❌ | `{musl} ⊄ {glibc}` |
| 14 | `linux/amd64` (no libc detected) | `linux/amd64+libc.glibc` | ❌ | `{glibc} ⊄ {}` (feature-demanding offer needs capable host) |
| 15 | `linux/amd64+libc.glibc,libc.musl` (dual-libc host) | `linux/amd64+libc.glibc` | ✅ | `{glibc} ⊆ {glibc,musl}` |
| 16 | `linux/amd64+libc.glibc,libc.musl` | `linux/amd64+libc.glibc,libc.musl` | ✅ | `{glibc,musl} ⊆ {glibc,musl}` |

### Scoring — lexicographic `(is_specific, matched_refinement_count, matched_os_feature_count)`

`Index::select` (and `lookup_host_leaf`, and authoring pinning) score every **compatible** offer
and keep the maximum. The tier walk is gone. The score is a **3-tuple**: the middle axis
(`matched_refinement_count`) ranks a `variant`-exact offer above an unconstrained one.

```
score(R, O)  where is_compatible(R, O):
  is_specific              = 1 if O is Specific else 0            # O = Any → 0
  matched_refinement_count = 1 if O declares variant else 0        # 0-1
  matched_os_feature_count = |O.os_features|                     # all matched: subset already held
  return (is_specific, matched_refinement_count, matched_os_feature_count)   # lexicographic, higher wins
```

An offer that *declares* the variant refinement is, for a compatible offer, an offer that
*matches* it: `is_compatible` offer-gates `variant` on strict equality, so a declared `variant` on
a compatible offer necessarily equals the requirement's. Declaration therefore counts as a match.
The refinement axis ranks **above** the feature axis because it is a strict-equality commitment
the offer opted into — a harder constraint than an `os_features` capability subset.

- **Specific-with-nothing-matched beats `Any`.** `(1, 0, 0) > (0, 0, 0)`: rows #4/#11 beat a
  co-present `Any` offer. This is #212's "Matching Platform > Any".
- **Refinement-exact beats unconstrained.** `(1, 1, 0) > (1, 0, 0)`: an explicit
  `--platform linux/arm64/v8` against a `linux/arm64/v8` offer and a co-present unconstrained
  `linux/arm64` offer now picks the exact one (both are compatible — rows #8/#9 — and tied under
  the old 2-tuple, producing a spurious `Ambiguous`). This is the fix that motivated the third
  axis.
- **More matched features win.** `(1, 0, 1) > (1, 0, 0)`: row #12 (a `libc.glibc` offer) beats row
  #11 (a bare offer) for a glibc host. This is #212's "Matching Platform × Num Matching OS
  Feature > Matching Platform".
- **Ties → `Ambiguous`.** Two offers with an equal maximum score are irreducible: e.g. a
  dual-libc host against separate `libc.glibc` and `libc.musl` single-libc offers (both score
  `(1, 0, 1)`). The resolver returns `SelectResult::Ambiguous` (exit 65); the user disambiguates
  with an explicit `--platform`. The refinement axis ranks real differences — it never manufactures
  a winner among genuine equals (two identical `linux/arm64/v8` offers still tie).

#212's stated formula "Matching Platform × Num Matching OS Feature > Matching Platform > Any" is
this lexicographic order (`is_specific` outranks refinements outranks feature count outranks `Any`);
the refinement axis is a strictly finer tiebreak inserted between "Matching Platform" and the
feature count, not a departure from the formula.

### One shared selection helper — three call sites, one truth

The relation and its score are wrapped in **one** selection function that every consumer calls:

```
select_best(required, candidates: &[(T, Platform)]) -> Selection<T>
  # keeps the max-scoring compatible candidate; returns:
  Selection::Found(T)       # exactly one max-scorer
  Selection::Ambiguous(…)   # two+ compatible candidates tie at the max score
  Selection::None           # no compatible candidate (caller maps to NotFound/FeatureMismatch)
```

There are **three** consumers, and they must all route through `select_best` — not just the two
compatibility paths. The authoring pinning path is the third, and today it diverges:

| Call site | Before | After |
|---|---|---|
| Fresh resolve | `Index::select` tier-walks `supported_set()` + specificity filter (`index.rs:299-352`) | `Index::select(host)` = `select_best(host, candidates)` |
| Lock-read | `lookup_host_leaf` exact→base→`any` **string** tiers (`resolve.rs:703-708`) | `lookup_host_leaf(map, host)` parses each key via `Platform::from_str` then `select_best(host, parsed)` |
| **Authoring pin** | `resolve_for_specific` (`dependency_pinning.rs:129-152`) **counts `can_run` matches** and errors on `>1` | `resolve_for_specific` = `select_best(declared_platform, candidates)` |

The authoring divergence is a real correctness gap, not a style nit: `resolve_for_specific`
collects every `can_run` match and errors on more than one. A dependency offering `linux/amd64`
**and** `any` yields two `can_run` matches for a `linux/amd64` target, so authoring reports
`AmbiguousPlatform` — while `Index::select` at runtime scores `linux/amd64` (`(1,0,0)`) over `any`
(`(0,0,0)`) and installs cleanly. A package that installs is rejected at `create`. Routing all three
through `select_best` closes that gap by construction (and deletes `base_lock_key`'s "strip all
features" tier, the source of the dual-libc silent-`None` gap on the lock-read path).
`AuthoringDependency::pin_for` (`authoring/dependency.rs:183-200`) calls `lookup_host_leaf`
directly, so it inherits the shared helper transitively.

**Validation requirement — authoring-vs-Index parity.** A test must assert `select_best` yields the
same winner for `Index::select` and for authoring pinning over identical candidate sets, covering
at minimum: (a) Specific + `Any` candidates (Specific wins, no ambiguity); (b) feature-specific +
bare candidates (feature-specific wins). This is the regression guard against the two paths drifting
apart again.

---

## Decision D2 — Canonical string grammar (single encoding everywhere)

One lossless, injective, round-tripping grammar for `Display`, `FromStr`, CLI `--platform`, and
every `ocx.lock` / pin-map key. `lock_key`, `base_lock_key`, `from_lock_key`, and the four escape
helpers are **deleted** (`platform.rs:372-531`, `641-712`).

### The CPU `features` field is deleted from the model entirely

`Platform::Specific.features: Option<Vec<String>>` (OCI-RESERVED) is **removed from the struct**,
not merely left unserialized. Today it exists but every constructor sets it `None` and a
`debug_assert!` guards that — "never populated" is a *runtime* invariant, and the injectivity claim
below is false for as long as a public field can, in principle, hold a value the grammar cannot
render. Per the refactor doctrine, a field OCX's model cannot represent should not exist in the
type. The **only** place `features` is acknowledged is the OCI JSON parse boundary
(`TryFrom<native::Platform>`), which keeps warn-dropping an incoming `features` array from foreign
manifests — that is inbound tolerance, not a model field. With the field gone, round-trip
injectivity holds **unconditionally** at the type level, not by runtime convention.

### Form

```
os/arch[/variant][+osfeature[,osfeature…]]      |      any
```

The `+`-then-comma-list for `os_features` removes the old `Display`'s ambiguity: a single-value
feature `["a,b"]` (escaped `a%2Cb`) can no longer be confused with a two-value list
`["a","b"]` (`a,b`).

Examples: `linux/arm/v7` · `linux/amd64+libc.glibc,libc.musl` · `linux/arm64/v8+libc.glibc` ·
`any`.

### EBNF

```ebnf
platform      = "any" | specific ;
specific      = os "/" arch [ "/" variant ] [ "+" feature-list ] ;

os            = "linux" | "darwin" | "windows" ;          (* closed enum — OperatingSystem *)
arch          = "amd64" | "arm64" ;                        (* closed enum — Architecture *)

variant       = value ;                                    (* e.g. "v7", "v8" *)
feature-list  = feature { "," feature } ;
feature       = value ;                                    (* e.g. "libc.glibc" *)

value         = value-char , { value-char } ;             (* non-empty *)
value-char    = unreserved | pct-encoded ;
unreserved    = <any Unicode scalar except one of RESERVED> ;
RESERVED      = "%" | "/" | "+" | "," ;
pct-encoded   = "%" hex hex ;                              (* only the four escapes below *)
```

`os` and `arch` are closed enums with no reserved characters, so they never require escaping.
`variant` and each `feature` are registry-controlled strings and are escaped.

### Escaping scheme

Percent-encoding, restricted to the four structural characters (`%` first, as the introducer):

| Byte | Role in grammar | Escape |
|---|---|---|
| `%` | escape introducer | `%25` |
| `/` | os / arch / variant separator | `%2F` |
| `+` | feature-list introducer | `%2B` |
| `,` | feature separator | `%2C` |

Any other byte passes through verbatim, so the common values — `v7`, `v8`, `libc.glibc`,
`win32k`, `sse4` — contain no reserved character and are byte-identical to their raw form.
Decoding rejects a `%` not followed by exactly one of the four known two-character codes
(`InvalidFormat`).

`@` carries no grammar meaning and needs no escaping — it is a legal literal character in any
segment value, same as any other non-reserved byte.

The old `lock_key` reserved `;` and `=` for its `;osf=`/`;feat=` markers; the new grammar has no
markers, so `;` and `=` are no longer reserved — a strict simplification.

### Round-trip guarantee (the injectivity invariant)

```
∀ p : Platform,  Platform::from_str(&p.to_string()) == Ok(p)
```

`Display` is lossless because it emits **every field the model has**: `variant` (`/`) and
`os_features` (`+`, sorted + deduped, canonical). There is no dropped field to weaken the
claim — CPU `features` no longer exists on the type (deleted above). Because the struct has no
unrepresentable field, `Display` is injective unconditionally, so it is a valid, collision-free map
key — which is precisely why the separate `lock_key` family is no longer needed.

`Platform::Any` renders `any`; `any+…`, `any/…` are `InvalidFormat` (the agnostic
platform carries no fields).

### Wire-format change (pre-1.0, no back-compat)

The single-`+`-with-comma-list feature syntax **replaces** the old repeated-`+` form in every
grammar-string surface: CLI `--platform linux/amd64+libc.glibc,libc.musl` (was
`+libc.glibc+libc.musl`), and every `ocx.lock` / dependency-`platforms` **map key** (was the
`lock_key` `;osf=` encoding). Per `project_breaking_compat_next_version`, this simply breaks —
old locks/sidecars are rejected (D3), no migration prose in user docs.

**Untouched:** the OCI manifest JSON `platform.os.features` field is, and stays, a JSON string
array (`["libc.glibc","libc.musl"]`). The grammar change is to the *human/key string* encoding,
not the OCI wire object. Published-package resolution is unaffected. An incoming `os.version` key
on a foreign manifest's `platform` object is warn-dropped at the same `TryFrom<native::Platform>`
boundary as CPU `features` — inbound tolerance for foreign data, never a model field, and never
emitted on serialization.

---

## Decision D3 — `ocx.lock` V3 (the only supported version)

### V3 shape and version gate

- `lock_version = 3` is the **only** version the loader accepts. V1 **and** V2 are rejected
  with a clean, actionable error: **"unsupported ocx.lock version N; regenerate with `ocx
  lock`"** (`ProjectErrorKind::UnsupportedLockVersion { found }`). The version-peek
  (`lock.rs:349-358`) reads `lock_version` first and branches to this error for any value ≠ 3;
  do not rely on `serde_repr`'s generic "unknown variant" failure — surface the regenerate hint
  explicitly.
- The V3 on-disk shape is structurally the current V2 (`LockedToolV2`): bare `repository`
  coordinates + a `platforms: BTreeMap<String, Digest>` map — but the **keys are canonical
  grammar strings** (D2), not `lock_key` encodings.
- **Lock unit stays the platform-manifest digest**, never the image-index digest (uniform with
  dependency pins and the future public index — see `handover_index_indirection.md` D-A). The
  outer image-index digest is intentionally not stored.

### Deletions

| Deleted | Was | Because |
|---|---|---|
| `LockedResolution::LegacyIndex` (`lock.rs:205`) | V1 in-memory arm (image-index digest + `Index::select` at read time) | V1 rejected; no legacy read path. `LockedResolution` collapses to a single `PerPlatform` shape (no longer an enum, or a one-arm enum kept only for the `repository`+`platforms` grouping). |
| `ProjectLockV1` / `LockedToolV1` (`lock.rs:262`) | V1 on-disk read shape | V1 rejected. |
| `LockedToolV2` (`lock.rs:241`) as the *versioned* type + its `from_v2_str` loader | V2 on-disk shape with `lock_key`-encoded map keys | Replaced by the single V3 shape keyed on canonical grammar. Since exactly one version is ever read or written, the version-suffixed type name collapses — prefer one unsuffixed on-disk type + a single-value `LockVersion` guard. |
| `from_v1_str` (`lock.rs:379`) | V1 → in-memory normalization | dead with V1. |

### Canonical-key validation (load + every write path)

A platform-map key is a canonical grammar string. Because the grammar admits noncanonical spellings
that parse to the *same* `Platform` (`linux/amd64+a,a` vs `+a`, unsorted `+b,a` vs `+a,b` — the same
alias hazard `dependency_pinning.rs:271-279` already documents for the old encoding), every V3 map —
on **load** and at **every write site** — validates each key by:

1. **Canonicalization** — parse the key and require `parsed.to_string() == key` (byte-exact). A
   noncanonical spelling is rejected (`ProjectErrorKind::NoncanonicalPlatformKey { key }`), never
   silently normalized. This keeps the map's on-disk bytes the unique canonical form and preserves
   byte-stable output.
2. **Canonical-platform uniqueness** — after parsing, reject two keys that resolve to the **same**
   `Platform` (a duplicate *platform*, distinct from an exact duplicate *string* key that `BTreeMap`
   already collapses). This is **key-level uniqueness after normalization — NOT digest-value
   uniqueness**, which stays dropped (R6): two distinct platforms may still share a digest.

### Key uniqueness only — no digest-value uniqueness

Ordinary `BTreeMap` **key** uniqueness (plus the canonical-platform uniqueness above) is the only
invariant on the platform map; the existing `DuplicatePlatformKey` guard (`resolve.rs:672-678`) and
the sidecar deserializer's duplicate-key rejection stay. **There is no digest-*value* uniqueness
constraint.** Two distinct platform keys legitimately mapping to the same manifest digest is a real,
supported case — e.g. a publisher who pushes the `darwin/amd64` binary under `darwin/arm64` too so
Rosetta 2 runs it, or any noarch payload shared across entries. Rejecting that would break valid
packages. See Rejected Alternatives R6.

### Write/load validation parity

A single `ProjectLock::validate` (declaration-hash version, bare `repository`, canonical platform
keys) is invoked by **both** `from_str_with_path` (load) and `to_toml_string`/`save` (write), so the
same invariants gate both directions. Invariant: **every lock that serializes also loads** — a
round-trip property test guards it. Canonical-key checking is keys-only, never values (R6).

---

## Decision D4 — Single-platform resolution contract

- `Index::select` takes **one** requested platform: `select(&self, identifier, host: &Platform,
  op)`. The `Vec<Platform>` parameter and the priority-fallback tier walk are removed.
- `PackageManager::resolve` (`resolve.rs:279`) and its siblings pass **one** platform, defaulting
  to `Platform::current().unwrap_or(Platform::Any)`. On an unsupported host (`current() == None`)
  the requested platform is `Any`, which — by D1 rule 2 — matches only `Any` offers: the
  agnostic-package fallback is preserved by the *relation*, not by an enumerated `Any` tier.
- The CLI `--platform a,b` **priority fallback is removed**. `--platform` takes at most one value
  for resolution commands (`ocx package install/pull/exec`, `ocx run`, `ocx env`); more than one
  is a usage error (exit 64), matching the existing `ocx env` single-target rule
  (`toolchain_env.rs:380-391`).
- **`supported_set()` and `all_supported()` are deleted** — not narrowed. No platform-array
  helper survives on `Platform`. Their only jobs were the priority-walk tier list and the
  `Any`-as-a-host fallback, both of which the relation now subsumes. The resolution default is
  `Platform::current().unwrap_or(Platform::Any)` computed at each `resolve`/`select` entry: on an
  unsupported host (`current() == None`) the requested platform is `Any`, which by D1 rule 2
  matches only `Any` offers — the agnostic-package fallback lives in the *relation*, not a helper.

### The one exception — `patch sync` fan-out

`ocx patch sync` keeps its **multi-platform fan-out**: with no `--platform` it pins a companion
for *each* of the five concrete ship targets. This is an **explicit enumeration loop** (produce N
outputs), categorically different from *selection* (choose 1 of N): a list means "fan out to each
of these", never "try these in priority order". Because it is fan-out, not selection, the target
matrix lives at the fan-out site (`conventions.rs`, `platforms_or_all_supported` reworked into a
`patch sync`-local default) as a plain list of concrete platforms — **not** a
`Platform::all_supported()` "supported set" helper. The helper is deleted; the enumeration it
happened to carry moves to the single caller that fans out.

---

## Decision D5 — Strict single-platform authoring

- **`TargetPlatforms` is deleted** (`metadata/authoring/target_platforms.rs`). Published
  metadata never carried it (stripped at publish); authoring no longer needs a set.
- **One platform per `ocx package create` / `ocx package push`.** `--platform` takes a single
  value (default `current()`); `ocx package create --platform P` resolves each tag-only
  dependency to the single leaf compatible with `P` and embeds nothing broader.
- **The `--platform any` coverage-intersection machinery is deleted** (`dependency_pinning.rs:
  155-297`): `resolve_for_any`, `derive_target_platforms`, `covered_platforms`,
  `candidate_universe`, `advertised_platform_keys`.
- **Rule: an `any`-targeted package may depend only on `any`-offered dependencies.** A
  platform-agnostic package performs no platform-specific resolution, so it cannot pick a
  platform-specific dependency leaf. If a dependency offers no `any` manifest,
  `ocx package create --platform any` fails with a clear error (a dependency-pinning error, exit
  65), rather than silently deriving a covered subset. This replaces the intersection logic with
  a single invariant.
- **Rule: an `any`-targeted bundle prohibits direct digest pins on its dependencies.** A leaf
  manifest carries **no platform descriptor**, so a dependency pinned by a bare `@digest` cannot be
  verified to be `any`-offered — the any-only rule above is unenforceable against it. Therefore both
  `ocx package create` and `ocx package push` **reject** a direct digest pin in an `any`-targeted
  bundle (dependency-pinning error, exit 65). Concrete-targeted bundles keep direct digest pins
  unchanged (their target platform is known, so the pin's provenance is not in question). The
  validation must inspect **already-pinned** dependencies too — `pin_dependencies` currently
  short-circuits `is_pinned()` deps (`dependency_pinning.rs:61-65`), so the any-target digest-pin
  check runs as a separate pass that does not skip them.

### `any`-pin provenance (push gate)

Sidecar map keys are publisher *claims*, not *evidence*: a dependency keyed `any` may pin a digest
that is actually a platform-specific leaf. For an `any`-targeted bundle the push gate therefore
**fetches each dependency's own manifest by tag** and requires an `any`-platform entry whose digest
equals the pin (a flat `Manifest::Image`: its own digest must equal the pin). Errors:
`PublishGateError::AnyPinNotAdvertisedAsAny` (exit 65) when the fetched index advertises no such
`any` entry; `AnyPinProvenanceUnavailable` (**fail-closed**, exit code chain-classified from the
fetch cause) when the manifest cannot be fetched — an unverifiable claim is treated as untrusted,
never silently accepted. Cost: one extra manifest GET per dependency, on `any`-target pushes only.

### Recorded platform binds `create` to `push`/`test`

`ocx package create` resolves every dependency against its `--platform` value (default
`Platform::current()`). That platform is **recorded in the authoring sidecar** — a new optional
`AuthoringBundle.platform` field, serialized as a **canonical-grammar string** on the wire
(`#[serde(with = "platform_field")]`, `skip_serializing_if none`; optional in the JSON schema),
read back via `AuthoringMetadata::platform()` / `resolve_platform()`.

`ocx package push` and `ocx package test` no longer default their platform to the **host**. They
default to the **recorded** platform via `AuthoringMetadata::resolve_platform(explicit)`:

- no `--platform` → use the recorded platform (the exact platform the pins were resolved for);
- explicit `--platform` → a **checked assertion**: it must equal the recorded platform, else
  `AuthoringError::PlatformMismatch` (exit 65 `DataError`);
- sidecar missing the field (e.g. a hand-written or pre-binding sidecar) →
  `AuthoringError::MissingRecordedPlatform` (exit 65).

**Rationale.** The prior host-default silently decoupled the *published label* from *create's pin
resolution*: an amd64 CI runner running `create --platform linux/arm64` then a bare `push` would
publish an arm64-pinned bundle under the host's `linux/amd64` label — cross-compile corruption. And
because direct digest pins are platform-blind (leaves carry no platform, D5 above), no downstream
gate could catch it. Binding push/test to the recorded platform makes the label always match the
pins by construction. **Wire-format change** (new sidecar field) — pre-1.0, no back-compat.

### Positive

- **One relation, one grammar, one lock version.** Fresh-resolve and lock-read give provably
  identical answers; the dual-libc silent-`None` gap is closed by construction.
- **Feature-list ambiguity eliminated.** The single-`+`-then-comma-list syntax removes the old
  repeated-`+` ambiguity; `Display` is lossless and injective, so it doubles as the map-key
  encoding and the separate `lock_key` family (≈250 lines incl. escape codecs) is deleted.
- **#212 satisfied.** One platform per manifest; `Any` is the lowest-priority fallback inherent
  in the relation; the selection formula is the lexicographic score.
- **Smaller surface.** Deleted: `lock_key`/`base_lock_key`/`from_lock_key` + 4 escape helpers,
  `TargetPlatforms`, `resolve_for_any`/`derive_target_platforms`/`covered_platforms`/
  `candidate_universe`, `LockedResolution::LegacyIndex`, `ProjectLockV1`/`LockedToolV1`,
  `from_v1_str`, the V2 loader path, `supported_set`/`all_supported` entirely, and the
  `Vec<Platform>` on `select`.

### Negative

- **All committed V1/V2 locks and pinned sidecars break** and must be regenerated
  (`ocx lock` / `ocx package create`). Accepted: pre-1.0, no published-package impact.
- **`--platform a,b` priority fallback removed.** A workflow relying on "try amd64, else arm64"
  must invoke twice or pin explicitly. Acceptable: fallback masked resolution failures.

### Risks

| Risk | Note |
|---|---|
| **Escaping regressions** | The grammar codec is the security-relevant seam (a forged separator could alias two platforms into one lock key → wrong-binary install). Port the existing `lock_key` injectivity tests (`platform.rs:1392-1483`: comma-in-value, marker-forging separators) to the new `+`/`,` codec; they remain the canary. |

---

## Refactor Doctrine — no vestiges

The codebase is refactored **as if multi-platform array support never existed**. This is not a
migration from arrays to single-platform; it is a single-platform design written cleanly, per the
pre-1.0 no-back-compat policy (`project_breaking_compat_next_version`).

- **No vestiges.** No `_v2`/`legacy`/`_single`/`_new` suffixes, no "formerly a list" comments, no
  dead array-shaped parameters kept "just in case". Names read as originally designed:
  `select(host)`, not `select_single`; the platform map is *the* map, not "the V3 map".
- **No transitional shims.** No dual-read of old locks, no compat flag, no feature gate bridging
  the array and scalar worlds. Old locks fail with the regenerate hint (D3) — that is the entire
  migration story.
- **No compat naming.** APIs, types, and doc-comments describe the present single-platform
  contract in their own terms. A reader who never saw the array era should find nothing hinting
  it existed.
- **Docs match.** `website/` and reference pages read as a single-platform product; no "platform
  lists were removed" prose (per `project_no_migration_prose_in_docs`).

## Rejected Alternatives

### R1 — Keep `lock_key` as a second lossless encoding, only fix its ambiguity

Patch `from_lock_key`'s feature-list ambiguity in place, leave `Display` lossy. **Rejected:**
preserves two encodings of the same value and the divergence risk between them; the whole point
of the injective-`Display` move is that one lossless string suffices as both display and key. Two
encodings is the disease, not the ambiguity alone.

### R2 — Keep the `supported_set()` priority tier walk (multi-platform host list)

Keep `select(Vec<Platform>)` and the `[current(), Any]` walk; layer the score on top.
**Rejected:** the tier walk and the score answer the same question two ways; `Any`-as-a-host-tier
is exactly the redundancy #212 targets. Collapsing to one host + one relation removes a whole
class of "which tier won" reasoning and makes fresh-resolve and lock-read unifiable (they cannot
be unified while one side walks tiers and the other does exact-string lookup).

### R3 — Keep multi-platform authoring (`TargetPlatforms` + coverage intersection)

Retain `--platform any` deriving a covered set across dependencies. **Rejected by #212:** the
intersection machinery (`resolve_for_any` et al.) is subtle, hard to explain, and exists only to
support platform *lists* in authoring — which #212 removes. "One platform per manifest; an
`any` package depends only on `any` deps" is a single invariant replacing ~140 lines of set
algebra.

### R4 — Retain repeated-`+` feature syntax in the grammar

Keep `linux/amd64+libc.glibc+libc.musl`. **Rejected:** repeated `+` cannot host an injective
single-value-with-separator case without escaping the `+` inside values anyway; the comma-list
after a single `+` mirrors the OCI array shape and makes the escape set minimal (`, ` only inside
the list). One `+` introduces the list; `,` separates — matches the `[…]` JSON mental model.

### R5 — Bridge V1/V2 locks with a migration transcoder

Read V1/V2, rewrite forward on load. **Rejected:** pre-1.0 (`project_breaking_compat_next_
version`) — migration code is permanent complexity for a one-time inconvenience solved by
`ocx lock`. The regenerate-hint error is the whole migration story.

### R6 — Digest-value uniqueness within a platform map

An earlier draft enforced "no two platform keys carry the same `Digest`" at every write site
(a `DuplicatePlatformDigest` guard), on the theory that under one-platform-per-manifest each leaf
digest is unique to one key. **Rejected:** two distinct platform keys mapping to the same manifest
digest is **legitimate and supported**. The canonical case is Rosetta 2 — a publisher pushes the
`darwin/amd64` binary under `darwin/arm64` as well so Apple-silicon hosts run it via translation —
and more generally any noarch payload shared across entries. A value-uniqueness guard would reject
these valid packages (and would hard-fail on foreign OCI indexes that dedupe identical content
across platform entries). Only ordinary map **key** uniqueness applies; digest values may repeat,
and a shared-digest map must resolve normally — the relation (D1) never inspects value uniqueness.

### R7 — Carry a platform descriptor on each dependency pin

To let an `any`-targeted bundle verify that a direct-digest-pinned dependency is genuinely
`any`-offered (D5), an alternative was to record the pin's platform alongside the digest in the
sidecar. **Rejected:** the platform would be publisher-asserted metadata with no backing in the
pinned leaf manifest (leaves carry no platform descriptor), so it verifies nothing — a wrong or
forged value is indistinguishable from a correct one. It also adds a metadata surface (and a schema
field) to every pin for a check that a simple prohibition (D5: no direct digest pins in `any`-target
bundles) enforces cleanly. Unverifiable provenance plus extra surface for zero guarantee — dropped
in favor of the prohibition.

---

## Amendments to sibling ADRs

### `adr_platform_libc_os_features.md` §180 (applied)

The locked-decisions row **"Tiebreaker between multiple matching entries → Manifest array index
order"** was already stale: `Index::select` (`index.rs:299-352`) has shipped specificity scoring
+ `Ambiguous` since the Phase-4 changelog entry, not array-index order. This ADR's D1 makes the
scoring the *locked* rule. The row is edited in that file to point here. No other row in that ADR
changes; the `os.features` subset semantics, `libc.*` namespace, host detection, and RESERVED-
`features` handling are all carried forward verbatim into D1/D2.

### `adr_cascade_platform_aware_push.md` (cross-checked — no edit needed)

The cascade path uses `has_platform` / `manifest_has_platform` **strict `native::Platform`
struct equality** to decide index-entry eviction and merge (Bug 2 fix). This is a *different*
question from `is_compatible` ("is *this exact* platform descriptor already in the index?" vs
"does host X run candidate Y?"), and the two matchers stay intentionally distinct — the same note
`adr_platform_libc_os_features.md` § `has_platform` already records. D5 (one platform per push)
*strengthens* the cascade ADR: its per-platform merge premise, previously a publisher convention,
is now an enforced authoring invariant, so the "single-platform push overwrites multi-platform
index" hazard cannot arise from OCX-authored pushes. `os_features` normalization (sort + dedup at
the native boundary) that cascade eviction relies on is retained unchanged by D2. **Impact:
premises strengthened, no code or decision in that ADR is invalidated.**

---

## Inconsistencies found between locked decisions and code reality

All four first-draft flags are now resolved by the approved plan and user review — none is open.
The `supported_set`/`all_supported` `Any`-fallback and the digest-value-uniqueness questions were
resolved by user review (see D4 and R6). The remaining two are resolved by the approved plan:

1. **`lookup_host_leaf` rewrite — RESOLVED, in scope.** Plan WP-CD explicitly rewrites
   `lookup_host_leaf` onto the parsed-key relation (`Platform::from_str` + `is_compatible` +
   scoring, shared with `Index::select`), together with `pin_for` (`dependency.rs:183-200`), which
   calls it directly.

2. **`TargetPlatforms` multi-file deletion — RESOLVED, in scope.** Plan WP-CD deletes
   `target_platforms.rs`, the `dependency_pinning` machinery, and the `AuthoringBundle.platforms`
   consumers; WP-E re-derives `ocx package push` from the single `--platform` value (fan-out
   removal).

---

## Implementation Surface Map (informational — no code in this ADR)

| Decision | Files (primary) |
|---|---|
| D1 relation + score + shared helper | `oci/platform.rs` (`can_run` → `is_compatible` + `compatibility_score` + shared `select_best`), `oci/index.rs:286-397` (`select` = `select_best`, drop `Vec`/tier walk), `project/resolve.rs:680-708` (`lookup_host_leaf` = parse + `select_best`), `package/dependency_pinning.rs:129-152` (`resolve_for_specific` = `select_best`, replacing the `can_run`-count-and-error), `package/metadata/authoring/dependency.rs:183-200` (`pin_for`, transitively) — WP-A owns `select_best`, WP-CD the pinning call site |
| D2 grammar + `features`/`os_version` deletion | `oci/platform.rs` (`Display` 714-743, `FromStr` 745-832, delete `lock_key`/`base_lock_key`/`from_lock_key` 372-531 + escape helpers 638-712, **delete the `features` and `os_version` fields from `Platform::Specific`** L91 and every constructor — WP-A; `TryFrom<native::Platform>` keeps the inbound `features` and `os.version` warn-drops only), CLI help `crates/ocx_cli/src/options/platforms.rs:16-24` |
| D3 lock V3 + canonical-key validation | `project/lock.rs` (`LockVersion`, version peek 342-434, delete V1/V2 read paths + `LockedResolution::LegacyIndex`, add `UnsupportedLockVersion` + `NoncanonicalPlatformKey`; canonical-key + canonical-platform-uniqueness pass on load and every write; **no** digest-value guard — key uniqueness only), `project/resolve.rs:655-678` (`build_platforms_map` keeps `guard_unique_key`, adds the `parsed.to_string() == key` + canonical-platform dedup check), `package/metadata/authoring/dependency.rs:72-140` (sidecar deserializer applies the same canonical-key check) |
| D4 single-platform resolve | `oci/index.rs` (`select` signature → single `host`), `oci/platform.rs:542-622` (**delete** `supported_set` + `all_supported`), `package_manager/tasks/resolve.rs:279` (default `current().unwrap_or(Any)`), `crates/ocx_cli/src/conventions.rs:74-109` (drop `supported_platforms`/`all_supported_platforms`; rework `platforms_or_all_supported` into a `patch sync`-local concrete list), CLI `--platform` arity on resolution commands, `setup/bootstrap.rs:170,264,316`, `package_manager/tasks/update_check.rs:396` |
| D5 authoring | delete `package/metadata/authoring/target_platforms.rs`, `package/dependency_pinning.rs:155-297`, retarget `package/metadata/authoring.rs` + `ocx package push` fan-out to the single `--platform` value; add the any-target direct-digest-pin rejection as a pass that does **not** skip `is_pinned()` deps (`dependency_pinning.rs:61-65`); recorded-platform binding: `AuthoringBundle.platform` field (`platform_field`/`platform_field_schema` serde-as-string), `AuthoringMetadata::{platform, with_platform, resolve_platform}`, `AuthoringError::{PlatformMismatch, MissingRecordedPlatform}`, wired at `package_push.rs:120` + `package_test.rs:126` |
| Docs | `website/src/docs/reference/platforms.md`, `website/src/docs/authoring/multi-platform.*` (retitle/rewrite for single-platform), `metadata.md` (drop `platforms` target set), `.claude/rules/subsystem-oci.md` (Platform section: relation name, grammar, V3), `.claude/rules/arch-principles.md` (Platform glossary row), `website/src/docs/reference/command-line.md` (`--platform` flag docs), `website/src/docs/in-depth/versioning.md` (platform-key sections), `crates/ocx_schema` regenerate |

---

## Validation (contract, not implementation)

- [x] `Platform::from_str(&p.to_string()) == Ok(p)` property test over all field combinations,
      including reserved characters in `variant`/`feature`. *(implemented — escape
      round-trip tests)*
- [x] `is_compatible` unit tests reproduce every truth-table row (#1–#16). *(implemented,
      `platform.rs` D1 tests)*
- [x] Scoring tests (3-tuple): Specific-nothing-matched beats `Any` (`(1,0,0)>(0,0,0)`);
      refinement-exact beats unconstrained (`variant`-exact offer wins over a co-present
      unconstrained offer, no spurious `Ambiguous`); refinement axis outranks feature count; more
      features win (`(1,0,1)>(1,0,0)`); equal max → `Ambiguous` (incl. identical
      refinement-bearing offers still tie). *(implemented, `select_best_*` tests)*
- [x] `Index::select`, `lookup_host_leaf`, and authoring `resolve_for_specific` return identical
      winners for the same host + candidate set via the shared `select_best` (authoring-vs-Index
      parity): Specific + `Any` candidates → Specific wins, no ambiguity; feature-specific + bare →
      feature-specific wins. *(implemented)*
- [ ] `ocx package push`/`test` default to the sidecar's recorded platform; an explicit
      `--platform` mismatching it → `PlatformMismatch` (65); a sidecar missing the field →
      `MissingRecordedPlatform` (65).
- [ ] Dual-libc host resolves both a `libc.glibc` and a `libc.musl` lock entry (gap-closed
      regression).
- [ ] V1 and V2 `ocx.lock` fixtures both fail load with `UnsupportedLockVersion` + regenerate
      hint.
- [ ] Shared-digest aliasing: a platform map with two distinct keys pointing at the **same**
      digest (Rosetta 2: `darwin/amd64` + `darwin/arm64` → one manifest) loads and resolves
      normally — no uniqueness rejection.
- [ ] `ocx package create --platform any` fails when a dependency offers no `any` manifest.
- [ ] `ocx package create`/`push --platform any` reject a dependency carrying a direct `@digest`
      pin (including an already-pinned sidecar dep); a concrete-target bundle accepts it.
- [x] Noncanonical V3 keys (`+a,a`, unsorted `+b,a`) fail load and write with
      `NoncanonicalPlatformKey`; two keys resolving to the same `Platform` are rejected (canonical-
      platform uniqueness), while two keys → same *digest* still load (R6). *(implemented)*
- [ ] The `features` field is absent from `Platform::Specific` (a compile-level check / no
      constructor sets it); round-trip injectivity needs no runtime `debug_assert`.
- [ ] The `os_version` field is absent from `Platform::Specific` — no producer, no consumer, no
      grammar element; an incoming `os.version` key on a foreign manifest is warn-dropped at the
      `TryFrom<native::Platform>` boundary, same pattern as `features`.
- [ ] Ported injectivity canaries (comma-in-value, forged separators) pass on the new codec.

## Links

- [`adr_platform_libc_os_features.md`](./adr_platform_libc_os_features.md) — libc `os.features`
  subset semantics + host detection (amended §180 by this ADR)
- [`adr_cascade_platform_aware_push.md`](./adr_cascade_platform_aware_push.md) — strict-equality
  `has_platform`; cross-checked, premises strengthened by D5
- [`adr_custom_platform.md`](./adr_custom_platform.md) — the owned `Platform` enum this ADR evolves
- [`adr_per_platform_lock_pinning.md`](./adr_per_platform_lock_pinning.md) / [`adr_dependency_manifest_pinning.md`](./adr_dependency_manifest_pinning.md) — V2 lock + sidecar pinning this ADR versions to V3
- [`handover_index_indirection.md`](./handover_index_indirection.md) — parked index-lock track (work item #1: this ADR merged first)
- [issue #215](https://github.com/ocx-sh/ocx/issues/215), [issue #212](https://github.com/ocx-sh/ocx/issues/212)
- [OCI image-index spec v1.1.1](https://github.com/opencontainers/image-spec/blob/v1.1.1/image-index.md)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-07-17 | architect (opus) | Initial ADR specifying the five locked, user-approved platform-model unification decisions (relation, grammar, lock V3, single-platform resolution, single-platform authoring); amends `adr_platform_libc_os_features.md` §180; cross-checks the cascade ADR; flags four decision-vs-code inconsistencies. |
| 2026-07-17 | architect (opus) | User-review amendments: (1) **dropped digest-value uniqueness** entirely — key uniqueness only; moved to Rejected Alternatives R6 with the Rosetta 2 shared-digest rationale; (2) **deleted `supported_set()`/`all_supported()`** rather than narrowing them — resolution default is `current().unwrap_or(Any)`, `patch sync` fan-out matrix relocates to its CLI caller; (3) added the **no-vestiges refactor doctrine**. Inconsistencies #1 and #3 resolved and removed (two open flags remain). Surface map + validation updated. |
| 2026-07-17 | architect (opus) | Codex-gate amendments (all four findings verified against live code, none stale): (1) D1 mandates **one shared `select_best` helper across three consumers** — `Index::select`, `lookup_host_leaf`, and authoring `resolve_for_specific` (which today counts `can_run` matches and errors on `>1`, rejecting a `linux/amd64`+`any` dep that `select` installs); added authoring-vs-Index parity validation; (2) D5 **prohibits direct digest pins in `any`-targeted bundles** (leaves carry no platform → any-ness unverifiable), validation covers already-pinned deps, per-pin platform descriptor rejected as R7; (3) D3 adds **canonical-key validation** on load + every write (`parsed.to_string() == key`, canonical-platform uniqueness — key-level, NOT digest-value); (4) D2 **deletes the CPU `features` field from `Platform::Specific` entirely** so injectivity holds at the type level (OCI JSON boundary keeps the inbound warn-drop). Surface map (WP-A scope) + validation updated. |
| 2026-07-17 | architect (opus) | Post-review amendments (both verified against merged `feat/platform-model-unification`): (1) D1 scoring extended to the **3-tuple `(is_specific, matched_refinement_count, matched_os_feature_count)`** (fix `650f0ef0`, `compatibility_score`) — the middle axis ranks a `variant`/`os_version`-exact offer above an unconstrained one, fixing the spurious `Ambiguous` on explicit `--platform linux/arm64/v8` (truth-table rows 8/9, 12/13 tied under the 2-tuple); scoring section, pseudocode, examples, and validation updated. (2) D5 gains the **recorded-platform binding** (fix `39ffaf56`): `AuthoringBundle.platform` (canonical-grammar string on the wire, optional in schema), `ocx package push`/`test` default to the recorded platform via `resolve_platform`, explicit `--platform` becomes a checked assertion (`PlatformMismatch` 65 / `MissingRecordedPlatform` 65) — closes the cross-compile host-default corruption. Surface map + validation updated; parity / canonical-key / escape / scoring validation rows marked implemented. |
| 2026-07-17 | architect (opus) | Final gate-fix amendments (both verified against merged commit `9da15746`): (1) D5 **`any`-pin provenance check** — the push gate fetches each dependency's manifest by tag and requires an `any` entry whose digest equals the pin (flat manifest: own digest = pin); `PublishGateError::AnyPinNotAdvertisedAsAny` (65), `AnyPinProvenanceUnavailable` (fail-closed, chain-classified); one manifest GET per dep on any-target pushes. Sidecar keys are claims, not evidence. (2) D3 **write/load validation parity** — one `ProjectLock::validate` (declaration-hash version, bare repository, canonical keys) invoked by both `from_str_with_path` and `to_toml_string`/`save`; every serialized lock also loads (round-trip property test). |
| 2026-07-17 | architect (sonnet) | **`os_version` axis removed from the model entirely** — no producer (authoring records `Platform::current()`, which never set it), no consumer (all five ship platforms bare), and anti-positioning (OS-version-tied binaries are the Homebrew-bottle disease OCX rejects, see `product-context.md`). Canonical grammar drops to `os/arch[/variant][+feature[,feature...]]` \| `any`; `@` loses reserved status in the percent-escape codec (legal literal, no escaping); `is_compatible`'s offer-gated strict equality and the scoring `matched_refinement_count` axis now apply to `variant` only. OCI boundary: an `os.version` key in a manifest `platform` entry is warn-dropped at the `TryFrom<native::Platform>` boundary, same pattern as the CPU `features` warn-drop — verbatim-observed foreign data, reversible without migration. Grammar, EBNF, escape table, truth table (renumbered #1–#16), scoring, round-trip, and validation sections updated to read as designed without it. |
