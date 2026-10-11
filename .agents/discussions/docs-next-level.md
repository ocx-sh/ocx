# Discussion: OCX docs to the next level (Starlight, use-case focus, lean reference)

State: handed-off → loop · Updated: 2026-10-11
Ratified: 2026-10-11 → loop
Amended: 2026-10-11 — owner added phase 7 (cleanup of everything moved to ocx-website), whole-corpus docs-quality review with non-regression, and a final parallel haiku completeness sweep, after ratification
Confidence: owner ratified (explicit yes, 2026-10-11); research vintages 2026-10-11 (5 lanes, `.agents/research/research_{reference-vs-explanation,docs-reference-recon,starlight-reference-generation,theme-gap-ocx-docs,docs-use-case-inventory}.md`)

## Intent

Owner wants an autonomous loop that takes the OCX docs (`website/`, VitePress) to the
next level:

- Adopt the OCX website theme (`@ocx-sh/theme`, repo `ocx-sh/website`, local
  `../ocx-website`) — this is phase 3 of `../ocx-website/HANDOVER.md`. Improving the
  theme via a PR on an ocx-website branch and pinning ocx docs to that commit is
  explicitly fine.
- Docs stay fully complete (guides + full reference), but become more use-case and
  problem-solving focused, inside the new unified site (`ocx.sh/docs/`).
- Reference pages are bloated. Fix now and persist as AI config: a reference is only a
  reference — incl. `reference/environment.md` — no explanation, at most a link to one.
  Every full explanatory section in a reference page is challenged: needed at all, or
  moved to a guide/explanation page.

Use `/docs-plan` (skill) for the IA/use-case restructure.

## Requirements

Phases (loop order; each lands as its own PR or commit series):

1. **AI config first.** Sharpen the existing reference rule
   (`.claude/rules/docs-quality/page-types.md` DOC-TYPE-04/05/18/19) to an explicit entry
   shape — name, signature/type, default, one-line description, at most a link to an
   explanation; no explanatory sections, conventions or rationale on a reference page.
   Mirror the operative line in `.claude/rules/docs-style.md` and
   `.claude/rules/subsystem-website.md`. Wire the docs-quality `doc_declaration` /
   `page_type` checks into ocx verify (skill `docs-instrument`: changed-files red first,
   whole tree after backfill); every page declares `doc_type` / `doc_tier`.
   Propose the same sharpening to the shared docs-quality source (grim-published) as a
   PR there. Update `.claude/rules/product-tech-strategy.md`, `docs-style.md`,
   `quality-vite.md`, `subsystem-website.md` for Starlight + pnpm.
2. **Theme PR** on `ocx-sh/website` (local `../ocx-website`, `packages/theme/`):
   explicit `{#id}` heading anchors, lazy cast prefetch, roadmap component with
   allowed icons (or rewrite `website/src/docs/roadmap.md`), optional remark plugin for
   `.md`-relative links and `<<<` includes. ocx pins the PR's latest SHA
   (`pnpm add 'github:ocx-sh/website#<sha>&path:/packages/theme'` + `allowBuilds`).
   Theme hardening deferred.
3. **Mechanical port** of `website/` from VitePress 2 alpha to Astro 7 + Starlight 0.42
   with `@ocx-sh/theme/starlight`, base `/docs/`, starting from
   `../ocx-website/scripts/samples/convert.ts`. `/schemas/` as second dist. Update every
   docs reader: `test/lint/test_doc_command_reference.py`,
   `test/lint/test_env_reference_coverage.py`, `test/lint/test_doc_binding.py`,
   `test/lint/test_doc_project_toolchain.py`, `test/doc_scripts/cast.bzl`,
   `test/BUILD.bazel` data, `website/taskfile.yml` (lychee), `website/scripts/`.
   Changelog page keeps including `CHANGELOG.md` (never edited).
4. **Generated reference.** A generator (Rust/task step) writes the CLI reference (one
   page per command group), `environment.md` and the config reference from the CLI
   definition, the `ocx_env` registry and the config JSON Schema, with a `--check` drift
   gate in verify. Long text lives in the source doc/help strings
   (`.claude/rules/quality-cli-help.md` two-register). Explanatory sections leave the
   reference: moved to the matching explanation/how-to page or deleted when duplicate
   (`configuration.md` precedence/kill switch, `ocx.toml` narrative, `[[trust.policy]]`,
   `[shell]`, Future Config Keys; `environment.md` Truthy Values, Shell Activation Files,
   `__OCX_ENV_*`, repeated `apply_ocx_config` paragraphs). Hand-written reference pages
   (`metadata.md`, `env-composition.md`, `execution-records.md`, …) trimmed to the same
   entry shape. Old anchors keep resolving via redirects.
5. **Use-case IA** via `/docs-plan`: three tiers (first steps, everyday, integration),
   task-first pages titled by goal, `user-guide.md` (1641 lines) split, `faq.md`
   recipes promoted, gaps from the use-case inventory filled where the feature exists.
   Docs stay complete: every concept keeps one home.
