# Spec/Contract Review — `adr_shell_env_overhaul.md`

Phase 5, spec/contract consistency. Question: are the contracts testable,
internally consistent, and complete enough for `/hex-plan` to decompose without
re-deriving the design?

**Verdict: no, not yet.** Four Block-tier defects make named contracts
unimplementable or unsafe as written. Ranked most-severe first.

---

## 1. BLOCK — Decision 4: the consent predicate is vacuously true for a lock-less project

**Where:** Decision 4, clause 2 ("every source in the current lock matches the namespace whitelist").

Clause 2 is a universal quantifier over the lock's source set. When that set is
empty — no `ocx.lock`, or a lock with no tools — it is **true for every user,
including one with an empty whitelist**, so activation is permitted with no
consent stamp and no whitelist entry.

**Failure scenario (CONFIRMED against code).** `crates/ocx_lib/src/project/env.rs:13-21`
documents the shipped grammar with this exact example:

```toml
[env]
PATH = { type = "path", value = "node_modules/.bin" }
```

and "A relative `path` value resolves against the project root". So: clone a repo
containing an `ocx.toml` with a `[env] PATH = { type = "path", value = "bin" }`
and no `ocx.lock`. `cd` into it. Clause 2 passes vacuously → activation →
`<clone>/bin` is PATH-front at the next prompt. That is precisely the Decision 4
threat statement ("puts attacker-controlled binaries in front of `cmake`, `cargo`,
`git`") reached by a path the predicate does not cover, and it defeats the
headline claim "A fresh clone is inert."

The ADR itself makes the lock-less case first-class in Decision 3: "`[env]`
applies on its own authority independently of the lock, so watching locks alone
would miss `[env]`-only edits." So the two decisions are internally inconsistent:
Decision 3 says `[env]` activates without a lock; Decision 4 gates only on lock
sources.

Decision 4 also specifies the *unreadable* lock ("no activation") but never the
*absent* lock. Undecidable as written.

**Needs:** the predicate must be non-vacuous (require a non-empty stamped source
set, or gate project `[env]` separately from lock sources), and the absent-lock
case must be stated.

## 2. BLOCK — Decision 1: "reuses the existing `Entry` serialization" — there is no such serialization

**Where:** Decision 1 § Contract, "Shape": *"`applied` reuses the existing
`Entry {key, value, kind, separator}` serialization (D6 — no second schema)"*.
Component Contracts: `Ledger::decode(&str) -> Option<Ledger>`.

Three CONFIRMED errors in one load-bearing premise:

- `ocx_lib::package::metadata::env::entry::Entry` derives `#[derive(Debug, Clone)]`
  only — **no `Serialize`, no `Deserialize`** (`crates/ocx_lib/src/package/metadata/env/entry.rs`).
- `ModifierKind` derives `Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema`
  — **`Serialize` only, no `Deserialize`** (`crates/ocx_lib/src/package/metadata/env/modifier.rs:75`).
- The only serialized entry shape in the tree is `ocx_cli::api::data::env::EnvEntry`
  (`crates/ocx_cli/src/api/data/env.rs:71`): `#[derive(Serialize)]` only, lives in
  the **CLI** crate, renames the field `kind` → `"type"` via `#[serde(rename = "type")]`,
  and `skip_serializing_if` on `separator`.

**Failure scenario.** `Ledger::decode` is specified in `ocx_lib::shell::reconcile`
(Component Contracts), which cannot depend on `ocx_cli`. A builder implementing
the stated contract finds nothing to reuse, must add `Deserialize` to both `Entry`
and `ModifierKind`, and must decide whether the wire field is `kind` (ADR text) or
`type` (the shipped `--format json` shape the nushell shim already parses,
`shims.rs:212` — `$_ocx_e.type == "path"`). A ledger written as `kind` and an
`ocx env --format json` emitting `type` is exactly the "second schema" D6 forbids.

**Needs:** name the concrete type, the crate it lives in, the wire field names,
and whether `EnvEntry`'s `type` rename is the ledger's shape too.

## 3. BLOCK — `Shell::remove_path_element` cannot express the revert contract it exists for

**Where:** Component Contracts, `ocx_lib::shell::Shell`:
`remove_path_element(key: impl AsRef<str>, value: impl AsRef<str>) -> Option<String>`.
Decision 3 scopes revert to "**List-kind vars** (PATH, `LD_LIBRARY_PATH`, any
separator-joined list) — element operations only … Revert: **remove our elements**".

