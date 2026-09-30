# System Design: Snapshot tag lifecycle

**Status:** Proposed. **Date:** 2026-09-29. **Decision record:** [`adr_snapshot_lifecycle.md`](./adr_snapshot_lifecycle.md)
(grammar, rules, exit codes, examples and the owner decision log live there; this document does not restate them).

## 1. Context (C4 level 1)

```
 CI pipeline (GitHub Actions | GitLab CI), serialized per track
   |  ocx package push / prune / cascade repair      registry credential (push + delete)
   v
 OCI registry (canonical)  <---- announce: canonical GET per given tag, tag listing
   ^                       <---- bot: canonical GET before an ephemeral auto-merge (ruling C)
   |  ocx package announce                           forge credential (OCX_ANNOUNCE_TOKEN)
   v
 Index repository (forge)  --- bot: classify, auto-merge or human review
   |  publish from the base branch only
   v
 Served index (https)  <---- prune: canonical root read (routing + safeguard), no cache, no [mirrors]
   ^
   |  ocx index update / sync, resolve
 Consumers (local index copy)
```

Prune touches the registry and reads the served index. Announce touches the registry (read) and the forge (write).
Nothing else writes the index. GitHub governance is event-driven; GitLab governance is the existing
`governance-poll` schedule (`ci/templates/gitlab/indexbot.yml:147-160`), so a removal waits for the next poll.

## 2. Containers (C4 level 2)

| Container | Crate / repo | Change |
|---|---|---|
| `ocx package prune` | `ocx_cli` (`command/package_prune.rs`) | New command: argv, routing, selection, safeguard, delete loop, report |
| Registry primitives | `ocx_oci` (`client.rs`, `native_transport.rs`), `external/rust-oci-client` | `RegistryOperation::Delete`, `delete_tag`, `probe_manifest_canonical`, `DeleteUnsupported` |
| Selection | `ocx_package` (`version.rs`) | None: `Version` parse and `Ord` are reused as they are |
| Served-root read | `ocx_index` (`OcxIndex`) | Canonical, uncached read of one root that commits nothing locally |
| Announce | `ocx_announce` (`pipeline.rs`, `announce.rs`, `request.rs`) | Ruling A observation set, canonical observation, removal rules, carry-verbatim `regenerate`, `--ephemeral` |
| Tags files | `ocx_cli` (`conventions.rs`, `command/package_cascade_repair.rs`) | Newline format; `cascade repair` appends |
| Local index | `ocx_index` (`LocalIndex::commit_published_root`) | Drop an ephemeral row on authoritative absence |
| Bot + schema | index bot repo; index repository | Classification rows, registry confirmation, marker preservation, `tagEntry.ephemeral` |
| Mirror | ocx-mirror (`registry_sync/index_write.rs`) | Carry the marker; drop destination ephemeral rows missing upstream |

## 3. Sequences

### 3.1 Prune

```
prune(pkg, selection, force, tags_file, dry_run)
  parse argv                                              -> 64
  --offline                                               -> 81
  if config [registries."<ns>"] index:
      root = OcxIndex::fetch_root_uncached(pkg)          -> unreachable: 69 ("the index locates the registry; retry")
      root none                                           -> 79
      repo = guarded_physical(root.repository, pkg.registry, trusted, insecure, proxy_rules)   -> 78
  else: root = none; repo = pkg as a physical repository (ordinary client host guard)
  selection:
      --prerelease: list_tags(repo, canonical); family by Version parse; Version::cmp;
                    --keep-builds N: keep newest N (reason newest) + rolling (reason rolling)
      TAG...:       deduplicated, input order kept
  root none and not --force         -> 81 (no index to ask)
  classify every selected tag (all deletable under --force, except absent-everywhere tags):
      in root, ephemeral            -> deletable
      in root, no marker            -> refused durable          (skipped under --force)
      not in root: probe_manifest_canonical(repo:tag)
          Absent(_)                 -> absent, nothing to do
          Present(_)                -> refused not_in_index     (skipped under --force)
  any refused: exit 81 if any durable, else 75; nothing deleted; others not_attempted
  if dry_run: report would_delete / kept / refused, exit as the real run would; write nothing
  ensure_auth(repo, Delete)                               -> 80
  for tag in deletable (builds oldest first, rolling last):
      r = delete_tag(repo:tag)
          DeleteUnsupported (405 | 400 UNSUPPORTED)       -> 87 (first tag, so nothing deleted)
          other error                                     -> stop; rest not_attempted; exit by source
      confirm: probe_manifest_canonical, up to 3 tries 250 ms apart
          Absent(ManifestUnknown | NameUnknown | Unspecified) -> gone (deleted, or absent if r was AlreadyAbsent)
          Present after 3 tries                           -> 75
  gone ∪ (absent tags still in root) -> merge into tags_file (written even when empty)   -> 74
  report
```

