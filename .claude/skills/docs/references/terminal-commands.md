# Terminal Commands in `ShellSession`

Rules for what is framed live in `docs-style.md` § Terminal Commands. This file holds the mechanics.
Component props and the marker syntax: theme repo, `skills/ocx-theme-components/references/content-navigation.md` § ShellSession.

```mdx
import ShellSession from '@ocx-sh/theme/components/ShellSession.astro';

<ShellSession title="Install and check">

```sh
ocx package install nodejs/node:22
```

```text
Package         Version                Path
nodejs/node:22  ocx.sh/nodejs/node:22  ~/.ocx/symlinks/ocx.sh/nodejs/node/candidates/22
```

</ShellSession>
```

## Structure

- Every shell fence is inside a session or carries `title="<file name>"` (file content: script, rc snippet, config); `website/tests/shell-session.test.mjs` enforces it, includes too.
- Blank line after the opening tag and before the closing tag; fences at column 0 (inside `<TabItem>` too).
- A session holds only code fences and escaped includes: no prose, no other component.
- Two command fences in a row render the second as output: merge them into one fence or use two sessions.
- A line after a trailing `\` continues the command and gets no `$`. Remove a literal `$ ` or `> ` when wrapping an old fence.
- An include is `\<\<\< @/_scripts/<slug>.sh\{sh\}` in `.mdx`; a marker goes inside the braces after the language.
- `title`: optional, a 2-4 word noun phrase, only where it tells sessions apart. A fence `title="..."` moves to the session.
- `play` is not used. No tabs added for the frame; a fence inside a `<TabItem>` is wrapped inside that tab.
- Up to two short prose lines between a command and its output may move below the session if the sentence still reads; otherwise leave the output outside.

## Output fold

`collapsed` folds every output of the session behind an "Output" toggle. Use it when output is longer than about 8 lines or the prose does not discuss it. Do not use it when the output is the point of the step.

## Badges, highlight, legend

- Fence meta `{"1":1} {"3":3}` draws numbered badges at the right edge of lines 1 and 3. A badge shows the number of the line it sits on, and the legend says "Line N" with that number. Use it when the prose refers to a line by number.
- Fence meta `{2}` highlights one line. Use it when the prose points at exactly one output line.
- A badge carries no meaning alone: write the legend as prose directly under the frame ("Line 1 promotes dev to staging. Line 2 ..."). Never a marker without a legend.
- A `#` comment or blank line inside a command fence draws no prompt and may stay. To turn a comment into a legend instead: drop it from the fence, badge the commands, and make the comment text the legend.
- Keep numbered lines short: the badge sits at the line's end.

## Imports and word-count lints

Put the `ShellSession` import below the `{/* doc_type ... */}` / `{/* doc_tier ... */}` lines. If the page then trips DOC-TYPE-07 or DOC-TYPE-10 (words before the first `##`: an `import` line counts as prose), put the import just below the first `##` heading instead.

## Budget pages

A frame renders fewer elements than a plain block, so wrapping is safe on a page at the element budget. Add nothing else there: no `collapsed`, no title, no extra markers. Pages over or near the budget: see `subsystem-website.md` § Sidebar and Page Weight.
