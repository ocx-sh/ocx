# ADR: CI env export via `--ci` flag on `ocx env` / `ocx package env`

## Metadata

**Status:** Accepted
**Date:** 2026-06-02
**Deciders:** Architect (planning agent), Michael Herwig
**Beads Issue:** N/A
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md` — Rust 2024, no new dependency (reuses `serde_json`, `indexmap`, the existing `ci/` library half, the `Entry`/`ModifierKind` resolution surface, and the `--shell` flag grammar).
**Domain Tags:** api | integration | devops
**Supersedes:** N/A
**Superseded By:** N/A
**Realizes:** [`handshake_toolchain_cli.md`](./handshake_toolchain_cli.md) §6 "CI export" extension point.

## Context

`ocx env` and `ocx package env` already emit a composed environment in three
forms: a structured report (plain/JSON via the context `--format`) and an
eval-safe `--shell[=NAME]` stream. The `--shell` stream affects only the
**current** shell/step. What was missing is a **CI sink**: writing the same
composed environment into a CI system's persistence channel so tool dirs and
env vars are available to *later* pipeline steps.

The handshake (`handshake_toolchain_cli.md` §6, "On the radar") reserved this
surface deliberately and pinned its grammar:

> CI export. GitHub Actions (`$GITHUB_ENV`/`$GITHUB_PATH`, `KEY=VALUE`,
> autodetected sink, current-job) and GitLab CI/CD (JSON-lines
> `{"name","value"}`, path from `${{ export_file }}` passed explicitly, no path
> channel, *later-step* scope) differ enough that the destination/provider must
> be explicit value-flags (`--ci=<provider>`, an explicit out-path), never
> positionals or subcommands.

The library half already existed in `crates/ocx_lib/src/ci/`: a `CiFlavor`
enum, a `Flavor` trait (`write_entry`/`flush`), a complete buffered GitHub
Actions flavor, a CI error type with exit-code classification, and a
`clap::ValueEnum` impl. Only the GitLab flavor, provider auto-detect, the
destination axis, and the CLI wiring were new.

The deleted `ocx ci` command does **not** return — the capability comes back as
a flag on the existing env commands, exactly as the handshake prescribes.

## Decision Drivers

- **Handshake fidelity** — §6 pins value-flags (`--ci=<provider>`, explicit
  out-path), never positionals/subcommands, because the package tier already
  uses variadic positional ids.
- **Provider divergence** — GitHub (two-file inferred sink, current-job) and
  GitLab (JSON-lines, no path channel, later-step) cannot share one
  destination model; the provider must be explicit.
- **No dead surface** — only build what is wired now; reuse the `--shell`
  grammar and the existing `ci/` library rather than inventing a new path.
- **YAGNI on the destination axis** — only GitLab needs a file/stdout choice;
  GitHub infers its sink, so `--export-file` is GitLab-only.

## Industry Context & Research

**Research artifact:** N/A (behavior pinned by the handshake; provider formats
confirmed against live docs during implementation).
**Trending approaches:** GitHub Actions persists environment for later steps by
appending `KEY=VALUE` lines to the file named by `$GITHUB_ENV` and directories
to `$GITHUB_PATH` (workflow commands). GitLab CI/CD's step runner reads a
later-step export file; the handshake pins its line format as JSON-lines
`{"name","value"}`.
**Key insight:** the two providers differ on every axis that matters
(destination discovery, path handling, scope), so a single positional/subcommand
surface would force a lossy lowest-common-denominator. Explicit value-flags keep
each provider's contract intact.

## Considered Options

### Option 1: Resurrect an `ocx ci export` command

**Description:** Bring back a dedicated subcommand that resolves and exports.

| Pros | Cons |
|------|------|
| Self-contained surface | Handshake §7 deleted `ocx ci`; reintroducing it reopens a closed decision |
| Room for many sub-flags | Duplicates the entire resolve pipeline already in `env.rs` / `toolchain_env.rs` |
| | A new top-level command is a larger, harder-to-reverse surface than a flag |

### Option 2: `--ci[=PROVIDER]` flag on the existing env commands (chosen)

**Description:** Add `--ci[=PROVIDER]` (+ GitLab-only `--export-file`) as a new
emit branch beside `--shell` and the structured report, on both `ocx env`
(toolchain-tier) and `ocx package env` (OCI-tier).

| Pros | Cons |
|------|------|
| Exactly the handshake §6 grammar (value-flag, no positional/subcommand) | Two structs gain identical fields (already true for `--shell`) |
| Reuses the resolved `Vec<Entry>` and the whole resolution prologue | — |
| `conflicts_with`/`requires` give clap-level usage errors for free | — |

### Option 3: Positional provider argument (`ocx env github`)

**Description:** Take the provider as a positional.

| Pros | Cons |
|------|------|
| Short to type | `ocx package env` already has variadic positional ids — a provider positional is ambiguous |
| | Handshake explicitly forbids positionals/subcommands for this axis |

## Decision Outcome

**Chosen Option:** Option 2 — `--ci[=PROVIDER]` flag (with GitLab-only
`--export-file`).

**Rationale:** It is the surface the handshake reserved, it reuses the existing
resolution and the `ci/` library half, and clap's `conflicts_with`/`requires`
enforce the invariants without bespoke validation code.

### Decisions

1. **Surface.** `--ci[=PROVIDER]` on `ocx env` (root toolchain-tier) and
   `ocx package env` (OCI-tier). Not a resurrected `ocx ci` command.
2. **Providers.** `--ci=github` (alias `github-actions`), `--ci=gitlab` (alias
   `gitlab-ci`); bare `--ci` auto-detects from `$GITHUB_ACTIONS` / `$GITLAB_CI`
   (GitHub first), usage error (exit 64) if none detected.
3. **Mutual exclusion.** `--ci` ⟂ `--shell` (clap `conflicts_with`, exit 64).
   The CI channel is later-step; `--shell` is current-step. Only one applies.
4. **GitHub destination.** Inferred two-file sink from `$GITHUB_PATH`
   (path entries) and `$GITHUB_ENV` (everything else) — the existing
   `GitHubFlavor` behavior. `--export-file` is rejected for `--ci=github`
   (exit 64). An explicit `--ci=github` outside GitHub Actions fails
   `from_env()` (missing env files) with exit 78 (`ConfigError`).
5. **GitLab destination.** JSON-lines `{"name":"<key>","value":"<value>"}`, one
   per key, to **stdout by default** (redirect to the runner's `${{ export_file }}`)
   or to `--export-file=PATH` when given. GitLab has no path channel and
   later-step scope, so `PATH` and every path-type variable are flattened the
   way GitHub treats its non-`PATH` path case: accumulate package values, read
   the existing process value, prepend, join with `PATH_SEPARATOR`, emit one
   line.
6. **Output-file flag.** `--export-file` requires `--ci` (clap `requires`, exit
   64 without it) and is rejected for `--ci=github` (exit 64).
7. **Single serializer.** GitLab's wire shape lives in one function
   (`write_export_line`) behind one unit test, so a step-runner format
   divergence is a one-line fix.
8. **Key validation (env-injection hardening).** Both CI flavors validate the
   env-var key in `write_entry` through the shared
   `ocx_lib::env::is_valid_env_key` (the same validator the shell emitters use)
   and skip any entry whose key is not a POSIX identifier
   (`[A-Za-z_][A-Za-z0-9_]*`), emitting a `warn!`. This closes a newline in the
   GitHub `$GITHUB_ENV` key slot (second-variable injection, CWE-77) and the
   GitLab JSON-lines key charset. In addition, the GitHub `$GITHUB_PATH` branch
   rejects any value containing `\n`/`\r` (extra-PATH-dir injection,
   CWE-426/77); a real PATH directory never holds a newline. GitHub
   `$GITHUB_ENV` values are already heredoc-framed and GitLab values serde-
   framed, so neither needs an additional value guard. The shared path-prepend
   semantics (read existing process value, prepend buffered package values,
   join with `PATH_SEPARATOR`) live in one `ci::prepend_existing` helper used by
   both flavors' `flush_inner`.

### Consequences

**Positive:**
- The handshake §6 extension point is realized with zero grammar break.
- Both env commands gain CI export with one shared helper pair
  (`resolve_ci_arg`, `export_ci`) in `conventions.rs`.
- GitLab support (previously "not now") lands behind a single isolated flavor.

- CI export validates env-var keys through the shared `is_valid_env_key` and
  rejects newline-bearing `$GITHUB_PATH` values, closing the env-injection class
  (CWE-77 key injection, CWE-426/77 PATH-dir injection) with one validator
  shared with the shell emitters.

**Negative:**
- `toolchain_env.rs` and `env.rs` duplicate the two `#[arg]` fields — accepted,
  matching the existing `--shell` precedent (the structs are siblings, not a
  shared base).