The safeguard runs once, before the first DELETE; there is no per-tag re-check, because the served index only
changes through a reviewed merge and prune never writes it. `NameUnknown` and `Unspecified` count as gone only after
this run's own DELETE returned success.

### 3.2 Announce observation

```
given = --tags list | --tags-file lines | listing (--tags-from-registry) | committed rows (--refresh)
probe = given ∪ (--tags-from-registry: committed rows the listing lacks)
--ephemeral with --refresh                                   -> 64
for tag in probe (OBSERVE_CONCURRENCY):
    fetch_manifest_raw_bytes_addressed(tag, Canonical)
        Some(bytes)  -> Present: observe as today
        None         -> probe_manifest_canonical(tag)
            Absent(ManifestUnknown):
                base row ephemeral                           -> remove
                base row durable, named (--tags, --tags-file) -> remove (bot reviews)
                base row durable, not named                  -> durable_missing, keep
                no base row                                  -> UnresolvedTag (79)
            Absent(NameUnknown | Unspecified)                -> error, nothing written
            Present(_) (raced)                               -> 75
    other error                                              -> error, as today
regenerate(committed, observed, absent):
    for row in committed, in committed order:
        observed  -> regenerate (moved digest keeps yanked and ephemeral)
        absent    -> drop
        --tags and not named -> drop (Replace, as today)
        otherwise -> clone verbatim
    append newly observed tags in observation order (marked under --ephemeral); shift_remove("variants")
CommittedTagsDropped guard: exempt exactly the confirmed-absent rows
stale branch: carry_branch_tags union (not three-way); zero-change reconcile; orphan_paths; commit body
```

`fetch_manifest_raw_bytes_addressed` returns `Ok(None)` on any not-found and cannot tell the codes apart, so the
probe runs only on that path: one extra GET per absent tag.

### 3.3 Bot classification

```
for each key in base.tags ∪ head.tags:
    only in head                  -> addition (existing rules)
    only in base, durable         -> removal_durable     (human-review-required, ignores dials)
    only in base, base.ephemeral  -> RegistryPort.get_manifest(repository:key), canonical
        MANIFEST_UNKNOWN          -> removal_ephemeral   (auto-merge eligible)
        present | other code | no network checks | no credential -> human-review-required
    in both, ephemeral differs    -> marker_change       (human-review-required)
    in both, content differs      -> refresh             (existing rules)
summary lists removals and marker changes; one review label wins over any auto-merge path
```

## 4. Recovery

| Interruption | State | Recovery |
|---|---|---|
| Explicit prune stops mid-list | Some tags deleted, some listed in the tags file | Rerun: gone tags still in the root are appended again |
| Structural prune deleted, announce never ran | Rows point at deleted tags; a rerun selects nothing | Scheduled `announce --refresh` removes the ephemeral rows |
| Announce opens a request, bot has not merged | Removal pending; a prune of the same tags is 75 meanwhile | GitLab: next governance poll; a rerun updates the same branch |
| Durable removal awaits review | Row still served; the tag's digest resolves until registry GC | Merge, or re-push the tag and rerun announce |
| Stale branch rebuilt | A removed ephemeral row may be re-proposed; a pending durable removal is lost | Next announce naming the tag, or `--refresh`; re-propose the durable removal by naming it |
| Build pushed, never announced | Not in the index; every prune that selects it is 75 | `announce --tags-file` listing it with `--ephemeral` (ADR open question 2) |
| Rolling tag lost its source | Rolling tag points at a deleted build digest (still stored until GC) | `ocx package cascade repair`, then announce the moved tags |
| Tag deleted by mistake | Manifest stored until registry GC | `ocx package copy` of the digest to the tag, then announce |

