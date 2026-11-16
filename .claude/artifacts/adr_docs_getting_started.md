# ADR: The entry path is job-first: a one-screen install, a walkthrough, a routing overview

## Metadata

**Status:** Accepted
**Date:** 2026-11-16
**Deciders:** owner (goal statement), docs-overhaul orchestrator
**Domain Tags:** frontend
**Research artifact:** [`research_docs_getting_started.md`](./research_docs_getting_started.md),
[`research_docs_toc.md`](./research_docs_toc.md),
[`research_docs_shell_session_annotations.md`](./research_docs_shell_session_annotations.md)

## Context

The owner asked for a getting-started path modelled on the mise walkthrough: short, organised by
use case, with links to the detailed pages. Before this change a reader passed about 2,700 words
of install options before the first command, and the two tutorials were named by sequence
("Your first project"), not by job.

## Decision

1. **Install is one screen.** `getting-started/install-ocx` keeps the installer, the download-only
   route and the check that it worked. Every option moved, anchors unchanged, to the everyday page
   `guides/install-options`; old URLs redirect.
2. **The project tutorial is the walkthrough.** `getting-started/first-project` ("Pin a
   toolchain") has one section per job: pin, share, shell, CI, offline, update. Each section
   states the job, shows one tested command block and links the page that explains it.
3. **The overview routes by job.** `index.mdx` keeps the installer and one tested command, then
   lists the jobs with a link to the shortest section that does each.
4. **The package tutorial says when to pick it** ("Install a package").
5. **Every command on a tutorial sits in a `ShellSession` frame**, fed by a tested doc-script
   include. Exception: the landing page uses plain blocks, because the extra stylesheet costs it a
   Lighthouse point.
6. **Sidebar: four captions over collapsible groups** (Start here, Guides, Publish and integrate,
   Look things up). The route middleware that flattened groups into links is removed.
7. **`ShellSession` conventions** (theme PR): `collapsed` where the output is not the point of the
   step; a marked line or a numbered badge only when the prose below refers to that line.

## Amendment

- 2026-11-16: the right-hand "On this page" outline folds. The theme option `foldOutline` (default on)
  folds each section's subsections on a page whose outline has 10 or more rows; the section being read
  stays open. Shorter outlines stay flat. The phone copy of the outline is parsed in on first open, which
  pays for the fold controls: no gated page gained elements.

- 2026-11-16: the two tutorial titles and sidebar labels are renamed on owner feedback, from "Use ocx
  in a repository" and "Install without a project" to "Declare a toolchain in ocx.toml" and "Install
  one tool". The old titles used insider words ("project"), and the same file also pins the global
  toolchain. Slugs and anchors are unchanged.

- 2026-11-16 (second rename): the tutorials are now "Pin a toolchain" and "Install a package". Owner: "Declare a
  toolchain in ocx.toml is far too long and Install one tool is kinda weird". Slugs and anchors are unchanged.
  Start here also gains "Customize the install" and "Update or uninstall OCX".

## Consequences

- A new entry-path page needs a sidebar entry and an inventory row in `docs_ia.md`.
- The sidebar change costs 40 elements per page; the headroom is recovered in the theme header,
  not by raising the budget.
- Tutorials stay free of tabs and branches (DOC-DISC-17); alternatives live on the linked pages.

## Not done

- Output blocks backed by golden files (`# expect:` in doc scripts): deferred. Output shown on
  the tutorials is still hand-kept text.