- A malformed key/value is silently dropped (with a `warn!`) rather than
  failing the export. Accepted: a partial-but-safe export is preferable to
  aborting a CI step over one attacker-shaped entry.

**Risks:**
- *GitLab file format.* Live GitLab docs do not publish the export-file line
  format; the handshake pins JSON-lines `{"name","value"}`. Mitigation: the
  shape is isolated in `write_export_line` + one test, so a confirmed
  divergence is a one-line change.

## Technical Details

### Data flow (unchanged resolution, new emit branch)

```
entries: Vec<Entry>
  |- --ci=<provider>  -> export_ci(provider, export_file, &entries)   [NEW]
  |- --shell[=NAME]   -> emit_lines(shell, &entries)                  (existing)
  '- (default)        -> context.api().report(EnvVars)                (existing)
```

`--ci` and `--shell` resolve early (fast usage-error surfacing for bare-flag
autodetect failure); the emit branch is taken after `entries` is built.
`conflicts_with` guarantees at most one of `--ci`/`--shell` is set.

### API contract

```
ocx_lib::ci::CiFlavor::{GitHubActions, GitLab}
CiFlavor::detect() -> Option<CiFlavor>
CiFlavor::export(self, &[Entry], export_file: Option<PathBuf>) -> ocx_lib::Result<()>

ocx_cli conventions:
  resolve_ci_arg(Option<Option<CiFlavor>>) -> anyhow::Result<Option<CiFlavor>>
  export_ci(CiFlavor, Option<PathBuf>, &[Entry]) -> anyhow::Result<()>
```

