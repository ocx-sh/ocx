# Failure modes

You loaded this file before a sweep, or because a check failed and the cause is
not obvious. The ranking is by how often each failure shows up in agent-written
cleanups, most frequent first. Each row names the tell to look for in your own
diff and the step that catches it.

Contents: [Ranked](#ranked) ·
[1. Guards merged into one line](#1-guards-merged-into-one-line) ·
[2. A guard deleted inside an essay](#2-a-guard-deleted-inside-an-essay) ·
[3. History rewritten away](#3-history-rewritten-away) ·
[4. An owner credited by its name](#4-an-owner-credited-by-its-name) ·
[5. The pointer beside the essay](#5-the-pointer-beside-the-essay) ·
[6. Edits outside prose](#6-edits-outside-prose) ·
[Check findings decoded](#check-findings-decoded)

## Ranked

| Rank | Failure | Tell in your diff | Caught by | Rule |
|---|---|---|---|---|
| 1 | Several guards merged into one line, the consequence clause dropped first | The worklist lists 3 breaking edits for a block; the rewrite has one sentence | Reading the worklist against the rewrite; the `CLN-02 review` item; one led turn per edit in the re-check | CLN-02, GRD-03 |
| 2 | A guard deleted inside an essay, because the block was long | A whole block removed, with no clause of it anywhere in the diff | The carve-out, only when the recogniser fires (under half the time); otherwise the clause split and the re-check | CLN-01, RTE-01 |
| 3 | History rewritten away: a guard phrased in the past tense deleted as provenance | A removed clause with a past-tense verb and a named failure ("hung", "blinked out", "lost the write") | The provenance test: past tense, no present obligation and no number. A named failure is a present obligation | CLN-01, RTE-11 |
| 4 | An owner credited by its name | The pointer names a test no planted edit was run against | The planted-edit run in step 5 | CLN-07, GRD-07 |
| 5 | The pointer added beside the essay it should replace | A new `record.md` line above an unchanged paragraph | `cleanup_check.py` when the record is declared; otherwise a block that names a record and restates 2 or more of its sentences | CLN-04, RTE-09 |
| 6 | Success reported from the agent's own reading | "All guards preserved" with no check output and no probe answers | The report demands both | CLN-06 |
| 7 | Rendered help or schema text edited | A changed doc line on an item that derives a CLI parser or a schema, or a changed golden file | `cleanup_check.py` freezes interface lines and refuses golden paths | CLN-03, CLN-05 |
| 8 | A tool directive deleted | A removed `@ts-expect-error`, `# noqa`, `# type: ignore` or `eslint-disable` line | `cleanup_check.py` freezes directives; the repo's type checker and linter | CLN-03 |
| 9 | A doctest deleted or disabled | Removed or re-tagged fenced code inside a doc comment | `cleanup_check.py` freezes fenced lines; the repo's doctest run | CLN-03 |
| 10 | A build error read as the owner failing | The owner "failed" with an unused-import or unused-binding error, before any test ran | The outcome table in step 5 | CLN-07 |
| 11 | A sweep file collides with another branch | A merge conflict on a comment-only diff | The in-flight check in Scope, before starting and before merge | CLN-08 |
| 12 | A guard rewritten as an order to the reader | "NEVER …" or "Do not …" with no consequence in the sentence | Reading against the decision list, section 2 | GRD-05 |

## 1. Guards merged into one line

A long guard block usually bundles 2 to 5 separate guards, each forbidding a
different edit. The summary line keeps the first and loses the rest, and the
consequence clause goes before the mechanism does.

- **Tell:** count the breaking edits in your worklist for the block. Count the
  sentences in the rewrite that each name one of them. The numbers differ.
- **Catch:** the check lists a `CLN-02 review` item when fewer sentences fire
  the recogniser after the edit. It never fails the diff, because a regex also
  misses good rewrites, so read every item. The re-check asks one led question
  per breaking edit, and a lost edit scores Generic or Wrong.
- **Fix:** one sentence per edit, at that edit's line (decision list,
  section 2).

## 2. A guard deleted inside an essay

"Long means essay" is the most expensive shortcut in this procedure. Essays in
agent-written code almost always wrap a real guard in argument.

Wrong: the whole block goes, and the one sentence that guarded the call goes
with it.

```rust
let target = resolve(&request, &policy)?;
```

Right: the argument moves to the record, the guard stays at its line, and one
pointer replaces the rest.

```rust
// Design and rejected alternatives: docs/adr/0012-resolution.md#transport
// The transport path skips the local-physical check, so resolving through it breaks --offline.
let target = resolve(&request, &policy)?;
```

- **Catch:** the carve-out blocks a whole-block removal only when the recogniser
  fired on it, which is under half of real guards. The clause split in step 3 is
  the real catch. The carve-out is the backstop.

## 3. History rewritten away

A comment such as "this blinked out on the second paint" reads like history, but
it is the only record of a regression the line prevents. About a third of
comments that match a history phrase are live guards.

- **Tell:** a past-tense clause that names a failure, deleted as provenance.
- **Fix:** rewrite it as a present-tense guard: "Rendering twice clears the
  panel, so the second paint reuses the first node." Delete provenance only when
  it has no present obligation and no number (RTE-11).
- A grep for "used to", "previously" or "historically" finds candidates to read,
  never a delete list.

## 4. An owner credited by its name

A test named after the constraint is not proof that it guards anything. When
the breaking edit is applied, some named tests stay green because they never
build the guarded scenario. Where two tests are named, often only one fails.

- **Tell:** the pointer line names a test, and your notes have no planted-edit
  run for it.
- **Fix:** run the planted edit (step 5). Name only the tests that failed. If
  none failed, the guard keeps its mechanism, edit and consequence.

## 5. The pointer beside the essay

The agent adds `docs/adr/…md` above the paragraph and keeps the paragraph.
Nothing got shorter, and the next reader now has two copies that drift.

- **Tell:** a pointer line added with no removed lines below it.
- **Catch:** with the record declared through `--relocation`, the check fails
  when text appended to the record still lives in the source. Without a
  declared record, read the block: one that names a record and restates 2 or
  more of its sentences is the failure (RTE-09).

## 6. Edits outside prose

Under a goal of "shorter", agents tidy what they should not touch: a help
string, a `@ts-expect-error`, a doctest, a test. An instruction not to does
little under pressure. The structural check is what stops it.

| Edit | Why it matters | Caught by |
|---|---|---|
| Rendered help, schema or tool description text | Changes the user contract and drifts every golden | `cleanup_check.py` (interface lines, golden paths) |
| A directive line | Changes what the type checker or linter reports | `cleanup_check.py` (directive lines), then the repo's gate |
| Fenced code in a doc comment | Deletes or disables a doctest | `cleanup_check.py` (fenced lines), then the doctest run |
| A test file or `#[cfg(test)]` region | Weakens the check that owns a guard | `cleanup_check.py` (test paths and regions) |

## Check findings decoded

What a blocking finding from `cleanup_check.py` usually means, and the fix.

| Finding text starts with | Usual cause | Fix | Rule |
|---|---|---|---|
| `guard block removed whole` | A guard deleted inside an essay, or deleted as narration | Restore the block, then split it clause by clause | CLN-01 |
| `code line changed`, or another kind: `interface`, `directive`, `license` | A frozen line edited, often by an editor reformatting the file | Restore the line byte for byte | CLN-03 |
| `the file no longer parses` | A Python docstring that was its function's only statement was deleted | Restore one line of it | CLN-03 |
| `a cleanup never adds` or `never deletes a source file` | Prose moved into a new file, or a file dropped | Revert; prose moves only to a declared record | CLN-03 |
| `... reappears here` | A clause moved verbatim within the file, such as a guard moved to its line | Rewrite the clause as you move it (decision list, section 3) | CLN-04 |
| `did not land there`, or `contains none of the prose` | The record received a summary, not the text | Append the removed argument itself | CLN-04 |
| `still lives here` | The pointer was added and the essay kept | Delete the paraphrase; keep the pointer | CLN-04 |
| `a cleanup never touches` a test, lock, manifest, generated or golden file, or `a line inside an in-file test region` | An incidental edit | Revert that path or region | CLN-05 |
| `declare the record prose moved to` | A record changed but was not declared, an unrelated file changed, or an untracked file sits in the tree | Pass `--relocation` for the record, or revert a file the cleanup changed. An untracked file the cleanup did not create is the owner's: never delete it. Run from a clean worktree, or ask the owner to move it | CLN-05 |
| `a relocation record is append-only` | An existing record was edited instead of appended to | Restore it and append only | CLN-05 |
