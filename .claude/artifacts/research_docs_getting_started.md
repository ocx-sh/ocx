# Research: A use-case-led getting-started path

## Metadata

**Date:** 2026-11-16
**Domain:** web (docs information architecture)
**Triggered by:** owner request: rebuild the getting-started path on the mise walkthrough model. It should be short and use-case-led, and it should link out instead of explaining inline.
**Expires:** 2027-05-16, or on the next sidebar or `docs_ia.md` change
**Base:** branch `uno` at `91871b51`. Another session committed to `install-ocx.mdx` and `global-toolchain.mdx` within the hour before this was written, so every `path:line` below is read at that commit.

## Direct Answer

Keep four pages in **Start here** and give each one a single job.

- **Overview** becomes a router that sends each reader to the page for their job.
- **Install ocx** shrinks from 2,743 to about 300 prose words. The other 95 % moves, with its anchor ids unchanged, to a new everyday how-to, `guides/install-options.mdx`.
- **`first-project.mdx`** is rewritten in place as the mise-style walkthrough "Use ocx in a repository". It runs pin, share, shell, CI, offline, keep current, everyday commands and next steps. Each section opens with a use case, shows one tested block and ends in a go-deeper link.
- **`first-package.mdx`** stays as the optional "without a project" tutorial.

The change needs one new doc script (`first-project__offline.sh`) and reuses five existing ones. The riskiest constraint is mechanical: the old-URL test does not follow redirect chains. Every anchor that leaves `install-ocx` must therefore be repointed in three existing redirect files and in `old-urls.txt` in the same change.

## Findings

### The mise pattern (what makes the walkthrough work)

Sources: [walkthrough][mise-walk], [getting-started][mise-gs], [installing][mise-install].

- **One persona, one job.** The H1 is "Use mise in an existing project". It assumes mise is installed, and installing is a separate 3,500-word method catalogue.
- **Verb-phrase H2s, seven in all, no H3s:** Declare the project's tools · Keep personal settings local · Share the setup · Run project commands as tasks · Keep tools current · Everyday commands · Further reading.
- **The first sentence of each H2 is the job**, never the concept. Examples: "Start from the version files the project already has." "Commit `mise.toml`." "Wrap the commands the project already runs…"
- **Short.** About 700 prose words, about 14 code blocks and about 20 commands, roughly one command per 35–55 words.
- **Output is shown once.** Only the first section shows output (`mise ls --current` as `#` comments). Every other block is a command without output.
- **Depth is linked, not written.** Each section has 2–7 inline links plus a closing "See …" link. The page uses no admonitions, tabs or casts.
- **"Keep tools current" is 40 words with no code block.** It is two inline commands and three links.
- **"Everyday commands" is a 7-row table** with every row linked to its CLI reference entry. **"Further reading"** is a 7-item link list.
- **What it leaves out:** install, shell activation details, the backend catalogue, secrets, lockfile mechanics, trust internals and task hooks. Each of these is one link away.
- **Two pages, two roles.** `getting-started.html` is the greenfield tutorial: install, run a tool, set up a project, activate (tabbed per shell), find more tools, next steps. The walkthrough is the brownfield path, and getting-started links to it.

Patterns from comparable tools that mise lacks:

| Tool | Pattern | Take it? |
|---|---|---|
| [pixi first workspace][pixi] | One concept per H2, each closing with exactly one "learn more" link; optional depth in collapsed callouts | Yes, as one go-deeper line per section |
| [aqua tutorial][aqua] | Ends on proof: `command -v gh`, `gh version` transcripts | Yes, each section's block ends in printed output where a tested script exists |
| [uv getting started][uv] | Install, verify and learn-by-task live on separate tiny pages, with Previous and Next footers | Already true in Starlight; keep install separate |

### The current OCX entry path: what a new reader wades through

**Landing (`website/src/content/docs/index.mdx`, 174 words)**

- **Lead-in.** `:10-14` carries five sentence terminators before the first fence. `landing_check.py` reports DOC-TYPE-10, but that check is not in the CI gate (`test/lint/test_docs_quality_gate.py:26` runs only `doc_declaration` and `page_type`).
- **Hand-typed output.** `:18-22` is a typed copy of output, not checked against any run.
- **Menu names pages, not jobs.** "Start here" (`:24-30`) is Install / first package / first project.
- **No on-ramp for the primary users' jobs.** CI, Bazel, devcontainers, publishing, global tools and offline use are reachable only through two hub links (`:40-41`). Automation authors are the primary users (`product-context.md` § Target Users).

