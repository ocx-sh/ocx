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
>   `config.toml`, `state/managed-config/snapshot.json`, and `pause.json` are ordinary local
>   state at the same trust level as any other file such an attacker could already edit; the
>   tier's digest pins bind fetch-time content, not load-time tamper-evidence.
>
> Plan: `.claude/state/plans/plan_managed_config_v2.md`. Where any text below conflicts
> with this amendment, the amendment governs.

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
// Decision D (single-file): snapshot.json = { source, digest, fetched_at, config: "<raw TOML>" }
pub struct ManagedConfigSnapshot { source: String, digest: oci::Digest, fetched_at: String, config: String }
pub async fn fetch_managed_config(client, identifier) -> Result<Option<FetchedManagedConfig>, FetchError>;
  // via shared fetch_single_layer_artifact on local-only-mirror client: manifest → artifactType
  // check → single layer → media-type check → declared-size cap → STREAM-level cap threaded
  // (patch/persistence.rs:209 pattern); Ok(None) = ref genuinely absent, Err = network/auth
pub async fn persist_managed_config(state, source, fetched) -> Result<ManagedConfigSnapshot, PersistError>;
  // pure: digest re-verify both → parse as Config → strip .managed (WARN) →
  // ONE atomic temp+rename write of snapshot.json (kills crash window + concurrent-writer race)
```
Client surface: shared `fetch_single_layer_artifact(identifier, artifact_type, layer_media_type, max_bytes)` used by patch descriptors (refactored) + managed config; managed calls run on a client whose `MirrorMap` derives from the local-only Config view — normal `transport_reference` read seam, NO `canonical_reference` bypass.

**3. `StateStore`**: `managed_config_dir/snapshot_file/refresh_marker()` accessors, pure `managed_config_snapshot_path(ocx_home)` shared with loader, + promoted `is_throttled(path, interval)` / `touch(path)`.

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
6. Concurrent double-apply race → single-file snapshot keeps content+identity consistent (Decision D amendment).

## One-Way-Door surfaces (complete list)

- `[managed]` seed schema
- `OCX_MANAGED_CONFIG` semantics
- Artifact media types + manifest shape (`application/vnd.sh.ocx.config.v1`(+toml), empty config blob, single layer, no index/subject) — falls under CLAUDE.md's "OCI manifest stays backward compatible" exception
- `snapshot.json` format = local/low-stakes (old = treated absent, re-synced)

## Security posture

Trust root = operator's registry (same as [mirrors]/[patches] today); unsigned-but-digest-verified v1 = deliberate deferral to `[trust.policy]` #98 / auto-verify #99 (both `[patches]` + `[managed]` become consumers when those land); blast radius of compromised config registry = fleet-wide [mirrors]/[patches]/[registry] control — mitigations: digest-pin bootstrap, registry push-protection, registry/mirror system locks (G). No downgrade monotonicity (any digest change accepted incl. rollback) — documented known limitation, consistent with all tag-based OCI. `state/` snapshot user-writable = same local trust level as config.toml itself (no new local-attacker surface). TOML nesting-depth parse bombs inherited from existing shared `toml::from_str` path (controlled abort, low). Media-type consts live in `managed_config.rs` (patch precedent: consts in `patch/descriptor.rs`, NOT `media_type.rs`).

## Acceptance criteria

Canonical list of 29 criteria lives in the plan: `.claude/state/plans/plan_managed_config.md` § "Acceptance criteria (29)". That section is the single source of truth for the Specify phase.

## Product positioning

Differentiator row sibling to #9: "Centrally managed corporate config as OCI artifact — mirrors/patches/registry policy refreshed from one operator-controlled source." Reinforces Principle #7 (Private-first). Lands with implementation, not speculatively.

## Links

- Plan: `.claude/state/plans/plan_managed_config.md`
- Meta-plan: `.claude/state/plans/meta-plan_managed_config.md`
- Research: `.claude/artifacts/research_oci_config_artifact.md`
- Siblings/precedents: `adr_infrastructure_patches.md` (simplified-away sibling), `adr_self_setup.md` (fence machine), `adr_ci_env_export_flag.md` (env/flag dual-role precedent), `config/patch.rs` + `mirror.rs` (structural template), `website/src/docs/reference/environment.md` (same-commit rule)
- GitHub: #42 (unified freshness/TTL), #150 (setup digest binding), #159 (remote_client audit), #98/#99 (trust policy / auto-verify deferrals)
