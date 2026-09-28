---
name: code-docs-cleanup
description: Clause-by-clause procedure for shortening and cleaning up source-code comments without losing a guard, the comment that says why a non-obvious line must stay as it is and what breaks if it changes. Use when someone asks to shorten, trim or clean up comments, says a file has too many or overly verbose comments or is over-commented, cut comment density or comment bloat, split an essay comment or an over-long doc comment or docstring, move design rationale out of code into a decision record, remove plan, work-package, review-round or decision IDs from comments, delete comments that restate the code, bring a file under the code-docs length caps, or prepare a file or package for the code-docs comment ratchet. Not for writing the comment standards, which the code-docs rule carries, not for --help, JSON Schema, MCP tool descriptions or other text that renders to users, and not for user-facing documentation pages, which the docs-quality rules cover.
license: Apache-2.0
metadata:
  summary: Shortens comments clause by clause, guard first, gated by a structural diff check and a cold reason re-check for every shortened guard
  keywords: comments,code-comments,comment-cleanup,comment-density,doc-comments,docstrings,rustdoc,jsdoc,tsdoc,godoc,javadoc,kdoc,guards,why-comments,essay-comments,decision-records,adr,plan-ids,process-ids,pointers,ratchet,length-caps,refactor,code-docs
  repository: https://github.com/ocx-sh/grimoire-lore
---

# code-docs-cleanup

Shorten the comments in a source file without losing a guard. A guard is any
clause that names a constraint a plausible edit would break, with or without the
consequence. It is how the next agent learns why a non-obvious line looks the way
it does and whether it may change it.

The comment standards live in the installed `code-docs` rule, cited here by
ID: GRD (guards), RTE (routing), LNK (pointers and IDs), SRF (rendered
surfaces) and LEN (length and ratchet). The cleanup-safety rules, CLN, are this
skill's own: [Cleanup rules](#cleanup-rules). Three facts shape every step (as
of 2026-09):

- In agent-written code most comment lines are guards and contracts, not essays.
  Deleting by category or by length deletes guards.
- The shipped guard recogniser finds fewer than half of real guards (recall
  about 0.43). A block it does not flag is unclassified, never safe to cut.
- The editing agent's belief that the meaning survived is not evidence. The
  structural check and a cold re-check are.