**Install (`getting-started/install-ocx.mdx`, 2,743 prose words, 489 lines, `doc_tier: first-steps`)**

- **What first success needs.** Only `:17-26` (`<InstallCommand />` plus `install-ocx/about.sh`).
- **What the reader wades through after it:**
  - the installer options ladder and its rationale (`:44-155`, including the four-rung ladder at `:58-63` and "deliberately no `--modify-path`" at `:65`)
  - the corporate-CA tabs (`:97-150`)
  - the shells table (`:157-167`)
  - setup internals (`:222-243`)
  - pinned self-setup with digests (`:245-278`)
  - session PATH stores and the registry-editing removal recipe (`:280-383`)
  - signature verification (`:385-407`)
  - dev builds (`:409-421`)
- **Untested `ocx` commands.** Four of them sit in `text` fences (`:249`, `:268`, `:391`, `:401`). NC1 (`test/src/doc_binding.py:69`) scans only `sh` and `bash` fences, so these first-steps commands escape the tested-script rule.
- **Prose finding.** DOC-PLAIN-11 "newest" at `:125` (`prose.py`).

**First package (`getting-started/first-package.mdx`, 461 words)**

- **It teaches the OCI-tier symlink layer and ends without the reader's goal.** `:96` admits the tool is not on `PATH`: "does not put a bare `cmake` on your `PATH`".
- **Long versioned path.** The `~/.ocx/symlinks/.../current/content/bin/cmake` runs at `:66-72` and `:86-92` are a stable-paths concept.
- **`ShellSession` wraps commands that show no output.** This happens at `:78-82`, `:102-106` and `:128-132`, against docs-style § Components. At `:108` the reader gets a prose description of a table instead of the table.
- **Caveats after the closing section.** Exit 79, the empty local index and `--remote` sit after Next steps (`:143`).

**First project (`getting-started/first-project.mdx`, 333 words)**

- **Right persona, but only half the job.** It stops at shell activation and has no CI or offline step. CI is the primary use case, and offline is principle 2.
- **`ShellSession` without output** at `:80-86` (`git add`) and `:90-94` (pull).
- **`cat ocx.toml` shown as a transcript** at `:41-54`.
- **Jargon with no definition.** "consent stamp" at `:100` and "binding name" at `:56`.
- **Exit-65 caveat inline** at `:74`.

**Sidebar (`website/astro.config.mjs:53-58`)**

- Four entries, named after artifacts (package, project), not after jobs.
- Every other group is collapsed (`:59-124`).

**Use cases with no short on-ramp from Start here**

- **CI.** In `integration/github-actions.md` the turnkey `setup-ocx` YAML is at `:46`, behind three paragraphs, a note and a persistence-channel H2 (`:18`).
- **Publishing.** In `publishing/index.mdx` "Publish your first package" (`#tldr`, `:70`) sits after the journey (`:10`) and the decision flowchart (`:46`).
- **Global tools.** `guides/global-toolchain.mdx` is 1,923 words, though its quick add at `:20-30` is fine.
- **Offline.** `guides/work-offline.mdx` has no tested script; its `--offline` example at `:29` is an untested fence.
- **Bazel and devcontainers.** `integration/bazel.md` (486 words) and `integration/devcontainers-and-editors.md#devcontainer` are not linked from any first-steps page.
- **Shell activation.** The concept page is 4,233 words.

**Unused tested assets**

12 tested doc scripts are included or cast by no page: `getting-started/{env,env-multi,exec,find-candidate,install,install-select,project-toolchain,uninstall,index,select-deselect}` and `user-guide/{exec-once,install}`.

### Rule constraints that shape the design

