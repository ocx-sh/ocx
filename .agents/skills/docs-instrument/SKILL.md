---
name: docs-instrument
description: Documentation verification gate for a repository that adopted the docs-quality rule set, covering the checks, configs, fixtures, harnesses and reader signals that make those rules enforceable. Use when someone asks to wire up docs checks or a docs CI gate, add doc_type and doc_tier declarations, retrofit a page declaration, set up markdownlint or Vale for prose, check docs links with lychee or a strict build, test documentation examples or code fences, add a doc-example harness, record terminal casts, capture zero-result searches, measure time to first working result, add a docs issue template, a feedback widget or privacy-preserving analytics, add a Lighthouse ratchet for a docs site, or land a new docs lint without turning every open pull request red. Not for deciding which pages to write or what the use-case tiers are, which is the docs-plan skill.
license: Apache-2.0
metadata:
  summary: Stands up the docs-quality verification gate in an adopting repository, cheapest check first
  keywords: docs,documentation,ci,gate,lint,markdownlint,vale,lychee,link-check,doc_type,declaration,tested-examples,harness,asciicast,zero-result,analytics,lighthouse,ratchet,fixtures
---

# docs-instrument

A repository has adopted the `docs-quality` rule set. Most of its rules carry a
runnable verification, and none of those verifications runs yet. This skill
turns that rule set into a gate that actually fails.

Work in cost order. The declaration retrofit is first because eight other
families read the declaration, and none of their checks can classify a page
until it lands.

## Stop condition

Stop when all five hold. Do not keep going, and do not add rules.

1. Every `checks/` script the repo wires up exits 0 with `--self-test`.
2. Every wired check has been seen red once, on a planted violation you reverted.
3. Every file path named in a runner target, a lint config or a rule row
   resolves on disk.
4. The signal manifest names every signal, including the deferred ones and their
   preconditions.
5. The whole-tree warning counts are recorded as the ratchet baseline.

A check that cannot go red is not coverage. A path that does not resolve is not
a check. Both are the failure this whole gate exists to prevent.

## Before you start

- Read `rules/docs-quality/checks.md`. It carries one row per script: what it
  checks, how to run it, its exit codes and its fixtures. Every flag beyond the
  common four lives there. Never invent a script name or a flag.
- Every Python check shares one interface:
  `python3 checks/<name>.py [--root DIR] [PATH ...] [--format text|json] [--self-test]`.
  Exit 0 is clean, 1 is findings, 2 is a usage error or missing input.
  Findings print as `path:line: DOC-XXX-nn: message`.
- Find the generator config: `mkdocs.yml`, `book.toml`, `.vitepress/config.*`,
  `docusaurus.config.*`, `astro.config.*`, `docs/conf.py`. The answer decides
  most of the wiring. See [references/wiring-by-generator.md](references/wiring-by-generator.md).
- Do not rewrite prose in this skill's work. Retrofits and configs only. A
  content fix belongs in its own change.

## The procedure

### Step 1. Build the published-file list

Every check reads this list. Getting it wrong is how a prose lint ends up
reporting on research notes and agent config (DOC-PLAIN-23).

The list is `git ls-files` under the directory holding the generator config,
plus repo-root `README.md` and `CHANGELOG.md`. Assert it excludes `.agents`,
`.claude`, `.serena`, `.worktrees`, `node_modules`, `dist`, `target` and any
build output directory.

A repository with a committed docs tree and no generator config still needs a
list. Name that directory once, in the runner. The DOC-NAV family then reports
not applicable and exits 0, because a tree with no site carries no sidebar and
no search box (DOC-NAV-01).

**Output.** One shell function or task variable in the runner that prints the
list. Schema in [references/rollout.md](references/rollout.md).

### Step 2. Retrofit the page declaration

`checks/doc_declaration.py` reads a `doc_type` and `doc_tier` comment inside the
first 12 lines, using the comment opener of the file's markup family. It never
reads a path (DOC-TYPE-02). Expect a 100 percent failure rate on the first run.

1. Run `python3 checks/doc_declaration.py --root . --seed`. It proposes a type
   per page from the nav config or the page heading. Nav-label seeding measured
   94.3 percent accurate over 122 pages (`wave2-declaration-key.md` section 10),
   against 68.1 percent for a path classifier.
