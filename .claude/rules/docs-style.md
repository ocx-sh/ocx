---
paths:
  - website/**
---

# Documentation Instructions

Rules from OCX user guide review. Apply to new doc pages.

---

## Narrative Structure

Every `##` section open with two-three short paragraphs, build in sequence:

1. **The idea** — what concept this section cover? One sentence frame.
2. **The problem** — why matter? Show concrete real-world pain (not hypothetical). Use examples from familiar tools reader know.
3. **The solution** — how ocx address? Short, direct.

Then subsections for depth, comparisons, design decisions.

No sales pitch or marketing open. "One identifier. All platforms. No hash list." — avoid. Examples make case.

---

## Paragraph Style

- **Short paragraphs.** One idea per paragraph, especially section intros.
- **No stop-and-go.** No jumping between concepts without transition. Each paragraph lead to next.
- **Tables and code blocks** follow prose; prose set context first.
- **No dump lists of commands** without explaining what they represent.

---

## Headers

- Short headers — appear in right-hand outline/TOC, read as compelling chapter titles.
- Bad: `### Automatic Detection of Platform`, `### Embedded Indexes and Locked Environments`
- Good: `### Auto-Detection`, `### Locking`
- Use `{#custom-anchor}` on every section heading, pattern `{#parent-subsection}` for nesting.

---

## Real-World Examples and External Links

**Always search internet** before writing comparisons or analogies. No describing other tools from memory — fetch real docs, real examples.

- Concrete command sequences, real filenames, real repo links — not abstract descriptions.
- Useful sources: apt cross-arch (`dpkg --add-architecture`, ports mirrors), pip wheel filenames, Bazel `toolchains_llvm` dictionary, Docker official images, semver.org, GitHub Actions pinning docs.
- **Every external tool mentioned must hyperlink** — every occurrence, not just first. Never write "Bazel rules" or "devcontainer features" without link.
- Link OCI spec when discussing OCI-specific behavior (e.g. [OCI Image Index](https://github.com/opencontainers/image-spec/blob/main/image-index.md)).

---

## Analogies and Cross-References to Other Systems

When introducing design concept, compare to something reader know:

- Object store → Nix store, Git objects
- Index snapshot → APT package lists (`apt-get update`)
- Candidate/current symlinks → SDKMAN, Homebrew Cellar + opt, Linux `update-alternatives`
- Rolling tags → Docker official images (`ubuntu:24.04`, `nginx:latest`), Semantic Versioning

Analogies go in `:::info` callout, not inline prose. Keep main prose clean.

---

## Precision and Nuance

Be exactly correct. Nuances that came up:

- **OCI tags are mutable.** Never imply tag is "frozen" or "pinned" absolute. `_build` suffix is publisher *convention*, not enforced by registry.
- **"Pinned until index refresh" applies to ALL tags equally.** Distinction between rolling and build-tagged is what happens *after* refresh: rolling tags advance, build-tagged conventionally stay same. Table column should be "After index refresh", not "Resolves to".
- **Content-addressed = universally lockable.** Any package pin with digest (`cmake@sha256:abc…`), bypass tags and indexes. Make clear in locking docs before describing convenient options.
- **Cascade is convention, not enforced.** Publishers maintain manually. `ocx package push --cascade` automates, but not guaranteed at registry level.

---

## Tooltips

Use `<Tooltip term="short label">explanation</Tooltip>` to hide technical detail that interrupt prose flow.

- `term` appears inline (underlined). Natural noun phrase fitting sentence.
- Slot content is popup text. Backticks for inline code inside tooltips (render as `<code>`).
- Good candidates: technical terms, jargon, protocol-level concepts, long command sequences cluttering sentence.
- Bad candidates: anything reader need to follow flow — put in prose.

---

## Callout Boxes (VitePress)

| Type | When to use |
|---|---|
| `:::info` | Analogies to other systems, background context |
| `:::tip` | Actionable advice, example usage, recommended patterns |
| `:::warning` | Important caveats, commonly misunderstood things |
| `:::details` | Optional technical depth, spec references, implementation details |

Details blocks hide content by default — use for material correct and relevant but not required for main explanation.

---

## Internal Links

- **Every reference to another part of system must hyperlink.** "local index", "object store", "candidate symlink", etc. — all linked.
- Link to section with actual content, not empty stubs. Check anchor target exists and has prose before linking.
- Consistent anchor IDs: `#file-structure-index`, `#file-structure-objects`, `#path-resolution`, etc.

---

## Link Syntax

Use reference-style links — **never inline `[text](url)` in body**:

```markdown
See the [OCI Image Index specification][oci-image-index] for details.

[oci-image-index]: https://github.com/opencontainers/image-spec/blob/main/image-index.md
```

Collect all link definitions at **bottom of file**, grouped with comments:

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

All components globally registered — use directly in `.md` files without imports. See `subsystem-website.md` for full props reference, usage examples, VNode introspection pattern.

- `<Tooltip term="label">popup text</Tooltip>` — inline term with hover popup
- `<Tree>` / `<Node>` / `<Description>` — collapsible filesystem trees with annotations
- `<Steps>` / `<Step>` / `<Description>` — vertical progress indicator with detail panels
- `<Terminal>` / `<Frame>` — animated terminal sessions (inline frames or `.cast` files)
- `<PackageCatalog>` / `<PackageDetail>` — rendered from generated catalog data
- `<DependencyExplorer>` — SBOM viewer (rendered from generated dependency data)
- VitePress `::: code-group` — tabbed code blocks for side-by-side alternatives
- VitePress `::: info | tip | warning | details` — callout boxes

---

## Before Writing

1. Read source code to understand actual behavior — no documenting from memory.
2. Search internet for real examples from other ecosystems.
3. Identify problem feature solves before writing solution.
4. Verify internal links point to sections that exist and have content.
5. Check every external tool or project mentioned has hyperlink.