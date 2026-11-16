# Research: Shared docs glossary with linked tooltips

## Metadata

**Date:** 2026-11-16
**Domain:** web (docs site `website/`, theme `@ocx-sh/theme`)
**Triggered by:** owner ask — "rework all documents to use a shared glossary and check that the meaning of each word is consistent across the documentation, where each usage of the word is a glossary item with a tooltip and link to the glossary page section"
**Expires:** 2027-05 (re-measure the DOM numbers on every theme release; they moved by a median 40 elements during this research)
**Companion data:** [`research_docs_glossary_terms.json`](./research_docs_glossary_terms.json) — 152 proposed terms

## Direct Answer

- **Mechanism: one remark (Sätteri mdast) plugin in `@ocx-sh/theme`**, fed by one JSON data file in the OCX site, auto-links glossary terms in built prose of every `.md`, `.mdx` and generated reference page. No source page is edited for the links. A hand-written `<Term>` per usage is rejected (11,862 edits, 58 `.md` → `.mdx`, and the generated reference pages cannot carry components).
- **Per-usage HTML is one element:** `<a class="ocx-term" href="/docs/reference/glossary/#digest">digest</a>`. The popup text is **not in the page**: one shared `popover` element is created on first hover or focus and filled from `/docs/glossary.json` (152 terms, 7.9 KB gzip, fetched once). Without JS the term is a plain link to its glossary section.
- **"Every usage" does not land.** The 900-element DOM gate (`website/lighthouse.budgets.mjs`, Lighthouse `dom-size`) fails on 14 of 58 gated pages under every usage. The densest gated page, `explanation/versioning`, goes from 933 to 1087. The nearest landable rule is **first usage of each term per page** (+25 there, to 958), the convention Wikipedia's linking guide uses. That rule still needs a DOM offset or budget decision (Open question 2), because HEAD is already over 900 on four gated pages before any glossary work.
- **Consistency is the larger job.** The crawl found 251 raw conflicts. They merge into 46 resolution clusters plus 10 factual-drift fixes. The proposed banned forms (`avoid`) hit 407 times on 81 pages, 62 of them in generated reference pages whose source is Rust doc text under `crates/`.

## Key Findings

1. **Inventory.** Six sonnet crawlers read all 114 pages in page-disjoint slices and returned 579 term rows. Merged and pruned, that is **152 terms**: ocx 28, signing 22, oci 18, platform 18, index 15, publishing 14, project 9, env 9, config 7, shell 7, security 3, interface 2. The most frequent are package (1,298 prose uses on 107 pages), tag (718), registry (639), project (505), digest (500), index (443), install (429) and pin (399). Five terms match nothing yet. Three of them are canonical forms that the rewords introduce (`env file`, `libc feature`, `state carrier`), and two live only inside today's `<Tooltip>`s (`AMFI`, `PATHEXT`).
2. **Volume under each rule**, simulated over the built HTML with the plugin's skip rules (146 pages; changelog and the SBOM page excluded): every usage 11,862 links, first per term per `##` section 5,610, first per term per page 2,655.
3. **The worst conflicts.**
   - `shim` names three unrelated things: shell env files, the Windows `.exe` launcher and the deferred-package placeholder.
   - `index` has five readings: the OCX index, the OCI image index, the local index, the index Git repository and the signing fallback index.
   - `tier` has seven: config tier, command family, toolchain tier, patch section, GC directories, ladder rung and platform support level.
   - The object store has seven names: object, package, content, local and layer store, layer cache, and "three-tier CAS".
   - Two pages say `exec` runs in an isolated environment, which is factually wrong. The default layers over the caller's environment, as `first-package.mdx:28` and `integration/python.md:39` show.
4. **The DOM gate is already red at HEAD.** Commit `ecabeba1` ("the sidebar groups pages under four captions") added a median 40 body elements per page, measured on two builds of this worktree 26 minutes apart. Four gated pages now exceed 900: `explanation/versioning` 933, `explanation/deferred-packages` 922, `guides/manage-config-centrally` 908 and `publishing/bundle-anatomy` 907. `bodycount.py` (body elements, `<template>` excluded) matches Lighthouse's `dom-size` exactly on the earlier build (local-index 860, public-index 803, deferred-packages 882), so these are Lighthouse numbers.
5. **Today's `Tooltip` costs 4 elements per use**: a span, a button, a popup span and an arrow span. It also ships an eager 770 B gzip script on every page that uses it, which `explanation/versioning` loads. 15 uses on 4 pages, and most of their terms are now glossary entries.
6. **Heading anchors cost 5 elements each**: `a > span > svg > path`, plus `span.sr-only`. That is 40 elements on `explanation/versioning`, which has 8 anchors. A 1-element anchor would free 4 per heading. That is the largest single offset in reach, and it is theme-owned.

## Conflict table

Line numbers are from the 2026-11-16 crawl. Four pages were renamed `.md` → `.mdx` during it: `guides/self-hosted-sigstore`, `integration/bazel`, `publishing/sign-a-release` and `publishing/snapshot-tracks`. Generated pages name their Rust source, because the reword lands there and `task website:reference:generate` carries it over. The banned-form lint in WP-O3 is the authority for the exact current sites. The rows below are the known ones.

