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

Analogies go in `:::note` callout, not inline prose. Keep main prose clean.

---

## Precision and Nuance

Be exactly correct. Nuances that came up:

- **OCI tags are mutable.** Never imply tag is "frozen" or "pinned" absolute. `_build` suffix is publisher *convention*, not enforced by registry.
- **"Pinned until index refresh" applies to ALL tags equally.** Distinction between rolling and build-tagged is what happens *after* refresh: rolling tags advance, build-tagged conventionally stay same. Table column should be "After index refresh", not "Resolves to".
- **Content-addressed = universally lockable.** Any package pin with digest (`kitware/cmake@sha256:abc…`), bypass tags and indexes. Make clear in locking docs before describing convenient options.
- **Cascade is convention, not enforced.** Publishers maintain manually. `ocx package push --cascade` automates, but not guaranteed at registry level.

---

## Tooltips

Use `<Tooltip term="short label">explanation</Tooltip>` to hide technical detail that interrupt prose flow.

- `term` appears inline (underlined). Natural noun phrase fitting sentence.
- Slot content is popup text. Backticks for inline code inside tooltips (render as `<code>`).
- Good candidates: technical terms, jargon, protocol-level concepts, long command sequences cluttering sentence.
- Bad candidates: anything reader need to follow flow — put in prose.

---

## Callout Boxes

| Type | When to use |
|---|---|
| `:::note` | Analogies to other systems, background context |
| `:::tip` | Actionable advice, example usage, recommended patterns |
| `:::caution` | Important caveats, commonly misunderstood things |
| `:::danger` | Data loss, security or other irreversible consequences |

Optional technical depth (spec references, implementation details) goes in a plain `<details>` element, not a callout.

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
[cmd-exec]: ./reference/command-line/commands.md#exec
...

<!-- environment -->
[env-auth-token]: ./reference/environment.md#ocx-auth-registry-token
...

<!-- internal -->
[fs-index]: #file-structure-index
[fs-objects]: #file-structure-objects
```

---

## Components Available

A page using a component is `.mdx` and imports it; props → `subsystem-website.md` § Components and Markup.

- `<Tooltip term="label">popup text</Tooltip>` — inline term with hover popup (`@ocx-sh/theme/components/Tooltip.astro`)
- `<Tree>` / `<Node>` / `<Description>` — filesystem trees with annotations (`Tree.astro`, `TreeNode.astro`, `TreeDescription.astro`)
- `<Tabs>` / `<TabItem>` — tabbed alternatives (`Tabs.astro`, `TabItem.astro`)
- `<Steps>` — numbered procedure (`@astrojs/starlight/components`)
- `<Terminal>` — recorded `.cast` session (`@ocx-sh/theme/components/Terminal.astro`)
- `<PlatformIcons>` — OS glyph lists (`PlatformIcons.astro`)
- `:::note | tip | caution | danger` — callout boxes

---

## Casts Show Work, Not Grammar

Never record a `.cast` — or write a `test/doc_scripts/*.sh` to back one — whose output is a
`--help` dump, a usage line, or a list of argv refusals. That duplicates
`reference/command-line/`, which `task website:reference:generate` builds from the clap
definitions and is the one place a reader should look for grammar. Link the reference section instead.

A cast earns its place only when it shows a command **doing** something whose output the
reader could not predict from the flag list: an install that resolves a version, an env that
composes, a push that reuses a layer. If the recording's value is "here is what the flags
are", delete it and write a sentence with a link.

---

## Reference Pages

A `doc_type: reference` page is an optional intro of at most two sentences, then entries. An entry is
a heading with an explicit `{#kebab-id}` followed only by:

- a description of at most two sentences,
- fact tables or `key: value` lists (type, default, env, flag),
- at most one fenced block,
- at most one `See [x](...)` link line.

No asides, rationale, conventions or sub-sections that are not entries. Explanation moves to the
explanation page that owns it; `test/lint/test_reference_shape.py` reports `page:line rule`
(`paragraph`, `aside`, `fences`, `anchor`) and `test/lint/test_docs_quality_gate.py` fails on
any finding, with no exemption list.

---

## Before Writing

1. Read source code to understand actual behavior — no documenting from memory.
2. Search internet for real examples from other ecosystems.
3. Identify problem feature solves before writing solution.
4. Verify internal links point to sections that exist and have content.
5. Check every external tool or project mentioned has hyperlink.