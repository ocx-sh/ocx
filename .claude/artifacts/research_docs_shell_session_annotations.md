# Research: Line highlights and numbered badges in ShellSession

**Date:** 2026-11-16
**Question:** Can a `ShellSession` highlight lines and carry a numbered badge at the right edge of
a line, so prose below can refer to "line 1"? What works through Expressive Code already, what
does the component lack?
**Method:** read-only trace of theme 0.5.0 (`ShellSession.astro`, `shell-session.mjs`,
`shell-session-play.mjs`, `starlight/ec.mjs`, `markdown/includes.mjs`) and
`@expressive-code/plugin-text-markers` 0.44.2. No browser run; items under "Unconfirmed" need one.

## Findings

### What Expressive Code gives

- The theme sets no `plugins` in `ec.mjs`, so Starlight's default set applies: frames, shiki and
  text-markers. Collapsible-sections and line-numbers are not installed.
- Fence meta `{2}` puts `highlight mark` on the line element; `ins={…}` and `del={…}` put
  `highlight ins` / `highlight del`; a labelled marker `{"1":2}` adds `tm-label` and an inline
  `--tmLabel` custom property. Inline markers (`"text"`, `/re/`) wrap tokens in `<mark>`.
- No docs page uses any marker today.

### What the component does with them

- Nothing is stripped. `slimTokens` only rewrites plain token spans; marker classes sit on
  `.ec-line`, outside its pattern. The line matcher is a prefix match, so extra classes survive.
- The prompt and the marker label use the same pseudo-element. The `$ ` and `> ` prompts are
  `.ocx-shell-session .code::before`; the marker label is also `.code::before`, absolutely
  positioned at the left. A marked line can therefore lose its prompt and shift left.
- `ins` / `del` widen the block's inline padding, which misaligns the prompt column.
- Output lines force a comment-grey text colour; contrast against the mark background is unchecked.

### Includes cannot carry meta

`includes.mjs:7` reads `<<< @/path{lang}`; the language group is `\S+` and the fence is built from
it alone. `{sh {2}}` cannot be written, so a tested doc-script include inside a session cannot be
marked today.

### Right-edge badges

- Expressive Code draws labels at the left, as a pseudo-element.
- Smallest design with no new authoring syntax: reuse the labelled marker (`{"1":2}`), draw
  `--tmLabel` in `.code::after` at the inline end, and restore the prompt in `::before` for marked
  lines inside a session. CSS only, no script, no new stylesheet.
- A pseudo-element is not reliably announced by screen readers; the prose legend under the session
  is the accessible path. A DOM badge would be announced but is typed like command text by `play`
  mode unless placed outside `.code`.
- Copy is unaffected: Expressive Code copies `data-code` from the source text.

## Decision

Theme PR, `ShellSession`:

1. Marked lines keep their prompt and alignment inside a session (`mark`, `ins`, `del`).
2. A labelled marker renders as a badge at the right edge of its line.
3. `<<<` includes accept fence meta after the language, so tested doc scripts can be marked.
4. Story, unit tests and an e2e check for prompt presence and badge position.

Authoring form:

````mdx
<ShellSession title="Install">

```sh {"1":1} {"2":2}
ocx install cmake:3.31
ocx select cmake:3.31
```

</ShellSession>

Line 1 downloads the package. Line 2 points `current` at it.
````

## Unconfirmed

- Cascade order between Expressive Code's layer and the theme's layer on `.code::before`.
- Whether two labelled-marker groups parse in one fence.
- The inline-CSS cap on the component's style block.