## 5. Contracts

Rust shapes, sketched; names are final unless review renames them.

```rust
// ocx_oci::Client — a narrow canonical GET probe; ClientError::ManifestNotFound(String) stays untouched
pub async fn probe_manifest_canonical(&self, id: &OciIdentifier) -> Result<ManifestPresence, ClientError>;
pub enum ManifestPresence { Present(Digest), Absent(NotFoundCode) }
pub enum NotFoundCode { ManifestUnknown, NameUnknown, Unspecified }
pub async fn delete_tag(&self, id: &OciIdentifier) -> Result<DeleteOutcome, ClientError>; // canonical only
pub enum DeleteOutcome { Deleted, AlreadyAbsent }                                         // 2xx | 404
// ocx_oci::client::error::ClientError, new variant
DeleteUnsupported { registry: String, status: u16 },                                      // 405, or 400 UNSUPPORTED

// external/rust-oci-client token_cache.rs
pub enum RegistryOperation { Push, Pull, Delete }   // Delete scope = "pull,push,delete"

// ocx_index::OcxIndex
pub async fn fetch_root_uncached(&self, id: &PackageRef) -> Result<Option<RootView>, Error>;
pub struct RootView { pub sha256: Digest, pub repository: String, pub tags: BTreeMap<String, TagLifetime> }
pub enum TagLifetime { Durable, Ephemeral }

// ocx_announce
pub struct AnnounceRequest { /* ... */ pub ephemeral: bool }
pub enum Observation { Present(ObservedTag), Absent }
pub struct AnnounceOutcome { /* ... */ pub removed: Vec<String>, pub durable_missing: Vec<String> }

// ocx_cli (command-local)
enum PruneSelection { Tags(Vec<String>), Prerelease { family: Version, keep_builds: Option<NonZeroU32> } }
enum PruneAction { Deleted, Absent, WouldDelete, Kept, Refused, NotAttempted }
enum PruneReason { Newest, Rolling, Durable, NotInIndex }            // serialized null when absent
```

The probe is a GET because a HEAD carries no error body, and it is not the existing HEAD-based
`probe_manifest_digest_addressed` (`client.rs:1432`). `native_transport.rs:187-209` folds `NAME_UNKNOWN` and a bare
404 into `ManifestNotFound`; the probe reads the error envelope before that fold, and every existing caller keeps the
fold. Classification: `DeleteUnsupported` → 87 in `crates/ocx_cli/src/exit/ocx_oci.rs`; the safeguard refusals are a
prune error mapped to 81 (`durable`, no index) or 75 (`not_in_index`) in the same file family.

Tags files: `conventions::merge_tags_file` joins with `\n` and ends with a trailing newline (was `,`);
`parse_tags_file` already splits on `\n`, `\r` and `,`, so old files still parse. `cascade repair --tags-file`
replaces its truncating `tokio::fs::write` (`package_cascade_repair.rs:96-102`) with the read-merge-write push uses
(`options::tags::read_tags_file_if_present` + `merge_tags_file`, `package_push.rs:604-611`) and still writes when
empty. Prune uses the same pair.

Wire: `schema/root.schema.json` `$defs.tagEntry.properties.ephemeral` (`const: true`). Prune JSON report as in the
ADR. `AnnounceOutcome` JSON gains `removed` and `durable_missing` (always present, possibly empty). The bot's
`RegistryPort` gains the not-found code (today `get_manifest` raises `KeyError` on any 404).

## 6. Work packages

File-disjoint. DAG: `WP1 → WP2 → WP3 → WP4 → WP5`; `WP6` and `WP7` run beside WP3; `WP8` after WP2.

