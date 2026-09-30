# ADR: Snapshot tag lifecycle: `ocx package prune` and ephemeral index rows

## Metadata

**Status:** Proposed
**Date:** 2026-09-29
**Deciders:** Michael Herwig (owner), architect
**Tech Strategy Alignment:** Golden Path (Rust/Tokio); no new dependency.
**Domain Tags:** data | integration | security | api
**Amends:** `adr_announce_publisher_surface.md` (additive-only announce gains absence-driven removal),
`subsystem-oci.md` § "Neither scope deletes a pin" (one carve-out for ephemeral rows), and the ocx-mirror ruling
"the index is append-only" (`registry_sync/index_write.rs:152-156`, ephemeral rows only).
**Companion:** [`system_design_snapshot_lifecycle.md`](./system_design_snapshot_lifecycle.md) (sequences, contracts,
work packages, test plan, GitHub Actions example).

## Context

Publishers push pre-release builds continuously: a canary track from the default branch, one track per open change
request (a GitHub pull request or GitLab merge request), stage tracks. Each build is `<ver>-<pre>_<build>` and
cascades into the rolling `<ver>-<pre>`. Nothing removes them, so registry repositories and index roots grow without
bound.

Everything in the stack is additive today:

- Neither `ocx_oci` nor the `oci-client` fork can DELETE; `RegistryOperation` is `Push | Pull`
  (`external/rust-oci-client/src/token_cache.rs:70`).
- `announce --tags-file` and `--tags-from-registry` "never remove a committed tag"
  (`package_announce.rs:74-86`); the `CommittedTagsDropped` guard enforces it (`announce.rs:461-473`). Only
  `--tags` (Replace) drops rows.
- `--tags-file` re-observes every committed row as well as the file's tags (`union_onto_committed`,
  `pipeline.rs:120-129`), and `regenerate` builds `tags` only from observed entries (`pipeline.rs:492-521`).
- The bot's `classify_change` (`core/diff.py:86-128`) never looks at a vanished key, so a `--tags` removal of any
  row, a release included, classifies as `refresh` and auto-merges. **Removal already exists and is ungated.**
- Announce observes with `ReadAddressing::Mirrored` (`pipeline.rs:275-278`), so a configured mirror answers the
  question "does this tag exist".
- The local index never deletes a pin (`in-depth/indices.md:141`).

## Decision Drivers

1. **ocx is a low-level tool.** Tags are the user's interpretation. The CLI encodes no MR, PR, stage or channel
   semantics, no label patterns and no policy caps; it interprets a tag only where the version grammar already
   defines the meaning.
2. **Blast radius.** A mistake or a stolen CI token must not remove a durable tag without a human seeing it.
3. **One index writer.** Every index change goes through announce and the bot; one index change per pipeline.
4. **Convergence.** Rerunning any interrupted step repairs it.
5. **Compatibility.** Every published root keeps parsing; unmarked behaviour is unchanged.

## Research

[`research_snapshot_registry_delete.md`](./research_snapshot_registry_delete.md),
[`research_snapshot_index_format.md`](./research_snapshot_index_format.md),
[`research_snapshot_deletion_security.md`](./research_snapshot_deletion_security.md),
[`research_snapshot_ux_prior_art.md`](./research_snapshot_ux_prior_art.md),
[`research_snapshot_multistage_promotion.md`](./research_snapshot_multistage_promotion.md).

The key finding: retention is safe when the publisher records "removable" at publish time and the remover reads that
record from a copy it cannot tamper with. Tag DELETE is optional in the distribution spec (GHCR, Docker Hub, ECR,
ACR and GAR refuse it); zot, Harbor, GitLab and distribution with delete enabled accept it.

## Considered Options

| Option | Shape | For | Against |
|---|---|---|---|
| **A. Low-level prune + absence-driven announce** (chosen) | `prune` deletes tags from the registry only, guarded by the index marker; announce removes a row when its tag is confirmed absent; the bot gates removals | Smallest surface; no policy in the CLI; one index writer; converges by rerun | Registry DELETE is optional (exit 87 on GHCR and friends); the user computes retention for anything beyond one pre-release family |
| B. Policy-rich prune (previous draft) | `prune --channel/--retain/--all`, label grammar, repoint, keep-tag and referrer walks, index writes from prune | One command does everything | Encodes channel semantics and policy caps; about 3x the code; prune becomes a second index writer |
| C. Registry retention + index sync | Registry cleanup policy deletes; `announce --tags-from-registry` syncs | No client DELETE; works on more registries | A regex decides removability; no event teardown; durable rows need the same marker anyway |
| D. Yank, never delete | Old builds yanked | Portable | Unbounded growth; not what the owner asked for |

Scores 1-5, weighted to 100. Owner direction decides; the gates are pass/fail.

