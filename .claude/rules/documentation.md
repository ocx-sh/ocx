# Documentation Instructions

Guidelines distilled from review feedback on the OCX user guide. Apply these to any new documentation page.

---

## Narrative Structure

Every section at the `##` level should open with two or three short paragraphs that build in sequence:

1. **The idea** — what concept or dimension does this section address? One sentence framing.
2. **The problem** — why does it matter? Show a concrete, real-world pain point (not hypothetical). Use examples from familiar tools the reader already knows.
3. **The solution** — how does ocx address it? Short and direct.

Then go into subsections for depth, comparisons, and design decisions.

Do not open a section with a sales pitch or marketing language. "One identifier. All platforms. No hash list." — avoid this tone. Let the examples make the case.

---

## Paragraph Style

- **Short paragraphs.** One idea per paragraph, especially in section intros.
- **No stop-and-go.** Avoid jumping between concepts without transition. Each paragraph should lead naturally into the next.
- **Tables and code blocks** follow prose; prose sets the context first.
- **Do not dump lists of commands** without explaining what they represent.

---

## Headers

- Keep headers short — they appear in the right-hand outline/TOC and should read as compelling chapter titles.
- Bad: `### Automatic Detection of Platform`, `### Embedded Indexes and Locked Environments`
- Good: `### Auto-Detection`, `### Locking`
- Use `{#custom-anchor}` on every section heading, following the pattern `{#parent-subsection}` for nesting.

---

## Real-World Examples and External Links

**Always search the internet** before writing comparisons or analogies. Do not describe other tools from memory alone — fetch real docs and real examples.

- Use concrete command sequences, real filenames, real repository links — not abstract descriptions.
- Example sources that have been useful: apt cross-arch (`dpkg --add-architecture`, ports mirrors), pip wheel filenames, Bazel `toolchains_llvm` dictionary, Docker official images, semver.org, GitHub Actions pinning docs.
- **Every external tool mentioned must be hyperlinked** — every occurrence, not just the first. Never write "Bazel rules" or "devcontainer features" without a link.
- Link to the OCI spec when discussing OCI-specific behavior (e.g. [OCI Image Index](https://github.com/opencontainers/image-spec/blob/main/image-index.md)).

---

## Analogies and Cross-References to Other Systems

When introducing a design concept, compare it to something the reader already knows:

- Object store → Nix store, Git objects
- Index snapshot → APT package lists (`apt-get update`)
- Candidate/current symlinks → SDKMAN, Homebrew Cellar + opt, Linux `update-alternatives`
- Rolling tags → Docker official images (`ubuntu:24.04`, `nginx:latest`), Semantic Versioning

Analogies go in `:::info` callout boxes, not inline prose. Keep the main prose clean.

---

## Precision and Nuance

Be exactly correct. Important nuances that have come up:

- **OCI tags are mutable.** Never imply a tag is "frozen" or "pinned" in any absolute sense. The `+build` suffix is a publisher *convention*, not enforced by the registry.
- **"Pinned until index refresh" applies to ALL tags equally.** The distinction between rolling and build-tagged is what happens *after* a refresh: rolling tags advance, build-tagged ones conventionally stay the same. The table column should be "After index refresh", not "Resolves to".
- **Content-addressed = universally lockable.** Any package can be pinned with a digest (`cmake@sha256:abc…`), bypassing tags and indexes entirely. Make this clear in locking documentation before describing the more convenient options.
- **Cascade is a convention, not enforced.** Publishers must maintain it manually. `ocx package push --cascade` automates this, but it is not guaranteed behavior at the registry level.

---

## Tooltips

Use `<Tooltip term="short label">explanation</Tooltip>` to hide technical detail that would interrupt the prose flow.

- The `term` is what appears inline (underlined). It should be a natural noun phrase that fits in the sentence.
- The slot content is the popup text. Use backticks for inline code inside tooltips (they render as `<code>`).
- Good candidates for tooltips: technical terms, jargon, protocol-level concepts, long command sequences that would clutter the sentence.
- Bad candidates: anything the reader needs to understand to follow the flow — put that in the prose.

---

## Callout Boxes (VitePress)

| Type | When to use |
|---|---|
| `:::info` | Analogies to other systems, background context |
| `:::tip` | Actionable advice, example usage, recommended patterns |
| `:::warning` | Important caveats, things that are commonly misunderstood |
| `:::details` | Optional technical depth, spec references, implementation details |

Details blocks hide content by default — use them for material that is correct and relevant but not required to follow the main explanation.

---

## Internal Links

- **Every reference to another part of the system must be a hyperlink.** "local index", "object store", "candidate symlink", etc. — all linked.
- Link to the section that has actual content, not to empty stubs. Check that the anchor target exists and has prose before linking to it.
- Use consistent anchor IDs: `#file-structure-index`, `#file-structure-objects`, `#path-resolution`, etc.

---

## Link Syntax

Use reference-style links throughout — **never inline `[text](url)` in the body**:

```markdown
See the [OCI Image Index specification][oci-image-index] for details.

[oci-image-index]: https://github.com/opencontainers/image-spec/blob/main/image-index.md
```

Collect all link definitions at the **bottom of the file**, grouped with comments:

```markdown
<!-- external -->
[nix]: https://nixos.org/
[semver]: https://semver.org/
...

<!-- commands -->
[cmd-install]: ./reference/command-line.md#install
...

<!-- environment -->
[env-auth-type]: ./reference/environment.md#ocx-auth-registry-type
...

<!-- internal -->
[fs-index]: #file-structure-index
[fs-objects]: #file-structure-objects
```

---

## Vue Components Available

All components are globally registered — use directly in `.md` files without imports. See the `documentation` skill for full props reference and usage examples.

- `<Tooltip term="label">popup text</Tooltip>` — inline term with hover popup
- `<Tree>` / `<Node>` / `<Description>` — collapsible filesystem trees with annotations
- `<Steps>` / `<Step>` / `<Description>` — vertical progress indicator with detail panels
- `<Terminal>` / `<Frame>` — animated terminal sessions (inline frames or `.cast` files)
- VitePress `::: code-group` — tabbed code blocks for showing alternatives side-by-side
- VitePress `::: info | tip | warning | details` — callout boxes

---

## Before Writing

1. Read the relevant source code to understand the actual behavior — do not document from memory.
2. Search the internet for real examples from other ecosystems.
3. Identify the problem the feature solves before writing the solution.
4. Verify that any internal links point to sections that exist and have content.
5. Check that every external tool or project mentioned has a hyperlink.
