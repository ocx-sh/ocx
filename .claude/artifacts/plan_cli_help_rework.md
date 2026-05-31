# Plan: CLI help-text quality rule + strip internal provenance from `--help`

Status: Proposed · Type: feature (new quality rule) + refactor (docs) + 1 doc-correctness fix · Date: 2026-05-31
Branch: `feat/inline-completions` · Scope: small–medium · Artifact target: `.claude/artifacts/plan_cli_help_rework.md`

---

## Context

`ocx` builds its CLI with clap derive: `///` doc comments become user-visible help. Over
many handshake/ADR-driven refactors, **design provenance leaked into `ocx <cmd> --help`** —
across command, subcommand, group, and argument docs alike:

- **Stale fact** — `ocx env` help says `Default output: JSON (backend-first, section 3)`.
  Wrong since handshake §3 amended (2026-05-19): every command defaults to **plain**.
- **Section / clause refs** — `handshake §2 / §7`, `section 3`, `C1`, `C5 alias`.
- **ADR / code-path refs** — `See adr_cli_plugin_pattern.md and app::plugin_dispatch`.
- **Dates** — `amended 2026-05-19`; 14-digit example `0.3.0-dev_20260514120000`.
- **Internal jargon / rationale** — `backend-first`, `OCX is a backend tool`,
  `applied by [Self::build_api]`, `walk-order chain blob digests`, "former root commands
  moved here", "deleted (handshake §7)".

There is **no rule today** that says how CLI help should read, so this keeps recurring. The
user's ask: research authoritative CLI-help best practice, **codify it as an always-on
quality rule** covering commands/subcommands/groups *and* arguments, then apply it and
guard it. The website `command-line.md` already carries ~85% of the user narrative, so
trimmed detail is **not lost** and needs **no migration**.

---

## Research — authoritative best practice (done)

| Source | Key guidance taken |
|--------|--------------------|
| **clig.dev** (Command Line Interface Guidelines) | Concise help by default (what it does + 1–2 examples + key flags + "pass `--help`"). `-h`/`--help` everywhere incl. subcommands. **Lead with examples**, but heavy examples go on a **web page**, not in help. **Link help to the web docs** (deep-link the subcommand anchor). |
| **GNU Coding Standards** | Short descriptions are lowercase fragments, not full sentences; longer text is complete sentences. Provide full-length `--flag` forms. |
| **Fuchsia CLI help** | Summary lines **begin lowercase, not full sentences** (`grind   make smaller by many small cuts`). Descriptions = complete US-English sentences. Options concise — **"put more detail in the tool description, Examples, or Notes, not a lengthy option description."** Wrap at 80 cols. Meaningful words, no abbreviations, hyphen-separate. Alphabetical command lists. |
| **clap derive** | First paragraph → `Command::about` / `Arg::help` (short); a **blank line** sends the whole comment to `long_about` / `long_help`. clap **strips the trailing period** of the first paragraph (keeps ellipsis), word-wraps paragraphs; `verbatim_doc_comment` only for tables/ASCII art. |

Convergent model: **two tiers** (short imperative line + optional full description), **user
contract only**, **link out for depth** — which maps exactly onto clap's about/long_about
split and onto the project's existing two-register comment rule.

---

## Deliverable 1 — new rule `.claude/rules/quality-cli-help.md`

A shareable, project-independent `quality-*.md` rule (OCX specifics flagged inline), built
in the house format (principles → anti-pattern tiers → enforcement → See Also).
**Auto-loads on `crates/ocx_cli/src/**`** so it always applies to CLI work.

Normative content:

1. **Two-tier help (mirrors clap).**
   - *Short* (first paragraph → `about` / arg `help`): one imperative line, "what it does",
     target ≤ ~70 chars; no trailing period (clap strips it); shown in parent listings and
     completion tooltips.
   - *Long* (after a blank line → `long_about` / arg `long_help`): complete US-English
     sentences — what it does, the important flags, failure modes / exit codes, and a
     **link to the website** for depth. ≤ 1–2 inline examples; the rest live on the web.