| Criterion (weight) | A | B | C | D |
|---|---|---|---|---|
| Blast radius (30) | 4 | 4 | 2 | 5 |
| Simplicity, low-level fit (25) | 5 | 1 | 3 | 4 |
| Portability (20) | 2 | 2 | 4 | 5 |
| CI ergonomics (15) | 4 | 5 | 2 | 2 |
| Compatibility (10) | 4 | 3 | 4 | 5 |
| **Weighted total** | **77** | **58** | **57** | **86** |
| Gate: bounded growth | pass | pass | pass | **fail** |
| Gate: owner redesign | pass | **fail** | **fail** (regex decides) | **fail** |

## Decision Outcome

**Chosen: A.** `prune` is a registry tag deleter with one safeguard. `announce` is the only index writer; it learns
removals from confirmed absence of the tags it is given. The bot is the removal authority.

### Owner decision log (binding)

| # | Ruling | Status |
|---|---|---|
| R1 | ocx deletes from the registry. Tag DELETE only, never digest DELETE; registry GC reclaims storage. The push credential with `delete` added to the token scope. | Kept |
| R2 | `--force` removes non-ephemeral tags. | Superseded by the redesign: `--force` only lifts the safeguard; the bot reviews every durable index removal |
| R3 | Prune repoints rolling tags. | Superseded: no repoint in prune; the user runs `ocx package cascade repair` |
| R4 | Per-tag marker; unmarked builds are never auto-deleted. | Kept (enforced by the safeguard and the bot) |
| R5 | Promotion out of scope; `package copy` is the user's tool. One index governs each registry repository. | Kept |
| R6 | A bot-governed index is the supported authority; an ungoverned index is a variant with client guards only. GitLab governance is poll-based. | Kept, amended by S5 below |
| Addendum | Forge-neutral; GitHub and GitLab examples; no fixed number of stages. | Kept |
| Redesign 2026-09-29 | Principle "ocx is low-level"; the two-command design below. Supersedes channels, labels, retention grammar, repoint, keep-tag and referrer walks, scoped owner, `--max-removals`, sweep as a design element, and the earlier open questions. | Binding |
| A | Announce observes only the tags it is given: the `--tags` list, the `--tags-file` lines, the registry listing under `--tags-from-registry`, every row under `--refresh`. A row it is not given is carried verbatim. `--refresh` removes a gone ephemeral row and reports a gone durable one in `durable_missing` with exit 0. | Binding |
| B | Prune against the served index: a selected tag the index does not list is 75; one it lists without the marker is 81; one gone from both registry and index is skipped as `absent`. | Binding |
| C | Before auto-merging an ephemeral removal, the bot confirms `MANIFEST_UNKNOWN` from the canonical registry; a tag still present goes to human review. | Binding |
| S5 (R6 amendment) | Announce performs no permission checks and has no `--force`. On an ungoverned or direct-write index (`--out`, no bot), a durable tag the user names (`--tags`, `--tags-file`) that is gone from the registry is removed without review: naming it makes it the user's responsibility. A gone durable row that was not named is never touched. | Binding |

### `ocx package prune`

```
ocx package prune [OPTIONS] <PACKAGE> [TAG]...

  <PACKAGE>                 Package whose tags to delete, e.g. ocx.acme.example/acme/tool
  [TAG]...                  Delete exactly these tags (explicit mode)
  --prerelease <VERSION>    Select a pre-release family instead, e.g. 0.5.0-canary (structural mode)
  --keep-builds <N>         With --prerelease: keep the newest N builds and the rolling tag (N >= 1)
  --force                   Delete tags the index does not mark ephemeral, or with no index to ask
  --tags-file <PATH>        Append each tag that is gone, for `ocx package announce --tags-file`
  --dry-run                 Run the safeguard and report; delete nothing, write nothing
```

**Explicit mode** deletes exactly the named tags, any tag at all. The user computes the list.

**Structural mode** (`--prerelease`). The value must parse as a `Version` with a pre-release and no build
(`0.5.0-mr42`, `slim-0.5.0-canary`), else exit 64. Prune lists the registry's tags (canonical) and selects every tag
that parses as the same variant, core and pre-release with a build (`0.5.0-mr42_<build>`), plus the rolling
`0.5.0-mr42` itself. A tag that does not parse that way does not match; nothing else is interpreted. With
`--keep-builds N` the newest N builds and the rolling tag stay. "Newest" is `Version`'s own `Ord`, the one the
cascade uses; it orders builds as strings, so build ids must be fixed-width. `--build-timestamp=datetime`
(`_YYYYMMDDhhmmss`) satisfies that. `=date` does not: two builds on one day collide on one tag, and the second push
overwrites the first. Selection reads the registry listing only, never the index.

Why this scope is safe: a pre-release with a build cascades only into its own pre-release
(`subsystem-package.md` § Cascade Logic), so deleting a whole family never touches `x.y`, `x` or `latest`.

**Locating the registry.** For a namespace with a configured index (`[registries."<ns>"] index`), prune always
reads the package's root, `--force` or not: the root's `repository` pointer names the registry repository, checked
by the same `guarded_physical` host guard announce uses (`pipeline.rs:221`, SSRF refusal 78). A root that cannot be
read is 69, with the hint "the index locates the registry; retry" (`--force` does not skip this read). A package the
index does not know is 79. A namespace with no index uses `<PACKAGE>` itself as the repository.