| WP | Scope | Files | Done when |
|---|---|---|---|
| WP1 | Bot: classification rows, ruling-C confirmation (`RegistryPort` exposes the code), marker preservation, summary, reconcile skip | index bot repo | Bot tests red/green for each branch of § 3.3, including "present" and "no credential" |
| WP2 | Index repository: schema line, bot pin | index repository | Schema validates a marked and an unmarked root |
| WP3 | `ocx_oci`: `probe_manifest_canonical`, `ManifestPresence`, `NotFoundCode`, `Delete` scope, `delete_tag`, `DeleteUnsupported`; fork change; a delete-enabled registry:2 service in `test/docker-compose.yml` | `crates/ocx_oci`, `external/rust-oci-client`, `test/docker-compose.yml` | Unit tests over each envelope code; zot DELETE green; delete-disabled registry:2 gives 405 → `DeleteUnsupported`; the delete-enabled registry:2 tag-DELETE answer measured and mapped to 87 (unverified until measured) |
| WP4 | Announce: ruling A (`resolve_curated_tags` UnionFile observes only the file), canonical observation, removal rules, carry-verbatim `regenerate`, `--ephemeral`, 64 with `--refresh`, guard exemption, outcome fields, help text | `crates/ocx_announce`, `crates/ocx_cli/src/command/package_announce.rs` | § 3.2 covered by pipeline tests; help text updated |
| WP5 | `ocx package prune`: argv, routing, selection, safeguard, delete loop, report, exit mapping, `ocx_exit` 87; docs surfaces | `crates/ocx_cli/src/command/package_prune.rs`, `api/data/package_prune.rs`, `crates/ocx_cli/src/exit/`, `crates/ocx_exit`, `test/tests/test_package_prune.py`, docs | Acceptance module green; `ocx_exit` gains 87 |
| WP6 | Tags files: newline `merge_tags_file`; `cascade repair` appends | `crates/ocx_cli/src/conventions.rs`, `crates/ocx_cli/src/command/package_cascade_repair.rs` | Unit tests below |
| WP7 | Local index: drop ephemeral row on authoritative absence | `crates/ocx_index/src/local_index.rs` | Durable pin survives, ephemeral row drops, both tested |
| WP8 | ocx-mirror: carry marker, drop destination ephemeral rows missing upstream | ocx-mirror `registry_sync/index_write.rs` | `task satellite:verify` green; kept only if the diff stays under about 100 lines |

## 7. Test plan

