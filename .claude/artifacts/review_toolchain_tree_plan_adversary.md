# Adversarial review — `plan_toolchain_tree_layout`

**Date:** 2026-09-07 · **Scope:** plan-artifact, one-shot, read-only
**Reviewed:** `.claude/artifacts/plan_toolchain_tree_layout.md`,
`.claude/artifacts/adr_toolchain_activation.md` § *Addendum — 2026-09-07* (C-071…C-084),
`.claude/artifacts/research_toolchain_tree_corrupt_states.md`,
`.claude/artifacts/research_toolchain_tree_doc_surface.md`
**Tree:** `/home/mherwig/dev/ocx-sion` @ `sion`. Every `file:line` below was opened, not inferred.

---

## 1 · actionable — C-072 enumerates two raw joins onto the home root; there are six

C-072 says the two joins at `render_toolchain.rs:1437` and `:1743` are the complete set that
bypasses the grammar, and that closing both is sufficient. Four more exist in production code.
A builder who closes exactly the two named ships a renderer whose prune, report, heal and lock
still address `<root>/<group>` while `entry()` writes to `<root>/links/<group>/<entry>`.

The worst of the four is `repoint_link`. Its group-directory refusal is documented in place as
"never *through* a symlinked group directory: heal runs on every composing emit, so this is the
write side's most-travelled path into an attacker-shaped tree" (`render_toolchain.rs:2126-2129`).
After the move it would `symlink_metadata` `<root>/<group>` — a path nothing writes to any more —
while `replace_atomic` creates `<root>/links/<group>` via its own `create_dir_all` and writes the
link there. **A live security refusal becomes a no-op, and it does so silently: no test names the
guarded path, and the tree still renders correctly.** This holds under option B and option C
alike; it is not a consequence of the `active` symlink.

This finding also falsifies C-075's "every parent `prune_within` derives is a physical directory
under the root — `<root>`, `<root>/links`, `<root>/links/<group>` … so its two-sided
`dunce::canonicalize` containment check extends unchanged". `prune_within:2581` derives that
parent from `home.root().join(group)` and never consults the grammar, so the claim is true only
if `:2581` is also edited — which no contract asks for.

### The complete set — implement from this table

| `file:line` | Addresses today | Must address after C-072 | What depends on it |
|---|---|---|---|
| `render_toolchain.rs:975` | `home.root().join(group)` as `heal_lock_parameters`' `guarded_directory` | `home.links_group(group)?` | **Lock.** The per-entry heal lock is keyed on this directory. Left unmoved, two renders of the same entry take locks named after a directory neither writes — the mutual exclusion between the heal and the render's own write on `links/<group>/<entry>` is lost. |
| `render_toolchain.rs:1437` | `read_dir_utf8_names(&home.root().join(*group))` — `reconcile_links`' per-group entry-orphan scan | `home.links_group(group)?` | **Report + prune.** Enumerated by C-072. Unmoved, the entry-orphan pass reads an empty or absent directory and every departed entry silently stops being pruned. |
| `render_toolchain.rs:1647` | `artifact_path`, `Link` arm: `home.root().join(group).join(entry)` | `home.entry(group, entry)?` (or `links_group(group)?.join(entry)`) | **Report.** This is the path `RenderedItem` carries into the user-facing render report and into `prune_outcome`'s `RenderOutcome::Skipped { path }`. Unmoved, every reported path names a location that does not exist. |
| `render_toolchain.rs:1648` | `artifact_path`, `GroupDirectory` arm: `home.root().join(group)` | Depends on which pass emitted it — see finding 2; `<root>/<name>` for a depth-1 orphan, `<root>/links/<group>` for a group orphan | **Report + prune.** Same two consumers. Cannot be fixed by a single substitution while one enum variant serves both passes. |
| `render_toolchain.rs:1743` | `read_dir(home.root().join(DEFAULT_GROUP))` — `observe_default_group_links`' fingerprint | `home.links_group(DEFAULT_GROUP)?` | **Guard.** This is the fingerprint C-061's per-prompt gate compares. Unmoved, it reads an absent directory, returns an empty map, and the gate compares empty-to-empty — it passes on every tree, including a tampered one. Enumerated by C-072. |
| `render_toolchain.rs:2130` | `repoint_link`: `let group_directory = home.root().join(group);` then `symlink_metadata` refusal at `:2131-2134` and `create_owner_only` at `:2136` | `home.links_group(group)?` | **Guard (security) + write.** The refusal above; and the owner-only create. Unmoved: the refusal inspects the wrong path (no-op), the create leaves a stray depth-1 directory the next render prunes as an orphan, and the actual write directory `<root>/links/<group>` is created by `replace_atomic`'s `create_dir_all` **with the ambient umask**, losing R-W19(a)'s owner-only mode. |
| `render_toolchain.rs:2581` | `prune_within`, `Link` arm: `(home.root().join(group), entry.clone())` as the `(parent, name)` pair | `(home.links_group(group)?, entry.clone())` | **Guard + prune.** The two-sided `dunce::canonicalize` containment check at `:2605-2613` runs on this parent. Unmoved, `canonicalize` fails on a non-existent directory and every link prune becomes `file_error` → `Skipped`, permanently. |