Contents: [When to run](#when-to-run) · [Setup](#setup) · [Scope](#scope) ·
[Procedure](#procedure) · [Never touch](#never-touch) · [Cleanup rules](#cleanup-rules) · [Report](#report) ·
[MUST rows](#must-rows-this-procedure-surfaces) ·
[What agents get wrong](#what-agents-get-wrong) · [References](#references)

## When to run

Run it when:

- A change already edits a file whose comments are over the length caps
  (LEN-01, LEN-02), carry bare process IDs (LNK-01), or hold an essay.
- The owner names files for a sweep, or asks to bring a package down before its
  ratchet baseline is recorded (LEN-03).
- A review flags an over-cap block, or a pointer left beside the essay it was
  meant to replace (RTE-09).

Do not run it when:

- The text renders to users: `--help`, JSON Schema, MCP tool descriptions,
  argparse or commander help, or a published package's API docs. That is an
  interface change (SRF-01, SRF-02). It moves rationale into plain comments and
  may regenerate goldens, which this skill never does.
- The file is a test, generated, vendored, a lock or manifest file, or a golden.
  Bare IDs in test files and test regions (LNK-01's `bare-id-test` key) go in a
  separate test-only change: list them in the report.
- Another branch has the file in flight (see [Scope](#scope)).
- The fix is code: a type that owns a guard (GRD-12), a lint to enable (GRD-09,
  GRD-10), a test to rename. Each is its own reviewable diff.
- The repo's own rules protect the comment kind, for example a local rule that
  keeps section dividers or phase markers. The local rule wins until it is
  amended. Leave those comments and report the conflict.

## Setup

1. Find the rule's checks, in the project first, then in the global client
   directories:
   `find . "$HOME/.claude" "$HOME/.config" -path '*/code-docs/checks/cleanup_check.py' -not -path '*/node_modules/*' 2>/dev/null | head -n 1`.
   `CHECKS` below is the directory of that hit; any installed copy works. Read
   the `code-docs.md` index installed beside the `code-docs/` folder. No hit
   means the rule is installed neither in the project nor globally: install it
   with `grim add ghcr.io/ocx-sh/lore/code-docs`. Where the client tool cannot
   install it, copy `code-docs.md` and the `code-docs/` folder from the rule's
   source into the client's rules directory (such as `.claude/rules/`) with
   `rsync -a --exclude __pycache__` or `git archive`, so no stale `.pyc` file
   rides along, commit that on its own before the cleanup, and run the find
   again.
2. `python3 "$CHECKS/cleanup_check.py" --self-test` and
   `python3 "$CHECKS/guard_recogniser.py" --self-test` both exit 0.
3. `git status --porcelain` lists nothing untracked that the cleanup did not
   create. `cleanup_check.py` reads every untracked file as an added path and
   fails it (CLN-05). An untracked file you did not create is the owner's: never
   delete or revert it. Run the cleanup from a clean worktree, or ask the owner
   to move the file.
4. Pick the mode and set `BASE=HEAD`. The check fails any non-prose line that
   differs from the base, so the working tree holds only uncommitted cleanup:
   the touched files', or in a sweep one file's.

| Mode | When | Before each file |
|---|---|---|
| Touched files (default) | Cleaning files the current change already edits | Commit the change's code first |
| Sweep | The owner named files, or asked for a package sweep | Commit the previous file's cleanup, on a branch made for the sweep from the default branch |

5. Declare every library-kind package in `.code-docs.json` before ranking or
   cleaning anything: `python3 "$CHECKS/comment_census.py" --root . --report`
   lists undeclared packages that look published. The caps, and so the scope
   of the sweep, depend on the declaration.

Caps are the rule's defaults: plain blocks 5 lines, doc blocks 10, or 15 for a
public item in a library-kind package. The census reads them, and the list of
library packages, from `.code-docs.json` at the repo root. An adopter may
override both there, and list under `interface_files` the sources a generator
the checks do not know renders, such as a TS-to-JSON-Schema script. Their doc
comments are then frozen like clap's.

## Scope

- **Touched files:** only the files the change already edits (CLN-08). Never
  widen to a neighbour because it looks worse.
- **Sweep:** one file per commit (CLN-08). Rank packages with
  `python3 "$CHECKS/comment_census.py" --root . --report` and files with
  `--over-cap`. Before starting a file, and again before merge, skip it if either
  command prints anything:

```
FILE=src/example.rs; MAIN=origin/main; SELF=$(git branch --show-current)
git worktree list --porcelain | grep -e '^branch' | cut -d/ -f3- | grep -v -x -F -e "$SELF" | xargs -r -I{} git log -1 --format='{} %h %cs' "$MAIN..{}" -- "$FILE" || echo "in-flight check unavailable"
FILE=src/example.rs gh pr list --state open --limit 100 --json headRefName,files --jq '.[] | select(any(.files[]; .path == env.FILE)) | .headRefName' || echo "in-flight check unavailable"
```

The first names a worktree branch with commits on the file, the second an open
pull request that edits it. Commit age settles nothing: a branch idle for a
week still merges. `in-flight check unavailable` means unknown, never clear:
list the file as unchecked in the report and have the owner confirm it before
merge.

## Procedure

Work one file at a time, one block at a time, top to bottom. Steps 1 to 3 read
and classify every block. Steps 3 to 7 edit prose only, and only in over-cap
blocks, blocks with a bare ID, and guards that fail GRD-01 or GRD-05. Every
other block stays byte-identical. Steps 8 to 10 prove the edit.

### 1. List the blocks

```
FILE=src/example.rs
python3 "$CHECKS/comment_census.py" --root . --over-cap | grep -F -e "$FILE:"
python3 "$CHECKS/guard_recogniser.py" --root . | grep -F -e "$FILE:"
python3 "$CHECKS/linkage_check.py" ids --root . | grep -F -e "$FILE:"
python3 "$CHECKS/linkage_check.py" pointers --root . | grep -F -e "$FILE:"
git grep -n -F -e "${FILE##*/}" -- . ":(exclude)$FILE" ':(exclude)*.md'
MODULE=example; DOTTED=package.example   # the module without its extension, and its Python dotted path
git grep -n -F -e "$MODULE.js" -e "$MODULE'" -e "$DOTTED" -- . ":(exclude)$FILE" ':(exclude)*.md'
git grep -n -F -e "::: $DOTTED" -e "automodule:: $DOTTED" -- .
```

The first four rank the work. They do not bound it: every comment block in the
file goes through step 3, flagged or not. The rest find every reader, under
every name it can use: the path (a script, task file, CI config or test), the
module name without its extension (a TS/JS import ending in `.js`), the Python
dotted module path, and a doc-generator directive (mkdocstrings `::: pkg.module`,
Sphinx `automodule::`). Never exclude `*.md` from the directive search: a hit
there means the module's docstrings render into published docs, so treat them
as interface text (CLN-03). Read every hit; a name can match an unrelated file
(`_rest.py` also matches `01_shellcheck_rest.py`). One that reads the file owns
its comment text: text a check asserts stays verbatim. Record each in the
worklist.

### 2. Record every guard before you edit

Write a worklist in a scratch directory outside the repo. One row per guard
clause: the line it guards, the constraint, the breaking edit, the consequence,
and any owner the text claims (a test, a lint, a type). When unsure whether a
clause is a guard, it is one.

The worklist feeds steps 5, 8 and 9. It is never an input to the check, which
derives everything from the diff so that the editing agent never writes its own
grading input (CLN-04).

### 3. Classify clause by clause, guard first

Split every block into clauses and tag each with
[references/decision-list.md](references/decision-list.md) (RTE-01). Check for a
guard before anything else.

- Delete a clause outright only when the list positively identifies it as
  narration, tautology, a banner, a bare ID or unquantified pure provenance
  (CLN-01). Argument and record paraphrase leave only through step 6.
- Length, a history phrase or a `Step N:` opener identifies nothing.
- A block the recogniser flags, a `SAFETY:` label, the repo's deferred-shortcut
  marker (for example `ponytail:`) and a comment another rule mandates (a
  discard rationale, a lint-suppression reason) are never deleted whole (GRD-06).
- Text a repo check asserts (step 1) stays verbatim.
- A clause that fits no row stays.

### 4. Rewrite each guard as fact plus consequence, at its line

- Keep the constraint and the concrete consequence of breaking it. No edit goes
  below that floor (GRD-01). "Important", "careful" or "load-bearing" alone is
  not a consequence.
- Keep nothing else in the guard: no history, no weighed alternatives, no bare
  ID. Name the tempting edit only when the constraint does not imply it, and
  name the sibling the code must not be unified with when that is the risk
  (GRD-02).
- Split a guard block over 5 lines clause by clause. Every breaking edit the
  original forbade keeps its own sentence, at the line it guards. Never
  paraphrase several guards into one line (CLN-02, LEN-05). State each guard
  once (GRD-03).
- Phrase it as a present-tense fact, never a command to the reader (GRD-05).
  History that still constrains an edit becomes a present-tense guard (RTE-11).
- Put a caller-visible precondition in the doc comment and an implementation
  hazard in a plain comment at its line (GRD-04). Add no marker prefix.
  `SAFETY:` stays where the language uses it.
- When the guard clauses alone exceed the cap, keep them all and tighten the
  wording. The cap never drops a guard.
- Rewrite a clause as you move it. The check fails prose the recogniser does
  not flag that reappears, 80% or more intact, elsewhere in the same file
  (CLN-04).
- Never invent a consequence the original does not state and the code does not
  show. Keep the original wording and list the block in the report.

### 5. Shrink a guard to its owner only after a planted edit fails it

A guard shrinks to a one-line pointer only when every breaking edit it names
has a proven owner (CLN-07, GRD-07, RTE-03). An owner is an enabled lint, a
scoped `deny`, a type check, or any test, named in the block or not. It is
proven only when it passes on the unmodified scratch copy and fails on that
exact edit applied to the same copy: a test already red before the edit proves
nothing. Run the suite on the planted edit to find the candidates, then each
red test alone:

```
SCRATCH=$(mktemp -d)/proof
git worktree add --detach "$SCRATCH" HEAD
export CARGO_TARGET_DIR="$PWD/target"   # reuse the warm build
# TS, no workspace packages: ln -s "$PWD/node_modules" "$SCRATCH/node_modules"
# TS, in a workspace: (cd "$SCRATCH" && npm ci --offline) or (cd "$SCRATCH" && pnpm install --offline --frozen-lockfile)
# apply ONE breaking edit from the worklist by hand inside $SCRATCH, then:
(cd "$SCRATCH" && timeout 1800 cargo test -p example_crate)
(cd "$SCRATCH" && timeout 1800 cargo test -p example_crate a_file_ttl_cannot_outlive_the_ceiling)
git worktree remove --force "$SCRATCH"
```

Swap the commands for the repo's runner, with a name filter for the second, such
as `uv run pytest -k NAME`, `npx vitest run FILE -t NAME` or `go test -run NAME ./pkg/`,
or the lint or type check itself (`cargo clippy`, `tsc --noEmit`, `npx pyright`).
A shared environment that resolves the package back to the main tree, such as
an editable install or a symlinked `node_modules` in a workspace, never sees
the edit; install offline from the lockfile there instead.

| Outcome | Counts as |
|---|---|
| The test prints a failure, or the lint or type check errors on the edited line | Owner proven for this edit |
| A build error from a leftover the edit orphaned, such as an unused import or binding under deny-warnings | No result. Remove the leftover and run again |
| Exit 124 with no test result line | Proven only when the guard's stated consequence is the hang itself |
| The candidate already fails on the unmodified scratch copy | Not an owner. It proves nothing about this edit |
| The owner stays green | Not an owner. The prose stays |
| Two tests, one fails | Only the failing test is an owner |
| The owner is compiled out on this host, such as a platform-gated assert | Unproven here. Run it on its target platform, or the prose stays |

- The pointer names every test that failed on the edit and none that stayed
  green. Name the symbol, never a plan ID or a `file:line` (RTE-05).
- Keep that one local line naming the owner, even when the owning construct
  states itself, such as `assert_never(x)` or a scoped `#[deny(...)]`. This
  skill never cuts a guard to nothing (RTE-03).
- Any breaking edit without a proven owner keeps its mechanism, the edit and
  the consequence.
- A mutation tool is optional support, never the proof. Budget one warm build
  per repo, then seconds per test.

### 6. Split an essay

- Guard, contract and why clauses stay, rewritten per step 4.
- An argument clause (alternatives weighed, the design story) goes one of three
  ways:
  - A tracked record already holds it in its own words: delete it, keep one
    pointer to that record, and pass the record to the check with `--cites`
    (RTE-09). The check lists `--cites` records for a human to confirm.
  - It argues a decision someone could reopen, and it is not limited in scope,
    time, risk and cost: append it to a decision record, and the code keeps one
    pointer (RTE-10).
  - Otherwise the choice is small: keep its conclusion as one why sentence when
    the code does not show it, and delete the rest.
- A pointer is `path/to/record.md#section`. The file is tracked (LNK-02). The
  anchor is the shortest unique heading prefix, never a line number (LNK-03).
  Never point at a commit, a branch, a backup ref or a gitignored path (RTE-02).
- A record is new, or receives additions only. Declare each one to the check with
  `--relocation` (CLN-05). The moved text must land there, and the paraphrase
  never stays beside the pointer (CLN-04).
- An added record pointer is legal only when the block named that file before
  the edit, or the file is a `--relocation` or `--cites` record. The check fails
  any other (CLN-04, LNK-05).
- Some clauses belong somewhere this diff may not touch. A universal negative
  belongs on the doc comment of the test that asserts it (RTE-04). An invariant
  true at every file a path-scoped rule matches belongs in that rule (RTE-07).
  Leave these where they are and list them in the report.

### 7. Remove bare IDs, carry every pointer

- Delete a bare process ID and keep the constraint it labelled, in words
  (LNK-01). This covers the rule's families, the repo's own families (LNK-14),
  and a short label cited outside the block that defines it (LNK-13).
- Add a record pointer in its place only when the same block already names that
  file, or when you have read the record and it holds the removed argument in
  its own words, passed to the check with `--cites` (LNK-05). Never guess which
  document a colliding ID meant. A pointer that replaces prose the ID labelled
  follows step 6.
- Keep every file-qualified pointer (GRD-11):
  - A pointer to a record that moved: find the move and repoint it (LNK-10).
  - A dead pointer you cannot trace: leave it and report it.
  - A pointer to a commit, a branch or an untracked path is no pointer
    (RTE-02). Replace it with the constraint it stood for, stated in words.

### 8. Run the structural gate

```
python3 "$CHECKS/cleanup_check.py" --base "$BASE" --relocation docs/adr/0042-example-record.md --cites docs/design/decisions.md
```

Pass `--relocation` once per record that received text, and `--cites` once per
record a new pointer names because it already held the removed argument (step
6). Drop a flag that has no record. Exit 1 lists blocking findings, including
any added pointer step 6 does not allow: fix each one and run it again. Exit 0
may still print `CLN-02 review` items and the `--cites` records. Read each
review item against the worklist: every breaking edit still has its own
sentence.

Then check that no pointer was dropped (GRD-11). This prints the pointers the
diff removed and did not add back:

```
MB=$(git merge-base "$BASE" HEAD)
comm -23 <(git diff -U0 "$MB" -- . ':(exclude)*.md' | grep -e '^-' | grep -v -e '^---' | grep -oE -e '[A-Za-z0-9_./-]+\.md' | sort -u) <(git diff -U0 "$MB" -- . ':(exclude)*.md' | grep -e '^+' | grep -v -e '^+++' | grep -oE -e '[A-Za-z0-9_./-]+\.md' | sort -u)
```

Then run the repo's full local check chain: every CI job for these files and
every task-runner target (`task check`, `make check`, `npm run` scripts), not
only what CI runs. Run it at `BASE` first, in a scratch worktree or before the
first edit. Only a failure new since `BASE` counts; report the pre-existing
reds. The chain must include:

- the doctests: Rust `cargo test --doc` on each crate with a lib target, and
  Python `pytest --doctest-modules` where the repo uses doctests;
- the doc build with warnings denied, which catches a broken intra-doc link:
  `RUSTDOCFLAGS='-D warnings' cargo doc --no-deps`;
- the type checker, where a deleted `@ts-expect-error` or `# type: ignore`
  surfaces;
- the linter.

### 9. Re-check the reason for every shortened guard

For each guard you shortened, run a fresh read-only session against an
exported copy of the edited tree with no git history, so the probe can read
neither the commit message that carries the guard's reason (step 10) nor the
old comment through git. Ask the unled question first ("Why is it written this
way? Could it be written differently without changing anything that
matters?"), score it, and only then ask the led question naming the recorded
breaking edit (CLN-06). Prompts, command and scoring are in
[references/reason-recheck.md](references/reason-recheck.md).

- It is SHOULD for touched files and required for every shortened guard in a
  sweep file.
- Touched files run it before the commit. A sweep commits one file at a time
  and runs it once, over every guard the sweep shortened, before the final
  report.
- A guard passes at mechanism plus consequence with a `keep` verdict. Anything
  lower goes back to step 4, rewritten or restored from the base. In a sweep,
  that fix is a new commit that passes step 8 again.

### 10. Commit

- Commit the cleanup on its own, with only prose in it. Never fold it into a code
  commit, and never squash it into one later.
- In a sweep, one file per commit, together with any record that file's prose
  moved to.
- If the repo keeps the rule's baselines, record the drop in the same commit,
  never with `--allow-regression`, and let `--check` confirm it (LEN-03,
  LNK-01). Do it after step 8: `cleanup_check.py` fails any changed non-source
  path, baselines included.

```
python3 "$CHECKS/comment_census.py" --root . --update .code-docs-length.json
python3 "$CHECKS/linkage_check.py" ids --root . --update .code-docs-linkage.json
python3 "$CHECKS/linkage_check.py" pointers --root . --update .code-docs-linkage.json
python3 "$CHECKS/comment_census.py" --root . --check .code-docs-length.json
```

- In the body, list each shortened guard with its proof: the owner that failed,
  or the re-check level (in a sweep, the final report carries the levels). Also
  list the records that received text and what was left for another change.

## Never touch

`cleanup_check.py` fails the diff on each of these. Knowing them up front
saves the rerun.

| Line or path | What goes wrong | Rule |
|---|---|---|
| Code, including a comment that shares its line with code | A "comment-only" diff changes behaviour | CLN-03 |
| Doc text that renders to users (clap, schemars, click, typer, pydantic, MCP tools, files under `interface_files`) | The user contract changes, and goldens drift | CLN-03 |
| Tool directives: `@ts-expect-error`, `# noqa`, `# type: ignore`, `eslint-disable`, `nolint`, `//go:build` | The build or the lint result changes | CLN-03 |
| Licence headers | Legal text changes | CLN-03 |
| Fenced code, a Python `Example:`/`Examples:` section, a reST `::` literal block, and a `>>>` doctest line, inside doc comments | A test is deleted | CLN-03 |
| A Python docstring that is the function's only statement | The body empties and the file stops parsing | CLN-03 |
| Adding or deleting a source file | Code moves under cover of prose | CLN-03 |
| Test files and in-file test regions such as `#[cfg(test)]` | A test is "tidied" | CLN-05 |
| Lock, manifest, generated, golden or snapshot files | Dependencies or recorded output change | CLN-05 |
| Any other non-source path, except a declared record that only grows | Unreviewed edits ride along | CLN-05 |

## Cleanup rules

MUST = Block. SHOULD = Warn: fix, or state why not in the commit body.

| ID | Rule | Verification | Severity |
|---|---|---|---|
| CLN-01 | Classify every block clause by clause, guard first. Delete only a clause positively identified as narration, tautology, a banner, a bare ID or unquantified pure provenance. A block that matches nothing, or that the recogniser flags, stays. | Each deletion passes its decision-list test read against the code. `cleanup_check.py` fails a flagged or labelled block removed whole with no replacement | MUST |
| CLN-02 | Before shortening a guard block over 5 lines, split it into clauses. Every clause naming a breaking edit and its consequence keeps its own sentence at the line it guards. Never paraphrase several guards into one line. | List every edit the block forbade and find each surviving sentence. `cleanup_check.py` prints `CLN-02 review` items and never fails on them | MUST |
| CLN-03 | Leave every non-prose line byte-identical: code, interface doc text, tool directives, licence headers, fenced code and doctests. A Python `Example:`/`Examples:` section and a reST `::` literal block are frozen the same way as fenced code. A Python file that parsed at the base still parses. | `cleanup_check.py`, then the repo's own build, doc, doctest and lint gate | MUST |
| CLN-04 | Removed prose is gone: it never reappears at 80% token overlap or more in the file's other comments, a declared relocation lands in its record, and a pointer never sits beside the paraphrase it replaced. An added record pointer names a file the block named before the edit, a `--relocation` record, or a `--cites` record that already holds the removed argument. Every input comes from the diff, never from a list the editing agent wrote. | `cleanup_check.py`, with `--relocation` and `--cites` once per record; a human confirms each `--cites` record | MUST |
| CLN-05 | Touch no test file or in-file test region, lock or manifest, generated or golden file, or non-source path other than a declared record that is new or only grows. | `cleanup_check.py`. `RECORD=docs/adr/0042-example-record.md; git diff --numstat "$BASE" -- "$RECORD"` shows 0 deleted lines | MUST |
| CLN-06 | Re-check every shortened guard in a fresh read-only session: the unled question first, scored for mechanism plus consequence, then the led question naming the recorded breaking edit. A `may-change` after a strong unled answer fails. | Step 9, with [references/reason-recheck.md](references/reason-recheck.md) | SHOULD; required for every shortened guard in a sweep |
| CLN-07 | Shrink a guard to a one-line pointer only when every breaking edit it names has an owner that failed on that edit in a scratch copy: an enabled lint, a scoped `deny`, a type check or any test, named in the block or not. The pointer names every test that failed and none that stayed green. An edit with no proven owner keeps its mechanism, edit and consequence. | Step 5's planted-edit run, once per owner and edit | MUST |
| CLN-08 | Clean only the files the change already touches. A sweep runs one file per commit and skips any file another branch or an open pull request has in flight, checked before starting it and again before merge. | The two commands under [Scope](#scope) print nothing | SHOULD |

## Report

Return, per file:

- over-cap lines before and after, from `--over-cap`;
- blocks deleted, guards shortened (each with its owner proof or re-check level),
  and records that received text;
- every `CLN-02 review` item, and how you resolved it;
- every `--cites` record, for a human to confirm it holds the removed argument;
- gate failures already red at `BASE`, and files whose in-flight check was
  unavailable;
- what you left for another change: interface leaks, test-doc and rule
  candidates, dead pointers, local-rule conflicts, commented-out code, a clause
  the code contradicts, bare IDs in tests with the file's `bare-id-test` count,
  and any guard whose consequence you could not state.

## MUST rows this procedure surfaces

Restated so a cleanup run without the rule loaded still reports them with the
right ID. [Cleanup rules](#cleanup-rules) and the installed rule hold the text
and full verification.

| # | Finding | Rule |
|---|---|---|
| 1 | A block deleted without being positively identified as narration, tautology, a banner, a bare ID or unquantified pure provenance | CLN-01 |
| 2 | A recogniser-flagged block, a `SAFETY:` label, a deferred-shortcut marker, or a comment another rule mandates, removed whole with no replacement | CLN-01, GRD-06 |
| 3 | A block over 3 lines routed whole instead of clause by clause | RTE-01 |
| 4 | A guard block over 5 lines shortened without a clause split, or several guards paraphrased into one line | CLN-02 |
| 5 | A guard cut below constraint plus consequence | GRD-01 |
| 6 | A non-prose line changed: code, interface text, a directive, a licence header, fenced code or a doctest | CLN-03 |
| 7 | Removed prose survives elsewhere in the file, a declared relocation did not land, or a pointer sits beside the paraphrase it replaced | CLN-04 |
| 8 | The diff touches a test or test region, a lock or manifest, a generated or golden file, or a non-source path other than a declared record that only grows | CLN-05 |
| 9 | A guard shrunk to a pointer without a failing planted-edit run for every breaking edit, or a pointer naming a test that stayed green | CLN-07, GRD-07, RTE-03 |
| 10 | A file-qualified pointer dropped without the same target reappearing | GRD-11 |
| 11 | A record pointer added to a file the block did not name before the edit, that is neither a `--relocation` nor a `--cites` record | CLN-04, LNK-05 |
| 12 | A pointer to a commit, a branch, a backup ref or a gitignored path | RTE-02 |
| 13 | A record pointer that does not resolve to a tracked file | LNK-02 |

## What agents get wrong

Ranked, most frequent first. Each mode, its tell and its catch are in
[references/failure-modes.md](references/failure-modes.md).

1. **Collapsing a guard bundle into one paraphrase**, dropping the consequence
   clause first.
2. **Deleting by category or length:** "long means essay", "past tense means
   history", "`Step N:` means narration".
3. **Crediting an owner by its name.** A test named after the constraint often
   stays green on the breaking edit.
4. **Adding the pointer and keeping the essay** beside it.
5. **Reporting success from its own reading** instead of the check and the
   re-check.
6. **Incidental edits outside prose:** a help string, a directive, a doctest.

## References

| Read | When |
|---|---|
| [references/decision-list.md](references/decision-list.md) | Step 3 and 4: tagging a clause, writing a guard, choosing the register, meeting a cap |
| [references/reason-recheck.md](references/reason-recheck.md) | Step 9: the unled and led prompts, the session command, the scoring levels |
| [references/failure-modes.md](references/failure-modes.md) | Before a sweep, or when a check fails and the cause is unclear |