| Constraint | Source | Effect on the design |
|---|---|---|
| A tutorial carries no tabs and no prose branch ("or, with", "alternatively", "if you use/prefer") once two fences share a language | DOC-DISC-17, `page_type.py:62-63,264-286` (in the CI gate) | The walkthrough is one path. CI shows the generic tested commands, not "GitHub or GitLab". `<InstallCommand />` is safe because the source holds no `<Tabs` |
| At most 100 words between the introducing heading and the first command; no callout before it | DOC-DISC-16, `page_type.py:32,295-329` | Intro of 60 words or fewer; the first section opens on its block |
| At most 9 actions to the first success on a `cli` shape | DOC-DISC-15 | The first block already prints the tool version |
| A tutorial needs two or more interacting concepts | DOC-DISC-19 | The walkthrough qualifies (lock, store, shell hook, CI). A per-use-case micro-page would not |
| Landing: a one-sentence lead-in, a command or link menu before word 150, at most 2 buttons, at most 2 ordered items, at most 3 table rows | DOC-TYPE-10/11/12, DOC-TYPE-03 landing arm (`page_type.py:36-37,144-166`, `landing_check.py`) | The router is an **unordered markdown link list**. `LinkCard`/`CardGrid` do not count as a link menu in `landing_check.py:85-152`, and they add DOM |
| Every `ocx` invocation in an `sh`/`bash` fence on a `getting-started/*` page, or on any page that transcludes, must be a `\<\<\< @/_scripts/<slug>.sh\{sh\}` include with a published slug | NC1/NC2 in `test/src/doc_binding.py:107-120,131-237,305-351`, run by `test/lint/test_doc_binding.py` | Every command in the new sections is an include. `git add`, `cd`, `cmake --version` and file listings stay inline |
| A recording is a real run; never a typed transcript | DOC-EX-12 | No new casts. Output fences are copies of stub output, an honesty gap (Risk 2) |
| `ShellSession` pairs fences by position (odd = command, even = output) and is not used on a command without output | theme `ShellSession.astro:2-3,38`; docs-style § Components | Several command/output pairs can share one frame. A command without output gets a bare include |
| Every cross-linked heading has an explicit `{#id}`; the old-URL test checks the **target** id directly and follows no chain | DOC-NAV-07; `website/tests/old-urls.test.mjs:45-69` | Moved anchors keep their ids, and `redirects.d` targets are repointed, not chained |
| `#manual` is printed by CLI help | `crates/ocx_cli/src/command/self_group.rs:126` | `#manual` stays on `install-ocx` (an interface) |
| Lighthouse 100 in every category, DOM at most 900, pre-JS gzip at most 11 KB, HTML gzip at most 43,400 B | `website/lighthouse.budgets.mjs:14,18` | No `Terminal` on entry pages. Static `ShellSession` and markdown lists only |
| The first-steps tier sits in a different top-level group from the everyday hub | DOC-DISC-21, `docs_ia.md` §9 | The new install-options page goes to "Settings and trust", not to Start here |
| `test/SUITE_FLOOR` rises in the commit that adds tests and never falls | `subsystem-tests.md` § Floor files | Add one script, delete none, so no orphaned script is removed |

## Proposal

### Page set and sidebar order (group "Start here", stays expanded)

| # | Label | File (URL unchanged) | `doc_type` / `doc_tier` | Target prose words (today) |
|---|---|---|---|---|
| 1 | Overview | `index.mdx` (`/`) | landing / first-steps | ≤ 200 (174) |
| 2 | Install ocx | `getting-started/install-ocx.mdx` | how-to / first-steps | ≤ 350 (2,743) |
| 3 | Use ocx in a repository | `getting-started/first-project.mdx` | tutorial / first-steps | ≤ 750 (333) |
| 4 | Install without a project | `getting-started/first-package.mdx` | tutorial / first-steps | ≤ 420 (461) |

Add a new page, **"Customize the install"**, at `guides/install-options.mdx` (how-to, everyday, about 2,400 words). It goes in the "Settings and trust" group directly before "Update ocx".

The total first-steps prose falls from 3,711 to about 1,700 words.

Glossary items the new prose leans on (tooltip plus link, designed separately): lock file, digest, binding, consent, package store, local index, frozen, toolchain.

### Page 1, Overview (`index.mdx`)

| Section | Use case it opens with | Command(s) | Output source | Component | Go deeper |
|---|---|---|---|---|---|
| Lead-in (one sentence, kept from `:10`) | "Fetch pre-built tools from any OCI registry and run them reproducibly." | none | none | prose | none |
| Install | none | none | none | `<InstallCommand />` | `getting-started/install-ocx.mdx` |
| Run once | Run a tool before learning anything. The `ShellSession` title carries the framing, so no extra sentence and DOC-TYPE-10 stays at one | `first-package/exec.sh` (existing) | typed copy of the `cmake-versions` stub, `test/recordings/setups.py:89` | `ShellSession title="Run a tool without installing it"` | `getting-started/first-package.mdx#quick-start` |
| `## Pick your job {#start-here}` (anchor kept) | One task-phrased bullet per job, 7 items in an unordered link list | none | none | markdown list | see the list below |
| `## Look things up {#look-up}` (kept) | unchanged | none | none | prose | unchanged |