`prune_within:2582`'s `GroupDirectory` arm (`home.root().to_path_buf()`) is the seventh site and
is correct *only* for the depth-1 pass; see finding 2.

Test-only joins (`:3683, :4100, :4285, :4318, :4529, :4555, :4768, :4803-4811, :6553, :6900-6944,
:7386-7434, :7581, :7870, :7901, :7908`, plus `composer.rs:8242` and `activation.rs:3879`) are
fixture literals and are WP-2/WP-3 mechanical follow-on, not grammar bypasses.

## 2 · actionable — `RenderedArtifact` cannot express the split C-074 requires

`GroupDirectory` is emitted by both the depth-1 orphan scan (`render_toolchain.rs:1464`) and the
group-orphan pass, and `prune_within:2582` hardcodes `<root>` as its parent for both. C-074 sends
those two passes to different parents (`<root>` for the closed-set depth-1 comparison,
`<root>/links` for the lock-keyed group pass). One variant can no longer serve both. No contract
adds a fourth variant or carries the parent on the existing one, and no work package owns the enum
change — WP-2's scope line names the scan and the prune but not the type.

## 3 · actionable — two of the three §0 test rows have unreachable reds

Both rows predict "the payload disappears" from a populated directory. Nothing on either path can
produce that: the `Link` prune arm is `symlink::remove` (`:2620`), the heal's repair is
`replace_atomic` (`:2144`), `rename(2)` onto a populated directory fails, and `repoint_link`'s
`Err` is swallowed at `debug` (`:2098-2100`).

**Row 2 — `test_a_dereferenced_toolchain_copy_reports_the_same_entry_on_every_pull` (TR).**
Stated mutation ("delete the `is_expected` retention so the prune considers it") does not red it:
`symlink::remove` on a populated directory fails, the payload survives, `link_entries` still does
not regain the entry, and both runs still report. The row is worth keeping — **replace the mutation
with: make WP-0's directory arm call `remove_dir_all` instead of `remove_dir`.** The link comes
back and the payload disappears, redding assertions (b) and (c) together. That is also the exact
RUL-32 carve-out boundary C-082 draws against C-081, so the mutation tests the contract rather
than an incidental line.