2. Review every seeded value. Read the content for the pages the seed could not
   place, which is roughly a third of a flat tree.
3. Add `doc_tier` on `tutorial`, `how-to` and `landing` pages only. The other
   six types take a type line alone (DOC-DISC-13).
4. Land it as one commit that changes nothing but declaration lines.

Traps that destroy files, all measured (`wave2-declaration-key.md`):

- Never write the declaration as YAML frontmatter. mdBook renders it as a
  visible fake heading and indexes it (DOC-TYPE-28).
- Never place the comment above existing frontmatter. It destroys the
  frontmatter on all three tested generators (DOC-TYPE-29).
- Use `{/* */}` in MDX. An HTML comment is a hard build error there, and
  Docusaurus applies MDX parsing to plain `.md` unless `markdown.format` is set
  to `detect` (DOC-TYPE-30).

**Output.** One retrofit commit, and `checks/doc_declaration.py` exiting 0 over
the file list. Commit shape in [references/rollout.md](references/rollout.md).

### Step 3. Wire the checks into the existing runner

Use the task runner or test runner the repo already has. Do not add a second one.

Wire the scripts the rule set names: `prose.py`, `page_type.py`,
`landing_check.py`, `nav_depth.py`, `doc_examples.py`, `links_raw.py`. They all
import `strip_prose.py`, which strips front matter, declaration comments, fenced
code, code spans, link targets, images, tables, HTML, admonition markers and
include directives. Never hand a check raw file text.

Every one of them runs twice, and this is the rule that decides whether the gate
survives its first week:

- Once over the changed files, at error severity.
- Once over the whole list, at warning severity, until the backfill lands.

A check may launch at error whole-tree only at zero standing violations. The
median adopting page already fails several prose rules, so a whole-tree red gate
blocks every open pull request on day one. Record each warning count as the
ratchet baseline. Full schedule in [references/rollout.md](references/rollout.md).

**Output.** Two runner targets, `docs:check` at error on the diff and
`docs:audit` at warning whole-tree, plus a recorded baseline count per check.

### Step 4. Add markdownlint, and Vale only as a second tier

Tier 0 is grep and markdownlint and is always present. Tier 1 is Vale, only when
the repo accepts a new binary. No rule may take its only verification from
tier 1, because a Vale-only check is an unchecked rule everywhere Vale is absent.

Use the shipped `checks/markdownlint.jsonc`. It sets `MD025` with
`front_matter_title` empty. The default treats a frontmatter `title:` as a second
H1, which is a 100 percent false-positive rate against that convention. `MD054` takes per-style booleans and has no `style` key. `MD041` is
off wherever the declaration comment is line 1.

Give every checkable construct exactly one owning tool, and disable the other
tool's equivalent (DOC-PLAIN-20). Link style is the known collision. markdownlint
`MD054` owns it, so the Vale rule that demands the opposite stays off.

If Vale goes in, use `checks/vale.ini`. Pin each package by org and repo, never
by display name, and print the resolved rule count and error share before
enabling it. Two separately authored packages ship under the same popular name,
one with 17 tiered rules and one with about 111 rules all at error.

**Output.** A markdownlint target on the changed-file list, and optionally a
Vale target that no rule depends on alone.

### Step 5. Wire link checking

Two passes, and they answer different questions (DOC-OBS-01, DOC-OBS-02).

**Built output.** The generator's own strict build satisfies this. Seven of nine
measured sites already pass through `mkdocs build --strict` rather than through a
separate checker (`wave2-calibration-a.md` section 3). Do not replace a working
strict build with lychee. Where no strict mode exists, run
`lychee --include-fragments <build-dir>` after the build, with `<build-dir>`
taken from the generator row in
[references/wiring-by-generator.md](references/wiring-by-generator.md).

**Raw markdown.** Any pre-build pass needs a source root and an exclusion for
every page whose anchors are generated at build time. Without both it either
floods the log or checks nothing. One measured repo produced 65 phantom dead
links from a single four-line generated stub.