The job menu links to the shortest section that does each job:

- Pin a repository's tools → `getting-started/first-project.mdx`
- Give CI the same tools → `getting-started/first-project.mdx#ci`, then `integration/github-actions.md#ci-github-actions-toolchain`
- Put a tool on your PATH everywhere → `guides/global-toolchain.mdx#global-toolchain-add`
- Publish your own tool → `publishing/index.mdx#tldr`
- Provision Bazel or a dev container → `integration/bazel.md#bazel-project`, `integration/devcontainers-and-editors.md#devcontainer`
- Work offline or through a mirror → `guides/work-offline.mdx#offline-modes`, `guides/corporate-mirror.mdx`
- Install and switch versions without a project → `getting-started/first-package.mdx`

**Moves out.** The "That installs ocx. A download-only route…" sentences (`:14`) are cut; the install page link carries the download route. The `#everyday` and `#publish` sections fold into the job menu. No page links either anchor, and neither appears in `old-urls.txt`.

### Page 2, Install ocx (`getting-started/install-ocx.mdx`)

| Section | Use case it opens with | Command(s) | Output source | Component | Go deeper |
|---|---|---|---|---|---|
| Goal + install (kept `:17-26`) | "This page installs ocx and puts it on your `PATH`." | `install-ocx/about.sh` (existing) | none shown; the version varies per release | `<InstallCommand />`, bare include | `reference/command-line/commands.md#about` |
| zsh GUI note (kept, moved below the command) | `ocx` is not found after install | none | none | `:::note` | `troubleshooting/shell.mdx#commands` |
| `## Download the binary instead {#manual}` (anchor kept for CLI help) | "Your policy forbids piping a script into a shell." | the inline archive download + `ocx self setup` (unchanged, NC1-clean) | none | archive table + `<Tabs syncKey="os">` (allowed on how-to) | `guides/install-options.mdx#install-bare-binary-what`, `#manual-pinned` |
| `## Next steps {#next-steps}` (kept) | none | none | none | link list | `first-project.mdx`, `guides/install-options.mdx`, `guides/update-ocx.mdx`, `integration/docker.md` |

**Moves out to `guides/install-options.mdx`, with ids byte-identical:**

- `#with-script` (with the wget caution and the one-installer-per-shell rationale)
- `#script-options` (fix "newest" at `:125` on the way)
- `#shells`, `#install-bare-binary-what`, `#manual-pinned`
- `#session-path`, `#session-path-stores`, `#session-path-removal`
- `#verify-signature`, `#dev-builds` (this one brings `install-ocx/version-verbose.sh` and `version-json.sh`)

The new page opens with one goal sentence, then the moved H2s in today's order. Relative links inside the moved text are rebased:

- `#manual` becomes `../getting-started/install-ocx.mdx#manual`
- `../guides/update-ocx.mdx` becomes `./update-ocx.mdx`

The four untested `text` fences move unchanged, because they are not first-steps content any more. Backing them with scripts is a follow-up.

### Page 3, Use ocx in a repository (`getting-started/first-project.mdx`, rewritten in place)

Intro (≤ 60 words): "By the end of this page a repository pins CMake, teammates and CI run the identical build, and it still builds with no network. It assumes ocx is [installed][installation]."

Every section follows the same shape: a use-case sentence, one block, one or two sentences of result, and one go-deeper line.

