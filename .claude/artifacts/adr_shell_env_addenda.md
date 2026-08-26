# Addendum: Shell Environment Overhaul — Binding Resolutions

Companion to [`adr_shell_env_overhaul.md`](./adr_shell_env_overhaul.md) and
[`analysis_shell_env_edge_cases.md`](./analysis_shell_env_edge_cases.md).

**This addendum binds.** ADR + addendum are one specification; where they
conflict, **the addendum wins**. It closes all 51 `UNSPECIFIED-BY-ADR` rows of the
edge-case register plus 6 adjacent rows the register marks "not stated", in 43
resolutions (A-39 through A-43 close review findings rather than register rows). Every code claim the register makes was re-verified against the
worktree before being built on; where the register is wrong about the code, the
resolution says so and decides from the code (see **Register errors** at the end).

Each resolution is numbered `A-NN` and names the `EC-*` ids it closes. A test
author needs no further design work: the **Test hook** line names the rows whose
expected-behaviour column this fills, and the red state that proves the check
discriminates.

**Zero deferrals.** Every gap is settled here. The two resolutions with the
largest blast radius — **A-26** (deletes the ADR's grant auto-stamp rule) and
**A-15** (changes `Shell::export_constant` on five arms) — are called out in
`## Deferred` so the owner can overturn either in one line without reading the
rest.

---

## Resolutions

**Decision 1 — private carrier, ledger, forgery**

### A-01 — Over cap emits a marker-only ledger, never an absent one

Closes: EC-LEDGER-006 (tightens EC-LEDGER-005)

**Question.** After an over-cap omission, how does the next prompt tell "over cap"
from "absent", and what stops it recomposing forever?

**Decision.** `Ledger::encode` MUST NOT omit the variable when the payload exceeds
16 KiB. It MUST instead emit a **marker-only ledger** — `{ v, fp, verdict,
over_cap: ["global","project"] }`, both scope payloads dropped, roughly 60 bytes —
and only omit the variable if even that fails to encode (unreachable in practice).
A scope named in `over_cap` is reconciled exactly as an absent scope: rebuild D
from truth, run the subtractive prefix repair, leave constants in place. One
abandonment line is emitted per transition into the over-cap state, not per
prompt. `ocx shell state` reports the state from the marker.

**Rationale.** `fp` lives *inside* the ledger (ADR:76), so omitting the variable
destroys the fingerprint too — the next prompt sees no recorded `fp`, recomposes,
re-overflows, omits again, and prints again, every prompt, for the life of the
shell. That is a worse outcome than the reporting gap the register found, and it
breaks the stat-only no-op budget (ADR:32, ADR:395). This is **one rule, not a
ladder** — the ADR's rejection of the three-rung ladder (ADR:73) stands; the
information loss is identical to what ADR:73 already accepted, and the fingerprint
is what is recovered.

**Test hook.** EC-LEDGER-006, EC-LEDGER-005. Red state: a fixture whose composed D
exceeds the cap must produce a **decodable** carrier with `fp` intact and no
`applied`; a build that omits the variable fails on the second prompt's recompose
count (asserted as 0 recomposes over five further prompts with a static watch set).

**Verdict.** Overrules EC-LEDGER-006 — its envelope tag `0` versions a *shape*
change on the *encoder* axis, which Decision 1's own two-versions rule (ADR:68)
forbids; an optional additive field on the existing schema needs no version move
at all. Adopts the register's underlying finding that D10's enumeration must stay
truthful.

### A-02 — `PATH` and `PATHEXT` are never constant-kind, on either direction

Closes: EC-LEDGER-007

**Question.** With neither scope declaring `PATH`, rule (b) has no operand and a
forged `Constant` + `priors[PATH]` restore fires. What closes it without requiring
D to be non-empty?

**Decision.** `reconcile::plan` MUST refuse `ModifierKind::Constant` for the keys
`PATH` and `PATHEXT`, compared through `EnvKey` so the refusal is
case-insensitive on Windows: such a desired entry is dropped with one warn-once
line and never applied, so `applied`/`priors` can never legally carry a constant
record for either key. On decode, a ledger entry recording `PATH`/`PATHEXT` as
`Constant`, or any `priors` entry for either key, MUST be discarded without
acting. The refusal lives in `plan` only — `ocx run` / `ocx exec` composition is
unchanged.

**Rationale.** The register's premise ("ocx never writes `PATH` as `Constant`") is
false today: `parse_env_value` accepts any key with any kind
(`crates/ocx_lib/src/project/env.rs`), package `Var.key` is a plain `String`, and
`apply_entries` maps `Constant => self.set` unconditionally
(`crates/ocx_lib/src/env.rs:593`). Enforcing the invariant at `plan` makes the
producer claim true, and independently stops a consented project from
wholesale-overwriting the user's interactive `PATH`.

**Test hook.** EC-LEDGER-007. Red state: the forged carrier
(`applied[PATH] = {type:"constant", value:<real PATH>}`,
`priors[PATH] = Value("/attacker/bin")`) against an empty D must yield an empty
`Plan.restores`; a build without the decode-side discard emits the restore.

**Verdict.** Overrules EC-LEDGER-007's mechanism (an asserted producer invariant),
adopts its outcome by enforcing that invariant at `plan`.

### A-03 — `dir` never gates a revert; the revert set is L-scoped, always

Closes: EC-LEDGER-009, EC-LEDGER-010

**Question.** Rule (a) voids the project scope on any `dir` mismatch, but every
project switch is a mismatch — so what does a mismatch mean?

**Decision.** `key` and `dir` are advisory identity labels: they MUST NOT
construct a path, MUST NOT re-grant consent, and MUST NOT gate a revert. A `dir`
equal to the walk's result means "same scope, no scope event"; **any** other value
— including a malformed or absent one — means the recorded scope has been **left**,
so `L.scopes.project.applied` becomes the revert set and is executed before the
newly discovered scope is applied (a switch is revert-then-apply in one pass). The
revert set is never intersected with D: a constant in L that D no longer names is
**reverted** under the `C == L.applied` guard, never discarded.

**Rationale.** ADR:158 already states the L-scoped rule and ADR:78's corrected rule
(c) states it from the other side; only rule (a)'s "invalidates the whole project
scope" and the Validation tier-1 bullet (ADR:427) still encode the superseded
discard. `applied` entries are self-describing (key, value, kind), so `dir` is not
needed to undo them.

**Test hook.** EC-LEDGER-009 (a `/p1`→`/p2` switch restores `JAVA_HOME`),
EC-LEDGER-010 (the tier-1 unit asserts revert, not discard). Red state: a `plan`
that voids the scope on mismatch leaves `JAVA_HOME=/a` inside `/p2`.

**Verdict.** Adopts EC-LEDGER-009 and EC-LEDGER-010.

### A-04 — Schema `v` is additive-only; a break carries a revert-read arm

Closes: EC-LEDGER-015

**Question.** A new binary reading an older `v` as "absent" destroys live `priors`
in every open terminal at once — is that acceptable?

**Decision.** `v` MUST be additive-only: new fields are optional, existing fields
are never removed, renamed or repurposed, so an addition never bumps `v`. If a
shape break is ever unavoidable, the releasing binary MUST ship a
**revert-read-only** arm for `v-1` in the same release — it deserializes `applied`
and `priors` only, runs one final revert/retirement pass, and re-emits in the new
shape. It never writes the old shape; there is no dual-write window. The
old-binary-reads-new direction is unchanged (unknown `v` ⇒ absent).

**Rationale.** ADR:75 specifies the degradation only for old-reads-new, and ADR:76
states that `priors` are the one datum nothing can reconstruct — losing them
fleet-wide on `self update` inflicts the `unset`-gesture cost without the user
asking for it.

**Test hook.** EC-LEDGER-015. Red state: a `v:1` fixture fed to a `v:2` decoder
must yield a revert plan; a decoder treating it as absent produces no `restores`.

**Verdict.** Adopts EC-LEDGER-015.

### A-05 — Set-but-empty is `Value("")`, never `Unset`

Closes: EC-CONST-008

**Question.** How is a variable that exists with an empty value captured and
restored?

**Decision.** Prior capture MUST use `std::env::var_os(key)`: `None` ⇒
`priors: Unset`; `Some(s)` ⇒ `priors: Value(s)`, **including `Some("")`**. Capture
reads set-ness, never truthiness — no `filter(|v| !v.is_empty())` and no
`unwrap_or_default` anywhere on that path. Reverting `Value("")` MUST emit that
arm's `Shell::export_constant(key, "")`, never `Shell::unset`. Named residual: on
Windows, `cmd.exe`'s `SET "K="` and PowerShell's `$env:K = ''` both delete the
variable, so `Value("")` and `Unset` collapse there — asserted at tier 1 on the
emitted string, not on the runtime effect.

**Rationale.** The two-variant `priors` shape (ADR:69) is only meaningful if
capture reads existence, and `Shell::unset` emits a genuinely different statement
per arm (`shell.rs:448-465`) that is not recoverable from an empty-value set.

**Test hook.** EC-CONST-008. Red state: `export JAVA_HOME=` before entering `/p1`,
then leaving — `[ -z "${JAVA_HOME+x}" ]` must be false; a truthiness-based capture
unsets it.

**Verdict.** Adopts EC-CONST-008, correcting its named failure mode (`env::var().ok()`
yields `Some("")` and is correct; the failures are `filter` and `unwrap_or_default`).

### A-06 — The carrier is trusted at the privilege level that set it

Closes: EC-PROC-013

**Question.** What survives when `__OCX_ENV_STATE` crosses a privilege boundary
via `sudo -E`?

**Decision.** No code change. The ADR MUST be corrected rather than annotated:
ADR:79's reasoning ("setting `__OCX_ENV_STATE` requires the ability to set an
arbitrary variable in the victim's shell — which is the ability to set `PATH`
directly") holds only among equal principals, and ADR:78 leans on it to close the
forged-`LD_PRELOAD` vector, so that closure is circular under `sudo -E`. State
plainly: *the carrier is trusted at exactly the privilege level of the process
that set it; ocx does not defend a privilege crossing, and authenticating the
carrier would not change that — any key an unprivileged forger can read is no key
at all.* Name the real mitigations: `sudo`'s `env_reset` / `secure_path` defaults
(which `-E` is a deliberate opt-out of), `unset __OCX_ENV_STATE` in the privileged
shell (Decision 1's own repair gesture), and `ocx shell state` as the way to see a
forged scope before it acts.

**Residual, at its true size.** Rule (c)'s constant arm restores `L.priors[k]`
under the `C == L.applied` guard, and the revert set is L-scoped and deliberately
**not** intersected with D (ADR:158) — so a forged entry naming a key D never
mentions is in the revert set, and the forger authors both operands of the guard.
Across `sudo -E` that is an **arbitrary-value-for-arbitrary-key** primitive
(`LD_PRELOAD`, `PYTHONPATH`, `GIT_SSH_COMMAND`), not "removal only".

**Rationale.** The only structural fix is an authenticated carrier, which
Decision 1 declines for a reason that remains sound in the same-principal case; a
blocklist of dangerous key names is incomplete by construction. Honest scoping
beats a guard that cannot hold. A-02 removes the `PATH`/`PATHEXT` case from this
primitive; the rest stands.

**Test hook.** EC-PROC-013 stays `manual-only`. The automatable half is
`rust-unit`: assert `plan` **does** produce that restore, so the documented
residual is pinned rather than assumed away and a future narrowing reds this test
deliberately.

**Verdict.** Adopts EC-PROC-013's "posture statement, not a code change";
overrules its blast-radius claim ("removal of an entry the forger claims to have
applied") as contradicted by ADR:158.

---

**Decision 3 — reconciler**

### A-07 — Apply is per kind: path prepends, list appends, project still wins

Closes: EC-LIST-008

**Question.** `ModifierKind::List` appends — so is "in front, in order" wrong, and
does global beat project for list-kind?

**Decision.** `plan` MUST route by kind, never by one rule:

| Kind | In-process | Emitted | Position |
|---|---|---|---|
| path | `utility::path::move_to_front` | `Shell::export_path` | front |
| list | `utility::list::append_unique` | `Shell::export_list(key, value, effective_sep)` | back, whole opaque contribution, never split into elements |
| constant | — | `Shell::export_constant` | overwrite |

Emission order stays **global first, project second** for all three kinds — do
**not** reverse it for lists. Precedence is unchanged in outcome (**project beats
global**) but is reached by three mechanisms: front position for path-kind, *last*
position for list-kind, overwrite for constants.

**Rationale.** `List` is documented and tested as move-to-**back**
(`crates/ocx_lib/src/package/metadata/env/list.rs:16-24`: "appended … re-applying
moves the contribution to the back … The consumer resolving duplicates last-wins
is what the direction serves"), so appending project last *is* project winning.
Reversing emission order would invert precedence and break parity with
`apply_entries`, which `ocx run` uses.

**Test hook.** EC-LIST-008. Red state: global and project both declaring `GOFLAGS`
list-kind — the resolved value must **end** with the project's contribution; a
`move_to_front`-for-lists build puts global's last.

**Verdict.** Adopts EC-LIST-008's diagnosis; overrules its "emit list scopes in
reverse" alternative, because last-wins consumer semantics already deliver project
precedence.

### A-08 — The ledger stores the *effective* separator; `None` means path-kind only

Closes: EC-LIST-007

**Question.** `remove_list_element(.., None)` means the platform PATH separator,
but a list entry that survived `reconcile_list_separators` with `None` is applied
with `" "`.

**Decision.** `LedgerEntry.separator` MUST hold the **effective** separator — the
post-`reconcile_list_separators` value with the kind's default already applied — so
for `kind == List` it is **always `Some`**, defaulting to
`package::metadata::env::list::DEFAULT_SEPARATOR` (`" "`). List-kind reverts always
call `Shell::remove_list_element(key, value, Some(effective))`. `None` is reserved
for path-kind and continues to mean `env::PATH_SEPARATOR`. `plan` resolves the
effective separator once, at record time, never at revert time.

**Rationale.** `reconcile_list_separators` deliberately leaves `separator: None` on
a key nobody established (`crates/ocx_lib/src/env.rs:945-952`) and `apply_entries`
then folds with `DEFAULT_SEPARATOR = " "`
(`crates/ocx_lib/src/package/metadata/env/list.rs:12`), so a revert reading `None`
as `:` splits on the wrong byte and the contribution is permanently unremovable.

**Test hook.** EC-LIST-007. Red state: a `GOFLAGS` list entry with no declared
separator — the emitted removal must carry `" "` flanks; a `None`-preserving build
emits `:` flanks.

**Verdict.** Adopts EC-LIST-007.

### A-09 — Ownership is component-wise; the removal operand comes from C

Closes: EC-LIST-009, EC-PATH-015

**Question.** Does `str::starts_with` claim `/home/u/.ocx-backup/bin`, and how is a
prefix-owned element removed when C spells it with a trailing slash?

**Decision.** The prefix test MUST be `Path::starts_with` against an `$OCX_HOME`
canonicalized once at reconcile start — a component boundary, never a byte prefix —
so `.ocx-backup` and `.ocxevil` are foreign. Retirement MUST enumerate the
segments **as they appear in C**, select the prefix-owned ones absent from D, and
emit each removal naming that observed segment verbatim, so selection and removal
share one operand and stay byte-exact. Removal stays byte-exact in every arm; no
prefix-matching removal primitive is added.

**Rationale.** `utility::path::remove_segment`'s own doc says a segment naming the
same directory by a different string survives untouched and "this function is not
a containment check"; drawing the operand from C closes that without inheriting
PowerShell's substring over-match or fish's index-shift hazard (ADR:142).
`plan`'s `owned_prefixes: &[&Path]` parameter (ADR:375) already carries the
prefixes as `Path`s, and `plan` already receives `current: &Env`.

**Test hook.** EC-LIST-009 (a foreign `.ocx-backup/bin` survives a corrupt-carrier
repair), EC-PATH-015 (a trailing-slash owned element is removed, count reaches
zero). Red state: a `str::starts_with` build deletes `.ocx-backup/bin`; a
D-sourced-operand build leaves `…/bin/` on PATH.

**Verdict.** Adopts EC-LIST-009; overrules EC-PATH-015's prefix-removal
recommendation, because operand-from-C achieves the same with no new per-arm
primitive and no over-match risk.

### A-10 — `L ⊆ emittable(D)`: `plan` drops what no arm can emit or revert

Closes: EC-REC-006, EC-LIST-010 (with A-17 and A-20)

**Question.** What happens to a desired entry whose key fails `is_valid_env_key`,
whose path-kind value embeds the platform separator, whose value is empty, or
whose value contains a newline?

**Decision.** Before anything reaches `Plan` or `L`, `plan` MUST drop — each with
one warn-once line:

1. any entry whose key fails `env::is_valid_env_key`;
2. any path-kind entry whose value contains `env::PATH_SEPARATOR`;
3. any list or path element whose value is the empty string;
4. any list or path element whose value contains LF or CR.

`L ⊆ emittable(D)` is thereby an invariant, not an accident. Independently,
`parse_env_value` MUST reject a path-kind value containing `PATH_SEPARATOR` at the
same boundary that already raises `ProjectErrorKind::EnvSeparatorEdgedValue`, so
an authoring mistake is caught with a scope/key message at exit 65. `ocx run` /
`ocx exec` composition is untouched by the `plan`-side drops — the gate is
reconciler-local, and the read path for already-published packages keeps
resolving.

**Rationale.** Every emitter returns `None` for an invalid key
(`shell.rs:154-158`, `:319-327`, `:427-431`, `:448-452`), so an apply silently
emits nothing while L would carry a key no arm can ever remove. `export_path`'s
stated precondition is "a single directory with no embedded `PATH_SEPARATOR`",
which `parse_env_value` never checks — the split-based arms insert two segments and
remove neither. Empty and newline values are handled in A-17 and A-20.

**Test hook.** EC-REC-006 (`2FOO`, `A-B` never appear in the encoded ledger),
EC-LIST-010 (`PATH = {type="path", value="a:b"}` refused at parse; a
metadata-sourced one dropped by `plan`). Red state: a build without the gate
encodes the key and emits no removal.

**Verdict.** Adopts EC-REC-006 and EC-LIST-010, extending EC-LIST-010 with the
`plan`-side drop.

### A-11 — An indeterminate walk retains the scope; a determinate one reverts

Closes: EC-SCOPE-006, EC-FS-013, EC-FS-014, EC-FS-015

**Question.** A transient `.git` EIO, an `ENAMETOOLONG`, an unlinked CWD, or the
filesystem root each make the walk return `None` — must the project be torn down?

**Decision.** Before reverting a project scope on a walk that produced no hit or a
different hit, the reconciler MUST run one determinacy check: if
`L.scopes.project.dir` is still an ancestor-or-self of the CWD **and**
`symlink_metadata(dir/ocx.toml)` reports a regular file, the walk's answer is
**indeterminate** — retain the scope unchanged and emit nothing. A genuine leave
(CWD outside `dir`), a genuine deletion (`NotFound`), and `OCX_NO_PROJECT=1` all
fail that check and revert normally. Additionally:

- A `std::env::current_dir()` failure (the CWD itself unlinked) MUST degrade to
  "no project resolved this prompt", log at debug, exit 0, and MUST NOT fall back
  to a cached CWD.
- An `ENAMETOOLONG`-class probe error needs **no new branch**: `has_git_dir`
  already fails closed to "boundary" on any non-`NotFound` I/O error
  (`config/loader.rs:704-716`), which stops the ascent at that level.
- The walk's termination at the filesystem root needs **no special case**:
  `walk_for_project_file`'s `current.parent()` match already returns `None`
  (`config/loader.rs:688-692`). Assert it; do not add code.

**Rationale.** `walk_for_project_file` collapses a non-`NotFound` candidate error,
a fail-closed `.git` boundary and a real miss into the same `None`, so the
reconciler cannot distinguish them from the return value. One extra `stat` on the
**revert** path — never on the no-op path — is cheaper than plumbing a tri-state
through a loader every command shares, and it matches Decision 2's own posture
("an indeterminate probe retains the stamp", ADR:126). The hook's exit-0 contract
is ADR:31 and ADR:383.

**Test hook.** EC-SCOPE-006 (`chmod 000 /p1/.git` leaves `$JAVA_HOME` untouched
across the next two prompts — a build without the guard flaps it), EC-FS-015
(`rm -rf` the CWD ⇒ exit 0, no stderr; red state = propagating `NotFound` as
`Error::Io`/74), EC-FS-014 (a near-`PATH_MAX` path ⇒ `Ok(None)`, not `Err`; red
state = flip `has_git_dir`'s non-`NotFound` arm to `false`), EC-FS-013
(bounded-timeout walk test; red state = an injected off-by-one on the `None` arm
times out or panics).

**Verdict.** Adopts EC-SCOPE-006, EC-FS-013 and EC-FS-015; adopts EC-FS-014's
outcome and overrules its framing — it is a test-and-document gap, not an
implementation gap.

### A-12 — A symlinked candidate promotes the ancestor, silently, and says so on demand

Closes: EC-SCOPE-007

**Question.** A symlinked `/work/proj/ocx.toml` promotes `/work` and, the register
claims, warns every prompt.

**Decision.** No logging change. The hook discards the binary's stderr
unconditionally (A-21), so the loader's `log::warn!` never reaches the prompt. The
promotion itself stands: the walk's symlink rejection is the shipped discovery
contract and MUST NOT be special-cased for the reconciler. `ocx shell state` MUST
add a reason row naming the skipped symlinked candidate and the ancestor project
activated instead, and MUST name `--project` / `OCX_PROJECT` as the opt-in.

**Rationale.** `walk_for_project_file` skips a symlinked candidate and continues
upward with a warn (`config/loader.rs:651-657`); silence at the prompt plus
visibility on demand reaches the register's goal without changing log levels for
every other caller of a shared loader.

**Test hook.** EC-SCOPE-007. Red state: assert `ocx shell state` names
`/work/proj/ocx.toml` as skipped while the per-prompt streams are empty; a build
without the reason row prints nothing anywhere and the user has no path to the
answer.

**Verdict.** Adopts EC-SCOPE-007's second half; overrules its first half (demote
loader warnings) as unnecessary and as a behaviour change for non-hook callers.

### A-13 — The consent inputs join the watch set — as recorded paths, not a config read

Closes: EC-FP-008, EC-CFG-012

**Question.** A grant written to `config.toml`, or exported as `OCX_CONSENT_*`,
never expires the cached `verdict: "inert"`.

**Decision.** The watch set MUST gain:

1. the **config-tier paths recorded in the ledger at compose time** — home
   `config.toml`, user `config.toml`, and the `OCX_CONFIG` / `--config` override if
   one was in effect — each with presence, mtime and size, so a tier file that did
   not exist becomes a change when created;
2. the project's consent stamp `state/projects/<key>/consent.json`.

The raw values of `OCX_CONSENT_PATHS` and `OCX_CONSENT_NAMESPACES` MUST be folded
into `fp` (two `getenv`s, no I/O). The per-prompt path stats the **recorded** list
and reads **no config**: path discovery happened during the last `ConfigLoader`
pass. The added paths join the same shell-side newer-than short-circuit as the
existing members.

**Rationale.** ADR:279 forbids *parsing* config per prompt, not stat'ing it —
`ocx.toml` is already stat'ed on the identical path — and taking the tier paths
from the ledger rather than re-deriving them is what keeps that discipline
absolute. Without this the `inert` cache (ADR:281) is unexpirable and a grant added
mid-session never takes effect until the shell restarts. Cost: ~3-4 stats and 2
`getenv`s on a path already doing ~6.

**Test hook.** EC-FP-008, EC-CFG-012. Red state: with the shell parked in an inert
clone, adding the `paths` grant from another terminal must activate at the next
prompt, and exporting `OCX_CONSENT_PATHS` must activate at the next prompt — a
build with the shipped watch set stays inert in both, and a build that parses
config per prompt fails the NFR latency gate.

**Verdict.** Adopts EC-FP-008 and EC-CFG-012, specifying the ledger-recorded-paths
mechanism that keeps ADR:279 intact.

### A-14 — The fast-path ceiling is granularity-free, and so is its test

Closes: EC-FP-002

**Question.** Does the mtime+size ceiling widen on FAT/exFAT/NFS, and does
anything change?

**Decision.** No mechanism change — the ceiling stands as accepted (ADR:166). Two
rules make it precise: the fingerprint MUST compare the full
`std::fs::Metadata::modified()` `SystemTime`, never a seconds-truncated value, so
the window is the filesystem's own granularity and nothing coarser; and the
ceiling MUST be stated granularity-free as *"an unchanged `(mtime, size)` pair is
invisible"*, with FAT/exFAT (2 s) and NFS (1 s) named as widening it and Windows
named as a first-class host for them. Any test of the ceiling MUST force the
collision by explicitly setting the mtime back to the recorded value, never by
writing quickly.

**Rationale.** A test that races the clock is green on ext4 for the wrong reason
and red on a slow runner — the "green that cannot be told from never ran" class
the ADR's Validation section commits to refusing. Reading the full `SystemTime` is
free and strictly narrows the window on ext4/NTFS/APFS.

**Test hook.** EC-FP-002 (documented residual); de-flakes EC-FP-001. Red state: the
forced-mtime fixture must show the write going unnoticed, proving the assertion
observes the ceiling rather than the clock.

**Verdict.** Adopts EC-FP-002, adding the full-`SystemTime` rule and the
forced-mtime test construction.

---

**Per-shell emission (Decision 3 idiom hazards, Component Contracts `Shell`)**

### A-15 — The constant emit uses that arm's own escaper and quoting context

Closes: EC-QUOTE-009, EC-CONST-010, EC-QUOTE-006

**Question.** Which escaper does `export_constant` use per arm, and is its output
byte-identical to what `Env::apply_entries` sets in-process?

**Decision.** Change `Shell::export_constant` (`shell.rs:427-441`) so every arm
uses that arm's `export_path` escaper and quoting context:

| Arm | Escaper | Emit |
|---|---|---|
| `Ash \| Ksh \| Dash \| Bash \| Zsh` | `escape_posix_single_quoted` | `export {key}='{value}'` |
| `PowerShell` | `escape_single_quoted_doubled` | `$env:{key}='{value}'` |
| `Elvish` | `escape_single_quoted_doubled` | `set E:{key} = '{value}'` |
| `Fish` | `escape_value` | unchanged |
| `Nushell` | `escape_value` (as reduced by A-16) | unchanged |
| `Batch` | `escape_value` | unchanged, subject to A-20 |

Delete `escape_value`'s `Ash \| Ksh \| Dash \| Bash \| Zsh` arm
(`shell.rs:484-489`) — after this change it has no caller — together with its
`!` → `\!` replacement and the tests that lock it in. The reconciler's
constant-restore path emits through this same `export_constant`, and an `Unset`
prior through `Shell::unset` (`shell.rs:448-461`): **no second escaper is minted
anywhere in the reconciler.**

**Rationale.** `escape_value`'s `!` → `\!` is a byte-corruption, not a hardening —
its own comment concedes `\!` is *literal* in double quotes, which is exactly the
defect: the shell sets `a\!b` where `apply_entries` (`env.rs:594`) stores `a!b`
verbatim, so Decision 3's `C == L.applied` exit guard can never hold for a
`!`-bearing constant and the variable is treated as a foreign override forever.
Measured on this host, the backslash is retained in **every** case — non-interactive
dash/ksh/bash/zsh *and* interactive bash with `histexpand on` — so the divergence
is unconditional, not interactive-only. Elvish is the same defect by another
mechanism: `escape_value` emits `\$`/`` \` `` into a double-quoted string, which
elvish rejects as an invalid escape sequence — the fact ADR:63 already cites to
justify base64url for the carrier, and which `export_path`'s Elvish arm already
avoids.

**Test hook.** EC-QUOTE-009 / EC-CONST-010: a per-arm parity test asserting the
emitted line, evaluated by a live shell, yields bytes equal to `apply_entries`'
value over a fixture set including `!`, `$`, backtick, `\`, `"`, `'`, `%`, LF.
Red state: revert the POSIX arm to `escape_value` and the `!` case reds with
`\!` ≠ `!`. EC-QUOTE-006: `Shell::Elvish.export_constant("JAVA_HOME", "/opt/j$dk")`
run through a live elvish exits 0; red state = the current `"…\$…"` emit exits
non-zero with a parse error.

**Verdict.** Adopts EC-QUOTE-009, EC-CONST-010 and EC-QUOTE-006; corrects
EC-QUOTE-009's causal claim (the corruption is unconditional, not an
interactive/non-interactive divergence).

> **Amended 2026-08-26 — shipped in full.** The escaper change landed with the addendum;
> the elvish half of the test hook landed later, with #349. `PARITY_ARMS` now carries
> `["elvish", "-c"]` and its `seed_string` / `read_string` arms, so
> `live_export_constant_matches_apply_entries` runs EC-QUOTE-006's
> `Shell::Elvish.export_constant("JAVA_HOME", "/opt/j$dk")` through a real elvish — proven
> red by emitting one byte too many from the elvish arm, which fails with
> `argv=["elvish", "-c"]`. The silence that made the gap invisible is closed too:
> `assert_every_present_interpreter_ran` fails when an installed interpreter ran nothing,
> and `every_hook_shell_has_a_parity_arm` anchors the matrix on the `Shell` enum rather
> than on itself.

### A-16 — Nushell emits plain strings, so its escaper drops the interpolation cases

Closes: EC-QUOTE-007

**Question.** Does the Nushell escaper match the quoting context the Nushell emits
actually use?

**Decision.** All three Nushell emits use a **plain, non-interpolating**
double-quoted literal (`shell.rs:238`, `:412`, `:439`). `escape_value`'s Nushell
arm (`shell.rs:511-516`) MUST therefore be reduced to `\` → `\\` and `"` → `\"`
only; the `$`, `(` and `)` replacements are deleted. The arm's doc comment
(`shell.rs:503-510`), which claims `export_path` uses `$"..."` interpolation, MUST
be rewritten — it has been stale since that arm was written. If any Nushell emit
ever adopts the `$"..."` form, that emit gets its own escaper; the plain-string
escaper is never reused there.

**Rationale.** The `export_path` Nushell arm's own comment states the contract:
*"Plain (non-interpolating) double-quoted literals, so `$`/`(` cannot fire"*
(`shell.rs:233-234`). Escaping them is therefore unnecessary for safety, and it is
*corrupting* unless nushell recognises `\$`, `\(` and `\)` as escapes in the plain
form. That escape table could not be verified on this host (no `nu` binary — and
`run_script` returns `None` when the interpreter is absent, `shell.rs:1293`, so
every live-nu assertion in the tree currently skips silently). Emitting no
backslash is correct under **both** readings; emitting one is correct under only
one. Choose the reading that does not depend on an unverified table.

**Test hook.** EC-QUOTE-007: a live-nu byte round-trip for `/tmp/a(b)$c\d"e'f`
through `export_path`, `export_list` and `export_constant`. Red state: reintroduce
`$` → `\$` and the round-trip reds if the escape is unrecognised — which is the
point of running it live. **Blocking prerequisite:** `nu` and `elvish` MUST be
present in the shell-zoo image and their absence MUST fail the job, never skip it;
until then every nu/elvish green in `shell.rs` is indistinguishable from "never
ran".

**Verdict.** Adopts EC-QUOTE-007's recommendation (plain-string escaper) on the
fail-safe grounds above, and adopts the reviewing worker's independent finding
that the stale doc comment is the underlying defect.

> **Amended 2026-08-26 — shipped in full, prerequisite included.** EC-QUOTE-007's three
> emits are now all live-nu: `export_list` through
> `live_nushell_list_matches_the_in_process_fold`, `export_path` and `export_constant`
> through the `["nu", "-c"]` entry in `PARITY_ARMS`. Proven red by emitting one byte too
> many from the nushell constant arm, which fails with `argv=["nu", "-c"]`. The blocking
> prerequisite is met on both sides: `verify-basic.yml`'s unit-test leg installs nu and
> elvish through `ocx package env` and sets
> `__OCX_TESTING_REQUIRE_LIVE_SHELLS=nu,elvish`, so an absent interpreter fails that leg;
> and `test/taskfile.yml` derives its list from the zoo image, arming pwsh, nushell and
> elvish on the Debian leg that installs them while the Alpine leg keeps the apk set.

### A-17 — `export_path` refuses an empty value

Closes: EC-QUOTE-005

**Question.** What does `export_path` emit when the value is the empty string?

**Decision.** Give `Shell::export_path` the empty-value guard `export_list`
already has (`shell.rs:331-333`): before the match, return
`Some(self.comment(format!("ocx: {key} path entry is empty, nothing to prepend")))`
when the raw value is empty. `plan` additionally drops empty elements (A-10.3), so
the ledger never records an element no arm emits.

**Rationale.** Measured: `Shell::Bash.export_path("PATH", "")` against ambient
`/a:/b` yields `:/a:/b` — a leading empty segment, which POSIX resolves as the
current working directory. The POSIX-awk, fish, PowerShell, elvish and nushell
arms produce the same leading empty by construction; the Batch arm is worse
(`SET "PATH=;%PATH:;=%"` deletes *every* separator, collapsing PATH to one
segment). `utility::path::move_to_front` already refuses to prepend an empty
value, so in-process and emitted disagree today. Reachable from
`[env] PATH = { type = "path", value = "" }` and from any interpolation resolving
to empty. CWD-on-PATH is a privilege-escalation primitive, not a formatting nit.

**Test hook.** EC-QUOTE-005: every arm returns the comment for `""`; the parity
test asserts the emitted result equals `move_to_front(ambient, "")`. Red state:
remove the guard and the bash arm reds with `:/a:/b` ≠ `/a:/b`; the live-bash leg
confirms the leading `:`.

**Verdict.** Adopts EC-QUOTE-005.

### A-18 — bash/zsh `export_path` collapses empty segments to a fixpoint

Closes: EC-PATH-004

**Question.** Which side is normative when bash/zsh preserve an ambient empty
segment and the other six implementations strip it?

**Decision.** `utility::path::move_to_front` is normative. Add a second fixpoint
loop to the `Bash | Zsh` arm of `export_path` (`shell.rs:172-177`), between the
value-removal loop and the leading/trailing strip, in the idiom the arm already
uses: `while [ "$KEY" != "${KEY//::/:}" ]; do KEY="${KEY//::/:}"; done`.

**Rationale.** Measured: ambient `/a::/b`, value `/opt/bin` — the current emit
yields `/opt/bin:/a::/b` while `move_to_front` yields `/opt/bin:/a:/b`. The
POSIX-awk arm's `$0!=""`, fish's list filter, PowerShell's
`Where-Object {$_ -and …}`, elvish's `not-eq $p ""` and nushell's `where {$p != ""}`
all strip. Six arms and the in-process path agree; bash/zsh are the outlier, and
under a per-prompt reconciler the surviving empty segment is re-asserted every
recompose. The wrapped-empty-ambient case still yields the bare value, so
`live_bash_empty_path_has_no_separators` stays green.

**Test hook.** EC-PATH-004: parity unit test, bash/zsh emit evaluated live versus
`move_to_front`, ambient `/a::/b` and `::`. Red state: drop the collapse loop and
the `/a::/b` case reds.

**Verdict.** Adopts EC-PATH-004.

### A-19 — One PATH-element comparison rule, and the ledger stores what was written

Closes: EC-PATH-001, EC-PATH-008, EC-PATH-010

**Question.** How is a PATH element compared, across three implementations, two
platforms and a quoted-segment spelling — and what does the ledger store so
`C == L.applied` stays decidable?

**Decision.** One rule. A PATH element is compared **segment-exact**, after **one
normalisation** (strip a single surrounding pair of `"`), **case-sensitively on
Unix and ASCII-case-insensitively on Windows** — in `utility::path::move_to_front`
and `remove_segment`, in every `Shell::export_path` arm, and in every
`remove_list_element` arm. Because the emitter and the shell it emits for run on
the same host, each arm selects its comparison at **emit time** under
`cfg!(windows)`, exactly as `env::PATH_SEPARATOR` already does. Concretely:

- the PowerShell arm (`shell.rs:212`) replaces `-ne` with
  `[String]::Equals($_, $__ocx_p, [StringComparison]::Ordinal)` on non-Windows and
  `OrdinalIgnoreCase` on Windows;
- the PowerShell `-split` pipeline trims one leading and one trailing `"` per
  segment before comparing;
- Batch needs no change — `%VAR:search=%` is case-insensitive and Batch exists only
  on Windows, where case-insensitive is the correct answer.

**The ledger stores the exact bytes ocx wrote (Invariant L-1 unchanged); the
*comparison* is normalised, never the stored string** — `C == L.applied` is
evaluated with this predicate, not with `==`. **The emitted key is never
re-cased**: `EnvKey`'s ASCII fold (`env.rs:296-320`) governs the in-process map
only, and the emit uses the key's authored spelling, which is what makes
`$env:Path` versus `$env:PATH` correct on Linux (PowerShell#3571).

**Rationale.** Measured with pwsh 7 on Linux: ambient `/opt/Bin:/x`, value
`/opt/bin` — the shipped `-ne` emit returns `/opt/bin:/x`, silently deleting a
genuinely different directory; the ordinal form returns `/opt/bin:/opt/Bin:/x`.
`export_list` already reached ordinal .NET methods for exactly this reason
(`shell.rs:375-386`); `export_path` did not. `std::env::split_paths` unquotes on
Windows, so the in-process side already sees `C:\Program Files\b` where the
emitted split sees the quoted form — one strip on the emitted side is the whole fix.

**Test hook.** EC-PATH-001 (live pwsh on Linux, `/opt/Bin` survives; red state =
the shipped `-ne` emit), EC-PATH-008 (three-way parity table over `C:\Opt\Bin` +
`C:\opt\bin` asserting one answer per platform; red state = leave `move_to_front`
case-sensitive on Windows), EC-PATH-010 (Windows-leg fixture with a quoted ambient
segment; red state = drop the quote-strip).

**Verdict.** Adopts EC-PATH-001 and EC-PATH-010; adopts EC-PATH-008's platform
rule but **overrules its second clause** ("record the normalisation in the
ledger") — normalising the stored string breaks Invariant L-1 and makes the
constant-restore path emit a wrongly-cased directory. Normalise the comparison,
not the storage.

### A-20 — The Batch contract: one precondition, three refusals, a `%`-only escaper

Closes: EC-QUOTE-010, EC-QUOTE-011, EC-QUOTE-004

**Question.** What exactly does the Batch arm promise, given it stays shipped and
reachable via `ocx --format json --global env`?

**Decision.**

1. `Shell::export_path` and `export_constant` return **`None`** on the `Batch` arm
   when the value contains `%`, LF or CR — the `export_list` precedent of emitting
   nothing rather than something that grows or splits.
2. `escape_value`'s Batch arm (`shell.rs:520-526`) is reduced to `%` → `%%` only;
   the `^`/`&`/`<`/`>`/`|` caret escapes are **deleted**, because both Batch emits
   wrap the value in `SET "KEY=…"` where cmd does not process those characters, so
   the carets survive into the value verbatim and corrupt it.
3. `plan` drops any element containing LF or CR on **every** platform (A-10.4), so
   a ledger inherited by a cmd session never names an element cmd cannot emit.
   Every non-Batch arm carries an LF correctly (POSIX/PowerShell/elvish inside a
   single-quoted literal, fish and nushell inside a double-quoted one).
4. The Batch arm's doc comment states, and a unit test asserts, the
   **delayed-expansion-off precondition**: the emit is correct only under cmd's
   default; under `cmd /v:on` or after `setlocal EnableDelayedExpansion`, a
   `!`-bearing value is consumed as a variable reference and the segment is
   truncated. Nothing in ocx controls the consuming `.bat`'s `setlocal` and no
   spelling is correct under both states, so this is a **named ceiling**, not a bug
   to fix.

Batch stays `None` for `remove_list_element` and hosts no prompt hook, so the
reconciler is unaffected by all four.

**Rationale.** `%VAR:search=%` has no escape for a literal `%` in `search`, so a
`%`-bearing value's delete half never matches and every apply prepends another
copy — unbounded growth under a per-prompt reconciler, the same failure mode that
already made `export_list` return `None`. The emit is consumed both by
`call file.bat` and by `FOR /F … DO @%i` (the arm's own doc comment), and `FOR /F`
executes each line as a separate command — so an LF silently splits one `SET` into
two, and `%%` means `%` in the first channel and `%%` in the second.

**Test hook.** EC-QUOTE-010 (`Shell::Batch.export_path("PATH", r"C:\a%b\bin").is_none()`,
plus a Windows-leg proof that the pre-change emit grows on every re-source; red
state = restore the emit), EC-QUOTE-004 (Batch returns `None` for an LF value;
every other arm emits one statement whose live evaluation puts the LF-bearing
directory first), EC-QUOTE-011 (Windows leg runs the same emit under `cmd /v:on`
and asserts the documented truncation, so the ceiling is demonstrated; red state =
run it under the default and the truncation assertion reds).

**Verdict.** Adopts EC-QUOTE-010, EC-QUOTE-011 and EC-QUOTE-004; extends
EC-QUOTE-010 with the caret over-escaping it did not notice.

### A-21 — Diagnostics: shell code that prints, escaped as an argument, never at startup

Closes: EC-REC-003, EC-QUOTE-015, EC-HOOK-010

**Question.** stdout is `eval`'d and the reconcile call's stderr is discarded — where
does `ocx: +JAVA_HOME ~PATH -PYENV_ROOT (acme, lock a1b2c3)` go, how is it escaped,
and when may it fire?

**Decision.**

1. **Channel.** The summary line, the inert-project hint, the over-cap
   abandonment line, the direnv/mise yield line and Decision 7's
   `[shell.consent]`-strip reason MUST all be emitted as **shell code on stdout
   that prints to stderr when eval'd**, via one new per-arm primitive
   `Shell::emit_message(text) -> Option<String>`. Batch returns `None` — it hosts no
   hook. Nothing user-visible travels on the binary's stderr.
2. **Escaping.** Every emitted diagnostic passes **the same escaper as a value on
   that arm** (`escape_posix_single_quoted` for bash/zsh/ash/ksh/dash,
   `escape_single_quoted_doubled` for PowerShell and elvish, `escape_value` for
   fish and nushell) and rides as a **format argument, never as the format
   string**: the POSIX arms emit `printf '%s\n' '<escaped>' >&2`. Passing the
   message as the format string is a second defect the escaper does not cover — a
   `%` in a project path would be consumed as a conversion spec.
3. **The stderr discard stays unconditional.** `2>/dev/null` on the reconcile call
   is applied at invocation time, before the exit status that would distinguish an
   unknown-flag clap error from a normal run is knowable, so it cannot be scoped.
4. **The startup path emits no diagnostics at all.** Every message is deferred to
   the first `--reconcile` run. The corollary that makes this reliable is
   normative: **the first prompt of every shell always reconciles**, because layer
   2's mtime fast path has no recorded fingerprint to compare against and "no
   record" counts as changed. Where the hook is disabled entirely, `ocx about` and
   `ocx shell state` are the only channels — stated, not silently lost.

**Rationale.** ADR:298 already establishes `printf … >&2` inside the eval'd script
as the only reliable channel (the shims discard the binary's stderr,
`shims.rs:63,103,105,146,239`); the ADR is not self-contradictory, it simply never
says the summary uses it, and no `Shell` primitive emits one (`Shell::comment`
produces a comment, which prints nothing). An unescaped `'` in a project path such
as `/home/u/it's work/proj` closes the argument and parses the remainder as shell
source. On (4): powerlevel10k treats *any* console output during zsh
initialisation as an error, disables instant prompt and warns on every subsequent
shell start — and pwsh's `$ErrorActionPreference='Stop'` (A-22) is the same class.
Sniffing `POWERLEVEL9K_INSTANT_PROMPT` fixes one consumer of an open-ended class;
deferring the message by one prompt removes the class.

**Test hook.** EC-REC-003 (the emitted stdout of a change-producing reconcile
contains a printf-to-stderr statement carrying the summary; red state = written
with `log::info!`, stdout lacks it), EC-QUOTE-015 (a project path containing `'`,
`%` and LF produces exactly one statement per arm and prints byte-exactly; red
state = pass the message as the format string and the `%` case reds), EC-HOOK-010
(tier-3 with p10k installed: zero bytes on stderr before the first prompt, message
present at the first prompt; red state = emit one startup diagnostic and p10k's
"console output during zsh initialization" warning appears).

**Verdict.** Adopts EC-REC-003's channel and EC-QUOTE-015 (extended with the
format-argument rule); overrules EC-REC-003's "scope the stderr discard" half (the
discard is decided before the failure it guards is observable) and EC-HOOK-010's
mechanism (a per-tool sniff is unbounded maintenance for a deferrable message).

### A-22 — The pwsh hook runs under its own error preferences and restores `$?`

Closes: EC-EMIT-008

**Question.** What makes the pwsh `prompt` wrapper safe under a hardened user
profile?

**Decision.** The emitted PowerShell hook body MUST be wrapped in
`try { … } catch { }` in full; the reconcile invocation MUST additionally set
`$ErrorActionPreference = 'Continue'` and
`$PSNativeCommandUseErrorActionPreference = $false` in the hook's own scope,
restoring both in a `finally`. The hook MUST capture `$?` and
`$global:LASTEXITCODE` on entry and restore both on exit — the same
`$?`-preservation rule Decision 5 layer 1 already states for bash's
`PROMPT_COMMAND`.

**Rationale.** With `$PSNativeCommandUseErrorActionPreference = $true` and
`$ErrorActionPreference = 'Stop'` — a common pwsh 7.3+ hardening pair — a native
command that writes to stderr or exits non-zero raises a **terminating** error, and
`2>$null` does not prevent it. That turns the older-binary case (a clap
unknown-flag error after a rollback) into a prompt that throws on every render:
precisely the "hook breaks a prompt" outcome ADR:291's discard-and-ignore rule
exists to prevent and which that rule alone does not achieve.

**Test hook.** EC-EMIT-008: tier-3 pwsh-on-Linux with both preferences set in the
profile and `current` pointing at a binary that rejects `--reconcile` — assert the
prompt renders and `$?` survives. Red state: drop the `try`/preference scope and
the prompt render throws.

**Verdict.** Adopts EC-EMIT-008, extended with the `$?`/`$LASTEXITCODE`
preservation it did not name.

---

**Decision 6 — regeneration and the nushell channel**

### A-23 — `Plan` carries a structural `v`; the nu applier gains a `list` arm and an unknown-`type` skip

Closes: EC-VER-003, EC-NU-006

**Question.** How does the `Plan` JSON survive a binary-version boundary into a nu
body that lags one `self update` hop, and what does that body do with a `type` it
does not know?

**Decision.**

1. `Plan`'s JSON wire shape MUST carry a top-level `"v": 1`, on the same envelope
   discipline as the ledger. The nu consumer applies one rule: **`v` absent or
   unrecognised ⇒ apply nothing this prompt and return silently** — no error, no
   partial apply. `v` is **structural only**: it bumps on a breaking reshape, never
   on an added field, and unknown fields inside a recognised `v` are ignored,
   matching this repo's no-`deny_unknown_fields` read-path doctrine. Additive
   `type` values are handled by (2), not by a `v` bump, so nushell never freezes
   for a hop over an additive change.
2. `NU_ENV_APPLY_LOOP` (`setup/shims.rs`, inlined verbatim into `ENV_NU` and into
   the vendor autoload body) MUST become a **four-way** dispatch: `type == "path"` ⇒
   the existing move-to-front prepend; `type == "list"` ⇒ the `export_list` fold
   using the entry's effective separator; `type == "constant"` ⇒ `load-env`;
   **anything else ⇒ skip, applying nothing**. All four use only pre-0.101-stable
   nu features, per the constraint the const's own doc comment already pins.

**Rationale.** The shipped loop is a two-way branch,
`if type == "path" { … } else { load-env … }`. `ModifierKind` is already
`Path | Constant | List` and `EnvEntry` serialises `type: "list"`, so today's
`else` arm applies a **list** entry as a constant — overwriting a separator-joined
variable instead of folding into it. That is a live defect on a kind that ships,
not only the forward-compat hazard the register describes, and the same arm would
apply a future fourth `ModifierKind` (deferred [#265](https://github.com/ocx-sh/ocx/issues/265))
as a raw constant.

**Test hook.** EC-NU-006 (the emitted body text contains an explicit unknown arm;
tier-2 cross-shell test where a bash session writes a carrier with a synthetic
future `type` and the nu session applies nothing for it and everything else
normally — red state = restore the two-way branch and the synthetic entry lands as
a constant; separately, a `type: "list"` entry must fold, not overwrite — red state
= today's `else` arm), EC-VER-003 (two builds with different `Plan` `v`, `current`
swapped mid-session: the nu prompt is a silent no-op and recovers after swapping
back; red state = omit the `v` gate and the mismatched shape throws or partially
applies).

**Verdict.** Adopts EC-VER-003; widens EC-NU-006's fix from "add an unknown arm"
to "add a `list` arm **and** an unknown arm", because the `else` fall-through is
already wrong for a kind that ships today.

### A-24 — The nushell PWD hook is appended, and every level of the path is defaulted

Closes: EC-NU-004

**Question.** How is the PWD hook installed without clobbering a user hook or
erroring on a config that has no `hooks` key?

**Decision.** In `ENV_NU` / the vendor autoload body, install the hook by
**appending with `++`** onto
`($env.config.hooks?.env_change?.PWD? | default [])` and assigning the result
back — never `=` onto `.PWD`, and never `$env.config.hooks = { … }`. Every
intermediate level MUST be defaulted (`| default {}`) so a `nu -n` session whose
`$env.config` carries no `hooks` key does not error. The appended element is a
**closure**, not a string. The body MUST run **after** the user's `config.nu`,
which the vendor-autoload slot (`$nu.vendor-autoload-dirs`) provides; that
ordering is a contract with a test, not an assumption.

**Rationale.** starship's nushell integration defines
`$env.config.hooks.env_change.PWD` itself, so an assignment silently stops the
user's prompt updating — which is why ADR:288 already says "append … never assign
it" for this shell. The absent-`hooks` half follows from D3: nu parses a whole file
before running it, so one erroring expression voids the entire autoload —
including the PATH prepend the `try/catch` was shaped to protect.

**Test hook.** EC-NU-004(a) tier-3 with starship's nu integration installed first,
both hooks fire — red state: assign instead of append and starship's hook stops
running. EC-NU-004(b) tier-2 `nu -n` with no `hooks` key — the block completes and
PATH carries the ocx bin dir; red state: drop one `| default {}` and the block
errors, taking the PATH prepend with it. Ordering: the appended hook survives a
`config.nu` that assigns `$env.config`; red state: install from a slot that runs
before `config.nu`.

**Verdict.** Adopts EC-NU-004, with the vendor-autoload ordering added as an
explicit contract.

---

**Decision 4 — consent and the activation whitelist**

### A-25 — Any unusable stamp is an absent stamp

Closes: EC-CONSENT-008

**Question.** Unrecognised `v`, malformed JSON, unreadable file — three outcomes or
one?

**Decision.** One. `consent::load(key) -> Option<ConsentStamp>` MUST return `None`
on every failure — I/O error, JSON parse error, unknown field, or a `v` this binary
does not recognise — and the caller MUST treat `None` exactly as "no stamp": clause
1 fails, clauses 2 and 3 still evaluate, log at **debug**, never warn, never error.
`ConsentStamp` MUST carry `#[serde(deny_unknown_fields)]` with all four fields
required — no `#[serde(default)]` on `sources` or `project_dir` — so a truncated
stamp can never deserialize into a valid-looking one.

**Rationale.** Mirrors Decision 1's envelope-tag rule verbatim and D3 (ADR:31). A
warn would violate the no-WARN-on-common-benign doctrine ADR:75 already invokes
for the absent ledger.

**Test hook.** EC-CONSENT-008 (a) `{"v":2,…}`, (b) truncated, (c) `chmod 000` ⇒ all
three `load() == None`, `evaluate` returns `Inert`, no panic, exit 0. Red state:
drop `deny_unknown_fields` and add `#[serde(default)]` to `sources` — case (a) then
loads as a valid stamp with an empty source set while (b) stays `None`; the split
proves the derives are load-bearing.

**Verdict.** Adopts EC-CONSENT-008.

### A-26 — A `paths` grant is unconditional; the auto-stamp rule is deleted

Closes: EC-CONSENT-013

**Question.** Under a standing `paths` grant, does source-set drift re-confirm?

**Decision.** **No.** Clause 3 activates on its own, every prompt, regardless of
the lock's source set — ADR:185 and ADR:238 are correct as written and win.
ADR:193's auto-stamp mechanism MUST be **deleted**, not narrowed: the sentences
*"The **first** activation under a grant records a consent stamp … and **every
later activation runs clause 1** against that stamp"* and *"So a new namespace
appearing in the lock re-confirms for a grant-activated project exactly as it does
for a hand-stamped one"* are struck. **Nothing on the activation path writes a
stamp.** The owner-confirmed shape in the rest of ADR:193 (`paths` primary,
`namespaces` the auto-enabler, global always trusted) stays.

**Rationale.** The two grants are not symmetric, and that is the whole answer.
`namespaces` (clause 2) re-quantifies over the current source set *every prompt*,
so drift detection for it is **structural** and needs no stamp — ADR:193's premise
that the source-set predicate would be "dead code" is false for clause 2. `paths`
(clause 3) is a directory grant whose stated use case (ADR:236, the devcontainer
publisher) is precisely one where the operator *cannot* enumerate sources; making
it drift-sensitive breaks it on the first `git pull` that adds a tool, in an
environment with no human at the prompt to re-confirm. This is git
`safe.directory` semantics — exact directory, unconditional, no content re-check —
which ADR:45 already cites as the field precedent. Not stamping also keeps the
write seam at exactly six commands (A-29) and keeps a write off the stat-only
per-prompt path (D4).

**Consequence to state in the ADR.** Revoking a `paths` grant is immediately
effective, because no stamp was ever derived from it; and `ocx shell state` reports
*"active via `paths` grant; source-set drift is not tracked for path grants"*,
which is truthful rather than phantom.

**Test hook.** EC-CONSENT-013: stamp absent, `paths` covers P, lock gains
`ghcr.io/evil/tool` ⇒ `Activate`, and `state/projects/<key>/` still does not exist.
Red state, **both arms required**: re-introduce the auto-stamp and the
directory-absence assertion fails; make clause 3 conditional on clause 1 and the
`Activate` assertion fails.

**Verdict.** Adopts EC-CONSENT-013's "clause 3 wins as written"; overrules its
"the stamp is silently rewritten to the new source set", because writing it
re-arms clause 1 for exactly the population clause 3 exists to exempt, adds a
seventh stamp writer, and puts a write on the per-prompt path.

### A-27 — One `namespaces` grammar, one validator

Closes: EC-GRANT-005, EC-GRANT-006, EC-GRANT-008

**Question.** What exactly does `[shell.consent] namespaces` accept, and what does
it refuse?

**Decision.** A single `fn validate_consent_pattern(&str) -> Result<()>`, applied
to the bare string form and to every element of `include` and `exclude`, at
deserialization.

**Accepted — exactly two spellings, nothing else:**

1. `<host>[:<port>]/<org>` — org grant, no wildcard.
2. `<host>[:<port>]/<org>/*` — org grant, explicit descendant form (equivalent to 1
   at source granularity).

> **Amended 2026-08-25 — the whole-registry spelling is withdrawn**
> ([#344](https://github.com/ocx-sh/ocx/issues/344)). `<host>/*` and a bare `<host>` are both refused,
> as one error class, so neither is the way to spell what the other no longer says. The reason is the
> only bound clause 2 has: an attacker must get content onto this host under the granted namespace,
> which for content the host does not already hold needs that namespace's publish credential (for content
> it does hold, less — A-39's residual). A pattern spanning every organization on a host voids that bound
> wherever anyone can register on the host, and ocx cannot tell an open registry from a closed one — so
> the spelling is refused rather than documented as dangerous. An operator who genuinely trusts a whole
> private registry lists its organizations, or uses a `paths` grant. Consequence, stated: `exclude` no
> longer subtracts from a wide grant; it subtracts an organization one tier included from another tier's
> view, which is what `accumulate`'s exclusion-wins rule is for.

**Rejected at parse — the pattern is refused and `[shell.consent]` deserialization
fails:**

- the empty string, and a bare `*`;
- any `*` anywhere other than as the final two bytes `/*`, and any pattern with
  more than one `*`;
- a trailing `/` with no `*`, and the pattern `/*`;
- any empty `/`-delimited component (leading `/`, `//`);
- **any ASCII uppercase byte anywhere**;
- three or more components after stripping an optional trailing `/*`;
- `@` anywhere, or `:` after the first `/`.

Implementation: strip an optional trailing `/*`; the remainder must be one or two
components; validate the second component, when present, through the shipped
`Identifier` repository validator rather than minting a second charset.

**Unknown keys inside the spec table.** `ShellConsent::namespaces` MUST carry a
`#[serde(deserialize_with = …)]` pointing at a **consent-scoped** visitor that
mirrors `ScopeSpec`'s hand-written one but replaces its unknown-key arm with an
error, keeps the neither-key floor, and runs the validator above on every pattern
before constructing `ScopeSpec::Set`. The shared `ScopeSpec` deserializer is **not**
changed — its tolerance is deliberate for `[[trust.policy]]`.

**Per channel.** In a `config.toml` tier a rejected pattern is an ordinary parse
error, so that tier contributes nothing; on `OCX_CONSENT_NAMESPACES` the **whole**
contribution is discarded with one warning and the config tiers stand alone (D3 —
no channel may break a prompt). Empty tokens are dropped before any pattern is
constructed (ADR:234, unchanged).

> **Superseded on the config side by [A-40](#a-40--a-refused-shellconsent-table-is-stripped-the-rest-of-the-file-survives).**
> "That tier contributes nothing" is now true only of a table carrying a non-empty
> `namespaces.exclude`. Everywhere else the refused table is **stripped** and the
> rest of that file still applies — the *grant* contributes nothing, the file does.
> The `OCX_CONSENT_NAMESPACES` half above is unchanged.

**Rationale.** `pattern_matches` (`trust.rs:375-383`) is segment-bounded only on
its no-wildcard branch and returns `true` for the empty pattern; `visit_str`
(`trust.rs:325-329`) accepts *every* string, `""` included; `visit_map`
(`trust.rs:342-346`) explicitly drops unknown keys for forward-compat. Nothing
shipped validates a consent pattern today. The uppercase rule is not about folding
widening: `Identifier` parsing **rejects any uppercase repository outright**
(`oci/identifier.rs:505`, `IdentifierErrorKind::UppercaseRepository`), so no lock
can carry one and an uppercase pattern is simply dead — the EC-GRANT-005 family.

**Test hook.** EC-GRANT-005, EC-GRANT-006, EC-GRANT-008: a table-driven case per
rejected form asserting `Err`, plus the three accepted spellings asserting `Ok`
and their match sets (`ocx.sh/acme/*` matches `ocx.sh/acme`, never
`ocx.sh/acme-evil`). Red states: delete the trailing-`/` check and `"ocx.sh/acme/"`
parses then matches nothing; delete the wildcard-position check and
`"ocx.sh/acme-corp*"` matches `ocx.sh/acme-corp-evil`; delete the consent-scoped
visitor and `{ include = [...], require_signed = [...] }` parses with the narrowing
key silently dropped.

**Verdict.** Adopts EC-GRANT-005 and EC-GRANT-008; adopts EC-GRANT-006's verdict
(reject at parse) and overrules its rationale — the repository half is not
case-significant, it cannot be uppercase at all.

### A-28 — `paths` entries stay a literal byte compare, with a near-miss diagnostic

Closes: EC-GRANT-012

**Question.** Does a case-only difference between a `paths` entry and the canonical
directory match?

**Decision.** **No.** Entries MUST be compared as literal bytes after separator and
trailing-slash normalization only — no case folding, no canonicalization of the
entry. A case-only mismatch is `Inert`. To pay off the support cost, `ocx shell
state`'s not-active reason enumeration MUST include a **near-miss** line when a
`paths` entry differs from the canonical directory only by ASCII case or separator
style.

**Rationale.** ADR:225's own reasoning, one step further: canonicalizing entries
follows an attacker-controllable parent symlink; folding case merges `/a/B` and
`/a/b` into one grant on a case-*sensitive* filesystem, which widens. Inert is the
fail-safe direction, and the diagnostic costs one `eq_ignore_ascii_case` on a path
the reason enumeration must build anyway. The key is unaffected either way —
`name_for_path` hashes raw path bytes (`reference_manager.rs:59-63`).

**Test hook.** EC-GRANT-012: `paths = ["/Users/u/Repo"]`, canonical dir
`/Users/u/repo` ⇒ `Inert`, and the reason string names the near-miss entry. Red
state: replace the compare with `eq_ignore_ascii_case` and the `Inert` assertion
fails; drop the near-miss branch and the reason-string assertion fails.

**Verdict.** Adopts EC-GRANT-012.

### A-29 — Read-only commands never consent, stated as a negative contract

Closes: EC-IDENT-013

**Question.** Which commands may write a consent stamp?

**Decision.** The ADR MUST state the negative alongside ADR:201's positive:
`consent::record()` is called from **exactly six** command paths — `add`, `remove`,
`lock`, `update`, `pull`, `run` — and from nowhere else. Every other command,
explicitly including `ocx env`, `ocx inspect`, `ocx shell state`,
`ocx self activate` (with and without `--reconcile`), `ocx list`,
`ocx direnv export` and `ocx completions`, MUST NOT create or modify
`state/projects/<key>/`. `record()` is `pub(crate)` with the allowlist named in its
doc comment; enforcement is the acceptance test below, not discipline.

**Rationale.** ADR:201 already establishes that the stamp needs its own seam
(`register_project_dir_best_effort` has exactly two call sites and covers neither
`run` nor `pull`), which means every call site is new code with no shipped
structure to inherit the negative from. `ocx shell state` is the command a confused
user is told to run and Decision 10 declares it read-only; a stamp written from
there would consent to the project it is diagnosing. A-26 removes the only other
candidate writer (the activation path), so the allowlist has no carve-out.

**Test hook.** EC-IDENT-013 (`pytest-hostshell`): in an unstamped, ungranted
project run each of the read-only commands and assert `state/projects/` gains no
entry; then run `ocx lock` and assert it does. Red state: add a `record()` call to
`ocx env`'s handler and the first half fails; remove it from `lock` and the second
half fails.

**Verdict.** Adopts EC-IDENT-013.

---

**Decision 2 — project identity, state root, GC sweep**

### A-30 — One project identity: canonicalize the resolved file, then take its parent

Closes: EC-IDENT-002

**Question.** `OCX_PROJECT` names a symlinked `ocx.toml` — which directory is the
project?

**Decision.** The consent path MUST derive the project directory the way the
shipped ledger already does: canonicalize the **resolved config file**, then
`.parent()`, then `dunce::canonicalize` — i.e. reuse
`register_project_dir_best_effort`'s derivation
(`project/registry.rs:196-198` → `register`'s `dunce::canonicalize` at `:310`) as
**one shared helper**, not a second directory-based derivation. That single
canonical directory is the input to `name_for_path`, to the stamp's `project_dir`
field, and to the `paths` compare.

**Rationale.** `resolve_explicit_project_path` follows symlinks by design and
returns the **un-canonicalized** path, so `/w/fake/ocx.toml` yields project dir
`/w/fake` and a different 16-hex key than `/w/real` — the VS Code symlink-fork class
ADR:44 cites. `registry.rs:175-186` already documents this exact derivation and its
purpose verbatim: *"It also collapses aliased lookups (relative segments,
symlinks) to one ledger entry."* The two-call ordering is load-bearing on Windows:
`tokio::fs::canonicalize` at `:196` and `dunce::canonicalize` at `:310` do not
produce the same string, and the ledger's key is the second form. Canonicalizing
is also the **safer** direction: if `/w/fake` is granted by `paths` and its
`ocx.toml` symlinks to `/attacker/ocx.toml`, canonicalizing makes the identity
`/attacker`, which is not granted ⇒ `Inert`; leaving it uncanonicalized would
activate the attacker's config under the victim's grant.

**Test hook.** EC-IDENT-002: `OCX_PROJECT=/w/fake/ocx.toml` and
`OCX_PROJECT=/w/real/ocx.toml` yield an identical key and `project_dir`; plus the
inverse case above ⇒ `Inert`. Red state: canonicalize the directory instead of the
file and the two keys diverge; skip the `dunce` step and the key stops matching the
ledger entry `register` writes for the same project.

**Verdict.** Adopts EC-IDENT-002's primary recommendation; overrules its fail-safe
alternative (refusing a symlinked explicit tier), which reverses a shipped,
documented decision for no gain since canonicalizing produces the stricter outcome.

### A-31 — An unreadable stamp is retained by the sweep

Closes: EC-IDENT-011

**Question.** `ocx clean` cannot read a stamp's `project_dir` — remove or keep?

**Decision.** **Keep.** `ocx clean` MUST remove `state/projects/<key>/` only when
*both* hold: the stamp deserializes at a `v` this binary understands, **and** a
re-probe of its `project_dir` immediately before removal proves the directory
definitively absent. Unreadable, malformed, unknown-`v` and indeterminate-probe all
**retain**, with one line at debug.

**Rationale.** This is ADR:126's own indeterminate rule extended to the read that
precedes the probe — a stamp whose `project_dir` cannot be read yields an
indeterminate probe by definition. Over-retention costs one directory in a store
documented as deletable at any time; under-retention deletes consent a newer or
rolled-back binary wrote, and an unreadable stamp is already inert at `evaluate`
(A-25), so removing it buys nothing.

**Test hook.** EC-IDENT-011: three stamps — (a) valid with `project_dir` gone, (b)
`chmod 000`, (c) `{"v":2,…}` with a live `project_dir`. After `clean`, only (a) is
removed. Red state: drop the parse precondition and (b)/(c) are swept; drop the
pre-removal re-probe and a stamp whose directory reappears between readdir and
removal is swept.

**Verdict.** Adopts EC-IDENT-011.

---

**Decision 7 — config tiers and the managed gate**

### A-32 — The explicit config tier outranks the managed tier, and that stays

Closes: EC-CFG-006

**Question.** Does `--config` / `OCX_CONFIG` beat a digest-pinned managed
`[shell] hook`?

**Decision.** The shipped order MUST stand: `[shell] hook` and
`[shell] completions` from the explicit tier win over the managed tier. ADR:326's
sentence *"the managed tier beats every file a user can edit"* MUST be rewritten to
*"beats every **discovered** tier (system → user → home); the explicit tiers
`--config` / `OCX_CONFIG` still merge on top."* `ocx shell state` MUST name the
tier that actually decided the rung — never hard-code "managed".

**Rationale.** `fold_managed_tier` runs, then `merged.merge(overlay.clone())` runs
after it (`config/loader.rs:180-182`), and the loader's own comment states the
intent verbatim: *"the managed tier folds in AFTER the discovered chain … but BELOW
`OCX_CONFIG` and `--config`, so the explicit tiers must merge on top of the managed
fold — never underneath it."* ADR:326's own load-bearing claim — the toggle grants
nothing, consent still gates every project — is what makes a user override costless.

**Test hook.** EC-CFG-006: digest-pinned managed `hook = true` plus `OCX_CONFIG`
with `hook = false` ⇒ merged `hook == Some(false)`. Red state: move
`merged.merge(overlay)` above `fold_managed_tier` and the assertion flips.

**Verdict.** Adopts EC-CFG-006.

### A-33 — `OCX_CONFIG` / `--config` is the third consent-bearing channel

Closes: EC-CFG-007

**Question.** Can an explicit-tier file carry `[shell.consent]`, and does Decision
7's digest gate apply to it?

**Decision.** It can, and the digest gate MUST NOT apply. Decision 4's env-channel
bullet (ADR:233) MUST enumerate `--config` / `OCX_CONFIG` alongside
`OCX_CONSENT_PATHS` / `OCX_CONSENT_NAMESPACES` as an equally consent-bearing
channel of the same, already-out-of-scope threat class (a hostile parent process).
The ADR MUST additionally state that `OCX_NO_CONFIG=1` does **not** prune an
explicit-tier `[shell.consent]` — the only gesture that makes a shell wholly inert
is `OCX_NO_HOOK=1`.

**Rationale.** `guard_managed_sigstore_trust` is called from inside
`fold_managed_tier` only and gates on `source.digest().is_none()` — an explicit
file has no `[managed] source`, so the gate is not merely skipped, it is undefined
there. `OCX_NO_CONFIG` empties the discovered chain and suppresses the managed
fold but never touches `explicit_paths` (`config/loader.rs:145-157`, `:327-331`).

**Test hook.** EC-CFG-007: `OCX_CONFIG` naming a file with
`[shell.consent] paths = [<canonical P>]` ⇒ `evaluate(P) == Activate`; the same
file plus `OCX_NO_CONFIG=1` ⇒ still `Activate`. Red state: add the digest gate to
the overlay path and the first assertion flips to `Inert`.

**Verdict.** Adopts EC-CFG-007.

---

**Decisions 5 and 9 — hook, wrapper, coexistence, limits**

### A-34 — The hook always resolves through `current`; `OCX_BINARY_PIN` does not reach it

Closes: EC-VER-004

**Question.** Does the emitted per-prompt hook's `--reconcile` call resolve the
binary via `current`, or does it respect `OCX_BINARY_PIN`?

**Decision.** The hook always resolves through `current`; `OCX_BINARY_PIN` has
**no** effect on it. State this explicitly in Decision 6's thin-dispatcher
invariant rather than leaving it inferable.

**Rationale.** Verified: none of the five `env.*` shim bodies reads
`OCX_BINARY_PIN` — each hardcodes the `current` symlink path
(`setup/shims.rs:37`, repeated per family). The pin has exactly three consumers,
none of them the shim: the Windows `.exe` shim, the script host's `ocx` module, and
generated Unix launcher bodies. All three are *re-entrant/downstream* invocations —
a running ocx sets the pin to its own `current_exe()` so a child pins back to the
same binary. The interactive shell's own top-level resolution is upstream of that
mechanism and structurally cannot consult it.

**Test hook.** EC-VER-004: `rust-unit` grep of the five emitted shim bodies
asserting none contains `OCX_BINARY_PIN`; `pytest-hostshell` with the pin set to an
older build in shell A while `self update` swaps `current` in shell B — A picks up
the **new** binary.

**Verdict.** Overrules the register's hedge ("most likely has no effect") — code
inspection makes this certain; adopts its recommendation to state it explicitly.

### A-35 — The wrapper returns the wrapped command's exit status

Closes: EC-EMIT-004

**Question.** Does the Decision 5 layer 3 wrapper return the wrapped invocation's
exit status, or the fingerprint check's?

**Decision.** The wrapper MUST capture the real binary's exit status immediately
after it returns — before running any other command, including the fingerprint
check — and MUST return exactly that value. The fingerprint check runs purely for
its side effect and MUST NOT influence the returned status.

**Rationale.** ADR:293-294 specifies the wrapper's behaviour and scope but is
silent on the exit-code contract, while Component Contracts (ADR:383) pins
exit-code semantics as a hard contract elsewhere in the same ADR. An optimization
that silently changes `$?` for every wrapped invocation breaks
`ocx add --global foo && foo` in exactly the way the wrapper exists to make safe.

**Test hook.** EC-EMIT-004 (tier 3, the wrapper exists only in an interactive
shell): `ocx install nonexistent-tool; echo $?` ⇒ non-zero. Red state: drop the
captured-status return and `$?` becomes 0 after a failing subcommand.

**Verdict.** Adopts EC-EMIT-004.

### A-36 — The hook-order flap is accepted and bounded to one prompt

Closes: EC-HOOK-011

**Question.** When ocx's hook registers before direnv's or mise's in the same
`PROMPT_COMMAND` / `precmd_functions`, must the resulting one-prompt
apply-then-revert flap be prevented?

**Decision.** Accepted as a bounded, self-healing artifact of D2's convergence
model. No reordering logic, no cross-tool coordination, no retry. Document it once
on the shell-integration/coexistence page.

**Rationale.** D2 (ADR:30): *"Shell-land has no transactions. Every prompt
re-converges; races self-heal."* Forcing ocx's hook to win the ordering race would
require either prepending instead of appending — which ADR:287-288 explicitly
rejects ("append-only, never clobbered") — or inspecting a signal another tool has
not set yet, which is impossible. The flap is exactly one prompt.

**Test hook.** EC-HOOK-011 (tier 3, both install orders): assert the PATH segment
count returns to baseline **by the second prompt** — convergence, not
flap-never-occurs. Red state: the flap persisting past the second prompt, which is
the actual bug this test catches.

**Verdict.** Adopts EC-HOOK-011's second alternative (bounded, no residue);
overrules its first ("stable from the first prompt regardless of hook order") as
unachievable without violating Decision 5 layer 1.

### A-37 — Both yield sentinels fire independently

Closes: EC-COEX-006

**Question.** How does ocx behave when `DIRENV_DIR` (matching) and `MISE_SHELL` are
both set?

**Decision.** No code change. The two yield checks are **independent `if`s, never an
`elif` chain**: ocx yields its project scope on a matching `DIRENV_DIR` **or** on
`MISE_SHELL` / `__MISE_ORIG_PATH`, regardless of the other's state, and prints one
info line **per observed tool**. State the three-way case as accepted-as-is on the
coexistence page: direnv-versus-mise ordering is between those two tools and ocx is
out of the fight either way.

**Rationale.** ADR:347 already phrases the rule as two independent conditions;
nothing in the implementation needs to know about the other tool to implement
either arm.

**Test hook.** EC-COEX-006: both sentinels set and matching ⇒ ocx applies
global-only and prints one line per tool. Red state: an `elif` chain between the
two checks silently suppresses the second tool's line.

**Verdict.** Adopts EC-COEX-006.

### A-38 — Combined env-block size is an OS boundary, not an ocx mitigation

Closes: EC-SIZE-004, EC-SIZE-005

**Question.** Does the 16 KiB carrier cap need a companion check on the combined
argv+envp size?

**Decision.** No code change, no size accounting, no warning. The 16 KiB cap bounds
only the ledger's own contribution. An `E2BIG` on `execve` — for the interactive
shell or for an `ocx run` child — is an ordinary OS-level spawn failure that
degrades through the existing `Command::spawn()` → `std::io::Error` → `Error::Io`
(exit 74) path, identical to any other spawn failure. State the boundary in NFR
prose and stop.

**Rationale.** `Env::apply_child_env` has no size accounting today
(`crates/ocx_lib/src/env.rs:545-549`) and needs none — `utility/child_process.rs`
already surfaces any envp failure as a plain `io::Error`. An ocx-specific
pre-flight check would be a warning nothing consumes: the same anti-pattern NFR
Operability rejects for a verbosity tri-state (ADR:396) and Decision 1 rejects for
a degradation ladder (ADR:73).

**Test hook.** EC-SIZE-004 / EC-SIZE-005: no new test beyond existing
spawn-failure coverage; if added, assert the failure is the ordinary `IoError`/74,
not a bespoke code.

**Verdict.** Adopts the register's "no new mitigation needed" half for EC-SIZE-004;
overrules EC-SIZE-005's "assert or at minimum log a warning" as unconsumed
machinery.

### A-39 — Clause 2 quantifies over the store's pull-origin record, never over the lock's claim

Closes: [ocx-sh/ocx#344](https://github.com/ocx-sh/ocx/issues/344) — the residual
ADR Decision 4 discloses, not a row of the edge-case register.

**Question.** The package store is keyed by `(registry, digest)` only; the
repository is deliberately absent from the path so identical content deduplicates
across repositories. Composition resolves a locked tool to
`repository.clone_with_digest(leaf)` and then looks the directory up by registry
and digest alone, so **the lock's `repository` field never has to be true for the
content to be found**. What, then, does a `namespaces` grant actually authorize
when it is matched against that field?

**Decision.** Clause 2 MUST quantify over `verified_sources` — the store's own
record of which logical repository *this host* resolved and materialized each
locked digest under — and never over `lock_sources`. The record is
`refs/origins/`, one marker file per distinct repository under the package
directory, whose file name is `ReferenceManager::name_for_path` of its own content
so a clobbered marker is detectable without a second integrity file. It is written
by `file_structure::record_origin` from exactly one place: the fetching branch of
`package_manager::tasks::pull::setup_owned_impl`, gated on `from_registry` =
`provided_metadata.is_none()` (`pull.rs:334`, consumed at `:451`). That predicate
excludes `pull_local` — a local tarball whose repository is author-supplied text no
registry vouched for — and **excludes nothing else**. Reaching the branch means the
two store-hit fast paths did not return, which is a strictly weaker condition than
a registry round-trip; the residual section below states exactly how much weaker,
because the first draft of this addendum overstated it. A tool the store cannot corroborate — no host leaf, not
materialized, or materialized with no marker — poisons the whole grant, and the
resulting `None` is a **refusal**, never "unconstrained". That refusal is its own
`Reason::UncorroboratedNamespace`, carrying claim and record side by side so `ocx
shell state` shows the gap rather than a bare "no grant".

Clause 1 keeps using the **claimed** set, deliberately: a stamp is an explicit
per-directory gesture recording what the lock said at the time, and its drift
detection is a comparison against that same claim.

**Coordinate identity.** The marker records the **logical** coordinate
(`pinned.as_identifier()`), never the transport one — `record_origin` is called
from `setup_owned_impl` with `pinned`, while the layer fetch three statements
later travels over `resolved.transport_pinned`, and under an operator's
`[mirrors]` entry or an index indirection those differ. Recording the routed
address instead would pin consent to routing, which is exactly what
`adr_lock_records_physical_address.md` was rejected for, and would split consent's
identity across two spellings — `source_of` already derives the lock half from the
logical coordinate, so the store half must match or the two sets could never be
compared. It is safe to do so on two counts: a redirect is **operator-configured**
(`[mirrors]` and the index selector are `config.toml`-tier only; `ProjectConfig` is
`deny_unknown_fields` with no such field, and `ProjectEnv` refuses every `OCX_*`
key, so a checked-in file reaches neither), and the content is **digest-verified**
whichever endpoint serves it, so a redirect cannot substitute different bytes. The
residual is therefore narrow and named: under such a redirect the bytes came from
the operator's own routing, and *who published* them is answered by
`[[trust.policy]]` plus signature verification — the same residual
`website/src/docs/in-depth/shell-integration.md` `{#residual}` states for consent
generally, not a second one.

**Rationale.** A-27 already refused the whole-registry spelling, which bounds the
grant by publisher identity — an attacker must hold a listed organization's
publish credential. It does not bound it by *content*: a lock pairing a listed
org's name with the digest of content that came from a different repository on the
same registry satisfied a claim-based clause 2 and put that borrowed content's
`entrypoints/` on `PATH`. Nothing in the store contradicted it, because nothing in
the store recorded a repository at all. Verifying at consent time is impossible by
construction — C-028 runs `evaluate` before composition and a prompt may not block
on the network — so the evidence has to have been written earlier, by the pull
that materialized the content. That is the whole of this resolution: move the
quantifier from text the project authors to a record only an **act of pulling on
this host, under that name** can produce. An earlier spelling of this sentence read
"a record only a registry can cause"; that is false as shipped, and the residual
below says what survives of it.

**Consequences, stated plainly.** A namespace grant now activates a project whose
tools this machine has already fetched — the warm shared store, which *is* the
fleet case — and stays inert on a cold one until the first `ocx pull`, which is one
of the six stamp-writing commands and therefore hands the project a clause-1 stamp
in the same moment. One refusal does **not** self-heal *while the store
hit holds*: a digest already materialized under repository A is a store hit for a
lock naming repository B at the same digest, so `setup_owned_impl` returns before
`record_origin` and that pull mints no B marker. That is the correct direction — a
store hit is not evidence the digest was ever fetched under B — but such a project
needs a stamp or a `paths` entry for as long as the hit holds. It is **not**
unconditional, and an earlier spelling of this sentence ("no later pull ever mints
B's marker") said it was: both early returns are gated on `check_install_status`
(`pull.rs:344`, `:381`), so a package directory that is removed, or left with a
partial or not-OK status, falls through into the fetching branch and does mint B's
marker — from the layer cache, with no registry in the loop. That is one face of
the residual below. `refs/origins/` is provenance, not liveness, and is
therefore outside the GC reachability graph.

**Residual — the write gate does not observe the wire.** Stated as its own
paragraph because the security conclusion Decision 4 carries used to rest on the
opposite. Three facts, each read off the shipped code rather than inferred:
`from_registry` excludes only `pull_local`; both store-hit early returns are
conditional on `check_install_status`, so an absent or not-OK package directory
falls through into the fetching branch; and that branch needs no network, because
the layer cache short-circuits the fetch whenever `layers/{digest}/content/` is
present and acquires the client lazily so an offline manager still re-assembles
(`pull.rs:908-916`), while a digest-addressed manifest read is local-first in every
chain mode (`chained_index.rs:1029`). The package path is `(registry, digest)` only
— `file_structure::package_store`'s
`path_uses_only_registry_and_digest_not_repository` pins that — so a pull naming
**any** logical repository on a registry whose layers are already cached mints that
repository's marker with zero registry contact and no credential anywhere. The
marker is therefore evidence that this host materialized digest-verified content
and bound it to a logical repository name; it is **not** evidence that any registry
vouched for the binding once the content was already local.

What survives is still the whole of the improvement over `lock_sources`: the claim
is satisfiable by **text a clone's author writes**, the record only by an **act of
pulling on this host under that name**. On a cold store the two coincide — the
bytes must come off the wire under that name, which is A-27's publish-credential
bound. On a warm store they do not, and clause 2 then bounds local action rather
than publisher identity: one `ocx pull` naming the granted org at an already-cached
digest is enough, and the marker it writes is a fact about the package, not about
any project, so an unrelated clone carrying nothing but a lock inherits it.

**The fix belongs in the write gate, not here.** `record_origin` should be
reachable only from a path that actually transferred bytes for this digest from
this registry — the natural seam is the fetching branch learning whether any layer
was pulled rather than served from `layers/`, which is knowledge `extract_layers`
already has and discards. Filed as
[ocx-sh/ocx#348](https://github.com/ocx-sh/ocx/issues/348). Until it lands, the
grant's true bound is the one stated above, and both `Decision 4` in the ADR and
`project::consent`'s module docs say so in those terms. This is the same *shape* as
the redirect residual two paragraphs up — the marker is honest about what this host
did, and the gap is between that and what a registry attested — and it takes the
same instrument where the difference matters: `[[trust.policy]]` plus signature
verification answers "who published these bytes", which no provenance marker can.

**Test hook.** `project::consent::tests::s344_a_lock_borrowing_a_granted_orgs_name_for_foreign_content_is_refused`
— a lock claiming `ocx.sh/<granted>` over a digest the store recorded as served by
`ocx.sh/<other>` is `Inert(UncorroboratedNamespace)`. Red state: pass
`lock_sources` where `verified` is expected in `evaluate_with_stamp` and it
activates. Siblings pin the two halves the claim-based version could not
distinguish: `s344_a_lock_the_store_corroborates_still_activates_as_clause_two`,
`s344_a_materialized_package_with_no_recorded_origin_fails_closed`, and
`s344_a_stamp_still_activates_when_the_store_corroborates_nothing` (clause 1 is
untouched). The write itself is invisible to all of them — every one calls
`record_origin` directly — so
`test/tests/test_install.py::test_install_records_the_pulling_origin` is the only
check that a real pull writes a marker at all. Red state: gate `record_origin` off
in `pull.rs` and it fails while every Rust test stays green.

**Verdict.** Supersedes the "accept and document precisely" option
[#344](https://github.com/ocx-sh/ocx/issues/344) listed first. Retains A-27's
grammar decision unchanged.

### A-40 — A refused `[shell.consent]` table is stripped; the rest of the file survives

Closes: the consequence half of EC-GRANT-005/006/008, reopened by
[ocx-sh/ocx#344](https://github.com/ocx-sh/ocx/issues/344)'s fleet blast radius.

**Question.** A-27 refuses a malformed `namespaces` pattern, and `ShellConsent`
refuses an unknown key inside the consent table. Does that refusal fail the whole
`config.toml`?

**Decision.** No. A refused `[shell.consent]` table is **stripped**, and every other
section of that file still applies — on **every** tier: managed, home, user, system
and `--config` alike. `ConfigLoader::parse_config_stripping_refused_consent` re-parses
the payload through `toml::Table`, removes `shell.consent`, and re-`try_into`s it;
anything that still fails keeps the **original** error, spans and all, so the strip
can never decay into "swallow anything". The reason is logged **and** recorded on the
payload (`ShellConfig::consent_strip_reason`), where `ocx about` surfaces it and the
reconciler emits it through the eval'd script (A-21) — the same shape and the same
recorded reason `guard_managed_shell_consent` already uses for an unpinned managed
source, one tier wider. A third case: a `[shell.consent]` table that fails to parse for an ordinary TOML type error rather than a refusal — `namespaces = 123`, `paths = "x"` — is the operator's own typo, not a judgement about consent, and also exits 78; the strip is gated on `ConfigLoader::consent_table_shape_is_readable`, which admits only tables whose `toml::Value` shape matches `ShellConsent` and therefore leaves every deliberate refusal (pattern grammar, empty `include`, unknown key inside `namespaces`, `deny_unknown_fields`) still stripping at exit 0.

**One exception, and it is the load-bearing half.** A refused table carrying a
**non-empty `namespaces.exclude`** keeps the hard failure. `exclude` is the only thing
a consent table says that *takes a grant away*, and it accumulates across tiers
(`ShellConsent::merge`, against a `covered && !excluded` predicate) — so stripping it
leaves whatever `include` another tier contributed standing unopposed. An operator's
fleet-wide "withdraw the compromised org" would become a **grant** on every host too
old to read it, and the attacker is whoever holds the *withdrawn* org's credential.
That is widening, the one direction the carve-out forbids. `exclude = []` withdraws
nothing and does not block the strip; a value shape this ocx cannot read at all counts
as a withdrawal, because that is precisely a narrowing written by a newer ocx.

**Exit codes, stated precisely because they are a declared interface.** A refused table
that **withdraws** is the pre-existing hard failure: **78 `ConfigError`** on the home /
system / `--config` tiers, and on the managed tier the snapshot goes unapplied with one
WARN. A refused table that only **grants** exits **0**, with the reason recorded and
emitted. A table that fails to parse for an ordinary type error, not a refusal, keeps
that same hard failure — **78** (or the managed-tier WARN) — because
`consent_table_shape_is_readable` never admits it to the strip. No new `ExitCode`
variant.

**Rationale.** `arch-principles.md`'s fleet forward-compat row and its
consent-bearing-table carve-out both hold, and only together. One `config.toml` is
fleet-wide state, so a refusal that takes the file down takes `[registries]`,
`[mirrors]` and `[[trust.policy]]` with it — on a `required = false` managed tier that
silently drops the operator's trust pins and falls back to the default registry, so a
commit whose subject is a *narrowing* would widen the effective posture on every host at
once. The carve-out is about the **direction** of the change, not about which file dies:
dropping the whole grant is the narrowest possible outcome, but only for a table that
grants and does not withdraw. Typo detection belongs to the published JSON schema, which
is where that same row already puts it.

**Test hook.** `config::loader::tests::c344_a_refused_consent_table_is_dropped_and_the_discovered_tier_still_loads`
and `c344_a_refused_consent_table_in_a_managed_payload_folds_everything_else` (both tier
classes); `c344_the_strip_drops_a_grant_but_never_a_withdrawal` (one fixture, the
`exclude` line the only variable, both polarities — the granting payloads are the
positive control that stops the fix degenerating into "never strip");
`c344_the_strip_does_not_rescue_a_file_broken_anywhere_else`;
`c344_a_plain_type_error_in_the_consent_table_is_not_stripped`, paired with
`c344_a_refused_pattern_still_strips` as the same guard's negative and positive
control. Red states, one per guard:
replace the function body with a plain `toml::from_str::<Config>` and the first two red;
delete the `consent_table_withdraws` early return and the withdrawing payload starts
loading with its `exclude` silently gone; return the second-pass result unconditionally
instead of falling back to `refusal` and the fourth stops erroring; delete the
`consent_table_shape_is_readable` early return and the type-error payload starts
stripping silently instead of keeping its hard failure, the fifth.

**Verdict.** Supersedes A-27's **Per channel** consequence for the `config.toml` tiers;
retains A-27's grammar and its `OCX_CONSENT_NAMESPACES` rule unchanged. Corrects
ADR:258, ADR:455 and design-spec C-031 / C-051.

### A-41 — The host-capability record is evidence, not assertion, and a degraded pass is never persisted

Closes: the missing record entry for C-044's persisted format — a versioned on-disk
format, which `CLAUDE.md` § Stability tiers makes an interface.

**Question.** C-044's latency fix persists host-capability detection at
`$OCX_HOME/state/host/capabilities.json` under a 1-hour TTL. What does that file
record, and when may it be written?

**Decision.** Three rules.

1. **Evidence, not assertion.** `HostCapabilityRecord` is `{ version, loaders }` and
   nothing else. `loaders` is one `LoaderRecord { path, feature, identity }` per loader
   that classified positive, sorted by path, and the `os.features` answer is **derived**
   from it (`HostCapabilityRecord::libcs`) rather than stored beside it. A record
   claiming a libc family no recorded loader produced is therefore not rejected — it is
   **unrepresentable**. The forge this closes is `{"os_features":["libc.musl"],"loaders":[]}`,
   which V1's two-independent-lists shape honoured on a glibc host.
2. **Freshness is a file identity, not a path existence.** `LoaderIdentity` records
   device, inode, size and mtime (seconds + nanoseconds) off the same `stat` the presence
   check already performs, and `evidence_still_holds` re-checks all four; path existence
   alone survives a replace-in-place. Deliberately not a content hash: hashing one 960 KB
   loader per invocation measured 0.41 ms against the ~2.3 ms the record saves, and every
   ordinary replacement moves at least one field (rename moves the inode, in-place rewrite
   moves the mtime, a layer or bind-mount swap moves the device).
3. **A detection that classified nothing is never persisted.** The record has one shape
   and cannot say "I could not look", and an empty loader list is *vacuously* fresh — so
   latching one answers `os.features` with the empty set for the whole TTL, and a
   glibc-only package then fails to resolve (`FeatureMismatch`, exit 65) on every install
   until it expires. A degraded pass produces exactly that empty list: the directory scan
   losing its `spawn_blocking` join, or every probe hitting `PROBE_TIMEOUT` on a loaded
   runner. So `record_detection` writes nothing, costing one re-detect per invocation —
   precisely what every invocation paid before the record existed.

`RecordVersion` is `serde_repr`, so an unrecognised integer is a clean miss with no
hand-written check to forget, and bumping it is the entire migration story. The
`deny_unknown_fields` on the record and every nested struct is the **other direction** of
`arch-principles.md`'s carve-out, not a breach of it: this is machine-local derived state
with a version tag and a 1-hour TTL, not the fleet-wide `Config` tree, and a stray key
means the file came from somewhere else — for which the only safe reading is "probe now".
Every failure mode (missing, unreadable, corrupt, expired, evidence-invalidated) is a
miss, and a miss re-runs detection; nothing here turns a slow command into a failed one.

**Rationale.** C-044 withdrew [#340](https://github.com/ocx-sh/ocx/issues/340)'s budget
amendment by moving the loader walk off the prompt path, and this record is what makes
the common case one file read. A cache whose entries can outvote the evidence they were
derived from is a *worse* answer than no cache — wrong for an hour, and silent. Making
that state unrepresentable is cheaper than refusing it.

**Test hook.** `oci::host_capabilities::tests::a_record_claiming_a_libc_it_has_no_evidence_for_is_refused`
(the forge verbatim: well-formed, stamped now, inside the TTL, declaring glibc, backed by
nothing); `a_record_this_writer_could_not_have_produced_is_refused`, which mutates the
two axes **separately** off one control so neither guard is silently doing the other's
work — a stale `version` with no stray field, and a stray `os_features` key with the
correct `version`, each asserted to land before it is trusted;
`record_naming_a_removed_loader_is_a_miss`; and
`a_detection_that_classified_nothing_is_not_recorded`. Red state for rule 3: delete
`record_detection`'s empty-classification early return and a degraded pass writes a file
that then answers for an hour.

**Verdict.** Extends C-044, which anchored only the path and the TTL. Changes no ADR
decision.

### A-42 — Every existence probe resolves in the namespace it asks about, and no wider

Closes: the read side of ADR:316, which states only the write side ("append-only, never
clobbered").

**Question.** Every arm asks "am I already registered?" before appending its hook. What
lookup domain may that question use?

**Decision.** **The probe's lookup domain must be no wider than the thing it probes for.**
A shell function is probed with a function-scoped builtin — `typeset -f` (bash, zsh),
`functions -q` (fish), `Test-Path function:` (pwsh); a global variable with
`Test-Path variable:`; and elvish's closure through its parsed `arg-names`, never through
its rendered source. A probe answering a broader question reads as "already registered"
for something that is not ocx's registration, and the shell then runs **unhooked for its
whole life, with no diagnostic** — the worst failure shape in this design, because it is
silent and permanent. Totality is part of the rule (C-051): the elvish probe
short-circuits so `[arg-names]` is indexed only on a closure, and `has-value` over a list
cannot raise, so no element of a user's hook list can abort the registration.

**Both prior failures, recorded because the rule is only credible with them.**

1. The POSIX arms used `command -v __ocx_prompt_hook`, which resolves aliases, builtins
   and `$PATH` executables as well as functions — so an executable of that name anywhere
   on `$PATH` satisfied it. At the wrapper's call site that is *worse* than a false
   "already registered", because the next word **runs** what was found. `typeset -f` is
   zero for a shell function and nothing else (verified in bash 5 and zsh 5 against a
   `$PATH` executable, an alias and a real function), is a builtin in both, and is cheaper
   — no `$PATH` walk.
2. The elvish arm scanned `to-string $edit:before-readline` for a sentinel comment.
   `to-string` renders each closure together with its `&def` (its literal body, comments
   included) and its `&src` (the whole source of the `eval` unit that defined it), so a
   user's own pre-existing hook that merely **mentioned** the marker — in a comment, in a
   string — made the probe true and no ocx hook was registered at all. The replacement is
   structural: the marker is the closure's **rest-parameter name**
   (`{|@__ocx-prompt-hook| … }`), which elvish parses into `arg-names`, and text cannot
   forge an entry in a list the parser produces.

**Rationale.** Both defects are invisible to every behavioural test that asserts on a
*registered* shell, because the failure is that registration silently did not happen —
which is why the rule ships as a structural guard over the emitted text rather than as
care.

**Test hook.** `shell::hook::tests::every_existence_probe_is_scoped_to_the_namespace_it_asks_about`
pins the exact probe string per arm plus the wrapper's guarded call, so a widening of any
of them reds. The elvish half is a live test seeding a synthetic hook list with the
closure the emission actually registers — `elvish_already_registered` and
`elvish_hook_closure` are one spelling each, consumed by both the emission and the test,
because a probe re-spelled in the test could pass against an emission that had stopped
producing it. Red states: swap `typeset -f` back to `command -v`; swap the `arg-names`
read back to a `to-string` substring scan and seed a decoy hook that mentions the marker.

**Verdict.** New rule; changes no prior decision. ADR:316's append-only clause governs the
**write**, this governs the **read** that gates it.

### A-43 — The per-prompt guard's terms are enumerated, and a yield sentinel is one of them

Closes: the record gap A-36 left. A-36 decided the hook-order flap is bounded to one
prompt and rewrote EC-HOOK-011 around convergence; neither it nor A-37 ever said what
the guard *reads*, so a term could be added to it — and one was, in `be740590` — with
no design anchor to add it against.

**Question.** The reconcile is skipped whenever the guard finds nothing moved. Which
facts is the guard obliged to watch, and what is the rule for admitting a new one?

**Decision.** The guard's term set is **normative and enumerated here**, five terms on
the POSIX arms:

| Term | Spelling | What it catches |
|---|---|---|
| Empty carrier | `[ -z "${__OCX_ENV_STATE-}" ]` | the first prompt of every shell (no record counts as changed), and `unset __OCX_ENV_STATE` as the user's escape hatch (C-012, C-046) |
| Directory | `[ "${__ocx_pwd-}" != "$PWD" ]` | `cd` — without it the guard is blind to a directory change, because the watch paths were baked into the body at shell start and are still the *previous* project's (C-019 member 7) |
| **Yield sentinel** | `[ "${__ocx_yield-}" != "${DIRENV_DIR-}\|${MISE_SHELL-}\|${__MISE_ORIG_PATH-}" ]` | a direnv or mise session appearing **or leaving** mid-shell |
| Stamp presence | `[ -z "${__ocx_stamp-}" ] \|\| [ ! -f "${__ocx_stamp-}" ]` | a `mktemp` that failed, or a reaped `/tmp` |
| Watch mtime | `[ '<path>' -nt "${__ocx_stamp-}" ]` per member | Decision 3's fingerprint fold moving on disk |

Every read uses `-` default expansion, because the guard has to be `set -u`-safe: the
carrier is unset on the first prompt by construction, and every sentinel is unset in most
shells — which is the common case here, not the edge.

fish and PowerShell carry the same set in their own dialects (`__ocx_pwd` /
`__ocx_yield`; `$global:__ocxPwd` / `$global:__ocxYield`, whose stamp is an in-process
`[datetime]` and therefore needs no presence term). Elvish carries terms 1-3 only, folded
into the single `__OCX_ENV_PWD` composite `<pid> <pwd> <DIRENV_DIR> <MISE_SHELL>
<__MISE_ORIG_PATH>`, because it has no clock to compare a stamp against (ADR Decision 5's
elvish bullet) and no shell-local an `eval` unit can both write and read.

**Three rules the yield term makes explicit, and which govern any future term.**

1. **A sentinel the detector honours and the guard cannot see is a yield that never
   happens.** `YIELD_SIGNALS` mirrors, exactly, the variables `shell::coexistence::detect`
   reads. The guard decides whether ocx is invoked at all, so the two sets are one set;
   they are spelled once in `hook.rs` and consumed by both.
2. **The guard compares a recorded snapshot against live values, so every term fires in
   both directions by construction.** A sentinel appearing and a sentinel going away are
   the same inequality — which is what delivers A-36's *composed again* half. A one-way
   "is direnv live?" test would have caught only the first.
3. **Raw values, never the verdict.** This is a "something moved" tripwire; `detect` —
   which additionally compares `DIRENV_DIR` against the resolved project directory — owns
   the decision. A `DIRENV_DIR` naming an unrelated directory therefore costs one
   reconcile that changes nothing, the same price the `$PWD` term already pays for a `cd`
   outside any project.

Admission rule for a fourth: a term is admitted only if it is a **parameter expansion or
builtin comparison in every arm**, so the quiet path stays at zero execs (C-044). That is
why elvish buys no `test -nt` term and drops the stamp instead of paying one exec per
quiet prompt.

**Rationale.** The shipped defect A-36 did not predict: the guard watched the carrier,
`$PWD`, the stamp and the watch paths, and **none of those moves when `DIRENV_DIR`
appears**. The reconciler was correct throughout — run directly under a live `DIRENV_DIR`
it emits the unset and the PATH subtraction — but nothing ever ran it, so ocx's project
scope sat beside direnv's for the rest of the shell's life. That is a failure of the
*record* as much as of the code: A-36 reasoned about hook **order** and never wrote down
the guard's **inputs**, so no reviewer had a list to check the new sentinel against. This
addendum is that list.

**Test hook.** `shell::hook::tests` pins the guard string per arm, so adding or dropping a
term reds. Red state for the yield term specifically: drop
`|| [ "${__ocx_pwd-}" != "$PWD" ]`'s sibling `__ocx_yield` comparison from the POSIX guard
and the pinned-string test reds; behaviourally, EC-HOOK-011's tier-3 both-install-orders
case must still converge by the second prompt with a `DIRENV_DIR` set **after** the first
prompt, which the pre-`be740590` guard fails.

**Verdict.** Extends A-36 with the enumeration it omitted, and generalizes A-37 (which
decided the two sentinels fire independently in `detect`) to the guard that decides
whether `detect` runs at all. Changes no prior decision.

---

## ADR corrections

Mechanically applicable in a later edit pass. ADR line numbers are as of the
522-line revision dated 2026-08-25.

| ADR line | Corrected statement |
|---|---|
| 63 | The elvish fact cited here (`\$` rejected in a double-quoted string) applies to `Shell::export_constant`'s Elvish arm as well as to the carrier — that arm emits exactly that form today (A-15). |
| 72, 428 | The per-arm escaper map governs the **element-match / `export_path`** emits. `export_constant` uses `escape_value` on *every* arm today (`shell.rs:432`); A-15 changes that. Say which emit path the map governs. |
| 73 | Over cap ⇒ emit a **marker-only ledger** retaining `v`, `fp`, `verdict` and `over_cap: [scopes]`; omit the variable only if that fails. "Omitted entirely" as the first rung destroys the fingerprint and makes every later prompt recompose (A-01). |
| 78, rule (a) | "…a `dir` that does not match the walk's result **means that scope has been left: its `applied` map becomes the revert set**" — not "invalidates the whole project scope". A malformed `dir` is a mismatch, not a void record (A-03). |
| 78, rule (b) | Add: `plan` refuses `Constant` for `PATH`/`PATHEXT` (via `EnvKey`), so a ledger recording either as `Constant`, or carrying `priors` for either, is discarded on decode. Rule (b) alone has no operand when D names the key nowhere (A-02). |
| 78-79 | The forgery posture holds only among equal principals. State the privilege-crossing residual explicitly (arbitrary constant restore via rule (c), because the revert set is L-only per ADR:158) and name `env_reset` / `secure_path` and `unset __OCX_ENV_STATE` as the mitigations. Remove "closed by the next bullet" — the next bullet is the equivalence that fails (A-06). |
| 92 | Key derivation is `name_for_path(dunce::canonicalize(canonicalize(<resolved project file>).parent()))`. The alias-collapsing canonicalization is on the **file** at `registry.rs:196-198`, not the dir at `:310`; both calls are load-bearing and their order fixes the Windows form (A-30). |
| 126 | Sweep precondition: remove only when the stamp deserializes at a known `v` **and** the pre-removal re-probe proves `project_dir` absent. Every other outcome retains (A-31). |
| 139 | Split per kind: path-kind → `move_to_front` / `export_path` (front, in order); list-kind → `append_unique` / `export_list` (back, whole opaque contribution, effective separator); constant-kind → `export_constant`. "In front, in order" is false for list-kind (A-07). |
| 140 | `remove_list_element` is `append_unique`'s inverse — flank-delimited removal of one whole contribution, never a segment op. `None` means the platform PATH separator **for path-kind only**; list-kind always passes `Some(effective)` (A-08). |
| 142 | Add a fourth hazard, this codebase's own and measured, not field-known: `escape_value`'s `!` handling corrupts the value on every POSIX arm, interactive or not (A-15). Add a fifth: PowerShell element comparison is case-insensitive by default — `export_path`'s `-ne` deletes a differently-cased sibling on Linux (A-19). |
| 143 | Ownership is `Path::starts_with` against a canonicalized `$OCX_HOME` (component boundary); retirement's removal operand is the segment **as observed in C**, not as spelled in D (A-09). |
| 154 | Retirement: enumerate C's segments, select the prefix-owned ones absent from D, and name the observed string in the removal (A-09). |
| 160 | "Lists and PATH — each application prepends" is wrong: `ModifierKind::List` appends (`env/list.rs:16-24`). Project still beats global for list-kind, but because list consumers are last-wins and project is applied last (A-07). |
| 166 | Watch set gains the ledger-recorded config-tier paths and the project's consent stamp; `fp` folds the raw `OCX_CONSENT_*` values (A-13). The ceiling is stated granularity-free (full `SystemTime`; FAT/exFAT/NFS named) (A-14). Add: a walk returning no hit while `L.scopes.project.dir` is still an ancestor-or-self of CWD with a regular `ocx.toml` present is **indeterminate** — retain the scope (A-11). Add: an `ENAMETOOLONG`-class probe error is already covered by `has_git_dir`'s fail-closed default; no separate handling exists or is needed. |
| 193 | Delete both auto-stamp sentences. Replace with: grants do not stamp; `namespaces` is drift-sensitive by its own quantifier (clause 2 re-evaluates every prompt), `paths` is a directory grant and deliberately drift-blind, which ADR:185 already says and its devcontainer use case requires; revoking a grant is therefore immediately effective. Keep the owner-confirmed shape in the rest of the bullet (A-26). |
| 195, 229 | A source is exactly two components, so descendant implication is **vacuous at source granularity** — `ocx.sh/acme/*` and `ocx.sh/acme` match the same set. Delete the `ocx.sh/acme/team/tool` example, which describes a repository and would license three-component patterns the grammar now rejects (A-27). |
| 201 | Add the negative: exactly six writers; every other command, `ocx shell state` and `ocx self activate` included, must not create `state/projects/<key>/` (A-29). |
| 225 | Add: a case-only or separator-only near-miss between a `paths` entry and the canonical directory is reported by `ocx shell state`'s reason enumeration (A-28). |
| 230 | Withdraw *"`ScopeSpec` already errors on a malformed form (`trust.rs:350-362`) … so this is the shipped behavior and not new machinery."* `visit_str` (`trust.rs:325-329`) accepts every string, `""` included; the `:349-354` error covers only a table naming neither key. Pattern validation is **new machinery** — replace with A-27's three accepted spellings and seven rejections. |
| 231 | The `trust.rs:252-257` quote establishes the *neither-key* floor, not unknown-key refusal — `visit_map` drops unknown keys at `:342-346`. `deny_unknown_fields` on `ShellConsent` alone does not deliver the stated property; the split extends one level deeper, into a consent-scoped `deserialize_with` on `namespaces` (A-27). |
| 233 | Enumerate `--config` / `OCX_CONFIG` as a third consent-bearing channel of the same out-of-scope threat class, and state that `OCX_NO_CONFIG=1` does not prune it — only `OCX_NO_HOOK=1` makes a shell wholly inert (A-33). |
| 291 | Add: the emitted hook resolves the binary through `current` unconditionally; `OCX_BINARY_PIN` is a downstream re-entrant mechanism and has no effect on the `--reconcile` call (A-34). Add: on pwsh, stderr-discard plus status-ignore is insufficient under `$ErrorActionPreference='Stop'` — the arm needs `try`/`catch` plus a scoped preference override and `$?`/`$LASTEXITCODE` preservation (A-22). |
| 293-294 | Add: the wrapper captures the real binary's exit status before the fingerprint check and returns that captured value unconditionally (A-35). |
| 298, 396 | Narrow to the **per-prompt reconcile path only**: the startup path emits no diagnostics; the message passes the arm's escaper and rides as a `printf` **argument**, never the format string; the stderr discard stays unconditional; the summary, hint and over-cap lines are emitted as shell code via `Shell::emit_message`. Add the corollary that the first prompt of every shell always reconciles (A-21). |
| 316 | The `Plan` JSON wire shape carries a **structural `v`** with the ledger's degradation rule (unrecognised ⇒ apply nothing this prompt), and the inlined nu applier needs a **`list` arm and an unknown-`type` skip**. The one-hop lag is currently worse than stated: today's `else` arm applies a `type: "list"` entry as a constant, so nushell already diverges on a kind that ships (A-23). |
| 326 | "beats every file a user can edit" → "beats every **discovered** tier (system → user → home); `--config` / `OCX_CONFIG` still merge on top (`loader.rs:180-182`)." `ocx shell state` names the deciding tier, never asserts "managed" (A-32). |
| 347-352 | Add: both yield sentinels fire independently and print one line per observed tool; the three-way case is accepted as-is (A-37). Add: hook-registration order relative to direnv's/mise's own entries is unspecified and accepted under D2 — any apply-then-revert flap is bounded to one prompt and self-heals (A-36). |
| 364 | Truthful once the `over_cap` marker exists — keep the bullet, and add the marker to Decision 1 (A-01). |
| 375 | `plan` additionally drops entries failing `is_valid_env_key`, path-kind entries containing `PATH_SEPARATOR`, empty elements and LF/CR-bearing elements, before `Plan` or `L`; `L ⊆ emittable(D)` is a stated invariant. `LedgerEntry.separator` holds the **effective** separator and is always `Some` for `kind == List` (A-08, A-10). |
| 376 | The per-arm escaper rule applies to `export_constant` as a **change**, not a description (A-15). Add: `export_path` is not total — it returns `Some` for an empty value and for `%`/LF/CR on Batch; three `None`/comment arms are required before the reconciler consumes it (A-17, A-20). Add the PowerShell ordinal/`OrdinalIgnoreCase` split and the quoted-segment strip (A-19). Add `Shell::emit_message` (A-21). |
| 377 | The parity contract needs a third clause: `export_constant` must be byte-identical to `apply_entries`' `set` (`env.rs:594`), because the `C == L.applied` guard compares exactly those two products — measured, they differ today for any `!`-bearing value on all five POSIX arms. And the contract must name the **comparison predicate**: on Windows the guard is ASCII-case-insensitive, so `C == L.applied` is not `==` (A-15, A-19). |
| 383 | Add: a `current_dir()` failure on the CWD itself degrades to "no project resolved this prompt", debug, exit 0 — never a stale cached CWD, never exit 74 (A-11). |
| 387 | `ocx shell state`'s reason list gains: a skipped symlinked `ocx.toml` candidate and the ancestor activated instead (A-12); a `paths` near-miss differing only by case or separator (A-28); "active via `paths` grant; source-set drift is not tracked for path grants" (A-26); the deciding config tier by name, never "managed" (A-32). |
| 395 | Add: the 16 KiB cap bounds only the ledger's contribution; the combined argv+envp size is bounded by the OS, degrades through the ordinary spawn-failure → `IoError`(74) path, and is deliberately not separately mitigated (A-38). |
| 427 | Replace *"an L key absent from D ∪ prefix-owned — each must be discarded, per Decision 1"* with *"an L constant absent from D is **reverted**, not discarded; the forgery bound is D ∪ L, enforced by 'an L entry may only undo itself' plus the `PATH`/`PATHEXT` constant refusal"* (A-03, A-02). |
| 443 | Delete "a grant auto-stamps on first activation and clause 1 runs on every later one"; replace with the A-26 pair (a `paths`-granted project activates on drift and writes no stamp; a `namespaces`-granted project goes inert when a source leaves the grant). |

---

## Register errors

Places the edge-case register is wrong about the shipped code. Each was verified in
the worktree.

- **EC-LEDGER-007** — the register's rule rests on "ocx never writes `PATH` as
  `Constant`", which is false today: `parse_env_value` accepts any key with any
  kind, `Var.key` is a plain `String`, and `apply_entries` maps
  `Constant => self.set` unconditionally (`env.rs:593`). A-02 enforces the
  invariant instead of asserting it.
- **EC-PATH-015** — the register claims only prefix-based removal reconciles
  prefix-ownership with byte-exact matching. `plan` receives `current: &Env`
  (ADR:375), so drawing the removal operand from C makes selection and removal
  byte-identical with no new per-arm primitive (A-09).
- **EC-REC-003** — calls ADR:291 and ADR:298 "mutually exclusive as written". They
  are not: ADR:298 already establishes the channel. The real gap is that neither
  the NFR bullet nor the summary line says it uses that channel, and no `Shell`
  primitive emits one (A-21).
- **EC-SCOPE-007** — claims the symlink warn reaches the prompt. It does not: the
  hook discards the binary's stderr unconditionally, and the shims do the same on
  the startup path. The residual is a missing diagnostic, not prompt noise (A-12).
- **EC-CONST-008** — names `env::var(..).ok()` as the failing reader; `.ok()` yields
  `Some("")` and is correct. The failures are `filter(|v| !v.is_empty())` and
  `unwrap_or_default`. It also omits the Windows residual (A-05).
- **EC-QUOTE-009** — claims the `\!` divergence is interactive-vs-non-interactive.
  Measured, the backslash is retained in **every** case, interactive bash with
  `histexpand on` included; the corruption is unconditional (A-15).
- **EC-QUOTE-007** — its premise (that `\(` and `\$` are unrecognised in a plain nu
  double-quoted string) could not be verified either way on this host. The
  verifiable defect is the stale doc comment at `shell.rs:503-510`, which claims
  `export_path` uses `$"..."` interpolation; the arm has used a plain `"{value}"`
  all along, and says so in its own comment at `shell.rs:233-234` (A-16).
- **EC-QUOTE-010** — says `escape_value` "doubles `%` for the `.bat` context",
  correct only for the `call file.bat` channel; via `FOR /F` the `%%` survives as
  two characters. It also misses that the caret escapes are over-escaping — both
  Batch emits wrap the value in `SET "KEY=…"`, where cmd processes none of
  `^ & < > |` (A-20).
- **EC-PATH-008** — its second clause ("record the normalisation in the ledger")
  violates Invariant L-1 and makes the constant restore emit a wrongly-cased
  directory. Normalise the comparison, not the storage (A-19).
- **EC-HOOK-010** — the `POWERLEVEL9K_INSTANT_PROMPT` sniff fixes one consumer of a
  class that also contains pwsh's `$ErrorActionPreference='Stop'` (the register's
  own EC-EMIT-008) (A-21).
- **EC-NU-006** — frames the unknown-`type` fall-through as forward-compat.
  `ModifierKind::List` already ships and already serialises as `type: "list"`, so
  today's `else { load-env … }` arm already applies list entries as constants on
  nushell. Live defect, not future (A-23).
- **EC-GRANT-006** — claims the repository half is case-significant so folding would
  widen. `Identifier` parsing rejects any uppercase repository outright
  (`oci/identifier.rs:505`), so no lock can carry one and the pattern is simply
  unmatchable. The verdict survives on the EC-GRANT-005 rationale (A-27).
- **EC-PROC-013** — claims the three normative rules bound the blast radius to
  "removal of an entry the forger claims to have applied". ADR:158 scopes the
  revert set to L and explicitly does not intersect it with D, so the primitive is
  arbitrary-value-for-arbitrary-key (A-06).
- **EC-IDENT-002** — proposes canonicalizing the resolved project file as a new
  rule; it is already the shipped ledger derivation, documented at
  `registry.rs:175-186`. The gap is that ADR:92 cites the wrong call (A-30).
- **EC-FS-014** — framed as an implementation gap; `has_git_dir`'s fail-closed
  default already stops the ascent. Test-and-document only (A-11).
- **EC-SIZE-005** — recommends a size assertion or warning at a seam that has no
  size accounting and needs none; unconsumed machinery (A-38).
- **EC-VER-004** — hedges "most likely has no effect". Code inspection makes it
  certain (A-34).
- **EC-LEDGER-006** — framed as a reporting inconsistency; it is larger, because
  `fp` lives inside the ledger and omitting the variable destroys the fingerprint
  (A-01).

---

## Register rows retired or restated

Downstream test authors must not write these as the register states them.

| Row | Status |
|---|---|
| `EC-CONSENT-012` | **Retired.** A-26 deletes the auto-stamp, so "first activation under a grant stamps silently" is no longer the specification. Replace with A-26's assertion pair: a `paths`-granted project activates and `state/projects/<key>/` stays absent. |
| `EC-CONSENT-013` | **Restated** by A-26 — its diagnosis stands, its remedy (silently rewrite the stamp) does not. |
| `EC-LEDGER-005` | **Tightened** by A-01: the carrier is a decodable marker-only ledger, not an omitted variable; `priors` are still lost, and that assertion stands. |
| `EC-LEDGER-010` | **Restated** by A-03 — the Validation tier-1 bullet it quotes (ADR:427) is itself corrected. |
| `EC-LIST-008` | **Restated** by A-07 — adopt the diagnosis, not the "emit list scopes in reverse" alternative. |
| `EC-PATH-008` | **Restated** by A-19 — platform rule adopted, ledger-normalisation clause rejected. |
| `EC-PATH-015` | **Restated** by A-09 — operand-from-C, not prefix removal. |
| `EC-HOOK-010` | **Restated** by A-21 — the startup channel is deleted rather than conditionally suppressed. |
| `EC-NU-006` | **Widened** by A-23 — a `list` arm is required alongside the unknown arm. |
| `EC-SIZE-005` | **Restated** by A-38 — no warning, no accounting. |

---

## Deferred

**None.** Every gap is decided above.

Two resolutions carry enough blast radius that the owner may want to overturn them
in one line rather than discover them in review:

- **A-26** deletes ADR:193's grant auto-stamp rule outright. The reasoning is that
  clause 2 already re-quantifies the source set every prompt, so a `namespaces`
  grant is drift-sensitive without a stamp, while a `paths` grant is a directory
  grant that ADR:185 and ADR:238 already make unconditional. If the owner wants
  `paths` grants to be drift-sensitive, the counter-decision is one sentence — but
  it re-introduces a stamp write on the activation path and breaks the devcontainer
  case ADR:236 states.
- **A-15** changes `Shell::export_constant` on five shipped arms and deletes an
  `escape_value` arm plus its tests. It is a behaviour change to a shipped emitter
  used by `ocx env`, `ocx package env` and `ocx direnv export`, not only by the new
  reconciler. The measured defect (`!` → `\!` corrupting every POSIX constant) is
  real and independent of this ADR; the alternative is to scope the change to the
  reconciler's own restore path and leave the shipped emitters corrupting, which
  breaks the `C == L.applied` guard and the in-process/emitted parity contract
  (ADR:377).
