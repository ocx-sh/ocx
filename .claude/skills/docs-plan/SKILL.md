---
name: docs-plan
description: Documentation discovery and information architecture for a repository, in three tiers (first steps, everyday, integration), with user needs, a page inventory, a coverage table, a delete list, and seeded doc_type and doc_tier declarations. Use when someone asks what documentation a project needs, wants a docs plan, docs audit, docs roadmap, docs IA, page inventory, coverage table, delete list, or content gap analysis, when scoping a quickstart, getting-started page, first-steps page or tutorial, when asked which pages to write, merge, split or delete, when someone mentions user needs, top tasks, friction logs, doc_type, doc_tier, product shape, or "what should the docs cover". Not for wording one page. The docs-quality rules cover prose, examples and navigation.
license: Apache-2.0
metadata:
  summary: Discovery procedure that turns a repository's own evidence into tiered use cases, a typed page inventory, and an IA plan in one durable artifact
  keywords: documentation,docs-plan,information-architecture,use-cases,user-needs,friction-log,quickstart,first-steps,doc-type,doc-tier,content-audit,coverage,delete-list,tiers,diataxis
---

# docs-plan

Discover what a project's documentation must cover, from the project's own
evidence, and write the result to one durable artifact.

The artifact holds a ranked task list in three tiers and one user need per task.
It also holds a typed inventory of every page, a coverage map, a delete list and
an IA plan. Nothing here writes documentation prose.

Use this skill before writing or restructuring documentation. Use the
`docs-quality` rules while writing individual pages.

## Stop condition

Stop when the artifact file exists on disk and all of the following hold.

- Every top-level subcommand or exported entry point appears either in the task
  list or in the explicit out-of-scope list.
- Every shortlisted task carries a user need, a tier, a ranking signal, a mapped
  page or the literal `missing`, and an action.
- Every existing page carries a `doc_type` value, proposed or already declared.
- The delete list contains only pages that failed both delete signals.

Then stop. Do not write the pages. Do not apply the delete list without the
maintainer saying so, because deleting documentation unattended is a different
risk class from writing a page.

## The evidence rule

Every claim in the artifact comes from the repository, its issue tracker, its
logs, or a run you actually performed. Never from an existing docs page title,
and never from a number you did not measure. Those two shortcuts are the way
this procedure gets faked, and both have their own rule.

## Procedure

Run the steps in order. Each names the output it must leave behind.

### 1. Declare the product shape

Read the docs config for one of `cli`, `library`, `hosted-service` or
`framework`. Look in the `mkdocs.yml` extra table, a `pyproject.toml` tool
table, or a `docs.toml`. If none is declared, infer one from the repository and
write it into the config as part of this run.

Output: one shape value, recorded in the artifact header. Every first-steps
threshold later branches on it. See
[references/tiers-and-types.md](references/tiers-and-types.md).

### 2. Enumerate the surface

List every top-level subcommand, every exported entry point and every README
section heading. Add every issue and pull request title, and every changelog
entry describing a shipped feature. Add search logs and invocation telemetry
when the project has them, which is rare.

Never source a candidate from an existing docs page title or heading. That
invents a need to justify a page that already exists.

Output: the candidate longlist, with a `source` value per candidate.

### 3. Collapse and shortlist

Merge duplicates. Move anything out of scope into a named out-of-scope list
rather than deleting it. The out-of-scope list is what makes the longlist
auditable later.

Output: the shortlist, plus the out-of-scope list.

### 4. Run a friction log on the top candidates

Attempt each shortlisted task for real, as a named first-time persona, and write
what happened while it happened. Three sections, no fix section. Paste verbatim
output from the run.

An agent that already has the repository in context cannot un-know it. Delegate
the log to a subagent with no repository access wherever the harness allows it.

Output: one friction log file per task attempted. Template in
[references/friction-log.md](references/friction-log.md).

### 5. Rank by one named signal

Pick the strongest signal that actually exists, from `issue-pr-frequency`,
`invocation-telemetry`, `zero-result-logs`, `friction-log-severity`, in that
priority order. Record which one you used.

Never invent a percentage, a vote count or a respondent count. Most projects
land on `friction-log-severity` and must say so.

Output: a ranked shortlist and one `signal` value.

### 6. Write one user need per task

Use the fixed form: "As a X, I need to Y, so that Z". Reject any need whose need
or outcome clause names a page, a command or a flag. Reject "understand", "know"
and "be aware of" unless a concrete action follows.

Output: `as_a`, `i_need_to` and `so_that` fields per task. Form, worked pair and
the rejection check in [references/user-needs.md](references/user-needs.md).

### 7. Inventory and type every existing page

Walk the published documentation tree. For each page record the path, the prose
word count, and a `doc_type` from the nine-value enum. Seed the type from the
generator nav config where one exists, which is the strongest available seed.
Fall back to a content read.

Never derive `doc_tier` from nav position. That was measured and does not work.

Output: the inventory table, one row per page. Enum and seeding in
[references/ia-plan.md](references/ia-plan.md).

### 8. Map needs to pages

For each task, name the page that serves it or the literal `missing`. For each
page, mark it as serving a need or as unmapped. Set `page_status` to `missing`,
`stub`, `adequate` or `duplicate`, and `action` to `write`, `expand`, `merge`,
`keep` or `delete`.

