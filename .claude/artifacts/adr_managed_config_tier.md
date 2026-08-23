# ADR: Corporate Managed Configuration Tier (`[managed]`)

> **v2 amendment (2026-07-05, managed-config v2 — config-as-package).** The custom
> artifact wire shape and the oras-push publish recipe (Decision H, and the
> `application/vnd.sh.ocx.config.v1`(+toml) media types referenced throughout) are
> **superseded**. Managed config is now an **ordinary ocx package** whose content is a
> single `config.toml` (image index -> `any/any` entry -> image manifest -> tar+gzip
> layer; flat image manifests accepted). Rationale: publish/versioning/cascade/rollback
> reuse the existing package machinery instead of growing a parallel artifact subsystem;
> the feature was unreleased, so the flag-day rewrite carried no migration burden.
> Concretely:
>
> - **Decision H reversed** — `ocx config push` exists (validate payload: parses as
>   `Config`, no `[managed]` section, <= 64 KiB; stage as `config.toml`; bundle tar+gzip;
>   synthesized minimal bundle metadata; `Publisher::push`/`push_cascade`). The oras
>   recipe is dropped from the docs.
> - **Fetch leg v2** — resolve top manifest (its digest = drift identity, same value the
>   HEAD-based probe returns), select the `any/any` platform entry (flat image accepted),
>   pull the tar+gzip layer with three independent 64 KiB caps (declared size, streamed
>   bytes, decompressed bytes), digest re-verify in the fetch, scan the tar for
>   `config.toml` (extra entries ignored). Zero CAS interaction; patches excluded by
>   construction; the local-only mirror posture is unchanged.
> - **Identity gate v2** — snapshot matches when `registry/repository` is equal
>   (`without_specifiers`) AND any seed digest pin equals the snapshot digest. Tags float
>   within a repository (enables `ocx config update <VERSION>` pins/rollbacks under a
>   fleet floating tag); cross-repo snapshots stay rejected (CI cache-poison defense).
>   Supersedes Decision A(iii)'s "tag/digest significant" full-identity equality.
> - **Snapshot v2** — adds optional `tag` (the tag synced at persist time); v1 snapshots
>   stay readable (`tag = None`), no migration.
> - **`ocx config update [VERSION] --pause <dur> --resume`** — VERSION = tag | digest |
>   tag@digest (tag@digest fetches by TAG and fail-closed asserts the digest, exit 65);
>   `--pause` (cap 7d, content-bearing `state/managed-config/pause.json`, atomic write)
>   short-circuits ONLY the background tick, never the required gate; any explicit update
>   without `--pause` clears the pause; expired/corrupt pause reads as absent.
> - **`deny_unknown_fields` dropped on `[managed]`** (fleet forward-compat: a seed
>   written for a newer ocx must not brick older binaries; supersedes Decision C's
>   fail-fast-typo posture at the parse level — the fence machine is unchanged).
> - **TOFU digest surfacing** — `ocx config push` reports the index digest;
>   `self setup --managed-config` reports it on `adopted` AND `already_adopted`;
>   `--check` compares against it.
> - Review findings W1–W3, W5–W8 folded in (fresh-machine one-liner doc, gated
>   `Context::managed_config_snapshot()`, setup self-heal of a wiped/mismatched snapshot,
>   clear-path tests, corrupt-embedded-TOML loader test, `config update` doc link, probe
>   error debug-logging).
> - **Decision F activation-conditions documentation fix (doc-only, 2026-07-05).** The
>   background tick only fires when stderr is a TTY, outside CI, online, unpaused, and past
>   the `interval` throttle window (`check_for_managed_config_refresh` + the pause/throttle
>   checks inside `check_managed_config_refresh`) — the doc previously undersold this by
>   listing only the CI/offline/TTY trio. Practically: `refresh = "apply"` never
>   auto-converges a CI runner or another headless host; those hosts converge only through
>   the explicit `ocx config update` CI recipe (Decision B). Website + this ADR updated to
>   state the full gate list; no code change.
> - **Trust-boundary clarification (CWE-345, doc-only).** The identity gate (Decision A)
>   defends against cross-repository/cached-registry snapshot poisoning — it is not a
>   defense against an attacker who already has local write access to `$OCX_HOME`.
>   `config.toml`, `state/managed-config/snapshot.json`, `state/managed-config/config.toml`, and `pause.json` are ordinary local
>   state at the same trust level as any other file such an attacker could already edit; the
>   tier's digest pins bind fetch-time content, not load-time tamper-evidence.
>
> Plan: `.claude/state/plans/plan_managed_config_v2.md`. Where any text below conflicts
> with this amendment, the amendment governs.