The root read is canonical and read-only: the configured index URL, never a `[mirrors]` index entry, with caching
disabled, committing nothing to the local index. The served index is published only from the reviewed base branch,
so it is the base-SHA copy; the local index is never consulted, because a user can edit it.

**Safeguard**, before any delete, over every selected tag:

| Tag in the served root | Tag in the registry | Verdict |
|---|---|---|
| `"ephemeral": true` | any | Deletable |
| Present without the marker | any | Refused, reason `durable` (81) |
| Absent | present | Refused, reason `not_in_index` (75): not announced yet, or its announce is not merged |
| Absent | absent | Skipped as `absent`; nothing to do anywhere |

Any refusal aborts the whole run before the first DELETE; the error names each refused tag and its reason. If both
reasons occur, 81 wins: a retry cannot fix a durable tag. 75 is retryable and self-heals once the pending announce
merges. A namespace with no index has nothing to ask, so a run without `--force` is 81.

`--force` skips the safeguard and nothing else: the root is still read to locate the registry.

**Effect: registry only.** For each deletable tag, builds oldest first and the rolling tag last:

1. `DELETE /v2/<repo>/manifests/<tag>` on the canonical registry. Never a digest, never a `[mirrors]` entry, never a
   platform manifest. The token carries the push scope plus `delete`, from the credentials push resolves.
2. Confirm the tag is gone with a canonical manifest GET, retried briefly (3 tries, 250 ms apart, for registries
   whose tag index is eventually consistent). A GET, not a HEAD, because a HEAD response carries no error body.
   After its own successful DELETE, prune accepts any not-found answer, `NAME_UNKNOWN` included: deleting the last
   tag can take the repository with it. A tag that stays present is exit 75.
3. Append to `--tags-file` every selected tag that the served root lists and the registry no longer has, whether
   this run deleted it or it was already gone. A tag deleted under `--force` that the root never listed is not
   appended: announce has no row to remove and would exit 79 on it.

A non-dry run writes the tags file whenever it gets past selection, even empty. The format is newline-separated
with a trailing newline, merged into existing content (`conventions::merge_tags_file`, which changes from comma to
newline joining). Prune never talks to the forge and never writes the index.

**Idempotency.** A rerun of an explicit prune after a full delete feeds announce the same list, because every tag is
gone from the registry and still in the root. A rerun of a structural prune selects nothing once the registry lost
the family; the rows it left behind are removed by `announce --refresh` (scheduled, see Examples).

**What prune does not do:** no platform manifests, keep tags, referrers or attestations, and no repoint; run
`ocx package cascade repair` afterwards. The docs recommend `push --no-keep-tag` for ephemeral pushes, because a keep
tag pins the digest past the tag's deletion.

**Registries without tag DELETE.** A 405, or a 400 `UNSUPPORTED`, is exit 87 (`RegistryDeleteUnsupported`, new),
arriving on the first DELETE, before anything was deleted. CI retries 69 and 75; 87 must never be retried.

**Residual race.** DELETE is not conditional. Without `--keep-builds`, prune deletes the rolling tag, and a push to
the same track that runs concurrently may cascade into it at the same moment. Serialize each track's jobs: GitLab
`resource_group`, GitHub `concurrency` (see Examples). Build tags are unique, so builds cannot race.

### `ocx package announce` (the only index writer)

Flags are unchanged: `--tags`, `--tags-file`, `--tags-from-registry`, `--refresh`, `--yank`/`--unyank`, plus one new
flag, `--ephemeral`. `--ephemeral` with `--refresh` is 64, because `--refresh` adds nothing.

**Observation is canonical and covers only what the run is given** (ruling A). The given tags are the `--tags`
list, the `--tags-file` lines, the canonical registry listing under `--tags-from-registry`, and every committed row
under `--refresh`. `--tags-file` no longer re-observes committed rows; a row nobody names is carried verbatim. Under
`--tags-from-registry`, a committed row the listing lacks is asked by GET before it counts as absent, so an eventually
consistent listing cannot remove a row whose tag still resolves. Observation uses `ReadAddressing::Canonical`
(today `Mirrored`, `pipeline.rs:278`). The outcome per observed tag:

| Registry answer | Row in the base root | Result |
|---|---|---|
| Present | any | Upsert, as today; a new row gets the marker under `--ephemeral` |
| `MANIFEST_UNKNOWN` | ephemeral | Remove the row |
| `MANIFEST_UNKNOWN` | durable, named by `--tags` or `--tags-file` | Remove the row; the bot reviews it (S5: no review on an ungoverned index) |
| `MANIFEST_UNKNOWN` | durable, reached by `--refresh` or `--tags-from-registry` | Keep the row, list it in `durable_missing`, exit 0 |
| `MANIFEST_UNKNOWN` | none | `UnresolvedTag` (79), as today, so a typo stays an error |
| `NAME_UNKNOWN`, a 404 without an error body, any other error | any | An error, as today; nothing is removed |

