# Documentation Drift Review — `ocx package copy` / `ocx package describe --from`

Reviewer: doc-reviewer (read-only). Branch `evelynn` vs `main` (0ed4a446).

## Scope

Source: `crates/ocx_cli/src/command/package_copy.rs`, `crates/ocx_cli/src/api/data/package_copy.rs`,
`crates/ocx_cli/src/command/package_describe.rs`, `crates/ocx_lib/src/oci/copy.rs`,
`crates/ocx_lib/src/publisher/copy.rs`.

Docs: `website/src/docs/reference/command-line.md`, `website/src/docs/user-guide/promoting-packages.md`
(new), `website/.vitepress/config.mts`, `.claude/rules/subsystem-cli-commands.md`.

## Trigger audit

| Source change | Doc file | Verdict |
|---|---|---|
| New command `package copy` (`command/package_copy.rs`) | `reference/command-line.md` | Present, mostly accurate — 2 gaps below |
| New flag `--from` on `package describe` | `reference/command-line.md` | Present, accurate |
| JSON output shape (`CopyReport`) | `reference/command-line.md` Output section | Incomplete — see Critical #1 |
| New user-facing workflow (promotion) | `website/src/docs/user-guide/promoting-packages.md` (new page) | Present, well-structured, correctly wired into sidebar |
| `.claude/rules/subsystem-cli-commands.md` entries | — | Accurate against code |
| `CHANGELOG.md` | — | Confirmed **not** touched by this diff (`git diff --name-status main...evelynn` has no `CHANGELOG.md` entry) — correct per repo policy |

## Flag-table audit (`package copy`)

Every flag declared on `PackageCopy` (`--to`, `-i/--identifier`, `-p/--platform`, `-c/--cascade`,
`--canonical-tag`/`--no-canonical-tag` via `options::CanonicalTag`, `--referrers`/`--no-referrers`,
`--description`, `--annotation`, `--dry-run`) has a matching row in `command-line.md`'s Options
table, with correct defaults (`--canonical-tag` default-on confirmed via
`options/canonical_tag.rs`'s own `default_is_enabled` test; `--referrers` default-on confirmed by
`execute()` computing `referrers: !self.no_referrers`). No flag in code absent from docs, no
documented flag absent from code.

`package describe --from`: `conflicts_with_all = ["readme", "logo", "title", "description",
"keywords"]` matches the doc's "Mutually exclusive with the field options above" and the
replace-not-merge semantic in `copy_from()`'s doc comment matches the prose in `command-line.md`.

## Exit-code table audit (`package copy`)

Stage 1 already flagged the `--platform` mismatch row (docs say 64, code raises `ClientError::
InvalidManifest` → 65 at `publisher/copy.rs:333-335`) — not re-reported here. Every other row was
traced to source and is correct:

| Doc row | Code path | Verdict |
|---|---|---|
| digest source, `--platform` absent/repeated → 64 | `package_copy.rs:100-107`, `UsageError` | Correct |
| digest source, `--identifier` absent → 64 | `package_copy.rs:108-114`, `UsageError` | Correct |
| image index named by digest → 64 | `publisher/copy.rs:314-321`, `UsageError` | Correct |
| `--to` + `--identifier` together → 64 | clap `conflicts_with = "identifier"` on `--to` | Correct |
| source tag/digest does not resolve → 79 | `ClientError::ManifestNotFound` → `ExitCode::NotFound` (`oci/client/error.rs:246`) | Correct |
| auth to either registry fails → 80 | `ClientError::Authentication` → `ExitCode::AuthError` (`error.rs:245`) | Correct |
| `--referrers` + no Referrers API → 84 | `ClientError::ReferrersUnsupported` → `ExitCode::ReferrersUnsupported` (`error.rs:261`) | Correct |

`CopyError`'s own `ClassifyExitCode` impl (`publisher/copy.rs:45-59`) correctly delegates through
`#[error(transparent)]` for both the `Usage` and `Other` arms, confirmed by the two contrasting unit
tests `a_structural_refusal_classifies_as_a_usage_error`.

## Critical gaps (user-visible behaviour undocumented)

### 1. Plain-text output is two tables, not one — the doc only describes one

`crates/ocx_cli/src/api/data/package_copy.rs:88-129` (`CopyReport::print_plain`) renders **two**
`print_table` calls: the per-platform breakdown (Platform / Digest / Result), *and* a second summary
table with columns `Target / Status / Tags / Canonical Tags / Referrers / Blobs`. This is what every
user sees by default (plain is the default format).

`reference/command-line.md:3668-3679` ("Output" section) describes only the first table, then says
"`--format json` adds `blobs` (`present`/`mounted`/`uploaded`) and `referrers_copied`" — wording that
implies those fields, and by extension the summary data, are JSON-only. In fact `blobs`,
`referrers_copied`, the cascade tags and the canonical-tag count are **all** visible in plain mode too,
via the second table (`Blobs` column = `present=N,mounted=N,uploaded=N`; `Tags` column = joined
cascade tags; `Canonical Tags` column = count; `Referrers` column = count).

