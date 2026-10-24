# Decision list

You loaded this file at steps 3 and 4 of the cleanup: you are about to rewrite
one comment block and need to decide, clause by clause, what stays, what goes
and in what form.

Contents: [0. The unit of work](#0-the-unit-of-work) ·
[1. Split the block into clauses and tag each](#1-split-the-block-into-clauses-and-tag-each) ·
[2. Write each guard as fact plus consequence](#2-write-each-guard-as-fact-plus-consequence) ·
[3. Pick the register](#3-pick-the-register) · [4. Length caps](#4-length-caps) ·
[5. Output](#5-output) · [Worked pairs](#worked-pairs)

## 0. The unit of work

You rewrite ONE comment block. Every line outside the block stays
byte-identical: code, tool directives, licence headers, and fenced code inside
doc comments. The replacement goes in place, at the same position with the same
indentation.

A block is the run of consecutive comment lines of one kind (doc or plain),
with only blank lines between them. A blank line does not end a block.

## 1. Split the block into clauses and tag each

Tag every clause, in this order: first check whether it is a guard, then work
down the table. A block of one sentence is one clause.

| Tag | Test | Action | Rule |
|---|---|---|---|
| guard | Names a constraint whose breach a plausible edit would cause, with or without the consequence | Keep (section 2) | GRD-01 |
| label | `SAFETY:` on an unsafe block, or the repo's deferred-shortcut marker (for example `ponytail:`) | Keep the label. The text after it follows the guard rules | GRD-06 |
| mandated | A comment another rule requires: a discarded-result rationale, a lint-suppression reason, "a comment naming X" | Keep. The text follows the guard rules | GRD-06 |
| contract | What a caller must know without opening the body: behaviour, errors, panics, preconditions, units | Keep, 1-3 lines | GRD-04 |
| why | A non-obvious local reason that is not a guard (why this approach, why this value) | Keep, one sentence | RTE-01 |
| pointer | Names a tracked file, a symbol, an RFC or spec section, an issue URL | Keep if it resolves. Record moved: repoint it. Dead and untraceable: leave it and report it. Commit, branch or untracked path: drop the pointer and state the constraint it stood for | GRD-11, LNK-10, RTE-02 |
| deferred shortcut | Names a deliberate shortcut, its ceiling and its upgrade path | Keep | RTE-01 |
| provenance, live | Past tense that justifies a value still in the code, with a count or a named artifact | Keep, at most 2 lines beside the value | RTE-11 |
| provenance | Past tense, no present obligation, no number | Drop. If it guards against a regression, rewrite it as a present-tense guard | RTE-11 |
| record paraphrase | Restates a tracked record the block points at | Drop. Keep the pointer and any clause the record does not hold | RTE-09 |
| argument | Weighs alternatives, tells the design story, restates a decision record | Drop. If a tracked record already holds it, keep one pointer to that record and declare it with `--cites`. If none does and it argues a decision someone could reopen, append it to a record first. If the choice is small, keep its conclusion as one why sentence when the code does not show it | RTE-10 |
| universal negative | A property no single line implements ("nothing is ever reserved") | Keep. It belongs on the doc comment of the test that asserts it, which this diff may not touch: report it | RTE-04 |
| subsystem invariant | True verbatim at every file one path-scoped rule matches | Keep. Report it as a rule candidate | RTE-07 |
| narration | The next code line already says it | Drop, unless a clause names what the step guards; then it is a guard | CLN-01 |
| tautology | The item's name and type already say it | Drop | CLN-01 |
| process ID | A bare plan, contract, work-package, review-round or decision ID with no file | Drop the ID; keep the constraint it labelled | LNK-01, LNK-05 |
| banner | Section markers, decorations | Drop, unless the repo's own rules protect it | CLN-01 |
| commented-out code | Code syntax in a plain comment | Leave it. Report it for its own change | CLN-01 |
| stale | The clause states something the code now contradicts | Keep verbatim. Report it for a human; never delete or correct it in the cleanup | CLN-01 |

- A clause that fits no row stays. When unsure between guard and anything else,
  it is a guard.
- Delete a whole block only when every clause in it is narration, tautology, a
  banner, a bare ID or pure provenance (CLN-01).
- Positive identification means passing the row's test, read against the code:
  - **narration:** the next code line already says it;
  - **tautology:** the name already says it;
  - **pure provenance:** past tense, with no present obligation and no number.
- Nothing else identifies a clause. Length does not, a history phrase does not
  ("used to", "previously"), and a `Step N:` opener does not. A phase marker such
  as "Step 4: one lock over the whole body" is the only record of a lock hazard
  when nothing else names it.
- The recogniser's flag is a floor, not a verdict. A flagged block is never
  deleted whole. A block it misses is unclassified and still goes through this
  table (GRD-06).

## 2. Write each guard as fact plus consequence

- State the constraint and the concrete consequence of breaking it: "X must
  hold, or Y happens." Name the tempting edit only when the constraint does not
  imply it, and name the sibling it must not be unified with when that is the
  risk.
- Present tense. A fact, never a command to the reader: "do not" and "never
  touch" become "X is required because Y" (GRD-05).
- One sentence per breaking edit. Never merge several guards into one vague
  line. Every edit the original forbade keeps its own sentence (CLN-02).
- A guard shrinks to one line naming its owners only when every breaking edit
  it names has one: a test, lint or type that a planted-edit run shows failing
  on that edit (CLN-07, step 5). A test counts whether or not the block names
  it. Name every owner that failed and none that stayed green.
- No marker prefix. `SAFETY:` stays where the language uses it.

Additions the cleanup applies on top:

- **Consequence test (GRD-01).** For each guard, point at the words that name
  what breaks: a hang, a leak, a torn write, a wrong exit code, an escape.
  "Important", "careful" or "load-bearing" alone is not a consequence.
- **Never invent one.** If the original states no consequence and the code does
  not show one, keep the original wording and list the block for review.
- **At its line (CLN-02, LEN-05).** Each guard sits at the line its breaking
  edit would touch. Delete a doc copy of a guard that already sits at its line.
- **Once (GRD-03).** State a guard at the type or at the binding, not both.
- **Three lines is a signal (GRD-02).** A rewritten guard longer than 3 lines is
  usually a bundle or an essay. Split it again. Most guards fit in 1 or 2 lines.
- **History that still constrains (RTE-11).** "This blinked out on the second
  paint" becomes "Rendering twice clears the panel, so the second paint must
  reuse the first node."
- **Pointers ride along (GRD-11).** A file-qualified pointer in the original
  stays in the rewrite unless the same target appears elsewhere in the diff.

## 3. Pick the register

- **Doc comment** (`///`, docstring, `/** */`): only what a caller who never
  opens the body needs.
- **Plain comment** (`//`, `#`): implementation hazards. When a doc block holds
  an implementation hazard, the rewrite may turn it into a plain comment at the
  same position, or move it to the line it guards (GRD-04).
- Rewrite, never copy, when you move a clause within a file. The check fails
  prose the recogniser does not flag that reappears 80% or more intact
  elsewhere in the same file (CLN-04).

## 4. Length caps

- **Plain comment block:** at most 5 lines (LEN-01).
- **Doc comment block:** at most 10 lines. A public item in a library-kind
  package may run to 15 (LEN-02).
- These are the rule's defaults. The census reads caps and the list of library
  packages from `.code-docs.json`, where an adopter may change them.
- Doc lines inside sections that restate the signature or give examples do not
  count:
  - Rust `# Errors`, `# Panics`, `# Examples`, `# Aborts`, and fenced code;
  - Python `Args:`, `Returns:`, `Raises:`, `Yields:`, `Attributes:` and the reST
    `:param:` family;
  - JSDoc `@param`, `@returns`, `@throws`, `@example`;
  - Javadoc `@param`, `@return`, `@throws`, `@see`.

  `# Safety`, `Note:` and `WARNING` text always count (LEN-07).
- If the guard clauses alone exceed the cap, keep them all; tighten wording,
  never drop a guard. A guard earns no exemption either: split it to its lines
  (LEN-05).
- No ratio target exists. The caps bind whatever the density of the file around
  the block (LEN-04). Deleting a guard that is already within the cap earns
  nothing.

## 5. Output

- Return the replacement lines exactly as they will appear in the file, with
  the original indentation and comment syntax, or an empty list when every
  clause is dropped.
- A Python docstring that is its function's only statement never becomes
  empty. Keep its one most useful line, or the file stops parsing (CLN-03).

## Worked pairs

A guard bundle collapsed into one line. The second and third edits it forbade
are gone, and so is every consequence:

```rust
// Shared resolver for offline and platform handling; see the design notes.
let target = resolve(&request, &policy)?;
```

The same bundle split. One sentence per breaking edit, each with its
consequence, at its own line:

```rust
// The transport path skips the local-physical check, so resolving through it breaks --offline.
let target = resolve(&request, &policy)?;
// Narrowing by --platform before membership is checked refuses members that match.
let members = members_of(&target, &policy)?;
```

An instruction with no consequence. The next agent cannot re-judge it when the
code changes:

```python
# NEVER swallow this error.
return fsync_parent(parent)
```

The same guard as a fact plus its consequence:

```python
# A failed parent-directory fsync means the rename may not survive a crash, so
# swallowing it reports a publish that can vanish.
return fsync_parent(parent)
```