6. **Production switch.** `deploy-website.yml` → ocx-website deploy action to Bunny
   `/docs/` + `/schemas/`; legacy edge rule removed; old URLs redirect.
7. **Cleanup of everything moved to ocx-website.** Once a surface is served from
   `ocx-sh/website`, the ocx copy is deleted, not left dormant:
   - root-site content that belongs to the `/` claim: `website/src/index.md` (landing,
     `FeatureSection`), `website/src/team.md` — first confirm the root site
     (`../ocx-website/site/`) carries an equivalent; port any missing piece there in the
     theme/site PR, then delete;
   - the VitePress tree: `website/.vitepress/` (config, 21 Vue components, theme),
     `website/bun.lock`, VitePress-only scripts and Bazel/task targets
     (`website/site.bzl`, `website/BUILD.bazel`, `website/taskfile.yml` and sibling
     taskfiles where they only serve VitePress);
   - Cloudflare Pages deploy: `.github/workflows/deploy-website.yml`,
     `.github/workflows/deploy-website-release.yml`, and Pages references in
     `taskfiles/infra.taskfile.yml`, `taskfiles/release.taskfile.yml`;
   - AI config that describes the old site: `.claude/rules/product-tech-strategy.md`,
     `docs-style.md`, `subsystem-website.md`, `subsystem-taskfiles.md`,
     `workflow-github.md`, `arch-principles.md`, `meta-ai-config.md`, `quality-vite.md`
     (delete or rewrite for Starlight), plus `.claude/rules.md` catalog rows and
     `CLAUDE.md` mentions, in the same commit (`task claude:tests`);
   - superseded hex state: `.agents/discussions/unified-web-presence.md` (parked,
     superseded by `../ocx-website/HANDOVER.md`).
   Not cleanup targets: `.github/workflows/deploy-dev.yml` (publishes binaries to
   `dev.ocx.sh`, not the website) and the vendored generator-agnostic
   `.claude/rules/docs-quality/**` and `.claude/skills/docs-instrument/**`.

Cross-cutting (binds every content phase, 3–5):

- **Whole-corpus review, no grandfathering.** Every published page — not only the
  reference — is reviewed against the current docs guideline as installed via the rules
  (`.claude/rules/docs-quality.md` + `docs-quality/**`, `docs-style.md`; skill
  `docs-review`), and re-laid-out or rewritten where the guideline or the new IA demands
  it. A page is never carried over unreviewed because it already exists.
- **Non-regression.** Before any page is moved, split or rewritten, snapshot a baseline:
  per-page docs-review grade, and a content inventory (every fact, example, anchor,
  command, flag, env var, config key, error/exit code mentioned). After: each page's
  grade is equal or better, and every inventory item has a new home or a recorded,
  reasoned deletion (duplicate, obsolete, speculative). Nothing disappears silently.
- **Final completeness sweep (phase 8).** Parallel `haiku` subagents, one per page or
  reference family, each checking a narrow slice: (a) code coverage — every visible
  command/flag, `OCX_*` variable, config key, metadata field, exit code and slug has an
  entry; (b) baseline coverage — every inventory item from the non-regression snapshot
  is present or has a recorded deletion; (c) use-case coverage — every goal in
  `.agents/research/research_docs-use-case-inventory.md` whose feature exists has a
  page; (d) guideline — the page passes the docs-quality gate. Findings feed back into a
  fix round; the sweep repeats until a round returns nothing new.