| Layer | Test | Proves |
|---|---|---|
| Unit (`ocx_package`) | Family selection over mixed tags: other variants, other pre-releases, releases, non-versions | Only `<ver-pre>_<build>` plus the rolling tag match |
| Unit (`ocx_cli`) | `--keep-builds` with fixed-width datetime ids; `--prerelease` with a build or no pre-release | Newest N kept by `Version::cmp`; 64 on grammar |
| Unit (`ocx_cli`) | Safeguard matrix: ephemeral, durable, not in root and present, not in root and absent, durable plus not-in-index | Deletable / 81 / 75 / `absent` / 81 wins |
| Unit (`ocx_cli`) | `merge_tags_file` output; `cascade repair` onto an existing file; `cascade repair` with nothing moved | Newline plus trailing newline, parse round-trip; appended, not truncated; file still written |
| Unit (`ocx_oci`) | Envelope `MANIFEST_UNKNOWN` / `NAME_UNKNOWN` / bare 404 through the probe | Three distinct `NotFoundCode`s; `ManifestNotFound` callers unchanged |
| Unit (`ocx_announce`) | Each branch of § 3.2 against a fake registry and base root | Remove, keep, report, 79 or 64 exactly as sketched |
| Unit (`ocx_announce`) | `regenerate` with named, absent and unnamed rows | Unnamed rows byte-identical and in committed order; absent dropped; new appended; moved row keeps `yanked` and `ephemeral` |
| Unit (`ocx_announce`) | Stale rebuild: base still holds a row the branch removed | Row re-proposed; the next announce naming it removes it again |
| Acceptance (zot) | Push, prune `--prerelease --keep-builds 1`, cascade repair, one announce `--ephemeral --out`, all through one tags file | Registry lost the builds; written root lost exactly those rows and gained the new build |
| Acceptance (zot) | Prune of a durable tag without `--force`, with and without `--dry-run` | 81 both times; registry unchanged (tag list compared before and after) |
| Acceptance (zot) | Prune of a pushed tag the index does not list | 75; registry unchanged |
| Acceptance (zot) | Prune with no index configured, with and without `--force` | 81, then deleted |
| Acceptance (zot) | `--dry-run` of a passing selection | Exit 0; registry unchanged; report lists `would_delete` and `kept` |
| Acceptance (zot) | Prune the last tags of a repository | Exit 0; confirmation accepts `NAME_UNKNOWN` |
| Acceptance (zot) | `announce --refresh --out` with a gone durable and a gone ephemeral row | Exit 0; ephemeral removed; durable kept and in `durable_missing` |
| Acceptance (registry:2, delete disabled and enabled) | Prune of one tag | 87 both; tag still present (enabled case pending WP3's measurement) |
| Acceptance | Rerun of an explicit prune that already deleted everything | Exit 0; tags file lists the same tags |
| Acceptance | Rerun of a structural prune that already deleted everything, then `announce --refresh --out` | Prune selects nothing, exit 0; refresh removes the rows |

Each negative case is shown red on a mutation before it is trusted: remove the safeguard call and the durable-prune
test must fail; switch announce back to `Mirrored` and the canonical-absence test must fail; make `regenerate` drop
unnamed rows and the carry test must fail.

Obligations: the new module carries `@pytest.mark.smoke` on one prune test and a `command` marker; `test/SUITE_FLOOR`
rises in the same commit; the module gets its Bazel `sh_test` target (it appears in `ACCEPTANCE_MODULE_TARGETS`)
and a `module_slots` entry for any `xdist_group` it names; the index server fixture it reads goes in `module_data`.

## 8. GitHub Actions example

```yaml
on:
  pull_request: { types: [opened, synchronize, reopened, closed] }
  schedule: [{ cron: "17 3 * * *" }]
concurrency: { group: "snapshot-${{ github.event.number || 'schedule' }}", cancel-in-progress: false }
env:
  PKG: ocx.acme.example/acme/tool
  REG: registry.acme.example/acme/tool   # must accept tag DELETE; GHCR does not (prune exits 87)
  FORGE: --index-repo github.com/acme/ocx-index --forge github
jobs:
  push:
    if: github.event_name == 'pull_request' && github.event.action != 'closed'
    runs-on: ubuntu-latest
    outputs: { tags: "${{ steps.push.outputs.tags }}" }
    steps:
      - uses: ocx-sh/setup-ocx@25fa771f8572572dc64528db89560de68a163a0e # v1.4.0
      - id: push
        run: |
          ocx package push --cascade --no-keep-tag --build-timestamp=datetime --tags-file tags.txt \
            -i "$REG:0.5.0-pr${{ github.event.number }}" dist/tool.tar.xz
          echo "tags=$(paste -sd, tags.txt)" >> "$GITHUB_OUTPUT"
  announce:
    needs: push
    runs-on: ubuntu-latest
    steps:
      - uses: ocx-sh/setup-ocx@25fa771f8572572dc64528db89560de68a163a0e # v1.4.0
      - run: |
          printf '%s\n' "$TAGS" > tags.txt
          ocx package announce $FORGE --tags-file tags.txt --ephemeral "$PKG"
        env: { TAGS: "${{ needs.push.outputs.tags }}", OCX_ANNOUNCE_TOKEN: "${{ secrets.OCX_INDEX_TOKEN }}" }
  teardown:
    if: github.event_name == 'pull_request' && github.event.action == 'closed'
    runs-on: ubuntu-latest
    steps:
      - uses: ocx-sh/setup-ocx@25fa771f8572572dc64528db89560de68a163a0e # v1.4.0
      - run: |
          ocx package prune --prerelease "0.5.0-pr${{ github.event.number }}" --tags-file tags.txt "$PKG"
          ocx package announce $FORGE --tags-file tags.txt "$PKG"
        env: { OCX_ANNOUNCE_TOKEN: "${{ secrets.OCX_INDEX_TOKEN }}" }
  index-refresh:
    if: github.event_name == 'schedule'
    runs-on: ubuntu-latest
    steps:
      - uses: ocx-sh/setup-ocx@25fa771f8572572dc64528db89560de68a163a0e # v1.4.0
      - run: ocx package announce $FORGE --refresh "$PKG"
        env: { OCX_ANNOUNCE_TOKEN: "${{ secrets.OCX_INDEX_TOKEN }}" }
```

The workflow-level `concurrency` group serializes each pull request's runs (the residual race in the ADR). Push and
announce are separate jobs, so "Re-run failed jobs" repeats the announce with the same tags and never pushes a second
build; `announce --tags-file` reads the comma-joined output as well as newlines. Registry login for push and delete
is the usual `ocx login --password-stdin` step; it is omitted above.
