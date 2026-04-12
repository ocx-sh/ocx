# Research: Code Comment Best Practices & AI Comment Noise

**Date:** 2026-04-12
**Domain:** Code quality, Rust, AI-assisted development
**Triggered by:** Excessive AI-generated comment noise in OCX codebase
**Expires:** 2027-04-12

---

## Core Principle

Comments should communicate what the code cannot. The "why not what" shorthand is useful but imprecise — the real split is **intent/rationale vs. mechanics**. Mechanics live in code; rationale lives in comments.

**Ousterhout's test** (A Philosophy of Software Design): "If someone unfamiliar with the code could write your comment just by reading it, it adds no value."

**Fowler's extraction rule** (Refactoring): "A block of code with a comment that tells you what it is doing can be replaced by a method whose name is based on the comment."

---

## Comment Smell Taxonomy

From peer-reviewed research (Springer Empirical Software Engineering, 2024) — 11 inline code comment smell types:

| # | Smell | Description | AI Frequency |
|---|-------|-------------|-------------|
| 1 | **Obvious** | Restates what code expresses | Very High |
| 2 | **Redundant** | Duplicates info in same scope | High |
| 3 | **Misleading** | Contradicts current behavior | Low |
| 4 | **Vague** | Lacks actionable info ("fix this") | Low |
| 5 | **Mumbling** | Developer talking to themselves | Medium |
| 6 | **Outdated** | Was accurate, no longer matches | Low |
| 7 | **Commented-out code** | Dead code in comment form | Low |
| 8 | **Journal** | Changelog entries (author, date) | Low |
| 9 | **Closing brace** | `} // end if` | Low |
| 10 | **Mandated** | Required by policy regardless of value | Medium |
| 11 | **Excessive** | Individually OK, collectively noise | Very High |

AI tools primarily produce smells #1, #2, #10, and #11.

---

## The AI Comment Problem

LLMs are trained on tutorial-style code and produce over-explained output by default. Over-commenting is the **first dead giveaway** of LLM-generated code (DEV Community, 2025).

Key findings:
- LLMs have "an implicit bias towards blathering" — boilerplate is high-probability output (HN, 2024)
- AI-generated code "reads like a tutorial rather than a project"
- 60-80% of AI-generated code review comments are noise (Gitar, 2024)
- Google's internal AutoCommenter achieved only 54% "useful" rate

**Mitigation consensus**: explicit instructions in AI config files (CLAUDE.md, .cursorrules) are the most effective lever. Positive framing works better than negative: "Write comments explaining rationale only" beats "Don't write unnecessary comments."

---

## Three Tiers of Legitimate Comment Content

| Tier | Content | Example |
|------|---------|---------|
| **Rationale** | Why this design over obvious alternatives | `// BTreeMap for deterministic serialization order` |
| **Non-obvious invariant** | Constraint the type system can't express | `// Slice guaranteed non-empty by constructor` |
| **External reference** | Links to specs, RFCs, algorithms | `// Per RFC 7234 S5.2.2.4: max-age=0 requires revalidation` |

---

## Rust-Specific Conventions

**RFC 505** (API Comment Conventions): First line is a short imperative sentence. Full sentences, proper punctuation. American English.

**RFC 1574** (More API Documentation Conventions): Standard sections — `# Examples`, `# Panics`, `# Errors`, `# Safety`.

**Rust API Guidelines**: Every public item gets a `///` doc comment. First sentence is standalone summary. Functions returning `Result` need `# Errors`. `unsafe` blocks need `// SAFETY:`.

**Two-register model for OCX**:
- `///` rustdoc: contract-level docs for API consumers (what, when it fails, invariants)
- `//` inline: implementation-level rationale for maintainers (why this approach)
- Never mix: don't explain implementation in `///`, don't explain contracts in `//`

---

## Automated Enforcement

| Tool | What It Catches |
|------|-----------------|
| `clippy::undocumented_unsafe_blocks` | `unsafe` without `// SAFETY:` |
| `clippy::doc_markdown` | CamelCase/paths not in backticks |
| `clippy::empty_docs` | Empty `///` comments |
| `rustdoc::broken_intra_doc_links` | Dead intra-doc links |
| `missing_docs` (rustc) | Public items without docs |

**Gap**: No tool detects "obvious" comments. This requires code review or AI config rules.

**Dylint** (Trail of Bits): Custom Clippy-compatible lints without forking. Could detect narration patterns (`// Get`, `// Set`, `// Return` above matching operations). No public implementation exists yet.

---

## Sources

- [Rust API Guidelines — Documentation](https://rust-lang.github.io/api-guidelines/documentation.html)
- [RFC 505: API Comment Conventions](https://rust-lang.github.io/rfcs/0505-api-comment-conventions.html)
- [RFC 1574: More API Documentation Conventions](https://rust-lang.github.io/rfcs/1574-more-api-documentation-conventions.html)
- [Martin Fowler — CodeAsDocumentation](https://martinfowler.com/bliki/CodeAsDocumentation.html)
- [A Philosophy of Software Design (notes)](https://www.mattduck.com/2021-04-a-philosophy-of-software-design.html)
- [Clean Code Summary](https://gist.github.com/wojteklu/73c6914cc446146b8b533c0988cf8d29)
- [Stack Overflow Blog — Best Practices for Writing Code Comments](https://stackoverflow.blog/2021/12/23/best-practices-for-writing-code-comments/)
- [Taxonomy of Inline Code Comment Smells (Springer, 2024)](https://link.springer.com/article/10.1007/s10664-023-10425-5)
- [Code Comment Anti-Patterns (bytedev)](https://bytedev.medium.com/code-comment-anti-patterns-and-why-the-comment-you-just-wrote-is-probably-not-needed-919a92cf6758)
- [Was this Python written by an AI? (DEV, 2025)](https://dev.to/dev_tips/was-this-python-written-by-a-human-or-an-ai-7-signs-to-spot-llm-generated-code-3370)
- [AI Code Review Without the Comment Spam (Gitar)](https://gitar.ai/blog/ai-code-review-without-the-comment-spam)
- [Write Rust Lints Without Forking Clippy (Trail of Bits)](https://blog.trailofbits.com/2021/11/09/write-rust-lints-without-forking-clippy/)
- [Linux Kernel Coding Style](https://docs.kernel.org/process/coding-style.html)
- [Cursor Forum — unnecessary comments](https://forum.cursor.com/t/how-to-tell-the-model-not-write-unnecessary-comments/105136)
