---
paths:
  - "**/*.rs"
  - "**/*.py"
  - "**/*.pyi"
  - "**/*.ts"
  - "**/*.tsx"
  - "**/*.mts"
  - "**/*.cts"
  - "**/*.js"
  - "**/*.jsx"
  - "**/*.mjs"
  - "**/*.cjs"
  - "**/*.vue"
  - "**/*.svelte"
  - "**/*.astro"
  - "**/*.go"
  - "**/*.java"
  - "**/*.kt"
  - "**/*.kts"
  - "**/*.swift"
  - "**/*.cs"
  - "**/*.scala"
  - "**/*.c"
  - "**/*.h"
  - "**/*.cc"
  - "**/*.cpp"
  - "**/*.cxx"
  - "**/*.hpp"
  - "**/*.hh"
  - "**/*.hxx"
  - "**/*.m"
  - "**/*.mm"
  - "**/*.rb"
  - "**/*.php"
  - "**/*.lua"
  - "**/*.dart"
  - "**/*.zig"
  - "**/*.sh"
  - "**/*.bash"
  - "**/*.zsh"
  - "**/*.ps1"
  - "**/*.psm1"
  - "**/Makefile"
  - "**/*.mk"
  - "**/Dockerfile"
  - "**/CMakeLists.txt"
  - "**/*.cmake"
  - "**/BUILD"
  - "**/BUILD.bazel"
  - "**/*.bzl"
  - "**/*.star"
  - "**/MODULE.bazel"
  - "**/WORKSPACE"
  - "**/WORKSPACE.bazel"
  - "**/*.nix"
  - "**/*.gradle"
  - "**/*.groovy"
summary: The code-comments index, holding the guard floor, where each clause goes, the length caps and ratchet, the gate, and where the depth lives
keywords: comments,code-comments,doc-comments,docstrings,rustdoc,tsdoc,jsdoc,godoc,javadoc,kdoc,guards,comment-density,ratchet,decision-records,adr,plan-ids,process-ids,interface-text,clap,json-schema,mcp,ai-agents
license: Apache-2.0
repository: https://github.com/ocx-sh/grimoire-lore
---

# Code comments

Two failures, in opposite directions. Left alone, an agent writes six-line
guards, essays and plan IDs where a human writes two lines; told to shorten, it
cuts the one clause that said what breaks. Here a comment that protects a line
keeps its constraint and consequence through every edit, and everything else is
capped, routed out of the source or deleted, so a cold agent learns from the
source alone why a non-obvious line looks the way it does and whether it may
change. This rule loads on every source edit and governs comments, doc
comments, docstrings, test names and the doc strings a generator copies into
help, schemas and hover.

