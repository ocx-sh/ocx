# ADR: Dependency Manifest Pinning — create resolves, push gates + fans out

- **Status:** Accepted (2026-07-10)
- **Related:** `adr_package_dependencies.md`, `adr_per_platform_lock_pinning.md`,
  `adr_index_routing_semantics.md`, `adr_cascade_platform_aware_push.md`

## Problem — index-digest GC hazard

Package metadata dependencies pin by digest (`PinnedIdentifier`), and the docs
blessed pinning an OCI *image-index* digest for "platform-aware resolution".
That advice is a time bomb: OCX's own cascade publish **rewrites a tag's index
on every platform push** — the old index digest becomes untagged immediately
and registry GC collects it. An index-pinned dependency therefore 404s forever
once the dep publisher adds a platform or re-pushes any platform. The child
*platform manifests* survive (referenced by every successor index).

The project lock already solved this exact hazard (`adr_per_platform_lock_pinning.md`
lock V2: per-platform **leaf manifest digests**; the index digest is deliberately
not stored). Dependencies never got the same treatment.

## Decision

**Dependencies must pin manifest digests, never index digests.**

Split of responsibility:

| Command | Role |
|---|---|
| `ocx package create` | **Compiler** — resolves tag-only deps into manifest pins via the selected index, visibly rewriting the sidecar the user tests |
| `ocx package push` | **Pure gate + mechanical fan-out** — rejects unpinned/index-pinned deps; materializes one published `Metadata` per target platform |

Published wire format unchanged. Install/read path untouched — already-published
index-pinned packages keep installing (locks read-path backward compat).

## Authoring vs published model

New serde shells, reusing every leaf type (`Env`, `Entrypoints`, `Visibility`,
`DependencyName`, `Version`, `Digest`, `Identifier`):

```
AuthoringMetadata::Bundle(AuthoringBundle)
  AuthoringBundle { version, strip_components, env, dependencies, entrypoints,
                    platforms: Option<TargetPlatforms> }        // package target set (sidecar-only)
  AuthoringDependency { identifier: oci::Identifier,            // registry required, digest OPTIONAL
                        visibility, name,
                        platforms: Option<BTreeMap<String, Digest>> }  // key = Platform::lock_key()
```

Projection seam: `AuthoringMetadata::to_published(&self, platform) -> Result<Metadata, _>`
collapses each per-dep map to a single `PinnedIdentifier` via the lock V2
3-tier lookup (exact `lock_key` → `base_lock_key` → `"any"`, reusing
`project::resolve::lookup_host_leaf`) and strips sidecar-only fields
**by construction** — the published types simply do not have them.

Rejected alternatives:
- *Relax `Dependency` in place* — blast radius = entire install path; kills the
  type-level "published deps are pinned" guarantee.
- *Generic `Dependency<I>`* — generics infect custom `Deserialize` + `JsonSchema`
  impls and still need the extra sidecar fields.

## Semantics

**Create** — `--platform` declares the CONTENT's platform; the package target
set is derived, never a user choice:

- `--platform` omitted: all deps must already be pinned (else usage error 64,
  hint `-p`); validate + canonical sidecar rewrite; no network.
- `--platform <concrete>`: pinned deps pass through untouched (no network);
  unpinned deps resolve against the selected index — exactly one
  `can_run`-compatible leaf → single manifest pin; zero → `NoCompatiblePlatform`
  (65, lists available); several → `AmbiguousPlatform` (65). Embed
  `platforms: [P]`.
- `--platform any`: deps whose tag resolves to `{"any": digest}` get plain
  single pins; any platform-specific dep gets a per-dep `platforms` map; the
  package target set = platforms covered by EVERY dep (3-tier lookup; any-keyed
  and directly-pinned deps are universal). Empty intersection →
  `EmptyPlatformIntersection` (65).
- Dep tag not found → 79. `--offline`/`--frozen` + unpinned + local miss →
  policy blocked 81 (index layer, `adr_index_routing_semantics.md` routing).

**Push** — no decisions, no resolution:

- Sidecar with embedded target set: no `-p` → fan out ALL platforms in the set;
  `-p` ∈ set → that platform only (member-filter/assert, decision D1);
  `-p` ∉ set → usage error 64.
- Legacy sidecar (no embedded set): `-p` required, single-platform push —
  today's behavior, so CI and test helpers keep working.
- Gate (before any upload): dep with neither digest nor map →
  `DependencyUnpinned` (65, "re-run ocx package create"); map lacking a
  fan-out platform key → `MissingPlatformCoverage` (65); each **unique**
  projected pin verified once via `Client::pull_manifest` — an index digest →
  `DependencyPinnedToIndex` (65, explains the GC hazard); 404 →
  `DependencyManifestNotFound` (79); auth failure → passthrough (80).
- Fan-out: `apply_build_meta` hoisted once pre-loop (or platforms land on
  different timestamp tags); `list_tags` once; then **sequential** per-platform
  push through the existing `push_manifest_and_merge_tags` / cascade path
  (`merge_platform_into_index` is read-modify-write — no concurrency).
  `PushOutcome.manifest_digest` = last merged index digest; `cascade_tags` =
  ordered union.

## Snapshot semantics

A dependency pin is a snapshot: if the dep publisher later adds a platform, the
consumer package does not automatically cover it — re-run `create` + `push` to
widen coverage. This is the same trade the project lock makes, and the reason
the pins survive GC at all.

## Schema

One superset `v1.json` (same URL): the authoring form documents digest-optional
dep identifiers and the sidecar-only `platforms` fields as stripped-at-publish;
published blobs remain a valid subset. No schema fork, no `v2.json`.

## `;osf=` / libc interplay

A plain-leaf dep (`linux/amd64`) covers a feature-bearing package platform
(`linux/amd64;osf=libc.glibc`) via the base tier. A feature-bearing dep does
NOT cover the plain platform — fail-closed, matching install-time `can_run`
subset semantics.

## Out of scope

- Pin-from-`ocx.lock` (tracked as a GitHub issue; interface open — group/name
  selection ambiguity; `LockedTool.resolution` PerPlatform is already the shape).
- Any read-path/install change; any published-wire-format change.
