# Corrupt / divergent on-disk states of the rendered toolchain tree

Research record. Read-only analysis of `/home/mherwig/dev/ocx-sion` @ `sion`.
Every `file:line` is traced, not guessed. Rows marked **[measured]** were executed
on this host; everything else is read from source. Rows tagged **`[active-only]`**
exist *solely* because of the proposed `active ->` symlink and can be struck in one
pass if that option is dropped.

Proposed layout under discussion:

```text
<home>/
├── .gitignore                   "*" (C-004), ensure-present
├── active -> shells/default/    relative symlink (POSIX) / absolute junction (Windows)
├── links/<group>/<entry>/       directory link to a package root
└── shells/<name>/bin/           launcher trampolines
```

replacing today's `<home>/{.gitignore, bin/, <group>/<entry>/}`.

---

## Ranked — most likely to occur in real use

1. **A2 — `active` absent** after `rsync -r` (no `-l`/`-a`), a zip round-trip, or a
   Windows extractor. **[measured]**: `rsync -r` prints `skipping non-regular file
   "active"` and copies `shells/` intact. **`[active-only]`**
2. **A1 / §0 — a dereferencing copy turns a link into a real directory.**
   **[measured]** on `cp -rL`, `tar -h` and `zip`. This is the shipped defect in §0
   for `<group>/<entry>` today, and the same shape hits `active` tomorrow.
3. **A15 — a cross-project or cross-machine copy carrying trampolines that bake the
   *source* project's absolute root.** Needs no exotic tool: `cp -a`, `tar`, Docker
   `COPY` and `git archive` all preserve the tree faithfully — faithfully wrong.
4. **A20 — the legacy `bin/` and `<group>/` tree beside the new names**, on literally
   every upgrade across the rename.
5. **A3 — a dangling link after a branch switch or a partial restore.** The shipped
   `two_branch_checkout` fixture already exercises this shape for `<group>/<entry>`.

### The one I would refuse to ship without

**A4 — containment of `active`'s resolved target.** `<home>/active/bin` is the
directory that goes on `PATH`, and for the global tier it is written *as a literal
string* into the OS session environment (`setup.rs:442`, `setup/session_path.rs:7-9`)
where nothing ever re-validates it. `lstat` follows intermediate components, so
inserting `active` between the home root and `bin` defeats **every** symlink refusal
this subsystem currently has — `refuse_symlinked_home_leaves`
(`render_toolchain.rs:2299-2308`), `create_bin_owner_only` (`:2388-2428`) and
`bin_matches_recorded` (`activation.rs:1279-1281`) all stat a path whose `active`
component is followed and report an ordinary directory. Without a containment check
on the raw `read_link` value, run before any write, the rename converts three
independent guards into zero. If `active` does not ship, this risk goes with it.

---

## §0 — Shipped defect: a real directory at `<group>/<entry>` is `Skipped` forever

**Independent of the layout change. Reproducible on `sion` today. Own issue.**

### Trace

1. A dereferencing copy replaces the `<group>/<entry>` symlink with a real directory
   holding a copy of the package root. **[measured]** on `cp -rL`, `tar -h` and
   `zip`/`unzip`; `cp -r`, `cp -a`, `tar` (default) and `rsync -a` all preserve the
   link correctly.
2. `reconcile_links` (`render_toolchain.rs:1414-1434`) still names the entry — it is
   in the lock — so `is_expected` (`:1493-1507`) retains it and the prune pass never
   considers it. There is no path by which the renderer removes it.
3. `publish_link_within` (`:1540`):
   - `:1541` `read_link(entry)` → **EINVAL** on a real directory **[measured]**, so
     the `Unchanged` fast path is not taken.
   - `:1544-1551` the parent (`<group>/`) is a real directory, so the symlink refusal
     does not fire.
   - `:1559` `create_owner_only(parent)` succeeds — the parent already exists.
   - `:1564` `replace_atomic(target, entry)` stages a temp symlink and `rename`s it
     onto `entry`. `rename(symlink → existing directory)` is **EISDIR even when the
     directory is empty** **[measured]** (`symlink.rs:174-181` returns it as
     `Error::InternalFile`).
   - `:1568-1571` → `RenderOutcome::Skipped { path, reason }`.
