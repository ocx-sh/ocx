# Rulings: Toolchain Activation

## Metadata

- **Status**: Fold complete 2026-09-06 — one tracked record for every orchestrator ruling issued
  during `/hex-execute` on `plan_toolchain_activation.md`.
- **Trigger**: [V-32](../state/review-findings.md) — 616 `RUL-NNN` citations across 45 files under
  `crates/` and `test/` (81 distinct ids), 70 of them resolving only into `.claude/state/`, which is
  gitignored with zero tracked files. A clone of this repository could not resolve a citation like
  `RUL-79` in `composer.rs`. This document is the fix: every id a shipped file cites now has a
  stable, tracked anchor.
- **Scope**: Defines the **81 ids actually cited** in `crates/` and `test/` (verified below), plus
  every other ruling that happened to share a source block with one of those 81 — a ruling
  document is transcribed whole, not diced around which of its entries got cited. It does **not**
  attempt to catalogue every `RUL-NNN` that exists anywhere under `.claude/state/`; ids never cited
  in shipped source (e.g. `RUL-76`, `RUL-100`, `RUL-105`, `RUL-109`, `RUL-111`–`RUL-114`) are out of
  scope for this fold and are left in the (untracked, still-present) source files.
- **Sources** (all untracked, kept in place — see *Provenance*): `.claude/state/rulings/w2-rulings.md`,
  `wp8-rulings.md`, `wp8-rulings2.md`, `wp9-rulings.md`, `wp12bd-rulings.md`, `wp15-rulings.md`;
  `.claude/state/rulings_toolchain_activation_wp12bd.md` (byte-identical to `wp12bd-rulings.md`);
  `.claude/state/plans/plan_toolchain_activation.md` (Divergences table, rows D-V28, D-V31, D-V33,
  D-V35 — the plan restates rulings inline rather than in a standalone `**RUL-N**` block).
- **Wording**: preserved verbatim from source, including cross-references to `C-NNN` (ADR
  contracts), `D-VNN` (plan divergences), `R-WNN` (routed findings) and `S-NNN` (ADR scenarios).
  Anchors are new; ruling text is not paraphrased.

## How to read this