2. **Applies to every clap-rendered surface** (the user's point): root command, **subcommand
   variants**, **command-group enum dispatchers**, and arguments / flags / possible-values —
   not just parameters. The description must sit on the surface clap actually renders
   (variant doc; or a payload struct/enum doc only when that is what clap renders — confirm
   with `--help`). Mixing user-contract and internal rationale in one `///` is the smell.

3. **Register split (cross-links `quality-rust.md` two-register model).** Clap-facing `///`
   = user contract ONLY. **Forbidden in clap help:** design provenance (handshake/ADR/§/spec
   references, clause labels like `C1`/`C5`), dates/timestamps, implementation rationale
   (`build_api`, "backend-first"), migration history ("moved here", "deleted", "former",
   "amended"). That content belongs in `//` / `//!` / ADRs.

4. **Style conventions.** ASCII-only (WinPS 5.1 console-codepage hazard — cross-ref the ASCII
   guard); no abbreviations in names/placeholders, hyphen-separate (consistent with
   `quality-rust.md` identifiers); wrap ~80 cols; state a flag's **default** and **env-var
   equivalent**; **deep-link canonical web docs** (`https://ocx.sh/docs/reference/command-line#anchor`)
   for detail — model: `crates/ocx_cli/src/options/content_path.rs` (already does this well).

5. **Anti-patterns (tiered).**
   - **Block** — internal design reference in clap-facing help; a help string that states
     behavior incorrectly (e.g. wrong default format); non-ASCII byte in help.
   - **Warn** — implementation rationale / jargon in `///`; a `long_about` that dumps full
     narrative the website should own; example flood (>2 inline); tautological help that
     restates the command/flag name with no added information.
   - **Suggest** — missing website deep-link for a behavior-rich command; missing
     default / env-var note on a resolution flag; short help > ~70 chars.

6. **Automated enforcement.** `cli_definition_is_valid`, `cli_help_text_is_ascii`,
   `cli_help_text_has_no_internal_references` (new, Deliverable 3), `test_completion_ascii.py`.
   New CLI surfaces are covered automatically because the guards walk the built command tree.

7. **See Also** — `subsystem-cli.md`, `subsystem-cli-commands.md`, `quality-rust.md`
   (two-register), `docs-style.md` (website narrative owns depth), `quality-core.md`.

---

## Deliverable 2 — register the rule in AI config (meta-ai-config protocol)

Same-commit catalog updates (then `task claude:tests` must pass):

- `.claude/rules.md`:
  - "By concern" → **CLI command changes** row: add `quality-cli-help.md`.
  - "By auto-load path" → `crates/ocx_cli/src/**` row: add `quality-cli-help.md`.
  - "Declared Path-Scope Overlaps": add whatever the structural test requires for
    `quality-cli-help.md` × `subsystem-cli.md` (shared `crates/ocx_cli/src/**`) and ×
    `quality-rust.md` (`**/*.rs`). The shareable-`quality-*` exemption *may* already cover
    these — run `task claude:tests` and add a declared group only if it fails.
- `CLAUDE.md`: the "By concern" pointer lives in `rules.md` (single source of truth); add a
  CLAUDE.md reference only if `test_ai_config.py` flags drift.
- Structural gate: `task claude:tests` green (catalog ↔ reality).

---

## Deliverable 3 — regression guard `cli_help_text_has_no_internal_references`

In `crates/ocx_cli/src/app.rs` `mod tests`. **Extract** the `check`/`record` walk already in
`cli_help_text_is_ascii` into a shared collector so both guards cover the identical set of
clap-rendered strings (single source of truth for "what is help text"):

```rust
/// Every (location, help-text) clap renders: command about/long_about + before/after(_long)
/// help, each arg help/long_help, and possible-value names + help.
fn collect_clap_help_texts() -> Vec<(String, String)> { /* the existing recursive walk */ }
```

New guard (regex-free — no new dep; markers chosen to avoid false positives in real help):

```rust
fn marker(text: &str) -> Option<&'static str> {
    let lower = text.to_ascii_lowercase();
    if text.contains('§')           { return Some("`§` section sign"); }
    if lower.contains("handshake")  { return Some("`handshake`"); }
    if lower.contains("adr_")       { return Some("`adr_` reference"); }
    if lower.contains("amended")    { return Some("`amended` (design history)"); }
    if has_iso_date(text)           { return Some("ISO date (YYYY-MM-DD)"); }   // 20dd-dd-dd
    if max_digit_run(text) >= 8     { return Some("8+ digit timestamp"); }      // 20260514120000
    None
}
```
`has_iso_date` / `max_digit_run` = small char-scan helpers. This is a **backstop for the
unambiguous re-entry vectors**, not the full worklist — spelled-out `section N`,
`backend-first`, the stale "JSON default", and clause labels (`C5`) are too generic to guard
and are removed by hand per the inventory.

---

## Deliverable 4 — apply the rule (rework the help text)

### Surfacing model — what clap actually renders (verified)

For a subcommand variant `Foo(FooArgs)`: the **variant** doc → that subcommand's
`about`/`long_about`; each `FooArgs` field → an **arg** with its own help; **`FooArgs`'s own
top-level `///` is orphaned** (rustdoc only) **unless the variant has no doc**, in which case
clap falls back to the payload struct/enum doc. Proof: the passing `cli_help_text_is_ascii`
guard walks the real `Cli::command()` tree, yet `ToolchainEnv` / `SelfActivate` struct docs
contain non-ASCII — so clap never surfaces them. **The guard traversal is the authoritative
definition of "clap-facing."** The worklist is therefore confirmed empirically (guard report
+ `--help` dumps), not by guessing clap's merge rules.

### Candidate offenders (confirm rendered before editing)

| File:line | Surface | Problem | Guard catches? |
|-----------|---------|---------|----------------|
| `command.rs:62-67` | `ocx env` variant `about` | stale `Default output: JSON`, `backend-first`, `section 3` | No → manual |
| `command.rs:109-110` | `External` variant `about` | `See adr_cli_plugin_pattern.md and app::plugin_dispatch` | `adr_` → yes (if rendered) |
| `app/context_options.rs:68-73` | `--format` arg `long_help` | `handshake section 3 amended 2026-05-19`, `[Self::build_api]` | `amended`+date → yes |
| `command/package_push.rs:33-39` | `--build-timestamp` arg `long_help` | 14-digit example `20260514120000` | 8+-digit → yes |
| `command/toolchain_env.rs:103` | `ocx env --shell` arg `long_help` | `C5 alias, no new variant` | No → manual |
| `command/package.rs:8-15` *(if rendered as `ocx package` about via no-doc fallback)* | group `about` | `handshake §2 / §7`, `C1`, "former root commands moved here" | `§`/`handshake` → yes |
| `command/shell.rs:8-12` *(Shell variant in `command.rs` has NO doc → likely renders)* | `ocx shell` about | `handshake section 7`, "deleted" history | No → manual |
| `command/package_inspect.rs:10-28` *(if rendered)* | `ocx package inspect` long_about | verbose + `OCX is a backend tool` + jargon | No → manual (Warn-tier trim) |

Out of scope (orphaned internal rustdoc, per "clap-facing only"): module `//!`, internal-fn
`///`, `api/data/*` docs, and any payload struct/enum doc the `--help` dump shows is **not**
rendered. These keep design provenance — the correct register.

### Sample rewrites (ASCII, user-facing, two-tier)

`command.rs` `Env` variant (fixes the stale fact):
```rust
/// Compose and print the toolchain environment.
///
/// Reads the in-scope `ocx.toml` + `ocx.lock` (or the global toolchain under
/// `--global`). Defaults to a plain table; `ocx --format json env` emits JSON.
/// `--shell[=NAME]` is the only eval-safe form.
```
`app/context_options.rs` `--format`:
```rust
/// Output format for stdout reports: `plain` (default) or `json`.
///
/// Applies to every command; there is no per-command `--format`. The
/// `--shell[=NAME]` output of `env` / `package env` is unaffected.
```
`command.rs` `External`: `/// External subcommand: dispatched to an `ocx-<name>` binary found on PATH.`
`package_push.rs`: replace `…_20260514120000` with digit-free `…_<YYYYMMDDhhmmss>`.
`toolchain_env.rs:103`: `` `--shell=sh` is an alias for `--shell=dash` (POSIX strict). ``
Group docs (`ocx package`, `ocx shell`): one clean imperative line on the **variant** in
`command.rs` (add the missing `Shell` variant doc) so the rendered source is unambiguous;
leave the orphaned enum docs alone. Everything the guard / `--help` dump flags gets the same
treatment: say what it does; delete the history.

---

## Implementation order (contract-first)

1. Persist this plan → `.claude/artifacts/plan_cli_help_rework.md`.
2. Author `quality-cli-help.md`; register in `rules.md`; `task claude:tests` green.
3. Add `collect_clap_help_texts` + `cli_help_text_has_no_internal_references`; run it →
   record authoritative marker offenders.
4. Dump `--help` for `ocx`, `ocx env`, `ocx package`, `ocx package inspect`, `ocx shell`,
   `ocx self activate` → confirm which command/subcommand/group/arg strings are rendered.
5. Rewrite confirmed clap-facing offenders per the rule (commands + subcommands + groups +
   args). Do not touch orphaned rustdoc.
6. Re-run both guards until green and the `--help` dumps read cleanly.

---

## Verification

- `cargo test -p ocx --bin ocx cli_help_text_has_no_internal_references cli_help_text_is_ascii cli_definition_is_valid` — all pass.
- `--help` dumps re-read: no `§`/section/handshake/ADR/date/jargon; `ocx env --help` states plain-default.
- `cd test && uv run pytest tests/test_completion_ascii.py -q --no-build` — 17 pass.
- `task claude:tests` — AI-config structural gate green.
- `task verify` — full gate green before commit.

## Commits (after verify green; never push)

- `chore(ai): add quality-cli-help rule + catalog wiring` — Deliverables 1 & 2.
- `test(cli): guard CLI help against internal design references` — Deliverable 3.
- `docs(cli): strip internal design references from --help text` — Deliverable 4.

## Follow-ups / notes

- **Memory (after implementation):** record the preference *"codify recurring quality issues
  as a researched, always-on quality rule — not a one-off fix"* under the user's OCX memory.
- **Risk:** misclassifying a string as orphaned → every edit confirmed against an actual
  `--help` dump + the guard walks the real built tree.
- **Risk:** guard false positives on a legitimate future date/flag → keep the marker set
  narrow; prefer digit-free phrasing or an explicit scoped exception, never widen silently.