Per selection:

- **`--tags-file`**: the table for every listed tag; every other row is carried verbatim.
- **`--tags`** keeps Replace: rows it does not name are dropped as today, and the bot reviews every durable drop.
  A repository the registry no longer has (`NAME_UNKNOWN`) fails every observation, so no announce form removes its
  rows; a human change to the index repository does. zot and distribution keep a repository after its last tag.
- **`--tags-from-registry`** becomes a sync. It adds every tag the listing holds (durable, or ephemeral under
  `--ephemeral`), removes gone ephemeral rows and reports gone durable ones. **Warning:** a snapshot that was pushed
  but not yet announced with `--ephemeral` becomes a durable row here, and every later prune of it is 81. Schedule
  `--refresh` for convergence, never `--tags-from-registry`.
- **`--refresh`** re-observes every committed row: a gone ephemeral row is removed, a gone durable row is reported.
- **`--ephemeral`** marks each tag the run *adds*. It never changes the marker on an existing row: the marker is
  immutable. It does not affect removals.

`regenerate` walks the committed rows in committed order: an observed row is regenerated (a moved digest keeps
`yanked` and `ephemeral`, `pipeline.rs:505-507`), a confirmed-absent row is dropped, a row the run was not given is
cloned verbatim; newly observed tags are appended. `CommittedTagsDropped` exempts exactly the confirmed-absent rows.
A run that changes nothing commits nothing (the zero-change reconcile). The commit body lists added, removed and
`durable_missing` tags plus the CI run URL. The JSON outcome gains `removed` and `durable_missing`.

Additions and removals travel in one announce, so a pipeline makes one index change. Announce is idempotent.

**Stale branches.** A diverged announce branch is rebuilt on the base with its branch-only rows unioned in
(`carry_branch_tags`, `pipeline.rs:472`, deliberately not a three-way merge). A rebuild can therefore re-propose a
removed ephemeral row; the next announce naming the tag, or `--refresh`, removes it again. A pending durable removal
holds the package's announce branch in human review; later snapshot announces join that branch, and after a stale
rebuild the durable removal must be re-proposed by naming the tag again.

**Help text changes.** `--tags-file`: "Add, update or remove the tags listed in this file: a listed tag the registry
no longer has is removed from the index. Rows the file does not list are left as they are." `--tags-from-registry`:
"Add every tag the registry holds; remove ephemeral rows whose tag is gone, and report durable ones."
`--refresh`: "Re-observe every committed tag; remove ephemeral rows whose tag is gone, and report durable ones."
`--ephemeral`: "Mark the tags this run adds as removable without review."

### Bot and index repository

| Change in a request | Bot verdict |
|---|---|
| Row removed, marked ephemeral **at the base**, registry confirms `MANIFEST_UNKNOWN` | Auto-merge under the existing owner governance |
| Row removed, ephemeral at the base, tag still present or no confirmation possible | Human review |
| Row removed, durable at the base | Human review, unconditionally, independent of every dial (this is how `prune --force` lands) |
| New row, with or without the marker | Existing governance, unchanged |
| Marker added to, or removed from, an existing row | Human review (the marker is immutable) |

The confirmation (ruling C) is a canonical manifest GET through the bot's `RegistryPort`, the port behind the G-15
network checks (`cli/validate_pr.py:80`, `core/registry_checks.py:25`). Today `get_manifest` raises `KeyError` on
any 404, so the port gains the error code. With network checks disabled there is no confirmation, and the removal
goes to human review. The bot reads the marker only from the base; a marker added in the same request is inert. The
review summary shows removals and marker diffs. `core/regenerate.py:95-96` preserves the marker. Reconcile does not
report a missing tag on an ephemeral row (announce is the remover). Schema: `ephemeral` on `$defs.tagEntry`. There
is no scoped owner role and no policy dial.

### Other commands

| Command | Behaviour |
|---|---|
| `ocx index update`, `index sync`, local index | A local row is dropped only when the authoritative root omits it **and** the local row is ephemeral (`RootScope::Package`, or the named tag under `Tags`); durable pins keep today's guarantee |
| Default-mode resolve | Unchanged (silence principle): a committed pin still resolves; content fetch is 79 after registry GC |
| `ocx --remote` resolve of a pruned ephemeral tag | 79, and the local row is dropped as for `index update <pkg>:<tag>` |
| `ocx package cascade repair` | The resync after a prune that removed a rolling tag's source. `--tags-file` now appends (`read_tags_file_if_present` + `merge_tags_file`, as push does) instead of truncating, and still writes an empty file |
| `ocx package push --tags-file` | Writes newline-separated, trailing newline (was comma-joined) |
| `ocx package copy` | Out of scope (R5); it writes no index, so no marker travels |
| ocx-mirror | Carries the marker verbatim; drops a destination row that is ephemeral and missing upstream |

### Caches