4. The heal path declines too, deliberately: `heal_links`'s probe (`:2081-2084`)
   returns `false` for anything that is neither a link nor absent, and `:2098` logs
   one `debug!` line. The comment at `:2065-2067` states this as intent — "a shape
   heal cannot repair … is left exactly as it is", to avoid giving heal a delete path
   C-051 does not authorise.
5. Net effect: **every** subsequent `ocx pull` reports the same `Skipped` item, the
   tool's link never becomes a link again, and the composing side degrades that entry
   to a digest path (C-067) for the life of the tree. Nothing tells the user which
   copy did it or what to delete.

### States that reach it

| Producer | Result | Evidence |
|---|---|---|
| `cp -rL <home> <dest>` | `<group>/<entry>` becomes a real dir; a *dangling* sibling link aborts the copy (`cannot stat`) leaving a partial tree | **[measured]** |
| `tar -h` / `tar --dereference` | same; dangling entries dropped with a warning | **[measured]** |
| `zip -r` without `-y`, then `unzip` | same | **[measured]** |
| Docker `COPY` of an already-dereferenced context; Windows Explorer drag-copy of a junction; most Windows zip extractors | same shape | inferred — not reproducible on this host |
| `cp -r`, `cp -a`, `tar` (default), `rsync -a`, `git archive` | link preserved; **not** affected | **[measured]** except `git archive` (mode 120000 is spec) |

### What it should do

Report it **once**, not every render: the entry is unrepairable by design, so the
renderer should say so with a `WARN` naming the path and the fact that ocx did not
create it, and the report should distinguish "skipped, retry may help" from "skipped,
this needs a human". Removing it is not on the table — RUL-32's refusal of a recursive
delete inside an attacker-writable tree still holds, and the directory contains a copy
of a package the user may care about.

---

## §A — The corrupt / divergent state space

`<home>` = `<project>/.ocx/toolchain` or `$OCX_HOME/toolchain`.
"Orphan scan" = `reconcile_links`'s home-root pass, `render_toolchain.rs:1455-1468`;
its allowlist is literally `[reserved_name(home.bin()), reserved_name(home.gitignore())]`
plus the locked group names. "Prune" = `prune_within`, `:2578-2646`.

### A.1 — `active`, the PATH-facing indirection (all `[active-only]`)