| H2 `{#id}` | Use case (first sentence) | Command(s) shown | Output source | Component | Go deeper |
|---|---|---|---|---|---|
| Pin and run the tools `{#project-toolchain}` (anchor kept: `contributing/bazel.mdx`, `redirects.d/P13a.json:6`) | "Every clone of this repository should build with the same CMake, with nothing installed machine-wide." | `first-project/init.sh` · `first-project/add.sh` · `first-project/exec.sh` (all existing, `setup:cmake-versions`), as three command/output pairs in **one** frame; then `ocx.toml` as a `toml title="ocx.toml"` file listing (replaces `cat`) | `created …/ocx.toml`; the `Binding Group Digest` table; `cmake version 3.31.12` + banner, typed from the stub at `setups.py:89` | one `ShellSession title="Pin CMake and run it"` (6 fences; use `collapsed` output and numbered badges 1–3 when the annotation feature lands) | `guides/project-toolchain.mdx`, `reference/project-file.md` |
| Share the pin `{#share}` (kept) | "A teammate who clones the repository gets your build with one command." | inline ` ```sh git add ocx.toml ocx.lock``` ` (bare fence, no frame), then `first-project/pull.sh` (existing) | `cmake version 3.31.12` (stub) | bare fence + `ShellSession title="On a fresh clone"` | `guides/lock-and-reproduce.mdx`, `guides/run-project-tools.mdx#exec` |
| Use them from your shell `{#shell}` (kept: `explanation/shell-integration.mdx`) | "Inside the repository, `cmake` answers by name; outside, it is gone." | inline `cd .. && cd my-project` + `cmake --version` (no `ocx` invocation, as today; the behaviour is proven by the pty tier of `test/tests/test_shell_reconcile.py`) | `cmake version 3.31.12` | `ShellSession title="Enter the repository"` | `explanation/shell-integration.mdx`, `troubleshooting/shell.mdx#diagnostics` |
| Run the same tools in CI `{#ci}` (new) | "A CI job must refuse a stale lock and build with exactly the locked tools." | `user-guide/ci-project.sh` (existing): `ocx lock --check`, `ocx --frozen pull`, `ocx exec -- cmake --version` | none shown (its `full-catalog` state prints 4.2.3, which would contradict 3.31 on this page) | bare include | `integration/github-actions.md#ci-github-actions-toolchain` (turnkey `setup-ocx`), `integration/gitlab-ci.md`, `integration/unattended-ci.md` |
| Build without the network `{#offline}` (new) | "An air-gapped runner, or a laptop on a plane, still builds from what is on disk." | **NEW** `first-project/offline.sh`: `ocx --offline exec -- cmake --version` | `cmake version 3.31.12` (stub) | `ShellSession title="Offline"` | `guides/work-offline.mdx#offline-modes`, `guides/corporate-mirror.mdx` |
| Keep tools current `{#update}` (new) | "Move to a newer CMake on purpose, never by accident." | none. The mise pattern: inline `ocx update` (re-resolve tags into the lock) and `ocx upgrade` (move the tag in `ocx.toml`) in prose | none | prose, 40 words or fewer | `guides/update-tools.mdx`, `reference/command-line/commands.md#update`, `#upgrade` |
| Everyday commands `{#everyday}` (new) | none, a table (mise pattern) | none (inline code in cells, no fences) | none | 7-row table: `ocx status`, `ocx add` / `ocx remove`, `ocx lock`, `ocx update`, `ocx exec`, `ocx --global add`, `ocx clean` | each row links its `commands.md#<id>`; `--global add` links `guides/global-toolchain.mdx#global-toolchain-add` |
| Next steps `{#next-steps}` (kept) | none | none | none | link list by job | global toolchain, `publishing/index.mdx#tldr`, `integration/bazel.md`, `integration/devcontainers-and-editors.md` (+ VS Code / Open VSX links kept from `:125`), `integration/docker.md`, `first-package.mdx` |

**Writer constraints.** No `<Tabs>`. None of "alternatively", "or, with", "if you use" or "if you prefer" anywhere on the page: the page has several `text` fences, so `PROSE_BRANCH` arms. No callout before the first block.

**Moves out:**

- **The binding sentence (`:56`) and "consent stamp" (`:100`)** become glossary tooltips.
- **The exit-65 caveat (`:74`)** is deleted. `guides/lock-and-reproduce.mdx` already documents exit 65 on drift (`:70`), so one link covers it.
- **`#run`** is dropped: nothing links it. A fragment map sends it to `#project-toolchain`.

### Page 4, Install without a project (`getting-started/first-package.mdx`, kept)