One `#### RUL-N` anchor per id, grouped by the wave/work-package the ruling was issued for (matches
the source files' own grouping). Each entry names its source. Where an id has two source
definitions, both are shown with a **Reconciliation** note explaining which is canonical and why.

## Wave 2 — WP-3 through WP-6 (`w2-rulings.md`)

Issued after the Stub and Verify-Architecture phases. Recorded in the plan as divergence rows
D-V16…D-V22 or routed findings R-W18…R-W26.

### WP-3

#### RUL-1 — ASCII case folding covers every reserved-name comparison, not only `bin`
ASCII case folding applies to **every** reserved-name comparison in `project/config.rs`, including
the shipped `default` and `all` group-name checks — not only the new `bin`. `[group.Default]`
silently coexisting with the `default` group is exactly the collision the contract exists to stop.
One shared comparison helper, not two rules in one file. Recorded as **D-V17**.

#### RUL-2 — the charset validator's blast radius is audited fixture-by-fixture, not blanket-fixed
Wiring the charset validator at all three sites before the body is implemented reds most existing
`ocx.toml` fixtures — that is expected under contract-first TDD. The **Implement** phase audits
every fixture the validator newly rejects and decides per fixture whether the fixture or the
validator is wrong. **Do not blanket-edit fixtures to green.**

#### RUL-18 — two stale doc comments on the Windows `LazyMode::Never` floor
Two doc comments in `project/config.rs` state the Windows `LazyMode::Never` floor as live fact;
WP-5's C-027 deletes it this same wave. Rewrite both. The pure-versus-host split on
`lazy_mode_ladder_for_tool` stays; only its stated rationale is dead.

### WP-4

#### RUL-3 — tier precedence: `config.toml` (incl. `[managed]`) beats `OCX_TOOLCHAIN_DIR` — CONFIRMED
`config.toml` (including `[managed]`) ▸ `OCX_TOOLCHAIN_DIR`. The environment variable is the
**weakest** tier, matching C-007's rule for the sibling toolchain keys.

#### RUL-4 — `toolchain-dir = $OCX_HOME` exactly is REFUSED, exit 78
C-017 admits *descendants* of `$OCX_HOME`; the store root itself is not a location within the
store. Trees there would land as 16-hex siblings of `packages/` and `blobs/`, inside what `ocx
clean` and the GC walk own, and the shipped `consent::record_in` already refuses to stamp
`$OCX_HOME` for the same reason. Refuse `$OCX_HOME` exactly, and `$OCX_HOME/toolchain` and its
whole subtree (R-W1). Every other descendant stays admitted.

#### RUL-5 — `ToolchainRoot::resolve`'s resolution order, as corrected after the architecture pass
`ToolchainRoot::resolve` has **no filesystem side effect** and **accepts a root that does not yet
exist**. Order:
1. Expand a **leading `~` only**, first — before the relative check, or the ADR's own worked
   example `~/.cache/ocx/toolchain` is refused. Use the shipped `config/shell.rs` `expand_against`
   seam made `pub(crate)`; never a second expander, since that function's doc calls itself the one
   seam.
2. **No `%VAR%` expansion** on any platform. A literal `%LOCALAPPDATA%` value fails containment and
   exits 78, which is diagnosable.
3. Refuse a **relative** value (R-W2).
4. Normalise lexically; apply C-017 / C-018 / RUL-4 / R-W1 **component-wise**.
5. Canonicalise the **nearest existing ancestor**, re-join the tail, re-apply step 4. Canonicalise
   the anchors too, or a symlinked `$HOME` mis-refuses.
6. C-019 on the root when it exists, on the **nearest existing ancestor** when it does not — that
   is the directory the renderer will create under.
Creating the directory is the **renderer's** job (WP-7, R-W19). Recorded as **D-V16**.

#### RUL-6 — writable-ancestor walk is out of scope
The ADR already defers the full component walk with reparse-point rejection and Windows
effective-ACL evaluation, as residual R9. Containment is the mitigation. Do not add it.

#### RUL-7 — the `config/error.rs` route is GRANTED
Without the `#[error(transparent)] From<ToolchainRootError>` variant and its `classify` arm, no
caller can `?`-propagate the refusal and exit 78 is unreachable from the CLI — a green
indistinguishable from never having run.

#### RUL-8 — the `resolve_toolchain_home` bypass is a documented residual, routed to WP-8 as R-W20
WP-4 states the hazard in `ToolchainRoot`'s doc comment and closes nothing; the fix is a signature
change at the call site that builds a home. A doc comment is not a guard.

#### RUL-9 — leave `fold_project_tier` alone
R-W2 names three tiers and the project tier is not one; the function has no production caller.

### WP-5

#### RUL-10 — C-025: the host question moves to the call site
`exposed_names` keeps C-021's signature and returns the **unfolded** map. The module also exposes a
pure `fold_case_insensitive(map) -> map` applying the last-walked-wins collapse on the case-folded
key; the renderer calls it when the home is on a case-insensitive filesystem. A `cfg!(target_os)`
branch inside `exposed_names` would be untestable cross-host — the unreachable-red class. Recorded
as **D-V18**.

#### RUL-19 — the case-fold winner keeps its original spelling
The winner keeps its ORIGINAL spelling in `bin/` and in `RenderStamp.names`. A package's
`binaries` claim of `Make` is what the package declares, and on a case-insensitive filesystem
`bin/Make` and `bin/make` are one file anyway — so the fold's job is to stop a second **write**,
never to rename a tool.

#### RUL-11 — `NameOwner::shadowed` order — CONFIRMED
Every losing claim, in walk order, oldest first.

#### RUL-12 — C-023 call shape — CONFIRMED
One call over the merged multi-root node list, so "last walked wins" has a well-defined total
order.

### WP-6

#### RUL-13 — the `$`-expansion deviation is ACCEPTED; the builder was right
Inside double quotes the `:-` *word is still expanded*, and `LauncherSafeString` deliberately
admits `$` and backtick, so the literal `"${OCX_BINARY_PIN:-<abs path>}"` would command-substitute
on any `$OCX_HOME` containing one. Ship the single-quoted assignment line plus
`"${OCX_BINARY_PIN:-$__ocx_binary}"`. Recorded as **D-V19**.

#### RUL-14 — C-033 strip scope is `.exec` only, not unconditional
D-V3 already corrected the POSIX counterpart to trampoline-only. Recorded as **D-V20**.

#### RUL-15 — body signature ACCEPTED
`unix_trampoline_body(&TrampolineTarget, Option<&Path>)`; `TrampolineTarget` stays the pure home
selector C-028 names.

#### RUL-16 — `.exec` probe order, overlap, case — CONFIRMED as shipped
`["shim", "shimref", "exec"]`; the overlap is pinned by an explicit test row; `global` is
byte-equal only.

#### RUL-17 — `Cargo.toml` `windows-sys` feature — authorised
Feature entry only. No new crate, no version change.

#### RUL-20 — non-absolute project root is REFUSED at render
Refused in both `unix_trampoline_body` and the `.exec` writer, with C-030's refusal shape. Without
it `Project("global")` emits a sidecar byte-identical to the global home's, while
`EXEC_SIDECAR_GLOBAL`'s doc asserts that collision is impossible. Recorded as **D-V21**.

## Wave 3 — WP-7 (`plan_toolchain_activation.md`, D-V28)

**Wave-3 orchestrator rulings RUL-21…RUL-37, settling what WP-7's contracts left open.** Each
closes an Unchecked Green or a contradiction the wave-3 Verify-Architecture and edge-case passes
found in text the plan wrote, not in the ADR.

#### RUL-21 — the `?`-propagation route is a `PackageErrorKind` variant
`ToolchainPathError`'s `?`-propagation route (R-W14) is one `PackageErrorKind` variant in
`package_manager/error.rs`, not a `crate::Error` variant; WP-7's file set extends by that file.

#### RUL-22 — a partially-skipped render still writes a stamp
A partially-skipped render **writes** a stamp recording only what landed.

#### RUL-23 — `pinned = true` suppresses the entire link pass, including prunes
`pinned = true` suppresses the **entire** link pass: no writes, **no prunes**.

#### RUL-24 — a home-root creation failure is C-050's skip, not a hard error
A home-root creation failure is C-050's skip, not a hard error.

#### RUL-25 — a `-g`-narrowed pull reconciles `bin/` only when the default group is selected
A `-g`-narrowed pull reconciles `bin/` **only when the default group is among the selected
groups**; otherwise `bin/` is untouched.

#### RUL-26 — Windows `bin/` fingerprints both on-disk files under their own names
On Windows both on-disk files (`<name>.exe`, `<name>.exec`) get their own `bin_fingerprint` entry
under their full on-disk names.

#### RUL-27 — the stamp describes the tree as it stands on disk, never the intended tree
The stamp always describes the tree **as it stands on disk at stamp time**, never the intended
tree; under `pinned` that means one `readlink` pass over the default group.

#### RUL-28 — `--dry-run` project-registration failure is a swallowed warn
`--dry-run` does not register the project; a registration failure is a swallowed warn.

#### RUL-29 — `heal_links` creates an absent link
`heal_links` **creates** an absent link.

#### RUL-30 — the render body runs under one `lock_scoped`
The render body runs under one `lock_scoped`; a timeout falls to C-050's skip.

#### RUL-31 — render and heal both select the platform through the shared `select_best`
Render and heal both select the platform through the shared `select_best`, never an exact key
lookup.

#### RUL-32 — an orphan directory is `Skipped`, never removed recursively
An orphan that is a directory is `Skipped`, never removed recursively.

#### RUL-33 — `ensure_home_root` refuses a symlinked home root or `bin/`
`ensure_home_root` refuses a home root or `bin/` that is a symlink, routed to C-050's skip.

#### RUL-34 — the collision debug line already ships in `record_claim`
The collision debug line is `record_claim`'s, already shipped; the renderer emits nothing extra.

#### RUL-35 — WP-7 owns the Windows `bin/` publish
WP-7 owns the Windows `bin/` publish (`.exe` hardlink from `ShimBinStore` plus `.exec` sidecar).

#### RUL-36 — `heal_links` never errors on lock contention
`heal_links` never errors on lock contention: `Ok(count)`, unrepaired entries uncounted and logged
at debug.

#### RUL-37 — `ensure_home_root` runs before the case-fold probe
`ensure_home_root` runs **before** the case-fold probe.

> **Why these are load-bearing** (from D-V28's rationale column): without **RUL-22/27** the stamp
> records an intent the tree does not match, so C-061's gate withholds on every prompt **forever**
> — the exact failure C-003 cites when it refuses to widen `link_fingerprint`, and it reappears
> under `pinned` (RUL-23) unless the stamp observes rather than intends. Without **RUL-25** a
> narrowed `ocx pull -g ci` must resolve the default group's metadata the invocation never asked
> for, adding a network dependency and a failure mode to a narrowed command. Without **RUL-26** the
> Windows `readdir` gate mismatches permanently, since `bin/` holds two files per name and
> `RenderStamp::names`' shipped invariant is "the on-disk entry set". Without **RUL-31** the
> renderer and the heal disagree on a *compatible* non-identical platform key and oscillate on
> every invocation. Without **RUL-33** C-053's containment is lexical only: a hostile clone
> committing `.ocx/toolchain` as a symlink to `$HOME` turns the prune into a delete primitive
> outside the project. **RUL-23** and **RUL-32** both refuse a delete: `pinned` is a property of
> what the *composition* yields (C-046) and must not remove live derived state, and a recursive
> delete inside an attacker-writable tree is not worth the convenience.

## Wave 3b — WP-7 Implement/review (`plan_toolchain_activation.md`, D-V31)

**Wave-3b orchestrator rulings RUL-38…RUL-47, settling what WP-7's Implement and review passes left
open.**

#### RUL-38 — one `lock_scoped` call takes a `ScopedLockParameters` value
One `lock_scoped` call's arguments are a `pub(crate) ScopedLockParameters` value rather than five
spelled at each site, so a test can take *the same* lock the code under test takes.

#### RUL-39 — a denied state store fails the stamp write itself
A denied state store fails the **stamp write itself**: every item `Skipped`, `stamp_written ==
false`, still `Ok`.

#### RUL-40 — `artifact_path` spells a path without the grammar validator
`artifact_path` spells an artifact's path **without** `ToolchainHome::entry`'s grammar validator;
containment is `prune_within`'s job, so an orphan whose on-disk name the grammar refuses is still
prunable by that name.

#### RUL-41 — `RenderOutcome::Skipped::reason` is human prose with no grammar
`RenderOutcome::Skipped::reason` is human-facing prose with **no grammar**; the machine-readable
half is `path`, and tests assert the path plus non-emptiness only.

#### RUL-42 — `read_fault_hook` is deliberately untested
`read_fault_hook` is deliberately untested: a one-line `var_os` read with no branch, every
assertable behaviour reached through `render_with`'s `fault` parameter.

#### RUL-43 — a non-recursive delete of an empty orphan group directory is correct
RUL-32 means "never removed **recursively**": a non-recursive `remove_dir` of an *empty* orphan
group directory is `Pruned` and correct.

#### RUL-44 — a symlink between the project directory and a project-scoped home root is refused
For a `RenderStampScope::Project` home lying inside its project directory, a symlink on any
component **between** the project directory and the home root is refused into C-050's skip; the
guard is scope-gated in the renderer and **not** in `ensure_home_root`, because the global home's
parent is `$OCX_HOME`, which a user may legitimately symlink onto another volume.

#### RUL-45 — an existing home root's mode is not examined
An **existing** home root's mode is not examined; C-019's permission refusal stays scoped to
configured `toolchain-dir` roots.

#### RUL-46 — case-insensitive prune comparison is ASCII-case-folded
On a case-insensitive home the **prune** comparison is ASCII-case-folded in both `reconcile_bin`
and `reconcile_links`; the write pass is unchanged and the fold is never extended to group or entry
names.

#### RUL-47 — `heal_links` applies RUL-44's component guard too
`heal_links` takes the scope and applies RUL-44's component guard too, degrading to `Ok(0)`.

> **Why these are load-bearing** (from D-V31's rationale column): RUL-38–RUL-42 were cited as
> binding authority inside the shipped file while existing in no design record — a reader grepping
> the plan for RUL-40, the stated authority for bypassing the grammar validator on the module's
> only delete path, found nothing (this is the exact defect this artifact fixes, one wave later,
> for the full set). RUL-43–RUL-47 close what the panel raised. **RUL-46** — the prune compared
> case-sensitively against a folded expected set, so on a case-insensitive home a pre-existing
> `bin/make` under the winning spelling `Make` was left `Unchanged` by the write pass and then
> **deleted** by the prune, with the stamp recording an empty `names` consistent with the
> now-empty `bin/`, so C-061's gate was satisfied while the tool was silently gone. **RUL-44/RUL-47**
> — `ensure_home_root` refused a symlinked home root and `bin/`, one component too low: committing
> `<project>/.ocx` as a symlink relocated every write and prune, and `heal_links` reached the tree
> with no guard at all. **RUL-45** refuses an escalation: a project home is group-writable at
> git-checkout time under umask 002, and refusing it would skip every render on user-private-group
> distros.

## Wave 3b — WP-8 stub (`plan_toolchain_activation.md`, D-V33; detail in `wp8-rulings.md`)

**Wave-3b orchestrator rulings RUL-48…RUL-61, settling WP-8's stub.**

RUL-48–RUL-54 have two source definitions — the plan's D-V33 recap and the standalone
`wp8-rulings.md` (issued first, after the edge-case hunt). **Reconciliation: `wp8-rulings.md` is
kept as the canonical text below** — it is the primary, first-issued source with the full
supporting argument; D-V33's cell is a compressed restatement for the plan's own bookkeeping and
does not add or contradict anything. Where D-V33 phrases a ruling more precisely for file-set
purposes (RUL-48's per-file breakdown), that phrasing is folded in below since it is strictly more
specific, not a divergent ruling.

#### RUL-48 — five file-set extensions, each compile- or reachability-forced
- `crates/ocx_cli/src/api/data/shell_state.rs` — C-056's resolved-home field lives in
  `ShellStateReport`, not in `command/shell_state.rs`. Without it the contract is unimplementable
  and the `--format json` field unreachable.
- `crates/ocx_lib/src/package_manager.rs` — re-export lines only. The module re-exports every
  other task's types and omits `RenderRequest`, `RenderReport`, `RenderedItem`, `RenderedArtifact`,
  `RenderOutcome`; `ocx_cli` cannot spell a request without them.
- `crates/ocx_lib/src/project/toolchain_home.rs` — R-W20's signature change.
  `resolve_toolchain_home` has **no non-test caller in either tree**, so WP-8 creates the first one
  and takes `Option<&ToolchainRoot>` from birth; the bypass is closed by construction rather than
  by a doc comment.
- `crates/ocx_lib/src/cli/progress.rs` — R-W24's comment. **The plan's file column is wrong**: the
  file is in `ocx_lib`, not `ocx_cli`. Comment only; the `CONOUT$` arm stays closed, as R-W24
  decided.
- `crates/ocx_cli/src/app/project_context.rs` — one module doc comment. It asserts "a pure-`ocx
  pull` … no longer auto-registers the project on first pull"; C-052 falsifies it in the same
  commit. A doc asserting a guarantee the code lacks is this codebase's most repeatable defect.

#### RUL-49 — D-V8's shared orchestration function lives in `package_manager/mutate.rs`
The shared orchestration function lives in `crates/ocx_lib/src/package_manager/mutate.rs` (new),
not in `project/mutation.rs`. D-V8's three requirements are "one shared function", "in `ocx_lib`,
not `ocx_cli`", and "not inside `MutationGuard::commit`" — all three hold either way, and `project`
importing `package_manager` inverts the shipped `package_manager → project` direction
(`project/mutation.rs` imports nothing from `package_manager` today). `project/mutation.rs` stays
in the file set only for a `pub(crate)` widening, if one is needed.

#### RUL-50 — `--pinned` on the global tier is accepted, not a usage error
There is a global `ocx.toml`, so `pinned` has a producer on both tiers; the flag selects the
emitter lane (C-066) and is meaningful wherever a composition is.

#### RUL-51 — `shell state` needs no new exit path for an unresolvable home
A `toolchain-dir` that fails C-017–C-019 is refused at **config load**, so every command including
`shell state` already exits 78 before the report is built. `ToolchainRoot::resolve` has no
filesystem side effect and accepts a root that does not yet exist (RUL-5), so once the config
parses the resolved home is always spellable: the field is **always present**, never `null`, and
`shell state` keeps its exit-0-in-every-reportable-state contract.

#### RUL-52 — the C-050 warn line interpolates the untrusted name with `{:?}`
`RenderOutcome::Skipped { path, reason }`, `RenderedArtifact::Trampoline(String)` and
`GroupDirectory(String)` all carry a `read_dir` name a hostile clone controls, and WP-8 renders
them into a `warn!`. Follow the convention `ToolchainPathError` states at
`crates/ocx_lib/src/file_structure/toolchain_store.rs:161` (CWE-117).

#### RUL-53 — a render skip never rolls back the mutation
C-054's shared function sequences `guard.commit(...)` then `render_toolchain(...)`; a C-050 skip
after the commit is exit 0 with a warn, and the lock stays written. The half-rendered tree is what
the stamp gate (C-061) is for.

#### RUL-54 — `--dry-run` stays `pull`-only
`add`, `remove`, `lock` and `update` do not grow one implicitly through the shared function.

#### RUL-55 — `--project` learns to accept a directory
`--project` learns to accept a directory, resolving it to `<dir>/ocx.toml`; the trampoline's baked
selector is unchanged. **Load-bearing**: without it every rendered trampoline exits 74.
`ConfigLoader::resolve_explicit_project_path` (`config/loader.rs:763`) refuses anything that is not
a regular file, and `unix_trampoline_body` bakes `--project '<abs project root>'` — a directory.
Measured: `--project <dir>` → 74, `--project <absent>` → 79, bare → 64. Baking `<root>/ocx.toml`
instead would contradict C-028's home selector, C-030/C-031's sidecar grammar, WP-6's goldens,
`EXEC_SIDECAR_GLOBAL`'s doc and the committed shim blobs. A named directory holding no `ocx.toml`
**is** `NoProject`.

#### RUL-56 — file-set extension for RUL-55
`ocx_lib/src/config/loader.rs` and the file carrying `--project`'s `value_name`.

#### RUL-57 — the shared function's sequence is commit → render → consent → materialize
commit → render → consent → materialize.

#### RUL-58 — a hard render failure after a successful commit is exit 0 with a warn
A hard render failure after a successful commit is exit 0 with a warn.

#### RUL-59 — `-g` on `lock`/`update` scopes resolution only
`-g` on `lock`/`update` scopes resolution only; the re-render is whole-home, and RUL-25's narrowing
is `pull`'s alone.

#### RUL-60 — `--pinned` is a `--pinned`/`--no-pinned` pair, not a single flag
`--pinned` is a `--pinned`/`--no-pinned` pair, not a single flag. `lazy_mode.rs`'s own doc says it
is deliberately not a paired toggle *because* its value set is open-ended; `pinned` is closed
two-valued, so the same rule makes it the `options::Pull` shape, which is also the only shape that
can override an `ocx.toml` `pinned = true` back to following.

#### RUL-61 — a `#[cfg(test)]` constructor is granted
A `#[cfg(test)]` `ToolchainRoot::from_validated` in `config.rs` is granted.

## Wave 3b — WP-8 Implement, WP-9, WP-15 round 1 (`wp8-rulings2.md`)

WP-8 orchestrator rulings, round 2 (after Specify) — binding.

#### RUL-69 — C-058's derivation must be unit-testable
`trampoline_lookup_exclusions` takes `&crate::app::Context`, which is only constructible through
the async `Context::try_init` — and that installs the global tracing subscriber, so no unit test
can build one. The derivation is therefore indistinguishable from a literal
`join("toolchain").join("bin")` at every level a test can reach, which is the unreachable-red
class. **Change the signature** to take what it actually needs — the `FileStructure` and the
resolved `ToolchainHome` (or the two `PathBuf`s) — and let the caller do the `Context` lookup. An
untestable signature is a design defect, not an acceptable cost.

#### RUL-70 — the default group is always in scope for `commit_and_render`
The Specify phase's reading is correct: a literal "every group the new lock declares" lets `ocx
remove` of the last default-group tool leave `bin_in_scope == false` and the removed tool's
trampoline alive, contradicting S-008. RUL-25's narrowing is `pull`'s alone, where the user asked
for it.

#### RUL-71 — `--pinned` at the package tier is refused
`ocx package env` and `ocx package exec` compose one package's closure and have no link lane to
choose. `ocx direnv export` is deliberately left open: C-065/C-070 name it a composing emitter, and
the flag there is WP-15's call.

#### RUL-72 — `ocx pull` takes no `--pinned`
C-055 names `env` and `exec` and nothing else. C-046 makes the flag inert on the tree — under
`pinned` the bodies are byte-identical and the link pass is suppressed — so a per-invocation
override on `pull` would change nothing a user could observe and would read as if it did. The
render's `pinned` comes from the ladder.

#### RUL-73 — a CLI-supplied group name exits 64, not 78 (a WP-3 regression, fixed here)
`test/tests/test_project_add.py::test_add_rejects_path_traversal_group_name` is red on `goat`: `ocx
add --group '../../etc'` used to exit 64 through `ProjectErrorKind::InvalidGroupName`, and WP-3's
C-014 charset validator now reclassifies it as a config error, 78. A value typed on the command
line is a **usage** error; 78 belongs to what a config file contains. WP-3's RUL-2 required
auditing every fixture the validator newly rejects — this one was missed. Fix it in the one
function all four mutation commands route through, `crates/ocx_lib/src/project/mutate.rs`'s group
validation, so the charset check there yields `InvalidGroupName`/64 while `project/config.rs`'s
parse-time validator keeps 78. **File-set extension granted**: `crates/ocx_lib/src/project/mutate.rs`
(WP-11's file, merged, no concurrent writer). Do not weaken the validator and do not edit the
acceptance test.

#### RUL-74 — a dry run never probes for case sensitivity

> Source note: unlike RUL-69–73, this id has **no entry in `wp8-rulings2.md`**. Its only
> definition anywhere under `.claude/` is the D-V35 recap sentence in
> `plan_toolchain_activation.md`, transcribed in full below.

A dry run **never** probes for case sensitivity; the earlier B4 fix stopped the probe writing
outside an existing home and left it moving the home's own mtime on every `--dry-run`.

## Wave 3b — WP-9 (`wp9-rulings.md`, all rounds)

WP-9 orchestrator rulings (wave-3b) — binding, issued after the edge-case hunt, plus three further
rounds after the stub, Specify and Implement passes.

#### RUL-62 — C-060's PATH order is the reverse of the desired vector
`Shell::export_path` prepends (`utility::path::move_to_front`), so the **last** entry emitted ends
up frontmost on PATH. To satisfy C-060 — `ocx_install_bin_path`, then the project `bin/`, then
`$OCX_HOME/toolchain/bin`, front to back — the desired order is the reverse: `$OCX_HOME/toolchain/bin`,
project `bin/` (bin mode only), `ocx_install_bin_path` **last**. **Do not reuse
`setup::session_path_directories`'s slice verbatim**: it documents `directories[0]` as nearest the
front, because its consumers build `PATH=d0:d1:$PATH`, and copying it yields exactly the CWE-426
order C-060 exists to forbid. The test asserts the **resulting PATH string**, never the desired
vector — an assertion on the vector's order cannot red the defect it is meant to catch.

#### RUL-63 — one spelling of one directory
`ocx_install_bin_path` exists twice: `crates/ocx_lib/src/setup.rs:365` and a private duplicate at
`crates/ocx_cli/src/command/self_group/activate.rs:801`. Two spellings of one directory make
`repair_owned_segments` delete one and add the other on every prompt. Delete the CLI copy and use
the `setup` one; `setup.rs` itself needs no edit.

#### RUL-64 — file-set extension: attribute deletions only
`crates/ocx_lib/src/package_manager/tasks/render_toolchain.rs`: the
`#[cfg_attr(not(test), expect(dead_code, …))]` pair on `heal_links` (and on `link_target` if WP-9
reaches it) must be deleted the moment WP-9's call lands — `unfulfilled_lint_expectations` is `-D
warnings`, so the compiler forces it, but it reads as an unrelated error if unexpected. Same
precedent as R-W18(b).

#### RUL-65 — file-set extension: the acceptance fixture
`test/src/shell_matrix.py` (`BASE_PATH`, `Arena.env`) must seed `ocx_install_bin_path` and
`$OCX_HOME/toolchain/bin`, or C-059 flips six shipped `test_shell_reconcile.py` cases with no owned
way to fix them. The file is WP-12e's, wave 5 — not concurrent. A wave-4 package does not land
leaving the acceptance suite red. **Superseded by RUL-85** — the named seam did not exist.

#### RUL-66 — `render_stamp.json` is not in the shell-side watch set — accepted as designed
`fingerprint.rs`'s `watch_paths` covers `ocx.toml`, `ocx.lock` and the consent stamp. `ocx pull`
re-saves the lock, so S-001's "run `ocx pull`, the entry appears" holds. Only a render touching
neither file — a bare heal, `pull --dry-run`, a `toolchain-dir` move — stays invisible until the
next watched change, and that is C-064's designed stale window seen from the other side. Adding the
stamp would put a stat on every prompt in every mode to buy a case `ocx pull` already covers.
Routed as **R-W49**, not fixed.

#### RUL-67 — the stamp gate compares set equality, both directions
A `bin/` entry the stamp does **not** name is a mismatch, exactly like a named entry that is
absent. A one-way lookup over `bin_fingerprint`'s keys lets **S-003's hostile force-committed
`bin/cmake` reach PATH** the moment every other entry matches — the scenario the gate exists for.
`RenderStamp::names` documents itself as "the on-disk entry set"; honour that literally.

#### RUL-68 — `Outcome` carries the home
`owned_prefixes` is assembled at `crates/ocx_cli/src/command/self_group/activate.rs:352-355` from a
single-element slice, and `plan_for` cannot re-derive the project home. C-063's `<home>` reaches
that site as a field on `Outcome`, minted only after `ConsentProof` (`activation.rs:662`) and only
for an in-scope project.

### Round 2 (after the stub)

#### RUL-85 — RUL-65 retargeted to `shell_matrix.py::clean_env`
RUL-65 retargeted: the seam is `test/src/shell_matrix.py::clean_env`, not `BASE_PATH` (a module
constant with no `$OCX_HOME` in scope) and not `Arena.env` (which lives in the two test files and
delegates to `clean_env`). Applied in **Implement**, in the same edit that makes C-059 emit.

#### RUL-86 — route `trampoline_ocx_binary` through `setup::ocx_install_bin_path`
`crates/ocx_lib/src/package_manager/launcher/generate.rs` granted: `trampoline_ocx_binary`
re-derives the install bin directory inline, so RUL-63 would otherwise leave two spellings. Route
it through `setup::ocx_install_bin_path`. WP-6's file, merged, no concurrent writer.

#### RUL-87 — C-018 × C-059 interleaving order
Desired is `global ++ session ++ project`, front-to-back: project-composed ▸ `install_bin` ▸
project `bin/` ▸ `$OCX_HOME/toolchain/bin` ▸ global-composed. C-060 fixes only the relative order of
its three, which this keeps; the alternative puts the **global** tier's composed entries ahead of
the project's own `bin/`, so a global tool shadows a project trampoline. C-060's CWE-426 rationale
no longer describes a reachable attack: post-D-V19 the body execs a single-quoted absolute
`$__ocx_binary`, never a bare `ocx` through PATH.

#### RUL-88 — C-061's hint rides `Outcome::messages`, not `debug!`
Every emitted hook redirects stderr to `/dev/null`, so a debug line cannot deliver the hint S-001
and C-064 both promise the user sees. Same reasoning C-034 applies one function up.

#### RUL-89 — one `BinEntryStamp::of_file` constructor
One constructor granted on `crates/ocx_lib/src/file_structure/state_store.rs`:
`BinEntryStamp::of_file(&Path, &Metadata)`, with **both** ends routed through it
(`render_toolchain::fingerprint_bin` and `activation::bin_stamp_matches`). Spelling
`hex::encode(Sha256::digest(read_bounded(...)))` twice is the contract-documented-at-one-site-
violated-at-a-sibling class this plan has already hit twice.

### Round 3 (after Specify)

#### RUL-90 — one spelling of the `<current>/content/bin` derivation
`crates/ocx_lib/src/shell/reconcile/fingerprint.rs` granted to WP-9, for the
`<current>/content/bin` derivation only. RUL-86 did not close "one spelling": three production
derivations existed (`setup.rs:365`, `generate.rs:196-202`, `fingerprint.rs:176-181`). Route all
three through `setup::ocx_install_bin_path`.

#### RUL-91 — `Outcome::owned_home` is `Some` regardless of mode
`Outcome::owned_home` is `Some` for any consented, in-scope project **regardless of mode**. Under
the `bin`-only reading, `bin → none` could never subtract `<home>/bin`, which lives outside
`$OCX_HOME` and is therefore unreachable by that prefix — the entry would strand on PATH with
nothing authorised to remove it.

#### RUL-92 — `next_ledger` does not record the two session entries
They are unconditional, so there is nothing to retire, and recording them puts two rows in every
carrier for a decision they can never drive.

#### RUL-93 — RUL-88's hint has no reachable red at this layer
The message channel is `session()`'s and reaching it needs a consent-stamp fixture. Implement wires
the hint through `Outcome::messages`; the observable assertion is routed to **WP-12b**
(`test_shell_reconcile.py`).

### Round 4 (after Implement)

#### RUL-94 — the `(false, false)` summary arm folds to `+`, not `-`
`crates/ocx_lib/src/shell/reconcile/plan.rs` granted. RUL-92 made `summary`'s `(false, false)` arm
**reachable**: the session directories are in every prompt's desired set and deliberately
unrecorded, so a prompt whose only `PATH` contribution is the session block names a key neither
ledger holds. The arm folded to `-`, printing `ocx: -PATH` on the very prompt ocx *started* setting
`PATH`. Flip to `+` and correct both restatements of the now-false "every key `sets` can name is in
`after`" claim. **Not** fixed by reversing RUL-92 — recording the session entries would hand
`repair_owned_segments` authority to retire them, which is the C-059 defect in another shape. One
new test pins `+`, red-proven against `-`.

#### RUL-95 — three fixture patches in `test_shell_reconcile_edge_cases.py`
`test/tests/test_shell_reconcile_edge_cases.py` granted (three patches). C-059 legitimately moves
each fixture's expected `PATH` shape while the contract it names survives: a foreign prepend still
survives in order, the ambient tail is still byte-identical, the POSIX emit still carries no bare
separator. Not routed to WP-12b — that is wave 6, these are red now, and WP-9 caused them. Added to
WP-9's acceptance gate.

## Wave 3b — WP-15 (`wp15-rulings.md`, all rounds)

WP-15 orchestrator rulings (wave-3b) — binding, issued after the edge-case hunt, plus two further
rounds after the stub and after file-ownership review.

#### RUL-78 — `${deps.*}` stays on digest paths
C-065 says "every dereference value", but a **dependency is not a lock entry**, so no
`<group>/<entry>` link exists for it and no trust test can be asked. `link_target` takes a
`&LockedTool` for that reason. The link lane covers what the tree actually contains — the selected
groups' entries — and `build_dep_context_map`'s `${deps.NAME.installPath}` keeps its digest path.
This is not a scope cut: there is no link to emit. Adding a lock lookup to invent one would mint
the second producer of the platform selection that RUL-31 exists to prevent.

#### RUL-79 — no carve-out for a deferred root — CLOSED
A link whose target is not yet materialized and a digest path to the same directory are the *same*
path — heal creates the link pointing at `packages.path(&pinned)`, which is what the digest arm
would have emitted. WP-7 already ships
`healing_leaves_a_correct_link_alone_even_when_its_target_does_not_exist`. So no lane split on
`ContentState::Deferred`. **Round 2 closure**: no `ContentState::Deferred` carve-out. C-013's
suppression is keyed on `content_state`, derived from `root.deferred()` and independent of the
emitted path spelling, so the `required` probe is suppressed either way.

#### RUL-80 — the trust probe compares raw targets, never canonicalised
Compare `read_link` against `link_target`'s answer **without canonicalising** — `heal_links`
compares raw targets, and a composer that canonicalises would disagree with the repairer and
follow a hostile target the heal refused. Probe link-ness with `crate::symlink::is_link`, never
`Path::is_symlink`: Windows links here are NTFS junctions.

#### RUL-81 — the trust answer is a per-entry `read_link`, not the heal's count
`heal_links` returns a count of *repairs*, not of trustworthy entries, and it calls
`ensure_home_root`, which **creates** the root — so neither its return value nor
`home.root().exists()` can answer "is there a rendered tree". Ask per entry at emit time, after the
heal, and degrade that entry to a digest path on any answer but a link matching the lock-derived
root (C-067: a mismatch is treated as absent, never as usable).

#### RUL-82 — one `install_path_for` seam
Since C-065's "one code path" is not true today: the hunt found six spellings of a digest path,
three of them emitted from two different producers (`InstallInfo::dir()` for a root,
`PackageStore::package_dir` for a dep). Introduce one `install_path_for(...)` seam and route
`emit_package_vars`'s `content`, `synth_entrypoints_path_for` and — on the digest arm —
`build_dep_context_map` through it. **Must stay digest and must not be touched**:
`synth_shim_path_for` (a shim store, not a package), `tc_entry_object_data` and `ComposeRoots`'
`store.package_dir` (read paths for `metadata.json` / `resolve.json`), `launcher/exec.rs:148` (a
fifth `EnvScope::Project` site, pinned by construction through a baked `pkg_root`), and every
persisted artifact the hunt enumerates — `packages/**/*.json`, `refs/**`, generated launcher
bodies, the render stamp, the execution record.

#### RUL-83 — `pinned` is WP-15's to resolve and thread
No production site assembles `Ladder<bool>` today; `options::Pinned` arrives from WP-8 parsed but
unconsumed (RUL-76). WP-15 assembles the ladder — CLI tier, `OCX_TOOLCHAIN_PINNED`, `ocx.toml`,
floor — and threads the resolved value to the composer through `EnvScope::Project` or a new
argument on `resolve_env_with_attribution`, the single `composer::compose` caller.

#### RUL-84 — WP-9 deletes the `expect(dead_code)` pair on `heal_links`
WP-9 merges before WP-15 and its C-062 call lands first, so **WP-9 deletes it**. If WP-15 somehow
lands first, it deletes it instead and says so. Neither adds an `#[allow]`.

### Round 2 (after the stub)

#### RUL-96 — `ComposePaths`'s production caller and the seven construction sites
`crates/ocx_lib/src/package_manager/tasks/resolve.rs` granted, plus the seven `EnvScope::Project`
construction sites (`ocx_cli/src/command/{toolchain_exec,toolchain_env,direnv_export,launcher/exec}.rs`,
`ocx_lib/src/activation.rs`, one test site). All WP-8's or WP-9's, both merged. Without it
`ComposePaths` has no production caller and C-065 is an Unchecked Green. `launcher/exec.rs` passes
`pinned = true` — a baked `pkg_root` is pinned by construction.

#### RUL-97 — integration payloads follow the link
C-065's literal text, and integrations are not on RUL-82's must-stay-digest list. A consumer that
*persists* a payload pins it at that consumer, not here.

#### RUL-98 — collapsing two lock entries to one digest key: tie-break order
First group in `groups` order, then first in `lock.tools` order. Both links name the same
directory, so only the emitted spelling differs.

#### RUL-99 — `pinned_for_project` stays in `composer.rs`
`lazy_mode_ladder_for_package` is the local precedent, and relocating it would pull `project/` into
the file set for tidiness alone.

### Round 3 (file ownership)

#### RUL-102 — `test_toolchain_render.py` is WP-12a's file
`test/tests/test_toolchain_render.py` is **WP-12a's** file in the plan; WP-15 created it first
because C-070's acceptance home had no other existing file and WP-7 merged without creating it.
WP-12a **extends** it and builds `test/src/toolchain_fixtures.py` around what is already there — it
does not recreate the file and does not own the whole of it.

#### RUL-103 — `test_path_idempotency.py` granted to WP-15
Its `test_direnv_export_is_idempotent` filtered PATH segments with `if "packages" in seg`, encoding
the **digest spelling** as the identity of a tool's bin directory. C-065 changes exactly that for a
project-tier emitter, so the filter goes empty and `assert tool_dirs` reds — a hard red by
construction, not a defect. Repaired by identifying the project's segments by exclusion from the
seed, plus a positive C-065 pin. No other work package owns the file, and leaving it red would
block the PR under the acceptance-is-a-merge-gate rule.

#### RUL-104 — the global tier is a composing emitter and follows the rendered link tree
*Reconstructed 2026-09-06 from its three citation sites, not recovered from a ruling record —
see "Cited but never defined" below for provenance.*

`ocx --global pull` renders `$OCX_HOME/toolchain/<group>/<entry>`, so the global env exporter
follows that tree exactly as the three project-tier composing emitters do (C-065), and resolves
the `pinned` lane through the same ladder (C-007) — taken in the same match arm as the config it
reads, so an unparseable global file yields the floor, which is the following lane, exactly as a
file stating nothing would.

Two bounds ride with it:
- The group set handed to the emitter is the **lock-derived** one, never the config-expanded
  env-group set: a group declaring only `[group.<g>.env]` and no tools has no links, and C-070 is
  precisely about not passing the wrong set.
- `toolchain` is `None` only when there is no global lock to derive links from — C-067's digest
  lane by construction, not a degraded state.

Cited at `crates/ocx_cli/src/command/toolchain_env.rs:623`, `:673`, `:742`.

## Wave 4 — WP-12b + WP-12d design half (`wp12bd-rulings.md`)

Design half, run before WP-12a landed. Both hunts read-only, Opus. (`RUL-106`…`RUL-114`; identical
in `rulings_toolchain_activation_wp12bd.md`.)

### WP-12b

#### RUL-106 — WP-12b's file set gains `test_toolchain_env.py` (Block, from hunt finding P1)
The parity oracle cannot live in `test_env.py`: that file is the OCI/package tier
(`EnvScope::Package`), and `toolchain_links()` returns `None` for it unconditionally
(`crates/ocx_lib/src/package_manager/resolve.rs:309-314`). `None` **is** the digest lane, so a
following-vs-pinned oracle written there emits digest paths in **both** lanes and passes in both
states — the fifth instance this run of a test that asserts a spelling while naming an identity,
and the first one caught before it was written. `test_env.py` stays in the set for its non-oracle
items. No concurrent writer: WP-15 (merged) is the file's last author.

#### RUL-107 — drive `direnv export` from inside `test_toolchain_env.py`
`ocx direnv export` is a composing emitter and belongs in the oracle, but `test/tests/test_direnv.py`
is **not** granted. Drive `direnv export` from inside `test_toolchain_env.py` — it is a toolchain-tier
emitter and the file is the toolchain tier. One grant, not two.

#### RUL-108 — item 21's acceptance scope is cut
Cut to: file tier beats environment tier; environment tier speaks when the file tier is absent; an
unrecognised value warns exactly once. The `Option`-level distinction R-W11 raises —
"unrecognised = absent" versus "unrecognised = short-circuit to the floor" — is
**acceptance-unobservable in every tier arrangement**, not just the one R-W11 names: with the file
tier absent and `OCX_TOOLCHAIN_ACTIVATE=garbage`, both readings yield `Env`, because the floor sits
directly under the environment tier. It stays pinned at the unit layer, where wave 1 already
asserts it. Writing an acceptance case for it would be a green that cannot red.

#### RUL-109 — "Hook import" struck from WP-12b's Scope cell
It has no referent in the plan, the ADR, or `shell/hook.rs`. The cell is rewritten to name the
seven items it actually owns and never listed — 8, 19, 20, 21, 26, 27, 28. Nothing was orphaned
(ownership binds by file), but the L estimate was made against roughly half the package.

#### RUL-110 — the `ocx`-lookup case uses `command -v` only, never exec
Every `matrix.run_script` call in WP-12b passes an explicit `timeout=`. R-W12 is still open: a
project trampoline named `ocx` re-execs itself inside `/bin/sh` before any ocx process starts, so
the failure mode is a hang, not a non-zero exit. `test/src/runner.py` is **not** edited — it has no
owner and a global default timeout is a behaviour change across 156 test files.

### WP-12d

#### RUL-111 — the stale S-010 test is collapsed, not inverted
`test_s010_windows_composes_eagerly_while_other_hosts_compose_a_shim`
(`test/tests/test_lazy_loading.py:860`) splits on host; C-027 deleted the `cfg!(windows)` floor, so
`resolve_for_host` is host-independent and the split is itself the stale artefact. Take the `else`
body unconditionally, rename to `test_s013_lazy_mode_always_defers_on_every_host`, add the one
Windows-discriminating clause (`<name>.exe` and `<name>.shimref` present, `<name>.shim` absent).
Same transformation WP-5 already performed in `lazy.rs`.

#### RUL-112 — the census is 16 occurrences, not 18; verdicts 8 DELETE, 10 KEEP
`test_execution_records.py:531` and `test_execution_record_standards.py:395` read
`"…POSIX-only in this phase"` with no `(S-010)` suffix. A `sed` on R-W22's literal silently misses
two of the eighteen. Every KEEP carries a condition naming something the test **observes** —
`os.pathsep != ":"`, `not hasattr(os, "killpg")`, `not Path("/bin/sh").exists()`, `not hasattr(os,
"setsid")`, `not hasattr(pexpect, "spawn")` — never a phase or a plan state. Six of the eight
DELETEs need the trigger retargeted from `run_after_sourcing` to a direct `subprocess.run` on the
launcher path; the shell was only ever a trigger, and `test_lazy_loading.py:603` already proves
PATH resolves to that same launcher. One in-scope fix rides along:
`test_lazy_direnv.py:289`'s `assert "/content/bin" in partial.stdout` is separator-literal and
would red on Windows for an unrelated reason.

#### RUL-113 — the consequential one: Windows cases take the registry-free committed-blob fixture
WP-12d **may add `tests/test_trampoline_exec.py` to `build-windows-shims.yml`'s pytest invocation**
(one line, ~:293), and its Windows cases take the **registry-free committed-blob fixture**: copy
`crates/ocx_lib/src/shims/ocx-shim-x86_64.exe` into a fixture `bin/`, hand-write the `.exec`
sidecar, observe argv through an `OCX_BINARY_PIN` `.cmd` script. Two facts force this. (1) Item
6.2's dispatch arm as written is **unrunnable**: the only Windows pytest job runs
`tests/test_windows_shim.py` alone, builds only `ocx_shim` (no `ocx`), and sets
`OCX_TESTS_NO_REGISTRY=1`, which skips every registry-dependent test — so "drive `ocx pull` then
invoke `toolchain/bin/<name>.exe`" has nothing to run on. (2) `OCX_SHIM_BINARY` is read in exactly
one place in the tree (`test/tests/test_windows_shim.py:63`) and CI points it at
`target/debug/ocx-shim.exe`, so **all 12 existing Windows shim tests execute a locally built
substitute and none has ever executed a committed byte** — R-W36 confirmed at the source.
Therefore: the Windows cases must **not** use `OCX_SHIM_BINARY` and must **not** reuse
`test_windows_shim.py`'s `shim_entrypoint` fixture. Say so in the docstring, or the next refactor
silently un-tests the blob.

#### RUL-114 — R-W46 gets no WP-12d case
`join_segments` (`crates/ocx_lib/src/utility/path.rs:110`) delegates to `std::env::join_paths`,
whose Unix arm quotes nothing, so on Linux the fixed and pre-fix forms produce identical bytes for
every input. The red is Windows-only and, per RUL-113's finding (1), has no job to run on today.
Recorded as unreachable rather than covered by a case that cannot red.

## Cited but never defined

**RUL-104** — cited three times in the shipped `crates/ocx_cli/src/command/toolchain_env.rs`
(lines 623, 673, 742 on `goat`), e.g. `// The third element is the resolved pinned lane (C-007,
RUL-104), taken …`. **No source under `.claude/` — ruling files, the plan, or any artifact —
defines it.** This is not a scope decision on this fold's part: it was searched for across every
ruling file listed in *Sources* above, across the plan's full text, and across `.claude/artifacts/`
and `.claude/rules/`, with zero hits outside the citing file itself (and its identical copies in
stale `.claude/worktrees/` checkouts, which are not a definition either). No ruling is invented
here to fill the gap by the fold itself, per instruction. **This is a finding, not a fold**:
whatever WP-15/WP-8-era context minted the C-007 pinned-lane authority the comment claims, it was
never written down.

**Resolution, 2026-09-06 (orchestrator).** Reconstructed rather than dropped, and recorded above as
`RUL-104`. The three citation sites are self-describing — between them they state the rendered path
(`$OCX_HOME/toolchain/<group>/<entry>`), the parity claim ("follows that tree like the other
three", C-065), the pinned-lane ladder (C-007), the lock-derived group-set bound (C-070) and the
`None` condition (C-067). Every clause of the anchor is quoted or paraphrased from those comments;
nothing is inferred beyond them. This makes the citations resolvable and the completeness check
green, at the cost of an anchor whose authority is the shipped code rather than a contemporaneous
ruling — which is why the provenance is stated in both places rather than silently normalised.

## Completeness check

The requirement is: every id cited in `crates/` or `test/` resolves against an anchor in this file,
except the one explicitly named above.

```sh
# 1. Ids actually cited in shipped source (the requirement list)
/usr/sbin/grep -rhoE 'RUL-[0-9]+' crates/ test/ --exclude-dir=__pycache__ | sort -u > /tmp/cited.txt

# 2. Ids this artifact defines (one per "#### RUL-N" anchor)
/usr/sbin/grep -oE '^#### RUL-[0-9]+' .claude/artifacts/rulings_toolchain_activation.md \
  | /usr/sbin/grep -oE 'RUL-[0-9]+' | sort -u > /tmp/defined.txt

# 3. Set difference: cited but not defined
comm -23 /tmp/cited.txt /tmp/defined.txt
```

**Clean run** — 81 cited ids, 107 anchors defined in this file:

```
RUL-104
```

Exactly the id named in *Cited but never defined* above — nothing else. `wc -l /tmp/cited.txt` →
`81`; `wc -l /tmp/defined.txt` → `107`; `comm -23 ... | wc -l` → `1`.

**Discriminating run** — the check must be able to go red. This file's own `#### RUL-63` anchor was
renamed in place (`RUL-63` → `RUL-SIXTY-THREE-MUTATED`) and the check re-run:

```
RUL-104
RUL-63
```

The check correctly reported the deliberately removed id in addition to the standing gap. The
anchor was restored (`diff` against the pre-mutation backup showed only the one mutated line) and
the check re-run to confirm it returns to the single-line clean state above — the `defined.txt`
output was byte-identical to the pre-mutation run.

## Provenance

The `.claude/state/` source files above are **not deleted** by this fold — they remain the
line-numbered, round-by-round record this document was built from, and cleaning them up is a
separate decision for the owner. Nothing in `crates/` or `test/` was touched; this is a
documentation-only change.