| # | State | How it arises | Current code would… | Should | Symptom if unhandled |
|---|---|---|---|---|---|
| A1 | `[active-only]` real populated directory at `active` | `cp -rL`, `tar -h`, `zip` **[measured]**; Explorer drag-copy of a junction | Orphan scan `:1457` — `active` ∉ allowlist → `GroupDirectory("active")` → prune `:2635-2643` stats it, `is_dir` → `remove_dir` → `ENOTEMPTY` → `Skipped` forever. Writing it: `read_link` EINVAL (`symlink.rs:91`), `replace_atomic` EISDIR (`symlink.rs:174`) **[measured]** | Refuse; one WARN + a "delete the tree" hint. Never `remove_dir_all` | Stale trampoline copies permanently on `PATH`; `Skipped` noise on every render; no diagnosis |
| A2 | `[active-only]` `active` absent, `shells/` intact | `rsync -r` **[measured]**; Windows extractors that drop links | Nothing creates it. `PATH` carries a nonexistent dir (benign). Gate: `read_dir` fails → `bin_matches_recorded` false (`activation.rs:1282-1284`) → withhold + `ocx pull` hint | **Silent** create — the commonest benign state | Tools silently absent from `PATH`, only a generic hint |
| A3 | `[active-only]` dangling `active` | Branch switch removing a shell; partial restore | `exists()` false, `is_link()` true (`symlink.rs:89`) **[measured]** → both write primitives heal it. But the orphan scan prunes it first (`:2638-2639`) | **Silent** repoint | Prune/re-create churn breaks C-047 byte-identity |
| A4 | `[active-only]` `active` resolves **outside `<home>`** | Windows junction copied cross-user/cross-machine (junctions are absolute-only → points into the *original* profile); hostile clone force-adds it; hand edit | **No containment check exists.** `symlink_metadata(<home>/active/bin)` follows `active` → not refused (`:2300-2306`, `activation.rs:1279`); `create_bin_owner_only` `mkdir`s inside the target (`:2388-2428`); the global tier persisted the path string once at `ocx self setup` (`setup.rs:442`) with **no per-prompt gate at all** | **Refuse**, exit 78 (§B.2) | Arbitrary-directory-on-`PATH` primitive. Global tier: survives reboots, no ocx process involved |
| A5 | `[active-only]` `active` is a regular file / FIFO / device | Hostile clone; truncating restore | `read_link` EINVAL **[measured]** → `symlink::update` hard-errors; `replace_atomic`'s `rename` **succeeds over a regular file** and destroys it | Replace only after proving link-or-absent | Silent destruction of a file at a repository-chosen path |
| A6 | `[active-only]` `active` → itself / a cycle | Hand edit; a restore that rewrote targets | `is_link` true, `exists()` false, `stat` → **ELOOP** **[measured]**; behaves as A3 | Heal silently, as A3 | None if A3 handled; unbounded resolution in any consumer that canonicalizes |
| A7 | `[active-only]` `active` → a *valid but wrong* `shells/<other>` | An interrupted shell switch; a colleague's copy | Unobserved. `link_fingerprint` is `<group>/<entry>`, default group only (`state_store.rs:339-347`); `bin_fingerprint` is per-file content (`:335-337`). Neither names `active` | Stamp the raw target; repoint on disagreement | The gate passes over a tree pointing at the wrong shell |
| A8 | `[active-only]` `Active` / `ACTIVE` beside `active` | macOS APFS default, Windows; a case-sensitive volume copied to either | `is_expected` folds only when the runtime probe says so (`:1493-1507`, probe `:2484-2512`). On a case-**sensitive** host `Active` is an orphan → `symlink::remove` (`:2638`) | Reserve `active`/`links`/`shells` case-insensitively on **both** components, as `validate_component` already does for `bin`/`.gitignore` (`toolchain_store.rs:556-561`) | A group named `Active` shadows the `PATH` link on macOS/Windows |

### A.2 — `links/`, `shells/`, `.gitignore` (layout-independent)

| # | State | How it arises | Current code would… | Should | Symptom |
|---|---|---|---|---|---|
| A9 | `links/` or `shells/` is a **regular file** | Hostile clone; a restore writing a file where a dir belongs | `create_dir_all` → `NotADirectory`; the orphan scan's prune arm `:2641-2643` calls `remove_file` and **deletes it silently** | Refuse tree-own names from the prune path entirely — they are never orphans | A user file at `<home>/links` deleted with no report |
| A10 | `links/` or `shells/` is a **symlink** (incl. dangling) | Same as A9; `cp -r` of a hand-linked tree | Orphan scan → `symlink::remove` (`:2638-2639`), removed outright. `publish_link_within` refuses a symlinked **parent** (`:1544-1551`) but that is `links/<group>/`, one level *deeper* than `links/` | Refuse with the same predicate as the home root (§B.1) | Writes land outside the home; the existing refusal now sits at the wrong depth |
| A11 | `shells/default/` missing while the rest is intact | Interrupted render; selective restore; an `rsync` filter | No render creates a shell directory unless the default group is in scope (`render_with:1044-1049`) | Ensure-present, in the order of §B.4 | A `PATH` entry that never resolves |
| A12 | `shells/<other>/` written by a **newer** ocx | Shared `$OCX_HOME`; a downgrade | If `shells/` is scanned like a group directory: unknown shell → prune → `remove_dir` → `ENOTEMPTY` → `Skipped` forever | Leave unknown `shells/<name>/` alone; prune only what this ocx renders | A downgrade destroys the newer ocx's tree, or reports `Skipped` forever |
| A13 | Empty `shells/` | Interrupted render; `git clean` on a tree whose `.gitignore` was removed | Nothing repopulates it under a `-g`-narrowed render | Ensure-present, like `.gitignore` (`toolchain_store.rs:411-455`) | A narrowed render leaves an unusable home |
| A14 | `.gitignore` a symlink to `~/.bashrc`, a FIFO, or oversized | Hostile clone (the documented threat, `toolchain_store.rs:365-405`) | **Already handled** — `symlink_metadata` + `read_bounded` + `write_bytes_atomic` (`:411-455`); covered by `ensure_gitignore_*` tests in `toolchain_store.rs` | Unchanged | — |

