# Design Spec — Whole-File Lock Model: Pin-Preserving Mutators, Migration-by-Read

Status: Approved (revised model — replaces the per-group / partial-upgrade design)
Scope: per-platform lock pinning V2 (#142), branch `goat`
Relates to: `.claude/artifacts/adr_per_platform_lock_pinning.md` (Decision 2, Codex-R2, Codex-H1)
Authoritative subsystem rules: `subsystem-package-manager.md`, `subsystem-cli-commands.md`,
`quality-rust-errors.md`, `quality-rust-exit_codes.md`, `quality-cli-help.md`

---

## 0. The model in one sentence

`add` / `remove` carry every binding they did not name **forward verbatim** (V2 byte-identical;
V1 by reading its **pinned, immutable index digest** and transcribing its leaves — never a
live-tag re-resolution); `ocx lock` is a whole-file reconcile (idempotent when clean, full
re-resolve of all declared tags when dirty); `ocx upgrade` is the single whole-file "advance every
version" verb; and groups are a *composition* concern only, never a lock/upgrade scope.

---

## 1. The headline rationale — migration ≠ re-resolution (the decisive insight)

The entire simplification rests on one distinction the previous design conflated:

> **V1 → V2 migration is reading a PINNED, content-addressed index digest and extracting its
> per-platform leaf manifests. That is deterministic and reproducible — it cannot drift. Only
> LIVE-TAG re-resolution (looking up what `:tag` points at *now*) drifts.**

A V1 lock entry stores `registry/repo@sha256:<index-digest>` (`LockedResolution::LegacyIndex`,
`lock.rs:200-219`). That digest is immutable: fetching its children (`index.fetch_candidates` on the
digest-addressed index, `resolve.rs:1023-1028`) yields the **exact same leaves** every time the index
manifest is retrievable. So transcribing V1 → V2 by reading the pinned digest preserves the pin
exactly — it is *free and safe to do on any write*. The only thing that can change a pin is
re-resolving the declared advisory tag (`:latest`, `:3`) against the registry to see where it points
*today* (`resolve_lock` → `resolve_to_platforms`, `resolve.rs:82-107`, `434-444`).

Consequences that drive every decision below:

- **Migration on any mutating write is acceptable** because it is pin-preserving by construction
  (`transcribe_v1_to_v2(exact_only=true)`, `resolve.rs:1007-1079`). `add`/`remove` migrate carried
  V1 entries silently and safely. There is no separate "migrate" verb needed for the common path.
- **Re-resolution must be explicit** because it is the only operation that moves a pin. It happens in
  exactly two whole-file commands the user invokes on purpose: `ocx lock` (reconcile, may advance
  moving tags as a deliberate whole-file op) and `ocx upgrade` (forced advance).
- **`ocx lock --upgrade` is deleted.** Its job — "migrate the format without moving pins" — is now an
  automatic side effect of any write. The remaining `exact_only=false` re-resolve fallback (V1 index
  GC'd → re-resolve the declared tag) is exactly the live-tag re-resolution we forbid on mutators;
  on `add`/`remove` it surfaces as a fail-closed error instead (§4).

Codex-H1 (anti-laundering) and Codex-R2 (anti-drift) survive **structurally**, not by guard, because
the offending path — a `resolve_lock(&[])` full live re-resolve reachable from `add`/`remove` — is
deleted (§7).

---

## 2. Behavior contract per command

Groups (`[group.ci]`) remain **only** a composition concern: `add -g ci` chooses which table to add
to; `run`/`env` compose a subset. Groups are **never** a lock or upgrade scope — the `--group`/`-g`
flag and any positional PKGS scoping are removed from `lock` and `upgrade`.

| Command | Re-resolves (live tag) | Carries forward verbatim | Errors when |
|---|---|---|---|
| `add X` | only `X` (the new binding) | every pre-existing binding (V2 byte-identical; V1 via pinned-index transcribe, exact-only) | (a) `ocx.toml` drifted from lock **before** this add (pre-mutation hash ≠ lock hash) → `StaleLockOnPartial` (65), remedy "run `ocx lock`"; (b) a carried entry's pinned V1 index is GONE → `LockUpgradeRequired` (78), remedy "run `ocx upgrade`"; (c) `X` unresolvable → `TagNotFound` (79) / `AuthFailure` (80) / `RegistryUnreachable` (69) / `PolicyBlocked` (81) |
| `remove X` | **nothing** | every survivor (same pinned-index transcription for V1) | (a) lock drifted on a *survivor* before this remove → 65 ("run `ocx lock`"); (b) a survivor's pinned V1 index is GONE → 78 ("run `ocx upgrade`") |
| `lock` | every declared tag **iff dirty**; **nothing** iff clean (byte-identical rewrite) | n/a (whole-file) | tag unresolvable (79/80/69/81); offline/frozen + uncached on a dirty re-resolve → `PolicyBlocked` (81) |
| `upgrade` | **every** declared tag, always (even when clean) | n/a (whole-file) | tag unresolvable (79/80/69/81); offline/frozen + uncached → `PolicyBlocked` (81) |
| `run` / `env` / `pull` | nothing (read-only; read BOTH V1 and V2) | everything (no write-behavior change to `pull`) | only when pinned content is unavailable: V1 index orphaned → cannot select host platform; V2 no host leaf → `NoHostLeaf` (78). Stale lock vs config at load → `StaleLock` (65) via the read-side gate |

Notes:

- **`ocx lock` on a dirty `ocx.toml` may advance a moving tag.** This is acceptable and intended: it
  is a *whole-file*, user-invoked reconcile, not a silent incidental bump on an unrelated mutation.
  When clean, `lock` is a no-op (the declaration_hash already matches; the rewrite is byte-identical,
  `lock.rs` → `resolve_lock` → `build_lock` re-stamps the same hash, `save` preserves `generated_at`
  via `tools_content_equal`, `lock.rs:480-518`).
- **`ocx upgrade` is the bump verb.** Bare whole-file `resolve_lock(staged.config(), index, &[], …)`.
  No subset, no carry-forward, nothing untouched — laundering/drift impossible by construction.
- **`run`/`env`/`pull` are unchanged.** A healthy V1 lock needs no upgrade to be *read*; the read
  path already branches on `LockedResolution` (`lock.rs:200-219`). There is no `--upgrade` migration
  step a user must run before reading.

---

## 3. The two-hash freshness gate (retained for `add`/`remove` only)

`add` and `remove` are the only commands that carry forward an untouched set, so they are the only
commands needing the freshness gate. Two distinct config hashes are in flight and must never be
conflated — passing one config to both roles either fails **every** clean `add` or trips the
commit-time coherence gate.

| Hash | Source | Role |
|---|---|---|
| **Pre-mutation hash** | `hash(guard.config())` — the `ocx.toml` snapshot taken **before** this command's staged edit (`mutation.rs:165-167`) | **Freshness anchor.** Compared vs `previous.metadata.declaration_hash` (`lock.rs:146-164`). Answers "was the lock current with everything *other than my edit*?" |
| **Candidate hash** | `hash(staged.config())` — the **post-mutation** config | **Stamped into the produced lock's metadata** via `build_lock(tools, candidate)` (`resolve.rs:849-865`), so the commit-time coherence gate (`mutation.rs:263-279`, compares `staged.candidate` hash vs `new_lock.metadata.declaration_hash`) passes. |

Why they must differ for `add`: candidate ≠ pre-mutation (a binding was inserted), so anchoring
freshness on the candidate (today's `resolve_lock_partial` single-hash gate, `resolve.rs:200-210`)
**always** mismatches and fails closed on a clean add. Anchor freshness on the **pre-mutation** hash
(unchanged by our own edit) while still stamping the **candidate** hash into the lock.

For `remove`, candidate has the removed binding absent; the pre-mutation snapshot still contains it.
Anchoring on the pre-mutation hash means a clean lock (matching the unmodified `ocx.toml`) passes the
gate, and the carry-forward filter (still-declared-in-`candidate`) drops the removed binding.

### 3.1 New entry point — `resolve_lock_touched`

`FreshnessMode`/`SkipGate` is **not needed** (the only `SkipGate` consumer in the old design was
`lock -g`, which is gone). The signature is a plain two-config + explicit-touched-set contract:

```text
pub async fn resolve_lock_touched(
    candidate: &ProjectConfig,      // post-mutation; drives new-binding resolve + stamped metadata hash
    pre_mutation: &ProjectConfig,   // pre-mutation snapshot; freshness anchor ONLY
    previous: &ProjectLock,
    index: &Index,
    touched: &[(String, String)],   // explicit (group,name) set to (re)resolve; EMPTY = resolve nothing
    options: ResolveLockOptions,
) -> Result<ProjectLock, super::Error>
```

Contract:

1. **Freshness gate (always on).** If `hash(pre_mutation) != previous.metadata.declaration_hash`
   → `Err(StaleLockOnPartial { previous_hash, current_hash })` **before any resolve** (terminal at
   the caller, exit 65). `current_hash` is `hash(pre_mutation)`, `previous_hash` is the lock's.
2. **Resolve exactly the `touched` `(group, name)` pairs.** Empty set ⇒ resolve nothing (remove);
   single new binding ⇒ resolve just it (add). This is the explicit-touched-set contract — it does
   **not** fall back to "resolve everything" when the set is empty (the `collect_work` whole-set
   behavior, `resolve.rs:331-356`).
3. **Carry forward** every predecessor entry whose `(group, name)` is **not** in `touched` AND is
   still declared in `candidate` (so remove's dropped binding falls out): V2 byte-identical;
   V1 exact-only transcribe via `transcribe_v1_to_v2(exact_only=true)` (`resolve.rs:1007-1079`),
   which fails `LockUpgradeRequired` (78) on a gone index — never a silent re-resolve.
4. **Stamp `hash(candidate)`** into the produced metadata via `build_lock(tools, candidate)`.

### 3.2 DRY extraction — `merge_carry_forward`

`resolve_lock_touched` and `resolve_lock_partial` share the carry-forward + exact-only-transcribe
merge (`resolve.rs:241-284`). Extract a private
`merge_carry_forward(previous, resolved, candidate, index, options)` and have both call it (a genuine
2-caller extraction per `quality-core.md` DRY). `resolve_lock_partial` keeps its single
candidate-hash gate **only if it still has a caller** — but with `upgrade X` and `lock -g` gone,
`resolve_lock_partial` has **no remaining production caller** (§4.6, §4.7). Decision: **delete
`resolve_lock_partial`** and its tests once `merge_carry_forward` and `resolve_lock_touched` exist;
keep `merge_carry_forward` as the single carry-forward home. (If a future feature needs subset
re-resolution it can reintroduce a caller; YAGNI says delete now.)

---

## 4. Exact code changes per file

### 4.1 `error.rs` — reuse `StaleLockOnPartial` (65) + `LockUpgradeRequired` (78), update message text

Two fail-closed modes for `add`/`remove`:

1. **Lock out of sync with `ocx.toml`** (drift on the pre-mutation snapshot) → reuse
   `StaleLockOnPartial` (`error.rs:309-315`, classified `DataError`/65 at `error.rs:378`), made
   **terminal** (callers stop catching it).
2. **Carried/survivor V1 index gone** → reuse the already-terminal `LockUpgradeRequired`
   (`error.rs:91-97`, classified `ConfigError`/78 at `error.rs:362`).

No new variant, no new exit code (`quality-core.md` YAGNI). Update two `#[error(...)]` strings to
lowercase/no-period (`quality-rust-errors.md`), drop the internal `resolve_lock` hint, and name the
**user** remedy. Per memory `feedback_never_pin_build_timestamp` and the decided model, the carried-
V1-gone remedy now points at **`ocx upgrade`** (the bump verb that re-resolves), not the deleted
`ocx lock --upgrade`:

- `StaleLockOnPartial` (was `error.rs:309-311`):
  `"lock is out of sync with ocx.toml (declaration_hash {current_hash} != locked {previous_hash}); run \`ocx lock\` to reconcile"`
- `LockUpgradeRequired` (was `error.rs:96`):
  `"tool '{name}': locked entry can no longer be migrated exactly; run \`ocx upgrade\` to re-resolve"`

Tier-neutral phrasing for `--global` (the error layer has no tier context): the message says
"run `lock` (add `--global` for the global toolchain)" rather than emitting a wrong copy-pasteable
`ocx lock`. Add a `--global` add/remove fail-closed test asserting the message does not hard-code a
project-only command.

### 4.2 `resolve.rs`

- **Add** `resolve_lock_touched` (§3.1) + private `merge_carry_forward` (§3.2). No `FreshnessMode`
  enum.
- **Delete** `resolve_lock_partial` (`resolve.rs:180-285`) — no remaining caller after §4.6/§4.7;
  fold its carry-forward into `merge_carry_forward`.
- **Delete** `ensure_untouched_v1_transcribable` (`resolve.rs:887-910`) and `touched_bindings`
  (`resolve.rs:929-944`) **only if** no caller remains. After the rewrites, `add`/`remove` call
  `resolve_lock_touched` (whose carry-forward already does exact-only transcription), so the
  standalone guard is redundant. `touched_bindings` is still useful to expand the touched
  `(group,name)` set for `add` — keep it if `add` uses it; otherwise inline a single-pair literal.
  Decision: `add` touches exactly one new binding in exactly one group, so build the touched literal
  inline `[(group_of(X), binding_name(X))]` and **delete** `touched_bindings`; `remove` touches the
  empty set. This removes both helpers (single carry-forward enforcement point, `quality-core.md`
  KISS).
- **Keep** `transcribe_v1_to_v2`, `resolve_lock`, `upgrade_lock` (still exported but unused by CLI —
  see §4.7 note), `build_lock`, `collect_work`, `declared_identifier`, `lookup_host_leaf` unchanged.

### 4.3 `add.rs` (`add.rs:99-166`)

Replace the `resolve_lock_partial → catch StaleLockOnPartial → ensure_untouched_v1_transcribable →
resolve_lock(&[])` block (`add.rs:108-166`) with:

```text
let touched = [(group_of(X), binding_name(X))];   // the single new (group,name)
let new_lock = match guard.previous_lock().cloned() {
    Some(prev) => resolve_lock_touched(
        staged.config(),          // candidate
        guard.config(),           // pre-mutation snapshot — freshness anchor
        &prev, context.default_index(), &touched,
        ResolveLockOptions::default(),
    ).await?,                      // propagate StaleLockOnPartial(65) / LockUpgradeRequired(78)
    None => resolve_lock(staged.config(), context.default_index(), &[], ResolveLockOptions::default()).await?,
};
```

- `binding_name(X)` uses `ocx_lib::project::binding_key(&identifier)` (`mutate.rs:83`), already
  imported at `add.rs:108`. `group_of(X)` is `self.group` (default group when absent).
- **Bootstrap (no predecessor) keeps the direct `resolve_lock`** — nothing to preserve, nothing to
  launder; it must never fail closed. Empty-tools predecessor (`lock_version=2, tools=[]`) goes
  through `resolve_lock_touched` with an empty carry set and resolves only X — also green.
- Delete the `StaleLockOnPartial` match arm, the `ensure_untouched_v1_transcribable` call, the
  `touched_bindings` call (`add.rs:116`), and the stale doc-comment paragraph at `add.rs:99-107`
  ("try the partial resolve first … fall back to a full `resolve_lock`").

### 4.4 `remove.rs` (the established remove contract changes)

Today `remove` calls `resolve_lock(staged.config(), &[], …)` **unconditionally** (`remove.rs:146-152`),
re-resolving every survivor's live tag (a silent bump), guarded only by a now-redundant
`ensure_untouched_v1_transcribable` pre-pass (`remove.rs:129-141`). Net-new wiring:

```text
let new_lock = match guard.previous_lock().cloned() {
    Some(prev) => resolve_lock_touched(
        staged.config(),          // candidate (removed binding already absent)
        guard.config(),           // pre-mutation snapshot (still contains removed binding)
        &prev, context.default_index(), &[],   // EMPTY touched set ⇒ resolve nothing
        ResolveLockOptions::default(),
    ).await?,
    None => resolve_lock(staged.config(), context.default_index(), &[], ResolveLockOptions::default()).await?,
};
```

The removed binding is absent from `candidate`, so the "still declared in `candidate`" filter drops
it from carry-forward. On a clean lock, `hash(pre_mutation)` (which still contains the removed
binding) equals the predecessor hash → proceed and carry all survivors verbatim.

**Behavior change (intended, contract change):** a survivor whose upstream tag moved keeps its old
pin on `remove` (previously silently bumped). Delete the `ensure_untouched_v1_transcribable` pre-pass
(`remove.rs:116-141`) and the `touched_bindings` call (`remove.rs:131-132`). Enumerate and flip every
acceptance test that asserts a re-resolved survivor digest on `remove` (test plan §8.2).

### 4.5 `upgrade.rs`

`ocx upgrade` becomes a **bare whole-file resolve** — delete the partial branch entirely.

- Delete the `--group`/`-g` flag (`upgrade.rs:38-44`) and the positional `packages` arg
  (`upgrade.rs:73-79`). `Upgrade` keeps `--check`, `--pull`/`--no-pull`.
- Delete `is_partial_op` (`upgrade.rs:82-94`), the group/binding validation loops
  (`upgrade.rs:97-130`), and the entire partial branch (`upgrade.rs:140-213`) including the
  `resolve_lock_partial → catch → ensure_untouched_v1_transcribable → resolve_lock(&[])` fallback.
- `execute` collapses to: stage lock-only (`upgrade.rs:132-134`), then unconditionally
  `resolve_lock(staged.config(), resolve_index, &[], ResolveLockOptions::default())`
  (the existing bare-upgrade arm, `upgrade.rs:138-139`).
- `--check` keeps the same shape but compares the whole-file candidate against `prev` via
  `lock_content_matches` (`upgrade.rs:272-289`), unchanged. With no subset, `--check` no longer has a
  "no predecessor" branch tied to a subset — keep the existing "no predecessor lock → exit 78"
  guard (`upgrade.rs:223-232`).
- Delete the three `is_partial_op`-guard unit tests (`upgrade.rs:362-393`) and the
  `ProjectErrorKind`/`resolve_lock_partial` imports (`upgrade.rs:8-12`) that become unused. Keep
  `bare_upgrade`-shaped behavior as the only behavior; replace `bare_upgrade_is_not_partial` with a
  parser test asserting `--group`/positional args are rejected (clap unknown-arg → exit 64).

### 4.6 `lock.rs`

`ocx lock` becomes a whole-file reconcile with **no `--group` and no `--upgrade`**.

- Delete the `--group`/`-g` flag (`lock.rs:37-38`), its empty-segment pre-validation
  (`lock.rs:87-94`), the snapshot group validation (`lock.rs:114-122`), and the `&self.groups`
  argument to `resolve_lock` (`lock.rs:149-156`) — call `resolve_lock(staged.config(),
  context.default_index(), &[], ResolveLockOptions::default())`.
- Delete the `--upgrade` flag (`lock.rs:69-82`) and the whole `if self.upgrade { upgrade_lock(...) }`
  branch (`lock.rs:135-148`). `upgrade_lock` (`resolve.rs:958-987`) loses its CLI caller; keep the
  library function (cheap, still correct) **or** delete it — decision: **delete `upgrade_lock`** (no
  caller; YAGNI). Its only purpose was the deleted `lock --upgrade`; the pin-preserving transcription
  it wrapped now lives inside `merge_carry_forward` for the carry-forward path.
- `--check` (`lock.rs:96-106`, `run_check`) unchanged — read-only hash compare, exit 0/65/78.
- Whole-file reconcile semantics fall out of the existing `resolve_lock` + `save` path: clean lock →
  `tools_content_equal` true → `generated_at` preserved → byte-identical rewrite (`lock.rs:480-518`);
  dirty lock → full re-resolve of all declared tags.

### 4.7 `pull.rs` — read-only, unchanged

`pull` is read-only (loads `ocx.lock`, materializes; no resolve call). No write-behavior change.
A healthy V1 lock reads fine. Out of scope: the pre-existing V1-lock direnv-no-refire wart (flag as a
follow-up issue, do not scope-creep).

### 4.8 Deleted/changed-site summary

| Site | Before | After |
|---|---|---|
| `add.rs:108-166` | partial → catch 65 → guard → `resolve_lock(&[])` | `resolve_lock_touched(touched={X}, pre=guard.config, cand=staged.config)`; propagate |
| `remove.rs:116-152` | guard pre-pass + unconditional `resolve_lock(&[])` over survivors | `resolve_lock_touched(touched=∅, pre=guard.config, cand=staged.config)`; propagate |
| `upgrade.rs:38-213` | `-g`/PKGS flags + partial branch + fallback | `--group`/PKGS deleted; bare `resolve_lock(&[])` only |
| `lock.rs:37-82,114-156` | `-g` filter + `--upgrade` (`upgrade_lock`) | `-g`/`--upgrade` deleted; bare whole-file `resolve_lock(&[])` |
| `resolve.rs:180-285,887-944,958-987` | `resolve_lock_partial`, `ensure_untouched_v1_transcribable`, `touched_bindings`, `upgrade_lock` | deleted; `merge_carry_forward` + `resolve_lock_touched` added |

---

## 5. V2 lock-format / metadata changes — NONE

Entirely orchestration-layer. `LockMetadata` (declaration_hash + versions) and
`LockedResolution::{PerPlatform, LegacyIndex}` untouched (`lock.rs:144-219`). Read-both-V1/V2,
write-V2-only preserved: every write goes through `build_lock` → `LockVersion::V2`; carry-forward
transcribes V1 → V2 exact-only; the serializer panics on `LegacyIndex` (`lock.rs:663-665`).
`declaration_hash` algorithm + `DECLARATION_HASH_VERSION` unchanged; no per-binding fingerprint
added. **No per-binding lock state.**

---

## 6. What this eliminates (explicit)

- **Block A (untouched-V2 drift on partial upgrade)** — gone: no partial upgrade exists.
- **The `ocx lock -g` data-loss bug** (`lock -g ci` writing a lock containing only `ci`, dropping
  `[tools]`) — gone: the `-g` flag is removed; `lock` is whole-file.
- **The edit-then-`upgrade X` fail-closed friction** (Option-A/B per-binding-hash discussion) — moot:
  there is no `upgrade X`. `ocx upgrade` is whole-file, so a hand-edited tag is simply re-resolved
  along with everything else.
- **`FreshnessMode`/`SkipGate` machinery** — never built; the only `SkipGate` consumer (`lock -g`)
  does not exist.
- **`ocx lock --upgrade` as a distinct verb** — folded into automatic carry-forward migration.

---

## 7. Codex-R2 + Codex-H1 hold structurally

**Codex-H1 (anti-laundering).** `add`/`remove`: the pre-mutation-hash freshness gate runs *before*
any carry-forward; on drift it fails closed (65). On a match, the predecessor genuinely described the
pre-mutation config, so carrying its untouched entries forward and stamping the candidate hash is
honest (candidate differs from pre-mutation only by the touched binding, resolved fresh). No
untouched entry is revalidated under a hash it never matched — identical for V1 and V2 (the gate is
content-agnostic). The commit-time coherence gate (`mutation.rs:263-279`) remains as defense-in-depth
(a hash-inversion bug fails loudly, not silently). `upgrade`/`lock`: whole-file — there is no
untouched set, so nothing to launder.

**Codex-R2 (anti-drift).** The killing mechanism is that **no `resolve_lock(&[])` is reachable from
`add`/`remove`** — the only path that re-resolved untouched live tags. After deletion, untouched
entries are reached only via `merge_carry_forward`: V2 → byte-identical pass-through (zero registry
contact); V1 → exact-only transcription of the **pinned index digest** (same leaves if retrievable;
`LockUpgradeRequired` if gone — never a silent live re-resolve). The "deeper R2" V2-drift caveat
(unselected-group V2 silently re-resolving under the old full fallback) is eliminated by construction:
no full fallback exists on a mutator.

Both hold because the offending path is **absent**, not guarded — the whole point of the model.

---

## 8. Test plan

### 8.1 Rust unit tests (`resolve.rs`) — write FIRST; the index fake panics on any unexpected resolve

- `resolve_lock_touched_resolves_only_touched_carries_rest_v2` — touched = {one} of 3 V2 entries;
  assert only that entry changed, the other two byte-identical; **stamped metadata hash ==
  hash(candidate), NOT hash(pre-mutation)** (locks in the two-hash contract).
- `resolve_lock_touched_empty_touched_carries_everything` — remove's case: touched = ∅; every
  survivor carried verbatim, zero registry contact (panic-on-resolve fake never fires).
- `resolve_lock_touched_add_on_clean_lock_succeeds` — candidate differs from pre-mutation by the new
  binding; pre-mutation == prev hash; resolve only the new binding; assert produced metadata hash ==
  hash(candidate) (proves no universal add-failure, no commit-gate rejection).
- `resolve_lock_touched_rejects_drifted_pre_mutation_hash` → `StaleLockOnPartial` before any resolve.
- `resolve_lock_touched_untouched_v1_gone_fails_lock_upgrade_required` → 78, never re-resolved.
- `resolve_lock_touched_untouched_v1_present_transcribes_exact` → pin-preserving V2 (pinned index
  digest read, same leaves).
- `merge_carry_forward_drops_undeclared_binding` — a `(group,name)` absent from `candidate` is not
  carried (remove path).

`upgrade.rs`: replace the 3 deleted `is_partial_op` tests with a parser test asserting `--group` and
positional args are rejected (exit 64). `lock.rs`: parser test asserting `--group`/`--upgrade` are
rejected (exit 64). `error.rs`: assert updated `StaleLockOnPartial` message names `ocx lock`,
`LockUpgradeRequired` names `ocx upgrade`, neither contains an internal function name, both are
tier-neutral.

### 8.2 pytest acceptance tests (`test/tests/`)

- `test_project_add.py`:
  - `test_add_preserves_untouched_pin_when_upstream_tag_moved` (core regression — move A's upstream,
    `ocx add B`, A's digest unchanged).
  - `test_add_fails_when_toml_handedited_since_lock` (exit 65, message names `ocx lock`).
  - `test_add_with_no_lock_succeeds`, `test_add_with_empty_tools_v2_lock_succeeds` (bootstrap never
    fails closed).
  - update the existing missing-V1-index add test → exit 78 message now names `ocx upgrade`.
- `test_remove.py` (or lifecycle):
  - `test_remove_preserves_untouched_pins_when_upstream_moved`.
  - `test_remove_fails_when_toml_handedited_since_lock` (exit 65).
  - **Enumerate + flip** every existing remove test that asserts survivor re-resolution to assert
    verbatim preservation (the contract change in §4.4). Read each body before flipping; confirm its
    intent.
- `test_upgrade.py`:
  - `test_upgrade_bumps_every_tag` (whole-file forced bump even when clean).
  - `test_upgrade_rejects_group_flag` and `test_upgrade_rejects_positional_args` (exit 64).
  - `test_upgrade_offline_uncached_policy_blocked` (exit 81).
  - delete/retire any `test_upgrade_named_*` / `test_upgrade_group_*` tests (the subset surface is
    gone) — enumerate and remove.
- `test_lock.py`:
  - `test_lock_clean_is_byte_identical_noop` (clean → byte-identical, `generated_at` preserved).
  - `test_lock_dirty_reresolves_all_declared_tags` (dirty → whole-file re-resolve; a moved tag
    advances — documented as intended).
  - `test_lock_rejects_group_flag` and `test_lock_rejects_upgrade_flag` (exit 64).
  - delete/retire `test_lock_group_filter_*` and any `lock --upgrade` tests — enumerate and remove.
- `--global` path: `test_global_partial_mutator_fail_closed_message_not_project_only`.

### 8.3 Verification honesty

Run the unit suite with the panic-on-resolve fake to *prove* zero registry contact for untouched
entries; cite the passing test names (not "should work"). Final gate: `task verify`.

---

## 9. Doc surfaces + ADR amendments + mirror impact

### 9.1 Doc surfaces (per memory `feedback_plans_must_list_docs`; no migration prose in user docs; help follows `quality-cli-help.md`)

- `website/src/docs/reference/command-line.md`:
  - `add`/`remove`: partial mutators preserve untouched pins; drift → exit 65 run `ocx lock`; a
    carried legacy entry that can no longer be migrated → exit 78 run `ocx upgrade`.
  - `lock`: **whole-file reconcile**; remove the `-g/--group` row and the `--upgrade` documentation;
    state clean → no-op, dirty → re-resolves all declared tags (a moving tag may advance).
  - `upgrade`: **whole-file forced bump**; remove the `-g/--group` and `PKGS...` rows; state it
    re-resolves every declared tag; offline/frozen + uncached → exit 81.
  - Groups section: groups are a composition concern (`add -g`, `run`/`env` subset), never a
    lock/upgrade scope.
- `website/src/docs/reference/command-line.md` flag tables in `subsystem-cli-commands.md` mirror —
  update the toolchain-tier summary rows for `lock` (drop `-g/--group`) and `upgrade` (drop
  `-g/--group`, drop `PKGS...`).
- `website/src/docs/in-depth/project.md` — "pin preservation" subsection: only `ocx lock` (reconcile)
  and `ocx upgrade` (forced bump) move version pins; `add`/`remove` never bump an untouched binding;
  V1→V2 migration is automatic and pin-preserving on any write.
- `website/src/docs/user-guide.md` — one use-case line: "adding or removing a tool never silently
  upgrades your other tools."
- exit-codes reference — 65 "lock out of sync; run `ocx lock`" and 78 "legacy entry; run
  `ocx upgrade`" listed for `add`/`remove`.
- Doc-comments (`///`, `quality-cli-help.md`): `add.rs`/`remove.rs`/`upgrade.rs`/`lock.rs` — drop the
  "falls back to a full resolve" and "`--upgrade` migrates" language; state the present user contract;
  no internal references, no migration history.

### 9.2 ADR amendments (`adr_per_platform_lock_pinning.md`)

- Lock-format-migration section: the silent full-re-resolve fallback in `add`/`remove`/`upgrade` is
  **deleted**; `add`/`remove` carry untouched bindings forward verbatim and migrate carried V1 entries
  by reading the **pinned index digest** (deterministic, pin-preserving); `ocx lock --upgrade` is
  **removed** (migration is automatic on write); `ocx lock` is a whole-file reconcile and `ocx upgrade`
  is the whole-file bump verb; groups are not a lock/upgrade scope.
- Record the migration-vs-re-resolution distinction (§1) as the rationale.
- Codex-R2/H1: upheld **structurally** (no `resolve_lock(&[])` reachable from a mutator), not by the
  deleted `ensure_untouched_v1_transcribable` guard.
- Consumer/validation tables: update `add`/`remove`/`upgrade`/`lock` rows; remove `lock --upgrade`.

### 9.3 ocx-mirror impact — NONE

Read consumer; never invokes `add`/`remove`/`lock`/`upgrade`; no on-disk format or metadata change
(§5). **No mirror coordination required.**

---

## 10. Difficulty verdict + ordered TDD steps

### 10.1 Honest difficulty verdict — SMALLER than the prior Medium

**Size: SMALL–MEDIUM (low end of Medium).** This model is materially smaller than the prior spec's
"Medium (high end)":

- **Net deletions outnumber additions.** Deleted: `resolve_lock_partial`,
  `ensure_untouched_v1_transcribable`, `touched_bindings`, `upgrade_lock`, the entire `upgrade`
  partial branch + `is_partial_op` + its validation loops, the `lock` `-g` filter + `--upgrade`
  branch, ~6+ now-obsolete acceptance tests. Added: one function (`resolve_lock_touched`) + one
  private helper (`merge_carry_forward`), both recombining existing primitives.
- **One concentrated risk** (the two-hash gate, §3) versus the prior spec's distributed risk across a
  partial-upgrade matrix.
- **Two whole-file commands become trivial.** `upgrade` collapses to one `resolve_lock(&[])` line;
  `lock` collapses to one `resolve_lock(&[])` line. The bulk of the old `upgrade.rs`/`lock.rs`
  complexity is *removed*, not rewritten.

Quantified estimate: roughly **1–1.5 focused days** (down from the prior 1.5–2.5). The long pole is
enumerating and flipping the existing `remove`/`upgrade`/`lock` acceptance tests whose assertions
encode the old subset/re-resolve behavior — careful work, not hard work.

**Riskiest part:** the two-hash split in `resolve_lock_touched` (§3). Anchor freshness on the
candidate hash → every clean `add` fails closed; stamp the pre-mutation hash → the commit-time gate
(`mutation.rs:270-279`) rejects. Prove it first with the panic-on-resolve unit tests.

### 10.2 Ordered implementation steps (TDD: failing test → change)

1. **Error message contract.** Test (`error.rs`): `StaleLockOnPartial` Display names `ocx lock`;
   `LockUpgradeRequired` names `ocx upgrade`; no internal function name; tier-neutral. → Edit both
   `#[error(...)]` strings (§4.1). File: `crates/ocx_lib/src/project/error.rs`.
2. **`resolve_lock_touched` + `merge_carry_forward` skeleton (stub `unimplemented!()`).** Gate:
   `cargo check`. File: `crates/ocx_lib/src/project/resolve.rs`.
3. **Unit tests for `resolve_lock_touched` (write FIRST, fail against stub).** All §8.1
   `resolve_lock_touched_*` + `merge_carry_forward_*` tests with the panic-on-resolve index fake;
   assert stamped metadata hash == hash(candidate). File: `resolve.rs` (`#[cfg(test)]`).
4. **Implement `merge_carry_forward` + `resolve_lock_touched`** (§3.1, §3.2). Gate: step-3 tests
   pass. File: `resolve.rs`.
5. **`add` rewrite.** Acceptance tests first (§8.2 add). → Rewrite `add.rs:99-166` per §4.3 (delete
   fallback + guard + `touched_bindings`; call `resolve_lock_touched`; keep bootstrap `resolve_lock`).
   Files: `crates/ocx_cli/src/command/add.rs`, `test/tests/test_project_add.py`.
6. **`remove` rewrite (behavior change).** Enumerate existing survivor-re-resolution tests; add the
   two new remove tests; flip the enumerated ones to verbatim-preservation. → Rewrite `remove.rs`
   per §4.4 (`resolve_lock_touched(touched=∅)`; delete pre-pass guard). Files:
   `crates/ocx_cli/src/command/remove.rs`, `test/tests/test_remove.py`.
7. **`upgrade` collapse.** Tests first (§8.2 upgrade; delete obsolete `_named`/`_group` tests). →
   Delete `-g`/PKGS + partial branch + `is_partial_op`; bare `resolve_lock(&[])` (§4.5). Files:
   `crates/ocx_cli/src/command/upgrade.rs`, `test/tests/test_upgrade.py`.
8. **`lock` collapse.** Tests first (§8.2 lock; delete obsolete `_group_filter`/`--upgrade` tests). →
   Delete `-g`/`--upgrade`; bare whole-file `resolve_lock(&[])` (§4.6). Files:
   `crates/ocx_cli/src/command/lock.rs`, `test/tests/test_lock.py`.
9. **Drop dead library code.** Delete `resolve_lock_partial`, `ensure_untouched_v1_transcribable`,
   `touched_bindings`, `upgrade_lock` and their re-exports (`project.rs:38-39`); confirm no unused-fn
   warnings. Gate: `cargo build` clean. Files: `resolve.rs`, `crates/ocx_lib/src/project.rs`.
10. **`--global` remedy message.** Test
    `test_global_partial_mutator_fail_closed_message_not_project_only`; confirm step-1's tier-neutral
    phrasing satisfies it. Files: `test/tests/`, `error.rs` if needed.
11. **Docs + ADR.** Update `command-line.md`, `in-depth/project.md`, `user-guide.md`, exit-codes ref,
    the four command `///` doc-comments (§9.1), and the ADR amendments (§9.2). File the `pull`
    V1-direnv follow-up issue (§4.7).
12. **Full gate.** `task rust:verify` during the review-fix loop; `task verify` final. Confirm the
    panic-on-resolve fake catches any reintroduced silent re-resolve.

---

## 11. Adversarial self-check — flows that could break or surprise

Each surprising flow below is **intended and documented** under the decided model:

1. **`ocx lock` on a dirty `ocx.toml` advances a moving tag (e.g. `:latest`).** Surprising if a user
   expects `lock` to be purely a "write what's pinned" op. **Intended:** `lock` is a whole-file,
   user-invoked reconcile; advancing a moving tag is what reconcile means. Documented in
   `command-line.md` `lock` + `in-depth/project.md`. Distinguished from a *silent incidental* bump
   (which is what `add`/`remove` now refuse to do). Mitigation for users who want a pure check:
   `ocx lock --check` (read-only, exit 65 on drift, never writes).

2. **Fresh clone, all-V1 lock, every V1 index GC'd → `ocx add B` fails exit 78.** Surprising coupling:
   an unrelated carried V1 entry blocks the add. **Intended:** the pinned index is genuinely gone, so
   exact-only transcription cannot preserve the pin; re-resolving the live tag would drift it
   (Codex-R2). Fail-closed with remedy "run `ocx upgrade`" (whole-file re-resolve restores
   retrievable pins). Documented in `command-line.md` `add` + exit-codes ref. Acceptance test:
   `test_add_*` missing-index case (exit 78, names `ocx upgrade`).

3. **`run`/`env` on an orphaned V1 lock (index GC'd) cannot select the host platform.** **Intended
   and unchanged** by this design — read path already branches on `LockedResolution`; a V1 entry
   resolves its leaf via the legacy index-digest path, and if that index is gone the read fails (the
   pre-existing behavior the ADR's V2 format fixes going forward). No new failure introduced here.
   Remedy: `ocx upgrade` to re-resolve into V2. Out of scope to change read behavior.

4. **`ocx upgrade` on a clean lock still re-resolves everything.** Surprising if a user expects "no
   change → no work." **Intended:** `upgrade` is the explicit forced-bump verb; doing nothing on
   clean would make it indistinguishable from `lock`. If the re-resolve yields identical content, the
   save is byte-identical (`tools_content_equal` preserves `generated_at`), so the on-disk result is a
   no-op even though network work happened. Documented in `command-line.md` `upgrade`.

5. **`add`/`remove` after a hand-edit to an *unrelated* binding's tag → exit 65.** `declaration_hash`
   is whole-config, so the gate cannot tell "edited the binding I'm adding" from "edited another
   binding"; both trip the pre-mutation-hash mismatch. **Intended:** safe default — never carry
   forward against a stale lock. Remedy is one command: `ocx lock` reconciles the edit, then the add
   proceeds. Documented; acceptance test `test_add_fails_when_toml_handedited_since_lock`.

6. **`upgrade`/`lock` invoked with the now-removed `-g`/PKGS flags → exit 64.** Surprising for users
   with muscle memory or scripts. **Intended** (pre-1.0, no migration prose per memory
   `feedback_no_migration_prose_in_docs`): clap rejects the unknown arg → EX_USAGE 64. The error is
   clap's standard unknown-argument message. Covered by parser tests in §8.1.

All six are confirmed intended and have a documentation surface and/or a test in §8. No flow silently
moves a pin on a mutator; no flow drops a binding from the lock; no flow re-resolves a live tag
except the two explicit whole-file commands.
