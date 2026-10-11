# Docs information architecture (C-014)

Output of the `docs-plan` procedure over `website/src/docs/` (46 pages) plus the two root-site pages `website/src/index.md` and `website/src/team.md`, seeded with `.claude/rules/product-context.md`, the use-case inventory research and the reference recon. Plan: [`plan_docs_next_level.md`](./plan_docs_next_level.md) (P6, C-014, D3, D8, D11).

```yaml
product_shape: cli
signal: friction-log-severity
generated: 2026-10-11
surface_sources:
  - cli-subcommand        # crates/ocx_schema/tests/golden/cli.json, every top-level command
  - issue-title           # the research lane list (named issues only; no live tracker sweep was run)
  - readme-heading        # product-context.md "Use Cases" and "CLI at a Glance"
out_of_scope:
  - {id: X01, surface: "ocx about, ocx version", why: "one-line diagnostics, shown inside the install task"}
  - {id: X02, surface: "ocx shell completion", why: "setup detail, a flag of the install task"}
  - {id: X03, surface: "ocx-mirror pipeline", why: "separate tool with its own docs; linked from the reference sidebar"}
```

Read this file as data. Every table is parsed by a later step: P5 reads the **seed table**, P9 reads the **homes table**, P12 reads the **page inventory** and the **split table**, P13a to P13f read the rows of their `cluster`. Do not add a column or reorder one.

## 1. Product shape and reader model

OCX is a backend tool: its readers are automation authors and platform engineers first, and developers typing commands second (`product-context.md` § Target Users). Shape `cli`: the first-steps exit condition is one printed value from a tool that was fetched and run. The docs have three tiers and one rule about where reference sits.

| Tier | Reader state | Goals | Where they live |
|---|---|---|---|
| first-steps | nothing installed, no concepts | G01 to G06 | `start` sidebar group: install, first package, first project |
| everyday | working user, repeats tasks | G07 to G32 (14 consumer, 5 upkeep and troubleshooting, 7 publisher) | `packages`, `projects`, `settings`, `troubleshooting`, `publishing` groups |
| integration | wiring ocx into CI, containers, build systems, other programs | G33 to G40 | `integrations` group |

Reference and explanation sit outside the tiers: they are addressable from every tier and gate no tier. The `concepts` group holds explanation pages, the `reference` group holds lookup pages, and both are linked from task pages instead of being read in order.

## 2. Method record

- **Candidates** came from the CLI surface, the product-context use cases and the issue titles named in `research_docs-use-case-inventory.md`, never from page titles. The research file keeps only a summary of the 40 goals (6 first-steps, 26 everyday, 8 integration; its full table is "reproduced by re-running the lane prompt"). The 40 rows below are that table re-derived from the surface and carry the same tier counts; they are the coverage baseline for the phase-8 use-case sweep.
- **Signal** is `friction-log-severity`. No telemetry or search log was available, and no live tracker sweep was run. Three friction logs were run (below), so only the goals they cover carry a rank; the other goals are ordered by tier and dependency, not ranked.
- **Friction logs** were written by three subagents restricted to a Markdown snapshot of the published docs and a scratch `OCX_HOME`, against the real binary (ocx 0.6.5). They are first-time-persona runs with verbatim output, not a stand-in for readers.
- **Limit of the logs:** the snapshot lacked the build-generated example scripts (`<<< @/_scripts/*.sh` includes; the directory is generated at docs build time and git-ignored), so every command block that comes from an include was invisible to the persona. Findings about missing commands in Getting Started are an artifact of that, not a docs defect, and are left out of the table below.


### Surface map (every top-level command)

| Command | Goals |
|---|---|
| `ocx env` | G02, G08, G33, G38 |
| `ocx add, remove, init` | G04, G12 |
| `ocx lock` | G04, G10 |
| `ocx update, upgrade` | G13 |
| `ocx pull` | G04, G05 |
| `ocx exec` | G05 |
| `ocx status` | G10 |
| `ocx inspect` | G09 |
| `ocx clean` | G21 |
| `ocx login, logout` | G15 |
| `ocx config` | G18, G19 |
| `ocx index` | G16, G23 |
| `ocx direnv` | G06, G08 |
| `ocx shell` | G06, G22 |
| `ocx self` | G01, G14 |
| `ocx patch` | G24 |
| `ocx package` | G02, G03, G07, G09, G20, G21, G26 to G32, G40 |
| `ocx about, version, shell completion` | out of scope X01, X02 |

### Friction findings that shape the IA

| Log | Task | Steps that blocked | Findings carried into the plan |
|---|---|---|---|
| [T03](../../.agents/docs-plan/friction-logs/T03-ci-setup.md) | G33, G34 | 2 | No page covers running unattended; the facts are spread over three reference pages and never say which to set. GitLab `script:` jobs are not covered, only the experimental step runner. `user-guide.md` calls a CI export command "a deferred extension point" while `in-depth/ci.md` documents `ocx env --ci`. `setup-ocx` is shown with a literal `<sha>` placeholder. Examples with `node:20` fail with exit 79. |
| [T01](../../.agents/docs-plan/friction-logs/T01-first-package.md) | G02, G03 | 1 | The docs' running example `kitware/cmake:3.28` is not in the index (exit 79), so the first tutorial step fails. Global flag position (`--remote` before the command) is stated nowhere the persona read. The default registry (`ocx.sh`) is stated in no sentence. No page says how to call the selected tool by bare name after install. |
| [T02](../../.agents/docs-plan/friction-logs/T02-project-toolchain.md) | G04, G05 | 0 | Commands worked. The persona found no way to confirm the child process used the locked tools, saw exit 65 documented for four unrelated causes, and found no ignore file for the `.ocx/` directory the pages say is ignored. |

Rank by severity (blocked steps, then contradictions): T03 first, T01 second, T02 third. Consequences: `integration/unattended-ci.md` and `integration/registry-auth-recipes.md` are `write` actions; `getting-started/first-package.md` must use a package that resolves; the two CI pages merge three sources that disagree.

## 3. Task list (40 goals, three tiers)

