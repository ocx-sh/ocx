# Component Examples

Custom Vue components available in all documentation pages.

---

## Tooltip

Annotates an inline term with expandable rich content.  Hover the underlined
word to see the tooltip.  The `term` attribute is the trigger; the slot accepts
any Markdown-rendered HTML including `<code>`, `<strong>`, links, etc.

**Usage**

```html
<Tooltip term="object store">
  Content-addressed storage keyed by <strong>SHA-256 digest</strong>.
  Files are never duplicated regardless of how many packages reference them.
</Tooltip>
```

**Props**

| Prop | Type | Default | Description |
|---|---|---|---|
| `term` | `string` | — | Trigger text with dashed underline |
| `side` | `'top' \| 'bottom' \| 'left' \| 'right'` | `'top'` | Preferred bubble placement |
| `delay-duration` | `number` | `400` | ms before tooltip opens |

**Live example**

Every <Tooltip term="package">A versioned bundle of pre-built binaries published to an OCI registry under a tag (e.g. <code>cmake:3.28</code>).</Tooltip>
installed by ocx lives in the <Tooltip term="object store">Content-addressed storage at <code>~/.ocx/objects/</code>. Each object is identified by its <strong>SHA-256 digest</strong> and is never duplicated regardless of how many tags point to it.</Tooltip>.
Running <Tooltip term="ocx clean" side="bottom">Removes all objects whose <code>refs/</code> directory is empty — i.e. no symlink points to them anymore. Use <code>--dry-run</code> to preview.</Tooltip>
removes any objects no longer referenced by a symlink.

---

## FileTree

Renders a collapsible, selectable directory-structure tree.  Use `<Tree>` with
nested `<Node>` tags — no JavaScript required.  Directories expand/collapse on
click; any row can be selected to highlight it.  When `open-icon` is provided,
directories swap icons between their collapsed and expanded states.

**Usage**

```html
<Tree>
  <Node name="root/" icon="📁" open-icon="📂" open>
    <Node name="child/" icon="📁" open-icon="📂">
      <Description>annotation</Description>
      <Node name="file.txt" icon="📄" />
    </Node>
  </Node>
</Tree>
```

**`<Node>` attributes**

| Attribute | Type | Description |
|---|---|---|
| `name` | `string` | File or directory name (`/` suffix is conventional for directories) |
| `<Description>` | sub-element | Muted annotation shown to the right |
| `icon` | `string` | Emoji or symbol; replaces the ▾/▸ toggle arrow |
| `open-icon` | `string` | Icon shown when this directory is expanded (falls back to `icon`) |
| `open` | `boolean` | Expanded on first render (default `true` for directories) |

**Live example**

<Tree>
  <Node name="~/.ocx/" icon="🏠" open>
    <Node name="objects/" icon="🗄️">
      <Description>content-addressed store</Description>
      <Node name="ocx.sh/" icon="📁" open-icon="📂">
        <Node name="cmake/" icon="📦">
          <Node name="sha256:abc123…/" icon="📁" open-icon="📂">
            <Description>one object per digest</Description>
            <Node name="content/" icon="📂">
              <Description>package files (read-only)</Description>
            </Node>
            <Node name="metadata.json" icon="📋">
              <Description>declared env vars, platforms</Description>
            </Node>
            <Node name="refs/" icon="🔗">
              <Description>back-references — guards GC</Description>
            </Node>
          </Node>
        </Node>
      </Node>
    </Node>
    <Node name="index/" icon="🗂️">
      <Description>local mirror of registry index</Description>
      <Node name="ocx.sh/" icon="📁" open-icon="📂">
        <Node name="tags/" icon="📁" open-icon="📂">
          <Node name="cmake.json" icon="📄">
            <Description>tag → digest mapping</Description>
          </Node>
        </Node>
      </Node>
    </Node>
    <Node name="installs/" icon="🔀" :open="false">
      <Description>forward symlinks</Description>
      <Node name="ocx.sh/" icon="📁" open-icon="📂">
        <Node name="cmake/" icon="📁" open-icon="📂">
          <Node name="current" icon="➡️">
            <Description>set by ocx select</Description>
          </Node>
          <Node name="candidates/" icon="📁" open-icon="📂">
            <Node name="3.28" icon="➡️">
              <Description>set by ocx install</Description>
            </Node>
          </Node>
        </Node>
      </Node>
    </Node>
  </Node>
</Tree>

---

## Stepper

A vertical progress indicator with an optional collapsible detail panel.
Use `<Steps>` with nested `<Step>` tags.  Slot content inside each `<Step>`
is rendered as **Markdown** and shown in the detail panel when that step is clicked.

**Usage**