Remediation: describe both tables in the Output section, or note explicitly that the summary row
(target/status/tags/canonical-tag-count/referrers-count/blob-breakdown) is printed in plain mode too,
and correct "`--format json` adds…" to name only what's genuinely JSON-only (nothing — the JSON shape
is a strict superset with per-tag/per-canonical-tag *values* instead of a joined string/count).

*(Note for the writer, not a doc gap per se: this print_plain shape is two tables, which is a
deviation from `subsystem-cli-api.md`'s "Single-Table Rule" — worth flagging to whoever owns the code
so the fix is scoped correctly, either collapsing to one table or updating that rule's exemption list.)*

### 2. `--dry-run` silently omits the cascade/canonical-tag plan

`crates/ocx_lib/src/publisher/copy.rs:222-260`: the entire "Phase 2 — tags" block (cascade-tag
resolution via `target_tags()` and the canonical-tag push) is gated by `if !request.dry_run { … }`.
Under `--dry-run`, `CopyOutcome.cascade_tags` and `.canonical_tags` are **always empty**, even when
`--cascade` and/or `--canonical-tag` (the default) are passed — the dry-run report never computes or
shows which rolling tags would move.

Both `reference/command-line.md:3665` ("`--dry-run`: Report what would be copied and write nothing")
and `user-guide/promoting-packages.md`'s "Checking before committing" section ("`--dry-run` reports
the same per-platform rows and writes nothing, so a release job can show the plan before it acts")
present dry-run as a full preview of "the plan." Neither mentions that a `--cascade` promotion's
tag-move plan is not part of that preview — a pipeline gating on `ocx package copy --cascade
--dry-run --format json` output to review which rolling tags would move will see empty arrays with no
indication this is a dry-run limitation rather than "nothing would move."

Remediation: add a line to both docs' dry-run sections noting that `--dry-run` previews only the
per-platform disposition; cascade/canonical tag moves are not computed or reported under `--dry-run`.

## Medium gaps (edge cases, internal changes)

### 1. `describe --from`'s new failure modes have no exit-code documentation

`crates/ocx_cli/src/command/package_describe.rs:149-168` (`copy_from`): when the source repository
has no description, the command returns a bare `anyhow::anyhow!("{source} has no description to
copy")` with no structured error type and no `ClassifyExitCode` source in its chain — this falls
through `classify_error`'s chain-walk to the generic `ExitCode::Failure` (1), not a specific code
like `NotFound` (79). The mutual-exclusion refusal (`--from` + a field flag) is a clap
`conflicts_with_all` → exit 64.

`reference/command-line.md#package-describe` has never had an exit-code table (confirmed via `git log
-p` — the section has been table-less since the command's introduction), so this is not a regression
introduced by this diff, but `--from` is a new, user-reachable failure surface worth naming now that
`describe` has a sibling (`copy`) whose exit-code table sets the expectation. Consider adding a
minimal exit-code table to `#package-describe`, or at minimum documenting that "no description to
copy" is a generic failure (exit 1), not `NotFound`.

## Accuracy issues (existing docs now incorrect)

### 1. "Uploads nothing" / "no-op" claim overstates re-run idempotency when referrers are involved

- `reference/command-line.md:3694-3696` (tip "Promotion is safe to re-run"): "A second identical copy
  reports every platform as `unchanged` and uploads nothing — the blobs are already there…"
- `user-guide/promoting-packages.md:78-80`: "Re-running a finished promotion is a no-op: every row
  reads `unchanged` and nothing is uploaded."

Per `crates/ocx_lib/src/oci/copy.rs`, `copy_leaf` runs unconditionally for **every** platform on
**every** invocation, regardless of the computed `Disposition` (`publisher/copy.rs:200-208`, comment:
"`Unchanged` still runs"). With `--referrers` (the default), `copy_leaf` calls `copy_referrers`
(`oci/copy.rs:326-398`), which — unlike `copy_blob`, which HEAD-checks the target and skips present
blobs — has **no existence check against the target** before calling `push_referrer_manifest`. `seen`
is a fresh `BTreeSet` per `copy_leaf` call, so it dedupes only within one run's recursion, not across
runs. The consequence: every repeated promotion of a **signed** package (or one carrying an SBOM/
attestation) re-fetches and re-PUTs every referrer manifest at the target, every time — the "uploads
nothing" claim is true only for platform-manifest blob *bytes*, not for referrer manifests, which are
genuinely re-transferred on every run. The manifest for the leaf itself is also re-fetched and re-PUT
each time (small, but also not "nothing").

Remediation: qualify both claims — "no blob content is re-uploaded" rather than "uploads nothing" —
and note that `--referrers` re-transfers the referrer set on every run (harmless/idempotent at the
target, but not free, and the reported `referrers_copied`/JSON count will be non-zero on every rerun
of a signed package, not just the first).

## Suggestions

- Consider whether `CopyReport::print_plain`'s second table (Critical #1) should instead be collapsed
  into one table per `subsystem-cli-api.md`'s Single-Table Rule, which would make the Output-section
  doc simpler to keep accurate going forward (fewer things to describe, one row-shape instead of two).
- The JSON shape described in the code's own doc comment (`api/data/package_copy.rs:9-20`,
  `CopyReport`) is more complete than what's in `command-line.md` — consider citing that struct doc
  comment as the source of truth when filling the gap from Critical #1, since it already enumerates
  every field correctly.

