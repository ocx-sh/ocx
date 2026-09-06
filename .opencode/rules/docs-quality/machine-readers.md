---
title: Machine readers
summary: What a docs site owes an agent that reads it, why the Markdown twin is the mechanism and llms.txt only an index, and which agent-directed prose actually changes behaviour
---

# Machine readers

An agent reading these docs gets whatever the build output hands it. Seven rules
decide whether that is the page content or the HTML chrome wrapped around it.

Contents: [Rules](#rules) · [Agent-directed prose, shown](#agent-directed-prose-shown) ·
[Build output by generator](#build-output-by-generator) · [Out of scope](#out-of-scope) ·
[Not studied](#not-studied) · [Pinned decisions](#pinned-decisions)

## Rules

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| DOC-AGENT-01 | Publish a Markdown twin of every documentation page at a predictable URL by copying the Markdown source into the build output. | Without a twin an agent parses HTML chrome to reach content it could have read directly. | `diff <(find docs -name '*.md' -printf '%P\n') <(find "$OUT" -name '*.md' -printf '%P\n')` with `$OUT` set to the build output directory. Measured: twin present on 0 of 9 real sites, Markdown-source precondition on 9 of 9 (wave-2 calibration, 9 real docs sites). | SHOULD |
| DOC-AGENT-03 | Ensure content collapsed behind a `<details>` block, a tab or an accordion appears unfolded in that page's twin. | A twin generated from the rendered page silently deletes content an agent has no other route to. | `grep -nE -e '<details\b' -e '^=== "' -e '^::: tabs' <page>` lists the collapsed blocks. Then unverified: reading heuristic. Look for each block's inner text inside that page's twin. Runs on changed files first. | CONSIDER |
| DOC-AGENT-04 | Publish `llms.txt` in the spec's shape as a recommended index, never as the required agent-readability mechanism. | 97 percent of published `llms.txt` files logged zero requests in May 2026 (mecanik.dev), so a site shipping only that file serves nobody. | `awk 'NR==1&&!/^# /{exit 1} /^## /{h=1} /^> /&&!h{g=1} END{exit !g}' llms.txt`, run only when the file exists. Measured: 0 of 9 sites publish it at repo root, the static passthrough directory or `docs/` (wave-2 calibration, 9 real docs sites). It is an existence test, so no false positive is possible. | SHOULD |
| DOC-AGENT-05 | Name the specific consumer when justifying an agent-facing mechanism, never aggregate AI-traffic or search-visibility language. | Generic AI-crawler traffic is 1.1 percent of requests (mecanik.dev retrieval-bot share, May 2026), so a mechanism justified by it is wrong. | `grep -nEi -e 'AI traffic' -e 'AI crawler' -e 'search visibility' <path>` lists candidates. Then unverified: reading heuristic. Look for a named reader in the same sentence as each hit. Runs on changed files first. | SHOULD |
| DOC-AGENT-06 | Give every agent-directed callout an imperative instruction, or delete the callout. | A bare "For agents" label moved compliance by nothing, while a stated instruction moved 5 of 15 to 15 of 15 (passo.uno, n=15). | `grep -nEi '\bfor agents?\b' <path>` and `grep -nEi '\bnote to agents?\b' <path>`. The `for ` or `note to ` prefix is required. Each hit must be followed by a paragraph carrying use, run, prefer, install, follow, call or fetch. Measured: the optional-prefix pattern returned 22 hits over 249 pages at 22 of 22 false positives, and the required prefix returns 0 hits over the same corpus (wave-2 calibration, 249-page corpus). | SHOULD |
| DOC-AGENT-07 | Place any agent-directed instruction before the page's second `##` heading. | An instruction below the fold is never reached, because reading agents truncate long pages (Mintlify, context-for-agents). | `awk '/^## /{h++} /[Ff]or agents?/{if (h>=2) exit 1}' <path>`. Measured: 0 hits over the corpus, so the rule has no current target (wave-2 calibration, 249-page corpus). | SHOULD |
| DOC-AGENT-08 | Require only static-file mechanisms unless the repository's own host config proves the site can serve more. | Content negotiation and custom response headers need an edge layer a bare static host does not have. | `curl -s -o /dev/null -w '%{http_code}' <page-url>.md` returns 200. The negotiation arm runs only when a `_headers` file or an edge-function config exists in the repository. Measured: 0 of 9 sites carry either, so that arm stays off at 0 false positives (wave-2 calibration, 9 real docs sites). | MUST |

## Agent-directed prose, shown

DOC-AGENT-06 is the one rule here that reads as pedantic until you see the two
blocks side by side. The label is inert. The directive is a lever.

Wrong, measured as inert:

```markdown
> **For agents:** This page documents the export command and its flags.
```

Right, the same callout carrying an instruction:

```markdown
> **For agents:** Run `tool export --help` before copying any flag from this
> page. The flag list below is generated per release and can lag.
```

DOC-AGENT-05 fails the same way, in the justification rather than the callout.

Wrong, and unfalsifiable:

```markdown
The Markdown twin captures AI traffic and improves our search visibility.
```

Right, and checkable by whoever doubts it:

```markdown
An editor assistant such as Claude Code or Cursor fetches a docs URL directly.
The twin hands it the page body instead of the site chrome.
```

## Build output by generator

The twin has no measured implementation anywhere. Before adopting
DOC-AGENT-01, build the site once. Then check whether a `.md` file placed beside
a page reaches the output directory at that page's own path.

| Generator | Build output | The trap |
|---|---|---|
| MkDocs Material | `site/` | Every `.md` under `docs/` renders as a page, so a sibling twin becomes a second page in the nav. |
| VitePress | `.vitepress/dist/` | Same for every `.md` in the source tree. `public/` copies verbatim and is where `llms.txt` belongs. |
| mdBook | `book/` | Only pages listed in `SUMMARY.md` render, and `SUMMARY.md` is itself a mandatory page. |
| Docusaurus | `build/` | Pages are MDX, so a copied twin hands an agent JSX rather than Markdown. |
| Starlight | `dist/` | Same MDX payload, and content sits under `src/content/docs/`. |
| Sphinx | `_build/html/` | Source is reStructuredText unless the project runs MyST, so a copied twin is not Markdown at all. |

Content negotiation and custom response headers are out of reach on all six
unless the deploy target adds an edge layer. That is what DOC-AGENT-08 gates.

## Out of scope

`AGENTS.md`, a `skill.md` file and an MCP server are agent-facing, and none of
them is a documentation page. The three formats have not converged, they address
a different audience, and no single check validates them end to end. Link to
them from the docs and keep them out of the required-mechanism list.

## Not studied

- **Twin generation mechanics.** Whether MkDocs Material, VitePress or mdBook can
  emit a per-page twin with no custom plugin. Nobody has measured it, and this
  gap is why DOC-AGENT-01 ships at SHOULD.
- **Twin drift.** No check proves a generated twin still matches its page.
  DOC-AGENT-03 covers collapsed content only.
- **Instruction durability.** The compliance result rests on 15 runs of one
  model, and the same author's second test scored 12 of 12 in every condition.
- **Discovery files beyond `llms.txt`.** Vercel layers five files on top of
  per-page twins. That set has not been read or costed.
- **Non-Markdown sources.** No MDX or reStructuredText twin has been built or
  measured, so the last three rows of the generator table are reasoning, not
  results.

## Pinned decisions

No row in this family pins a project decision. Every severity rests on a
measured count or on a named published test, and both are on the row.
DOC-AGENT-01 returns to MUST when one working twin configuration exists on
MkDocs Material, VitePress and mdBook.