Contents: [The gate](#the-gate) · [Non-negotiables](#non-negotiables) ·
[Where it goes](#where-it-goes) · [Caps](#caps) ·
[Where the depth is](#where-the-depth-is) · [Severity](#severity) ·
[What no check sees](#what-no-check-sees) · [Siblings](#siblings)

## The gate

`checks/` below means this rule's own `code-docs/checks/` directory, wherever
it is installed. The scripts import only the standard library and each other,
so keep the directory whole. Run every command from the repository root. Exit 0
is clean, 1 is findings, 2 is a usage error or missing input.

```sh
python3 checks/comment_census.py --root . --check .code-docs-length.json           # LEN-03
python3 checks/linkage_check.py ids --root . --check .code-docs-linkage.json       # LNK-01
python3 checks/linkage_check.py pointers --root . --check .code-docs-linkage.json  # LNK-02
python3 checks/linkage_check.py records --root . --base origin/main                # LNK-06
CLI=mytool; python3 checks/interface_leak.py --help-walk "$CLI"                    # SRF-03, every --help
SCHEMAS=schemas; python3 checks/interface_leak.py "$SCHEMAS"                       # SRF-03, emitted schemas
```

- **Rename** `origin/main`, `mytool` and `schemas` to the target branch, each
  shipped CLI and each directory of emitted schemas or captured output, and add
  `--lang python` or `--lang ts` off Rust. `--help-walk` recurses only clap and
  commander `Commands:` sections: an argparse or typer CLI pipes every screen
  from its SRF-03 test instead. That test, walking the generator's own model,
  is the durable form of the last two lines.
- **Adopting.** Run `linkage_check.py ids --root . --discover` first and give
  every prefix it lists a verdict (LNK-14). Then write each baseline once with
  `--update FILE --allow-regression` in place of `--check FILE`, commit both
  files, and run the `--check` lines in CI. From then on `--update` only lowers
  keys; a raised key needs `--allow-regression` and a reviewer. The checks skip
  installed agent rules and skills (`.claude/rules/`, `.opencode/skills/`),
  where this rule's own checks and fixtures sit.
- **Files.** The census rewrites its baseline whole; `ids` and `pointers`
  rewrite only their own keys and share one. Caps, library packages and
  `interface_files` (sources an unrecognised generator renders, SRF-01) live in
  `.code-docs.json`, repo-minted ID families and record globs in
  `.code-docs-linkage-config.json`. Each check reads its file from the root.

## Non-negotiables

Every row blocks a merge. IDs resolve through
[Where the depth is](#where-the-depth-is), where each rule carries its rationale
and verification. Row 13 binds new and grown blocks; the ratchet in row 14
holds existing ones until a cleanup cuts them.

| # | Rule | ID |
|---|---|---|
| 1 | Write every guard, a comment that stops a plausible edit from breaking something, as the constraint plus the concrete consequence of breaking it (a hang, a leak, a torn write, a wrong exit code), and let no edit remove either half. Keep nothing else in it: history, weighed alternatives and restated context go. This covers comments another rule mandates: `SAFETY:`, a discarded-result rationale, a lint-suppression reason. Write one sentence per breaking edit and never merge guards. Add no marker prefix (`GUARD:`, `load-bearing`, `IMPORTANT`); keep only labels a language or another rule mandates. | GRD-01, GRD-02, GRD-03 |
| 2 | Route a comment block longer than 3 lines clause by clause, never whole. Guard, why and contract clauses stay, every other clause goes to its row of [Where it goes](#where-it-goes), and a clause that fits no row stays. | RTE-01 |
| 3 | Credit a test, lint or type as a guard's owner only after applying the guard's breaking edit in a scratch copy and watching that owner fail. A check that only demands a comment, or a test that shares the guard's nouns, owns nothing. Every edit without a proven owner keeps its prose, and a line that names owners names every test that went red and none that stayed green. That one line stays even when a test owns the guard. | GRD-07, RTE-03 |
| 4 | Never auto-cut a block the guard recogniser flags, a `SAFETY:` label, the repo's deferred-shortcut marker (for example `ponytail:`), or a comment another rule mandates. A block the recogniser misses is unclassified, never safe to cut, and the recogniser never passes or fails an edit. | GRD-06 |
| 5 | Keep every reason a future edit depends on in the tracked tree. Never point a comment at a commit SHA, a branch, a backup ref or a gitignored path, and never leave a commit body or pull request text as a reason's only store. | RTE-02 |
| 6 | Write no bare plan or process ID from a banned family in any comment or doc comment, prod or test: state the constraint in the present tense, or cite the record. When removing one, delete it or restate the constraint, and add a pointer only when the same block already names that file, or when the record, read, holds the removed argument in its own words. | LNK-01, LNK-05 |
| 7 | Every record pointer resolves: `[repo:]path.md[#anchor]` names a file tracked in that repo, and an anchor matches exactly one heading or ID definition in it. | LNK-02 |
| 8 | A rewrite may drop a bare ID, but never a file-qualified pointer unless the same target reappears in the diff. | GRD-11 |
| 9 | A branch that deletes or renames a path repairs every line citing it in the decision records and path-scoped rules that existed before the branch: the line drops the old path or also names the new one. | LNK-06 |
| 10 | Text a generator copies to users (CLI help, JSON Schema descriptions, MCP tool descriptions, a published package's API docs) states the user contract only: no process ID, record or source filename or ISO date, and where the generator copies raw (help, schema, tool text) no code path or intra-doc link. | SRF-01, LNK-04 |
| 11 | Move maintainer rationale out of interface text verbatim, in the same edit, to a plain comment at the line it protects or directly above the item. When it runs past the plain cap, split it one sentence per breaking edit, each at its line. Never delete a clause on the way out. | SRF-02 |
| 12 | Give each generated surface a test that walks the generator's own model and fails on row 10's tokens. A source scan is the cheaper pre-build complement, never the gate. | SRF-03 |
| 13 | Keep a plain comment block to 5 lines and a doc block to 10, or 15 on a public item in a library-kind package. A block runs across blank lines, interface doc blocks count as public doc blocks, and a guard gets no exemption and is never truncated to fit. | LEN-01, LEN-02, LEN-05, LEN-08 |
| 14 | The lines in over-cap blocks, counted per file in prod code, never rise above the checked-in baseline. | LEN-03 |
| 15 | Set no comment-to-code ratio target, and never copy the comment density of the file being edited. The caps bind whatever the surrounding file's density is; idiom and naming still follow the file. | LEN-04 |

## Where it goes

Route each clause, not each block: a guard wrapped in an essay is still a guard.

| Clause | Home | Routing test | Rule |
|---|---|---|---|
| Guard on one line | A plain comment at that line. The doc comment only for a precondition, error condition or sentinel a caller needs without opening the body | Would a plausible edit here break something no type, lint or test catches? | GRD-01, GRD-04 |
| Why-constraint | One sentence at the site | A local reason with no single named breaking edit? | RTE-01 |
| Contract | The item's doc comment | Does a caller outside this file need it to use the item correctly? | RTE-01 |
| Argument: alternatives weighed, a trade-off, rejected options | A decision record, plus one file-qualified pointer at the site. The comment keeps only the clauses the record lacks. A small choice keeps its conclusion as one why sentence | Does it argue for a decision someone could re-open? | RTE-10, RTE-09 |
| History | Deleted, or rewritten as a present-tense guard when it still constrains an edit. Provenance that justifies a live value keeps at most 2 lines carrying a count or a named artifact | Does it bear on a plausible future edit, or hold a live value in place? | RTE-11 |
| Bare plan or process ID | Deleted, or the constraint restated | Does it name a plan item instead of stating the reason? | LNK-01, LNK-05 |
| Invariant across a subsystem | The path-scoped rule whose glob covers it; the per-file copies deleted | Is the sentence true verbatim at every file the glob matches? | RTE-07 |
| Guard a test, lint or type is proven to catch | One line naming the owner, every red test by name. Nothing when the construct states itself (`assert_never(x)`, `const exhaustive: never = x`) | Did the owner fail on the planted breaking edit? | GRD-08, RTE-03 |
| Universal negative: nothing may X, never Y | The doc comment of the test that asserts it | Is there no single line that implements it? | RTE-04 |
| Reference to other code | The symbol's name, never a plan ID or a `file:line` | Is the target a function, test or type? | RTE-05 |
| Narration, tautology, phase marker | Deleted, unless a clause names what the step guards, which makes it a guard | Does the next line or the name already say it? | RTE-01 |

## Caps

| Block | Cap | Human p90 block length |
|---|---|---|
| Plain comment (`//`, `#`) | 5 lines | 3 lines, apps and libraries alike |
| Doc comment | 10 lines | 6 lines in apps |
| Doc comment on a public item in a library-kind package | 15 lines | 20 lines on public library items; 15 flags 14.3% of them |

The reference is 32 open-source repos (16 apps, 16 libraries) at their last
pre-2022 commit, measured 2026-09-27 with this census. Their comment-to-code
medians, about 0.12 for apps and 0.27 for libraries, are context beside
`--report` and never a target (row 15).

- **Library-kind** means a package something outside its repo consumes.
  Declare it under `library` in `.code-docs.json`; undeclared packages are app-kind.
- **Signature sections do not count** toward a doc cap: `# Errors`, `# Panics`,
  `Args:`, `Returns:`, `@param` and the like, plus fenced code. `# Safety`,
  `Note:` and `WARNING` text always count (LEN-07).
- **An over-cap guard is split**, one comment per breaking edit at that edit's
  own line, never exempted, kept whole or truncated (LEN-05).

## Where the depth is

Read the file for the work you are about to do. One level deep: these files
never link each other.

| Doing | Read |
|---|---|
| Writing, shortening or judging a comment that protects a line: what it must say, doc or plain register, whether a test, lint or type owns it, exhaustive matches, a named RAII binding, moving a guard into a type | [code-docs/guards.md](code-docs/guards.md) |
| Deciding where a clause goes: splitting an essay, moving an argument to a decision record or an invariant to a path-scoped rule, keeping or dropping history, pointing at a test or a sibling function, naming a test after its guard | [code-docs/routing.md](code-docs/routing.md) |
| Citing a decision record, removing a plan or process ID, switching the ID ban on in a repo, deleting or renaming a file that records cite, or repairing a stale citation | [code-docs/linkage.md](code-docs/linkage.md) |
| Editing a doc comment or string that renders into help, a JSON Schema, an MCP tool list or published API docs, writing a doc summary line, or writing a module doc | [code-docs/surfaces.md](code-docs/surfaces.md) |
| A block over its cap, adopting or updating the length baseline, declaring a library package, or reading the census report | [code-docs/length.md](code-docs/length.md) |
| Shortening existing comments in bulk, cutting a file toward its baseline, or removing plan IDs across a tree | run the `code-docs-cleanup` skill; IDs in test files and test regions go in a separate test-only change |

## Severity

MUST = Block: fix before it lands. SHOULD = Warn: fix, or state why not in the
commit body. CONSIDER = Suggest: never blocks, never re-raised after a decline.

An adopter may override these defaults once, with the reason recorded, never
per file: the cap numbers and library packages in `.code-docs.json`
(`length.md`), and repo-minted ID families, standards and record globs in
`.code-docs-linkage-config.json` (`linkage.md`). The ten built-in ID families
never come off.

## What no check sees

- **A deleted guard within the cap.** The ratchet counts only over-cap lines,
  so the deletion trips nothing. Row 1 holds it, in review.
- **Guards the recogniser misses.** It finds fewer than half of them. Its hits
  are protected; its misses are unclassified.
- **Repo-minted IDs.** A green `ids` run proves nothing until `--discover` has
  a verdict for every prefix the repo mints.
- **Files the census does not lex.** The rule also loads on build files and
  scripts (CMake, Bazel, Nix, Gradle, shell, Makefiles), but the census and
  the checks read only the extensions in `EXT_LANG`. Every row holds there in
  review.
- **Citations in prose.** "The ADR on caching, decision 1A" carries no token,
  so interface text that cites a record in words passes the scanner. Row 10
  holds it, in review.

## Siblings

- **`docs-quality`** owns markup files and user-facing prose. This set owns
  text written in source files, including text that renders to help, schemas
  and hover. User reference too long for a doc cap moves to the generator's
  explicit text attribute (`long_about`).
- **The language quality sets** own language syntax: rustdoc section headers,
  docstring and JSDoc tag conventions, and the lints that demand a doc. Where
  one of their comment rules speaks to length, placement or what to cut, this
  set decides and that rule points here.
