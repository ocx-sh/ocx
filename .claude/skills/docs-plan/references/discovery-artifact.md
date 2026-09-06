# The discovery artifact

Read this when writing or updating the artifact file. It holds the field
schema, the coverage table, the delete list, the file location, and the rule
rows this procedure is graded against.

Contents: [Where the file lives](#where-the-file-lives) ·
[Header schema](#header-schema) · [Task row schema](#task-row-schema) ·
[Coverage table](#coverage-table) · [Delete list](#delete-list) ·
[The rules](#the-rules)

## Where the file lives

Default: `docs/discovery/use-cases.yaml`, with friction logs beside it under
`docs/discovery/friction-logs/`. This default is pinned. An adopter may
override it once, in one place, by naming a different path in the docs config.
Every rule row below then reads that path.

Pick `.agents/discovery/` instead when the discovery output must stay off the
published documentation surface. The page-declaration check excludes `.agents`
by design, so an artifact there is never mistaken for a documentation page.

One file. Not a chat transcript, not a set of scattered notes. The point of a
file is that the next run can diff it.

## Header schema

```yaml
product_shape: cli          # cli | library | hosted-service | framework
signal: friction-log-severity
generated: 2026-09-05
surface_sources:            # where candidates came from, never docs page titles
  - cli-subcommand
  - issue-title
  - changelog
out_of_scope:               # enumerated surface entries deliberately not tasks
  - id: X01
    surface: "completions"
    why: "shell integration, not a reader task"
```

`product_shape` selects the first-steps thresholds. `signal` must hold one of
the four literal values, and it is the honest answer, not the best-sounding one.

## Task row schema

One row per shortlisted task. Every field is required unless marked optional.

```yaml
tasks:
  - id: T07
    task: "install the CLI on a machine with no prior toolchain"
    source: cli-subcommand    # cli-subcommand | entry-point | readme-heading |
                              # issue-title | changelog | search-log
    signal: friction-log-severity
    rank: 1
    tier: first-steps         # first-steps | everyday | integration
    depends_on: []            # must be empty on a first-steps task
    friction_log: docs/discovery/friction-logs/T07.md
    user_need:
      as_a: "a developer with no prior install of this tool"
      i_need_to: "get a working copy onto my machine"
      so_that: "I can follow the rest of the documentation"
    solution_shaped: false    # result of the check in user-needs.md
    existing_page: docs/installation.md   # or the literal: missing
    page_word_count: 640
    page_status: adequate     # missing | stub | adequate | duplicate
    action: keep              # write | expand | merge | keep | delete
```

`page_word_count` is prose words after stripping code, front matter, tables and
link targets. The `docs-quality` rule set ships `checks/strip_prose.py` for
exactly this, so use it rather than counting raw bytes.

## Coverage table

The coverage table is the same rows rendered for a human reviewer. Three
columns are enough: task, mapped page or `missing`, and verdict.

| Task | Page | Verdict |
|---|---|---|
| T07 install the CLI | docs/installation.md | keep |
| T11 pin a version in CI | missing | write |
| T02 inspect a lockfile | docs/reference/lock.md | expand |

Unmapped pages get their own short list under the table. An unmapped page is
not automatically deletable. It is a candidate for the delete test only.

## Delete list

Both signals must hold on the same page.

1. No surviving user need maps to it.
2. It is a stub or an exact duplicate of another covered page.

A stub is a page under 150 prose words. That cut comes from the corpus audit
behind this rule set, which measured a 24.6% stub share across 248 pages at
that threshold.

Two exemptions run before the word count.

- A page whose body is mostly a build-time generator directive is never a stub.
  Look for `.. auto(class|module|function)::`, a `:::` mkdocstrings line, or the
  generator's equivalent. Deleting one destroys the pointer to a whole API
  surface.
- A page that reaches a verified result is never a stub. A 20-word page that
  installs the tool can be the best page in the tree. Detect it by the
  `# doc:` success marker owned by the tested-example rules.

Each delete row states both signals in the artifact. A row with one signal is
not a delete, it is an `expand` or a `merge`.

## The rules

These rows govern the procedure and its artifact. They are the shipped
definitions.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| DOC-DISC-01 | Never source a candidate task from an existing docs page title or heading. | The docs tree invents a need that only justifies the page already written. | `grep -rhoE --include='*.md' '^#+ .*' docs > titles.txt` then compare against the artifact's `task` values. A task whose only source is an existing title fails. | MUST |
| DOC-DISC-02 | Write exactly one user need per shortlisted task, in the form "As a X, I need to Y, so that Z". | A task with no need sentence cannot be tested for solution shape later. | `python3 -c "import sys,yaml;d=yaml.safe_load(open(sys.argv[1]));sys.exit(any(not all(t['user_need'].get(k) for k in ('as_a','i_need_to','so_that')) for t in d['tasks']))" <artifact>` | MUST |
| DOC-DISC-03 | Reject a user need whose need or outcome clause names a page, command or flag. | The agent paraphrases the target page back at itself instead of reasoning about the task. | Build a token file from docs headings and CLI subcommand and flag names, drop every token under 4 characters, keep only phrases of 2 words or more, then `grep -oiFf tokens.txt needs.txt`. Any hit fails. The first token construction measured a 100% false-positive rate on 5 legitimate needs (calibration run A), so treat a hit as a prompt to re-read, not as a verdict. | SHOULD |
| DOC-DISC-04 | Structure every friction log as Context, Pros and cons, and Detailed stream of consciousness, with no proposed-fix section. | A fix named during discovery locks in a solution before the task is understood. | `grep -c '^## ' <log>` returns 3, and `grep -ni -e '^## .*solution' -e '^## .*proposed fix' -e '^## .*recommendation' <log>` returns nothing. | SHOULD |
| DOC-DISC-05 | Include verbatim output from a real run in the stream-of-consciousness section. | The agent narrates what a user would probably see instead of running the command. | `grep -nE '^[$`]' <log>` must print at least one line, which is a shell prompt or a fence. | MUST |
| DOC-DISC-06 | Name a concrete first-time persona in the Context section. | An agent grading its own work as an unnamed expert already knows the answer it claims to discover. | `grep -ni -e 'has never' -e 'first time' <log>` must match, and `grep -ni 'familiar with' <log>` must not. | SHOULD |
| DOC-DISC-07 | Rank tasks only by one of four named signals, and record which one was used. The four are `issue-pr-frequency`, `invocation-telemetry`, `zero-result-logs` and `friction-log-severity`, in that priority order. | The fabricated percentage or vote count is the most common way this procedure gets faked. | `grep -c -e '^signal: issue-pr-frequency' -e '^signal: invocation-telemetry' -e '^signal: zero-result-logs' -e '^signal: friction-log-severity' <artifact>` returns 1, and `grep -nEi -e '[0-9] ?%' -e '[0-9]+ votes' -e '[0-9]+ respondents' <artifact>` returns nothing. | MUST |
| DOC-DISC-08 | List every top-level subcommand or exported entry point as a candidate task, even one triaged out. | A zero-traffic project has no sampling frame, so surface completeness is the only honest substitute for a vote. | Diff the tool's `--help` subcommand list, or the package's exported names, against the artifact's `tasks` plus its `out_of_scope` list. Every surface entry appears in one of the two. | SHOULD, pinned |
| DOC-DISC-09 | Put a page on the delete list only when no user need maps to it and it is a stub or a duplicate. | One weak signal is not enough licence to delete, because the canonical deletion case had real traffic data this procedure does not. | Intersect the coverage table's unmapped rows with `python3 checks/strip_prose.py <page> > prose.txt && wc -w prose.txt` under 150 (corpus audit stub cut, 24.6% of 248 pages). Both must hold. | SHOULD |
| DOC-DISC-10 | Exempt from the stub half of the delete test any page that reaches a verified result, and any page whose body is mostly a build-time generator directive. | A 20-word page that installs the tool is not a stub, and a 4-line autodoc directive renders a full API surface. | Before the word count, `grep -n -e '^::: ' -e '^.. auto' <page>`. A hit exempts the page. Otherwise skip any row whose page carries the `# doc:` success marker. | SHOULD |
| DOC-DISC-11 | Write the discovery result to one durable artifact file with the fixed schema. | A result that exists only in a transcript cannot be diffed on the next run. | `test -f docs/discovery/use-cases.yaml` and the file parses with one entry per shortlisted task. | MUST |
| DOC-DISC-12 | Re-run discovery on every feature merge, and re-run the grep-only coverage audit on every tagged release. | There is no traffic curve to watch, so the trigger has to be a code change or a release boundary. | A CI job on the merge event and a job on the release tag regenerate the coverage columns and diff the stub and orphan counts against the prior run. | CONSIDER |

`DOC-DISC-13` to `DOC-DISC-25` are defined in the `docs-quality` rule set, under
its page-types depth file. This procedure consumes them. It does not redefine
them.