`checks/links_raw.py` resolves explicit `{#id}` anchors and root-relative
paths, skips build-time-anchor pages, and lists what it skipped.
`checks/lychee.toml` is the built-output example.

Prove it once. Break one anchor and one root-relative link in a fixture, confirm
a non-zero exit, revert.

**Output.** One built-output gate and one configured raw pass, both seen red.

### Step 6. Stand up the tested-example harness

Reach for the language's own doctest runner before writing anything
(DOC-EX-03). The per-language table, with the current tools and their real
limits, is [references/tested-examples-by-language.md](references/tested-examples-by-language.md).
Read it before you choose.

Where no runner fits, the harness is one file. It globs the example tree, runs
each file as a subprocess and asserts the exit code (DOC-EX-04). The subprocess
harness mode of `checks/doc_examples.py` is that file.

Bind a page to its test with a declared key in the test header, `# doc: <slug>`,
never with a mirrored file path (DOC-EX-02). A mirrored path breaks every page
the first time the test tree moves.

The gate itself is a set diff. Fences whose info string is on the runnable tier
list, against fences carrying a binding key. A non-empty difference fails
(DOC-EX-01).

Tag every fence from that tier list (DOC-EX-05). Write a tier suffix as one
hyphen-joined token such as `python-no-run`. A space in a fence info string is
unparsed under MkDocs Material and swallows the next fence (DOC-EX-20,
DOC-EX-21). Wrap a snippet that must not run in a paired marker carrying a
reason (DOC-EX-06).

The recording layer is optional and stays out of the gate (DOC-EX-11). Disable
recording, re-run the required gate, and the result must not change. Every
recording comes from a real run (DOC-EX-12), and a cast is committed only when
no build step regenerates it (DOC-EX-13).

**Output.** A harness bound to the pages, in the same required gate as the unit
tests. A failing example names the doc page, not only the test file (DOC-EX-07).

### Step 7. Add the reader signals the stack can carry

Add what the stack supports today, and defer the rest with its precondition
named (DOC-OBS-12). A requirement no repo can satisfy trains readers to ignore
the whole rule set.

In ascending cost:

| Signal | Cost | Rule |
|---|---|---|
| Docs issue template applying a `docs` label | zero | DOC-OBS-11 |
| Hand-measured, dated time to first working result | one person, one run | DOC-OBS-07 |
| Trigger matrix mapping source globs to the docs they invalidate | one file | DOC-OBS-03 |
| Zero-result search beacon and its sink | free tier to about 20 a month | DOC-NAV-10 |
| Privacy-preserving page analytics with custom events | free tier to about 45 a month | DOC-OBS-16 |
| Feedback widget | deferred until a real traffic denominator exists | DOC-OBS-16, DOC-OBS-17 |

Sinks, vendor comparison, the cost bands and the deferral form are in
[references/reader-signals.md](references/reader-signals.md). The per-generator
beacon wiring is in [references/wiring-by-generator.md](references/wiring-by-generator.md).

Two things to refuse. Enabling GA4 Enhanced Measurement never satisfies the
zero-result requirement. Its `view_search_results` event triggers on one of five
URL query parameters, and an overlay search writes none. Never bind the beacon
to a generator's internal search file either (DOC-NAV-16).

State the review trigger as a count or a release boundary, never as a bare
cadence word (DOC-NAV-15).

**Output.** `docs/.meta/observability.md`, holding every signal with a status, a
review trigger and a bias disclosure (DOC-OBS-10). Schema in
[references/reader-signals.md](references/reader-signals.md).

### Step 8. Add a Lighthouse ratchet where a built site exists

Only where the repo already builds a site. Assert the measured category scores,
a point or two below the current median, and raise the floor as it improves. The
one real measured instance runs accessibility at 0.97 against a median of 1.00.
Best practices sits at 0.93 against 0.96 and SEO at 0.97 against 1.00.
Performance is a warning at 0.85 against a median of 0.88.

Prove the gate red once. A missing `alt`, an empty `<button>` and an unlabelled
`<input>` moved one real site from 0.92 to 0.77 and failed the gate. Record that
proof beside the config. Schema in [references/rollout.md](references/rollout.md).

### Step 9. Every new check the adopter writes

This applies to a check you write, not to the shipped ones.

