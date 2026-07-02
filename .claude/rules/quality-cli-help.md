---
paths:
  - crates/ocx_cli/src/**
---

# CLI Help & Command Documentation

How to write the help text a user reads when they run `ocx --help`,
`ocx <command> --help`, or `ocx <command> <subcommand> --help`. Auto-loads on
every edit under `crates/ocx_cli/src/**`, so it **always applies** to CLI work.

clap derive turns `///` doc comments into help: the **first paragraph** becomes the
SHORT help (`Command::about` / `Arg::help`, shown in parent listings and embedded as
completion tooltips); after a **blank line**, the whole comment becomes the LONG help
(`Command::long_about` / `Arg::long_help`, rendered by `--help`). Help text is a
**user-facing product surface**, not a place for design history.

This rule complements two siblings: `quality-rust.md` (the two-register comment model)
and `subsystem-cli.md` (the CLI architecture + command taxonomy).

---

## The One Rule

> **`///` on a clap surface states the user contract. Nothing else.**

What the command/flag does, how to invoke it, what input it takes, when it fails. A
user who has never read the codebase is the only audience. If a sentence only makes
sense to someone who read an ADR, a handshake, or a commit — it does not belong here.

---

## Two-Tier Help (mirrors clap)

| Tier | Source | Audience surface | Content |
|------|--------|------------------|---------|
| **Short** | first paragraph -> `about` / `help` | `--help` listings, completion tooltips | One imperative line: *what it does*. Target <= ~70 chars. No trailing period (clap strips it; keeps `...`). |
| **Long** | full doc after a blank line -> `long_about` / `long_help` | `<cmd> --help`, `<flag>` detail | Complete US-English sentences: what it does, the important flags, failure modes / exit codes, and a link to the docs site for depth. |

- **Lead with the action.** "Compose and print the toolchain environment." not
  "This command is responsible for composing...".
- **Link out for depth.** One or two inline examples maximum; heavy examples and full
  narrative live on the documentation website. Deep-link the relevant page/anchor
  (model: `crates/ocx_cli/src/options/content_path.rs`, which links
  `https://ocx.sh/docs/reference/command-line#path-resolution`).
- **Flags state their default and env-var equivalent** when they have one
  (e.g. "Defaults to plain." / "Equivalent env var: `OCX_REMOTE`.").

## Applies to every clap surface — not just flags

Document all four, to the same standard:

1. **Root command** (`Cli`).
2. **Subcommand variants** (the `///` on each `Command` / group-enum variant — this is
   what clap renders as that subcommand's `about`).
3. **Command-group dispatchers** (the enum behind `ocx package`, `ocx self`, ...).
4. **Arguments, flags, and possible-values.**

> **Render-source gotcha.** For a variant `Foo(FooArgs)`, clap renders the **variant**
> doc as the subcommand's about and each `FooArgs` field as an arg — but `FooArgs`'s own
> top-level `///` is **orphaned** (rustdoc only) *unless the variant has no doc*, in
> which case clap falls back to it. Put the user-facing description on the surface clap
> actually renders, and **confirm with `--help`**. The whole `Cli::command()` tree is
> walked by `app::tests::cli_help_text_is_ascii` — that traversal is the authoritative
> definition of "clap-facing."

---

## Forbidden in clap-facing help (Block-tier)

Design provenance and implementation rationale belong in `//` comments, `//!` module
docs, or ADRs — never in text a user sees. Specifically, no:

- **Section / clause references** — `handshake §3`, `section 3`, clause labels (`C1`,
  `C5`), `per the amended ...`.
- **ADR / spec / code-path references** — `adr_*.md`, `app::plugin_dispatch`, RFC/spec
  numbers, `[Self::build_api]` rustdoc links (they render as raw bracket noise in
  `--help`).
- **Dates and build timestamps** — `amended 2026-05-19`, `..._20260514120000`. Use a
  digit-free placeholder (`<YYYYMMDDhhmmss>`) in format examples.
- **Migration history** — "former root commands moved here", "deleted (handshake §7)",
  "formerly ...". Describe the present contract; the past is the changelog's job.
- **Implementation jargon** — "backend-first", "walk-order chain blob digests",
  "OCX is a backend tool". Say what the user gets, in their words.
- **Incorrect statements of behavior** — help that contradicts reality (e.g. claiming a
  JSON default when the default is plain). A stale help string is a Block-tier bug.

---

## Anti-Patterns (Tiered)

### Block (must fix before merge)
- Any forbidden item above in a **clap-rendered** string.
- Non-ASCII byte in help (Windows PowerShell 5.1 decodes captured completion/`--help`
  streams under the console codepage and mojibakes it). Replace `->` for an arrow,
  `-` for an em-dash, `...` for an ellipsis.

### Warn (should fix)
- Implementation rationale or internal jargon in `///` where a user sentence belongs.
- A `long_about` that dumps full narrative the documentation website should own.
- Example flood (> 2 inline examples) — move the rest to the website.
- Tautological help that restates the command/flag name with no added information
  (`/// The format` on `--format`).
- Abbreviated flag names / value placeholders — prefer full words, hyphen-separated
  (consistent with the identifier rule in `quality-rust.md`).
- `-file`/`-path` suffix on a flag that reads input — a flag is named for the thing it denotes
  (`--metadata`, `--readme`, `--logo`, `--script`, `--descriptor`), never suffixed
  `-file`/`-path`. That suffix is reserved for a flag that **writes to** a file (an output
  sink), e.g. `--export-file`.

### Suggest (improvement)
- Missing documentation deep-link for a behavior-rich command.
- Missing default / env-var note on a resolution-affecting flag.
- Short help longer than ~70 chars (won't fit a one-line listing cleanly).

---

## Automated Enforcement

These gates run in `task verify`; new CLI surfaces are covered automatically because the
guards walk the built `Cli::command()` tree:

| Gate | Asserts |
|------|---------|
| `app::tests::cli_definition_is_valid` | clap structural invariants (the tree builds). |
| `app::tests::cli_help_text_is_ascii` | every clap-rendered string is ASCII. |
| `app::tests::cli_help_text_has_no_internal_references` | no `§` / `handshake` / `adr_` / `amended` / ISO-date / 8+-digit timestamp leaks into help. |
| `test/tests/test_completion_ascii.py` | generated completion + `self activate` output bytes are ASCII. |

The reference guards are a backstop for the unambiguous markers, not a substitute for
applying this rule by hand: spelled-out `section N`, jargon, and stale facts are too
generic to detect and must be caught in review.

---

## See Also

- `quality-rust.md` — Comment Quality / two-register model (`///` contract vs `//`
  rationale); the register split this rule applies to CLI help.
- `subsystem-cli.md` / `subsystem-cli-commands.md` — CLI architecture, command taxonomy,
  and the canonical per-command docs at `website/src/docs/reference/command-line.md`
  (where depth lives).
- `docs-style.md` — how the documentation website narrative is written (the target for
  detail trimmed out of help).
- `quality-core.md` — universal anti-pattern severity tiers.