- **Sections and anchors are unchanged.** `#quick-start`, `#installing`, `#switching-versions`, `#environment`, `#uninstalling` and `#next-steps` are all redirect targets in `redirects.d/P14.json:142-147` and `P13b.json:29`.
- **The intro gains one sentence on when to pick this page.** Use it for machine-wide installs with stable paths for editors and scripts. For a tool on `PATH` in every shell, the reader goes to the global toolchain.
- **The `ShellSession` frames at `:78-82`, `:102-106` and `:128-132` become bare includes.** The alternative, adding the real output, needs a typed copy (Risk 2).
- **`:143` is deleted.** Its facts already live in `guides/find-packages.mdx#find-versions` (`--remote` before the subcommand, `:12-29`) and `reference/exit-codes.md` (79). Next steps links both.

### Redirect and anchor work

- **New `website/redirects.d/entry-path.json`.** For `/docs/getting-started/install-ocx/` it holds `fragments` that map each of the 10 moved ids to `/docs/guides/install-options/#<same id>`. For `/docs/getting-started/first-project/` it maps `run` to `/docs/getting-started/first-project/#project-toolchain`. A built page with `fragments` and no `to` is allowed (`plugins/redirects.mjs:4-7`).
- **Repoint existing targets, because the test reads the target id directly and follows no chain:**
  - `P14.json` `/docs/installation/` fragments `dev-builds`, `manual-pinned`, `script-options`, `shells` and `with-script` go to the new page. `manual` and `next-steps` stay.
  - `P13b.json:32,34` go to the new page: `install-bare-binary-pin` to `#manual-pinned`, `install-bare-binary-what` to `#install-bare-binary-what`.
  - `P13f.json:10-11` go to the new page: `install-specific-version` to `#manual-pinned`, `installation-faq` to `#script-options`.
- **Append to `website/old-urls.txt`** the 10 `/docs/getting-started/install-ocx/#<moved id>` URLs and `/docs/getting-started/first-project/#run`. The live site serves them today, so the test then guards them.
- **Repoint these inbound source links to `../guides/install-options.mdx#<id>`:**
  - `integration/github-actions.md` (`#manual-pinned`, `#script-options`)
  - `integration/unattended-ci.md` (`#script-options`)
  - `troubleshooting/shell.mdx` (`#script-options`)
  - `guides/global-toolchain.mdx` (`#session-path`, `#session-path-removal`)
  - `guides/update-ocx.mdx` (`#session-path-removal`)
  - `explanation/shell-integration.mdx` (`#session-path`)
- **Links that need no edit:**
  - `#manual` in `guides/manage-config-centrally.mdx`
  - the generated `reference/command-line/self.md`, through its CLI help source at `self_group.rs:126`
  - `first-project#project-toolchain` and `#shell`
  - every `first-package` anchor

### Alternatives (not recommended)