Output: the coverage table.

### 9. Assign a tier to every task

Three values only: `first-steps`, `everyday`, `integration`. First-steps
membership is decided by dependency order, not by rank. The most painful task is
not the entry task.

Output: a `tier` per task, and an empty `depends_on` list on every first-steps
task. Tier contract per product shape in
[references/tiers-and-types.md](references/tiers-and-types.md).

### 10. Build the delete list

A page goes on the delete list only when both signals agree. It maps to no
surviving user need, and it is a stub or an exact duplicate. Before the word
count, exempt any page that is mostly a build-time generator directive, and any
page that reaches a verified result.

Output: the delete list, with the two signals stated per row.

### 11. Write the IA plan

Name the target tree, the nav groups, and the page each action produces. Put the
first-steps entry point in a different top-level nav group from the everyday hub.
Nav shape per generator in [references/ia-plan.md](references/ia-plan.md).

Output: the IA plan section.

### 12. Seed the declaration

Propose a `doc_type` comment line for every page, and a `doc_tier` line for
pages typed `tutorial`, `how-to` or `landing` only. The carrier is a comment,
never YAML frontmatter, and never above existing frontmatter.

Output: the seeded declaration lines, as a reviewable diff or patch.

### 13. Write the artifact

One file, fixed schema, one row per shortlisted task. A result that lives only
in a transcript cannot be diffed on the next run.

Output: the discovery artifact. Schema in
[references/discovery-artifact.md](references/discovery-artifact.md).

## Checks to run

Run these before declaring the run finished. The first three come from the
`docs-quality` rule set, which ships them under its own `checks/` directory.

```sh
# Propose a doc_type per page from nav config or heading, for step 12.
python3 checks/doc_declaration.py --seed --root docs

# Confirm the seeded declarations parse and sit below any front matter.
python3 checks/doc_declaration.py --root docs

# Confirm the planned nav depth and grouping in step 11.
python3 checks/nav_depth.py --root .

# Every task row carries the three need clauses and a tier.
grep -cE "^  i_need_to:|^  tier:" <artifact>

# No fabricated vote or percentage anywhere in the artifact.
grep -nEi "[0-9]+ ?%|[0-9]+ (votes|respondents|users surveyed)" <artifact>

# Every friction log has verbatim output.
grep -nE '^[$`]' docs/discovery/friction-logs/*.md

# No friction log proposes a fix.
grep -niE "^## .*(solution|proposed fix|recommendation)" docs/discovery/friction-logs/*.md
```

The last three must print nothing, except the verbatim-output grep, which must
print at least one line per log.

## Failure modes

These are the ways an agent gets this wrong, most frequent first.

1. **Fabricates a vote count or a percentage.** "73% of users need X" with no
   survey behind it. Step 5 forbids it, and the grep above catches it.
2. **Narrates a friction log instead of running one.** Plausible prose about
   what a user would probably feel, with nothing executed. Verbatim output is
   the only defence.
3. **Writes the user need by paraphrasing the target page.** The need names a
   command, a flag or a page title. It justifies content instead of describing
   a task.
4. **Sources candidate tasks from the docs tree.** The task list then mirrors
   the pages that already exist and discovers nothing.
5. **Collapses tier and type into one field.** A single value such as
   `getting-started` is easier to generate than two orthogonal keys.
6. **Writes the declaration as YAML frontmatter.** Frontmatter dominates
   training data. On mdBook it renders as a fake heading that enters the search
   index with its own anchor.
7. **Puts the declaration comment above existing frontmatter.** Reading "first
   line" literally destroys the frontmatter the page already had.
8. **Imports the CLI framing onto a library.** A library first-steps page built
   around a shell command instead of a printed value.
9. **Flags a generated-reference stub for deletion.** A four-line autodoc
   directive reads as a stub by word count and renders a whole API surface.
10. **Ranks by pain and calls the result tier one.** Dependency order decides
    first steps.
11. **Labels any onboarding page a tutorial.** A tutorial is required only when
    the reader must assemble two or more interacting concepts.
12. **Never deletes, only adds.** Every row needs an explicit action value, and
    `delete` has to be reachable.
13. **Leaks the template into a shipped page.** The literal strings "As a" and
    "so that" reach rendered documentation. Keep them inside the artifact.

## References

Read one level down, on demand. These files do not link each other.

| File | Read it when |
|---|---|
| [references/discovery-artifact.md](references/discovery-artifact.md) | Writing or updating the artifact file. Holds the field schema, the coverage table shape, the file location, and the rule rows this procedure is graded against |
| [references/friction-log.md](references/friction-log.md) | Running step 4. Holds the three-section template, the persona contract, and what disqualifies a log |
| [references/user-needs.md](references/user-needs.md) | Running step 6. Holds the form, a good and bad worked pair, the banned verbs, and the solution-shape rejection check |
| [references/tiers-and-types.md](references/tiers-and-types.md) | Running steps 1 and 9, or scoping a first-steps page. Holds the tier model per product shape and the first-steps exit condition |
| [references/ia-plan.md](references/ia-plan.md) | Running steps 7, 11 and 12. Holds the type enum, the nav seeding table, the nav shape per generator, and the plan output |
