---
# Read on demand through the index. This glob matches no file, so a client never auto-loads the file.
paths:
  - "**/.code-docs-depth-on-demand"
title: Process IDs, Record Pointers and Citations
summary: The LNK family, owning which process IDs a comment may carry, the one record-pointer form and how it resolves, what stays out of rendered text, and how a decision record's citations of code survive a rename
---

# Process IDs, Record Pointers and Citations

Code and the records that explain it point at each other, and both directions
rot silently. This file owns the links: which plan or process IDs a comment may
carry (none), the one pointer form a check can resolve, what never reaches
rendered text, and how decision records keep their citations of code alive
through a delete or rename. Where a clause goes belongs to the RTE rules. The
full leak token set and the per-generator output tests are SRF-01 and SRF-03.

Contents: [Dates and Defaults](#dates-and-defaults) · [The Gate](#the-gate) ·
[Process IDs in Comments](#process-ids-in-comments) ·
[Pointer Grammar](#pointer-grammar) · [Rendered Text](#rendered-text) ·
[Records Citing Code](#records-citing-code) ·
[The Weekly Job](#the-weekly-job) · [What Agents Get Wrong Here](#what-agents-get-wrong-here)

## Dates and Defaults

Measured 2026-09-27 with `checks/linkage_check.py` and `checks/interface_leak.py`
as shipped beside this rule, git 2.54 and lychee 0.24. `checks/` is this rule's
own directory. Run every command from the repository root.

| Default | Rule | Override |
|---|---|---|
| The ten banned ID families and the `C-S1-1` clause label | LNK-01 | `families` in the linkage config adds a repo's own. The built-in ones cannot be removed |
| A prefix needs a verdict at 5 prod comment hits | LNK-14 | `--threshold N` |
| Decision-record and path-scoped-rule globs | LNK-06, LNK-02, LNK-05 | `records` in the linkage config replaces the list; the same globs mark pointer targets for `pointers` and `cleanup_check.py` |
| `repo:` qualifiers resolve through a map of clones | LNK-02, LNK-07 | `repos` in the linkage config, paths relative to the config file |
| Weekly schedule | LNK-12 | Any schedule, never per commit |

The linkage config is one JSON file, every key optional. The check reads
`.code-docs-linkage-config.json` at the root when it exists; `--config FILE`
names another file.

```json
{"families": ["SIM", "SEAM"], "non_ids": ["LINT", "DOC-TYPE"],
 "records": ["docs/adr/*.md", ".claude/rules/*.md"], "repos": {"infra": "../infra"}}
```

## The Gate

```sh
python3 checks/linkage_check.py ids      --root . --check .code-docs-linkage.json
python3 checks/linkage_check.py pointers --root . --check .code-docs-linkage.json
python3 checks/linkage_check.py records  --root . --base origin/main
```

- **The ratchet** (`ids`, `pointers`) is a flat JSON baseline of `FILE::key`
  counts. `ids` owns `bare-id-prod`, `bare-id-test` and `short-label`;
  `pointers` owns `dead-pointer`. Both share one file and each rewrites only its
  own keys. `--check` exits 1 on any per-file rise, and a missing key reads 0.
- **`--update`** writes drops freely and refuses a rise unless
  `--allow-regression` is given. The first `--update` needs it, since every key
  rises from 0. Moving a file reds the ratchet under its new name until a
  reviewed `--update --allow-regression`: that prompt is intended.
- **Without a baseline** every finding prints and any MUST finding exits 1.
  SHOULD findings print `(advisory)` and never set the exit code. Under
  `--check`, a new short label raises its file's `short-label` count and fails
  the ratchet like any other key.
- **`records`** keeps no baseline: any stale citation exits 1.
- **Adopting, in order:** `ids --discover` until it exits 0 (LNK-14), then
  `--update .code-docs-linkage.json --allow-regression` for `ids` and
  `pointers`, commit the baseline and the config, then the three lines above
  in CI, then the weekly job (LNK-12).
- **Your own grep** for any rule here takes its file list from
  `git ls-files -z`, piped through `xargs -r -0 rg`, as the spot checks below
  do. A recursive `rg -e PATTERN .` skips hidden directories such as `.claude/`
  and descends into submodules: it found 2 of one repo's 52 ID-prefixed test
  names, and 5,013 ID lines in another where the repo held 505.

## Process IDs in Comments

`ids` reads comment text only, through the census classifier: doc, plain and
interface comments, and trailing comments on code lines. Prod and test both
count, under separate keys. Examples and scripts count as prod.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| LNK-01 | Write no bare process ID from a banned family in any comment or doc comment, prod or test. State the constraint in the present tense, or cite the record in LNK-02's form. The families: `C-`, `S-`, `WP-`, `DEC-`, `DX-`, `RUL-`, `A-`, `D-`, `D-V` and a numbered `ADR-`, plus the clause label shape `C-S1-1`. `SPDX-2.3`, `UTF-8`, `SHA-256`, `CWE-79`, `RFC 8259` and `#1234` are not IDs. | Plan IDs collide: 0 of 30 sampled `C-` IDs resolve to one document, and 14 of the top 15 IDs in one agent-written repo each name 3 to 5 unrelated decisions. Human-written code carries none, 0 hits across 33 reference repos (about 2 million lines), so a ban costs nothing. | `ids --check` (the gate). | MUST |
| LNK-13 | Cite a short label (`A2`, `H1`, `F5b`) only inside the comment block that defines it. A definition is the label and a colon at the start of a comment line, after any banner leader such as `──`. Convert the form `adr_x.md A2` to `adr_x.md#a2`. The check exempts a bare version (`V2`), RFC section shorthand (`RFC 9111 S4.3.4`) and a label inside a backtick span that holds a `[...]` character class. It does not exempt primitive type names (`U8`) or compound IDs (`C-S1-1`). | A label with no local definition names whichever `H1` the writer had in context: in one repo three files cite an `H1` defined nowhere near them, while two other files define unrelated ones. SHOULD because the shape is open: RFC shorthand alone was 104 of 551 hits in human code, and in one map-heavy app 26 of 32 hits were domain vocabulary (grid cells, heading levels). Read each hit before acting on it. | `ids`: advisory without a baseline, ratcheted as `short-label` under `--check`. | SHOULD |
| LNK-05 | When a cleanup removes a bare ID, delete it or restate the constraint it labelled. Add a record pointer only when the same comment block already names that record file, or when you have read the record and it holds the removed argument in its own words (pass it to `cleanup_check.py --cites`). Never guess which document an ID meant. | A colliding ID names 3 to 5 documents, and a wrong pointer that resolves passes LNK-02. Of 30 sampled removals, 22 were deletions, 6 rewrites and 2 pointers to a file the block already named. None needed a new pointer. | `cleanup_check.py --base "$(git merge-base origin/main HEAD)"` fails an added pointer to a file the block did not name, unless it is a `--relocation` or `--cites` record; it lists `--cites` records for review. | MUST |
| LNK-11 | A test name that lands on the default branch carries no plan-ID prefix. Strip `c050_s022_` and keep the behaviour sentence. Applies wherever a planning tool mints IDs into test names. | The ID outlives its plan and collides like LNK-01's. The plan archive already keeps the work-package-to-test map. The pattern appeared in exactly the repos built through one ID-minting planning tool (322 names in the largest), and in none of the others. | The test-name spot check below prints nothing. | SHOULD |
| LNK-14 | Before switching on the `ids` gate, run discovery and give every prefix it lists a verdict: add it to `families`, or record it in `non_ids` as a standard (`CWE-`, `UTF-`, `SHA-`) or a published rule ID (a lint code, a rule-set ID). Re-run until it exits 0. | Every repo mints its own families and the default list sees none of them. In one repo the list saw 846 lines and missed about 948 more in 11 local families: a line citing `SIM-024, D-006` was caught only for `D-006`. A green LNK-01 proves nothing until this has run. SHOULD because the 5-hit threshold is unvalidated. | `python3 checks/linkage_check.py ids --root . --discover` exits 0. It exits 1 while any prefix awaits a verdict. | SHOULD |

```sh
# LNK-11: must print nothing
git ls-files -z -- '*.rs' '*.py' | xargs -r -0 rg -n -e 'fn [cs][0-9]{2,4}_' -e 'def test_[cs][0-9]{2,4}_'
```

```rust
// wrong: which C-018? three plans define one
// Per C-018, hold the lock across the rename.

// right: the constraint and what breaks
// The lock is held across the rename; a reader in between sees no index file.
```

## Pointer Grammar

A pointer is `[repo:]path.md[#anchor]` and nothing else.

| Part | Form | Resolves when |
|---|---|---|
| `repo:` | The other repo's name and a colon. Omit it for the citing repo | The name is in the config's `repos` map, or equals the checkout directory's own name |
| `path.md` | A tracked path, or its unique trailing part, ending in `.md` | `git ls-files` of that repo holds it as a path or path suffix |
| `#anchor` | Lowercase. The shortest hyphen-boundary prefix of one heading slug or one `**ID**` definition | It prefixes exactly one heading slug, or exactly one bold ID holding a digit at a line start |

- **What counts as a pointer.** A `.md` name that starts with a record stem
  (`adr`, `plan`, `ruling`, `rulings`, `subsystem`, `decision`, `decisions`,
  `design_spec`, `handover`, then `_` or `-`), or a `.md` path under an `adr/`,
  `adrs/` or `decisions/` directory, or any `.md` path a `records` glob in the
  config matches. Name a record home outside those forms under `records`. An identifier without
  `.md` (`plan_for`) and a rustdoc link are never pointers.
- **Slugs** follow Python-Markdown's table of contents: lowercase, punctuation
  dropped, underscores kept, each run of hyphens and spaces collapsed to one
  hyphen. An explicit `{#id}` on a heading and an `<a id="...">` count as
  written.
- **A comment block is joined** before matching, so a pointer a formatter
  wrapped after `_` or `/` still reads whole. URLs are cut out first and
  belong to LNK-07.
- **A mapped clone that is absent** is skipped and counted as
  `absent_repo_pointers`. The weekly job resolves them with every clone present.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| LNK-02 | Every record pointer resolves: the file is tracked in the repo it names, which is the citing repo unless a `repo:` qualifier says otherwise, and an anchor, when present, prefixes exactly one heading slug or `**ID**` definition in that file. | Agents cite records under names that were renamed or never committed: 120 dead of 992 pointer mentions in one repo, and one renamed plan cited 27 times. About 24 of 30 dead mentions in smaller repos were live in a sibling repo and said nothing about which. | `pointers --check` (key `dead-pointer`). An ambiguous anchor prints `matches 2 headings`. | MUST |
| LNK-03 | Anchor a pointer into a record that holds more than one decision, with the shortest unique lowercase heading prefix: `adr_index.md#a2`. Never a full generated slug, never a line number (`adr_index.md:221`). Write the anchor lowercase: `#A2` reads as a short label under LNK-13. | The median decision record in one repo is 443 lines, so an unanchored pointer names no section. Headings carry dates and code paths that get reworded, which breaks a full slug, and any edit moves a line number. One record held two headings starting `C2`, which is why the prefix must be unique. | `pointers` prints `names a line number` and `#a2 is enough` as advisory findings. | SHOULD |
| LNK-07 | Write a cross-repo pointer as `repo:path.md#anchor`, never as a URL. A record URL that must stay is pinned to a commit SHA, never to `blob/main`. | A branch URL follows every later rename and rewrite of its target. About 10% of 9.6 million links in source comments are dead ([Hata et al., 2019](https://arxiv.org/abs/1901.07440)). | `pointers` flags a record URL whose `blob/` ref is not a SHA. External specs are out of scope. | SHOULD |

```text
wrong                                                     right
// see ADR A2 in the index design                         // see adr_index.md#a2
// see adr_index.md:221                                   // see adr_index.md#a2
// see adr_index.md#a2-the-index-is-the-source-of-truth   // see adr_index.md#a2
// see https://github.com/org/infra/blob/main/docs/adr/adr_tls.md
                                                          // see infra:docs/adr/adr_tls.md#c3
```

## Rendered Text

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| LNK-04 | Keep record pointers, record file names and process IDs out of any doc text that renders to a user surface: clap and schemars derives, click and typer commands, pydantic models, MCP tool descriptions, a published package's API reference. Move the reason to a plain comment in the same edit (SRF-02). | No generator filters what it copies, and the diff never shows the rendered side: a doc comment on a schema item becomes its `description`. One repo's published JSON Schemas carried 64 distinct internal IDs in 182 occurrences and 20 lines naming a record file. A file-qualified pointer leaks exactly like a bare ID. | Output side, the gate, whose token set holds every LNK-02 record form: `SCHEMAS=schemas; python3 checks/interface_leak.py "$SCHEMAS"` and `CLI=mytool; python3 checks/interface_leak.py --help-walk "$CLI"` exit 0. Source side, advisory: `python3 checks/interface_leak.py --source --root .` | MUST |

## Records Citing Code

A protected record is a decision record, a path-scoped agent rule or a root
agent instruction file that existed at the branch's merge base. Plans, specs,
reviews and research are point-in-time documents and stay as written. The
default globs are `.claude/artifacts/adr_*.md`, `.claude/rules/*.md`,
`.agents/adr/*.md`, `.agents/adrs/*.md`, `doc/adr/*.md`, `docs/adr/*.md`,
`docs/decisions/*.md`, `AGENTS.md` and `CLAUDE.md`. Add your client's rule
directory and your record home to `records`. When no file matches, `records`
says so on stderr: a clean result then means nothing was scanned.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| LNK-06 | A branch that deletes or renames a path repairs every citation of that path in the protected records. By the branch's end, each line naming the old path either no longer names it or also names the new path (LNK-08's subject case). Editing the record elsewhere does not count. | One crate split deleted or renamed 429 paths that 66 records cited on 375 lines. It repaired 25 lines in rules and left 350 lines in 58 records stale. A whole-file "record changed" test cleared a record that still cited 21 dead paths, because an unrelated 3-line edit touched it. Replayed over 300 commits, the per-citation gate fired 3 times, all real. | `records --base origin/main` exits 0. Replay one commit with `COMMIT=abc1234; python3 checks/linkage_check.py records --root . --base "$COMMIT^" --head "$COMMIT"`. A finding names the rename destination when git paired one, so the repair is a replacement; a deleted path needs a person. | MUST |
| LNK-08 | Repair a stale citation at the citation site. Where it says where code lives now, replace the path in place. Where the old path is the record's subject, add the new path beside it. Never edit decision text in a citation-repair commit, and never append a trailing "Citation updates" section. | A trailing note in a 443-line record sits hundreds of lines from the stale path, and stale paths sit inside headings. A citation is metadata, not decision substance. | Reading check on the word diff below: every `[-...-]{+...+}` pair is a path or symbol token, and no new trailing heading appears. | SHOULD |
| LNK-09 | When a record cites code that has a symbol, cite the path and a distinctive symbol: `src/net/reference.rs::parse_host_segment`. | A cited symbol survived two file moves where its path, with one rename traced, did not. A generic name fails the other way: `Identifier` matched 705 files. | The symbol spot check below prints exactly one definition. Zero means the symbol is gone. More than one means the record qualifies it with its module. | SHOULD |
| LNK-10 | Trace a missing cited path in three steps: find the commit that deleted it, read that commit's unfiltered rename map, and confirm the destination exists at HEAD. Never run `git log --follow` on the missing path. | `--follow` on a path that no longer exists prints nothing and exits 0, and a pathspec on `git show` turns an `R085` rename into a plain `D`: both read as "no rename". Destinations move again: 2 of 138 traced ones had. | The trace below prints the `R` line with the destination, and `git ls-files --error-unmatch` finds that destination at HEAD. | SHOULD |

```sh
# LNK-08: read every changed citation in the records, uncommitted edits included; add each configured records glob
git diff --word-diff=plain "$(git merge-base origin/main HEAD)" -- '*adr_*.md' '*/adr/*.md' '*/adrs/*.md' '*/decisions/*.md' '.claude/rules/*.md'
# LNK-09: exactly one definition
SYMBOL=parse_host_segment; git ls-files -z | xargs -r -0 rg -n -e "fn $SYMBOL\b" -e "def $SYMBOL\b" -e "class $SYMBOL\b" -e "struct $SYMBOL\b" -e "enum $SYMBOL\b" -e "func $SYMBOL\b" -e "function $SYMBOL\b"
# LNK-10: the deleting commit's rename map (R: destination, D: deleted,
# nothing: the path never existed as cited), then the destination at HEAD
OLD=src/legacy/client.rs; git log --diff-filter=D --pretty=%H -n 1 -- "$OLD" | xargs -r -I{} git show -M --name-status --format= {} | grep -F -e "$OLD"
NEW=src/oci/client.rs; git ls-files --error-unmatch "$NEW"
```

## The Weekly Job

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| LNK-12 | Schedule a weekly job for what no per-commit gate can see: record citations that name only a path fragment, cross-repo pointers resolved with every clone present, and URL liveness in comments and records. Report only the lines that are new since the last run. | `records` protects only citations that match a changed path exactly, and 305 of one repo's 1,106 record citations are fragments. URL checks need the network, which a per-commit gate must never depend on. | The three commands below, from a scheduler, each diffed against its previous report. | CONSIDER |

```sh
# fragment citations: the last two path segments, with no tracked match
git ls-files -z -- '*adr_*.md' '*/adr/*.md' '*/adrs/*.md' '*/decisions/*.md' '.claude/rules/*.md' | xargs -r -0 rg -o -N --no-filename -e '[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+\.(rs|py|ts|go|java|kt)\b' | sort -u | xargs -r -I{} sh -c 'git ls-files | grep -q -F -e "$1" || echo "unresolved: $1"' _ {}
# cross-repo pointers, every clone in the repos map checked out
python3 checks/linkage_check.py pointers --root .
# URL liveness; string-literal fixture URLs are noise the diff against last week absorbs
git ls-files -z -- '*.rs' '*.py' '*.ts' '*.go' '*.md' | xargs -r -0 lychee --no-progress --include-fragments
```

## What Agents Get Wrong Here

Ranked by how often each bites in agent-written code, measured 2026-09-27.

1. **Echoing plan vocabulary into code.** Implementing `C-018`, the agent
   writes `C-018` into the comment, because the plan is the most salient text in
   its context. Thousands of lines in one repo. Caught by LNK-01, LNK-11, LNK-13.
2. **Citing a record under a stale or never-committed name.** Nothing tells
   the agent a file it read earlier was renamed. Caught by LNK-02.
3. **Writing pointers and IDs into text that renders.** From the diff, a doc
   comment on a schema item looks like any other. Caught by LNK-04.
4. **Citing across repos without naming the repo.** Harness files copied
   between repos carry pointers that resolve only in the original. Caught by
   LNK-02 and LNK-07.
5. **Refactoring without touching the records that cite the moved paths.** Rare,
   but one such commit left 350 stale lines. Caught by LNK-06.
6. **Trusting a naive detector.** An extensionless regex flags `plan_for`, a
   recursive `rg` skips `.claude/`, and a "did the record change" test clears a
   record that still cites the dead path. Use the shipped checks.
7. **Trusting `git log --follow` on a deleted path, or stopping at the first
   rename hop.** The empty answer reads as clean. Caught by LNK-10.
8. **Minting a repo-local ID family the default list cannot see.** Caught by
   LNK-14, once, at adoption and whenever discovery lists a new prefix.
9. **Inventing a plausible pointer during cleanup,** because citing a source
   looks thorough. A colliding ID makes the guess wrong, and LNK-02 passes it.
   Caught by LNK-05.