- **A. New URL `/getting-started/walkthrough/`.** A clearer name, but it costs a `to` redirect for `first-project/` with 5 fragments, repointing `P13a.json:6`, and 3 inbound link edits. It gains nothing a reader sees, because the sidebar label carries the name.
- **B. One mega-page: install, run, project, activate (mise's `getting-started.html` shape).** Install needs per-OS tabs, which a tutorial cannot hold (DOC-DISC-17), and the page would blow the 100-word first-command and 9-action budgets.
- **C. Per-use-case micro-pages under `getting-started/` (ci, global, publish).** These duplicate everyday guides, pull everyday content into tier one (DOC-DISC-21), and each fails DOC-DISC-19 (one concept).
- **D. Trim `install-ocx.mdx` only.** This is the cheapest option, but CI, offline and the job router stay missing, so the use-case ask goes unmet.

## Work packages

The packages are file-disjoint. WP1, WP3 and WP4 run in parallel. WP2 depends only on the id contract: the moved ids are unchanged, and their new home is `guides/install-options.mdx`.

Before editing, every package runs `git log -1 --format=%h -- <its files>` and rebases. Another session is actively committing to `install-ocx.mdx` and `global-toolchain.mdx`, and `research_docs_toc.md` may touch `astro.config.mjs` or `route-middleware.ts`.

### WP1: split the install page

- **Files:**
  - `website/src/content/docs/getting-started/install-ocx.mdx`
  - NEW `website/src/content/docs/guides/install-options.mdx`
  - NEW `website/redirects.d/entry-path.json` (the `install-ocx` half and the `first-project#run` entry)
  - `website/redirects.d/{P13b,P13f,P14}.json`
  - `website/old-urls.txt`
- **Doc scripts:** none new. `install-ocx/version-verbose.sh` and `version-json.sh` move with `#dev-builds`. The new page then transcludes, so NC1–NC4 cover it automatically (`doc_binding.py:112-120`).
- **Verify:**
  - `cd test && OCX_TESTS_NO_REGISTRY=1 uv run pytest lint/test_docs_quality_gate.py lint/test_doc_binding.py -q`
  - `python3 .claude/rules/docs-quality/checks/prose.py website/src/content/docs/getting-started/install-ocx.mdx website/src/content/docs/guides/install-options.mdx`
  - `task website:build`, then `task website:check` (links + old URLs) and `task website:test`
- **Red-then-green check:** delete one fragment line and confirm `task website:check` names it, then restore it.

### WP2: repoint inbound anchors

- **Files:**
  - `website/src/content/docs/integration/github-actions.md`
  - `integration/unattended-ci.md`
  - `troubleshooting/shell.mdx`
  - `guides/global-toolchain.mdx`
  - `guides/update-ocx.mdx`
  - `explanation/shell-integration.mdx`
- **Doc scripts:** none.
- **Verify:** `task website:build && task website:lint:links`. Zero hits on `rtk proxy grep -rnE "install-ocx(\.mdx|/)#(with-script|script-options|shells|install-bare-binary-what|manual-pinned|session-path|verify-signature|dev-builds)" website/src/content/docs`.

### WP3: the walkthrough plus one new doc script

- **Files:**
  - `website/src/content/docs/getting-started/first-project.mdx`
  - NEW `test/doc_scripts/first-project__offline.sh`
  - `test/SUITE_FLOOR` (4263 → 4264)
  - possibly `test/limits/windows*/SKIP_CEILING` (+1; doc scripts skip on win32, `test/tests/test_doc_scripts.py:33-36`)
- **New script:**

  ```bash
  #!/usr/bin/env bash
  # state: setup:cmake-versions
  # doc: first-project/offline
  # title: Build without the network
  # description: Run the locked tool with all network access disabled, from the packages ocx add already downloaded.
  set -euo pipefail

  cd "$SCENARIO_TMP"
  mkdir my-project
  cd my-project
  ocx init
  ocx add $PKG_KITWARE_CMAKE
  # region cast
  ocx --offline exec -- cmake --version
  # endregion cast
  ```

- **How the script is wired:**
  - No `# cast:` line, so no `CAST_SCRIPTS` or `cast.bzl` edit is needed.
  - Discovery: `discover_doc_scripts` picks it up as a new case in `tests/test_doc_scripts.py`.
  - Bazel input: the `//test/doc_scripts:doc_script_sources` glob (`test/BUILD.bazel:277`), with no BUILD edit.
  - Publishing: `task website:scripts:publish` writes `website/src/_scripts/first-project/offline.sh`, and NC2 resolves the slug through `task test:doc-scripts:list`.
- **Verify:**
  - `task test` (builds `test/bin/ocx`), then `cd test && uv run pytest tests/test_doc_scripts.py -k "offline or first-project or ci-project" -v`. Do not pass `-k` through `task test:doc-scripts:drift`: its `CLI_ARGS` also reach the lint run, where the filter selects nothing.
  - Show red first: in a scratch copy, replace `ocx add` with `ocx add --no-pull` and confirm the case fails (exit 81 or a network refusal). Then restore it.
  - `cd test && OCX_TESTS_NO_REGISTRY=1 uv run pytest lint/test_doc_binding.py lint/test_doc_scripts_parser.py lint/test_doc_scripts_publish_structure.py lint/test_docs_quality_gate.py -q`
  - `python3 .claude/rules/docs-quality/checks/page_type.py website/src/content/docs/getting-started/first-project.mdx` exits 0. A scratch copy with a planted "alternatively" must exit 1, which proves DOC-DISC-17 arms on this page.
  - `task website:build && task website:lighthouse -- --page /getting-started/first-project/`, for DOM at most 900.
  - A cold-reader walk of the page by a subagent with no repository context (DOC-DISC-20).

### WP4: landing, sidebar, first package

- **Files:**
  - `website/src/content/docs/index.mdx`
  - `website/src/content/docs/getting-started/first-package.mdx`
  - `website/astro.config.mjs`: the Start-here labels and order per the table, plus `page('Customize the install', '/guides/install-options/')` in "Settings and trust" before "Update ocx"
- **Doc scripts:** none new (reuses `first-package/*`).
- **Verify:**
  - `python3 .claude/rules/docs-quality/checks/landing_check.py website/src/content/docs/index.mdx`: no findings. Today it reports DOC-TYPE-10.
  - `python3 .claude/rules/docs-quality/checks/page_type.py website/src/content/docs/index.mdx website/src/content/docs/getting-started/first-package.mdx`
  - `cd test && OCX_TESTS_NO_REGISTRY=1 uv run pytest lint/test_docs_quality_gate.py lint/test_doc_binding.py -q`
  - `task website:build && task website:check && task website:lighthouse -- --page /`

### WP5 (deferred, owner decision): golden-backed output

- **Files:** `test/doc_scripts/*.out` (via the existing `# expect:` header, `test/src/doc_scripts.py:254,658-662`), `website/scripts/publish_doc_scripts.py` (copy each golden to `_scripts/<slug>.out`), `test/src/doc_binding.py` (NC2 accepts `.out` includes) and their tests.
- **Effect:** pages then include `\<\<\< @/_scripts/<slug>.out\{text\}` instead of typed output fences.
- **Not part of this change.**

## Risks and open questions

1. **The old-URL test follows no redirect chain. This is the riskiest constraint.** Moving an `install-ocx` anchor without repointing `P13b`, `P13f` and `P14` (and appending the live URLs) breaks `task website:check` only at deploy time (`deploy-docs.yml`). **Recommendation:** WP1 does the move, the repointing and the `old-urls.txt` additions in one commit, and shows `task website:check` red and then green before committing.
2. **Typed output.** Every output fence on the entry path is a hand copy of stub output from `test/recordings/setups.py`, so nothing fails when it drifts. **Recommendation:** accept that for this change. Copy outputs only from the stub of the state the page's script runs (3.31 throughout), and schedule WP5.
3. **The walkthrough URL stays `/getting-started/first-project/`.** **Recommendation:** keep it (zero redirect work; the label is the name), not Alternative A.
4. **The CI section shows the tested generic commands and links the `setup-ocx` YAML.** The alternative is to embed the YAML, which is untestable here and turns the tutorial into a GitHub-or-other branch. **Recommendation:** generic commands plus the link.
5. **`first-package` stays in Start here** as an optional fourth entry. The alternative is to demote it into "Packages". **Recommendation:** keep it, since `docs_ia.md` G02 and G03 map to it and its anchors are redirect targets. Revisit once the global toolchain page has a short on-ramp of its own.

## Sources

| Source | Type | Date | Relevance |
|---|---|---|---|
| [mise walkthrough][mise-walk] | Docs | fetched 2026-11-16 | Primary pattern: headings, section openers, link placement |
| [mise getting started][mise-gs] | Docs | fetched 2026-11-16 | Greenfield tutorial and its split from the walkthrough |
| [mise installing][mise-install] | Docs | fetched 2026-11-16 | Install kept as a separate catalogue page |
| [uv getting started][uv] | Docs | fetched 2026-11-16 | Install, verify and learn split into tiny pages |
| [pixi first workspace][pixi] | Docs | fetched 2026-11-16 | One "learn more" link per section |
| [aqua tutorial][aqua] | Docs | fetched 2026-11-16 | Proof-by-output ending |
| `.claude/artifacts/docs_ia.md` §3, §4, §9 | Repo | 2026-10-11 | Goals G01–G06, the Start-here group |
| `.claude/rules/docs-quality/page-types.md`, `examples.md`, `navigation.md` | Repo | current | DOC-DISC-15…21, DOC-TYPE-10…13, DOC-EX-12, DOC-NAV-07 |
| `test/src/doc_binding.py`, `test/lint/test_docs_quality_gate.py`, `website/tests/old-urls.test.mjs` | Repo | current | The machine gates |

Devbox's quickstart returned 404 and proto was not fetched, so neither informs this file.

[mise-walk]: https://mise.jdx.dev/walkthrough.html
[mise-gs]: https://mise.jdx.dev/getting-started.html
[mise-install]: https://mise.jdx.dev/installing-mise.html
[uv]: https://docs.astral.sh/uv/getting-started/
[pixi]: https://pixi.prefix.dev/latest/first_workspace/
[aqua]: https://aquaproj.github.io/docs/tutorial/