1. Write a `fail-*.md` fixture the check must reject, and a `pass-*.md` it must
   accept, under `checks/fixtures/<script>/`. A check with no rejecting fixture
   has never been proven able to fail.
2. Run the check over the real corpus and record its hit count and its
   false-positive rate.
3. Only then choose a severity. A check stays at warning until that rate is
   measured, and no unmeasured check goes above warning.
4. Every number in the finding message names its source in parentheses.

Fixture and measurement shapes are in [references/rollout.md](references/rollout.md).

## Run these before you call it done

```sh
# every shipped check proves itself against its own fixtures
for s in checks/*.py; do python3 "$s" --self-test || echo "SELF-TEST FAILED $s"; done

# every path any rule or config names resolves on disk
grep -ohE 'checks/[A-Za-z0-9_.-]+' rules/docs-quality*.md rules/docs-quality/*.md \
  | sort -u | while read -r p; do test -e "$p" || echo "MISSING $p"; done

# no unwired gate is described as coverage
grep -rniE 'tracked, not built|future gate|not implemented yet|(gate|check).{0,15}TODO' \
  rules/ .github/ 2>/dev/null

# no absolute or repository-specific path leaked into a template or fixture
grep -rnE '/(home|Users)/|(crates|services|packages)/' checks/ | grep -v Binary
```

Then plant one violation per wired check, watch it go red, and revert it.

## Failure modes an agent falls into here

Ranked by how often the measurements caught them.

1. **Names a check script and never writes the file.** The dominant defect in
   this domain, measured at 17 rules and 7 phantom files. Verify with the
   path-resolution grep above.
2. **Ships a checker that checks nothing.** Points lychee at the raw markdown
   tree with no source root and no fragment flag. It sees zero issues and
   reports link checking as done.
3. **Writes a check that cannot fail.** A count compared to itself. A grep on a
   beacon with no listener. A probe that reports "cannot verify" and passes.
4. **Launches the new lint at error across the whole tree** because strict feels
   safer, and blocks every open pull request.
5. **Classifies pages by path** because the directory name looks right. A path
   classifier is wrong about a third of the time, and a path glob for runbooks
   matched 0 of 248 real pages.
6. **Parses a nav config with a strict YAML loader.** `yaml.safe_load` hard-fails
   on 4 of 7 real configs over `!ENV` and `!!python/name:` tags.
7. **Assigns a severity from how important the rule feels**, before the check has
   run once against real content.
8. **Copies a tool's own space-separated fence attribute** into the tier scheme,
   which silently eats the next fence under MkDocs Material.
9. **Builds a bespoke harness** for work an installed doctest runner already
   does, then sizes it like the large worked example it read.
10. **Fires the nav and search family on a bare docs tree** with no generator.
    Every finding is false on a repo that was never a docs site.
11. **Proposes analytics as zero-result capture**, missing that the automatic
    detection needs a URL parameter an overlay search never sets.
12. **Adds a feedback widget first** because it is the most visible ask, with no
    traffic denominator behind the percentage it will print.
13. **Invents a freshness interval** such as "review every 90 days". No source
    validates one, and a date stamp is never a gate (DOC-OBS-13).
14. **Leaks a source path into a portable template**, usually by filling the
    trigger matrix with the rows it just read.
15. **Wires a rewrite-in-place example runner into CI and calls it tested.** That
    tool accepts a broken command's new output as the new expectation
    (DOC-EX-23).

## References

Read one level down, on demand. These files do not link each other.

| File | Read it when |
|---|---|
| [references/wiring-by-generator.md](references/wiring-by-generator.md) | You know the generator and need the exact config key, build directory, comment opener, beacon location or link-check flag for it |
| [references/tested-examples-by-language.md](references/tested-examples-by-language.md) | Before choosing an example-test mechanism for any language, and before writing a harness of your own |
| [references/reader-signals.md](references/reader-signals.md) | Standing up step 7: sink shapes, vendor costs, bias disclosure, the manifest and deferral schemas |
| [references/rollout.md](references/rollout.md) | Wiring any check into CI: the two-severity rule, the ratchet schedule, the retrofit and template schemas, and the fixture contract for a new check |