## Implementation Plan

1. [x] `ci/error.rs` — add path-less `Write(io::Error)` variant (stdout case), classified `IoError`.
2. [x] `ci/gitlab_flavor.rs` — JSON-lines flavor + injectable writer + `detect()` + tests.
3. [x] `ci.rs` — `GitLab` variant, `detect()` append, `export(export_file)` param, `Display`, `ValueEnum`.
4. [x] `conventions.rs` — `resolve_ci_arg`, `export_ci` + unit tests.
5. [x] `toolchain_env.rs` / `env.rs` — `--ci` / `--export-file` fields + emit branch.
6. [x] Docs (command-line, environment, CI how-to), rules, handshake §6.
7. [x] Acceptance test `test/tests/test_ci_export.py`.

## Validation

- [x] `cargo test -p ocx_lib ci::` and `cargo test -p ocx conventions` pass.
- [x] CLI clap guards (`cli_help_text_is_ascii`, `cli_help_text_has_no_internal_references`) pass.
- [ ] `task verify` (final gate before commit).
- [ ] Acceptance: `cd test && uv run pytest tests/test_ci_export.py -v --no-build`.

## Links

- [`handshake_toolchain_cli.md`](./handshake_toolchain_cli.md) — §6 extension point (authority)
- [`adr_global_toolchain_tier.md`](./adr_global_toolchain_tier.md) — `--global` tier and the env exporter
- [`adr_cli_high_low_layering.md`](./adr_cli_high_low_layering.md) — toolchain-tier vs OCI-tier split

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-06-02 | Architect (planning agent) | Initial record — realize handshake §6 as `--ci` flag |
| 2026-06-02 | Builder (env-injection hardening) | Decision 8 — both flavors validate keys via shared `is_valid_env_key`, GitHub rejects newline `$GITHUB_PATH` values; shared `ci::prepend_existing` helper |