Out of scope: the root site, catalog and other consumers' docs; apex DNS cutover;
theme hardening (deferred); new product features for uncovered goals (e.g.
[ocx-sh/ocx#211](https://github.com/ocx-sh/ocx/issues/211)); hand edits to `CHANGELOG.md`.

## Decisions

- **Sequencing: move first, then content.** One mechanical PR ports `website/` to
  Starlight + `@ocx-sh/theme` (theme pinned to a commit); all content work follows on
  Starlight, so code-groups/containers are rewritten once.
- **Rule home: ocx rule + shared docs-quality.** Operative line in ocx
  `.claude/rules/docs-style.md` / `subsystem-website.md`, plus a proposed change to the
  shared docs-quality rule set so other OCX doc sites inherit it.
- **Autonomy: fully autonomous.** Loop decides IA, merges and deletions itself; owner
  reviews PRs. (Accepted counter-argument: page deletions/merges are the least
  reversible step; mitigated by git history and redirects.)
- **Reference source: generated.** CLI, env and config reference pages are generated
  from code sources (CLI definition / `cli.json`, `ocx_env` registry, config JSON
  Schema) with a drift `--check`; prose quality moves into the source help/doc texts.
- **CLI reference split: one page per command group** (each ≤15 items, DOC-TYPE-19);
  old anchors redirected.
- **Theme pin:** if ocx docs need no theme change, land on ocx-website `main` first and
  pin that SHA. If they need changes (new attributes/components — recon says they do:
  `{#id}` anchors, lazy cast prefetch, roadmap), open a PR on ocx-website, pin its latest
  commit and keep going; theme hardening runs later (owner may run it in a separate
  agent). D13 of `plan_phase3-pilots.md` is waived for this loop.
- **Deploy scope: including the production switch.** The loop moves `/docs/` and
  `/schemas/` from Cloudflare Pages (`.github/workflows/deploy-website.yml`) to Bunny via
  the ocx-website deploy action and retires the legacy edge rule. Owner grant for this
  loop overrides the standing "no deploy workflows" default. (Counter-argument named:
  production then rides a theme commit that may not be on ocx-website `main` — see
  Open questions.)
- **Ecosystem: ocx side + link.** ocx docs cover what ocx itself does (CI env export,
  `ocx_python` locks, …) and link to `/integrations/<x>/`; integration guides stay in
  their repos.
- **Toolchain: node + pnpm 11 like the pilots**; `.claude/rules/product-tech-strategy.md`
  and `.claude/rules/subsystem-website.md` updated to Starlight + pnpm.
- **Bloat fix doubles as a Lighthouse requirement:** theme budget is 14,200 B gzip HTML
  per page; 10 docs pages exceed it today.

## Research

- `.agents/research/research_reference-vs-explanation.md` — Diátaxis + 11 tools; uniform
  references are source-generated with a CI drift check; hybrid (generated facts,
  hand-written sidecar text) is the norm; no Starlight CLI/JSON-Schema plugin.
- `.agents/research/research_docs-reference-recon.md` — reference pages hand-written; CLI/env
  structurally linted vs `cli.json`; reference rule exists (DOC-TYPE-04/05/18/19) but no gate
  runs it; theme SHA-pin + D13 merge constraint.
- `.agents/research/research_starlight-reference-generation.md` — `cli.json` golden-only and
  thin (summaries only); no Starlight CLI/schema plugin; loader vs task-step vs lint.
- `.agents/research/research_theme-gap-ocx-docs.md` — 27 features mapped; theme PR needed
  for `{#id}` anchors, lazy casts, roadmap; Lighthouse page-size cap.
- `.agents/research/research_docs-use-case-inventory.md` — 40 reader goals; gaps: ecosystem
  (Bazel/devcontainer/Python), network hostility, publisher CI.

## Related

- `../ocx-website/HANDOVER.md` — unified web presence, phase 3 = consumers onto library
- `../ocx-website/.agents/plans/plan_phase3-pilots.md`
- `.agents/discussions/unified-web-presence.md` (parked, superseded by handover)
- Reference pages: `website/src/docs/reference/command-line.md` (6713 lines),
  `configuration.md` (2305), `environment.md` (1490), `metadata.md` (1150)

## Open questions

- [NEEDS CLARIFICATION: switch production while ocx pins a theme PR SHA not on ocx-website `main`?]
  Recommended: hold phase 6 until the theme PR is on `main` and repin — production should
  not serve an unhardened, unmerged theme commit.
- [NEEDS CLARIFICATION: generator mechanism — Astro content loader or Rust/task step writing Markdown?]
  Recommended: Rust/task step (Bazel-cached) — full `ocx_env` docs and clap long help are
  reachable only from Rust; `cli.json` is golden-only and carries summaries only.
- [NEEDS CLARIFICATION: Bunny edge-rule removal needs the owner-local account key.]
  Recommended: loop runs `task bunny:*` locally if `BUNNY_API_KEY` is present, else
  hands it back as one Actions line.

## Verification

- `task verify` green at each phase end; `task verify:scoped --force` per step.
- Generator `--check` shown both red (mutated source text) and green; reader floors on
  commands, flags, env vars and config keys (no fewer than today's lint floors 70/280/12/50).
- `doc_declaration` / `page_type` gate green on every page; no reference page carries an
  explanatory section.
- Every built page within the theme's HTML budget (14,200 B gzip); `ocx-site check` green;
  Lighthouse CI 100/100/100/100 on representative pages (incl. one per reference family).
- lychee with `--include-fragments` over the built site green; old URLs and anchors
  resolve (301/redirect).
- Per content page: docs-review grade after ≥ baseline; content-inventory diff shows
  every baseline item kept or deleted with a recorded reason.
- Phase 8: the parallel haiku sweep's last round reports zero new gaps across (a)–(d);
  each sweep agent states how much it read (reader floor), so a stopped reader cannot
  pass as clean.
- After phase 7: `grep -rIl -i -e vitepress -e pages.dev -e cloudflare` over the repo,
  excluding `.claude/rules/docs-quality/**`, `.claude/skills/docs-instrument/**`,
  `external/`, `CHANGELOG.md` and git history, returns nothing (proven non-empty
  before the cleanup); `website/src/index.md`, `website/src/team.md`,
  `website/.vitepress/` absent; `task claude:tests` green.
- After phase 6: `https://ocx.sh/docs/` and `/schemas/` served from Bunny,
  `cutover:verify` (in `../ocx-website/infra/cutover/`) green.