The signature has **no separator parameter**. CONFIRMED against the shipped pair:
`Shell::export_path(key, value)` (`shell.rs:154`) takes none because PATH uses the
platform separator; `Shell::export_list(key, value, separator)` (`shell.rs:319`)
takes one because a list's separator is per-entry data (`Entry.separator:
Option<String>`, settled by `env::reconcile_list_separators`).

**Failure scenario.** A package declares `CFLAGS` as `{type = list, separator = " "}`
or `CLASSPATH` as `{type = list, separator = ":"}` on Windows. Apply works
(`export_list` gets the separator). Revert calls `remove_path_element`, which can
only assume the platform separator — it either removes nothing or splits the value
on the wrong byte and corrupts it. Every non-default-separator list var is
permanently unrevertible.

**Needs:** `remove_list_element(key, value, separator)`, or an explicit statement
that revert is PATH-only and constants/list vars use a different mechanism.

## 4. BLOCK — `plan()` cannot execute the repair path it is specified to own

**Where:** Component Contracts: `plan(desired: &[Entry], current: &Env, ledger:
&Ledger) -> Plan`, "Platform-neutral, unit-testable, **no I/O**".

Decision 1's degradation rule and Decision 3's ownership rule both make `plan()`
responsible for repair-without-a-ledger: *"repair lists via the ownership prefix"*
/ *"elements under `$OCX_HOME` are self-identifying by prefix … The prefix test
doubles as the repair path when L is lost."*

**No parameter carries `$OCX_HOME`.** CONFIRMED: the signature has three
parameters, none of which is the prefix set. Decision 3 also says that if
[#189](https://github.com/ocx-sh/ocx/issues/189) lands, `.ocx/toolchain/` "joins
the prefix set additively" — a second prefix, also unrepresented.

**Failure scenario.** A shell drops `__OCX_ENV_STATE` (nested `sudo -i`, a
`env -i` wrapper, the over-cap rung). The next prompt calls `plan()` with an empty
ledger. It must remove ocx's PATH elements to converge, cannot identify them
without `$OCX_HOME`, and either leaves every element (unbounded PATH growth across
project switches) or reads the env var itself — violating "no I/O".

Secondary signature mismatch, same block: `Ledger::decode` returns
`Option<Ledger>` but `plan` takes `&Ledger`, with no specified `Ledger::empty()`
/ `Default`. The absent-ledger call is unrepresentable.

## 5. BLOCK/WARN — "zero config reads per prompt" fails for an unconsented project; no negative cache exists

**Where:** Decision 5 (*"The per-prompt path reads **no config at all** … Config is
loaded only when the fingerprint has already decided a recomposition is needed"*)
vs. Decision 4 clauses 2 and 3 (whitelist lives in `config.toml`) vs. NFR Latency
(*"no-op path ≤ 5 ms … asserted by a CI benchmark with a hard fail"*).

Evaluating consent for an unconsented project requires reading `[shell.trust]`
from `config.toml`. Nothing caches the `Inert` verdict: the ledger shape is
`scopes.project: { key, dir, applied, priors }` — **no verdict field, no
`inert_for_fp` marker**.

**Failure scenario (CONFIRMED from the ADR's own state shape).** A user sits in a
freshly-cloned, unconsented project. Every prompt: the upward CWD walk finds
`ocx.toml`, no stamp exists, so the ADR's own sequencing rule ("the stamp and
whitelist are read before any project file influences configuration") forces a
`ConfigLoader::load_with_local_view` pass — three `symlink_metadata` probes, up to
three TOML parses, one managed-snapshot read — **plus** parsing the lock for the
source set. That is the ADR's own Decision-5 cost estimate for shell *startup*,
now paid at every prompt, and it happens in the exact state ("fresh clone") the
design says is the common one. The ≤5 ms budget assertion would either fail or be
written to exclude the case it most needs to cover.

**Needs:** a verdict cache keyed on the fingerprint in the ledger, or an explicit
statement that the inert path is exempt from the budget with a separate number.

## 6. WARN — OD-2: `Config::merge` makes the managed tier win in *both* directions; reversal level 1 is defeated by level 2

**Where:** Decision 7 (`[shell] hook` merges unconditionally) vs. "How Would We
Reverse This?" §1–§2.

CONFIRMED fold order: `Config::merge` is *"`other` has higher precedence — its set
fields override `self`'s. Scalars: `other` wins when present"* (`config.rs:140-145`, `Config::merge` at `:145`),
and `fold_managed_tier` runs `accumulator.merge(parsed)` **after** the discovered
system→user→home chain (`config/loader.rs`, end of `fold_managed_tier`). So the
managed tier beats every file a user can edit.

Two consequences the ADR does not state:

- Decision 7 justifies only the fleet-**off** direction ("hooks off on build
  agents is a legitimate operator policy"). Under plain scalar merge the managed
  tier can equally force `hook = true` over a user's explicit `hook = false`. The
  security argument offered ("turning the hook on grants nothing, because consent
  still gates every project") is load-bearing and should be stated as such,
  because it is the *only* thing standing between a fleet toggle and forced
  activation.
- Reversal §1 lists "`[shell] hook = false`, or `OCX_NO_HOOK=1`, or `ocx self
  setup --no-hook`" as a per-user lever. Two of those three write the same
  lower-precedence config key and are silently overridden by §2's mechanism. Only
  `OCX_NO_HOOK` survives a managed tier. The four reversal levels are presented as
  independent; two are not.

## 7. WARN — Decision 3: the apply/revert asymmetry is stated but never reconciled with D1

**Where:** Decision 3, Constant-kind vars. Apply: *"set to D **unconditionally**"*.
Exit: *"restore the prior **only if C == L**"*.

The asymmetry is present in the text with a rationale for the guarded half
("Never clobber a user's mid-session override on the way out — the single
most-disliked direnv behavior") and a bare parenthetical for the unguarded half
("project wins while inside the project"). It is never named as an asymmetry, and
it is never reconciled with **D1 — never clobber a foreign write** ("must be
structurally impossible, not merely avoided"). Apply *is* a foreign-write clobber;
D1 is stated without that carve-out.

**Failure scenario.** Inside a project, a user runs `export JAVA_HOME=/opt/jdk21`
to test something. They then edit `ocx.toml`'s `[env]`, or run `ocx update`. The
fingerprint changes, recomposition fires, apply overwrites `JAVA_HOME` back to D
with no signal. On the way *out* the same override would have been respected. The
"Coincidence (C ≠ L but C == D): claim silently" rule only covers the case where
the foreign write happens to equal D.

Compounding: the ADR never says which trigger dominates. Enter/leave/switch are
PWD events; recomposition is fingerprint-driven. Whether a same-project prompt
with an unchanged fingerprint re-asserts D over a drifted C is unspecified, and it
is the difference between "foreign writes survive" and "foreign writes survive
until the next lock bump".

## 8. WARN — "sources" is never defined; the subset predicate and the `/*` boundary are undecidable

**Where:** Decision 4 (*"`sources` are the registries+namespaces the lock resolves
against"*), the whitelist grammar (`namespaces = ["ocx.sh/acme-corp/*"]`), and
`ConsentStamp { sources: BTreeSet<String> }`.

No normalization is given from an `oci::Identifier` to a source string. Open and
test-blocking:

- Is `ghcr.io/acme/tools/cmake:3.28` one source `ghcr.io/acme`, or
  `ghcr.io/acme/tools`, or the full repository path?
- Is the default registry (`ocx.sh`) elided or spelled? An identifier parsed via
  `Identifier::parse_with_default_registry` carries it either way.
- Does `ocx.sh/acme-corp/*` match `ocx.sh/acme-corp/sub/team/tool`, i.e. is `*`
  one path segment or all descendants? The ADR argues descendant implication is
  "the *product requirement* here", but the grammar line says "namespace prefix
  with a single trailing `/*`" — those are different rules.
- Case and port normalization (`localhost:5000` is called out as the reason for
  the comma separator, but never as a normalization rule).

The Validation section names a test — *"namespace `/*` boundary"* — that cannot be
written against this text. Same for the *"same-cardinality swap"* subset test,
which needs to know whether `ghcr.io/acme` and `ghcr.io/acme-corp` are distinct
source strings.

## 9. WARN — `self setup --hook` names neither a config tier nor a write mechanism

**Where:** Decision 5 (*"writes `[shell] hook = true|false` into the user
`config.toml`"*) and Component Contracts (`ocx self setup | + --hook | --no-hook`).

"the user `config.toml`" is ambiguous against the three concrete accessors
`ConfigLoader::system_path()` / `user_path()` / `home_path()`
(`config/loader.rs:889,895,919`), where `user_path()` is
`config_dir()/ocx/config.toml` and `home_path()` is `$OCX_HOME/config.toml`.

CONFIRMED prior art points the other way: the shipped `self setup --managed`
writes to **`file_structure.root().join("config.toml")`** — the *home* tier
(`setup.rs:389`) — via `rc_block`-style fence machinery, not `user_path()`.

Nothing is said about *how* the key is written: whole-file rewrite (impossible —
`Config` derives `Deserialize` only, no `Serialize`, `config.rs:39`), surgical
`toml_edit` edit (the workspace has `toml_edit = "0.25"`), or a managed fence.
Nor: what happens when the file does not exist, when `--config`/`OCX_CONFIG` is in
play, or when a higher tier already sets the key (see finding 6).

A planner cannot write a work package for this without picking all four.

## 10. WARN — Decision 6 cites a test that does not enforce the invariant it is cited for

**Where:** Decision 6 (*"the `each_shim_resolves_the_binary_through_the_current_symlink`
test asserts `invokes_binary` at `:461-464`"*), offered as the guard for the
thin-dispatcher invariant.

CONFIRMED at `crates/ocx_lib/src/setup/shims.rs:445,461-464`. The assertion is:

```rust
let invokes_binary = body.contains("self activate")
    || body.contains("@('self', 'activate', '--shell=powershell')")
    || body.contains("--format json --global env");
assert!(invokes_binary, "{name} must invoke the ocx binary to activate");
```

It asserts *"invokes the binary"*, not *"is a pure dispatcher with no ocx business
logic"*. A shim that inlined arbitrary reconciliation logic would pass it
unchanged — indeed `ENV_NU` already does, and the third `||` arm exists precisely
to let it. The ADR's own Decision 6(b) says nushell inlines the whole body. So the
cited test is green for both the invariant holding and the invariant being
violated. The ADR should say the invariant is **unenforced** and name the check
that would enforce it, or drop the citation.

## 11. WARN — nushell: `remove_path_element -> Option<String>` has no consumer on the one shell with no `eval`

**Where:** Decision 6(b), Decision 5 delivery layer 2 (*"the hook re-invokes `self
activate` behind a hidden plumbing flag (`--reconcile`) → … reconcile script only
on change"*), and the `remove_path_element` contract.

CONFIRMED: `ENV_NU` (`shims.rs:197-215`) never calls `self activate`; it runs
`^ocx --format json --global env | from json` and applies entries with `load-env`
(`shims.rs:212`). There is no channel by which a `String` of nu source produced
per-prompt can be executed.

Three unresolved consequences, none stated:

- Is nushell's reconcile emitted as nu *text* inlined into the shim body (making
  every reconciler change a body change → one-hop lag per change, per 6(b)), or as
  *structured data* the inlined nu applies (in which case `Plan` needs a JSON
  wire shape that the ADR never specifies)?
- `load-env` cannot unset a variable; `hide-env` is the only primitive and the
  ADR's own Validation section flags its scoping inside hook blocks as "a known
  hazard". Decision 3's constant-revert (`restores: (key, None)`) is therefore
  unimplementable on nu until the spike lands.
- `remove_path_element` is specified to return `None` for "a shell that cannot
  express it (Batch)". Nushell is not named, though it is the shell that
  demonstrably cannot consume the return type.

## 12. WARN — "byte-identical between the in-process and emitted forms" is asserted with no named check

**Where:** Decision 3, list apply: *"the shipped `utility::path::move_to_front` /
per-shell `Shell::export_path` pair, unchanged and **byte-identical** between the
in-process and emitted forms"*. Also NFR / Untouched list.

CONFIRMED: `shell.rs` has per-shell *idempotency* tests that run real shells —
`live_bash_zsh_idempotent_move_to_front:1353`, `live_posix_…:1361`,
`live_fish_…:1436`, `live_powershell_…:1446`, `live_batch_…:1485`, plus
`export_path_batch_idempotent_move_to_front:1192`. Every one asserts the *emitted*
form is idempotent. **None compares the emitted result against
`utility::path::move_to_front`'s.** The parity note the discover artifact cites
(`shell.rs:319-345`) is prose on the sibling `export_list`/`append_unique` pair.

This matters more here than before the ADR: today the two forms serve different
call sites, and the reconciler is the first consumer that will apply in-process on
one prompt and via emitted text on another, in the same session. A divergence
would show up as PATH order flapping between prompts. The ADR should require the
parity test rather than assert the property.

## 13. WARN — the `ocx clean` sweep re-derives a key it could read, and drops state for lock-less projects

**Where:** Decision 2 § Contract: *"removes any `state/projects/<key>/` whose
`<key>` is not in `{ name_for_path(dir) | dir ∈ live_projects() }`"*.

Two CONFIRMED problems:

- **Re-derivation is unnecessary and lossy.** The ledger entry *filename already
  is* `name_for_path(canonical_dir)` (`registry.rs:325-326`). `live_projects()`
  discards it and returns targets — `dunce::canonicalize(entry_path)` for `Live`,
  and for `Unknown` a best-effort `read_link` recovery (`unknown_root_or_escalate`).
  Re-hashing a re-canonicalized target can produce a different 16-hex than the one
  written at stamp time (canonicalize is path-byte sensitive; `name_for_path`
  hashes `as_os_str().as_encoded_bytes()`). The sweep should compare against the
  ledger's own entry names.
- **A lock-less project's state is collected every run.** `probe_live_target`
  returns `Live` only when `<target>/ocx.lock` exists (`registry.rs:127-129`), and
  registration only happens from `ProjectLock::save` and project mutation
  (`project/lock.rs:448`, `project/mutation.rs:373`). An `[env]`-only project — the
  case Decision 3 explicitly designs for — has no lock, no ledger entry, and so its
  `consent.json` is swept on the next `ocx clean`, making the project inert again.
  The ADR's `Unknown`-is-live carve-over is stated; this one is not.

## 14. WARN — the global-tier carve-out has no reachable red state

**Where:** Decision 2 § Contract, "Global-tier carve-out": `name_for_path($OCX_HOME)`
is *"**exempt from the sweep**"*, and in the same bullet *"The global scope needs
no consent stamp at all"*.

If nothing ever writes `state/projects/<name_for_path($OCX_HOME)>/`, the exemption
can never be observed — a test for it passes whether the code implements it or
not. Either name what global state lands there (making the exemption testable), or
drop it. This is the "green that cannot be told from never ran" class the ADR's own
Validation section commits to avoiding.

## 15. SUGGEST — `[[trust.policy]]` is a mis-cited analogue for the union rule

**Where:** Component Contracts (*"both lists **append** (union, like
`[[trust.policy]]`)"*) and Decision 4 (*"Precedence: union, never override … There
is no untrusted tier in the union, so no precedence logic is needed"*).

CONFIRMED: `[[trust.policy]]` is not a plain union. `Config::merge` appends at
storage, then `trust::resolve` **masks by specificity** (a later tier's more
specific scope displaces an earlier tier's broader pin), and `apply_system_locks`
exempts the system tier so a user-tier entry cannot mask a system pin
(`config.rs:176-193`, `apply_system_locks` at `:184`). The chosen semantics (plain union, no masking) is
self-consistent; the citation makes it sound like an existing pattern is being
reused when a simpler new one is being introduced. Say "plain union — unlike
`[[trust.policy]]`, which masks by specificity".

## 16. SUGGEST — `remove_path_element` is specified for 10 shells, 5 of which can never host a hook

**Where:** Decision 3 (*"one new primitive, `Shell::remove_path_element`, with the
full 10-shell matrix"*), Validation (*"`remove_path_element` 10-shell idempotency
matrix"*), Decision 5 layer 1 (hook mechanisms named for bash, zsh, fish,
PowerShell, nushell only).

`Shell` has exactly 10 variants (`Ash, Ksh, Dash, Bash, Elvish, Fish, Batch,
PowerShell, Zsh, Nushell` — CONFIRMED). Five get a named per-prompt mechanism.
Batch (`cmd.exe`) has no prompt hook at all. Building and testing 10 arms for 5
consumers is real work a planner would schedule; say which arms are load-bearing.

Separately, the Batch `None` contract is inherited from the wrong sibling: the ADR
says `None` for "a shell that cannot express it (Batch — same contract as
`export_list`, which returns `None` unconditionally there)". But `export_path`
*does* work on Batch via `%VAR:search=%` substring-delete
(`shell.rs`, `export_path_batch_idempotent_move_to_front:1192`), so Batch can
delete a PATH element case-insensitively. The `export_list` `None` exists because
list elements need **case-sensitive** matching, which is a different constraint.

## 17. SUGGEST — `--reconcile`'s relationship to the enablement ladder is unspecified

**Where:** Decision 5 (`Hook::enabled(&self, interactive: bool, configured:
Option<bool>) -> bool`; *"hook presence in a session is decided once, at
startup"*) and Component Contracts (`ocx self activate | + --reconcile (hidden
plumbing, per-prompt)`).

`--reconcile` runs in a fresh process with no `configured` value (reading one would
violate the zero-config-per-prompt rule). Whether it bypasses `Hook::enabled`
entirely, and therefore whether `OCX_NO_HOOK=1` exported mid-session takes effect
at the next prompt or only at the next shell start, is not stated. Reversal §1
implies the latter ("Takes effect at the next shell start"), which is a defensible
answer — say it in the contract.

Minor, same block: the `Hook` struct as written carries no doc comments on its two
flags, unlike the shipped `Completion` template it cites
(`crates/ocx_cli/src/options/completion.rs:16-22`), where each flag's doc comment
*is* its `--help` text.

## 18. Completeness for decomposition — what is underspecified

A planner could produce file-disjoint packages for: `StateStore` path helpers,
`ocx_lib::config::shell` + schema regen, `options::Hook`, the `guard_managed_*`
extension, and `project::consent`. It could **not** produce them for the
reconciler, per-shell emission, or the CLI wiring, because these are missing:

| Missing | Consequence for decomposition |
|---|---|
| Ledger wire schema (see finding 2) | The reconciler package and the emission package cannot agree on a contract stub. |
| Per-shell emission detail — what the `--reconcile` stdout looks like per family, and how nushell consumes it (finding 11) | The 5-shell emission work cannot be split from the reconciler. |
| `remove_path_element` per-shell hazards. Decision 3 names two ("zsh glob over-match, bash `${//}` pattern escaping") for 10 arms | The matrix is a research task, not an implementation package. |
| Error and exit-code semantics | D3 says the hook exits 0 always, but nothing gives an exit code for `package create` rejecting a reserved key (`ExitCode::` variant?), for `--reconcile` failing, or for the `Inert` verdict. `quality-rust-exit_codes.md` requires these to be decided. |
| First-run-with-no-ledger behaviour end to end | Decision 1's degradation rule covers *decode*; nothing states whether the very first prompt applies-without-revert, or whether an empty ledger and a corrupt ledger take the same path (they should not — the first has nothing to repair). |
| Verbosity control surface | NFR Operability specifies `silent \| summary \| full` with a sample line, but names no config key, no flag, and no env var. Undecidable. |
| Consent-stamp write points | "Written by any explicit project-scoped ocx command (`add`, `remove`, `lock`, `update`, `pull`, `run`)" names six commands but no seam. `register_project_dir_best_effort` is called from two places only (`project/lock.rs:448`, `project/mutation.rs:373`) — `ocx run` and `ocx pull` are not among them, so the stamp needs its own call sites, in files those packages would otherwise not touch. |
| `[NEEDS CLARIFICATION 1]` (one list or two) | Changes the `ShellTrust` struct, the schema, the env-var count, and the test matrix. Blocks the config package. |
| `[NEEDS CLARIFICATION 3]` (PS 5.1 fidelity) | Changes the PowerShell emission package's scope. Blocks it. |
| `[NEEDS CLARIFICATION 2]` (default-on) | One constant in `Hook::enabled`. Does **not** block decomposition. |

## 19. Cited-anchor verification

Every anchor the review brief named, checked against the file.

| Citation | Verdict |
|---|---|
| `update.rs:109-111` — `refresh_shell_integration_after_swap` runs in the old binary | **CONFIRMED.** Lines 109-111 are the "**Timing caveat:**" doc lines, verbatim as the ADR paraphrases them. Note it is a doc comment, not the mechanism. |
| `project/registry.rs:117-118` — `probe_live_target` returns `Dead` for a non-symlink | **CONFIRMED.** `if !crate::symlink::is_link(entry_path) { return ProbeResult::Dead; }`. The ADR's Option-B risk argument holds. |
| `config/loader.rs:431-457` — `guard_managed_sigstore_trust` gates on a digest-pinned source | **CONFIRMED**, and it is the right home: the guard mutates `parsed: &mut Config` (the payload) and is called immediately before `accumulator.merge(parsed)` in `fold_managed_tier`, so a per-key `[shell.trust]` strip drops in with no merge-order change. |
| `composer.rs:1072-1102` — shim-slot ordering `entrypoints/ > bin/ > shims/` | **CONFIRMED.** `emit_shim_slot`'s doc block. See the caveat below. |
| `shims.rs:63,103,105,146,239` — shims discard the binary's stderr | **CONFIRMED**, with one imprecision: `:146` is the PowerShell arm and reads `2>$null`, not `2>/dev/null`. The claim holds. |
| `shims.rs:197-215` — nushell inlines the apply | **CONFIRMED.** `ENV_NU` const begins at `:197`; the `load-env` apply loop is `:212`. |
| `shims.rs:184` — nushell "reads `ocx --format json --global env`" | **MISCITED (harmless).** `:184` is a doc-comment line; the actual invocation is `:212`. |
| `shims.rs:461-464` — the shim test asserts `invokes_binary` | **CONFIRMED literally, but does not support the claim it is cited for** — see finding 10. |
| `activate.rs:53-61,100-103` — the `Completion` triad | **CONFIRMED.** `#[clap(flatten)] completion` at `:61`; `self.completion.enabled(std::io::stderr().is_terminal())` at `:100`. |
| `activate.rs` — runs pre-`Context::try_init`, constructs only `FileStructure::new()` | **CONFIRMED** by the `execute` doc comment and body. `FileStructure` does own a `StateStore` (`file_structure.rs:70,119`; `setup.rs:1051` uses `file_structure.state.…`), so Decision 2's "no pure associated fn is needed" holds. |
| `env.rs:1143-1146` — `is_reserved_ocx_key` matches `OCX_`/`__OCX_` case-insensitively | **CONFIRMED.** Decision 1's "no new reservation needed" holds. |
| `env.rs:498-502` — ocx strips an inherited `OCX_ENV` | **CONFIRMED** (`self.remove(keys::OCX_ENV)` at `:502`). Small imprecision: this is `apply_ocx_config`, the child-env path, not "every compose". |
| `project/config.rs:122,259` — `deny_unknown_fields` on `ProjectConfig` and `RawProjectConfig` | **CONFIRMED** (attribute at `:121`/`:259`, struct at `:122`/`:260`). The structural argument — `[shell]` in `ocx.toml` is a hard parse error, not a silent no-op — holds. |
| `reference_manager.rs:59-63`, `registry.rs:310,321,325` — key derivation, `dunce::canonicalize` at the call site, ARCH-1b no-self-link | **CONFIRMED.** `name_for_path` is `hex::encode(&sha256(path_bytes)[..8])`. |
| `options/completion.rs` as the `Hook` template | **CONFIRMED.** `Completion::enabled(interactive)` ladder matches; `Hook::enabled(interactive, configured)` adds one rung, consistently. |

**Anchor-adjacent caveat, worth a line in the ADR.** The "Untouched" row asserts
PATH shim-slot ordering is preserved, and `composer.rs:1072-1102` explains why:
*"Consumers apply entries by **prepending** … the *last* entry pushed is *first* in
the resolved PATH. A reader who assumes push order equals PATH order will invert
this and make the shim shadow the real binaries — the bug this ordering exists to
prevent."* The reconciler is a **new consumer** of that `Vec<Entry>`, applying via
`move_to_front` rather than the emit path. The invariant is preserved only if the
reconciler iterates entries in the same order the emit path does. The ADR asserts
the outcome and names no contract or test that produces it.

## Sections that are sound

- **Decision 2's option analysis.** The Option-B rejection (promoting `projects/<hash>`
  to a directory breaks `probe_live_target`) is correct against the code, and the
  data-loss framing is right. The chosen root fits `StateStore`'s existing shape.
- **Decision 8 (OD-3).** The accept-and-document reasoning, and the refusal to
  conflate the consent stamp with `[[trust.policy]]`, are internally consistent and
  correctly scoped.
- **Decision 1's carrier choice (option B).** The nested-shell-inheritance argument
  for environment placement over on-disk is decisive and correctly reasoned; the
  no-compression call is right for the payload size.
- **The `OCX_ENV`-is-not-renamed section.** Matches the brief's downstream findings
  and `env.rs:502`; the withdrawal is justified.
- **`OCX_NO_HOOK` boolean over tri-state.** Matches house style; the six cited
  `OCX_NO_*` precedents are real.
- **Migration and Rollout, and "How Would We Reverse This?"** — accurate on the
  pre-1.0 doctrine and the changelog-is-the-commit-subject rule, with the one
  correction in finding 6.