> **Forward-compat amendment (2026-07-31, fleet payload tolerance + gate correctness).**
> Two defects surfaced from the same question — what an older ocx does with a payload a
> newer one published — and are decided here. Where earlier text conflicts, this governs.
>
> - **Payload tolerance is a tier requirement, not a config-parser preference.** No
>   `deny_unknown_fields` anywhere in the `Config` tree: unknown top-level sections AND
>   unknown keys inside `[registry]`, `[registries.<name>]`, `[mirrors."<host>"]` are
>   ignored, joining the root/`[patches]`/`[managed]` posture. Those three tables denied
>   unknown fields for typo protection, which is the wrong trade for a surface this tier
>   makes fleet-wide: one added key failed the parse, and a failed parse costs the
>   *entire* payload on every host that had not upgraded — `ocx config update` exited 78,
>   and the loader dropped the snapshot whole. Typo detection moves to the published JSON
>   schema (editor-time, where it is cheap); the `additionalProperties: false` it emitted
>   for those two objects goes with the change. A `[mirrors."<host>"]` entry left with no
>   role this binary recognizes is skipped for that host rather than raised (parse-time
>   `EmptyEntry` deleted; `resolve_mirror_map`'s role-less guard stays).
> - **The `required` gate reads what was applied, not what exists.** Identity match is
>   necessary, never sufficient: an identity-matching snapshot whose payload failed to
>   parse was dropped by the loader while the gate reported the tier satisfied —
>   fail-open in the one place `required = true` asks to fail closed. The loader now
>   reports a `ManagedSnapshotState` (`Unmatched` | `PayloadUnusable` | `Applied`) and
>   `enforce_required_snapshot` consumes it, adding `ManagedConfigError::SnapshotUnusable`
>   (exit 78, beside `SnapshotRequired`). Decision E's remediation table below gains that
>   row. `required = false` warns instead of failing — a broken payload is not the benign
>   absent state the debug-only path exists for.
> - **The escape hatch for a genuinely incompatible change is the tag, not the parser.**
>   Tolerance covers added keys; it cannot cover a key whose meaning or value shape
>   changed, which an older binary would read with the old meaning. Those changes publish
>   under a new tag family (`:user-2`) with the old tag left serving old content; hosts
>   move as they upgrade. This is why `source` is a full OCI reference and not a URL —
>   recorded here because nothing else made the requirement explicit.

## Metadata

**Status:** Accepted · **Date:** 2026-07-04 · **Deciders:** @michael-herwig, architect (swarm-plan tier=high), Round-1 review panel
**Scope:** Medium; One-Way-Door Medium (`[managed]` seed schema + `OCX_MANAGED_CONFIG` semantics + artifact media types/manifest shape become wire contract once shipped)
**No new runtime dependency.** Reuses `toml`, OCI client, `FileStructure::state`, `config/patch.rs` + `setup/rc_block.rs` precedents.

> This ADR integrates the Round-1 review-panel amendments directly; where an original option text conflicts with an **(AMENDED)** decision, the amendment governs. Plan: `.claude/state/plans/plan_managed_config.md`.

## Context

Corporate/platform teams want a baseline ocx config (mirrors, patch-registry pointer, default registry) distributed to every workstation/CI runner, refreshed centrally. Channel = OCI registry ocx already uses. Design = managed config tier: `[managed]` pointer seeded in `$OCX_HOME/config.toml`, resolving to plain config.toml payload published as OCI artifact, synced into local state, merged above user config every invocation. Sibling of `[mirrors]`/`[patches]` tiers and the infrastructure-patches feature — but simpler: exactly ONE artifact per host (no per-package discovery, no companions, no CAS GC anchoring).

## Decision Drivers

- **C1** Compose with existing `Config::merge` fold — lock invariants fall out for free
- **C2** Refresh never blocks a normal command; network fetch only on explicit action (`self setup --managed-config`, `config update`)
- **C3** `required=true` fails closed on *local disk state*, not network reachability
- **C4** CI ephemeral mode = no persistent state, no separate code path
- **C5** Seed lives in user-editable config.toml; write must not need TOML-edit dep nor risk other sections
- **C6** Remote payload can never redirect/loosen the tier that fetched it (one-hop)

## Decisions

### A — Loader integration (AMENDED)

- Opt 1 true two-pass (fetch inside loader): REJECTED — violates C2, loader runs every command.
- **Opt 2 CHOSEN + identity-gate amendment**: managed snapshot = 4th existence-checked discovery candidate, slot after `home_path()`, below `OCX_CONFIG`/`--config`. Zero network in loader. **Identity-gated merge**: before folding the snapshot, the loader (i) resolves the effective source locally (env `OCX_MANAGED_CONFIG` > the **local-only view's** `managed.source` — discovered tiers PLUS the `OCX_CONFIG`/`--config` overlay; amended post-Codex-gate 2026-07-05: resolving from tiers 1–3 only made an overlay-declared seed activate the required-gate without ever folding its payload, and let fold/gate disagree when base and overlay named different sources), (ii) reads the snapshot's embedded provenance, (iii) merges ONLY if provenance source == effective source (canonical `oci::Identifier` equality, normalized Display form incl. tag/digest). Merge ORDER is unchanged — base → payload → overlay — so explicit-tier values still beat payload values; only the identity/activation resolution consults the overlay. Mismatch/missing provenance → skip merge entirely (treated absent). Closes the poisoned-snapshot hole for `required=false` too — wrong-identity content never reaches `Config`, mirrors/registry/patches included.
- Path duplication avoided via pure associated fn `StateStore::managed_config_snapshot_path(ocx_home: &Path) -> PathBuf` shared by loader + store accessor (no loader→FileStructure dependency, no store reconstruction).
- Merge-time rule for this candidate only: strip `[managed]` from payload before `config.merge(parsed)` + WARN if present (Decision I).
- `OCX_NO_CONFIG=1` suppresses the candidate AND disables the `OCX_MANAGED_CONFIG` env-override read (hermetic means hermetic).

### B — OCX_MANAGED_CONFIG dual role

- Opt 1 two code paths (bootstrap vs runtime): REJECTED — drift risk.
- **Opt 2 CHOSEN**: one `resolve_managed_config`; env read first, overrides `config.managed.source` for this invocation only, never written. `self setup --managed-config` = only writer (flag > env > existing seed precedence, then persists). Fetch-on-demand at load NEVER acceptable (C2), CI included: CI recipe = `OCX_MANAGED_CONFIG=... && ocx config update && ocx <cmd>`. Runtime `OCX_MANAGED_CONFIG=""` = unset (matches `OCX_CONFIG=""` precedent).

### C — Seed write mechanism (AMENDED)

- Opt 1 `toml_edit` dep: REJECTED — deps bar; need = insert/replace one self-contained block.
- **Opt 2 CHOSEN + serialization amendment**: generalize `setup/rc_block.rs` fence machine (Fresh/Current/FormatUpgraded/Dirty, canonical/marker/actual SHA-256) — second genuine caller justifies parameterizing fence label. Fence body produced by **real TOML serialization of the typed `ManagedConfig` struct** (`project/mutate.rs:170` precedent) — never `format!` interpolation of the ref (Block-tier TOML-injection fix, CWE-74; ref additionally re-parsed as `oci::Identifier` before write).

```toml
# >>> ocx managed v1 a1b2c3d4 >>>
[managed]
source = "internal.company.com/ocx-config:user"
required = true
refresh = "notify"
interval = "1d"
# <<< ocx managed <<<
```

Dirty → exit 82 (existing DirtyRcBlock contract), `--force` overwrites. No `toml_edit` dep.

### D — Snapshot storage (AMENDED)

- Opt 1 CAS blobs + GC roots (patches model): REJECTED — patches need CAS for many per-package descriptors; managed config = singleton, wholesale-replaced.
- Opt 2 two files (content + provenance sidecar): SUPERSEDED by amendment — crash window between the two writes and concurrent-writer torn state (two racing `apply` ticks interleaving content/provenance from different fetches).
- **AMENDED CHOSEN**: **single file** `state/managed-config/snapshot.json` = `{source, digest, fetched_at, config: "<raw TOML string>"}` — one temp+rename = atomic. Loader reads one file, validates identity (A), parses embedded TOML, strips `[managed]`, merges. No CAS, no cross-process lock needed. Zip-and-move trivially portable; zero GC interaction.

### E — required enforcement + exit code

- Opt 1 inside loader: REJECTED — mixes semantic validation into mechanical loader (patch precedent validates at `Context::try_init`).
- **Opt 2 CHOSEN**: `resolve_managed_config(config, env_override, snapshot)` at `Context::try_init` (mirrors `resolve_patch_config`) for required/refresh policy; identity validation lives in the loader (A):
  1. effective source (env > seed > None→`Ok(None)`)
  2. parse Identifier (InvalidSource), interval (InvalidInterval); defaults required=true/notify/1d
  3. ref-match guard for policy purposes: provenance.source ≠ resolved source → treat snapshot absent (never apply snapshot under wrong identity — CI cache-poison defense)
  4. required + absent → `SnapshotRequired` → exit 78 ConfigError (not 81 PolicyBlocked: no op was refused; host never synced). Identical online/offline; remediation "run ocx config update"
  5. !required + absent → Ok, tier contributes nothing, throttle-gated stderr hint (benign-state rule — no per-invocation WARN)

### F — Refresh mechanics

- Opt 1 full fetch every tick: REJECTED — wasted bandwidth under notify.
- **Opt 2 CHOSEN**: background tick (skipped when `manual`) = throttled **digest-only** probe vs snapshot digest. No drift → touch marker. Drift+notify → stderr advisory "run `ocx config update`" + touch. Drift+apply → full fetch+persist+swap silently (touches only ocx-owned state). `ocx config update` = always full path, throttle bypassed (Duration::ZERO, mirrors self_update explicit-intent convention). Probe contract inherits update-check precedent explicitly: touch-on-success-AND-error (never on throttle short-circuit); `Ok(None)` = ref genuinely absent vs `Err` = network/auth (patch fetch split). Throttle primitive (`is_throttled`/`touch_state_atomic`) promoted from `update_check.rs` onto `StateStore` (2nd caller, justified DRY; start of #42 unified freshness story). Marker = separate zero-byte `.last-refresh-check`. Kill switch = new `OCX_NO_CONFIG_REFRESH` (NOT OCX_NO_UPDATE_CHECK — different concern, independently silenceable). Interval parser = ~10 lines `\d+[smhd]?` (bare = seconds), precedent: patches' flat glob matcher over `glob` crate.

### G — Lock semantics (AMENDED per user decision #2)

Free by construction for `[patches]`: system tier folds first, `PatchConfig::lock_as_system` set on accumulator, `PatchConfig::merge` ignores later tiers including managed payload (same fold). `ManagedConfig` gets own `system_locked` so system-scope `[managed] required=true` non-loosenable. **AMENDED**: `RegistryDefaults` + `MirrorConfig` gain the same `lock_as_system` pattern — system-scope `[registry]`/`[mirrors]` non-overridable by all higher tiers incl. managed payload. Independently valuable hardening; lands as prep refactor.

### H — Publish surface v1

- Opt 1 `ocx config publish` (mirrors patch publish).
- **Opt 2 CHOSEN (user-accepted)**: document `oras push --artifact-type application/vnd.sh.ocx.config.v1 <ref> config.toml:application/vnd.sh.ocx.config.v1+toml` recipe. No ocx-side validation value-add (consumer validates TOML on fetch); operator persona already runs OCI tooling. Additive v2 either way — not a one-way door.

### I — Remote payload validation

Strip `parsed.managed = None` before merge, WARN if was Some (misconfig or hostile redirect — operator-visible, non-fatal). All other sections merge normally; loosening structurally blocked by G.

### Mirror posture (AMENDED, supersedes "never mirror-rewritten"; user-confirmed)

Managed fetch IS mirror-rewritten, but its mirror map derives from **local tiers only** (system/user/home/OCX_CONFIG/--config/OCX_MIRRORS — managed payload excluded). Preserves the no-cycle intent (payload can never influence its own refresh route = can't self-brick or self-hijack) while keeping the fetch routable in air-gapped networks where all egress goes through locally-configured `[mirrors]` — the feature's primary target. Original blanket bypass borrowed a push-path precedent (`merge_platform_into_index`) and would make the fetch DOA behind corporate firewalls; `client.rs` structural test `canonical_reference_only_used_in_allowed_files` confirms reads route through the mirror seam. Mechanically: `ConfigLoader` returns local-only Config view alongside merged view; managed fetch uses a client built from the local-only mirror map.

### Setup ordering (AMENDED)

`self setup --managed-config` = resolve ref (flag>env>seed) → **synchronous fetch+persist FIRST → fence write only on success** (bootstrap hard-gate precedent, setup.rs:151 — a transient network blip during onboarding must not leave `required=true` fence with no snapshot, which would brick every subsequent command). Clearing via `""` removes fence AND deletes snapshot dir (no ghost tier); warns if `OCX_MANAGED_CONFIG` is still exported (env would re-activate the tier next command → SnapshotRequired surprise).

**Amendment (2026-08-06, #292):** a fence already `Current` no longer short-circuits the fetch. Setup (`self setup` and `config setup`, one shared `apply_managed_config`) now re-syncs an already-adopted seed on every run: fetch, then replace the snapshot only if the digest changed (`Refreshed{from,to}`), or confirm it unchanged (`AlreadyAdopted`, now verified rather than assumed). The re-sync is **best-effort iff an identity-matching snapshot is already on disk** — a fetch failure warns, keeps that snapshot, and still exits 0 (`RefreshUnavailable{digest,reason}`); first adoption and self-heal (fence `Current` but snapshot wiped or belongs to another source) have no fallback and keep the hard-fail fetch-first contract above unchanged — over-broad best-effort can never leave a `required=true` fence with no snapshot behind it. An in-force `ocx config update --pause` is honored: it holds the setup-time re-sync too, not just the background tick. `[managed].refresh` and `OCX_NO_CONFIG_REFRESH` are deliberately NOT consulted here — both stay scoped to the background tick only (Decision F); `--offline`/`OCX_OFFLINE` and a digest-pinned seed (content-addressed, cannot drift) are the levers that skip the setup-time re-sync instead.

### `--check` flag (user decision #1)

`ocx config update --check` mirrors `ocx self update --check` — probe-only: effective source (flag>env>seed), snapshot digest + fetched-at, refresh policy, active kill switches, live drift vs registry when reachable; never swaps; offline degrades to local-state report. Output via `context.api()` (Printable, JSON-capable).

### DRY (AMENDED)

Extract shared `Client::fetch_single_layer_artifact(identifier, artifact_type, layer_media_type, max_bytes)` — patch descriptor fetch = 1st caller (refactored onto it), managed config = 2nd. Stream-level byte cap threaded through (not just declared-size pre-check, CWE-400 — `patch/persistence.rs:209` pattern).

## Component contracts

**1. `config/managed.rs`** (sibling of patch.rs):
```rust
pub struct ManagedConfig { pub source: Option<String>, pub required: Option<bool>,
  pub refresh: Option<RefreshPolicy>, pub interval: Option<String>,
  #[serde(skip)] pub system_locked: bool }        // + Deserialize, JsonSchema, deny_unknown_fields
pub enum RefreshPolicy { Apply, Notify, Manual }   // snake_case serde
pub struct ResolvedManagedConfig { source: oci::Identifier, required: bool,
  refresh: RefreshPolicy, interval: Duration, system_required: bool }
pub enum ManagedConfigError { EmptySource, InvalidSource{..}, InvalidInterval{value},
  SnapshotRequired{source} }                       // thiserror, non_exhaustive → ExitCode::ConfigError
pub fn resolve_managed_config(config: &Config, env_override: Option<&str>,
  snapshot: Option<&ManagedConfigSnapshot>) -> Result<Option<ResolvedManagedConfig>, ManagedConfigError>;
```
`Config.managed: Option<ManagedConfig>` wired into `Config::merge` like `patches`.

**2. `managed_config.rs` + `managed_config/persistence.rs`**:
```rust
pub const MANAGED_CONFIG_ARTIFACT_TYPE: &str = "application/vnd.sh.ocx.config.v1";
pub const MANAGED_CONFIG_LAYER_MEDIA_TYPE: &str = "application/vnd.sh.ocx.config.v1+toml";
pub const MAX_MANAGED_CONFIG_BYTES: u64 = 64 * 1024;   // mirrors MAX_CONFIG_SIZE
pub struct FetchedManagedConfig { manifest_bytes, layer_bytes, manifest_digest, layer_digest }
// Decision D — amended 2026-07-09 (two-file): metadata `snapshot.json` = { source, tag, digest,
// fetched_at } + readable sibling `config.toml` holding the raw payload (the `config` field is
// #[serde(skip)]'d out of the JSON and repopulated from the sibling on read). Payload written first,
// metadata last; metadata-absent ⟹ whole snapshot reads absent. Supersedes the single-file shape.
pub struct ManagedConfigSnapshot { source: String, tag: Option<String>, digest: oci::Digest, fetched_at: String, /* #[serde(skip)] */ config: String }
pub async fn fetch_managed_config(client, identifier) -> Result<Option<FetchedManagedConfig>, FetchError>;
  // via shared fetch_single_layer_artifact on local-only-mirror client: manifest → artifactType
  // check → single layer → media-type check → declared-size cap → STREAM-level cap threaded
  // (patch/persistence.rs:209 pattern); Ok(None) = ref genuinely absent, Err = network/auth
pub async fn persist_managed_config(state, source, fetched) -> Result<ManagedConfigSnapshot, PersistError>;
  // pure: digest re-verify both → parse as Config → strip .managed (WARN) →
  // write config.toml (payload) then snapshot.json (metadata), each its own atomic temp+rename
  // (amended 2026-07-09; per-file torn-write-safe — cross-file pairing is best-effort, drift-sync heals)
```
Client surface: shared `fetch_single_layer_artifact(identifier, artifact_type, layer_media_type, max_bytes)` used by patch descriptors (refactored) + managed config; managed calls run on a client whose `MirrorMap` derives from the local-only Config view — normal `transport_reference` read seam, NO `canonical_reference` bypass.

**3. `StateStore`**: `managed_config_dir/snapshot_file/toml_file/refresh_marker()` accessors, pure `managed_config_snapshot_path(ocx_home)` + `managed_config_toml_path_for_snapshot(snapshot_path)` shared with loader, + promoted `is_throttled(path, interval)` / `touch(path)`.

**4. Loader**: `discover_paths()` 4th candidate; `load_and_merge` identity-gated one-hop strip branch for that path; no new loader error variant. Returns local-only Config view alongside merged view.

**5. `package_manager/tasks/managed_config.rs`**: `PackageManager::update_managed_config(&resolved) -> ManagedConfigUpdateResult {AlreadyCurrent, Updated{digest}, NotConfigured}`; `check_managed_config_refresh(&resolved)` (never fails caller).

**6. CLI**: `ConfigGroup::Update` (mirrors PatchGroup); `ConfigUpdateArgs::execute` via `context.manager()`, output via `context.api()`; `--check` probe mode with Printable status data. `self setup` phase 1.5: resolve ref (flag>env>seed) → synchronous fetch + persist snapshot FIRST → fence write only on success. `""` clears fence AND deletes snapshot dir; warns if `OCX_MANAGED_CONFIG` still exported. Background-tick hook = sibling of `app/update_check.rs` (env gate, is_ci, offline, TTY, skip-list skeleton).

**7. env**: `keys::OCX_MANAGED_CONFIG` (resolution-affecting → `OcxConfigView.managed_config_source` + `apply_ocx_config`, plain string like OCX_CONFIG — resolved [patches]/[mirrors] sub-payloads forward via their existing mechanisms), `keys::OCX_NO_CONFIG_REFRESH`. environment.md same commit.

## UX scenarios

| Scenario | Outcome |
|---|---|
| Workstation onboard (`self setup --managed-config <ref>`) | sync fetch+persist first; fence written on success only; next commands merge tier, zero latency. Fetch failure → no fence, no partial state |
| CI ephemeral (env + `ocx config update` first step) | no seed written anywhere; snapshot container-local; required satisfied |
| Offline warm | tier merges from snapshot; zero network; identical to online |
| Offline cold + required | SnapshotRequired exit 78, remediation named; same online |
| Dirty fence, no --force | exit 82; --force overwrites |
| Registry down during `config update` | Unavailable/AuthError; existing snapshot untouched (never partial-overwrite) |
| Digest pin | `ref@sha256:` parses via existing Identifier; byte-reproducible CI |
| notify fires | stderr advisory, marker touched, content NOT fetched, command unaffected |
| apply fires | silent swap; failure with valid cache = debug only |
| Re-setup different ref | fence rewritten; ref-match forces fresh sync fetch (re-adopt) |
| `config update --check` | probe-only report (source/digest/fetched-at/policy/kill-switches/drift); never swaps; offline = local-state report |

## Error taxonomy

| Error | Exit | Remediation |
|---|---|---|
| EmptySource / InvalidSource / InvalidInterval | 78 | fix seed / env var |
| SnapshotRequired | 78 | `ocx config update` (or self setup if never adopted); identical online/offline |
| SnapshotUnusable | 78 | identity-matching snapshot present, payload not parseable as `Config` — re-sync with `ocx config update`, or repair the published payload. Distinct from SnapshotRequired: "absent" would describe the wrong state (2026-07-31 amendment) |
| update fetch network/auth | 69 / 80 | standard registry remediation; snapshot untouched |
| resolved source's artifact absent in registry | 79 | fix the ref / publish the artifact (amended post-Codex-gate 2026-07-05: was silently mapped to a `NotConfigured` success + a panic path in setup) |
| digest mismatch on persist | 65 | tampered/corrupt registry bytes; re-run update |
| dirty fence | 82 | `--force` |

## Edge cases

1. **Snapshot ref-mismatch** (CI reusing cached OCX_HOME from other pipeline) → treated absent at loader merge; never applied under wrong identity. Most important defensive rule.
2. `[managed]` in payload → strip + WARN (redirect attempt visible).
3. Seed cleared (`""`) → fence removed AND snapshot dir deleted (no ghost tier).
4. required=false + registry gone → background debug, explicit update surfaces error, resolution proceeds empty.
5. System-scope `[managed] required=true` → not clearable via home-tier fence (`system_locked` fold), mirrors [patches].
6. Concurrent double-apply race → each of the two snapshot files is written via its own atomic temp+rename, so a racing writer never leaves a torn/byte-mixed file (Decision D, amended 2026-07-09 to the two-file layout). The cross-file pairing (metadata-of-A + payload-of-B) is best-effort; both are complete valid files and the next drift sync re-persists a consistent pair.

## One-Way-Door surfaces (complete list)

- `[managed]` seed schema
- `OCX_MANAGED_CONFIG` semantics
- Artifact media types + manifest shape (`application/vnd.sh.ocx.config.v1`(+toml), empty config blob, single layer, no index/subject) — falls under CLAUDE.md's "OCI manifest stays backward compatible" exception
- `snapshot.json` + `config.toml` two-file format = local/low-stakes (old = treated absent, re-synced)

## Security posture

Trust root = operator's registry (same as [mirrors]/[patches] today); unsigned-but-digest-verified v1 = deliberate deferral to `[trust.policy]` #98 / auto-verify #99 (both `[patches]` + `[managed]` become consumers when those land); blast radius of compromised config registry = fleet-wide [mirrors]/[patches]/[registry] control — mitigations: digest-pin bootstrap, registry push-protection, registry/mirror system locks (G). No downgrade monotonicity (any digest change accepted incl. rollback) — documented known limitation, consistent with all tag-based OCI. `state/` snapshot user-writable = same local trust level as config.toml itself (no new local-attacker surface). TOML nesting-depth parse bombs inherited from existing shared `toml::from_str` path (controlled abort, low). Media-type consts live in `managed_config.rs` (patch precedent: consts in `patch/descriptor.rs`, NOT `media_type.rs`).

## Acceptance criteria

Canonical list of 29 criteria lives in the plan: `.claude/state/plans/plan_managed_config.md` § "Acceptance criteria (29)". That section is the single source of truth for the Specify phase.

## Product positioning

Differentiator row sibling to #9: "Centrally managed corporate config as OCI artifact — mirrors/patches/registry policy refreshed from one operator-controlled source." Reinforces Principle #7 (Private-first). Lands with implementation, not speculatively.

## Amendment (2026-08-19) — `[trust.sigstore]` publish-time inlining

Two new obligations on the tier, from the self-hosted-Sigstore work
(`adr_offline_verify_trust_cache.md`, Amendment 2026-08-19). No change to the
wire shape, the seed fence, the snapshot format, or the precedence position.

**1. `ocx config push` inlines the trusted root.** When the payload's
`[trust.sigstore]` names a path-form `trusted_root`, `config push` reads that
file, validates it parses as a Sigstore trusted root, and publishes it as
`trusted_root_json`. This is a pre-publish transform on the TOML (via
`toml_edit`, so comments survive) — not a new layer, not a new media type. The
published payload therefore names no path on anyone's disk, which is what makes
it fleet-safe.

**2. Two guards on the consuming side.**

- A managed payload carrying `trusted_root_json` is honoured **only** when the
  `[managed] source` seed is digest-pinned. A tag-pinned seed means the trust
  root would arrive over the very channel it exists to verify; the circularity
  has to be broken by a pinned seed, not by policy. Otherwise the field is
  ignored with a warning.
- A path-form `trusted_root` arriving from the managed tier is always ignored
  with a warning — same posture as the existing one-hop `[managed]` strip: a
  fleet payload cannot name a path on someone else's machine.

The payload cap is unchanged. `MAX_MANAGED_CONFIG_BYTES` is 64 KiB and a
trusted root measures ~2 KB (`test/sigstore/trusted_root.json`), leaving ~60 KiB
of headroom — the raise the plan proposed was unnecessary and was not made.

## Amendment (2026-08-23) — `[registries.<name>].insecure` makes the managed tier a transport authority

Closes the gap `[registries.<name>].insecure` (`fix/oci-cross-host-upload-auth-272`) opened in
Decision I's coverage claim. Where this conflicts with earlier text, this amendment governs.

**What changed.** Decision I says every section outside `[managed]` "merge[s] normally; loosening
structurally blocked by G." Decision G's enumeration (`[patches]`, `[managed]`, `RegistryDefaults`,
`MirrorConfig`) never included `[registries.<name>]` — that table carries its own per-entry
`system_locked`, not one of G's four whole-table locks, and a per-entry lock only fires for a name
the system tier has already written. `insecure` (`crates/ocx_lib/src/config/registry.rs`) is the
first field on that table where the *absence* of a system-tier entry is itself the loosening: a
managed payload can declare `[registries."<any-host>"] insecure = true` for a host the system tier
never named, and there is no entry yet for a lock to attach to. Decision I's clause held for every
table G actually covers; it was never true for `[registries.<name>]`, and `insecure` is what turns
that gap from theoretical into exploitable — managed-tier publish access now reaches index, mirror,
and ordinary registry transport for any host, fleet-wide, on the next `ocx config update`.

**Decision: keep the union; do not carve the managed tier out of `insecure`.**

- *Considered and rejected* — deny `[registries.<name>].insecure` from the managed tier
  specifically. `system_locked` is the only tier-restriction primitive this config tree has, and it
  restricts a lower tier *against a value the higher tier already set*, never *a specific key
  regardless of whether the higher tier said anything*. Building the latter is new config-system
  surface for one field. It also costs the tier's own target user: an air-gapped fleet whose
  registry is genuinely plain HTTP is exactly who `[managed]` exists to serve, and forcing that
  fleet to provision `OCX_INSECURE_REGISTRIES` on every host to route around a restriction the tier
  itself would impose defeats the point of centralizing the config.
- *Chosen* — keep `insecure` in the union domain, and give the system tier a subtractive lever.
  The property worth keeping is the one `insecure_hosts`'s own module doc already states: no
  source can take a host back out of the set, so there is no four-way truth table
  (unset/true/false × env-present/absent) for a reader to resolve. Widening that from zero
  exceptions to exactly one — an explicit system-scope `insecure = false`, which also subtracts
  from `OCX_INSECURE_REGISTRIES` — preserves order-independence (a lock means "this source is
  final," not "last tier wins") while giving the platform engineer the only lever
  `[registries.<name>]` needs: pre-declare the hosts that must never go plaintext.

**Residual exposure — recorded, not softened.** The lever protects only a host the system tier
thought to name. A managed-config publisher — compromised or malicious — can still self-authorize
plaintext transport for any host the system tier has not already locked to `insecure = false`, and
that authorization reaches index, mirror, and ordinary registry traffic identically
(`crates/ocx_lib/src/config/insecure.rs`). This is a deliberate acceptance, not an oversight:
`[managed]` is sold on fleet-wide policy distribution (`product-context.md` differentiator #11),
and denying it this key outright would cost every legitimately-plaintext fleet the provisioning
step this tier exists to remove. An operator who wants the hard guarantee enumerates the hosts and
locks them at system scope; an operator who does not is trusting the managed-config registry the
same way they already trust it for `[mirrors]`/`[patches]`/`[registry]`.

**Scope note — the fetch client is a separate decision.** Whether a managed payload's own
`insecure` entries may license plaintext for *fetching the next managed-config snapshot* is
governed by the mirror-posture decision — `build_managed_config_client` in
`crates/ocx_cli/src/app/context.rs`, cited in code as `adr_managed_config_tier.md`, "Mirror
posture (AMENDED)" — and its C6 driver — the payload must never be able to redirect the tier
that fetched it, one-hop. That invariant is unchanged by this amendment: the managed-config
fetch client's transport allowance must still derive from the local-only view, never the
merged view `insecure` now travels on for everything else.

## Links

- Plan: `.claude/state/plans/plan_managed_config.md`
- Meta-plan: `.claude/state/plans/meta-plan_managed_config.md`
- Research: `.claude/artifacts/research_oci_config_artifact.md`
- Siblings/precedents: `adr_infrastructure_patches.md` (simplified-away sibling), `adr_self_setup.md` (fence machine), `adr_ci_env_export_flag.md` (env/flag dual-role precedent), `config/patch.rs` + `mirror.rs` (structural template), `website/src/docs/reference/environment.md` (same-commit rule)
- GitHub: #42 (unified freshness/TTL), #150 (setup digest binding), #159 (remote_client audit), #98/#99 (trust policy / auto-verify deferrals)
