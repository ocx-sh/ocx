# Proposal for ocx-sh/grimoire-lore: docs-quality reference-page sharpening

Source: ocx `hex/docs-next-level` (P1). The vendored copy under `.claude/rules/docs-quality/**` is
grim-pinned, so these changes are made in the shared source and arrive through `grim install`. ocx
enforces the same shapes locally in the meantime (`test/lint/test_reference_shape.py`,
`test/lint/test_docs_quality_gate.py`, `.claude/rules/docs-style.md` § Reference Pages).

## Why

ocx's reference pages grew rationale, conventions and asides between entries (the plan's bloat source).
DOC-TYPE-04/05/18/19 as written leave these gaps:

| Rule | Gap seen |
|---|---|
| DOC-TYPE-04 | Checks voice (first person, narrative opener, problem framing). A third-person rationale paragraph is not a voice finding. |
| DOC-TYPE-05 | CONSIDER, and its pattern has never run against real prose; an aside inside a reference entry is not read. |
| DOC-TYPE-18 | Proves the entry set equals the code's enumeration, not what each entry may contain. |
| DOC-TYPE-19 | CONSIDER, and the unit ("top-level entry headings") is undefined for a page whose entries sit one level below a section heading. |

## Proposed rule text

**DOC-TYPE-04 (sharpen, MUST).** Define the reference entry shape: a `doc_type: reference` page is an
optional intro of at most two sentences, then entries. An entry is a heading with an explicit
`{#kebab-id}`, then only: a description of at most two sentences, fact tables or `key: value` lists,
at most one fenced block, and at most one "See [x](...)" link line. No asides, rationale, conventions
or sub-sections that are not entries.

**DOC-TYPE-05 (sharpen, SHOULD).** On a reference page an aside (`:::`, `<Aside`, `!!!`, `> [!NOTE]`)
is a finding, not a CONSIDER: explanation lives on an explanation page and the entry links to it.

**DOC-TYPE-18 (extend).** The set-equality test also asserts every entry heading carries an explicit
anchor, because anchors are the stable key the generator, redirects and external links share.

**DOC-TYPE-19 (promote to SHOULD, define the unit).** An entry is a heading of the page's entry level
(the deepest level that appears under the page title more than once), warn past 15, fail past 20.
Generated reference pages cap at 20 entries by construction; a generator that would exceed it must
split the group, not warn.

## Proposed check changes (`checks/page_type.py`)

1. `reference_shape(path, text)` for `doc_type: reference` pages, outside fences, reporting
   `page:line rule` with the rules `paragraph` (more than two sentences), `aside`, `fences` (a second
   fenced block under one entry, reported at the second opener) and `anchor` (heading without
   `{#id}` or the MDX form `\{#id\}`). A reference implementation with fixtures (planted, clean,
   skipped-type, sentences-inside-a-fence-are-not-prose) is `test/lint/test_reference_shape.py` in ocx.
2. DOC-TYPE-19 counts entries by the entry level above, and reports the count of explanatory sections
   (any heading whose body holds a paragraph of more than two sentences) separately, so a page
   drifting from entries to essay is visible before it reaches the cap.
3. Pin both thresholds (`2` sentences, `1` fence, `15`/`20` entries) as named constants at the top of
   the file, like the existing caps, with the "pinned default" note in `docs-quality.md` § Severity.

## Proposed check changes (`checks/nav_depth.py`)

`find_generator` knows mkdocs, VitePress and mdBook only, so on a Starlight site DOC-NAV-01 reports
"no generator" and the whole DOC-NAV family is inert. Add Starlight:

- Detect `astro.config.{mjs,js,ts}` containing `starlight(`.
- Read `sidebar:` the way `vitepress_nav` does (balanced-bracket scan, no module parse): level 1 is
  the sidebar group, deeper `items:` nest; `collapsed: true` marks a collapsed level (DOC-NAV-02).
- Treat `autogenerate: { directory }` entries as groups of unknown size and skip DOC-NAV-03's count
  for them rather than guessing.
- Fixture: a three-level and a four-level sidebar, one with `autogenerate`.

## Owner steps

1. Open a PR on ocx-sh/grimoire-lore with the rule text above in `docs-quality/page-types.md` and
   `docs-quality/navigation.md`, the check changes, and fixtures.
2. After release, `grim install` in ocx; then delete the ocx-local shape check only if the vendored
   `page_type.py` reports the same `page:line rule` set on `test/lint/fixtures/reference_shape/`.