```html
<Steps>
  <Step title="Step one" status="complete">
    <Description>Done.</Description>

    This was completed in **v0.1**. Commands shipped: `install`, `exec`.

  </Step>
  <Step title="Step two" status="current">
    <Description>Ongoing.</Description>
  </Step>
  <Step title="Step three" status="upcoming">
    <Description>Next up.</Description>
  </Step>
</Steps>
```

**`<Step>` attributes**

| Attribute | Type | Description |
|---|---|---|
| `title` | `string` | Short label next to the indicator |
| `<Description>` | sub-element | Supporting text below the title |
| `status` | `'complete' \| 'current' \| 'upcoming'` | Controls indicator colour and connector fill |
| slot content | Markdown | Rendered in the detail panel when this step is clicked; omit for non-clickable steps |

**Live example — roadmap**

<Steps>
  <Step title="v0.1 — Foundation" status="complete">
    <Description>OCI registry integration, basic install and run.</Description>

  First working release. Establishes the core OCI pull pipeline: fetch manifest → select platform layer → stream blob → extract to `~/.ocx/objects/`.

  Commands shipped: `install`, `exec`, `version`.

  </Step>
  <Step title="v0.2 — File structure" status="complete">
    <Description>ObjectStore, IndexStore, InstallStore refactoring.</Description>

  Replaced the monolithic `oci::FileStructure` with three focused types: `ObjectStore` (content-addressed blobs), `IndexStore` (local tag mirror), and `InstallStore` (symlinks).

  Full unit-test coverage for all three stores added in this milestone.

  </Step>
  <Step title="v0.3 — Reference model" status="current">
    <Description>ReferenceManager, GC, clean / uninstall / deselect.</Description>

  Introduces `ReferenceManager`: every forward symlink (`candidates/`, `current`) now creates a back-reference in the object's `refs/` directory.

  New commands: `deselect`, `uninstall`, `clean`, `find`. The GC (`ocx clean`) only removes objects whose `refs/` directory is empty.

  </Step>
  <Step title="v0.4 — Documentation" status="upcoming">
    <Description>User guide, command reference, architecture spec.</Description>

  Planned additions to the website: full *File Structure* section with ASCII diagram, complete command-line reference for all new commands, and an internal architecture spec covering the OCI-as-package-store design.

  </Step>
  <Step title="v0.5 — Ecosystem" status="upcoming">
    <Description>GitHub Actions integration, Bazel rules, shell profiles.</Description>

  First-party integrations: a GitHub Actions composite action for installing packages in CI, a Bazel rule for consuming packages as toolchains, and shell profile helpers for auto-exporting package environment variables.

  </Step>
</Steps>

---

## Terminal

Embeds an animated terminal session using [asciinema-player](https://docs.asciinema.org/manual/player/).
Use `<Terminal>` with nested `<Frame>` tags — each frame is one line of output
at a given timestamp.  Multiple frames sharing the same `at` value appear
simultaneously (multi-line output).

**Usage**

```html
<Terminal title="Installing a package">
  <Frame at="0">$ ocx install cmake</Frame>
  <Frame at="0.5">Downloading cmake@3.28.0...</Frame>
  <Frame at="1.5">Extracting to ~/.ocx/objects/...</Frame>
  <Frame at="2">Done. Installed cmake@3.28.0</Frame>
</Terminal>
```

**`<Terminal>` attributes**

| Attribute | Type | Default | Description |
|---|---|---|---|
| `title` | `string` | — | Title shown in the terminal chrome title bar |
| `src` | `string` | — | Path to a `.cast` file (alternative to inline frames) |
| `cols` | `number` | `80` | Terminal width in columns |
| `rows` | `number` | auto | Terminal height in rows (calculated from frame count) |
| `autoPlay` | `boolean` | `true` | Start playback automatically |
| `speed` | `number` | `1` | Playback speed multiplier |
| `idle-time-limit` | `number` | `2` | Compress pauses longer than this (seconds) |
| `loop` | `boolean` | `false` | Loop playback |
| `fit` | `'width' \| 'height' \| 'both' \| 'none'` | `'width'` | How the player scales |

**`<Frame>` attributes**

| Attribute | Type | Description |
|---|---|---|
| `at` | `number` | Time in seconds when this line appears |

**Live example — inline frames**

<Terminal title="Getting started with ocx">
  <Frame at="0">$ ocx install hello-world</Frame>
  <Frame at="1">Resolving hello-world@latest...</Frame>
  <Frame at="1.5">Downloading hello-world@1.0.0 [================] 100%</Frame>
  <Frame at="2.5">Installed hello-world@1.0.0</Frame>
  <Frame at="3.5">$ ocx exec hello-world -- hello</Frame>
  <Frame at="4.5">Hello, World!</Frame>
</Terminal>

**Live example — from `.cast` file**

<Terminal src="/casts/demo.cast" title="ocx workflow" :autoPlay="false" />