### A.3 — Cross-project and cross-machine copies (layout-independent)

| # | State | How it arises | Current code would… | Should | Symptom |
|---|---|---|---|---|---|
| A15 | Trampolines bake the **source** project's absolute root | `cp -a`/`tar`/`git archive`/Docker `COPY` of a project to a new path; a colleague's tarball | Bodies bake the absolute root (`trampoline_target:2676`, C-028) and are byte-different per directory — `test_toolchain_render.py:313-346` is the shipped positive control. At the *new* path the stamp key differs, so `bin_mode_entry` (`activation.rs:1409-1436`) finds no stamp → withhold → re-render. The stale bodies are **not** pruned by the prompt path (C-064) and stay on disk until the next `ocx pull` | Correct today for the project tier; the residual is that the copied bodies exist and resolve to the *source* project until a render runs | A trampoline that runs `ocx --project '<other-machine-path>'`; on a shared host that path may be another user's |
| A16 | Copy restored to the **same** absolute path, `$OCX_HOME` copied with it | Container image layer; a backup restore; a CI cache | Stamp key matches, bodies are byte-identical, and an inode change alone does **not** fail the gate — `activation.rs:1330-1341` falls through to the content hash, which matches → **gate passes** | Correct for the trampolines; the gap is that a link/`active` dereferenced by the same copy is not covered (A1, §0) | A tree that passes every check while its links are real directories |
| A17 | Windows: project copied between users | Junctions are absolute-only, so `links/<group>/<entry>` points into the original profile | `read_link` yields the foreign absolute path ≠ the lock's target → `publish_link_within:1541` falls through → `replace_atomic` repoints it. Correct | Unchanged | — (this one already works) |

### A.4 — Permissions, crashes, concurrency, legacy