Every removal decision bypasses caches and `[mirrors]`: prune's DELETE and GET, prune's root read (no `[mirrors]`
index entry, caching disabled), announce's observation and the bot's confirmation are canonical. A transparent cache
in front of the registry can only answer a stale "present", which delays a removal (the safe direction). A stale
"absent" of a durable row lands in human review; of an ephemeral row, the bot's own confirmation catches it, and
otherwise the next announce naming the tag restores the row.

The local index copy is outside this rule. `index update`, `index sync` and `--remote` drop only ephemeral local
rows, and they read the configured index source, which a `[mirrors]` index entry may replace: the local copy follows
the consumer's configured authority, and an air-gapped host has only the mirror. A lagging mirror can drop only an
ephemeral local row, and the next update from a current source restores it. Nothing is written to a registry or a
published index.

### Multi-stage promotion

A stage is a tag convention ([research](./research_snapshot_multistage_promotion.md)); promotion is out of scope
(R5). A `:staging` retag announced without `--ephemeral` is durable (81 unless forced) and never matches a structural
prune; tag DELETE removes names, never digests, so the digest it shares with a pruned build stays reachable.

## Examples

Shared: `PKG=ocx.acme.example/acme/tool`, `REG=registry.gitlab.example.com/acme/tool`,
`FORGE="--index-repo gitlab.example.com/platform/ocx-index --forge gitlab"`. Push, prune and cascade repair all
append to one tags file; one announce publishes it.

**Canary** (every default-branch pipeline).

```sh
rm -f tags.txt
ocx package push --cascade --no-keep-tag --build-timestamp=datetime --tags-file tags.txt -i "$REG:0.5.0-canary" dist/tool.tar.xz
ocx package prune --prerelease 0.5.0-canary --keep-builds 10 --tags-file tags.txt "$PKG"
ocx package cascade repair --tags-file tags.txt "$REG"
ocx package announce $FORGE --tags-file tags.txt --ephemeral "$PKG"
```

Effect: the new build and the moved `0.5.0-canary` are announced (the build as a new ephemeral row, the rolling tag
keeping its marker); the 11th-newest build is deleted from the registry and its row removed, all in one request.
`cascade repair` moves nothing here, because the kept rolling tag already points at the newest build; it is in the
recipe for the prune that does move one. In CI, split push from the rest so a retry never pushes twice (CI below).

**Change request closed** (GitLab `environment:on_stop`, GitHub `pull_request: closed`; serialized per track).

```sh
rm -f tags.txt
ocx package prune --prerelease 0.5.0-mr42 --tags-file tags.txt "$PKG"
ocx package announce $FORGE --tags-file tags.txt "$PKG"
```

Effect: every `0.5.0-mr42_*` build and then `0.5.0-mr42` are deleted; the bot confirms each is gone and auto-merges
the removal of those ephemeral rows. `latest`, `0.5`, `0` are untouched. A teardown that failed midway is repaired by
rerunning it; the scheduled `--refresh` removes whatever rows a lost run left behind.

**Explicit list** and **lazy sync** (no tags file; a scheduled `--refresh` converges, never `--tags-from-registry`,
which would turn a snapshot pushed but not yet announced with `--ephemeral` into a durable row).

```sh
./compute-stale-tags.sh | xargs ocx package prune --tags-file tags.txt "$PKG"   # explicit: your policy
ocx package announce $FORGE --tags-file tags.txt "$PKG"
ocx package prune --prerelease 0.5.0-mr42 "$PKG"                               # lazy: teardown job
ocx package announce $FORGE --refresh "$PKG"                                    # lazy: nightly schedule
```

**`--force` on a release.**

```text
$ rm -f tags.txt
$ ocx package prune --force --tags-file tags.txt ocx.acme.example/acme/tool 1.2.3
Action   Tag    Digest        Reason
deleted  1.2.3  5d1e07a2c3f0
$ ocx package cascade repair --tags-file tags.txt registry.gitlab.example.com/acme/tool
$ ocx package announce $FORGE --tags-file tags.txt ocx.acme.example/acme/tool
index: merge request https://gitlab.example.com/platform/ocx-index/-/merge_requests/421 opened (removes 1.2.3; moves 1.2, 1, latest)
```

The bot labels !421 `human-review-required` (a durable row removed). Until it merges, the index still lists `1.2.3`
with its digest, and until registry GC the manifest still exists, so a resolve of `1.2.3` or of its digest pin keeps
working. After GC, content fetch is 79.

**Safeguard refusals.**

```text
$ ocx package prune --prerelease 0.5.0-canary --keep-builds 10 ocx.acme.example/acme/tool
error: refusing to delete 1 tag of ocx.acme.example/acme/tool: 0.5.0-canary_20260901080000 is durable in the index at https://ocx.acme.example; nothing was deleted
hint: pass --force to delete it anyway; the index row stays until a reviewed announce removes it
$ echo $?
81
$ ocx package prune --tags-file tags.txt ocx.acme.example/acme/tool 0.5.0-mr42_20260929120000
error: refusing to delete 1 tag of ocx.acme.example/acme/tool: 0.5.0-mr42_20260929120000 is not in the index at https://ocx.acme.example yet; nothing was deleted
hint: retry once its announce has merged, or announce it with --ephemeral
$ echo $?
75
```