One YAML row per goal in the `docs-plan` schema. `existing_page` names old paths under `website/src/docs/`; `pages` names new paths under `website/src/content/docs/`. `page_word_count` is the stripped-prose count of the existing page as a whole (a goal served by one section of a large page carries the whole page's count). `rank` is set only where a friction log ran.

```yaml
tasks:
  - id: G01
    task: "get ocx onto a machine that has nothing yet"
    source: cli-subcommand
    signal: friction-log-severity
    rank: null
    tier: first-steps
    depends_on: []
    friction_log: null
    user_need:
      as_a: "a developer on a fresh Linux, macOS or Windows machine"
      i_need_to: "get a working copy of the tool onto the machine, with or without running a downloaded script"
      so_that: "I can start fetching the tools I actually need"
    solution_shaped: false
    existing_page: "installation.md; user-guide.md#install-bare-binary"
    page_word_count: 1659
    page_status: duplicate
    action: merge
    new_pages: ["getting-started/install-ocx.mdx"]
  - id: G02
    task: "fetch a first tool package and run it"
    source: cli-subcommand
    signal: friction-log-severity
    rank: 2
    tier: first-steps
    depends_on: []
    friction_log: .agents/docs-plan/friction-logs/T01-first-package.md
    user_need:
      as_a: "a developer who has never used an OCI-based package manager"
      i_need_to: "get one real tool from a registry and run it on my machine"
      so_that: "I can see that the tool works before I learn the concepts"
    solution_shaped: false
    existing_page: "getting-started.md; user-guide.md#install"
    page_word_count: 1558
    page_status: adequate
    action: expand
    new_pages: ["getting-started/first-package.mdx"]
  - id: G03
    task: "switch between two installed versions of a tool and remove one"
    source: cli-subcommand
    signal: friction-log-severity
    rank: 2
    tier: first-steps
    depends_on: []
    friction_log: .agents/docs-plan/friction-logs/T01-first-package.md
    user_need:
      as_a: "a developer with one tool installed"
      i_need_to: "change which version my shell uses and clean out a version I no longer want"
      so_that: "I can try a new release without losing the old one"
    solution_shaped: false
    existing_page: "getting-started.md#switching-versions"
    page_word_count: 1558
    page_status: adequate
    action: keep
    new_pages: ["getting-started/first-package.mdx"]
  - id: G04
    task: "pin the tools one repository needs"
    source: cli-subcommand
    signal: friction-log-severity
    rank: 3
    tier: first-steps
    depends_on: []
    friction_log: .agents/docs-plan/friction-logs/T02-project-toolchain.md
    user_need:
      as_a: "a platform engineer who wants every teammate to get the same tool versions"
      i_need_to: "write down the tools and versions a repository needs and lock them"
      so_that: "everyone who clones it gets identical tools"
    solution_shaped: false
    existing_page: "getting-started.md#project-toolchain"
    page_word_count: 1558
    page_status: adequate
    action: expand
    new_pages: ["getting-started/first-project.mdx"]
  - id: G05
    task: "run one command with exactly the pinned tools"
    source: cli-subcommand
    signal: friction-log-severity
    rank: 3
    tier: first-steps
    depends_on: []
    friction_log: .agents/docs-plan/friction-logs/T02-project-toolchain.md
    user_need:
      as_a: "a developer in a repository that pins its tools"
      i_need_to: "run a build or test command with the repository's tools, without installing them globally"
      so_that: "my result does not depend on what else is on my machine"
    solution_shaped: false
    existing_page: "getting-started.md#project-toolchain; user-guide.md#exec"
    page_word_count: 1558
    page_status: adequate
    action: expand
    new_pages: ["getting-started/first-project.mdx", "guides/run-project-tools.mdx"]
  - id: G06
    task: "have the project tools appear in my shell when I enter the directory"
    source: cli-subcommand
    signal: friction-log-severity
    rank: 3
    tier: first-steps
    depends_on: []
    friction_log: .agents/docs-plan/friction-logs/T02-project-toolchain.md
    user_need:
      as_a: "a developer who works in the same repositories every day"
      i_need_to: "get the pinned tools on my search path automatically while I am inside the repository"
      so_that: "I stop prefixing every command and still use the pinned versions"
    solution_shaped: false
    existing_page: "getting-started.md#project-toolchain; in-depth/shell-integration.md"
    page_word_count: 1558
    page_status: adequate
    action: expand
    new_pages: ["getting-started/first-project.mdx", "explanation/shell-integration.mdx"]
  - id: G07
    task: "choose among versions, variants and exact digests"
    source: cli-subcommand
    signal: friction-log-severity
    rank: null
    tier: everyday
    depends_on: []
    friction_log: null
    user_need:
      as_a: "an engineer who needs a specific build of a tool"
      i_need_to: "pick a release by tag, by variant or by exact content"
      so_that: "my installs match what I tested"
    solution_shaped: false
    existing_page: "user-guide.md#versions"
    page_word_count: 14008
    page_status: adequate
    action: move
    new_pages: ["guides/choose-versions.mdx"]
  - id: G08
    task: "give an editor or script a path that survives upgrades"
    source: cli-subcommand
    signal: friction-log-severity
    rank: null
    tier: everyday
    depends_on: []
    friction_log: null
    user_need:
      as_a: "an engineer configuring an editor, a build system or a service"
      i_need_to: "point a program at a tool location that does not change when the version changes"
      so_that: "I do not have to edit configuration after every update"
    solution_shaped: false
    existing_page: "user-guide.md#stable-paths"
    page_word_count: 14008
    page_status: adequate
    action: move
    new_pages: ["guides/stable-paths.mdx"]
  - id: G09
    task: "run a tool together with the tools it depends on"
    source: cli-subcommand
    signal: friction-log-severity
    rank: null
    tier: everyday
    depends_on: []
    friction_log: null
    user_need:
      as_a: "an engineer using a tool that needs a runtime or a plugin set"
      i_need_to: "see and use the dependency tree of a package"
      so_that: "the tool runs against the right runtime without manual setup"
    solution_shaped: false
    existing_page: "user-guide.md#dependencies"
    page_word_count: 14008
    page_status: adequate
    action: move
    new_pages: ["guides/run-with-dependencies.mdx"]
  - id: G10
    task: "reproduce a build from a lock file"
    source: cli-subcommand
    signal: friction-log-severity
    rank: null
    tier: everyday
    depends_on: []
    friction_log: null
    user_need:
      as_a: "a maintainer whose builds must give the same tools on every run"
      i_need_to: "freeze tool versions into a file and refuse to drift from it"
      so_that: "an old commit builds with the tools it was built with"
    solution_shaped: false
    existing_page: "user-guide.md#lock; user-guide.md#locked-frozen-equivalents"
    page_word_count: 14008
    page_status: adequate
    action: move
    new_pages: ["guides/lock-and-reproduce.mdx"]
  - id: G11
    task: "keep a personal set of tools available everywhere"
    source: cli-subcommand
    signal: friction-log-severity
    rank: null
    tier: everyday
    depends_on: []
    friction_log: null
    user_need:
      as_a: "an engineer who uses the same tools in every directory"
      i_need_to: "maintain a machine-wide set of tools outside any repository"
      so_that: "my everyday tools are present in every shell"
    solution_shaped: false
    existing_page: "user-guide.md#global-toolchain"
    page_word_count: 14008
    page_status: adequate
    action: move
    new_pages: ["guides/global-toolchain.mdx"]
  - id: G12
    task: "group tools and set project environment values"
    source: cli-subcommand
    signal: friction-log-severity
    rank: null
    tier: everyday
    depends_on: []
    friction_log: null
    user_need:
      as_a: "a maintainer of a repository with build, test and release tools"
      i_need_to: "split the tool list into groups and add project environment values"
      so_that: "each job loads only what it needs"
    solution_shaped: false
    existing_page: "user-guide.md#project"
    page_word_count: 14008
    page_status: adequate
    action: move
    new_pages: ["guides/project-toolchain.mdx"]
  - id: G13
    task: "update the pinned tools to newer versions"
    source: cli-subcommand
    signal: friction-log-severity
    rank: null
    tier: everyday
    depends_on: []
    friction_log: null
    user_need:
      as_a: "a maintainer who must keep tools current"
      i_need_to: "move pinned tools to newer releases while keeping the pins I want fixed"
      so_that: "I take upgrades on purpose, not by accident"
    solution_shaped: false
    existing_page: "user-guide.md#project (lifecycle commands); in-depth/project.md#pin-preservation"
    page_word_count: 14008
    page_status: stub
    action: write
    new_pages: ["guides/update-tools.mdx"]
  - id: G14
    task: "update or remove ocx itself"
    source: cli-subcommand
    signal: friction-log-severity
    rank: null
    tier: everyday
    depends_on: []
    friction_log: null
    user_need:
      as_a: "an engineer with an older install"
      i_need_to: "move to a newer release of the tool or remove it cleanly"
      so_that: "I get fixes without reinstalling by hand"
    solution_shaped: false
    existing_page: "user-guide.md#update-ocx; installation.md#updating"
    page_word_count: 14008
    page_status: duplicate
    action: merge
    new_pages: ["guides/update-ocx.mdx"]
  - id: G15
    task: "authenticate to a private registry"
    source: cli-subcommand
    signal: friction-log-severity
    rank: null
    tier: everyday
    depends_on: []
    friction_log: null
    user_need:
      as_a: "an engineer whose tools live in a private registry"
      i_need_to: "give the tool credentials for a registry in a way that suits my machine"
      so_that: "I can fetch private packages without exposing secrets"
    solution_shaped: false
    existing_page: "user-guide.md#authentication"
    page_word_count: 14008
    page_status: adequate
    action: move
    new_pages: ["guides/authenticate.mdx"]
  - id: G16
    task: "work without network access"
    source: cli-subcommand
    signal: friction-log-severity
    rank: null
    tier: everyday
    depends_on: []
    friction_log: null
    user_need:
      as_a: "an engineer on a laptop, a plane or a locked-down network"
      i_need_to: "use already fetched tools and refresh the local catalogue only when I choose"
      so_that: "my builds do not fail because the network is gone"
    solution_shaped: false
    existing_page: "user-guide.md#offline"
    page_word_count: 14008
    page_status: adequate
    action: move
    new_pages: ["guides/work-offline.mdx", "guides/serve-an-index.md"]
  - id: G17
    task: "send registry traffic through a corporate mirror"
    source: cli-subcommand
    signal: friction-log-severity
    rank: null
    tier: everyday
    depends_on: []
    friction_log: null
    user_need:
      as_a: "a platform engineer in a company that forbids direct registry access"
      i_need_to: "route all reads through an internal mirror"
      so_that: "tools install inside the company network without opening egress"
    solution_shaped: false
    existing_page: "user-guide.md#mirrors"
    page_word_count: 14008
    page_status: adequate
    action: move
    new_pages: ["guides/corporate-mirror.mdx"]
  - id: G18
    task: "change the tool's defaults with a configuration file"
    source: cli-subcommand
    signal: friction-log-severity
    rank: null
    tier: everyday
    depends_on: []
    friction_log: null
    user_need:
      as_a: "an engineer who wants a different default registry or behaviour"
      i_need_to: "set defaults once at the system, user or project level"
      so_that: "I stop repeating options on every run"
    solution_shaped: false
    existing_page: "user-guide.md#configuration; in-depth/configuration.md"
    page_word_count: 14008
    page_status: adequate
    action: move
    new_pages: ["guides/configure-ocx.mdx"]
  - id: G19
    task: "manage one configuration for a whole fleet"
    source: cli-subcommand
    signal: friction-log-severity
    rank: null
    tier: everyday
    depends_on: []
    friction_log: null
    user_need:
      as_a: "an operator who runs many developer machines and CI hosts"
      i_need_to: "publish one configuration and roll it out, pause it or roll it back"
      so_that: "every host follows the same policy without visiting each one"
    solution_shaped: false
    existing_page: "user-guide.md#managed-config"
    page_word_count: 14008
    page_status: adequate
    action: move
    new_pages: ["guides/manage-config-centrally.mdx"]
  - id: G20
    task: "check who signed what I install"
    source: cli-subcommand
    signal: friction-log-severity
    rank: null
    tier: everyday
    depends_on: []
    friction_log: null
    user_need:
      as_a: "a security-minded engineer"
      i_need_to: "verify a package signature and require specific signers"
      so_that: "I only run software from identities I trust"
    solution_shaped: false
    existing_page: "user-guide.md#supply-chain; in-depth/signing.md"
    page_word_count: 14008
    page_status: duplicate
    action: merge
    new_pages: ["guides/verify-installs.mdx", "guides/pin-signer-identity.mdx", "guides/self-hosted-sigstore.md"]
  - id: G21
    task: "remove packages and reclaim disk space"
    source: cli-subcommand
    signal: friction-log-severity
    rank: null
    tier: everyday
    depends_on: []
    friction_log: null
    user_need:
      as_a: "an engineer whose disk is filling up"
      i_need_to: "remove what I no longer use and delete what nothing refers to"
      so_that: "I keep the machine small without breaking active projects"
    solution_shaped: false
    existing_page: "user-guide.md#cleanup; getting-started.md#uninstalling"
    page_word_count: 14008
    page_status: adequate
    action: move
    new_pages: ["guides/clean-up.mdx"]
  - id: G22
    task: "find out why my shell does not pick up the tools"
    source: cli-subcommand
    signal: friction-log-severity
    rank: null
    tier: everyday
    depends_on: []
    friction_log: null
    user_need:
      as_a: "an engineer whose project tools are missing from the shell"
      i_need_to: "inspect the shell integration state and repair it"
      so_that: "the tools appear again without restarting everything"
    solution_shaped: false
    existing_page: "in-depth/shell-integration.md#diagnostics"
    page_word_count: 5990
    page_status: adequate
    action: move
    new_pages: ["troubleshooting/shell.mdx"]
  - id: G23
    task: "find which packages and versions exist"
    source: cli-subcommand
    signal: friction-log-severity
    rank: null
    tier: everyday
    depends_on: []
    friction_log: null
    user_need:
      as_a: "an engineer who does not know the exact package name"
      i_need_to: "list the repositories and tags a registry or index offers"
      so_that: "I can choose a package without leaving the terminal"
    solution_shaped: false
    existing_page: "user-guide.md#namespaces; in-depth/indices.md"
    page_word_count: 14008
    page_status: stub
    action: write
    new_pages: ["guides/find-packages.mdx"]
  - id: G24
    task: "apply infrastructure patches to packages I consume"
    source: cli-subcommand
    signal: friction-log-severity
    rank: null
    tier: everyday
    depends_on: []
    friction_log: null
    user_need:
      as_a: "a platform engineer who must adapt third-party tools to company infrastructure"
      i_need_to: "layer company-specific changes onto packages without rebuilding them"
      so_that: "tools work inside our network and policy"
    solution_shaped: false
    existing_page: "user-guide/patches.md"
    page_word_count: 4079
    page_status: adequate
    action: move
    new_pages: ["guides/patch-packages.mdx"]
  - id: G25
    task: "work out why a package will not run on my operating system"
    source: cli-subcommand
    signal: friction-log-severity
    rank: null
    tier: everyday
    depends_on: []
    friction_log: null
    user_need:
      as_a: "an engineer on Alpine, NixOS, macOS or Windows"
      i_need_to: "learn what the platform needs and which workaround applies"
      so_that: "I get the tool running on my machine"
    solution_shaped: false
    existing_page: "faq.md#linux; faq.md#macos; faq.md#windows"
    page_word_count: 2612
    page_status: adequate
    action: move
    new_pages: ["troubleshooting/platforms.mdx"]
  - id: G26
    task: "publish a package to a registry"
    source: cli-subcommand
    signal: friction-log-severity
    rank: null
    tier: everyday
    depends_on: []
    friction_log: null
    user_need:
      as_a: "a tool author with a release archive"
      i_need_to: "push an archive with metadata and publish rolling tags"
      so_that: "consumers can install the release by a stable name"
    solution_shaped: false
    existing_page: "authoring/building-pushing.md; authoring/index.md"
    page_word_count: 2767
    page_status: adequate
    action: move
    new_pages: ["publishing/build-and-push.mdx", "publishing/index.mdx"]
  - id: G27
    task: "package an existing binary release for ocx"
    source: cli-subcommand
    signal: friction-log-severity
    rank: null
    tier: everyday
    depends_on: []
    friction_log: null
    user_need:
      as_a: "a maintainer repackaging upstream releases"
      i_need_to: "turn an upstream archive into a package with the right layout and metadata"
      so_that: "consumers get the upstream tool without me hand-building each release"
    solution_shaped: false
    existing_page: "authoring/bundle-anatomy.md; authoring/migration.md"
    page_word_count: 981
    page_status: adequate
    action: move
    new_pages: ["publishing/bundle-anatomy.mdx", "publishing/repackage-releases.mdx"]
  - id: G28
    task: "declare dependencies, environment values and launchers in a package"
    source: cli-subcommand
    signal: friction-log-severity
    rank: null
    tier: everyday
    depends_on: []
    friction_log: null
    user_need:
      as_a: "a tool author whose tool needs other tools"
      i_need_to: "describe what my package needs and what it exposes"
      so_that: "consumers get a working environment without writing glue"
    solution_shaped: false
    existing_page: "authoring/dependencies.md; authoring/env-surface.md; authoring/entry-points.md"
    page_word_count: 1153
    page_status: adequate
    action: move
    new_pages: ["publishing/declare-dependencies.md", "publishing/env-surface.md", "publishing/entry-points.md"]
  - id: G29
    task: "test a package before pushing it"
    source: cli-subcommand
    signal: friction-log-severity
    rank: null
    tier: everyday
    depends_on: []
    friction_log: null
    user_need:
      as_a: "a tool author iterating on a package"
      i_need_to: "run my package and a scripted check locally"
      so_that: "I catch mistakes before consumers do"
    solution_shaped: false
    existing_page: "authoring/testing.md"
    page_word_count: 2184
    page_status: adequate
    action: move
    new_pages: ["publishing/test-locally.mdx", "publishing/script-tests.md"]
  - id: G30
    task: "announce a package so others can find it"
    source: cli-subcommand
    signal: friction-log-severity
    rank: null
    tier: everyday
    depends_on: []
    friction_log: null
    user_need:
      as_a: "a publisher who wants consumers to discover a package"
      i_need_to: "claim a name in an index and announce new tags"
      so_that: "consumers can find and trust the package"
    solution_shaped: false
    existing_page: "authoring/announcing.md"
    page_word_count: 3040
    page_status: adequate
    action: move
    new_pages: ["publishing/announce.md", "publishing/announce-from-ci.md"]
  - id: G31
    task: "publish one package for several platforms and C libraries"
    source: cli-subcommand
    signal: friction-log-severity
    rank: null
    tier: everyday
    depends_on: []
    friction_log: null
    user_need:
      as_a: "a tool author supporting several operating systems"
      i_need_to: "push per-platform builds under one name, including glibc and musl variants"
      so_that: "each consumer gets the build that matches their host"
    solution_shaped: false
    existing_page: "authoring/multi-platform.md"
    page_word_count: 2844
    page_status: adequate
    action: move
    new_pages: ["publishing/multi-platform.mdx", "publishing/libc-variants.md"]
  - id: G32
    task: "sign a release and attach an SBOM"
    source: cli-subcommand
    signal: friction-log-severity
    rank: null
    tier: everyday
    depends_on: []
    friction_log: null
    user_need:
      as_a: "a publisher whose consumers verify supply-chain evidence"
      i_need_to: "sign each release and attach attestations"
      so_that: "consumers can verify who built it and what is in it"
    solution_shaped: false
    existing_page: "user-guide.md#supply-chain-signing; in-depth/signing.md#ci; user-guide/attestations.md"
    page_word_count: 14008
    page_status: adequate
    action: write
    new_pages: ["publishing/sign-a-release.md", "publishing/attest-and-sbom.mdx"]
  - id: G33
    task: "provide pinned tools in a GitHub Actions or GitLab CI job"
    source: readme-heading
    signal: friction-log-severity
    rank: 1
    tier: integration
    depends_on: []
    friction_log: .agents/docs-plan/friction-logs/T03-ci-setup.md
    user_need:
      as_a: "a CI maintainer who owns a pipeline"
      i_need_to: "put the tools a job needs on the search path for the current and later steps"
      so_that: "pipelines run the same tools as developers"
    solution_shaped: false
    existing_page: "in-depth/ci.md; user-guide.md#ci"
    page_word_count: 953
    page_status: duplicate
    action: merge
    new_pages: ["integration/github-actions.md", "integration/gitlab-ci.md"]
  - id: G34
    task: "run ocx unattended in a pipeline"
    source: issue-title
    signal: friction-log-severity
    rank: 1
    tier: integration
    depends_on: []
    friction_log: .agents/docs-plan/friction-logs/T03-ci-setup.md
    user_need:
      as_a: "a CI maintainer whose jobs cannot answer prompts"
      i_need_to: "know which switches make the tool non-interactive, offline or frozen"
      so_that: "a job never hangs and never changes the lock"
    solution_shaped: false
    existing_page: "reference/environment.md (scattered); [#400](https://github.com/ocx-sh/ocx/issues/400) and [#323](https://github.com/ocx-sh/ocx/issues/323) in the research"
    page_word_count: 16148
    page_status: missing
    action: write
    new_pages: ["integration/unattended-ci.md", "integration/registry-auth-recipes.md"]
  - id: G35
    task: "use ocx in a Docker image build"
    source: readme-heading
    signal: friction-log-severity
    rank: null
    tier: integration
    depends_on: []
    friction_log: null
    user_need:
      as_a: "an engineer building container images"
      i_need_to: "copy the binary in and bake a project toolchain into an image"
      so_that: "images contain exactly the pinned tools"
    solution_shaped: false
    existing_page: "docker.md"
    page_word_count: 848
    page_status: adequate
    action: move
    new_pages: ["integration/docker.md"]
  - id: G36
    task: "use ocx tools from Bazel"
    source: issue-title
    signal: friction-log-severity
    rank: null
    tier: integration
    depends_on: []
    friction_log: null
    user_need:
      as_a: "a build engineer who maintains Bazel rules"
      i_need_to: "fetch a pinned tool for a Bazel build without a URL matrix per platform"
      so_that: "one rule version always uses one tool version"
    solution_shaped: false
    existing_page: "none ([#191](https://github.com/ocx-sh/ocx/issues/191) and [#192](https://github.com/ocx-sh/ocx/issues/192) in the research)"
    page_word_count: null
    page_status: missing
    action: write
    new_pages: ["integration/bazel.md"]
  - id: G37
    task: "expose package tooling to a devcontainer or an editor"
    source: readme-heading
    signal: friction-log-severity
    rank: null
    tier: integration
    depends_on: []
    friction_log: null
    user_need:
      as_a: "a developer using a devcontainer or an editor with language servers"
      i_need_to: "have the editor and container find tools and tool settings from packages"
      so_that: "the editor works the same on every machine"
    solution_shaped: false
    existing_page: "none (integrations metadata is described in reference/metadata.md)"
    page_word_count: null
    page_status: missing
    action: write
    new_pages: ["integration/devcontainers-and-editors.md"]
  - id: G38
    task: "consume ocx output from scripts and other programs"
    source: cli-subcommand
    signal: friction-log-severity
    rank: null
    tier: integration
    depends_on: []
    friction_log: null
    user_need:
      as_a: "an automation author calling the tool from scripts and programs"
      i_need_to: "read structured output, error documents and exit codes, and check them against schemas"
      so_that: "my automation reacts correctly to every outcome"
    solution_shaped: false
    existing_page: "reference/machine-interface.md; reference/execution-records.md"
    page_word_count: 1053
    page_status: adequate
    action: expand
    new_pages: ["integration/scripts-and-json.md", "reference/machine-interface.md", "explanation/execution-records.md"]
  - id: G39
    task: "turn a Python lock file into packages"
    source: issue-title
    signal: friction-log-severity
    rank: null
    tier: integration
    depends_on: []
    friction_log: null
    user_need:
      as_a: "a Python team that wants locked dependencies delivered like other tools"
      i_need_to: "translate a lock file into packages the tool can install"
      so_that: "Python environments are reproducible with the same machinery"
    solution_shaped: false
    existing_page: "none (ocx_python crate only)"
    page_word_count: null
    page_status: missing
    action: write
    new_pages: ["integration/python.md"]
  - id: G40
    task: "promote builds and run snapshot tracks from a release pipeline"
    source: cli-subcommand
    signal: friction-log-severity
    rank: null
    tier: integration
    depends_on: []
    friction_log: null
    user_need:
      as_a: "a release engineer moving builds through environments"
      i_need_to: "copy a tested build forward and keep per-change snapshots"
      so_that: "production gets exactly what passed testing"
    solution_shaped: false
    existing_page: "user-guide/promoting-packages.md; user-guide/snapshot-tracks.md; [#108](https://github.com/ocx-sh/ocx/issues/108) in the research"
    page_word_count: 1017
    page_status: adequate
    action: expand
    new_pages: ["publishing/promote-packages.mdx", "publishing/snapshot-tracks.md", "integration/publisher-ci.md"]
```

## 4. Coverage against the 40 goals

Verdict reads: `move` the content already exists in one place and travels with its page, `merge` several sources collapse into the new page, `expand` the page exists and misses material, `write` the page is new, `keep` no change beyond the re-layout.

| Goal | Tier | Task | New page(s) | Today | Verdict |
|---|---|---|---|---|---|
| G01 | first-steps | get ocx onto a machine that has nothing yet | `getting-started/install-ocx.md` | installation.md; user-guide.md#install-bare-binary (duplicate) | merge |
| G02 | first-steps | fetch a first tool package and run it | `getting-started/first-package.md` | getting-started.md; user-guide.md#install (adequate) | expand |
| G03 | first-steps | switch between two installed versions of a tool and remove one | `getting-started/first-package.md` | getting-started.md#switching-versions (adequate) | keep |
| G04 | first-steps | pin the tools one repository needs | `getting-started/first-project.md` | getting-started.md#project-toolchain (adequate) | expand |
| G05 | first-steps | run one command with exactly the pinned tools | `getting-started/first-project.md`, `guides/run-project-tools.md` | getting-started.md#project-toolchain; user-guide.md#exec (adequate) | expand |
| G06 | first-steps | have the project tools appear in my shell when I enter the directory | `getting-started/first-project.md`, `explanation/shell-integration.md` | getting-started.md#project-toolchain; in-depth/shell-integration.md (adequate) | expand |
| G07 | everyday | choose among versions, variants and exact digests | `guides/choose-versions.md` | user-guide.md#versions (adequate) | move |
| G08 | everyday | give an editor or script a path that survives upgrades | `guides/stable-paths.md` | user-guide.md#stable-paths (adequate) | move |
| G09 | everyday | run a tool together with the tools it depends on | `guides/run-with-dependencies.md` | user-guide.md#dependencies (adequate) | move |
| G10 | everyday | reproduce a build from a lock file | `guides/lock-and-reproduce.md` | user-guide.md#lock; user-guide.md#locked-frozen-equivalents (adequate) | move |
| G11 | everyday | keep a personal set of tools available everywhere | `guides/global-toolchain.md` | user-guide.md#global-toolchain (adequate) | move |
| G12 | everyday | group tools and set project environment values | `guides/project-toolchain.md` | user-guide.md#project (adequate) | move |
| G13 | everyday | update the pinned tools to newer versions | `guides/update-tools.md` | user-guide.md#project (lifecycle commands); in-depth/project.md#pin-preservation (stub) | write |
| G14 | everyday | update or remove ocx itself | `guides/update-ocx.md` | user-guide.md#update-ocx; installation.md#updating (duplicate) | merge |
| G15 | everyday | authenticate to a private registry | `guides/authenticate.md` | user-guide.md#authentication (adequate) | move |
| G16 | everyday | work without network access | `guides/work-offline.md`, `guides/serve-an-index.md` | user-guide.md#offline (adequate) | move |
| G17 | everyday | send registry traffic through a corporate mirror | `guides/corporate-mirror.md` | user-guide.md#mirrors (adequate) | move |
| G18 | everyday | change the tool's defaults with a configuration file | `guides/configure-ocx.md` | user-guide.md#configuration; in-depth/configuration.md (adequate) | move |
| G19 | everyday | manage one configuration for a whole fleet | `guides/manage-config-centrally.md` | user-guide.md#managed-config (adequate) | move |
| G20 | everyday | check who signed what I install | `guides/verify-installs.md`, `guides/pin-signer-identity.md`, `guides/self-hosted-sigstore.md` | user-guide.md#supply-chain; in-depth/signing.md (duplicate) | merge |
| G21 | everyday | remove packages and reclaim disk space | `guides/clean-up.md` | user-guide.md#cleanup; getting-started.md#uninstalling (adequate) | move |
| G22 | everyday | find out why my shell does not pick up the tools | `troubleshooting/shell.md` | in-depth/shell-integration.md#diagnostics (adequate) | move |
| G23 | everyday | find which packages and versions exist | `guides/find-packages.md` | user-guide.md#namespaces; in-depth/indices.md (stub) | write |
| G24 | everyday | apply infrastructure patches to packages I consume | `guides/patch-packages.md` | user-guide/patches.md (adequate) | move |
| G25 | everyday | work out why a package will not run on my operating system | `troubleshooting/platforms.md` | faq.md#linux; faq.md#macos; faq.md#windows (adequate) | move |
| G26 | everyday | publish a package to a registry | `publishing/build-and-push.md`, `publishing/index.md` | authoring/building-pushing.md; authoring/index.md (adequate) | move |
| G27 | everyday | package an existing binary release for ocx | `publishing/bundle-anatomy.md`, `publishing/repackage-releases.md` | authoring/bundle-anatomy.md; authoring/migration.md (adequate) | move |
| G28 | everyday | declare dependencies, environment values and launchers in a package | `publishing/declare-dependencies.md`, `publishing/env-surface.md`, `publishing/entry-points.md` | authoring/dependencies.md; authoring/env-surface.md; authoring/entry-points.md (adequate) | move |
| G29 | everyday | test a package before pushing it | `publishing/test-locally.md`, `publishing/script-tests.md` | authoring/testing.md (adequate) | move |
| G30 | everyday | announce a package so others can find it | `publishing/announce.md`, `publishing/announce-from-ci.md` | authoring/announcing.md (adequate) | move |
| G31 | everyday | publish one package for several platforms and C libraries | `publishing/multi-platform.md`, `publishing/libc-variants.md` | authoring/multi-platform.md (adequate) | move |
| G32 | everyday | sign a release and attach an SBOM | `publishing/sign-a-release.md`, `publishing/attest-and-sbom.md` | user-guide.md#supply-chain-signing; in-depth/signing.md#ci; user-guide/attestations.md (adequate) | write |
| G33 | integration | provide pinned tools in a GitHub Actions or GitLab CI job | `integration/github-actions.md`, `integration/gitlab-ci.md` | in-depth/ci.md; user-guide.md#ci (duplicate) | merge |
| G34 | integration | run ocx unattended in a pipeline | `integration/unattended-ci.md`, `integration/registry-auth-recipes.md` | reference/environment.md (scattered); [#400](https://github.com/ocx-sh/ocx/issues/400) and [#323](https://github.com/ocx-sh/ocx/issues/323) in the research (missing) | write |
| G35 | integration | use ocx in a Docker image build | `integration/docker.md` | docker.md (adequate) | move |
| G36 | integration | use ocx tools from Bazel | `integration/bazel.md` | none ([#191](https://github.com/ocx-sh/ocx/issues/191) and [#192](https://github.com/ocx-sh/ocx/issues/192) in the research) (missing) | write |
| G37 | integration | expose package tooling to a devcontainer or an editor | `integration/devcontainers-and-editors.md` | none (integrations metadata is described in reference/metadata.md) (missing) | write |
| G38 | integration | consume ocx output from scripts and other programs | `integration/scripts-and-json.md`, `reference/machine-interface.md`, `explanation/execution-records.md` | reference/machine-interface.md; reference/execution-records.md (adequate) | expand |
| G39 | integration | turn a Python lock file into packages | `integration/python.md` | none (ocx_python crate only) (missing) | write |
| G40 | integration | promote builds and run snapshot tracks from a release pipeline | `publishing/promote-packages.md`, `publishing/snapshot-tracks.md`, `integration/publisher-ci.md` | user-guide/promoting-packages.md; user-guide/snapshot-tracks.md; [#108](https://github.com/ocx-sh/ocx/issues/108) in the research (adequate) | expand |

Gap goals (feature check required before writing; ocx side only, link `/integrations/<x>/` where the integration lives in its own repository): G13 update tools (a stub today), G23 find packages, G34 unattended CI, G36 Bazel, G37 devcontainers and editors, G39 Python lock translation. P13e confirms in the code that each feature exists and records "feature absent" in its ledger instead of writing the page when it does not; G39 is the doubtful one (`ocx_python` is a crate, no CLI verb was found in `cli.json`).

Pages mapped to no goal (candidates for the delete test only): `website/src/index.md`, `website/src/team.md` (root site), `roadmap.md`, `changelog.md`, `contributing/bazel.md`. The last three are project pages that serve contributors and release readers, not the 40 reader goals, and stay.

## 5. Page inventory (old path to new path)

Columns `old | new | doc_type | doc_tier | cluster | sidebar`. `old` is relative to `website/src/docs/`, `new` to `website/src/content/docs/`; `—` in `old` means a new page. `doc_tier` is `—` for types that carry no tier. No page appears twice in `new`. URL of a new page: `/docs/<new without .md>/`, with `index.md` at the directory URL and `trailingSlash: always`.

Cluster rule: a page belongs to the cluster that owns the old page it is split from (so splits never span two writers); a gap page belongs to the cluster of its tier; hand-written reference pages belong to `misc`; pages written by the P3 generator are `generated`. Exceptions that the owning-page rule produces are visible in the table, for example `guides/self-hosted-sigstore.md` is a how-to page in cluster `explanation`.

| old | new | doc_type | doc_tier | cluster | sidebar |
|---|---|---|---|---|---|
| — | index.mdx | landing | first-steps | first-steps | start |
| installation.mdx | getting-started/install-ocx.mdx | how-to | first-steps | first-steps | start |
| getting-started.mdx | getting-started/first-package.mdx | tutorial | first-steps | first-steps | start |
| getting-started.mdx | getting-started/first-project.mdx | tutorial | first-steps | first-steps | start |
| user-guide.mdx | guides/choose-versions.mdx | how-to | everyday | everyday | packages |
| — | guides/find-packages.mdx | how-to | everyday | everyday | packages |
| user-guide.mdx | guides/run-with-dependencies.mdx | how-to | everyday | everyday | packages |
| user-guide.mdx | guides/stable-paths.mdx | how-to | everyday | everyday | packages |
| user-guide/patches.mdx | guides/patch-packages.mdx | how-to | everyday | everyday | packages |
| user-guide.mdx | guides/clean-up.mdx | how-to | everyday | everyday | packages |
| user-guide.mdx | guides/project-toolchain.mdx | how-to | everyday | everyday | projects |
| user-guide.mdx | guides/run-project-tools.mdx | how-to | everyday | everyday | projects |
| user-guide.mdx | guides/lock-and-reproduce.mdx | how-to | everyday | everyday | projects |
| user-guide.mdx | guides/update-tools.mdx | how-to | everyday | everyday | projects |
| user-guide.mdx | guides/global-toolchain.mdx | how-to | everyday | everyday | projects |
| user-guide.mdx | guides/configure-ocx.mdx | how-to | everyday | everyday | settings |
| user-guide.mdx | guides/authenticate.mdx | how-to | everyday | everyday | settings |
| user-guide.mdx | guides/work-offline.mdx | how-to | everyday | everyday | settings |
| user-guide.mdx | guides/corporate-mirror.mdx | how-to | everyday | everyday | settings |
| user-guide.mdx | guides/manage-config-centrally.mdx | how-to | everyday | everyday | settings |
| user-guide.mdx | guides/verify-installs.mdx | how-to | everyday | everyday | settings |
| user-guide.mdx | guides/pin-signer-identity.mdx | how-to | everyday | everyday | settings |
| in-depth/self-hosted-sigstore.md | guides/self-hosted-sigstore.md | how-to | everyday | explanation | settings |
| in-depth/indices.md | guides/serve-an-index.md | how-to | everyday | explanation | settings |
| user-guide.mdx | guides/update-ocx.mdx | how-to | everyday | everyday | settings |
| in-depth/shell-integration.mdx | troubleshooting/shell.mdx | troubleshooting | — | explanation | troubleshooting |
| faq.mdx | troubleshooting/platforms.mdx | troubleshooting | — | misc | troubleshooting |
| authoring/index.mdx | publishing/index.mdx | landing | everyday | publishing | publishing |
| authoring/bundle-anatomy.mdx | publishing/bundle-anatomy.mdx | how-to | everyday | publishing | publishing |
| authoring/dependencies.md | publishing/declare-dependencies.md | how-to | everyday | publishing | publishing |
| authoring/env-surface.md | publishing/env-surface.md | how-to | everyday | publishing | publishing |
| authoring/entry-points.md | publishing/entry-points.md | how-to | everyday | publishing | publishing |
| authoring/building-pushing.mdx | publishing/build-and-push.mdx | how-to | everyday | publishing | publishing |
| authoring/testing.mdx | publishing/test-locally.mdx | how-to | everyday | publishing | publishing |
| authoring/testing.mdx | publishing/script-tests.md | how-to | everyday | publishing | publishing |
| authoring/multi-platform.mdx | publishing/multi-platform.mdx | how-to | everyday | publishing | publishing |
| authoring/multi-platform.mdx | publishing/libc-variants.md | how-to | everyday | publishing | publishing |
| authoring/announcing.md | publishing/announce.md | how-to | everyday | publishing | publishing |
| authoring/announcing.md | publishing/announce-from-ci.md | how-to | everyday | publishing | publishing |
| — | publishing/sign-a-release.md | how-to | everyday | publishing | publishing |
| user-guide/attestations.mdx | publishing/attest-and-sbom.mdx | how-to | everyday | everyday | publishing |
| user-guide/promoting-packages.mdx | publishing/promote-packages.mdx | how-to | everyday | everyday | publishing |
| user-guide/snapshot-tracks.md | publishing/snapshot-tracks.md | how-to | everyday | everyday | publishing |
| user-guide/patches.mdx | publishing/maintain-patches.mdx | how-to | everyday | everyday | publishing |
| authoring/migration.mdx | publishing/repackage-releases.mdx | how-to | everyday | publishing | publishing |
| — | integration/index.md | landing | integration | integration | integrations |
| in-depth/ci.md | integration/github-actions.md | how-to | integration | integration | integrations |
| in-depth/ci.md | integration/gitlab-ci.md | how-to | integration | integration | integrations |
| — | integration/unattended-ci.md | how-to | integration | integration | integrations |
| — | integration/registry-auth-recipes.md | how-to | integration | integration | integrations |
| — | integration/proxy-and-ca.md | how-to | integration | integration | integrations |
| docker.md | integration/docker.md | how-to | integration | integration | integrations |
| — | integration/bazel.md | how-to | integration | integration | integrations |
| — | integration/devcontainers-and-editors.md | how-to | integration | integration | integrations |
| — | integration/scripts-and-json.md | how-to | integration | integration | integrations |
| — | integration/python.md | how-to | integration | integration | integrations |
| — | integration/publisher-ci.md | how-to | integration | integration | integrations |
| in-depth/storage.mdx | explanation/storage.mdx | explanation | — | explanation | concepts |
| in-depth/storage.mdx | explanation/toolchain-tree.md | explanation | — | explanation | concepts |
| in-depth/versioning.mdx | explanation/versioning.mdx | explanation | — | explanation | concepts |
| in-depth/dependencies.mdx | explanation/dependencies.mdx | explanation | — | explanation | concepts |
| in-depth/indices.md | explanation/indices.md | explanation | — | explanation | concepts |
| in-depth/indices.md | explanation/local-index.md | explanation | — | explanation | concepts |
| in-depth/indices.md | explanation/public-index.md | explanation | — | explanation | concepts |
| in-depth/project.md | explanation/project-toolchain.md | explanation | — | explanation | concepts |
| in-depth/configuration.mdx | explanation/configuration.mdx | explanation | — | explanation | concepts |
| in-depth/environments.mdx | explanation/environments.mdx | explanation | — | explanation | concepts |
| — | explanation/toolchain-activation.md | explanation | — | explanation | concepts |
| in-depth/shell-integration.mdx | explanation/shell-integration.mdx | explanation | — | explanation | concepts |
| in-depth/entry-points.md | explanation/entry-points.md | explanation | — | explanation | concepts |
| in-depth/lazy-loading.mdx | explanation/deferred-packages.mdx | explanation | — | explanation | concepts |
| user-guide/patches.mdx | explanation/patches.mdx | explanation | — | everyday | concepts |
| — | explanation/integrations.md | explanation | — | explanation | concepts |
| — | explanation/execution-records.md | explanation | — | explanation | concepts |
| in-depth/signing.mdx | explanation/signing.mdx | explanation | — | explanation | concepts |
| in-depth/cosign-parity.mdx | explanation/cosign-parity.mdx | explanation | — | explanation | concepts |
| in-depth/environments.mdx | explanation/launcher-forwarding.md | explanation | — | explanation | concepts |
| in-depth/configuration.mdx | explanation/managed-configuration.md | explanation | — | explanation | concepts |
| in-depth/project.md | explanation/project-groups.md | explanation | — | explanation | concepts |
| reference/execution-records.md | explanation/records-and-sbom.md | explanation | — | explanation | concepts |
| reference/execution-records.md | explanation/records-launcher-frames.md | explanation | — | explanation | concepts |
| in-depth/shell-integration.mdx | explanation/shell-consent.mdx | explanation | — | explanation | concepts |
| in-depth/storage.mdx | explanation/toolchain-render.md | explanation | — | explanation | concepts |
| — | security.md | explanation | — | misc | project |
| reference/metadata.md | reference/metadata/index.md | reference | — | misc | reference |
| reference/metadata.md | reference/metadata/env.md | reference | — | misc | reference |
| reference/metadata.md | reference/metadata/dependencies.md | reference | — | misc | reference |
| reference/metadata.md | reference/metadata/entry-points.md | reference | — | misc | reference |
| reference/metadata.md | reference/metadata/integrations.md | reference | — | misc | reference |
| reference/env-composition.md | reference/env-composition.md | reference | — | misc | reference |
| reference/execution-records.md | reference/execution-records.md | reference | — | misc | reference |
| reference/script-host-api.md | reference/script-host-api.md | reference | — | misc | reference |
| reference/platforms.md | reference/platforms.md | reference | — | misc | reference |
| reference/machine-interface.md | reference/machine-interface.md | reference | — | misc | reference |
| reference/command-line.md | reference/command-line/index.md | reference | — | generated | reference |
| reference/command-line.md | reference/command-line/commands.md | reference | — | generated | reference |
| reference/command-line.md | reference/command-line/config.md | reference | — | generated | reference |
| reference/command-line.md | reference/command-line/direnv.md | reference | — | generated | reference |
| reference/command-line.md | reference/command-line/index-commands.md | reference | — | generated | reference |
| reference/command-line.md | reference/command-line/package-publishing.md | reference | — | generated | reference |
| reference/command-line.md | reference/command-line/package-consuming.md | reference | — | generated | reference |
| reference/command-line.md | reference/command-line/patch.md | reference | — | generated | reference |
| reference/command-line.md | reference/command-line/self.md | reference | — | generated | reference |
| reference/command-line.md | reference/command-line/shell.md | reference | — | generated | reference |
| reference/environment.md | reference/environment.md | reference | — | generated | reference |
| reference/configuration.md | reference/configuration.md | reference | — | generated | reference |
| reference/configuration.md | reference/project-file.md | reference | — | generated | reference |
| reference/command-line.md | reference/exit-codes.md | reference | — | generated | reference |
| generated (SBOM task) | reference/dependencies.md | reference | — | generated | reference |
| roadmap.md | roadmap.md | explanation | — | misc | project |
| changelog.md | changelog.md | changelog | — | misc | project |
| contributing/bazel.md | contributing/bazel.md | how-to | everyday | misc | project |

Total: 104 new pages. Per cluster: first-steps 4, everyday 24, publishing 14, explanation 21, integration 12, misc 14, generated 15.

Pages created before P12 (P9 writes them at their new path because they are the only home an overflow section has): `reference/metadata/{index,env,dependencies,entry-points,integrations}.md` (P9 splits `reference/metadata.md` and deletes it), `explanation/integrations.md`, `explanation/toolchain-activation.md`, `explanation/execution-records.md`, `integration/proxy-and-ca.md`. Every other split sibling is created by the owning P13 pipeline.

Generated reference pages follow the page names in `crates/ocx_schema/src/docs/render.rs` on the P3 branch (commit 810724e26): a group is the first word of a command that has subcommands, every leaf command lands in `commands.md`, `package` splits into `package-publishing.md` and `package-consuming.md` (a fixed table of its subcommands in the generator). P3 owns the exact set; P12's cluster-set check reads the built set, so a rename on the P3 side needs only this table updated. **One conflict to settle in P3:** the `index` group writes `reference/command-line/index.md`, the same path as the command-line landing page. This table assumes the group file is renamed to `index-commands.md`. `reference/dependencies.md` is the git-ignored SBOM page (`sbom:generate:page`), a build-time generator page that the stub test must exempt.

## 6. Merge, split and delete

### 6.1 Split and merge table

`git mv target` is the new page the old file moves to at P12 (keeps history); the siblings are created by the cluster named in the inventory. A page that appears under `old` once in the inventory is a plain move and is not listed here.

| old | git mv target | siblings created | action |
|---|---|---|---|
| getting-started.md | getting-started/first-package.md | getting-started/first-project.md | split |
| user-guide.md | guides/choose-versions.md | guides/run-with-dependencies.md, guides/stable-paths.md, guides/clean-up.md, guides/project-toolchain.md, guides/run-project-tools.md, guides/lock-and-reproduce.md, guides/update-tools.md, guides/global-toolchain.md, guides/configure-ocx.md, guides/authenticate.md, guides/work-offline.md, guides/corporate-mirror.md, guides/manage-config-centrally.md, guides/verify-installs.md, guides/pin-signer-identity.md, guides/update-ocx.md | split |
| user-guide/patches.md | guides/patch-packages.md | publishing/maintain-patches.md, explanation/patches.md | split |
| in-depth/indices.md | explanation/indices.md | guides/serve-an-index.md, explanation/local-index.md, explanation/public-index.md | split |
| in-depth/shell-integration.md | explanation/shell-integration.md | troubleshooting/shell.md | split |
| authoring/testing.md | publishing/test-locally.md | publishing/script-tests.md | split |
| authoring/multi-platform.md | publishing/multi-platform.md | publishing/libc-variants.md | split |
| authoring/announcing.md | publishing/announce.md | publishing/announce-from-ci.md | split |
| in-depth/ci.md | integration/github-actions.md | integration/gitlab-ci.md | split |
| in-depth/storage.md | explanation/storage.md | explanation/toolchain-tree.md | split |
| reference/metadata.md | reference/metadata/index.md (P9 splits; no mv) | reference/metadata/env.md, reference/metadata/dependencies.md, reference/metadata/entry-points.md, reference/metadata/integrations.md | split |
| reference/command-line.md | none (P9 deletes; generated pages replace it) | reference/command-line/index.md, reference/command-line/commands.md, reference/command-line/config.md, reference/command-line/direnv.md, reference/command-line/index-commands.md, reference/command-line/package-publishing.md, reference/command-line/package-consuming.md, reference/command-line/patch.md, reference/command-line/self.md, reference/command-line/shell.md, reference/exit-codes.md | replace by generated pages |
| reference/configuration.md | none (P9 deletes; generated pages replace it) | reference/configuration.md, reference/project-file.md | replace by generated pages |

Merges without a split (several old sources feed one new page, the owner of the destination writes the merged page): `guides/update-ocx.md` (user-guide update + installation updating and uninstalling), `getting-started/install-ocx.md` (installation + user-guide bare-binary install + faq install-version), `integration/github-actions.md` and `gitlab-ci.md` (in-depth/ci + user-guide CI + installation CI setup), `guides/verify-installs.md` (user-guide verify + in-depth/signing offline verification), `publishing/sign-a-release.md` (user-guide sign + in-depth/signing CI + building-pushing signing-after-push), `troubleshooting/platforms.md` (faq Linux, macOS, Windows). Each source section is deleted by its own owner once the destination has the content; the destination page is the only live copy.

### 6.2 Section map for split pages

Every old heading of a split page and where it lands. H3 rows appear only where they leave the parent H2's page. Dispositions: `move` (the section travels), `merge into the existing section` (destination already has related text; combine, keep every fact), `delete here, the target already carries it` (duplicate; ledger reason `duplicate`), `delete, obsolete`. These rows are the source of each P13 pipeline's fragment map (D11).

| old page | section | lines from | new page | disposition |
|---|---|---|---|---|
| user-guide.md | #install | 10 | getting-started/first-package.md | merge into the existing section |
| user-guide.md | #install-bare-binary | 32 | getting-started/install-ocx.md | merge into the existing section |
| user-guide.md | #versions | 151 | guides/choose-versions.md | move |
| user-guide.md | #namespaces | 186 | guides/find-packages.md | move |
| user-guide.md | #stable-paths | 196 | guides/stable-paths.md | move |
| user-guide.md | #dependencies | 275 | guides/run-with-dependencies.md | move |
| user-guide.md | #lock | 318 | guides/lock-and-reproduce.md | move |
| user-guide.md | #global-toolchain | 341 | guides/global-toolchain.md | move |
| user-guide.md | #global-toolchain-ci-path | 477 | integration/github-actions.md | merge into the existing section |
| user-guide.md | #project | 521 | guides/project-toolchain.md | move |
| user-guide.md | "Lifecycle commands" | 536 | guides/update-tools.md | move |
| user-guide.md | #exec | 663 | guides/run-project-tools.md | move |
| user-guide.md | #ci | 721 | integration/github-actions.md | merge into the existing section |
| user-guide.md | #publish | 750 | publishing/index.md | delete here, the target already carries it |
| user-guide.md | #authentication | 758 | guides/authenticate.md | move |
| user-guide.md | #offline | 830 | guides/work-offline.md | move |
| user-guide.md | #mirrors | 884 | guides/corporate-mirror.md | move |
| user-guide.md | #configuration | 961 | guides/configure-ocx.md | move |
| user-guide.md | #managed-config | 985 | guides/manage-config-centrally.md | move |
| user-guide.md | #managed-config-ca | 1093 | integration/proxy-and-ca.md | move |
| user-guide.md | #update-ocx | 1135 | guides/update-ocx.md | move |
| user-guide.md | #supply-chain | 1178 | guides/verify-installs.md | move |
| user-guide.md | #supply-chain-signing | 1188 | publishing/sign-a-release.md | move |
| user-guide.md | #supply-chain-trust-policy | 1235 | guides/pin-signer-identity.md | move |
| user-guide.md | #cleanup | 1329 | guides/clean-up.md | move |
| user-guide.md | #locked-frozen-equivalents | 1345 | guides/lock-and-reproduce.md | move |
| user-guide.md | #project-toolchain-migration | 1364 | — | delete, obsolete |
| getting-started.md | #quick-start | 12 | getting-started/first-package.md | move |
| getting-started.md | #installing | 38 | getting-started/first-package.md | move |
| getting-started.md | #switching-versions | 66 | getting-started/first-package.md | move |
| getting-started.md | #uninstalling | 92 | getting-started/first-package.md | move |
| getting-started.md | #environment | 108 | getting-started/first-package.md | move |
| getting-started.md | #project-toolchain | 136 | getting-started/first-project.md | move |
| getting-started.md | #next-steps | 158 | getting-started/first-project.md | move |
| getting-started.md | #next-steps-config | 166 | guides/configure-ocx.md | delete here, the target already carries it |
| installation.md | #with-script | 39 | getting-started/install-ocx.md | move |
| installation.md | #manual | 109 | getting-started/install-ocx.md | move |
| installation.md | #docker | 173 | integration/docker.md | delete here, the target already carries it |
| installation.md | #updating | 183 | guides/update-ocx.md | merge into the existing section |
| installation.md | #uninstalling | 219 | guides/update-ocx.md | merge into the existing section |
| installation.md | #ci | 246 | integration/github-actions.md | merge into the existing section |
| installation.md | #dev-builds | 256 | getting-started/install-ocx.md | move |
| installation.md | #next-steps | 274 | getting-started/install-ocx.md | move |
| faq.md | "Versioning" | 6 | explanation/versioning.md | merge into the existing section |
| faq.md | #installation-faq | 28 | getting-started/install-ocx.md | merge into the existing section |
| faq.md | #project | 61 | guides/run-project-tools.md | merge into the existing section |
| faq.md | #dependencies | 85 | explanation/dependencies.md | merge into the existing section |
| faq.md | "Linux" | 111 | troubleshooting/platforms.md | move |
| faq.md | "macOS" | 143 | troubleshooting/platforms.md | move |
| faq.md | "Windows" | 213 | troubleshooting/platforms.md | move |
| in-depth/ci.md | #ci-github-actions | 16 | integration/github-actions.md | move |
| in-depth/ci.md | #ci-gitlab | 98 | integration/gitlab-ci.md | move |
| in-depth/indices.md | #format | 14 | explanation/indices.md | move |
| in-depth/indices.md | #remote | 24 | explanation/indices.md | move |
| in-depth/indices.md | #local | 32 | explanation/local-index.md | move |
| in-depth/indices.md | #servable | 177 | guides/serve-an-index.md | move |
| in-depth/indices.md | #active | 294 | explanation/indices.md | move |
| in-depth/indices.md | #bundled | 323 | explanation/indices.md | move |
| in-depth/indices.md | #public-index | 345 | explanation/public-index.md | move |
| in-depth/indices.md | #mirroring | 437 | guides/corporate-mirror.md | delete here, the target already carries it |
| in-depth/indices.md | #writing | 453 | explanation/indices.md | move |
| in-depth/indices.md | #keep-tags | 503 | explanation/indices.md | move |
| in-depth/indices.md | #update-family | 515 | explanation/indices.md | move |
| in-depth/indices.md | "See Also" | 530 | explanation/indices.md | move |
| in-depth/shell-integration.md | #activation | 8 | explanation/shell-integration.md | move |
| in-depth/shell-integration.md | #session-path | 73 | explanation/shell-integration.md | move |
| in-depth/shell-integration.md | #consent | 121 | explanation/shell-integration.md | move |
| in-depth/shell-integration.md | #commands | 159 | troubleshooting/shell.md | move |
| in-depth/shell-integration.md | #diagnostics | 174 | troubleshooting/shell.md | move |
| in-depth/shell-integration.md | #coexistence | 254 | troubleshooting/shell.md | move |
| in-depth/shell-integration.md | #repair | 270 | troubleshooting/shell.md | move |
| in-depth/shell-integration.md | #residual | 278 | explanation/shell-integration.md | move |
| in-depth/signing.md | #trust-root | 18 | explanation/signing.md | move |
| in-depth/signing.md | #referrers-cache | 37 | explanation/signing.md | move |
| in-depth/signing.md | #referrers-write | 56 | explanation/signing.md | move |
| in-depth/signing.md | #bundle-storage | 76 | explanation/signing.md | move |
| in-depth/signing.md | #cosign-interop | 88 | explanation/cosign-parity.md | delete here, the target already carries it |
| in-depth/signing.md | #key-mode | 114 | explanation/signing.md | move |
| in-depth/signing.md | #identity-matching | 131 | explanation/signing.md | move |
| in-depth/signing.md | #deployments | 149 | explanation/signing.md | move |
| in-depth/signing.md | #self-hosted | 166 | guides/self-hosted-sigstore.md | delete here, the target already carries it |
| in-depth/signing.md | #ci | 206 | publishing/sign-a-release.md | move |
| in-depth/signing.md | #slice-boundary | 254 | explanation/signing.md | move |
| in-depth/signing.md | #attestations | 262 | publishing/attest-and-sbom.md | delete here, the target already carries it |
| in-depth/signing.md | #current-limitations | 270 | explanation/signing.md | move |
| in-depth/signing.md | #deferred-future-work | 281 | explanation/signing.md | move |
| in-depth/signing.md | #offline-verification | 285 | guides/verify-installs.md | delete here, the target already carries it |
| in-depth/signing.md | #signing-flow | 314 | explanation/signing.md | move |
| in-depth/signing.md | #see-also | 325 | explanation/signing.md | move |
| in-depth/storage.md | #stores | 10 | explanation/storage.md | move |
| in-depth/storage.md | #packages | 69 | explanation/storage.md | move |
| in-depth/storage.md | #layers | 120 | explanation/storage.md | move |
| in-depth/storage.md | #index | 178 | explanation/storage.md | move |
| in-depth/storage.md | #symlinks | 203 | explanation/storage.md | move |
| in-depth/storage.md | #toolchain | 247 | explanation/toolchain-tree.md | move |
| in-depth/storage.md | "See Also" | 323 | explanation/storage.md | move |
| authoring/announcing.md | #announcing-entry | 47 | publishing/announce.md | move |
| authoring/announcing.md | #announcing-claim | 67 | publishing/announce.md | move |
| authoring/announcing.md | #announcing-tags | 93 | publishing/announce.md | move |
| authoring/announcing.md | #announcing-postures | 166 | publishing/announce-from-ci.md | move |
| authoring/announcing.md | #announcing-owners | 376 | publishing/announce.md | move |
| authoring/announcing.md | #announcing-review | 411 | publishing/announce.md | move |
| authoring/announcing.md | #announcing-consumers | 448 | publishing/announce.md | move |
| authoring/announcing.md | #announcing-runner | 462 | publishing/announce-from-ci.md | move |
| authoring/announcing.md | #announcing-troubleshooting | 482 | publishing/announce.md | move |
| authoring/testing.md | #motivation | 6 | publishing/test-locally.md | move |
| authoring/testing.md | #basic | 16 | publishing/test-locally.md | move |
| authoring/testing.md | #identifier | 49 | publishing/test-locally.md | move |
| authoring/testing.md | #keep | 61 | publishing/test-locally.md | move |
| authoring/testing.md | #output | 72 | publishing/test-locally.md | move |
| authoring/testing.md | #self | 90 | publishing/test-locally.md | move |
| authoring/testing.md | #clean | 101 | publishing/test-locally.md | move |
| authoring/testing.md | #digest-layers | 112 | publishing/test-locally.md | move |
| authoring/testing.md | #workflow | 124 | publishing/test-locally.md | move |
| authoring/testing.md | #scripted-tests | 148 | publishing/script-tests.md | move |
| authoring/testing.md | #scripted-tests-ocx-api | 173 | reference/script-host-api.md | delete here, the target already carries it |
| authoring/testing.md | #scripted-tests-expect-api | 213 | reference/script-host-api.md | delete here, the target already carries it |
| authoring/testing.md | #exit-codes | 344 | publishing/test-locally.md | move |
| authoring/testing.md | #see-also | 354 | publishing/test-locally.md | move |
| authoring/multi-platform.md | #concept | 10 | publishing/multi-platform.md | move |
| authoring/multi-platform.md | #pattern | 16 | publishing/multi-platform.md | move |
| authoring/multi-platform.md | #metadata | 38 | publishing/multi-platform.md | move |
| authoring/multi-platform.md | #stability | 54 | publishing/multi-platform.md | move |
| authoring/multi-platform.md | #libc | 58 | publishing/libc-variants.md | move |
| authoring/multi-platform.md | #wasm | 168 | publishing/multi-platform.md | move |
| authoring/multi-platform.md | #dependency-platforms | 207 | publishing/multi-platform.md | move |
| authoring/multi-platform.md | #see-also | 251 | publishing/multi-platform.md | move |
| user-guide/patches.md | #patches-how | 25 | explanation/patches.md | move |
| user-guide/patches.md | #patches-consumer | 269 | guides/patch-packages.md | move |
| user-guide/patches.md | #patches-maintainer | 329 | publishing/maintain-patches.md | move |
| user-guide/patches.md | #patches-sync | 522 | publishing/maintain-patches.md | move |
| user-guide/patches.md | #patches-enforcement | 551 | guides/patch-packages.md | move |
| user-guide/patches.md | #patches-no-patches | 579 | guides/patch-packages.md | move |
| user-guide/patches.md | #patches-offline | 616 | guides/patch-packages.md | move |
| user-guide/patches.md | #patches-pins | 649 | explanation/patches.md | move |
| user-guide/patches.md | #patches-config | 669 | guides/patch-packages.md | move |
| user-guide/patches.md | #patches-in-depth | 687 | explanation/patches.md | move |
| reference/metadata.md | #format | 24 | reference/metadata/index.md | move |
| reference/metadata.md | #env | 106 | reference/metadata/env.md | move |
| reference/metadata.md | #dependencies | 406 | reference/metadata/dependencies.md | move |
| reference/metadata.md | #entry-points | 603 | reference/metadata/entry-points.md | move |
| reference/metadata.md | #executables | 693 | reference/metadata/entry-points.md | move |
| reference/metadata.md | #integrations | 837 | reference/metadata/integrations.md | move |
| reference/metadata.md | #extraction | 947 | reference/metadata/index.md | move |
| reference/metadata.md | #json-schema | 968 | reference/metadata/index.md | move |
| reference/metadata.md | #schema | 1003 | reference/metadata/index.md | move |
| reference/metadata.md | #oci-platform | 1021 | reference/metadata/index.md | move |
| reference/metadata.md | #schema-changelog | 1056 | reference/metadata/index.md | move |

### 6.3 Delete list

Pages deleted outright need both signals: they map to no surviving reader goal, and they are a stub or an exact duplicate of covered content. Section deletions inside surviving pages are listed in the homes table and carry a ledger reason.

| path | no surviving need | stub or duplicate | ledger reason |
|---|---|---|---|
| `website/src/index.md` | yes: it is the root-site landing page (D8), not a docs page | duplicate of the root site `/` claim; 571 prose words, the "Content-Addressed and Deduplicated" section goes into the upstream patch | upstream-patch |
| `website/src/team.md` | yes: root-site page (D8) | stub: 1 prose word | upstream-patch |

Kept although they look deletable: `changelog.md` (2 prose words; a build-time include of `CHANGELOG.md`, so the generator-directive exemption applies), `reference/dependencies.md` (generated SBOM page, exemption applies). `faq.md` is not deleted: its entries are promoted to task and concept pages and its Linux, macOS and Windows entries remain as `troubleshooting/platforms.md`. `roadmap.md` is rewritten as a table or list page without components (D3).

### 6.4 FAQ promotion

| FAQ entry | Lands in | Writer |
|---|---|---|
| Build separator | `explanation/versioning.md` | P13d |
| How do I install a specific ocx version | `getting-started/install-ocx.md` | P13a |
| `ocx package exec` vs `ocx exec` | `guides/run-project-tools.md` | P13b |
| No version ranges, automatic security patches, shared dependencies | `explanation/dependencies.md` | P13d |
| glibc on Alpine with gcompat, non-FHS hosts, libc detection across namespaces | `troubleshooting/platforms.md` | P13f |
| macOS code signing, Windows executable resolution | `troubleshooting/platforms.md` | P13f |

## 7. Homes for explanatory sections leaving reference pages

Columns: the reference page and section that leaves, what happens, the home, **where P9 edits** (the page as it exists before P12; "create" means P9 writes the new page at its new path), and the ledger reason. Every home is a page in the inventory. When the home is a section that later splits (for example inside `user-guide.md`), P9 writes into the old section and P13 carries it to the new page.

| source | section | disposition | home | P9 edits | reason |
|---|---|---|---|---|---|
| reference/configuration.md | File Locations, Explicit additions (#file-locations) | keep the path table in the generated page; the prose goes | explanation/configuration.md (#tiers, #explicit) | in-depth/configuration.md | duplicate |
| reference/configuration.md | Discovery and Merge Precedence, Merge rules (#precedence) | delete | explanation/configuration.md (#precedence, #merge-rules) | in-depth/configuration.md | duplicate |
| reference/configuration.md | Kill switch (#precedence-kill-switch) | delete | explanation/configuration.md (#kill-switch) | in-depth/configuration.md | duplicate |
| reference/configuration.md | Unknown keys and sections (#unknown-keys) | one line stays; the rationale moves | explanation/configuration.md (#merge-rules) | in-depth/configuration.md | duplicate |
| reference/configuration.md | `toolchain_dir`: Expansion, Refusals (#keys-toolchain_dir) | move | guides/project-toolchain.md | user-guide.md#project-toolchain_dir | duplicate |
| reference/configuration.md | "Rolling it out to a fleet", "Publishing to a fleet", "System-locked" repeated per key | move once, delete the repeats | guides/manage-config-centrally.md; explanation/configuration.md (#managed) | user-guide.md#managed-config; in-depth/configuration.md#managed | duplicate |
| reference/configuration.md | `extra_ca_certs`: Refused values, Where it sits (#keys-extra_ca_certs) | move | integration/proxy-and-ca.md | create (integration/proxy-and-ca.md) | duplicate |
| reference/configuration.md | `[registry]`, `[registries.<name>]`: `trusted_hosts`, `insecure`, `file://` bases | move | guides/corporate-mirror.md; guides/serve-an-index.md (file:// bases) | user-guide.md#mirrors; in-depth/indices.md#servable-consuming | duplicate |
| reference/configuration.md | `[mirrors]` (#keys-mirrors) | delete the narrative | guides/corporate-mirror.md | user-guide.md#mirrors | duplicate |
| reference/configuration.md | `[patches]` (#keys-patches) | delete the narrative | guides/patch-packages.md | user-guide/patches.md#patches-config | duplicate |
| reference/configuration.md | `[records]` (#keys-records) | delete the narrative | reference/execution-records.md (#execution-records-configuration) | reference/execution-records.md | duplicate |
| reference/configuration.md | `[managed]` (#keys-managed) | move | guides/manage-config-centrally.md; explanation/configuration.md (#managed) | user-guide.md#managed-config; in-depth/configuration.md#managed | duplicate |
| reference/configuration.md | `[update]` (#keys-update) | move | guides/update-ocx.md | user-guide.md#update-ocx | duplicate |
| reference/configuration.md | `[[trust.policy]]` fields, signers, scope, include and exclude (#keys-trust) | move (about 414 lines) | guides/pin-signer-identity.md | user-guide.md#supply-chain-trust-policy | duplicate |
| reference/configuration.md | `[[trust.policy]]` specificity, regex identities, tier precedence, no match | move | explanation/signing.md (#identity-matching) | in-depth/signing.md#identity-matching | duplicate |
| reference/configuration.md | `[trust.sigstore]` (#keys-trust-sigstore) | move | guides/self-hosted-sigstore.md | in-depth/self-hosted-sigstore.md#trusted-root | duplicate |
| reference/configuration.md | `[shell]` `hook` and `completions` ladders, `[shell.consent]` (#keys-shell) | move (about 352 lines) | explanation/shell-integration.md (#consent, #activation) | in-depth/shell-integration.md | duplicate |
| reference/configuration.md | `[shell]` `modify_path`, `profiles` | move | getting-started/install-ocx.md | installation.md#script-options | duplicate |
| reference/configuration.md | Environment Variable Override Table (#env-overrides) | delete | reference/environment.md (generated) | none | duplicate |
| reference/configuration.md | Error Reference (#errors) | delete | reference/exit-codes.md (generated) | none | duplicate |
| reference/configuration.md | Project Configuration, `[group.<name>]`, `[package."<id>"]`, names (#project-config) | move the narrative; entries stay in project-file.md | guides/project-toolchain.md | user-guide.md#project | duplicate |
| reference/configuration.md | Toolchain-level `lazy-mode`, `lazy-report` (#project-config-toolchain-lazy) | move | explanation/deferred-packages.md | in-depth/lazy-loading.md | duplicate |
| reference/configuration.md | Toolchain-level `activate` and `pinned`, resolution orders (#project-config-activate, #project-config-pinned) | move | explanation/toolchain-activation.md | create (explanation/toolchain-activation.md) | duplicate |
| reference/configuration.md | JSON Schemas (#schemas) | delete | reference/machine-interface.md (#machine-interface-schemas) | none | duplicate |
| reference/configuration.md | Future Config Keys (#future, #future-registries-fields, #future-clean, #future-project) | delete | none | none | speculative |
| reference/environment.md | Primer: Truthy Values (#truthy-values) | one line stays on the generated page; the prose goes | explanation/configuration.md (new subsection "Environment overrides") | in-depth/configuration.md | duplicate |
| reference/environment.md | Internal: Shell Activation Files (#shell-activation-files) | move | explanation/shell-integration.md (#activation) | in-depth/shell-integration.md | duplicate |
| reference/environment.md | `_OCX_APPLIED`, `__OCX_ENV_STATE`, `__OCX_ENV_PWD` essays | delete the prose; one line per variable stays | explanation/shell-integration.md (#activation-state-carrier) | in-depth/shell-integration.md | duplicate |
| reference/environment.md | repeated `apply_ocx_config` paragraphs under about 15 variables | delete | explanation/environments.md (#ocx-forwarding) | in-depth/environments.md | duplicate |
| reference/environment.md | `OCX_AUTH_<REGISTRY>_*`, `DOCKER_CONFIG` essays | move | guides/authenticate.md | user-guide.md#authentication | duplicate |
| reference/environment.md | `OCX_ALLOW_YANKED`, `OCX_BINARY_PIN`, `OCX_SELF_UPDATE`, `OCX_UPDATE_CHECK_INTERVAL` essays | move | guides/choose-versions.md; guides/update-ocx.md | user-guide.md#versions; user-guide.md#update-ocx | duplicate |
| reference/environment.md | `OCX_ANNOUNCE_*` essays | move | publishing/announce-from-ci.md | authoring/announcing.md#announcing-runner | duplicate |
| reference/environment.md | `OCX_SIGNING_KEY`, `OCX_KEY_PASSWORD`, `OCX_IDENTITY_TOKEN`, `OCX_SIGSTORE_TRUSTED_ROOT`, OIDC token sources | move | publishing/sign-a-release.md; guides/self-hosted-sigstore.md | in-depth/signing.md#ci; in-depth/self-hosted-sigstore.md | duplicate |
| reference/environment.md | `OCX_CONSENT_PATHS`, `OCX_CONSENT_NAMESPACES`, `OCX_NO_CONSENT`, `OCX_NO_HOOK`, `OCX_NO_PROJECT` essays | move | explanation/shell-integration.md (#consent) | in-depth/shell-integration.md#consent | duplicate |
| reference/environment.md | `OCX_MIRRORS`, `OCX_INSECURE_REGISTRIES`, `OCX_INDEX` essays | move | guides/corporate-mirror.md; guides/serve-an-index.md | user-guide.md#mirrors-env; in-depth/indices.md | duplicate |
| reference/environment.md | `OCX_PATCHES`, `OCX_PATCH_SNAPSHOT` essays | move | guides/patch-packages.md | user-guide/patches.md#patches-config | duplicate |
| reference/environment.md | `OCX_LAZY_MODE`, `OCX_LAZY_REPORT` essays | move | explanation/deferred-packages.md | in-depth/lazy-loading.md | duplicate |
| reference/environment.md | `OCX_TOOLCHAIN_*` (activate, dir, pinned, update) essays | move | explanation/toolchain-activation.md; guides/project-toolchain.md (#project-toolchain_dir) | create; user-guide.md#project-toolchain_dir | duplicate |
| reference/environment.md | `OCX_RECORDS_DIR`, `OCX_RECORDS_NAME`, `OCX_LAUNCH_IDENTITIES` essays | move | reference/execution-records.md | reference/execution-records.md | duplicate |
| reference/environment.md | `OCX_HOME` role in setup, `OCX_NO_MODIFY_PATH`, `OCX_NO_COMPLETION` essays | move | getting-started/install-ocx.md | installation.md#script-options | duplicate |
| reference/environment.md | `OCX_OFFLINE`, `OCX_REMOTE`, `OCX_FROZEN` essays | move | guides/work-offline.md; guides/lock-and-reproduce.md | user-guide.md#offline; user-guide.md#lock | duplicate |
| reference/environment.md | External CI variables: `CI`, `GITHUB_*`, `GITLAB_CI`, `CI_*`, `SOURCE_DATE_EPOCH` | move | integration/github-actions.md; integration/gitlab-ci.md; integration/unattended-ci.md | in-depth/ci.md | duplicate |
| reference/environment.md | External proxy and CA variables: `HTTPS_PROXY` family, `SSL_CERT_*`, Git trust store; `OCX_EXTRA_CA_CERTS` | move | integration/proxy-and-ca.md | create (integration/proxy-and-ca.md) | duplicate |
| reference/environment.md | direnv and mise coexistence variables | delete the prose | troubleshooting/shell.md (#coexistence) | in-depth/shell-integration.md#coexistence | duplicate |
| reference/environment.md | `ocx-mirror` section (#ocx-mirror-pipeline) | delete; the reference sidebar links the tool's own docs | none | none | duplicate |
| reference/environment.md | Script env access (#script-env-access) | one line stays | reference/script-host-api.md | reference/script-host-api.md | duplicate |
| reference/command-line.md | General Options: `--format` and the JSON documents | delete the prose | reference/machine-interface.md (#machine-interface-roots); integration/scripts-and-json.md | reference/machine-interface.md | duplicate |
| reference/command-line.md | Exit codes (#exit-codes) | delete | reference/exit-codes.md (generated) | none | duplicate |
| reference/command-line.md | `exec` running semantics | delete the restatement | explanation/project-toolchain.md (#running); guides/run-project-tools.md | in-depth/project.md#running | duplicate |
| reference/command-line.md | Command narrative and examples: install, pull, select, deselect, uninstall, which, env, clean | move (P7 puts the help text in source first) | getting-started/first-package.md; guides/choose-versions.md; guides/clean-up.md | getting-started.md; user-guide.md | duplicate |
| reference/command-line.md | Command narrative: init, add, remove, lock, update, upgrade, status, inspect, exec | move | guides/project-toolchain.md; guides/update-tools.md; guides/lock-and-reproduce.md; guides/run-project-tools.md | user-guide.md | duplicate |
| reference/command-line.md | Command narrative: package create, push, cascade, description, announce, claim, test | move | publishing/build-and-push.md; publishing/announce.md; publishing/test-locally.md | authoring/building-pushing.md; authoring/announcing.md; authoring/testing.md | duplicate |
| reference/command-line.md | Command narrative: package sign, verify, attest, sbom | move | publishing/sign-a-release.md; guides/verify-installs.md; publishing/attest-and-sbom.md | user-guide.md#supply-chain; user-guide/attestations.md | duplicate |
| reference/command-line.md | Command narrative: patch, config, index, login, logout, shell, direnv, self | move | guides/patch-packages.md; guides/manage-config-centrally.md; guides/find-packages.md; guides/authenticate.md; troubleshooting/shell.md; guides/update-ocx.md | user-guide/patches.md; user-guide.md; in-depth/shell-integration.md | duplicate |
| reference/metadata.md | Dependencies: Authoring vs Published, Manifest Pins, No Version Ranges, Registry Requirement | move | explanation/dependencies.md; publishing/declare-dependencies.md | in-depth/dependencies.md; authoring/dependencies.md | duplicate |
| reference/metadata.md | Entry Points: Disk Layout, Baked Arguments, Example; Executables: claim, scope, `None` vs empty, vs entry points, writing the field | move | explanation/entry-points.md; publishing/entry-points.md | in-depth/entry-points.md; authoring/entry-points.md | duplicate |
| reference/metadata.md | Integrations: No Merge, Ever, Namespace Keys, Size Caps, schemaVersion convention, who can declare | move | explanation/integrations.md | create (explanation/integrations.md) | duplicate |
| reference/metadata.md | Extraction: `strip_components` | delete | publishing/bundle-anatomy.md (#strip-components) | authoring/bundle-anatomy.md | duplicate |
| reference/metadata.md | JSON Schema: Editor Integration, Generating Locally, Validation | one line each stays | reference/machine-interface.md (#machine-interface-schemas); publishing/bundle-anatomy.md | reference/machine-interface.md | duplicate |
| reference/metadata.md | OCI Platform Fields: `os.features` and libc tagging | delete the prose | publishing/libc-variants.md | authoring/multi-platform.md#libc | duplicate |
| reference/env-composition.md | Strict Isolation (#strict-isolation) | move | explanation/environments.md | in-depth/environments.md | duplicate |
| reference/env-composition.md | Patch Opt-Out Scope, Patch Companions (#patch-opt-out-scope, #visibility-surfaces-companions) | move | explanation/patches.md | user-guide/patches.md#patches-no-patches | duplicate |
| reference/env-composition.md | Tier Selection (#tier-selection) | move | guides/global-toolchain.md | user-guide.md#global-toolchain | duplicate |
| reference/env-composition.md | Toolchain Activation: matrix, trampolines, collisions (#toolchain-activation) | move; the matrix table stays | explanation/toolchain-activation.md | create (explanation/toolchain-activation.md) | duplicate |
| reference/env-composition.md | Self-Referencing Values (#self-referencing) | move | explanation/environments.md | in-depth/environments.md | duplicate |
| reference/env-composition.md | Integrations (#integrations) | move | explanation/integrations.md | create (explanation/integrations.md) | duplicate |
| reference/env-composition.md | Project Environment, Precedence (#project-env) | move; the precedence table stays | explanation/project-toolchain.md | in-depth/project.md | duplicate |
| reference/execution-records.md | A policy check that works today (#execution-records-consumer) | move | explanation/execution-records.md; integration/scripts-and-json.md | create (explanation/execution-records.md) | duplicate |
| reference/execution-records.md | What `pkg:oci` buys, Chaining to an SBOM or attestation, Two records per launcher invocation, The launcher shim frame, Patch companions in the record | move | explanation/execution-records.md | create (explanation/execution-records.md) | duplicate |
| reference/script-host-api.md | At a glance, Cross-type wall (#overview, #cross-type-wall) | move | publishing/script-tests.md | authoring/testing.md#scripted-tests | duplicate |
| reference/platforms.md | Compatibility: relation, scoring (#compatibility) | move | publishing/libc-variants.md (#libc-selection) | authoring/multi-platform.md#libc-selection | duplicate |
| reference/platforms.md | Shared Digests (#shared-digests) | move | explanation/storage.md | in-depth/storage.md | duplicate |
| reference/machine-interface.md | Exit Codes (#machine-interface-exit-codes) | delete | reference/exit-codes.md (generated) | none | duplicate |
| reference/machine-interface.md | Consumer how-to prose (reading outcomes, error handling) | move | integration/scripts-and-json.md | reference/machine-interface.md | duplicate |

The generated entries keep today's anchors (C-006); an explanatory section never stays on a reference page, even as a collapsed block. A "one line stays" row means a single type, default or description sentence in the entry, which is the entry shape of C-001.

## 8. Page budget (14,200 B gzip HTML per page)

Measure used here: gzip of the Markdown source, which understates built HTML (tags and highlighted code add weight). Nine pages exceed the budget on source alone; `in-depth/signing.md` is 263 B short of it and is the tenth page the discussion counts. Each is brought under by moving a section (table 6.2), a generated split, or both. Pages near the line are listed because built HTML adds weight.

| old page | prose words | source gzip B | plan |
|---|---|---|---|
| reference/command-line.md | 44056 | 172680 | generated: 10 group pages, `package` in two, none over 20 entries |
| reference/configuration.md | 16526 | 52999 | generated: `configuration.md`, `project-file.md`; narrative leaves (table 7) |
| reference/environment.md | 16148 | 45004 | generated: `environment.md`; essays leave (table 7) |
| user-guide.md | 14008 | 44756 | split into 17 guide pages; sections also land in six other pages (table 6.2) |
| reference/metadata.md | 6800 | 23705 | split into 5 reference pages, overflow leaves (table 7) |
| in-depth/indices.md | 6858 | 19923 | split: indices, local-index, public-index, serve-an-index (table 6.2) |
| reference/execution-records.md | 5351 | 17596 | trim to shape and filename grammar; five sections move to `explanation/execution-records.md` |
| in-depth/shell-integration.md | 5990 | 16743 | split: concept page and `troubleshooting/shell.md` (table 6.2) |
| reference/env-composition.md | 4990 | 15631 | trim to tables; six sections move (table 7) |
| in-depth/signing.md | 4459 | 13937 | shrink: five duplicate or misplaced sections leave (table 6.2) |
| user-guide/patches.md | 4079 | 12343 | split: patch-packages, maintain-patches, explanation/patches (table 6.2) |
| in-depth/project.md | 3551 | 12106 | near the line; P13d dedupes the running-binaries section against the guide |
| authoring/announcing.md | 3040 | 11953 | split: announce, announce-from-ci |
| in-depth/storage.md | 3457 | 11107 | split: toolchain section becomes `explanation/toolchain-tree.md` |
| contributing/bazel.md | 3314 | 10303 | near the line; P13f measures built HTML and splits "Reading Grafana" if over |
| faq.md | 2612 | 9281 | dissolves into task pages (table 6.4) |
| authoring/multi-platform.md | 2844 | 9200 | split: multi-platform, libc-variants |
| authoring/building-pushing.md | 2767 | 9123 | near the line; P13c measures built HTML |
| in-depth/configuration.md | 2925 | 9029 | measure built HTML in P13 |
| in-depth/entry-points.md | 2698 | 9026 | measure built HTML in P13 |
| authoring/testing.md | 2184 | 8937 | split: test-locally, script-tests; API duplicates deleted |
| in-depth/cosign-parity.md | 2869 | 8762 | near the line; P13d trims duplicate interop text shared with signing |

## 9. Sidebar groups

Ten top-level groups, two nesting levels at most. `start` holds the first-steps entry point and is a different top-level group from the everyday hub (`packages`). `slug` is the value of the `sidebar` column. P12 writes them as Starlight `label` groups in `astro.config.mjs`, in this order, `autogenerate` off.

| order | slug | label | tier of its pages | pages |
|---|---|---|---|---|
| 1 | start | Start here | first-steps | 4 |
| 2 | packages | Packages | everyday | 6 |
| 3 | projects | Projects | everyday | 5 |
| 4 | settings | Settings and trust | everyday | 10 |
| 5 | troubleshooting | Troubleshooting | none | 2 |
| 6 | publishing | Publishing | everyday | 18 |
| 7 | integrations | Integrations | integration | 12 |
| 8 | concepts | Concepts | none (explanation) | 19 |
| 9 | reference | Reference | none (reference) | 25 |
| 10 | project | Project | none | 3 |

Order inside each group is the order in the lists below (the inventory table is ordered for reading the plan, not for the sidebar).

**Start here** (`start`): `index.md`, `getting-started/install-ocx.md`, `getting-started/first-package.md`, `getting-started/first-project.md`.

**Packages** (`packages`): `guides/choose-versions.md`, `guides/find-packages.md`, `guides/run-with-dependencies.md`, `guides/stable-paths.md`, `guides/patch-packages.md`, `guides/clean-up.md`.

**Projects** (`projects`): `guides/project-toolchain.md`, `guides/run-project-tools.md`, `guides/lock-and-reproduce.md`, `guides/update-tools.md`, `guides/global-toolchain.md`.

**Settings and trust** (`settings`): `guides/configure-ocx.md`, `guides/authenticate.md`, `guides/work-offline.md`, `guides/corporate-mirror.md`, `guides/manage-config-centrally.md`, `guides/verify-installs.md`, `guides/pin-signer-identity.md`, `guides/self-hosted-sigstore.md`, `guides/serve-an-index.md`, `guides/update-ocx.md`.

**Troubleshooting** (`troubleshooting`): `troubleshooting/shell.md`, `troubleshooting/platforms.md`.

**Publishing** (`publishing`): `publishing/index.md`, `publishing/bundle-anatomy.md`, `publishing/declare-dependencies.md`, `publishing/env-surface.md`, `publishing/entry-points.md`, `publishing/build-and-push.md`, `publishing/test-locally.md`, `publishing/script-tests.md`, `publishing/multi-platform.md`, `publishing/libc-variants.md`, `publishing/announce.md`, `publishing/announce-from-ci.md`, `publishing/sign-a-release.md`, `publishing/attest-and-sbom.md`, `publishing/promote-packages.md`, `publishing/snapshot-tracks.md`, `publishing/maintain-patches.md`, `publishing/repackage-releases.md`.

**Integrations** (`integrations`): `integration/index.md`, `integration/github-actions.md`, `integration/gitlab-ci.md`, `integration/unattended-ci.md`, `integration/registry-auth-recipes.md`, `integration/proxy-and-ca.md`, `integration/docker.md`, `integration/bazel.md`, `integration/devcontainers-and-editors.md`, `integration/scripts-and-json.md`, `integration/python.md`, `integration/publisher-ci.md`.

**Concepts** (`concepts`): `explanation/storage.md`, `explanation/toolchain-tree.md`, `explanation/versioning.md`, `explanation/dependencies.md`, `explanation/indices.md`, `explanation/local-index.md`, `explanation/public-index.md`, `explanation/project-toolchain.md`, `explanation/configuration.md`, `explanation/environments.md`, `explanation/toolchain-activation.md`, `explanation/shell-integration.md`, `explanation/entry-points.md`, `explanation/deferred-packages.md`, `explanation/patches.md`, `explanation/integrations.md`, `explanation/execution-records.md`, `explanation/signing.md`, `explanation/cosign-parity.md`.

**Reference** (`reference`): `reference/command-line/index.md`, `reference/command-line/commands.md`, `reference/command-line/config.md`, `reference/command-line/direnv.md`, `reference/command-line/index-commands.md`, `reference/command-line/package-publishing.md`, `reference/command-line/package-consuming.md`, `reference/command-line/patch.md`, `reference/command-line/self.md`, `reference/command-line/shell.md`, `reference/environment.md`, `reference/configuration.md`, `reference/project-file.md`, `reference/exit-codes.md`, `reference/metadata/index.md`, `reference/metadata/env.md`, `reference/metadata/dependencies.md`, `reference/metadata/entry-points.md`, `reference/metadata/integrations.md`, `reference/env-composition.md`, `reference/execution-records.md`, `reference/script-host-api.md`, `reference/platforms.md`, `reference/machine-interface.md`, `reference/dependencies.md`; external link `mirror.yml` to the ocx-mirror docs.

**Project** (`project`): `roadmap.md`, `changelog.md`, `contributing/bazel.md`.

The `reference` group shows the command-line pages as one nested group "Command line" (index first), the generated pages next, then the hand-written pages. The `project` group keeps `roadmap`, `changelog` and `contributing/bazel`.

## 10. Declaration seed for P5 (old paths, as converted)

P5 converts pages in place (same relative path, under `src/content/docs/`) before the IA moves, so it seeds by **old** path. For a page that splits, the seed is the type of the page as a whole; P12 and P13 retype the children from table 5. Carrier: `<!-- doc_type: x -->` and `<!-- doc_tier: y -->` comments below the front matter, within the first 12 lines (never YAML front matter, `{/* */}` form in MDX pages). `type source`: `nav label` where the VitePress sidebar group names the type (In Depth, Reference), `content read` otherwise.

| old | doc_type | doc_tier | type source | prose words | source gzip B |
|---|---|---|---|---|---|
| authoring/announcing.md | how-to | everyday | content read | 3040 | 11953 |
| authoring/building-pushing.md | how-to | everyday | content read | 2767 | 9123 |
| authoring/bundle-anatomy.md | how-to | everyday | content read | 981 | 3885 |
| authoring/dependencies.md | how-to | everyday | content read | 1153 | 4449 |
| authoring/entry-points.md | how-to | everyday | content read | 1625 | 5072 |
| authoring/env-surface.md | how-to | everyday | content read | 1592 | 6137 |
| authoring/index.md | landing | everyday | content read | 815 | 3738 |
| authoring/migration.md | how-to | everyday | content read | 793 | 3822 |
| authoring/multi-platform.md | how-to | everyday | content read | 2844 | 9200 |
| authoring/testing.md | how-to | everyday | content read | 2184 | 8937 |
| changelog.md | changelog | — | content read | 2 | 92 |
| contributing/bazel.md | how-to | everyday | content read | 3314 | 10303 |
| docker.md | how-to | integration | content read | 848 | 3776 |
| faq.md | troubleshooting | — | content read | 2612 | 9281 |
| getting-started.md | tutorial | first-steps | content read | 1558 | 5819 |
| in-depth/ci.md | how-to | integration | content read | 953 | 3576 |
| in-depth/configuration.md | explanation | — | nav label | 2925 | 9029 |
| in-depth/cosign-parity.md | explanation | — | nav label | 2869 | 8762 |
| in-depth/dependencies.md | explanation | — | nav label | 1616 | 5253 |
| in-depth/entry-points.md | explanation | — | nav label | 2698 | 9026 |
| in-depth/environments.md | explanation | — | nav label | 2250 | 8045 |
| in-depth/indices.md | explanation | — | nav label | 6858 | 19923 |
| in-depth/lazy-loading.md | explanation | — | nav label | 982 | 3521 |
| in-depth/project.md | explanation | — | nav label | 3551 | 12106 |
| in-depth/self-hosted-sigstore.md | how-to | everyday | content read | 1747 | 6835 |
| in-depth/shell-integration.md | explanation | — | nav label | 5990 | 16743 |
| in-depth/signing.md | explanation | — | nav label | 4459 | 13937 |
| in-depth/storage.md | explanation | — | nav label | 3457 | 11107 |
| in-depth/versioning.md | explanation | — | nav label | 1589 | 5999 |
| installation.md | how-to | first-steps | content read | 1659 | 6949 |
| reference/command-line.md | reference | — | nav label | 44056 | 172680 |
| reference/configuration.md | reference | — | nav label | 16526 | 52999 |
| reference/env-composition.md | reference | — | nav label | 4990 | 15631 |
| reference/environment.md | reference | — | nav label | 16148 | 45004 |
| reference/execution-records.md | reference | — | nav label | 5351 | 17596 |
| reference/machine-interface.md | reference | — | nav label (machine-interface already declared) | 1053 | 3631 |
| reference/metadata.md | reference | — | nav label | 6800 | 23705 |
| reference/platforms.md | reference | — | nav label | 1240 | 4663 |
| reference/script-host-api.md | reference | — | nav label | 1009 | 4942 |
| roadmap.md | explanation | — | content read | 219 | 1336 |
| user-guide.md | how-to | everyday | content read | 14008 | 44756 |
| user-guide/attestations.md | how-to | everyday | content read | 2048 | 6349 |
| user-guide/patches.md | how-to | everyday | content read | 4079 | 12343 |
| user-guide/promoting-packages.md | how-to | everyday | content read | 1017 | 3699 |
| user-guide/snapshot-tracks.md | how-to | everyday | content read (already declared) | 1456 | 5759 |

Deleted at P5 per D8: `website/src/index.md`, `website/src/team.md` (not docs pages). The new landing page `index.md` is written by P13a.

## 11. Cluster scopes

| cluster | pipeline | new pages | what it owns |
|---|---|---|---|
| first-steps | P13a | 4 | landing page, install, first package, first project; fixes the failing example package (T01); time-to-first-result proxy S-006 |
| everyday | P13b | 24 | `user-guide.md` split into guides, `user-guide/*` children, gap pages `find-packages` and `update-tools` |
| publishing | P13c | 14 | `authoring/*` renamed and split, plus the new `sign-a-release` page |
| explanation | P13d | 21 | `in-depth/*` concept pages, plus their how-to and troubleshooting children (`serve-an-index`, `self-hosted-sigstore`, `troubleshooting/shell`), plus the three pages P9 creates |
| integration | P13e | 12 | `in-depth/ci.md`, `docker.md`, and the gap pages (Bazel, devcontainers and editors, Python, proxy and CA, unattended CI, registry auth recipes, publisher CI, scripts and JSON) |
| misc | P13f | 14 | roadmap rewrite (D3), changelog include, contributing, the FAQ remainder, and the six hand-written reference families after P9 trims them |
| generated | P3 / P9 | 15 | written by the generator; no P13 pipeline edits them |

Cross-cluster content: a destination page is written by the cluster in the inventory; the old source section is deleted by the source's owner afterwards. Moves that cross clusters are the `merge into the existing section` rows of table 6.2 and the FAQ table 6.4. A page needed by two clusters stays with the cluster named in table 5 and the other links to it.