| # | State | How it arises | Current code would… | Should | Symptom |
|---|---|---|---|---|---|
| A18 | Read-only home; no write on the parent; root-owned entry; Windows ACL denial | Restrictive checked-in modes; a `sudo` run; corporate policy; CI cache restore | `ensure_home_root` fails → loud WARN → `skipped_render()` (`:812-822`, `:874`); `bin/` failures degrade per entry (`:1165-1170`). Windows: `junction::create` retries only the transient class (`symlink.rs:400-418`) | Same shape for every new tree-own name — a C-050 skip, never a hard error | A render that fails outright on a policy-denied machine |
| A19 | Crash between two writes | `SIGKILL`, container OOM | The stamp is written **last** (`:1068-1097`, C-048), so an interrupted render leaves the previous stamp → gate mismatches → withhold → next render repairs | Keep every new name inside the same "stamp last" rule | A name written *after* the stamp leaves a stamp vouching for a tree it never saw |
| A20 | Legacy `<home>/bin/` and `<home>/<group>/` beside the new names | Every upgrade across the rename | `bin` is still in the orphan allowlist (`:1456`) so it survives forever; a legacy `<group>/` is pruned via `remove_dir` → `ENOTEMPTY` → `Skipped` | Decide **once**, explicitly: these are ocx's own names, so deleting them does not violate "never delete what we did not write". Silence either way | A stale `bin/` stays on `PATH` ahead of the new directory in an unrefreshed session |
| A21 | Two concurrent `ocx pull` on one home | Two terminals; an IDE plus a shell | One `lock_scoped` over the whole body from step 4 (`:841-852`), after `ensure_home_root`. Known check-then-open race, [ocx-sh/ocx#415](https://github.com/ocx-sh/ocx/issues/415) | Every new name's validate-then-write must sit **inside** that lock | Two renders racing → a stamp describing a tree the other rewrote |

---

## §B — The defensive contract

**B.1 — One validity predicate, one entry point.** A single
`validate_home(home) -> Vec<Violation>` answering for the tree-own names only
(`{.gitignore, links, shells}` + `{active}` if it ships). Everything else at the home
root stays the orphan scan's business. Called from exactly two places: the renderer at
step 2, beside `refuse_symlinked_home` (`:801-828`), and the per-prompt read path,
beside `refuse_symlinked_home` in `owned_project_home`. Spreading validity across the
renderer is how `bin/` ended up defended by four cooperating guards
(`ensure_home_root`, `create_bin_owner_only`, `publish_link_within`,
`bin_matches_recorded`) that one component of layout change defeats all at once.

| Name | Valid iff | Violation → action |
|---|---|---|
| `links`, `shells` | absent, or a real directory | symlink / file → **refuse + one WARN** |
| `.gitignore` | already contracted (`toolchain_store.rs:411-455`) | unchanged |
| `[active-only]` `active` | is a link **and** its raw target, joined lexically onto `<home>` and normalised, stays under `<home>` **and** names `shells/<name>` | absent / dangling / wrong shell / cycle → **silent heal**. Real dir / file / FIFO → **refuse + one WARN + hint**. Escapes `<home>` → **refuse, exit 78** |

Against the no-warn-on-benign rule: the refusals above cannot be produced by any
sequence of ocx commands. Each requires a dereferencing copy, a hand edit or a hostile
checkout — so the diagnostic is the only signal the user gets, and silence leaves a
permanently broken tree that renders green. The *heals* (absent, dangling, wrong
target) are the common benign states and stay silent.

**B.2 — Containment (the security clause).** `[active-only]`. Judge the target
**before any write and before any `stat` of `<home>/active/bin`**, on the raw
`read_link` value, **lexically** — never via `canonicalize`, which resolves the link
and makes it contained in itself (the argument `prune_within:2589-2593` already makes
for the home root). Reuse `utility::fs::path::join_under_root` rather than minting a
predicate. Refusal: a `PackageErrorKind` classifying to `ExitCode::ConfigError = 78`,
matching D-V14's grammar refusals — same class, untrusted file names a path component.
Absolute targets rejected unconditionally on POSIX, as `symlink::validate_target`
already does (`symlink.rs:40-42`); Windows junctions are absolute by construction, so
there the test is `starts_with(canonical <home>)` on the stored target.

**B.3 — The stamp must carry `active`'s raw target.** `[active-only]`. One added
field. Without it: (a) the **global tier has no gate at all** — `setup.rs:442` writes
the literal string once into the OS session environment and nothing re-reads it; (b)
on the project tier the render *writes through* `active` (`create_bin_owner_only`)
before `bin_stamp_matches` ever runs; (c) an inode change alone does not fail the gate
(`activation.rs:1330-1341` falls through to the content hash), so a byte-identical
dereferenced copy passes while `active` is a real directory. `link_fingerprint` cannot
be reused — it is default-group-scoped by contract (`state_store.rs:339-347`) and
widening it re-opens the per-prompt-mismatch problem that scoping fixed.

**B.4 — Ordering and crash safety.** `shells/<name>/bin/` exists → its trampolines are
published → **then** `active` is created/repointed via `symlink::replace_atomic`
(atomic on POSIX; the Windows arm's bounded non-atomic window is documented at
`symlink.rs:203-224`) → **then** the stamp (C-048, `:1068`). Every step inside the
render lock (`:841`). No step deletes anything it did not write; `remove_dir_all`
appears nowhere (RUL-32).

---

## §C — Test matrix

`RT` = `crates/ocx_lib/src/package_manager/tasks/render_toolchain.rs` `mod tests`.
`TS` = `crates/ocx_lib/src/file_structure/toolchain_store.rs` `mod tests`.
`TR` = `test/tests/test_toolchain_render.py`.

Reuse, do not reinvent: `test/src/toolchain_fixtures.py` — `snapshot_tree:238-262`
(records `os.readlink`, so it *does* observe a repoint), `bin_entries:265`,
`link_entries:272`, `two_branch_checkout:511-568`, `read_render_stamp:181-211`;
`test/src/assertions.py:7` (`assert_symlink_exists`, junction-aware); and the existing
orphan/staleness cases at `TR:406`, `TR:937`, `TR:989`, `TR:1073-1118`.
`TR:313-346` is the shipped positive control for "a body bakes the project root" —
cite it, do not duplicate it.

### C.1 — The shipped defect (§0)

| # | State | Test name | Where | Setup | Assertion | Mutation that reds it | CI? |
|---|---|---|---|---|---|---|---|
| 1 | §0 | `a_real_directory_at_a_locked_entry_is_reported_not_retried` | RT | render a home; replace one `<group>/<entry>` link with a real directory holding a file | outcome is `Skipped` **and** the reason names the shape, not a bare EISDIR string | make `publish_link_within` retry or fall through to `symlink::update` — the reason string changes to EINVAL and the assert reds | yes |
| 2 | §0 | `test_a_dereferenced_toolchain_copy_reports_the_same_entry_on_every_pull` | TR | `locked_project`, then `shutil.copytree(home, alt, symlinks=False)`; run `ocx pull` twice against `alt` | both runs report the entry; `link_entries(alt)` never regains it; the payload file survives | delete the `is_expected` retention so the prune considers it — the payload disappears and the second assert reds | yes |
| 3 | §0 | `heal_leaves_a_real_directory_at_an_entry_untouched` | RT | plant the same shape, call `heal_links` | `HealOutcome::Healed(0)`; the directory and its contents are byte-identical after | change `:2084`'s probe to `true` for non-links — `replace_atomic` runs and the surviving-contents assert reds | yes |

### C.2 — `links/`, `shells/`, `.gitignore`

| # | State | Test name | Where | Setup | Assertion | Mutation that reds it | CI? |
|---|---|---|---|---|---|---|---|
| 4 | A9 | `a_regular_file_at_a_tree_own_name_is_refused_never_deleted` | RT | write a regular file at `<home>/links`, render | the file's bytes survive; a refusal is reported | drop `links` from `validate_home`'s reserved set — the orphan scan's `remove_file` arm (`:2641-2643`) deletes it and the survival assert reds | yes |
| 5 | A10 | `a_symlinked_tree_own_directory_is_refused_before_any_write` | RT | `symlink::create(other_dir, home/"shells")`, render | refusal; `other_dir` still empty afterwards | omit `shells` from `validate_home` — trampolines land in `other_dir` and the emptiness assert reds | yes |
| 6 | A12 | `an_unknown_shell_directory_is_left_alone` | RT | plant `shells/fish/bin/x`, render the default group | `shells/fish/bin/x` still present; no reported item names `fish` | make the shells scan prune unknown names — the presence assert reds | yes |
| 7 | A13 | `test_an_emptied_shells_directory_is_repopulated_by_the_next_pull` | TR | `locked_project`; `shutil.rmtree(home/"shells")`; `ocx pull` | `bin_entries` (repointed at the new shell path) equals the pre-delete set | delete the ensure-present step — the set comes back empty and reds | yes |
| 8 | A14 | (no new test) | TS | — | existing `ensure_gitignore_*` cases cover symlink / oversize / FIFO | — | yes |

### C.3 — Cross-project and cross-machine copies

| # | State | Test name | Where | Setup | Assertion | Mutation that reds it | CI? |
|---|---|---|---|---|---|---|---|
| 9 | A15 | `test_a_project_copied_to_a_new_path_does_not_expose_the_sources_trampolines` | TR | `locked_project`; `copytree(project, moved, symlinks=True)` **keeping** `.ocx`; run `ocx shell state` in `moved` **without** pulling | the project `PATH` entry is withheld — the stamp key differs, so the gate finds no stamp | make `bin_mode_entry` key the stamp on the home instead of the project dir (`activation.rs:1424-1428`) — the copied tree passes and the withhold assert reds | yes |
| 10 | A15 | `test_a_copied_tree_still_bakes_the_source_root_until_a_render_runs` | TR | as row 9 | `(moved_home/"bin"/binary).read_bytes()` still contains `str(project.directory)` | make the prompt path prune or rewrite stale bodies (violating C-064) — the byte assert reds | yes |
| 11 | A16 | `test_a_home_restored_at_the_same_absolute_path_passes_the_gate` | TR | `locked_project`; copy `home` and `$OCX_HOME/state` aside; delete and restore both (new inodes, same paths, same bytes); `ocx shell state` | the entry is exposed — inode churn alone must not withhold | make `bin_matches_recorded` require `file_id` equality instead of falling through to the hash (`activation.rs:1330-1341`) — reds | yes |
| 12 | A17 | `a_link_naming_a_foreign_absolute_target_is_repointed` | RT | plant `<group>/<entry>` as a link to an unrelated absolute directory; render | link now names the lock's digest root | make `publish_link_within` compare canonically instead of by `read_link` (`:1541`) — a foreign link that resolves to the same inode is left alone and reds | yes |

### C.4 — Permissions, crashes, concurrency, legacy, case folding

| # | State | Test name | Where | Setup | Assertion | Mutation that reds it | CI? |
|---|---|---|---|---|---|---|---|
| 13 | A18 | `test_a_read_only_home_skips_rather_than_fails` | TR | `locked_project`; `chmod 0o500` the home; `ocx pull` | rc == 0, a WARN on stderr, no traceback, stdout clean | make any new tree-own-name refusal a hard error instead of a C-050 skip — rc becomes non-zero and reds | yes (POSIX modes) |
| 14 | A18 | Windows ACL denial on the junction write | RT | deny write on `<home>` via ACL; render | skip + WARN, rc 0 | make `create_link`'s hard-ACL arm propagate instead of skipping | **no — Windows** |
| 15 | A19 | `a_crash_between_a_write_and_the_stamp_leaves_the_previous_stamp` | RT | existing `OCX_TEST_FAULT` hook, `RenderStage::AfterFirstEntryWrite` (`:2911-2940`) | the on-disk stamp is byte-identical to the pre-render one | move any new tree-own-name write *after* `set_render_stamp` — the stamp changes and reds | yes |
| 16 | A21 | `test_two_concurrent_pulls_leave_one_consistent_tree` | TR | two `ocx pull` subprocesses on one project, joined | every `bin_entries` name is in `read_render_stamp(...)["names"]` and vice versa (convergence, never an interleaving) | move a tree-own-name write outside the render lock (`:841`) — the sets diverge and reds | yes (assert convergence only; interleaving asserts are flaky under `-n auto`) |
| 17 | A20 | `test_a_legacy_bin_directory_beside_the_new_names_is_handled_deliberately` | TR | `locked_project`; plant `<home>/bin/oldtool`; `ocx pull` | whichever §A20 policy is chosen, asserted explicitly and **silently** (no WARN) | flip the policy — the assert reds either way, which is the point of writing it down | yes |
| 18 | A20 | `test_a_legacy_group_directory_at_the_home_root_is_not_reported_forever` | TR | plant `<home>/oldgroup/oldentry` (populated); `ocx pull` twice | the second run does not report `oldgroup` again | leave legacy names in the generic orphan path — `remove_dir`/`ENOTEMPTY` reports it on every run and reds | yes |
| ~~19-20~~ | ~~A8~~ | **STRUCK by team-lead — tests an impossible state.** These rows required `Links`/`Shells`/`Active` to become reserved group/entry names. They cannot collide: a group name is a component of `links/<group>/<entry>`, one level below every tree-own name, so `entry("Links","x")` renders `links/Links/x`. C-073 additionally *deletes* `ToolchainPathError::Reserved`, so the asserted variant will not exist. Re-reserving would reintroduce by hand the coupling C-071 removes by construction. Same claim was made independently by the doc-surface worker and refuted there too. | | | | |

### C.5 — `[active-only]`

Strike this whole block if `active` does not ship.

| # | State | Test name | Where | Setup | Assertion | Mutation that reds it | CI? |
|---|---|---|---|---|---|---|---|
| 21 | A4 | `active_pointing_outside_the_home_is_refused` | RT | build a home; `symlink::create(outside_dir, home/"active")`; render | refusal reported; `<home>/active/bin` never created; `outside_dir` still empty | delete the containment check from `validate_home` — trampolines land in `outside_dir` and the emptiness assert reds | yes |
| 22 | A4 | `active_escape_is_judged_on_the_raw_target_not_the_canonical_one` | RT | `active -> ../../escape` with `escape/` existing under a canonicalized ancestor | refused | swap the raw `read_link` for `dunce::canonicalize` in the predicate — the link resolves as contained in itself and reds | yes |
| 23 | A4 | `test_an_escaping_active_link_is_refused_and_nothing_is_written_through_it` | TR | force-commit `.ocx/toolchain/active -> <tmp>/evil` in a locked project; `ocx pull` | rc == 78; `evil/` empty; stderr names `active` | as row 21 | yes |
| 24 | A2 | `test_an_absent_active_is_recreated_silently` | TR | `ocx pull`; `os.unlink(home/"active")`; `ocx pull` | `assert_symlink_exists(home/"active")`; `os.readlink` == `shells/default`; **stderr carries no WARN** | delete the heal (reds the first half) / make the heal warn (reds the silence half) — both directions covered | yes |
| 25 | A3 | `test_a_dangling_active_is_repointed_not_pruned` | TR | `ocx pull`; repoint `active` at `shells/ghost`; `ocx pull` | link present and correct; `snapshot_tree(home)["active"]` is `("symlink", "shells/default")`, not an absence | leave `active` out of the orphan allowlist — the scan removes it (`:2638-2639`) and the presence assert reds | yes |
| 26 | A1 | `a_real_directory_at_active_is_refused_and_never_removed` | RT | plant `active/` holding one foreign file; render | `active/payload` survives byte-identical; a refusal is reported | swap the refusal for `remove_dir_all` — the payload disappears and reds | yes |
| 27 | A5 | `active_occupied_by_a_regular_file_is_never_renamed_over` | RT | write a regular file at `active`; render | the file's bytes survive; refusal reported | drop the is-link-or-absent precondition and call `replace_atomic` directly — `rename` succeeds over the file and reds | yes |
| 28 | A6 | `a_self_referential_active_is_repointed_without_hanging` | RT | `symlink::create("active", home/"active")`; render | repointed at `shells/default`; no `ELOOP` propagated; the call returns | make the heal `canonicalize` first — `ELOOP` surfaces and reds | yes |
| 29 | A7 | `test_the_render_stamp_records_the_active_target` | TR | `locked_project` | `read_render_stamp(ocx, home)["active"] == "shells/default"` | derive the field from config instead of `read_link` — reds as soon as the tree and config disagree | yes |
| 30 | A7 | `test_a_repointed_active_fails_the_prompt_gate` | TR | pull; create a second shell; repoint `active` at it; `ocx shell state` | the project entry is withheld | omit `active` from the gate comparison — the entry is exposed and reds | yes |
| 31 | A11 | `the_active_link_is_created_after_its_shell_directory` | RT | `OCX_TEST_FAULT` aborting between the two writes | on abort, `active` is absent — never a link to a missing shell | swap the two writes — `active` exists dangling and reds | yes |
| ~~32~~ | ~~A8~~ | **STRUCK by team-lead** — same impossible state as rows 19-20/35: `entry()` renders `links/<group>/<entry>`, so no user-supplied name reaches depth 1 under either option. It also asserts `ToolchainPathError::Reserved`, the variant C-073 deletes, and its stated red was "as row 19" — a struck row. Found by the spec reviewer after the first strike pass missed it. | | | | |
| 33 | A4 | Windows junction cross-user absolute target | RT | plant an absolute junction into a foreign profile path | refused by the same predicate | delete the Windows arm's `starts_with` — reds | **no — Windows** (the *predicate* is pure and is covered by row 21 on Linux) |
| 34 | A1 | Explorer / junction drag-copy dereference | TR | copy a rendered home via Explorer | `active` is a real directory and is refused | as row 26 | **no — Windows** |
| ~~35~~ | ~~A8~~ | **STRUCK by team-lead** — same reason as rows 19-20: `Active` beside `active` is not a tree-own-name collision, because no user-supplied name ever sits at depth 1. A case-fold hazard remains only *within* `links/` between two user group names, which is A8's real residue and predates this change. | | | | |

**Unverified-by-CI.** The pytest acceptance job is Linux-only, so rows 14, 33, 34 and
35 — plus the fold-collision half of row 20 — are design-reviewed, not covered. The
mitigation available today is the pattern `setup/session_path.rs:32-38` already
establishes: *"every refusal predicate and every renderer is pure and host
independent, so the rule each one encodes has a reachable red state on any CI leg."*
Follow that for every new guard and only the syscall behaviour stays uncovered.