**`--dry-run`** runs the safeguard and exits as the real run would.

```text
$ ocx package prune --dry-run --prerelease 0.5.0-canary --keep-builds 2 ocx.acme.example/acme/tool
Action        Tag                          Digest        Reason
would_delete  0.5.0-canary_20260926093300  c07e5d21a9b4
would_delete  0.5.0-canary_20260927141055  51be02f7d3aa
kept          0.5.0-canary_20260928101500  8812aa0c4e19  newest
kept          0.5.0-canary_20260929101500  e4d0b6a3f812  newest
kept          0.5.0-canary                 e4d0b6a3f812  rolling
$ ocx package prune --dry-run ocx.acme.example/acme/tool 0.4.2 0.5.0-canary_20260926093300
Action         Tag                          Digest        Reason
refused        0.4.2                        9b1f3e07aa21  durable
not_attempted  0.5.0-canary_20260926093300  c07e5d21a9b4
$ echo $?
81
```

`--format json` prints `{"package", "repository", "selection": {"tags": [...]} | {"prerelease", "keep_builds"},
"force", "dry_run", "index": {"url", "root_sha256"} | null, "tags": [{"tag", "digest", "action", "reason"}]}`, with
`action` one of `deleted`, `absent`, `would_delete`, `kept`, `refused`, `not_attempted`, and `reason` one of
`newest`, `rolling`, `durable`, `not_in_index`, `null`. A failed run prints the document before exiting.

**Index root before and after** the first dry run, executed (digests shortened).

```json
{ "repository": "oci://registry.gitlab.example.com/acme/tool",
  "tags": {
    "0.4.2":                       { "content": "sha256:9b1f…", "observed": "2026-09-01T10:00:04Z" },
    "0.5.0-canary":                { "content": "sha256:e4d0…", "observed": "2026-09-29T10:15:02Z", "ephemeral": true },
    "0.5.0-canary_20260926093300": { "content": "sha256:c07e…", "observed": "2026-09-26T09:34:10Z", "ephemeral": true },
    "0.5.0-canary_20260927141055": { "content": "sha256:51be…", "observed": "2026-09-27T14:11:40Z", "ephemeral": true },
    "0.5.0-canary_20260928101500": { "content": "sha256:8812…", "observed": "2026-09-28T10:15:31Z", "ephemeral": true },
    "0.5.0-canary_20260929101500": { "content": "sha256:e4d0…", "observed": "2026-09-29T10:15:02Z", "ephemeral": true } } }
```

After: the `0.5.0-canary_20260926093300` and `0.5.0-canary_20260927141055` rows are gone, every other byte is
identical, and `o/sha256/c07e….json` and `o/sha256/51be….json` are deleted in the same commit by the existing
`orphan_paths`. No tombstone is written.

### CI (GitLab)

```yaml
variables: { PKG: ocx.acme.example/acme/tool, REG: registry.gitlab.example.com/acme/tool,
             FORGE: --index-repo gitlab.example.com/platform/ocx-index --forge gitlab,
             OCX_AUTH_registry_gitlab_example_com_TYPE: basic }
.retry: &retry { retry: { max: 2, exit_codes: [69, 75] } }
canary-push:
  rules: [{ if: $CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH }]
  resource_group: snapshot-canary
  script: [ocx package push --cascade --no-keep-tag --build-timestamp=datetime --tags-file tags.txt -i "$REG:0.5.0-canary" dist/tool.tar.xz]
  artifacts: { paths: [tags.txt] }
canary-publish:
  <<: *retry
  rules: [{ if: $CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH }]
  needs: [canary-push]
  resource_group: snapshot-canary
  script:
    - ocx package prune --prerelease 0.5.0-canary --keep-builds 10 --tags-file tags.txt "$PKG"
    - ocx package cascade repair --tags-file tags.txt "$REG"
    - ocx package announce $FORGE --tags-file tags.txt --ephemeral "$PKG"
mr-push:
  rules: [{ if: $CI_PIPELINE_SOURCE == "merge_request_event" }]
  resource_group: snapshot-mr-$CI_MERGE_REQUEST_IID
  environment: { name: snapshot/mr-$CI_MERGE_REQUEST_IID, on_stop: mr-teardown, auto_stop_in: 3 days }
  script: [ocx package push --cascade --no-keep-tag --build-timestamp=datetime --tags-file tags.txt -i "$REG:0.5.0-mr$CI_MERGE_REQUEST_IID" dist/tool.tar.xz]
  artifacts: { paths: [tags.txt] }
mr-announce:
  <<: *retry
  rules: [{ if: $CI_PIPELINE_SOURCE == "merge_request_event" }]
  needs: [mr-push]
  resource_group: snapshot-mr-$CI_MERGE_REQUEST_IID
  script: [ocx package announce $FORGE --tags-file tags.txt --ephemeral "$PKG"]
mr-teardown:
  <<: *retry
  rules: [{ if: $CI_PIPELINE_SOURCE == "merge_request_event", when: manual }]
  resource_group: snapshot-mr-$CI_MERGE_REQUEST_IID
  environment: { name: snapshot/mr-$CI_MERGE_REQUEST_IID, action: stop }
  variables: { GIT_STRATEGY: none }
  script:
    - rm -f tags.txt
    - ocx package prune --prerelease "0.5.0-mr$CI_MERGE_REQUEST_IID" --tags-file tags.txt "$PKG"
    - ocx package announce $FORGE --tags-file tags.txt "$PKG"
index-refresh:
  rules: [{ if: $CI_PIPELINE_SOURCE == "schedule" }]
  script: [ocx package announce $FORGE --refresh "$PKG"]
snapshot-sweep:   # optional: tracks whose teardown never ran; ./closed-tracks.sh is your policy
  rules: [{ if: $CI_PIPELINE_SOURCE == "schedule" }]
  script:
    - rm -f tags.txt
    - ./closed-tracks.sh | xargs -r -I{} ocx package prune --prerelease {} --tags-file tags.txt "$PKG"
    - ocx package announce $FORGE --tags-file tags.txt "$PKG"
```

