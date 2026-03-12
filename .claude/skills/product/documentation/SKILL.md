---
name: documentation
description: Write OCX documentation (user guide, reference pages, website content). Applies OCX-specific narrative structure, linking conventions, and VitePress component patterns.
allowed-tools: Read, Write, Edit, Glob, Grep, WebSearch, WebFetch
---

# OCX Documentation

All documentation for OCX lives in `website/src/docs/`. The site is built with VitePress.

## Rules

Follow `.claude/rules/documentation.md` — it contains OCX-specific guidelines distilled from review feedback. Key points:

- **Narrative structure**: idea → problem → solution, then depth
- **No marketing tone** — let examples make the case
- **Reference-style links** — never inline `[text](url)`, collect definitions at file bottom
- **Every external tool hyperlinked** — every occurrence, not just the first
- **Analogies in `:::info` callout boxes**, not inline prose
- **Real-world examples** — always search the internet before writing comparisons
- **Custom anchors** on every heading: `{#parent-subsection}`

## Product Context

Read `.claude/rules/product-context.md` for positioning, differentiators, and competitive landscape before writing any user-facing documentation.

## VitePress Markdown Extensions

### Custom Containers

```md
::: info
Analogies to other systems, background context.
:::

::: tip
Actionable advice, example usage, recommended patterns.
:::

::: warning
Important caveats, things that are commonly misunderstood.
:::

::: details Summary text
Optional technical depth, spec references, implementation details.
Hidden by default — click to expand.
:::
```

### Code Groups

Tabbed code blocks for showing alternatives side-by-side (good for ocx vs other tool comparisons):

````md
::: code-group
```sh [ocx]
ocx install cmake:3.28
```

```sh [apt]
apt-get install cmake=3.28*
```

```sh [brew]
brew install cmake@3.28
```
:::
````

### Syntax Highlighting

Line highlighting with `{4}` or `{1,3-5}` after the language tag:

````md
```rust{3}
fn main() {
    let store = ObjectStore::new(root);
    store.content(&identifier, &digest)  // highlighted
}
```
````

## Vue Components

All components are globally registered — use them directly in any `.md` file without imports.

### Tooltip

Inline term with hover popup. Use for technical terms that would interrupt prose flow.

```html
<Tooltip term="object store">
  Content-addressed storage keyed by <strong>SHA-256 digest</strong>.
  Files are never duplicated regardless of how many packages reference them.
</Tooltip>
```

| Prop | Type | Default | Description |
|---|---|---|---|
| `term` | `string` | — | Trigger text (shown inline with dashed underline) |
| `side` | `'top' \| 'bottom' \| 'left' \| 'right'` | `'top'` | Preferred bubble placement |
| `delay-duration` | `number` | `400` | Milliseconds before tooltip opens |

The `term` should be a natural noun phrase that fits in the sentence. Slot content is the popup — supports HTML including `<code>`, `<strong>`, links.

**Good candidates**: technical terms, jargon, protocol-level concepts.
**Bad candidates**: anything the reader needs to understand the prose flow.

### FileTree

Collapsible, selectable directory tree. Use `<Tree>` with nested `<Node>` tags. Directories expand/collapse on click.

```html
<Tree>
  <Node name="~/.ocx/" icon="🏠" open>
    <Node name="objects/" icon="🗄️">
      <Description>content-addressed store</Description>
      <Node name="cmake/" icon="📦">
        <Node name="sha256:abc123…/" icon="📁" open-icon="📂">
          <Node name="content/" icon="📂">
            <Description>package files (read-only)</Description>
          </Node>
          <Node name="metadata.json" icon="📋" />
          <Node name="refs/" icon="🔗">
            <Description>back-references — guards GC</Description>
          </Node>
        </Node>
      </Node>
    </Node>
  </Node>
</Tree>
```

**`<Node>` attributes:**

| Attribute | Type | Description |
|---|---|---|
| `name` | `string` | File or directory name (`/` suffix is conventional for dirs) |
| `icon` | `string` | Emoji or symbol; replaces the default toggle arrow |
| `open-icon` | `string` | Icon shown when directory is expanded (falls back to `icon`) |
| `open` | `boolean` | Expanded on first render (default `true` for directories) |

**`<Description>`** — sub-element of `<Node>`. Muted annotation shown to the right of the name.

### Steps

Vertical progress indicator with optional collapsible detail panels. Use for roadmaps, onboarding flows, or multi-step processes.

```html
<Steps>
  <Step title="Install" status="complete">
    <Description>Package downloaded and extracted.</Description>

    Markdown content here is rendered in a collapsible detail panel.
    Supports **bold**, `code`, links, etc.

  </Step>
  <Step title="Configure" status="current">
    <Description>Set environment variables.</Description>
  </Step>
  <Step title="Run" status="upcoming">
    <Description>Execute the package.</Description>
  </Step>
</Steps>
```

**`<Step>` attributes:**

| Attribute | Type | Description |
|---|---|---|
| `title` | `string` | Short label next to the status indicator |
| `status` | `'complete' \| 'current' \| 'upcoming'` | Controls indicator colour and connector fill |

**`<Description>`** — sub-element of `<Step>`. Supporting text below the title.

**Slot content** — Markdown rendered in a detail panel when the step is clicked. Omit for non-clickable steps.

### Terminal

Animated terminal session using asciinema-player. Two modes: inline `<Frame>` tags or external `.cast` file.

**Inline frames:**

```html
<Terminal title="Installing a package">
  <Frame at="0">$ ocx install cmake</Frame>
  <Frame at="0.5">Resolving cmake@latest...</Frame>
  <Frame at="1.5">Downloading cmake@3.28.0 [================] 100%</Frame>
  <Frame at="2.5">Installed cmake@3.28.0</Frame>
</Terminal>
```

**External `.cast` file:**

```html
<Terminal src="/casts/demo.cast" title="ocx workflow" :autoPlay="false" />
```

**`<Terminal>` attributes:**

| Attribute | Type | Default | Description |
|---|---|---|---|
| `title` | `string` | — | Title in the terminal chrome title bar |
| `src` | `string` | — | Path to `.cast` file (alternative to inline frames) |
| `cols` | `number` | `80` | Terminal width in columns |
| `rows` | `number` | auto | Terminal height in rows |
| `autoPlay` | `boolean` | `true` | Start playback automatically |
| `speed` | `number` | `1` | Playback speed multiplier |
| `idle-time-limit` | `number` | `2` | Compress pauses longer than N seconds |
| `loop` | `boolean` | `false` | Loop playback |
| `fit` | `'width' \| 'height' \| 'both' \| 'none'` | `'width'` | How the player scales |
| `collapsed` | `boolean` | `false` | Start collapsed; click title bar to expand |

**`<Frame>` attributes:**

| Attribute | Type | Description |
|---|---|---|
| `at` | `number` | Time in seconds when this line appears |

Multiple frames with the same `at` value appear simultaneously (multi-line output).

## Before Writing

1. Read the relevant source code — do not document from memory
2. Search the internet for real examples from other ecosystems
3. Identify the problem the feature solves before writing the solution
4. Verify internal links point to sections that exist and have content
5. Check that every external tool or project mentioned has a hyperlink
