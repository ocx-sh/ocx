---
title: Documentation navigation and search
summary: The sidebar depth cap and the page-length trigger that has to ship with it, explicit anchors, and the zero-result loop from beacon to disposition
---

# Documentation navigation and search

DOC-NAV governs the shape of a docs site: how deep the sidebar nests and how
long a page grows. It also owns what happens when the site's own search finds nothing.

Contents: [Precondition](#the-precondition-doc-nav-01) ·
[Nav depth and page length](#nav-depth-and-page-length) ·
[Labels, anchors and stub pages](#labels-anchors-and-stub-pages) ·
[The zero-result loop](#the-zero-result-loop) ·
[Calibration](#what-the-checks-measured) ·
[Worked pairs](#worked-pairs) ·
[Generator divergence](#generator-divergence) ·
[Pinned decisions](#pinned-decisions) ·
[Not studied](#not-studied)

## The precondition (DOC-NAV-01)

Nothing in this file fires on a repo that has no docs site. A committed `docs/`
tree with no generator config carries no sidebar, no breadcrumb and no search box.
Every rule below then reports not applicable and exits 0. A check that
reports "failed" there is the check being wrong, not the repo.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| DOC-NAV-01 | Run every rule in this file only when the repo carries a docs-site generator config file. | A docs tree with no site has no navigation surface to check, so every finding is false. | `python3 checks/nav_depth.py --root .` detects `mkdocs.yml`, `book.toml`, `docs/book.toml`, `.vitepress/config.*`, `docs/.vitepress/config.*` and `website/.vitepress/config.*`. No hit prints not applicable and exits 0. | MUST |

The VitePress paths are not optional. A discovery list of `mkdocs.yml` and
`SUMMARY.md` alone skipped a whole VitePress site in calibration and reported
"no nav config" instead of failing loudly.

## Nav depth and page length

These five rules ship together or not at all. Capping nav depth pushes the same
content sideways into longer pages, so the depth cap is only honest when a
length trigger ships beside it. All five run out of `checks/nav_depth.py`,
which reads a page's declared type through `checks/doc_declaration.py` and its
prose word count through `checks/strip_prose.py`.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| DOC-NAV-02 | Nest the sidebar no deeper than three levels, and collapse the third level by default. | An expanded third level loses the reader between levels instead of disclosing one at a time. | `python3 checks/nav_depth.py --root .` fails at depth 4 and at an expanded level-3 node. Threshold 3 levels (NN/g's two open levels, plus one collapsed). | MUST |
| DOC-NAV-03 | Group the top-level navigation once the site reaches eight pages. | A flat list of every page hides the site's shape and leaves its pages untypeable. | `python3 checks/nav_depth.py --root .` counts top-level entries with no children. Threshold 8 pages (argued, against a measured failure at 20 flat items). Changed files gate first, whole tree warns until the backfill lands. | SHOULD, pinned |
| DOC-NAV-04 | Give a site whose nav reaches three levels a real breadcrumb, or bring the nav back to two levels. | A third level with no ancestor trail leaves the reader no way back up. | `python3 checks/nav_depth.py --root .` for measured depth, then `grep -c 'navigation.path' mkdocs.yml` at depth 3 or more. On VitePress and mdBook: unverified: reading heuristic. Look for a breadcrumb component that builds its trail from the current route, not a sentence saying "as discussed above". | SHOULD |
| DOC-NAV-05 | Cap in-page headings at H4 unless the page declares `doc_type: reference` and carries a structural drift test. | Headings below H4 fall out of the on-page outline, so their content stops being findable. | `python3 checks/nav_depth.py --root .` reports every heading at H5 or deeper and reads the exemption through `checks/doc_declaration.py`. Carve-out arm: unverified: reading heuristic. Look for the test file that fails when that page's headings move. Changed files gate first. | SHOULD |
| DOC-NAV-06 | Split a page once its prose passes 4000 words, unless the page declares `doc_type: reference`. | An unsplit page grows until search, the on-page outline and diff review all stop working on it. | `python3 checks/nav_depth.py --root .` counts stripped prose words through `checks/strip_prose.py` and reads the exemption through `checks/doc_declaration.py`. Threshold 4000 words (the calibration corpus distribution). Changed files gate first, whole tree warns until the backfill lands. | SHOULD, pinned |

DOC-NAV-05 owns the heading-depth cap for the whole set. DOC-NAV-06 owns page
length. The exemption is read from the declaration comment, never from a path and never
from frontmatter. A path classifier misread the page type 32 percent of the time
in calibration.

## Labels, anchors and stub pages

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| DOC-NAV-07 | Give every heading that another file links to an explicit `{#kebab-id}` anchor. | A heading reworded without its anchor breaks every inbound link and nothing reports it. | `python3 checks/links_raw.py --root .` reports each cross-file `#target` whose destination heading carries no explicit id. This is the authoring half only. DOC-OBS-02 owns the resolver that decides a link is dead. Changed files gate first. | MUST |
| DOC-NAV-09 | Name nav entries and page titles after the reader's task, never after an audience or an internal codename. | A label the reader does not recognise is a link the reader does not click. | `grep -rniE -e '^# .*developer' -e '^# .*admin' -e '^# .*beginner' -e '^# .*advanced' -e '^# .*professional' -e '^# .*workforce' docs/`, and the same six patterns over the nav config. The project jargon denylist arm ships at CONSIDER and is unverified: reading heuristic. Look for a branded or internal term in a nav label. | SHOULD |
| DOC-NAV-17 | Give every authored page under 150 prose words at least one outbound link. | A short page with no way onward is a dead end the reader reaches and leaves. | Count the words `python3 checks/strip_prose.py PATH` prints. Under 150 words (the stub floor from the corpus distribution), `grep -cE '\]\([^)]' PATH` returning 0 fails. Skip a page whose declaration says `landing`, read through `checks/doc_declaration.py`. Changed files gate first, whole tree warns until the backfill lands. | SHOULD |

DOC-NAV-17 sets a floor and no ceiling. No source supports an upper link budget on a
short page. Deleting a second useful link to satisfy this rule is the rule being
misread.

## The zero-result loop

Seven rules, one lifecycle: fire the signal, fix the page, classify the query,
review the log, and keep the beacon off the generator's internals. DOC-NAV owns
the whole loop. A deferred signal is recorded under DOC-OBS-12's shape, not
under a rule here.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| DOC-NAV-10 | Fire one named event when the site's own search returns zero results, and send it to a named sink from the same file. | A beacon with nothing listening is a no-op that still passes a grep for the event name. | `grep -rl 'docs:zero-result-search'` over the site source finds a file, and `grep -n -e 'fetch(' -e 'sendBeacon(' <that file>` names an endpoint the repo declares. The string must survive into the built bundle. Enabling GA4 Enhanced Measurement never satisfies this. | SHOULD, pinned |
| DOC-NAV-11 | Fix a zero-result query by rewording the page, never by adding a synonym or query-relaxation key. | None of lunr, minisearch or elasticlunr reads those keys, so the config edit does nothing and raises no error. | `grep -rn -e synonyms -e removeWordsIfNoResults -e optionalWords -e ignorePlurals -e removeStopWords mkdocs.yml .vitepress/config.* book.toml` must find nothing. The accepted fix puts the query's literal phrase in a heading or an "also called" line on the page that already covers it. | MUST |
| DOC-NAV-12 | Write the rendered zero-result state and the 404 template as a title, one or two sentences, and at least one link. | A bare "no results" line is a dead end for a reader who will probably not search again. | unverified: reading heuristic. Look for a heading, at most two sentences of body copy, and at least one link. Read the template that renders when search comes up empty. Applies to the rendered template only, never to an authored page. | CONSIDER |
| DOC-NAV-13 | Never flatten a search engine's title and body boosts to equal weight. | The mean query is two words long, which matches a title and not a paragraph. | `grep -c 'output.html.search' book.toml` returning 0 passes, because mdBook's defaults are already 2 against 1. If the section exists, assert `boost-title` above `boost-paragraph`. For VitePress, assert `searchOptions.boost` is absent or keeps title above text. | CONSIDER |
| DOC-NAV-14 | Give every zero-result query seen twice one of two dispositions, reword or gap. | An unclassified log becomes noise, and then every query reads as a missing page. | unverified: reading heuristic. Look for a log entry older than 30 days (asserted) carrying neither disposition. Look for a `gap` entry with no linked issue quoting the query. Gated behind DOC-NAV-10, so it has nothing to read until the signal lands. | CONSIDER |
| DOC-NAV-15 | State a review trigger as a count or a release boundary, never as a bare cadence word. | A rule that says "review regularly" never fires on a site with little traffic. | `grep -rniE -e 'review[^.]{0,60}regularly' -e 'review[^.]{0,60}frequently' -e 'review[^.]{0,60}periodically' -e 'review[^.]{0,60}often'` over the rule and runbook text, plus the same four patterns with the halves swapped. Default trigger 20 new entries or the next tagged release, whichever comes first (asserted). | CONSIDER |
| DOC-NAV-16 | Bind the zero-result beacon to rendered output or a documented public API, never to a generator's internal file. | A patched search worker breaks silently on the next generator upgrade, and its maintainer supports none of it. | unverified: reading heuristic. Look for an import in the beacon file that reaches into the generator's build output or its vendored search worker. Watching the rendered no-results node passes. Calling `pagefind.search()` passes. | SHOULD |

DOC-NAV-15 owns the cadence-word ban for the whole set. Any other rule that
needs a review trigger states a count or a release boundary instead.

### Sink options, by generator and host

Every shape below costs at most a few dollars a month, and most cost nothing.
Vendor free tiers and paid tiers read from each vendor's own pricing page.
The beacon itself is identical across all three engines, because none of them
exposes a zero-result hook.

| Generator or host | Sink that works | Cost band | What it costs you instead |
|---|---|---|---|
| MkDocs Material (lunr) | Watch the rendered no-results node, then POST to your own endpoint. | $0 on a serverless free tier of 100,000 requests a day, $5 a month past it | Nothing beyond the endpoint. Never override the shipped search worker. |
| VitePress (minisearch) | Same watcher and POST, or call `pagefind.search()` if the site already migrated. | Same $0 to $5 band | The Pagefind route means replacing the built-in search UI, which is engineering, not money. |
| mdBook (elasticlunr) | Same watcher and POST. | Same $0 to $5 band | `searcher.js` is internal. Patching it fails DOC-NAV-16. |
| A site already running Umami or Plausible | A named custom event with properties, landing in the dashboard you already read. | $0 self-hosted, up to roughly $20 a month hosted | No second integration, and no endpoint of your own. |
| A static host that hides its access log | No sink. GitHub Pages logs visitor requests and never exposes that log to the owner. | Not available at any price | A log-line sink collapses back into standing up a function. |

When no sink exists yet, defer the signal in DOC-OBS-12's shape rather than
shipping a beacon into a void. Record the deferral in the signal manifest and
name the precondition as a checkable fact, such as "an endpoint this site may
POST to exists". Review it at DOC-NAV-15's trigger. A deferral that names no
precondition is the failure DOC-OBS-12 exists to catch.

## What the checks measured

Measured over a 249-page, 9-site calibration corpus. "Red on a planted
violation" means the check was run against a fixture built to break it.

| Rule | Hits on the corpus | Red on a planted violation |
|---|---|---|
| DOC-NAV-01 | 3 of 249 surfaces correctly reported not applicable | structural, already correct |
| DOC-NAV-02 | 0 of 9 sites over depth 3 | yes, a depth-4 config |
| DOC-NAV-03 | 1 of 9 sites flat at 8 or more pages | yes, a 9-item flat config |
| DOC-NAV-04 | 1 of 9 sites at depth 3 with no breadcrumb | yes, on the MkDocs arm only |
| DOC-NAV-05 | 2 of 249 pages carry an H5 | not attempted |
| DOC-NAV-06 | 16 of 249 pages over 4000 prose words | not attempted |
| DOC-NAV-07 | one real orphaned anchor with three inbound references, plus two siblings | not attempted |
| DOC-NAV-09 | 0 of 249 titles and 0 of 9 nav configs | not attempted |
| DOC-NAV-10 | 0 of 9 sites carry the event, a sink call or any search-analytics key | not attempted |
| DOC-NAV-11 | 0 of 9 configs carry any of the five keys | not attempted |
| DOC-NAV-12 | 0 of 249 pages is a rendered empty-state or 404 template | not attempted |
| DOC-NAV-13 | 9 of 9 sites comply on generator defaults | yes, a flattened boost config |
| DOC-NAV-14 | vacuous, no site logs a query yet | not attempted |
| DOC-NAV-15 | 0 of 249 pages put a cadence word within 60 characters of "review" | not attempted |
| DOC-NAV-17 | 15 of 53 non-landing pages under 150 words carry zero links | not attempted |
| DOC-NAV-16 | not applicable, no site has a beacon to bind yet | not attempted |

No DOC-NAV check measured an unacceptable false-positive rate. Two rows carry a
different defect, a check that cannot go red honestly. Both say so on the row:
DOC-NAV-05's carve-out and DOC-NAV-16's binding test.

## Worked pairs

### DOC-NAV-02 and DOC-NAV-03, the sidebar

Wrong, four levels and every one of them open:

```yaml
nav:
  - Guide:
      - Tasks:
          - Deploy:
              - Rollback: guide/tasks/deploy/rollback.md
```

Right, three levels with the third collapsed, and top-level groups instead of a
flat file list:

```yaml
theme:
  features: [navigation.path]
nav:
  - Guide:
      - Tasks:
          - Deploy: guide/tasks/deploy.md
```

### DOC-NAV-07, the cross-file anchor

Wrong, the link slugs the heading it hopes to find:

```markdown
See [path resolution](../user-guide.md#path-resolution).
```

Right, the target heading carries the id the link names:

```markdown
## Path resolution {#path-resolution}
```

### DOC-NAV-10, the beacon

Wrong, the event fires into a void and the grep still passes:

```js
document.dispatchEvent(new CustomEvent('docs:zero-result-search', { detail: { q } }))
```

Right, the same file also names where the event goes:

```js
navigator.sendBeacon('https://sink.example.com/zero-result', JSON.stringify({ q }))
```

## Generator divergence

| Generator | What differs | What the check must do |
|---|---|---|
| MkDocs Material | `mkdocs.yml` ships `!ENV` and `!!python/name:` tags by default. Breadcrumbs come from the `navigation.path` feature. | Load the YAML permissively. A strict safe loader hard-failed on 4 of 7 real configs and skipped them silently. |
| VitePress | Nesting lives in the `sidebar` array's `items`. Sections are open unless `collapsed: true`. Depth past 6 is dropped with no error. No breadcrumb component ships. | Parse `items` nesting, treat an uncollapsed level 3 as a failure, and fall back to review for the breadcrumb. |
| mdBook | `SUMMARY.md` indent is the depth. A `# Part Title` line adds a group without adding a level. The file's own first `# ` line is mandatory. | Skip that first heading. Counting it as a divider marked a 20-item flat nav as grouped. |
| Docusaurus | The sidebar has no documented depth ceiling, and its own examples go past four levels. MDX parsing applies to plain `.md` by default. | Apply the same three-level cap. The framework not erroring is not permission. |

Starlight and Sphinx are not detected by `checks/nav_depth.py` today, so a site
on either reports not applicable under DOC-NAV-01 rather than passing quietly.

## Pinned decisions

A pinned row rests on a default this program chose, not on a measurement of your
site. Each one is reversible by editing one row, in one place.

- **DOC-NAV-03's 8-page grouping threshold.** Argued. The measured failure was a
  20-item flat nav. Move the number to 20 and the rule earns MUST back.
- **DOC-NAV-06's 4000-word split trigger.** Taken from the calibration corpus
  distribution, not from reader behaviour. A per-type number would be better and
  does not exist yet.
- **DOC-NAV-10's event name and sink endpoint.** `docs:zero-result-search` is a
  default string. The endpoint, its host and its privacy posture are the
  adopter's decision, made once and named in the beacon file.

DOC-NAV-14's 30-day staleness window and DOC-NAV-15's 20-entry trigger are
asserted defaults too. Both ship at CONSIDER, which is where an asserted number
belongs, so neither is pinned.

## Not studied

- **Versioned docs and internationalisation.** A version picker and a language
  switcher are navigation levels that nothing here measures.
- **Page-level accessibility.** No rule in this set covers alt text, colour
  contrast, keyboard order or table semantics. The sidebar and the search widget
  are uncovered too.
- **Agent-readable navigation.** Progressive disclosure helps a human reader and
  costs a retrieval agent roughly 31 times in bytes. Nobody has asked whether the
  depth cap and page splitting help or hurt that reader.
- **A behaviour-derived split threshold.** DOC-NAV-06's number rests on one
  corpus distribution and nothing else.
- **A mechanical page-to-test binding.** DOC-NAV-05's reference carve-out stays a
  reading heuristic. It needs the `# doc:` binding key to work on a page, not
  only on a fence.
- **The GA4 last mile.** The non-satisfier claim is settled by reading two
  primary sources together. Nobody has watched DebugView while typing a nonsense
  query into a live overlay search.
- **Hosted search dashboards.** Whether a free hosted DocSearch tier ships a
  no-results dashboard is evidence, not confirmation. Check your own dashboard.
- **Print and offline output.** Not examined at all.