A retried `*-publish` or `*-announce` job reuses the pushed `tags.txt` artifact, so it never pushes a second build.
Registry credentials (`OCX_AUTH_…_USER`/`_TOKEN`) need push and delete rights on the project registry; the index
credential is `OCX_ANNOUNCE_TOKEN`. The GitHub Actions equivalent is in the system design.

## Exit codes

| Code | Source | When |
|---|---|---|
| 0 | | deleted, nothing selected, everything already `absent`, or a `--dry-run` whose real run would pass |
| 64 | CLI | neither TAG nor `--prerelease`, or both; `--prerelease` not a pre-release without a build; `--keep-builds` without `--prerelease` or below 1; a digest as TAG; announce `--ephemeral` with `--refresh` |
| 69 | registry / index | registry unreachable; index root unreachable (hint: the index locates the registry, retry; `--force` does not skip it) |
| 74 | local | `--tags-file` write failed, after the deletes it lists |
| 75 | registry / index | 429/503; a tag still present after the confirmation retries; a selected tag the served index does not list yet (`not_in_index`) |
| 78 | config | SSRF refusal on the registry host (`guarded_physical`) |
| 79 | index / announce | the index has no root for the package (prune); a named tag absent from registry and index (announce, unchanged) |
| 80 | registry | 401 or 403 (`AuthError`), including a token without `delete` |
| 81 | policy | safeguard refusal of a `durable` tag; no index configured without `--force`; `--offline` |
| **87** | registry | **new** `RegistryDeleteUnsupported`: 405, or 400 `UNSUPPORTED` |

A `--dry-run` exits 81 or 75 exactly as the real run would. 87 gets `ErrorCategory::RegistryDeleteUnsupported`
(`error.kind: "registry_delete_unsupported"`), classified in `crates/ocx_cli/src/exit/ocx_oci.rs` beside
`ReferrersUnsupported`.

## Data model

`schema/root.schema.json` (index repository), `$defs.tagEntry.properties`:

```json
"ephemeral": { "const": true, "description": "Set when the tag was announced with --ephemeral. Removal of this row merges without review once the registry confirms the tag is gone. Absence means durable." }
```

`const: true` keeps one spelling. Key order: `content, observed, yanked, ephemeral`. Readers without the key ignore
it (no `deny_unknown_fields`). In `ocx_index` the read path parses it into `TagLifetime { Durable, Ephemeral }`.

## Reversibility

| Part | Door |
|---|---|
| CLI grammar, JSON report, exit 87, tags-file newline format | Two-way (pre-1.0, changelog-only) |
| `ephemeral` key in published roots | **One-way**: published roots must keep parsing |
| Bot: durable removal always reviewed | **One-way in practice**: loosening re-opens today's ungated removal |
| Registry tag DELETE | **One-way**: the manifest survives only until registry GC; before that, `package copy` of the digest restores the tag |
| Merged index removal | Two-way while the tag exists: `announce --tags-file` re-adds it |

## Threats

| Threat | Mitigation |
|---|---|
| Stolen forge token removes a durable row | Durable removal is always human review on a governed index |
| Stolen forge token removes ephemeral rows of live tags | The bot confirms `MANIFEST_UNKNOWN` on the canonical registry before auto-merging (ruling C) |
| Named durable removal on an ungoverned index | No review by design (S5): the user named the tag; unnamed durable rows are never touched |
| Mark-and-remove in one request | The bot reads the marker from the base only |
| Marker added to a release, then pruned | Marker changes are human review; no client path mutates a marker |
| Prune of a durable tag by mistake | Safeguard aborts before any delete; `--force` is explicit |
| Stale safeguard read | Canonical index URL, caching disabled, `[mirrors]` index bypassed. The marker is immutable, so a stale copy cannot turn durable into ephemeral; the danger is a tag removed and re-announced durable under the same name, or a moved `repository` pointer, both visible only in a fresh root |
| Stolen registry credential deletes tags | Recorded once: push plus `delete` widens a push credential from overwrite to removal. Recommended control: registry-side protected or immutable tag rules for release patterns (GitLab protected container tags, Harbor immutable tags) |
| Mirror or cache answers a removal decision | Canonical addressing for every DELETE, confirmation, observation and root read |
| Digest delete removes a sharing tag | Tag DELETE only |
| Late publish resurrects a torn-down track | Per-track serialization; the next teardown or scheduled `--refresh` converges; not a safety issue |
| Repudiation | Commit body carries added/removed tags and the CI run URL; registry audit logs |