**Row 3 — `heal_leaves_a_real_directory_at_an_entry_untouched` (RT).**
Stated mutation (flip `:2084`'s probe to `true` for non-links) does not red it: `repoint_link`
runs, `replace_atomic` fails, the `Err` is logged at debug, `repaired` stays `0`, contents survive
— `HealOutcome::Healed(0)` and the byte-identity assert both still pass. The row is worth keeping;
the mutation is aimed at the wrong guard. **Replace it with: give `repoint_link` a delete before
`replace_atomic`** (`remove_dir_all`/`remove_file` on a non-link occupant at `:2144`). Contents
vanish and the count becomes `1`, redding both assertions. This is the two-independent-guards case
from `quality-core.md` § *Unchecked Green*: the probe at `:2084` and the absence of a delete in
`repoint_link` both defend the same property, so deleting either alone leaves the row green. The
property the row exists to protect is the *second* one — `:2066-2068` states it: "Removing it
would give heal a delete path C-051 does not authorise."

**Row 1** is fine as a row but its mutation is wrong for the reason in finding 4; the correct
mutation is "delete WP-0's directory arm so the code falls through to `replace_atomic`" — the
reason string becomes the raw `rename` error instead of the remedy-naming one, redding the
"reason names the shape" assertion.

## 4 · actionable — C-082 diagnoses a call that is not on the path

`adr_toolchain_activation.md:1197-1198` reads: "today `publish_link` → `symlink::update` errors on
it (`symlink.rs:89-91`)". `publish_link_within` calls `crate::symlink::replace_atomic`
(`render_toolchain.rs:1565`), and the comment directly above it says so in as many words:
"`replace_atomic`, never `symlink::update`" (`:1561-1564`). `symlink::update` is not reachable
from the render's link publish at all.

**Change `adr_toolchain_activation.md:1197-1198`** — the clause fragment "`publish_link` →
`symlink::update` errors on it (`symlink.rs:89-91`)" — to name `publish_link_within` →
`symlink::replace_atomic` (`render_toolchain.rs:1565`), whose staged-link `rename(2)` onto a
populated directory fails on POSIX (`symlink.rs:227-229`) and whose Windows remove-then-rename arm
fails on the directory likewise. The conclusion is unchanged and the defect is real: every entry
reports `Skipped` on every render, forever. Only the mechanism and the citation are wrong — and
row 1's mutation, written against that non-existent call, must be rewritten with it.

## 5 · actionable — WP-0 does not deliver what S-002 promises

WP-0's own contract is non-recursive `remove_dir` (C-082, RUL-32 in full). A dereferencing copy of
a package root is never empty, so after WP-0 the user-visible outcome for the exact scenario the
work package exists for is still `Skipped` forever — with a remedy-naming reason string instead of
a raw errno. That is a real improvement and worth shipping in wave 0; it is not "a route back".
Plan § *User-experience scenarios* S-002 says "today this is `Skipped` forever with no route back
— that is WP-0", and § *Scope* calls it "the dereferenced-copy **heal**". Both over-claim. Only
the empty-directory case is healed.

## 6 · actionable — wave 3 is not file-disjoint

WP-4 and WP-5 both list `test/src/toolchain_fixtures.py`. WP-5 extends `two_branch_checkout`,
which occupies lines 511-568 of a 568-line file — the last function in it. The plan asserts
"waves 2 and 3 are already at their file-disjoint maximum" (§ *Parallelization*, *Why not wider*).
Either serialize WP-5 before WP-4, or move `two_branch_checkout`'s extension into WP-4 and leave
WP-5 owning only `test/tests/test_toolchain_render.py`.

## 7 · actionable — five test rows have no owner

Of §C's 18 live non-`[active-only]` rows, 8 live in Rust `mod tests`: rows 1, 3, 4, 5, 6, 12, 14,
15 in `render_toolchain.rs` and row 8 in `toolchain_store.rs`. WP-4 is titled "Acceptance suite",
claims "the 17 non-`[active-only]` corrupt-state tests", and lists only Python files. Rows 1 and 3
are plausibly WP-0's (it names `render_toolchain.rs`); row 8 needs no new test. **Rows 4, 5, 6, 12
and 15 are owned by nobody.** WP-2's scope line covers the renderer and its doc comments, not its
test module.

## 8 · actionable — "reported once" has no mechanism

Plan WP-2 scope says "legacy tree `Skipped` once with remedy"; §C row 18 asserts "the second run
does not report `oldgroup` again". C-076 prescribes a `RenderOutcome::Skipped` whose reason names
the remedy — a per-render outcome, with nothing suppressing a repeat. Nothing persists that a
legacy name was reported: the render stamp carries names, bodies and link targets (RUL-67), and
`state_store.rs:634` is its only file. Row 18's own mutation ("leave legacy names in the generic
orphan path — `remove_dir`/`ENOTEMPTY` reports it on every run and reds") describes the *default*
behaviour as the failure state, which means the once-only behaviour needs machinery no contract
specifies and no work package owns. Either C-076 gains a suppression clause and a WP gains its
implementation, or row 18 and WP-2's scope line drop the word "once".

## 9 · actionable — the live-row arithmetic is wrong, and three rows are silently option-B-only

§C holds **18** live non-`[active-only]` rows (1-18) plus **14** `[active-only]` rows (21-34;
19, 20, 32 and 35 struck) = 32 rows in total. The plan's testing strategy reads "**32 live rows**
… 15 further rows are tagged `[active-only]`", which takes the total for the non-active count and
overstates the active-only block by one. Not-CI-verifiable rows are **3** (14, 33, 34), not 4 —
and the artifact's closing *Unverified-by-CI* paragraph still cites struck rows 35 and 20.

Separately: **rows 5, 6 and 7 are about `shells/`**, which exists only under option B, but carry no
`[active-only]` tag — the tagging convention keys on `active`, not on the second block as a whole.
Under the recommended option C they land on WP-4 (wave 3, ships without WP-7) as tests for a
directory that never exists. Tag them, or retag the block by contract (C-078…C-084) rather than by
the symlink's name.

## 10 · actionable — row 32 asserts a deleted variant

Row 32 (`active_is_reserved_case_insensitively_on_both_components`, TS) expects
`ToolchainPathError::Reserved` for `entry("Active", "x")`. C-073 deletes that variant, and the
row's mutation cites row 19, struck. It will not compile. *(Confirmed struck by the team lead
after this review was written; recorded here for the file's completeness.)*

## 11 · actionable — `.claude/rules/` is in no work package

The doc-surface artifact excluded `.claude/rules/**` from its checklist as "37 files, not itemized
— rule prose about the abstract concept, not the tree shape"
(`research_toolchain_tree_doc_surface.md` § *Section 7 — Patterns searched*). That exclusion is
false. Five rule files spell the concrete shape:

- `.claude/rules/subsystem-file-structure.md:218` — "one `<group>/<entry>/` directory link
  (junction on Windows) to a package"
- `.claude/rules/subsystem-file-structure.md:369` — ARCH-4b sibling, C-052: "the `<group>/<entry>`
  links a render writes into a toolchain home"
- `.claude/rules/subsystem-package-manager.md:47` — `tasks/render_toolchain.rs`' row: "one
  `<group>/<entry>/` directory link per selected group"
- `.claude/rules/subsystem-cli.md:321` — "consulting no `<group>/<entry>` link"
- `.claude/rules/subsystem-cli-commands.md:74` — `pull`'s row: "`<group>/<entry>` links for every
  group the render covers"
- `.claude/rules/arch-principles.md:116-117` — the ADR index rows for
  `adr_project_toolchain_links.md` and `adr_toolchain_activation.md`, both of which spell
  `.ocx/toolchain/<group>/<entry>` and the `bin` reservation

The ADR's own migration table names three of them
(`.claude/rules/subsystem-{file-structure,cli,package-manager}.md`, wave 4). The plan's WP-6 lists
website pages, doc scripts, casts and schemas — no rule files. Nothing in `task verify` catches the
drift: `.claude/tests/test_ai_config.py` checks catalog structure and path-scope overlaps, not
prose. `arch-principles.md:116-117` additionally still asserts "`bin` reserved as group + tool
name", which C-073 deletes.

## 12 · actionable — `test/**` was never searched for the doc surface

The doc-surface artifact's § *Section 7* enumerates its search scope: `website/**`, `README.md`,
`CONTRIBUTING.md`, `.claude/rules/**`, `.claude/artifacts/**`, `.github/**`, `taskfiles/**`,
`crates/**/*.rs`, and `.cast` files. **`test/**` appears nowhere in that list.** Consequences:

- `test/bench/shell_latency.py:2866` — "``bin/`` and the `<group>/<entry>` links, never the package
  contents" — is named in the ADR's wave-4 table (`test/bench/shell_latency.py`) and in no work
  package. The file is 226 KB and carries 36 `toolchain` hits.
- The ADR's wave 4 names `test/tests/test_{toolchain_render,toolchain_activate,session_path,
  self_activate,trampoline_exec,shell_reconcile}.py`. WP-4 lists `test_toolchain_cli.py`,
  `test_trampoline_exec.py`, `test_toolchain_offline_after_pull.py`, `test_session_path.py`.
  **`test_toolchain_activate.py` (48 hits), `test_self_activate.py` (4) and
  `test_shell_reconcile.py` (11) are in the ADR and in no work package.**

WP-4's verify column is `full`, so `task verify` would red on the Python side — this is a sizing
and ownership error rather than a silent escape, unlike finding 11.

## 13 · actionable — C-073 mis-states why `TrailingDotOrSpace` survives

C-073 keeps `Empty`, `ControlCharacter`, `Separator`, `PathPrefix`, `TrailingDotOrSpace` and
`Relative` "because each is about path *escape*, not name collision". The code says the opposite
for one of them. `toolchain_store.rs:548-551`: "Windows strips trailing dots and spaces when it
resolves a path, so `bin.` and `bin ` reach the trampoline directory **the reservation below exists
to protect** while sailing past `eq_ignore_ascii_case("bin")`."

Its only documented rationale is the reservation C-073 deletes. The refusal may still be worth
keeping — Windows path normalisation is a real hazard independent of any one name — but the
justification has to be rewritten, and C-073 simultaneously requires `bin.` to keep exiting 78. As
written, WP-1 will delete the reservation and leave a comment citing it.

## 14 · trivia

- `toolchain_store.rs:479-482` — `entry()`'s doc says "The rendered link is written with
  [`symlink::update`]". It is not (`render_toolchain.rs:1565`). Already stale; WP-1 owns the file.
- "`entry()` and `bin()` are the only path-producing accessors" is false — `root()` (`:328`),
  `bin()` (`:337`), `gitignore()` (`:344`) and `entry()` (`:486`) all produce paths, each forwarded
  by `ToolchainStore` (`:586-627`). Harmless: the other three are tree-own names.
- `render_toolchain.rs` is 8,162 lines, not the plan's "~7,900".
- C-084's "exported at `setup.rs:33`" is wrong — `setup.rs:32-35` exports
  `deregister_session_path`, `register_session_path`, `session_path_stores`, not `deregister_in`.
  It is reachable as `setup::session_path::deregister_in` via `pub mod session_path`
  (`setup.rs:26`). The "today called by nothing" half is correct (only
  `deregister_session_path:381` and tests call it).

## Verified true, for the record

- `crates/ocx_lib/src/file_structure/toolchain_store.rs` is **absent** at tag `v0.6.0`
  (`git cat-file -e` → fatal, exists on disk only). The unreleased premise holds.
- The `bin` reservation string exists only in the working tree: `project/error.rs:176` and
  `website/src/docs/reference/configuration.md:1683`. Nothing at `v0.6.0`. Deleting it is a
  loosening, no `!`.
- `<group>/<entry>` really does reach user environments: `composer.rs:1320`
  (`links.home.entry(&tool.group, &tool.name)?`) → the trusted map → `install_path_for:1397-1405`
  returns the **link path** as a package's install path under `PathLane::Following`. The `JAVA_HOME`
  citation is illustrative, not a literal in the code, but the PATH claim is exact.
- Anchors `:1455` (`locked_groups`), `:2299-2308` (`refuse_symlinked_home_leaves`), `:2388`
  (`create_bin_owner_only`), `:2605-2613` (two-sided `dunce::canonicalize`), `:2911-2940` (fault
  hook), `:3367-3411` (`snapshot_subtree`), `toolchain_store.rs:558-563` (the `Reserved` arm) and
  every `test/src/toolchain_fixtures.py` / `test_toolchain_render.py` line cited by the
  corrupt-states artifact are all accurate.

---

## Verdict

**Safe to execute as written: no.** Fix first: re-enumerate the raw joins in C-072 — six, not two
— because the one at `repoint_link:2130` turns a shipped security refusal into a no-op the moment
`entry()` moves, under option B and option C alike, with no test that would notice.