| # | Term(s) | Conflict | Wins | Sites → reword |
|---|---|---|---|---|
| C1 | shim | 3 unrelated meanings | placeholder of a deferred package | `explanation/shell-integration.mdx:37` → "one env file per shell family"; `getting-started/install-ocx.mdx:32` → "per-shell `env.*` files"; `troubleshooting/shell.mdx:48` → "the command your env file already calls"; `troubleshooting/platforms.mdx:119` → "native launchers"; `explanation/entry-points.md:35` → "native launcher" |
| C2 | launcher, trampoline | fused "launcher trampoline"; synonyms | launcher = entry-point executable; trampoline = toolchain tree's `ocx exec` re-entry | `explanation/toolchain-activation.md:21`, `guides/global-toolchain.mdx:76` → "its trampoline"; `reference/project-file.md:20` (src `crates/ocx_project/src/activate.rs:23`) → "resolved by its trampoline"; `integration/devcontainers-and-editors.md:37` → "a trampoline for every tool" |
| C3 | entry point | entrypoint / entry-point / generic "way in" | "entry point" in prose; `entrypoints` only in code | `explanation/storage.mdx:144`, `guides/stable-paths.mdx:55`, `publishing/test-locally.mdx:69`, `explanation/environments.mdx:322` → "entry point(s)"; `guides/project-toolchain.mdx:371`, `explanation/project-toolchain.md:203` → "explicit command"; `explanation/environments.mdx:69` → "package command" (40 hits, 21 pages) |
| C4 | index | OCX index vs image index vs Git repo vs fallback | bare "index" = OCX index | `publishing/libc-variants.md:18` → "picks the image-index entry matching the host"; `publishing/libc-variants.md:85`, `publishing/multi-platform.mdx:69`, `guides/choose-versions.mdx:46` → "image index"; `explanation/signing.mdx:73` → "fallback tag"; `guides/choose-versions.mdx:18`, `guides/update-ocx.mdx:148` → "local index"; `publishing/announce-from-ci.md:71` → "index repository"; `guides/work-offline.mdx:83` → "the public index"; `publishing/announce-from-ci.md:58` → "public index" |
| C5 | tier | 7 meanings | config tier only (system, user, OCX home, managed) | `reference/command-line/commands.md:206` (src `crates/ocx_cli/src/command.rs:426`) → "The project counterpart"; `reference/command-line/patch.md:22` (src `command/patch.rs:25`) → "scopes to packages"; `reference/project-file.md:156` (src `crates/ocx_project/src/config.rs:166`) → "Project-level `lazy-mode`"; `integration/github-actions.md:33` → "Project example"; `reference/platforms.md:41` → column "Support level"; `guides/run-project-tools.mdx:73` → "the package command"; `explanation/environments.mdx:74` → "project commands"; `guides/lock-and-reproduce.mdx:56` → "applies to package commands"; `explanation/deferred-packages.mdx:44` → "per-package entry"; `explanation/patches.mdx:263` → "the local index pin"; `explanation/storage.mdx:148`, `explanation/toolchain-tree.mdx:93` → "blobs, layers and packages"; `publishing/maintain-patches.mdx:164` → "the `[patches]` table"; `explanation/shell-integration.mdx:181`, `reference/environment.md:280` (src `crates/ocx_env/src/vars.rs:137`) → "system-tier" |
| C6 | object store | 7 names | "object store" (the CLI help's word) | `reference/command-line/self.md:56` (src `command/self_group.rs:48`) → "object store"; `reference/command-line/commands.md:317` (src `command.rs:344`) → "The candidate and current symlinks"; `guides/corporate-mirror.mdx:56` → "object store, local index and symlinks"; `publishing/test-locally.mdx:35`, `publishing/maintain-patches.mdx:148`, `publishing/index.mdx:132`, `integration/github-actions.md:136` → "object store"; `explanation/indices.md:109` → "the object store's layers" (57 hits, 25 pages) |
| C7 | rolling tag | floating / moving / alias / ancestor / cascade alias | "rolling tag" | `reference/command-line/config.md:22` (src `command/config.rs:104`), `reference/command-line/commands.md:396` (src `command.rs:262`), `reference/command-line/package-publishing.md:158` (src `command/package_cascade_check.rs:33`), `guides/update-tools.mdx:62`, `explanation/versioning.mdx:37,125`, `publishing/build-and-push.mdx:114`, `publishing/sign-a-release.mdx:128` → "rolling tag(s)"; `guides/choose-versions.mdx:34` column value → "Rolling" |
| C8 | lock file | lockfile / lock file; mutex lock; pylock | "lock file" = `ocx.lock` | `reference/command-line/commands.md:75` (src `command.rs:151`) → "writes `ocx.lock`"; `guides/clean-up.mdx:20`, `explanation/dependencies.mdx:13`, `guides/corporate-mirror.mdx` heading → "lock file"; `explanation/local-index.mdx:31` → "never writes a mutex file"; `integration/python.md:33` → "turn its `pylock.toml` into one OCX package"; `explanation/versioning.mdx:169` → "the local index already pins the tag" |
| C9 | manifest, platform manifest | `ocx.toml` called manifest; leaf / per-platform / image manifest | manifest = OCI only; "platform manifest" | `explanation/project-toolchain.md:110`, `explanation/toolchain-activation.md:36` → "`ocx.toml`"; `explanation/local-index.mdx:114`, `publishing/multi-platform.mdx:127`, `explanation/dependencies.mdx:33`, `explanation/project-toolchain.md:45`, `publishing/build-and-push.mdx:16` → "platform manifest"; lock help text (src `command.rs:151`) → "platform-manifest digests" |
| C10 | candidate | 4 meanings | candidate symlink | `guides/manage-config-centrally.mdx:55` → "A draft `config.toml`"; `guides/lock-and-reproduce.mdx:82` → "the trial lock"; `reference/command-line/commands.md:431` (src `command.rs:431`) → "the platforms `ocx.lock` pins"; `reference/command-line/config.md:76` (src not found) → "then the payload under test" |
| C11 | current, select, active | current described as "active"; select = choose | current symlink; "active" only for `toolchain/active/` | `explanation/storage.mdx:273` → "selected package root"; `explanation/storage.mdx:298` → "you last selected"; `explanation/environments.mdx:22` → "`--self` switches to the private surface"; `reference/command-line/index.md:56` (src `crates/ocx_cli/src/app/context_options.rs:34`) → "Use the global toolchain"; `publishing/libc-variants.md:93` → "How OCX picks a build" |
| C12 | toolchain | declared set vs rendered dir vs compiler suite | set of bindings; dir = "toolchain tree" | `explanation/toolchain-render.mdx:9`, `troubleshooting/shell.mdx:106` → "toolchain tree"; `explanation/toolchain-tree.mdx:3` → "toolchain tree"; `publishing/entry-points.md:25` → "A compiler suite with fifty binaries"; `contributing/bazel.mdx:57` stays (except-listed) |
| C13 | home | index location / toolchain home / OCX home | "OCX home" = `$OCX_HOME` | `explanation/local-index.mdx:19` → "The collection location is resolved"; `explanation/toolchain-tree.mdx:76` → "every toolchain tree it writes"; `explanation/toolchain-activation.md:21` → "`<project>/.ocx/toolchain/active/bin`" |
| C14 | snapshot | index / managed / patch / build | qualified only: "snapshot build", "patch snapshot" | `explanation/managed-configuration.md:11` → "a locally cached payload"; `explanation/project-toolchain.md:6` "bundled snapshot", `explanation/indices.md:80` "frozen copy" → "shipped copy"; `publishing/multi-platform.mdx:141` → "a pin fixes one digest"; `publishing/announce.md:128` → "snapshot build"; `reference/command-line/patch.md:22` → "a patch snapshot" |
| C15 | variant | OCX variant / CPU variant / libc build / image flavour / enum | OCX variant; OCI field = "CPU variant" | `explanation/versioning.mdx:161` → "CPU variant"; `publishing/libc-variants.md:2` → title "Publish glibc and musl builds"; `integration/docker.md:18` → "Two images cover"; `reference/configuration.md:578` (src `crates/ocx_schema/src/docs/read.rs:321`) → "Selects the kind of this entry" |
| C16 | visibility | env-var "Visibility: public" (125×) vs entry/edge visibility | entry and edge visibility | `reference/environment.md:28` and every entry (generator under `crates/ocx_schema`, field of the `ocx_env` registry) → label "Audience: public" |
| C17 | surface / view / side | consumer view, self view, private view, private side | "interface surface", "private surface" | `publishing/env-surface.md:112` → "interface surface … private surface"; `explanation/patches.mdx:125` → "private surface loads"; `explanation/environments.mdx` headings "Consumer view"/"Self view" → surface names, anchors kept; `explanation/dependencies.mdx:19` → "user-facing part" |
| C18 | digest | "content digest" vs "manifest digest" for `packages[].digest` | "manifest digest" | `explanation/execution-records.md:165`, `explanation/records-launcher-frames.md:73` → "manifest digest" |
| C19 | claim | claimed unit; metadata "claim"; certificate claims | claim = first index registration of one package | `publishing/index.mdx:31` → "claim the package once"; `publishing/index.mdx:57` → "claiming the package"; `publishing/entry-points.md:152` → "The array is an assertion"; certificate claims except-listed |
| C20 | namespace | org prefix / registry domain / integrations key / consent | repository path prefix | `guides/corporate-mirror.mdx:146` → "for the registry name"; `publishing/announce.md:218` → "the `ocx.sh` registry name"; `explanation/integrations.md:11` → "keyed by a reverse-DNS key"; `explanation/environments.mdx:195` → "selects which group's names resolve" |
| C21 | registry | logical name vs storage host | registry = server; "registry name" = identifier segment | `guides/corporate-mirror.mdx:142`, `reference/configuration.md:245` (src `crates/ocx_config/src/lib.rs:90`) → "the registry name, not the storage host"; `explanation/public-index.mdx:63` → "The scope is the registry name" |
| C22 | repository | OCI repo vs Git repo | OCI repository | `publishing/build-and-push.mdx:142` → "not a source repository"; `getting-started/first-project.mdx:10` → "your project pins CMake"; `explanation/indices.md:133` → "on the index repository" |
| C23 | project, group | forge projects and groups; command group | OCX project, project group | except-listed: "GitLab project(s)", "publishing project", "GitLab group(s)", "command group(s)" |
| C24 | pin | digest pin / trust pin / pinned session / `pinned` key | "fix to one exact value" (covers digest and signer) | `explanation/shell-integration.mdx:96` → "opens a fixed-project session" |
| C25 | binding, tool, package | three words for one `ocx.toml` row | binding = row; package = artifact; tool = plain English | no reword; the definitions draw the line |
| C26 | managed block | marked / fenced / activation block | "managed block" | `guides/global-toolchain.mdx:56` → "appends a managed block"; `getting-started/install-ocx.mdx:32` → "a managed block"; `install-ocx:228` → "managed block marker" |
| C27 | trust root, trust store | Sigstore root vs TLS CA roots | trust root = Sigstore; TLS = "trust store" | `integration/proxy-and-ca.md` CA uses of "trust root" → "trust store" or "CA certificate" |
| C28 | kill switch | off-switch / opt-out | "kill switch" for `OCX_NO_CONFIG` | `guides/corporate-mirror.mdx:150` → "kill switch"; generic "opt-out" stays |
| C29 | deferred package | lazy tool / deferred tool / lazy-mode package | "deferred package" | `explanation/records-launcher-frames.md:18` → "a deferred package's shim"; `explanation/storage.mdx:81` → "deferred packages" |
| C30 | global toolchain | global tier / scope / set | "global toolchain" | `explanation/toolchain-activation.md:91`, `explanation/shell-integration.mdx:92`, `explanation/environments.mdx:169` → "global toolchain" (13 hits) |
| C31 | consent vs trust | consent framed as trust | consent (shell); trust only for signing | `explanation/shell-integration.mdx:9` → "The first question is about consent" |
| C32 | stamp | consent stamp vs render stamp | "consent stamp"; render → "render marker" | `explanation/toolchain-activation.md:81`, `reference/env-composition.md:120` → "render marker"; `explanation/toolchain-tree.mdx` heading `{#toolchain-stamp}` → "Render marker" (anchor kept) |
| C33 | shell hook | reconciler / hook | "shell hook" | `troubleshooting/shell.mdx` commands section, `explanation/shell-integration.mdx` → "shell hook" (20 hits, 7 pages) |
| C34 | sink | records sink vs `--ci` output files | records sink | `integration/unattended-ci.md:88` → "The CI files `ocx env --ci=github` writes"; `explanation/launcher-forwarding.md:59` → "the CI files" |
| C35 | bundle | publish bundle vs Sigstore bundle vs `.app` | publish bundle; "Sigstore bundle" qualified | `publishing/attest-and-sbom.mdx:52` → "pushes the Sigstore bundle"; `.app bundle` except-listed |
| C36 | descriptor | OCI descriptor vs patch descriptor | OCI; "patch descriptor" qualified | `guides/patch-packages.mdx` intro, `publishing/maintain-patches.mdx:18` → "patch descriptor" |
| C37 | mirror | `[mirrors]` vs republication vs `ocx-mirror` | `[mirrors]` endpoint | `guides/find-packages.mdx:62` → "A republished upstream package"; `reference/command-line/index-commands.md:81` (src `command/index.rs:65`) → "how a whole registry is copied"; `integration/python.md:33` → "`ocx-mirror` can instead" |
| C38 | dynamic loader | dynamic linker / ELF interpreter / ld.so | "dynamic loader" | `troubleshooting/platforms.mdx:15`, `publishing/libc-variants.md:14` → "dynamic loader" |
| C39 | libc feature | "feature tag" / "libc.glibc tags" | "libc feature" | `troubleshooting/platforms.mdx:19` → "an explicit `+libc.glibc` libc feature"; `publishing/multi-platform.mdx:47` → "libc features" |
| C40 | any platform | universal build / platformless / any | "any platform" (Mach-O "universal binary" stays) | `explanation/versioning.mdx:149` → "an `any`-platform build"; `explanation/storage.mdx:296` → "`any`-platform"; `troubleshooting/platforms.mdx:37` → "static builds and macOS universal binaries" |
| C41 | platform | column meaning OS only | os/arch pair | `getting-started/install-ocx.mdx:175` → column "OS" |
| C42 | materialize | British spelling | "materialize" | `reference/environment.md:320` (src `crates/ocx_env/src/vars.rs:230`), `explanation/records-launcher-frames.md:57` → "materializ…" |
| C43 | image index | "Image Index", "image-index" | "image index"; "OCI Image Index" only as spec link text | 13 capitalised hits on 10 pages → "image index" |
| C44 | root document | "index root" (30) vs "root document" (35) | "root document" | 29 hits on 14 pages, 11 in generated pages → "root document" |
| C45 | owner, maintainer, operator | three personas blur | operator = admin; owner = index `login:id`; publisher = pusher | `guides/patch-packages.mdx:15`, `publishing/maintain-patches.mdx:12` → "operator" |
| C46 | OCX vs ocx | product name casing mixed (216 uses) | "OCX" product in prose, `ocx` only as the command in code | style rule for `docs-style.md`; not a glossary term |

### Factual drift (fix regardless of the glossary)

| # | Site | Wrong | Fix |
|---|---|---|---|
| D1 | `getting-started/first-package.mdx:28` | "isolated environment that holds only the variables the package declares" | the package environment layered over yours; `--clean` isolates it |
| D2 | `integration/python.md:39` | "runs it in a clean environment" | runs it with the package environment layered over yours |
| D3 | `roadmap.md:29,49,50,64` | toolchain config, OCI referrers, SBOM attachment, shims marked Planned | mark shipped, link the guide |
| D4 | `explanation/indices.md:46` | "The remote index is the live OCI registry" | the live source: a registry, or an index service such as `index.ocx.sh` |
| D5 | `explanation/versioning.mdx:42` | mapping "only changes when `ocx index update` runs" | changes only when a command refreshes it (`ocx index update`, a `--remote` resolve) |
| D6 | `explanation/storage.mdx:248` | local index "reflects the state of that remote registry" | records what this host last resolved from that source |
| D7 | `publishing/index.mdx:8,111`, `publishing/bundle-anatomy.mdx:3` | "deterministic archive" | "archive" (reruns produce different digests, `bundle-anatomy.mdx:34`) |
| D8 | `reference/command-line/commands.md:352` (src `command.rs:352`) | `ocx remove <IDENTIFIER>` accepts binding names, which `:140` says are not identifiers | value name `<NAME>` or `<NAME|IDENTIFIER>`; a `cli.json` change, so it needs a contract-ledger row |
| D9 | C18 | records "content digest" | "manifest digest" |
| D10 | C19 | "claim the namespace" | "claim the package" |

## Mechanism evaluation

### Options

| Criterion | 1. Auto-wrap remark plugin + data file | 2. Hand-written `<Term>` per usage | 3. Hybrid (auto + `<Term>` override) |
|---|---|---|---|
| Source edits | 0 pages | 2,655 (first-per-page) to 11,862 (every) inline edits; 58 `.md` → `.mdx` | 0, plus rare overrides |
| Generated reference pages | covered, because the plugin sees built markdown | needs `crates/ocx_schema` to emit MDX components | covered |
| docs-quality prose checks (read source) | untouched | every sentence gains markup; `strip_prose.py` strips the tag, so prose length stays the same but each diff touches thousands of lines | untouched |
| Consistency enforcement | data file is the single list; lint on `avoid` forms | per-author discipline; no single list | as 1 |
| False positives | controlled by the variants list (no stemming) and `except` phrases | none | as 1, plus manual escape |
| Verdict | **recommended** | rejected | not now: the override is YAGNI until a real miss shows up; the data model (`variants`, `except`) absorbs every case the crawl found |

Where it lives: **the theme** (`packages/theme/src/markdown/glossary.mjs` plus the client script), not `website/plugins/`. The theme already owns the Sätteri plugin chain (`addMarkdownPlugins`), the chrome script that hosts the delegated trigger, the popover CSS tokens, the a11y and budget harness (Vitest, Playwright, Lighthouse 100×4), and the other ocx.sh sites that can reuse it. OCX keeps only the data, the generated glossary page and the lint.

### DOM arithmetic (Lighthouse `dom-size`, body elements)

Baseline: build of HEAD `ecabeba1`, 2026-11-16 02:03. Each new term adds exactly one element (`<a>`). Text nodes are free, and the popup does not exist until interaction.

Gated pages (58, `taskfiles/ci.taskfile.yml` `docs`), the ten closest to the limit under first-per-page:

| Page | Now | + every / section / page | = every / section / page |
|---|---|---|---|
| `explanation/versioning` | 933 | +154 / +71 / +25 | 1087 / 1004 / 958 |
| `explanation/deferred-packages` | 922 | +104 / +47 / +21 | 1026 / 969 / 943 |
| `explanation/local-index` | 900 | +189 / +42 / +34 | 1089 / 942 / 934 |
| `guides/manage-config-centrally` | 908 | +78 / +46 / +19 | 986 / 954 / 927 |
| `publishing/declare-dependencies` | 899 | +83 / +40 / +27 | 982 / 939 / 926 |
| `publishing/bundle-anatomy` | 907 | +43 / +30 / +14 | 950 / 937 / 921 |
| `publishing` | 887 | +56 / +37 / +25 | 943 / 924 / 912 |
| `publishing/libc-variants` | 892 | +79 / +25 / +20 | 971 / 917 / 912 |
| `guides/update-tools` | 894 | +89 / +42 / +17 | 983 / 936 / 911 |
| `reference/command-line/self` | 884 | +93 / +37 / +24 | 977 / 921 / 908 |

| Limit | Over today | every usage | first per section | first per page |
|---|---|---|---|---|
| 900 (site budget) | 4 | 14 | 13 | 10 |
| 925 (theme `content` class) | 1 | 14 | 8 | 5 |

On the build of 01:37, before `ecabeba1`, the same rules put 13, 5 and 2 pages over 900. The sidebar change alone cost about what first-per-page costs.

The ten densest pages overall are all ungated today, because they already miss the budget:

| Page | Now | + every / section / page |
|---|---|---|
| `reference/command-line/package-publishing` | 2095 | +638 / +238 / +74 |
| `reference/exit-codes` | 2817 | +560 / +216 / +76 |
| `reference/command-line/package-consuming` | 1712 | +455 / +187 / +74 |
| `reference/command-line/commands` | 1825 | +381 / +162 / +40 |
| `reference/environment` | 3515 | +336 / +295 / +61 |
| `explanation/cosign-parity` | 1321 | +309 / +108 / +38 |
| `explanation/project-toolchain` | 1408 | +276 / +102 / +42 |
| `guides/project-toolchain` | 2072 | +251 / +119 / +45 |
| `publishing/announce` | 1125 | +248 / +100 / +31 |
| `explanation/signing` | 1433 | +238 / +109 / +39 |

Offsets in reach, measured on `explanation/versioning`:

- Retiring its 2 `Tooltip`s saves 6 elements (4 each becomes 1).
- A 1-element heading anchor saves 32: 8 anchors at 5 elements each, down to 1 each. The icon becomes a CSS mask, which needs an exception to the theme's "UI icons are inline SVG" rule.
- With both offsets, first-per-page lands that page at 920. That fits 925 and misses 900. Every-usage and per-section do not land on any combination measured.

Accessibility also argues for first-per-page. Every usage would add 638 tab stops to `package-publishing`, and a screen reader would announce "link" 154 times on `versioning`.

### Per-usage HTML and popup delivery

Per usage, ONE element:

```html
<a class="ocx-term" href="/docs/reference/glossary/#digest">digest</a>
```

No `data-*`, no `title`, no `aria-describedby` in the HTML. The id comes from the `href` fragment, and the visible text is the source text unchanged.

| Popup text delivery | DOM | Page bytes | First-open latency | Verdict |
|---|---|---|---|---|
| Hidden span per usage (today's `Tooltip`) | 4 per usage | definition × N | 0 | rejected |
| `title` attribute | 1 | definition × N | native delay | rejected: no keyboard or touch path, double read by screen readers (DOC-EX-34) |
| `data-def` per usage | 1 | ~150 B × N | 0 | rejected on bytes |
| Inline `<script type="application/json">` table per page | 1 + 1 | 1–4 KB gzip | 0 | runner-up |
| CSS Custom Highlight on plain text | 0 | 0 | none | rejected: no focus target, no no-JS link |
| **Fetched `/docs/glossary.json`** `{id: [term, definition]}` | **1** | **0** | one fetch (7.9 KB gzip) on first hover or focus, inside the 400 ms hover delay, cached after that | **recommended** |

Behaviour, held to DOC-EX-34 and WCAG 1.4.13:

- **Trigger.** A delegated `pointerover` / `focusin` listener on `document`, added by the theme's existing `chrome.mjs`, imports `term-tip.mjs` on the first event whose target is inside `a.ocx-term`. Delegation also covers deferred fragments with no `onRoots` scan.
- **Popup.** `term-tip.mjs` creates one `<div id="ocx-term-tip" role="tooltip" popover="manual">`, positioned with CSS anchor positioning the way `Tooltip.astro` does it. While a term's popup is open, the script sets `aria-describedby="ocx-term-tip"` on that link and removes it on close. The popup holds the term in bold and the definition only, with no interactive content. The learn-more link lives in the glossary section.
- **Open and close.**
  - Focus opens the popup at once. Hover opens it after 400 ms, and moving the pointer onto the popup keeps it open (100 ms grace).
  - Escape closes it and focus stays on the link. Blur and pointer-leave close it, unless the link is still focused.
  - A scroll closes a popup opened by hover, but not one opened by focus. Only one popup is open at a time.
- **No JS, or the fetch fails.** The term is a plain link to `/docs/reference/glossary/#id` and needs nothing else. A tap on touch follows the link, because touch has no hover.
- **Reduced motion.** The popup uses the theme's duration tokens, which `tokens.css` already zeroes under `prefers-reduced-motion`.
- **Byte cost**, measured on an unminified prototype:
  - Delegator: ~0.3 KB gzip in the shared chrome. This counts toward the 11,264 B `preJsGz` budget, where pages use ~8.3–9.1 KB today.
  - `.ocx-term` style (inherit colour, dashed underline): ~0.1 KB gzip in the render-blocking shared sheet. The theme measures `starlight` at 22,528 of 22,550 B, so this needs a Spec Delta of about +100 B.
  - After interaction only: `term-tip.mjs` ~1.0 KB gzip, its CSS ~0.4 KB gzip and `glossary.json` 7.9 KB gzip. Lighthouse never triggers them.
  - Each link adds ~65 B raw and ~0.4 KB gzip per 25 links, far inside the 43,400 B HTML cap.

### Matching rules

| Rule | Decision |
|---|---|
| Scope | text nodes in mdast prose of every docs page; options `exclude` (default `changelog/`, `reference/dependencies/`) and the glossary page itself |
| Skipped ancestors | heading; `link`, `linkReference`, `definition`; `image`/`imageReference` alt; `inlineCode`/`code` (no text children); raw `html`; aside and `:::` label paragraphs (`data.directiveLabel`); every `mdxJsx*Element` except an allowlist (`Steps`, `Tabs`, `TabItem`, `Aside`, `details`) — so `Tree`, `ShellSession`, `Terminal`, `Tooltip`, `InstallCommand` content is never touched; component props are attributes, never text; frontmatter is not in the tree |
| What matches | only `variants` (explicit inflections, no stemming); the canonical `term` is display text |
| Case | a variant with an uppercase letter matches exactly (`OCI`, `Sigstore`, `SET`); an all-lowercase variant matches case-insensitively (`Package` at sentence start); output keeps the source text |
| Longest first | one alternation over all variants and `except` phrases, sorted by length; `image index` beats `index`, `pull request` (a `change-request` variant) beats `pull`; an `except` phrase consumes its text and emits nothing |
| Word boundary | left: not after a letter, digit, `_ - . / @ $ : #`; right: not before a letter, digit, `_ - / @`, nor `.` + letter — so `multi-platform`, `ocx-mirror`, `index.ocx.sh`, `$OCX_HOME`, `--platform`, `metadata.json` never match |
| Frequency | **first usage of each term per page** (open question 1); later usages stay text |
| Limits | a term split across nodes (`entry *point*`) is not matched; a homonym used in its other sense is linked wrongly until reworded, which is why the conflict table comes first |
| Cache | the glossary data hash joins the theme's `@ocx-sh/theme/cache-key` hast entry, so editing the data re-renders cached pages |

### Interplay with existing gates

- **docs-quality checks** (`.claude/rules/docs-quality/checks/*.py`) read source only. The plugin leaves the source untouched, so `prose.py`, `page_type.py`, `links_raw.py` and DOC-NAV-17 see no change.
- **lychee** (`task website:lint:links`, `--include-fragments`, offline over `dist`) checks every rendered term `href`, and every glossary-page "See" link. A data term without a section, or a dead learn-more target, turns it red with no new code.
- **Pagefind** indexes the unchanged link text only. The popup never exists at build, `glossary.json` is not HTML, and `<script>` is skipped. The glossary page is indexed, and its `h3` ids give sub-results. Risk: a glossary entry may outrank the explanation page for a one-word query. Each entry's "See" line keeps the path one click away.
- **`test_reference_shape.py`** applies to the generated glossary page (`doc_type: reference`). Each entry is `### Term {#id}`, a definition of at most 2 sentences and one `See [Page](…)` line, and the page fits the shape by construction.

### Glossary page

The page is generated, not hand-written: `website/src/content/docs/reference/glossary.md`. It is committed and drift-checked like the five generated reference pages, by `website:glossary:generate` and `:check`, and the theme's `glossaryMarkdown(terms)` renders it.

- **Layout.** `## Category {#category}` sections, each holding `### Term {#id}` entries. The frontmatter sets `tableOfContents.maxHeadingLevel: 2`, so the TOC lists 12 categories, not 152 terms.
- **Cost.** About 9 elements per entry, or about 1,800 elements per page. The page stays off the gated list, like the 53 pages already off it. A `<dl>` layout would halve that, but it loses Pagefind sub-results and the reference-shape contract.

## Recommendation

Build option 1 in the theme with first-per-page frequency and fetched definitions, then land the OCX data, page, lint and rewords. Per-usage HTML is the single `<a class="ocx-term">` above. Resolve the DOM decision (open question 2) before the OCX site pins the theme release. Otherwise the four already-red gated pages plus ten more fail the `docs` CI tier.

## Work packages

The DAG: T1 → T2 → T3; T4 runs in parallel with T1–T3, against the contract fixed here (the markup above and `glossary.json` = `{id: [term, definition]}`); T5 follows T3 and T4. O1 can start now. O2 needs the theme release. O3 needs O1 and O2. O4a–d need only the approved table and run in parallel with everything. O5 and O6 depend on the owner's answers.

### Theme (`ocx-sh/website`, `packages/theme`, one minor release)

| WP | Files | Content | Verification |
|---|---|---|---|
| T1 contract | `src/glossary/{schema.json,matcher.mjs,validate.mjs,markdown.mjs}` + `.d.mts` (prepack) | term-row JSON Schema (as `research_docs_glossary_terms.json`); `matcher(terms)` (rules above, shared by plugin and OCX lint); `validate` (unique ids and variants, ≤25-word definitions, learn-more format); `glossaryMarkdown(terms)` | Vitest: boundary cases (`multi-platform`, `ocx-mirror`, `index.ocx.sh`, `$OCX_HOME`, `--platform`), longest-first, `except`, case rule; validator red on dup id, dup variant, 26-word definition; markdown snapshot; `task dts:check` |
| T2 remark | `src/markdown/glossary.mjs`, `src/markdown/index.mjs` | Sätteri mdast plugin; wired after `includes`/`widgets`, before `mdLinks`, only when configured; per-document first-per-page state; link node `{type:'link', data:{hProperties:{className:['ocx-term']}}}` | Container tests on `.md` and `.mdx` fixtures: exactly one `<a class="ocx-term">` per first usage, none in each skipped context, none on the glossary page or excluded routes |
| T3 wiring | `src/starlight/index.mjs`, `src/glossary/route.mjs` | `ocxTheme({ glossary: { terms, page, exclude } })`; prerendered route `<base>glossary.json`; data hash in the cache-key entry | build fixture emits `glossary.json` equal to the data; changing a definition re-renders a cached page |
| T4 client | `src/components/term-tip.mjs`, `src/components/term-tip.css`, `src/chrome.mjs`, `src/starlight/starlight.css` | delegator, shared popover, `.ocx-term` style | jsdom: focus opens at once, hover after 400 ms, popup hoverable, Escape keeps focus, blur and scroll close, one open, `aria-describedby` toggles, fetch failure leaves the link; Playwright: keyboard Tab + Escape, no-JS navigation, reduced motion; budgets: `preJsGz` delta, shared-sheet Spec Delta; `deferral-contract.test.ts` accepts the document-level delegation |
| T5 docs | `examples/starlight/src/content/docs/components/glossary.mdx`, `src/stories/glossary/default.mdx`, `skills/ocx-theme-components/**`, `RELEASING.md` flow | demo page and story; skill names the `glossary` option (`skills-coverage.test.ts`) | `task check`, `task e2e`, `task lighthouse:changed` 100×4 on the demo page |

### OCX (this repo)

| WP | Files | Content | Verification |
|---|---|---|---|
| O1 data | `website/src/data/glossary.json` | reviewed copy of `research_docs_glossary_terms.json` | theme `validate` in O3 test |
| O2 wiring and page | `website/package.json`, `website/pnpm-workspace.yaml`, `.github/workflows/deploy-docs.yml` (the three-file repin), `website/astro.config.mjs` (option, sidebar Reference → Glossary), `website/scripts/glossary.mjs`, `website/taskfile.yml` (`glossary:generate`, `glossary:check` inside `website:check`), `website/src/content/docs/reference/glossary.md` (generated) | — | `task website:build`; `task website:lint:links`; `bodycount.py` over the 58 gated pages ≤ budget |
| O3 honesty lint | `website/tests/glossary.test.mjs` (`task website:test`) | data valid; every internal `learnMore` resolves to a page and an explicit `{#anchor}`; glossary page fresh; `avoid` forms in source prose (fences, inline code, link targets stripped, same matcher so accepted variants win) reported as `page:line form → term` | red on a planted banned form and a dead anchor, green on the tree after O4; rollout per docs-quality: error on changed files first, whole tree after O4 |
| O4a–d rewords | a: `getting-started/**`, `guides/**`, `roadmap.md`, `index.mdx`, `security.md`; b: `explanation/**`; c: `publishing/**`, `troubleshooting/**`, `integration/**`, hand-written `reference/**`, `contributing/**`; d: the `crates/**` sources named in the table, then `task website:reference:generate` | the conflict table; D1–D7 as separate commits (D8 is an interface change with its own ledger row) | O3 lint green per slice; `task website:reference:check` for d |
| O5 Tooltip retirement | `explanation/storage.mdx`, `explanation/versioning.mdx`, `explanation/dependencies.mdx`, `troubleshooting/platforms.mdx`; `.claude/rules/docs-style.md`, `.claude/rules/subsystem-website.md` (`chore:`) | 15 `<Tooltip>` → plain prose, auto-linked; docs rules describe the glossary instead of per-use tooltips | `task claude:tests`; DOM delta on `versioning` (−6) |
| O6 budget | `website/lighthouse.budgets.mjs` | only if approved: `domElements` 900 → 925 | CI `docs` tier green on the gated list |

Commit subject for the user-facing change, which is the changelog line: `docs: terms across the docs link to one glossary and show their definition on hover or focus`.

## Open Questions

1. **Frequency rule.** You asked for every usage. That puts 14 of 58 gated pages over the DOM budget (`versioning` 1087), and it adds up to 638 tab stops on one page. *Recommendation:* link the first usage of each term per page. Per section is the next step if the budget ever allows it: 13 pages over 900 today.
2. **DOM headroom.** HEAD `ecabeba1` already has 4 gated pages over 900. First-per-page leaves 10 over 900 and 5 over 925. *Recommendation:* raise the site budget to 925, the theme `content` class that the site may not exceed. Also allow a 1-element heading anchor in the theme, which saves 4 elements per heading. Fold the shared-sheet Spec Delta (+100 B) into the same theme change. The fallback is to drop the failing pages from the gated list until trimmed.
3. **Canonical picks.** Approve the table, notably:
   - "object store", not "package store"; "lock file", not "lockfile"; "platform manifest", not "leaf manifest"; "rolling tag"; "root document", not "index root".
   - `shim` only for the deferred placeholder; `tier` only for config tiers.
   - The table touches 81 pages and ~25 Rust doc strings behind the generated pages. *Recommendation:* approve as written.
4. **Retire the hand-written `<Tooltip>` in OCX docs**, 15 uses, in favour of glossary terms. *Recommendation:* yes. It saves 3 elements per use plus an eager 770 B script, and it leaves one tooltip mechanism. The component stays in the theme for other sites.
5. **Glossary URL.** *Recommendation:* `/docs/reference/glossary/`, in the Reference group, since the page is `doc_type: reference`. The alternative is a top-level `/docs/glossary/`.

## Prior art and sources

| Source | Type | Relevance |
|---|---|---|
| [MkDocs Material: adding a glossary](https://squidfunk.github.io/mkdocs-material/reference/tooltips/#adding-a-glossary) | Docs | same model: one glossary file auto-appended, every match becomes an `abbr` tooltip; cited in `docs-quality/examples.md` § Tooltips and glossaries |
| [Wikipedia MOS: duplicate and repeat links](https://en.wikipedia.org/wiki/Wikipedia:Manual_of_Style/Linking#Duplicate_and_repeat_links) | Style guide | a term is linked once, at its first occurrence; the basis for open question 1 |
| [WCAG 2.1 SC 1.4.13 Content on Hover or Focus](https://www.w3.org/WAI/WCAG21/Understanding/content-on-hover-or-focus.html) | Standard | dismissible, hoverable, persistent; DOC-EX-34 |
| [Pagefind: indexing](https://pagefind.app/docs/indexing/) | Docs | `<script>` and `<form>` are never indexed; `data-pagefind-ignore` |
| [Lighthouse: avoid an excessive DOM size](https://developer.chrome.com/docs/lighthouse/performance/dom-size) | Docs | the `dom-size` audit the 900 budget asserts on |
| `packages/theme/src/components/Tooltip.astro`, `tooltip.mjs` | Code | the existing popover, positioning and DOC-EX-34 behaviour the shared popup reuses |
| `packages/theme/src/markdown/{index,widgets,md-links}.mjs`, `satteri` 0.11 `MdastVisitorContext` (`parent`, `replaceNode`, `fileURL`) | Code | the plugin API the matcher runs in |
| `website/lighthouse.budgets.mjs`, theme `tests/budgets.mjs` | Code | DOM 900 (site) and 925 (theme `content`), `preJsGz` 11 KiB, shared sheet 22,550 B |
