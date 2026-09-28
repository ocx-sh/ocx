---
# Read on demand through the index. This glob matches no file, so a client never auto-loads the file.
paths:
  - "**/.code-docs-depth-on-demand"
title: Length Caps and the Ratchet
summary: The LEN family, owning how long one comment block may run, how the census counts it, and the per-file baseline that stops the count rising
---

# Length Caps and the Ratchet

Owns how long one comment block may run, how the census counts it, and the
per-file ratchet that holds the line. What a guard must say is GRD-01, where cut
prose goes is the index's routing table, and what interface text may contain is
SRF-01. Every command names `checks/`, this rule's support directory wherever it
is installed, and runs from the repository root. Commands match the census as
shipped 2026-09-27.

Contents: [The Caps](#the-caps) · [The Gate](#the-gate) ·
[Adopting the Census](#adopting-the-census) ·
[Rules the Census Enforces](#rules-the-census-enforces) ·
[Rules Review Enforces](#rules-review-enforces) · [Holding the Line](#holding-the-line) ·
[Human Reference Numbers](#human-reference-numbers) ·
[What Agents Get Wrong Here](#what-agents-get-wrong-here)

## The Caps

A **block** is the consecutive comment lines of one kind (doc, plain or
interface) with only blank lines between them. Blank lines neither count nor end
a block; code, a line of another kind, or a test/prod boundary ends it.

| Block | Cap, in counted lines | Rule |
|---|---:|---|
| Plain comment (`//`, `#`), any package | 5 | LEN-01 |
| Doc comment in an app-kind package, or on a non-public item anywhere | 10 | LEN-02 |
| Doc comment on a public item in a library-kind package | 15 | LEN-02 |
| Interface doc: text that renders into `--help` or a schema | its package's public tier: 10 app, 15 library | LEN-08 |
| Structured sections inside a doc block (`Args:`, `Example:`, `# Errors`, `@param`, fenced code, a reST `::` block, a `>>>` doctest) | not counted | LEN-07 |

- No guard, `SAFETY:` label or other marker exempts a block. A guard that runs
  over is split (LEN-05).
- **Syntax decides the kind.** A `//` file header in TypeScript or JavaScript is
  a plain block. Module docs go in the language's doc form (`/** @module */`,
  `//!`, a module docstring). In `.vue`, `.svelte` and `.astro` files the census
  counts script and style comments; it does not see HTML comments.
- **Declare every package a registry publishes as library-kind** before the
  first baseline: a `pyproject.toml` with a build backend, a `package.json`
  without `"private": true`, a `Cargo.toml` without `publish = false`.
  Undeclared packages get app caps. `--report` lists undeclared candidates.
- **The caps bind whatever the surrounding file's density is.** Follow the
  file's idiom and naming, never its comment length. Where a harness prompt says
  to match the surrounding comment density, the caps win on length (LEN-04).
- **Pinned defaults the adopter may override:** the three cap numbers, and
  `library_public` 20 for an adopter who declines LEN-07. Pick them before the
  first baseline, because a later change re-counts every file.
- **Every comment line counts**, delimiter-only lines included: JSDoc's
  `/**` and `*/`, a bare ` *` or `///` line, and a docstring's closing `"""`.
  Blank source lines between comment lines do not. The human reference numbers
  in this file were measured the same way.

## The Gate

```bash
python3 checks/comment_census.py --root . --check .code-docs-length.json    # CI, every change (LEN-03)
python3 checks/comment_census.py --root . --over-cap                        # every over-cap block by line (LEN-01, LEN-02, LEN-08)
python3 checks/comment_census.py --root . --over-cap | awk -F: 'FILENAME == ARGV[1] {want[$0]; next} $1 in want' <(git diff --name-only "$(git merge-base origin/main HEAD)" -- .) -   # only files this change touches
python3 checks/comment_census.py --root . --update .code-docs-length.json   # lock in drops; refuses any rise
python3 checks/comment_census.py --root . --report                          # per package: ratio, char ratio, over-cap lines (context)
```

Exit 0 is clean. Exit 1 is a finding: an over-cap block, a rise, an unmeasured
baseline key, or a refused update. Exit 2 is a bad config, a missing baseline or
bad usage. Gate on the exit status. Only prod code counts. Tests, examples,
benches, scripts, generated files, vendored trees and installed agent rules and
skills (`.claude/rules/`, `.opencode/skills/`) do not count. The census
reads tracked files only, so `git add` a new file before a local `--check`, or
the check never sees it.

## Adopting the Census

`.code-docs.json` at the repository root:

```json
{
  "library": ["crates/client-sdk", "packages/api"],
  "caps": {"plain": 5, "doc": 10, "library_public": 15},
  "strip_sections": true,
  "interface_files": ["src/schema/*.ts"]
}
```

- **`library`** lists the paths of packages with a consumer outside this
  repository: Cargo `publish` not `false`, npm without `"private": true`, or a
  Python package other projects install. Anything undeclared is app-kind, and
  `"."` makes the whole repository one library.
- **`caps` and `strip_sections`** are shown at their defaults. Omit them to take
  the defaults.
- **`interface_files`** lists source files a generator the census cannot see
  copies to users, such as a TypeScript file a JSON Schema is built from. Their
  doc comments count as interface blocks (LEN-08), and the cleanup check
  freezes them.
- **Any other key exits 2.** `--update` rewrites the whole baseline. Keep every
  other check's settings and baseline in files of their own.

1. Commit `.code-docs.json`.
2. Run `python3 checks/comment_census.py --root . --update .code-docs-length.json --allow-regression`
   once. Every key rises from zero, so the first write needs the flag. Commit the
   baseline.
3. Run `--check .code-docs-length.json` in CI as a blocking job on every change.
4. When the check prints `N files dropped below the baseline`, run `--update`
   without the flag and commit the baseline in the same change.

## Rules the Census Enforces

The gate above is the verification for every row; each cell names what goes
red. Existing over-cap blocks sit in the baseline, and MUST binds every new
block and every block a change grows.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| LEN-01 | Keep every plain comment block (`//`, `#`) to 5 lines or fewer, in every package kind. A blank line never splits a block, and no guard or marker exempts one. | Most agent-written codebases measured run 3 to 6 times the human share of plain blocks over 5 lines. Under a blank-line split, a 6-line block with one blank line in it passes as two 3-line blocks. | `--over-cap` lists the block as LEN-01, and `--check` fails the file's rise. Planted: a 6-line `//` block with one blank line after its third line is listed as one 6-line block. | MUST for new and grown blocks |
| LEN-02 | Keep every doc block to 10 lines or fewer. The one exception: a public item's doc block in a library-kind package may run to 15. Public means reachable from outside the package: Rust `pub` with no restriction, a Python name with no leading underscore under no private owner, a TS/JS export or a member of one, a Go exported identifier, Java `public` or an interface member, Kotlin `public` or no modifier. Everything else, including `pub(crate)` and Kotlin `internal`, gets 10. A block the census cannot place, such as a module doc, gets its package kind's tier. | Without the visibility test, a private helper in a library writes to public-contract length. In human libraries, public doc blocks have a p90 of 20 lines and non-public ones 9. | `--over-cap` lists the block as LEN-02 and names the tier it applied. Planted: two 11-line doc blocks in a declared library, one on `pub fn` and one on `fn`. Only the `fn` block is listed. | MUST for new and grown blocks |
| LEN-03 | Gate CI on the absolute count of prod comment lines in over-cap blocks, keyed per file, against a checked-in baseline. Run `--check` on every change and `--update` to lock in drops. A rise goes in only as `--update --allow-regression` in a diff whose description names every raised key. | The absolute count pays nothing for deleting a within-cap guard. It pays no more for deleting an over-cap guard than for compressing one. A per-file key stops one file's new essay from hiding behind another file's cut. A baseline key whose file still exists but was not measured fails, so an added generated-file header never reads as a drop. A move out of prod scope (into tests, examples or a skipped directory) does read as a drop, and no check catches it: review every dropped key whose file moved. | `--check` exits 1 on a rise or an unmeasured key. `--update` without the flag exits 1 on a rise. Planted: append a 6-line `//` block to a baselined file. `--check` exits 1. | MUST |
| LEN-07 | When counting a doc block, skip the sections that restate the signature or give examples. These are fenced code in any language; Rust `# Errors`, `# Panics`, `# Examples` and `# Aborts`; Python `Args:`, `Returns:`, `Raises:`, `Yields:`, `Attributes:`, `Example:` and `Examples:`, plus reST `:param:`, `:type:`, `:returns:`, `:rtype:` and `:raises:`, a reST `::` literal block and the indented lines under it, and a `>>>` doctest line; JSDoc/TSDoc `@param`, `@returns`, `@throws` and `@example`; Javadoc/KDoc `@param`, `@return`, `@throws` and `@see`. Always count `# Safety`, `# Undefined behavior`, `# Implementation notes`, `Note:` and `WARNING` text, and never move prose under a skipped heading to fit. | Without this, the cap taxes the parameter lists a caller needs. Two of the three human doc guards over their tier are over only because of such a section. A heading that bought length would be a marker exemption. | `--over-cap` prints `N structured-section lines not counted` beside the count, and `--report` prints a `stripped` column per package. Planted: a 17-line docstring whose 9-line `Args:` section is skipped counts 8 and is not listed. A Rust doc with 12 lines under `# Safety` is listed. | SHOULD: keep `"strip_sections": true`. Declining it, set `library_public` to 20 |
| LEN-08 | Count a doc block that renders into a user surface as the doc block of a public item. That covers Rust items deriving clap `Parser`, `Args`, `Subcommand` or `ValueEnum`, or schemars `JsonSchema`, plus click and typer command docstrings and pydantic model docstrings. Move user reference longer than the cap into the generator's explicit text attribute (`long_about`, `long_help`, `after_long_help`, `#[schemars(description = ...)]`). Move maintainer rationale into a plain comment or a record, never into the user surface (SRF-02). | clap and schemars render the whole doc comment. An essay on a derived item therefore ships in `--help` or a published schema. Left uncounted, it also escapes LEN-02. | `--over-cap` lists the block as LEN-08. `python3 checks/comment_census.py --root . --list-blocks --kind interface --min-block 11` lists interface blocks by length. | MUST for new and grown blocks |

## Rules Review Enforces

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| LEN-04 | Set no comment:code target anywhere: not in instructions, rules, CI or review. Never size a new comment to the file around it. Ratio and char ratio appear only in `--report`, beside the human bands, as context. | A ratio target taxes every new within-cap guard and always pays more for deleting a comment than for compressing it. Writing to the surrounding density feeds on an already inflated file. In one agent-written codebase, new code added long-block lines at 4 times its earlier rate within six months (173 to 728 lines per 1,000 code lines, April to September 2026). | `rg -n -i --hidden --glob '!.git' --glob '!**/code-docs*' -e 'comment density' -e 'comment:code' -e 'comment-to-code' -e 'comment ratio' .` Any hit that sets a target, or asks an agent to match a file's density, is a finding. Reading check: judge a new block in a dense file against its cap, never against its neighbours. | MUST |
| LEN-05 | Bring an over-cap guard under the cap by splitting it, never by exempting it, keeping it whole or truncating it. (1) Write one comment per breaking edit, at that edit's own line (GRD-03). (2) Keep caller-visible preconditions in the doc block, and make implementation hazards plain comments. (3) Move the rest behind a file-qualified pointer. Delete a copy of a guard that already sits at its line. If one guard's constraint and consequence still overflow, tighten the wording and never drop either half (GRD-01). | "It is a guard, so it may stay long" protects exactly the doc essays that restate guards already written at their lines. Rewritten to human shape, guards run 1 to 4 plain lines or 3 to 9 doc lines. In a blind reason-recovery eval, cutting 13 over-cap guards to their first line lost 15 points of reason recovery and 21 of full recovery; the clause-by-clause rewrite lost nothing. | Reading heuristic: for each over-cap block, count the distinct breaking edits it names. More than one means split it, not shrink it. Search the enclosing function for a plain comment stating the same edit. A hit means delete the doc copy. Planted: a 12-line `// SAFETY:` block is listed by `--over-cap`. | MUST for never truncating or exempting; SHOULD for the split order |
| LEN-06 | When a diff removes a comment block the guard recogniser fires on, or shrinks one so it no longer fires, keep its constraint and consequence (GRD-01), or name in one local line a test or lint proven to fail on its breaking edit by a planted run (GRD-07). A pointer never replaces a guard's constraint and consequence. List that block for review, with every other removed block of 3 or more lines. The list informs review and never fails a diff or runs as a CI gate. | The ratchet cannot see a deletion. A deleted within-cap guard leaves every count unchanged, and deleting an over-cap guard earns the same credit as compressing it, for less work. The recogniser finds about 43% of guards, so a quiet result proves nothing. On real history, a hunk-level gate flagged only sound edits. | Build the list by reading, with the commands below. For a cleanup diff, `python3 checks/cleanup_check.py --base origin/main` prints the shortened-guard items and blocks a flagged removal that has no replacement (CLN-01). | SHOULD |

```sh
git diff -U0 --diff-algorithm=histogram "$(git merge-base origin/main HEAD)" -- . ':(exclude)*.md' | grep -e '^-' | grep -v -e '^---'
BLOCK='The flush runs before the rename, otherwise a crash leaves a torn file.'
python3 checks/guard_recogniser.py "$BLOCK"   # FIRE: list it. quiet: unclassified, never safe
```

LEN-05 as a diff. Wrong: two guards bundled into a doc essay.

```rust
/// Opens the pipe read-write, because a read-only open blocks until a writer
/// appears and startup hangs. Writes the header before any body line, because
/// readers take line 2 as the anchor and misparse a body line there. [+9 lines]
```

Right: each guard at its own line, as a plain hazard comment.

```rust
/// Opens the pipe and writes its header.
fn open_pipe(path: &Path) -> io::Result<File> {
    // Read-only would block until a writer appears, hanging startup.
    let mut f = OpenOptions::new().read(true).write(true).open(path)?;
    // Header first: readers take line 2 as the anchor and misparse a body line.
    write_header(&mut f)?;
    Ok(f)
}
```

## Holding the Line

| Situation | Do | Never |
|---|---|---|
| `--check` is red on a file you touched | Split or compress the new or grown block (LEN-05) | Pass `--allow-regression`, or delete a guard elsewhere in the file to pay for the rise |
| Your change adds lines to an existing over-cap block | Put the new fact in its own comment at the line it concerns, within the cap, or shorten the block by as many lines, keeping every guard's constraint and consequence (GRD-01) | Pass `--allow-regression`, or leave the growth for a later cleanup |
| A baseline key reads as unmeasured | Find why the file left prod scope, such as an added generated header | Hand-edit the baseline |
| `--check` prints a drop for a file the change moved | Confirm the destination is prod code: a move into tests, examples or a skipped directory hides its blocks and reads as a drop | Lock in the drop before that review |
| A file was renamed or split | `--update --allow-regression`, and confirm the raised keys add up to the dropped ones | Treat the new key's rise as new comment mass |
| The check prints `files dropped below the baseline` | `--update` without the flag, committed with the change | Leave the drop unlocked for the next change to spend |
| An existing over-cap block is in the way | Shorten it through the `code-docs-cleanup` skill, which proves each shortened guard kept its reason | Cut it ad hoc: a deleted guard earns the same credit as a careful compression, and nothing mechanical tells them apart |

## Human Reference Numbers

Context for choosing or overriding a cap, never a target. Measured 2026-09-27 on
33 human-written open-source repositories (Rust, Go, Python, TypeScript, Java,
Kotlin), prod code. Medians and ratio bands use the last commit before 2022 of 16
apps and 16 libraries.

| Measure | Apps | Libraries | Where the cap sits |
|---|---:|---:|---|
| Plain block p90, lines | 3 | 3 | cap 5 flags 4.5% of app and 6.0% of library plain blocks, about p95 |
| Doc block p90, all items | 6 | 13.5 | n/a |
| Doc block p90 / p95, public items | 6 / 10 | 20 / 30 | 10 flags 4.5% in apps. Library 15 flags 14.3%, 9.3% after LEN-07 |
| Doc block p90 / p95, non-public items | 6 / 8 | 9 / 15 | 10 flags 3.2% in apps, 8.2% in libraries (5.3% after LEN-07) |
| Guard length, median, both kinds pooled | 2 | 2 | 0 of 30 plain guards over 5. 3 of 49 doc guards over their tier, 1 after LEN-07 (split by LEN-05) |
| Comment lines per code line, median (IQR) | 0.117 (0.056-0.199) | 0.273 (0.204-0.589) | printed by `--report`, never gated (LEN-04) |

Agent-written codebases measured the same day ran up to 20 times the human share
of over-cap blocks, and their guards had a median of 6 lines against 2.

## What Agents Get Wrong Here

Ranked by how often each bites.

1. **Matching the room.** The agent reads a file's 18-line doc blocks as the norm and writes to it, on every edit in a dense file (LEN-04, LEN-03).
2. **Writing a guard long and in the doc register.** It bundles several guards into one doc essay above the item (LEN-01, LEN-02, LEN-05).
3. **Deleting instead of compressing when the ratchet goes red.** It is the same credit for less work, and no check catches it (LEN-06).
4. **"It is a guard, so it may stay long."** The agent adds a marker or a `# Safety` heading to protect a block (LEN-05, LEN-07).
5. **Reaching for `--allow-regression`, or plain `--update`, when the check is red** (LEN-03).
6. **Splitting a block with a blank line**, which the census counts as one block (LEN-01).
7. **Escaping the census** with a generated-file header, which fails as an unmeasured key, or a move into tests or a skipped directory, which reads as a drop and needs review (LEN-03).
8. **Moving an essay verbatim to a record.** The count drops and the text does not shrink. The cleanup skill owns shortening relocated prose.
9. **Hiding prose under a skipped heading** such as a long `Returns:` or `# Examples` (LEN-07).
10. **Writing a maintainer essay on a clap or schemars item**, where it ships to users (LEN-08).