## Rollout and documentation

Order: ocx-indexbot (classification, registry confirmation, marker in writers and reconcile), then the index
repository (schema line, bot pin), then ocx, then ocx-mirror (`task satellite:verify`). Absence of the key means
durable, so no data migration. Work packages and the test plan: system design §§ 6-7.

Documentation surfaces: help text of `--tags-file`, `--tags-from-registry`, `--refresh`, new `--ephemeral`, and of
`push --tags-file` / `cascade repair --tags-file` (newline format, append);
`website/src/docs/reference/command-line.md` (the prune section; the announce rows at 2969-2970; the "a merge never
deletes" sentence at 1230; the cascade repair tags-file sentence); `website/src/docs/authoring/announcing.md:102-103`;
`website/src/docs/in-depth/indices.md:141` ("an update never deletes a pin" gains the ephemeral exception); a
snapshot-tracks how-to carrying the examples above and the `--no-keep-tag`, fixed-width build-id and per-track
serialization guidance; `subsystem-cli-commands.md` (`package prune` row; announce row; `cascade repair --tags-file`
note, now append; `index update` row); the exit-code and `error.kind` tables (87); the `PolicyBlocked` doc in
`ocx_exit::ExitCode` (adds the prune safeguard); the index governance page (ruling C).

Keep tags are a required documentation point (owner, 2026-09-29), not a runtime check: prune deletes only the
image-index tag, so a `__ocx.keep.*` tag left on a platform manifest keeps that manifest and its layers out of
registry GC and nothing is reclaimed. Neither announce nor prune checks for them (announce is not a linter). The
guidance "push ephemeral builds with `--no-keep-tag`, or prune frees no storage" appears in all of: the `prune`
long help, the `--ephemeral` long help, the `push --keep-tag` long help (when to disable it), the command-line
reference for all three, and the snapshot-tracks how-to (with the reason and what happens if it is missed).

## Prior Art

| System | Relevance |
|---|---|
| [OCI distribution spec, deleting tags](https://github.com/opencontainers/distribution-spec/blob/main/spec.md#deleting-tags) | Tag DELETE optional; 405 when unsupported |
| [GitLab cleanup policies](https://docs.gitlab.com/user/packages/container_registry/reduce_container_registry_storage/), [environments `on_stop`](https://docs.gitlab.com/ci/environments/), [`resource_group`](https://docs.gitlab.com/ci/resource_groups/) | Registry retention; event teardown; per-track serialization |
| [crates.io index `yanked`](https://doc.rust-lang.org/cargo/reference/registry-index.html) | Presence marker on the row |
| [npm dist-tags](https://docs.npmjs.com/cli/v10/commands/npm-dist-tag), Maven `-SNAPSHOT` | Mutable/durable split |
| [k8s promo-tools](https://github.com/kubernetes-sigs/promo-tools/pull/1997) | Build once, promote by copy |

## Open Questions

1. [NEEDS CLARIFICATION: ruling C's registry confirmation against a private registry needs a registry credential in
   the bot's CI, which it holds today only where G-15 network checks are configured.] **Recommended:** reuse the
   G-15 `RegistryPort` credentials; a missing credential or disabled network checks sends the removal to human
   review, never to auto-merge.
2. [NEEDS CLARIFICATION: a build pushed but never announced (the announce job failed and a whole-job retry pushed a
   fresh build) is never in the index, so every later prune that selects it exits 75, which ruling B calls
   self-healing but no scheduled job heals.] **Recommended:** keep 75 as ruled; the examples split push from publish
   so a retry never re-pushes, and the 75 hint names the fix (`announce --tags-file` with the tag and
   `--ephemeral`). Prune must not announce it itself, which would make it a second index writer.

## Changelog

| Date | Change |
|---|---|
| 2026-09-29 | Initial draft; review round 1 |
| 2026-09-29 | Owner redesign: low-level prune, absence-driven announce, bot as removal authority |
| 2026-09-29 | Final fix pass: rulings A, B, C and the S5 amendment to R6; root-based routing for prune; one tags file (newline format, `cascade repair` appends); scheduled `--refresh`; dry run runs the safeguard; per-track serialization; stale-branch rule without a three-way carry |
| 2026-09-30 | Execution amendments: prune appends only tags the served root lists (plan DEC-40); no `--tags` escape hatch for a vanished repository (DEC-16); the local-copy drop follows the configured source (DEC-15, DEC-43) |
