# WP-G Fix Log — docs, `ocx package copy` / `describe --from`

Review-fix loop, branch `hex/pkgcopy-fix--docs` (based on `evelynn` @ `dfcdcb98`).
One row per numbered finding from the task message. Each closed entry cites the
old text, the new text, and the code line that justifies the new text.

## 1. [BLOCK — ACCURACY] "uploads nothing" / re-run no-op claim

**Status:** fixed

- `website/src/docs/reference/command-line.md:3694-3696` (tip "Promotion is safe
  to re-run") — old: "A second identical copy reports every platform as
  `unchanged` and uploads nothing — the blobs are already there and the index
  entry already points at the digest."
  New: states that a re-run re-verifies rather than trusting the index entry —
  every platform's leaf manifest and referrer set are re-fetched and re-PUT
  (idempotent, no new content, no tag movement), only blob *bodies* are skipped
  via the target HEAD.
  Justified by `crates/ocx_lib/src/publisher/copy.rs:197-201` (comment:
  "`Unchanged` still runs: the target's index entry proves the manifest is
  there, not that every blob it names is") and `:200-208` (only `dry_run`
  short-circuits; `copy_leaf` runs unconditionally otherwise), plus
  `crates/ocx_lib/src/oci/copy.rs:155-162` (`push_manifest_raw` called
  unconditionally inside `copy_leaf`) and `:326-398`/`copy_referrers` (no
  existence check against the target before `push_referrer_manifest`; `seen` is
  a fresh `BTreeSet` per `copy_leaf` call, so referrers are re-copied every run).
- `website/src/docs/user-guide/promoting-packages.md:78-80` — old: "Re-running a
  finished promotion is a no-op: every row reads `unchanged` and nothing is
  uploaded." New: same re-verifies framing, scoped to the user-guide's register
  (shorter, points at the reference page for the mechanism).

## 2. [CRITICAL GAP] Plain output is two tables

**Status:** fixed — direction corrected by the orchestrator (see below)

- **Correction 1 (orchestrator, after initial fix landed).** My first pass
  documented the two-`print_table` shape as-shipped. That inverted the
  finding: `review_r1_spec_package_copy.md` A6 flags the two-table
  `print_plain` as a `subsystem-cli-api.md` "Single-Table Rule" violation (one
  table max, 5-column budget) with no precedent elsewhere in the tree — the
  **code** moves, not the doc. WP-D is collapsing it to one per-platform table
  on stdout (the result) plus a summary status line on stderr, JSON shape
  unchanged. Rewrote the **Output** section to describe that shipped-after
  shape instead: one table, one stderr summary line, same `--format json`
  fields (`cascade_tags_written`, `canonical_tags_written`, `referrers_copied`,
  `blobs`). This is the version now in the doc.
- `website/src/docs/reference/command-line.md` **Output** section (was
  `:3665-3679`) — old text described only the per-platform table and said
  `--format json` "adds `blobs` ... and `referrers_copied`", implying those are
  JSON-only, and never mentioned the stderr summary at all. New: names the
  per-platform table as the stdout result, the summary as a stderr status
  line, and that `--format json` carries the summary as structured fields
  alongside the `platforms` array (not a JSON-only addition).
  Justified by `crates/ocx_cli/src/api/data/package_copy.rs:88-129`
  (`CopyReport::print_plain`, currently two `data.print_table` calls — the
  target shape per the orchestrator's correction) and the `CopyReport` doc
  comment at `:9-20`. I have not re-read `package_copy.rs` after this
  correction to confirm WP-D's stdout/stderr split landed exactly as
  described — see `## Unverified`.

## 3. [CRITICAL GAP] `--dry-run` omits the cascade/canonical-tag plan

**Status:** fixed

- `website/src/docs/reference/command-line.md` — `--dry-run` option line and
  the **Output** section gained a sentence: dry-run previews only the
  per-platform disposition; cascade and canonical-tag moves are not computed
  under `--dry-run`, so `cascade_tags_written`/`canonical_tags_written` are
  always empty in a dry-run report regardless of `--cascade`/`--canonical-tag`.
- `website/src/docs/user-guide/promoting-packages.md` "Checking before
  committing" section gained the same note.
  Justified by `crates/ocx_lib/src/publisher/copy.rs:222-260` (the entire
  "Phase 2 — tags" block, including the cascade-tag loop and the
  `push_canonical_tag` call, is gated by `if !request.dry_run`).

## 4. [MEDIUM GAP] `describe --from` exit-code documentation

**Status:** fixed

- `website/src/docs/reference/command-line.md` `#package-describe` — added an
  **Exit codes** table (none existed before — confirmed via `git log -p` per
  the review artifact). Rows: `--from` + a field flag → 64 (clap
  `conflicts_with_all`); source has no description, or its `__ocx.desc` tag
  does not resolve → 1 (`Failure`) — a bare `anyhow!` with no
  `ClassifyExitCode` source, so it falls through the chain-walk to the generic
  code rather than `NotFound` (79); `--offline` → 81 (`PolicyBlocked`);
  authentication failure → 80.
  Justified by `crates/ocx_cli/src/command/package_describe.rs:149-168`
  (`copy_from`: `.ok_or_else(|| anyhow::anyhow!("{source} has no description to
  copy"))`) and `crates/ocx_lib/src/oci/client.rs:1723-1726` (`pull_description`
  maps `ClientError::ManifestNotFound` on the description tag to `Ok(None)`, so
  "repository absent" and "repository present, no description" are
  indistinguishable at this call site — both collapse to the same generic exit).

## 5. [WARN] `package copy` exit-code table wrong

**Status:** fixed — direction corrected by the orchestrator (see below)

- **Correction 2 (orchestrator).** My first pass changed the row's exit code
  from 64 to 65, matching what `publisher/copy.rs:333-335`
  (`ClientError::InvalidManifest`, classified `DataError`/65 by
  `oci/client/error.rs:262-273`) does *today*. The orchestrator adopted the
  review's other reading instead: this is genuinely an invocation fault (the
  caller named a platform the source does not offer), so 64 is the correct
  contract and the **code** is what moves — WP-D is changing
  `publisher/copy.rs:333-335` to raise `UsageError` instead. Reverted the row
  to `64`.
- `website/src/docs/reference/command-line.md` exit-code table, row "No
  platform in the source matches `--platform`" — reads `64` (unchanged from
  before this WP's first pass; the intervening edit to `65` was reverted).
  Matches `review_r1_spec_package_copy.md` finding A7's adopted remediation.
  I have not re-read `publisher/copy.rs` after the correction to confirm
  WP-D's `UsageError` change landed — see `## Unverified`.

## 6. [WARN] Exit 81 undocumented

**Status:** fixed

- `website/src/docs/reference/command-line.md` exit-code table — added row:
  "`--offline` is set (package copy always needs network access)" → 81
  (`PolicyBlocked`).
  Justified by `crates/ocx_cli/src/command/package_copy.rs:121`
  (`context.remote_client()?`) → `crates/ocx_cli/src/app/context.rs:591-593`
  (`remote_client()` returns `Err(ocx_lib::Error::OfflineMode)` when the field
  is `None`) → `crates/ocx_cli/src/app/context.rs:228`
  (`let (remote_client, oci_index) = if options.offline { ... }` — `None` only
  under `--offline`, unaffected by `--frozen`) →
  `crates/ocx_lib/src/cli/classify.rs:324` (comment: "Plan taxonomy:
  ocx_lib::Error::OfflineMode → PolicyBlocked (81)").
  Deliberately did **not** name `--frozen` as a trigger: `context.rs:270-274`
  states "Frozen keeps the remote source so digest-pinned content still
  fetches; only unpinned-tag resolution is refused" and `remote_client` is
  gated on `options.offline` alone, so `--frozen` alone does not block `copy`
  — consistent with `[[feedback_frozen_scopes_to_package_tier]]` (`copy` is a
  low-level registry operation, not package-tier resolution).

## 7. `--description` invisibility / dry-run vocabulary

**Status:** no-change-needed (code findings, not mine to fix — verified doc
does not overclaim either)

- `--description` (ux review A6): checked `command-line.md`'s `--description`
  line and `promoting-packages.md`'s description section — neither claims the
  report shows whether the description travelled; both describe only the flag
  itself and point at `describe --from` for the standalone path. No doc text to
  correct. The gap (no `description` field in `CopyReport`, dry-run silently
  skips the description copy) is a `CopyReport`/`package_copy.rs` change —
  noted under `## Handoffs`.
- Dry-run vocabulary (ux review A11): checked the Output section — after the
  fix for finding 3, the doc now says dry-run rows carry the *same* disposition
  words (`added`/`replaced`/etc.) that a completed copy uses, which matches the
  code as shipped (`crates/ocx_cli/src/api/data/package_copy.rs:88-106` renders
  the same `disposition` string regardless of `status`). Doc is accurate to
  current behaviour; the UX complaint is about the behaviour itself, not the
  doc. Noted under `## Handoffs`.

## 8. ADR reconciliation

**Status:** fixed

- `.claude/artifacts/adr_package_copy.md` § "Per-platform disposition" — added
  an inline dated amendment paragraph after the disposition table (following
  the file's own 2026-08-19 amendment convention: paragraph, not table row),
  correcting "skip blobs, leaf and merge entirely" for the `unchanged` row to
  the re-verifies contract from finding 1.
  Justified by the same `publisher/copy.rs:197-208` / `oci/copy.rs:155-162,
  326-398` evidence as finding 1.
- § "Write order" — added an inline dated amendment paragraph after the code
  block, correcting the canonical-tag placement from phase 1 to phase 2.
  Justified by `crates/ocx_lib/src/publisher/copy.rs:250-258` (canonical-tag
  push sits inside the phase-2 tag loop, after the index merges) and
  `crates/ocx_lib/src/oci/client.rs:642-682` (`push_canonical_tag`'s doc
  comment: "`merged_manifest` is the just-returned merge result ... `platform`'s
  entry is expected to be present by construction" — it reads the *merged*
  index, which does not exist until phase 2 has run).
- ADR implementation item 6 (`error_envelope.rs` `collect_context`/
  `collect_detail` arms) — marked as being delivered by WP-B/WP-D in this fix
  loop, matching `review_r1_spec_package_copy.md` finding A5 (WP7 half-landed:
  `classify.rs` has the arm, `error_envelope.rs` does not).
- Bonus (found while fixing finding 6, same evidence): the ADR's own
  "Exit codes" section (§ "describe") read "81 `--offline` / `--frozen`
  refusal", which is the same overclaim finding 6 corrects for the CLI
  reference — `--frozen` alone does not gate `context.remote_client()`.
  Amended in place rather than left contradicting the finding-6 fix two
  sections above it in the same file.

## 9. Plan — convergence table, Status block, WP9 contract #7

**Status:** fixed

- Status block: already at `Last update: 2026-08-21`, `Step: awaiting
  /hex-execute (review-fix loop)` on read. Changed `Step` to
  `/hex-execute → review-fix-loop` — this fix loop *is* the execute step
  running now, not a state waiting for it to start. Schema fields unchanged
  (`.claude/rules/meta-ai-config.md` "Plan Status Protocol").
- Convergence check table: appended a process note to every WP9 row (unit
  contracts #3/#4/#5/#7/#8, acceptance #1/#6/#9) — "assigned to WP-E this
  fix-loop round (2026-08-21); verify at merge" — rather than marking them
  `delivered`. WP-E's commits are not visible from this worktree (separate
  branch), so I cannot verify what landed; the orchestrator's brief says WP-E
  is closing most of these, which is a process fact I can record, not a code
  claim I can verify. Rows for WP7 and WP10 left untouched — out of this fix
  loop's stated scope.
- WP9 unit-contract #7 (in `## Test Contracts` → `### Rust unit (stub
  transport)`, not just the convergence-table row): old text — "Phase
  ordering: a scripted failure on the first index merge leaves every leaf,
  referrer and canonical tag written and no rolling tag moved." (implies
  canonical tag is phase 1, same error as the ADR). New text — "Phase
  ordering: a scripted failure on the cascade merge must leave every leaf,
  referrer and the primary tag's canonical tag written, and no rolling tag
  moved" — the corrected form `review_r1_spec_package_copy.md` finding A2
  gives verbatim.
  Also corrected the "Technical Approach" section's write-order prose (phase 1
  bullet list included "canonical tags" — moved it to the phase 2 bullet) for
  the same reason: this file restates the ADR's now-amended write order and
  would otherwise contradict both the ADR and the Convergence check table two
  screens below it in the same file.

## 10. Small declared additions

**Status:** no-change-needed

- `.claude/rules/subsystem-cli-commands.md:23` — `package copy` is already in
  the "Low-level registry" tier row (`| **Low-level registry** | \`package
  pull\`, \`package push\`, \`package copy\`, \`package describe\`, ... |`),
  landed by an earlier commit in this branch. Nothing to add.
- `.claude/artifacts/handshake_toolchain_cli.md:8-11` — already carries an
  **Amendment 2026-08-19** line for `ocx package copy` joining the OCI tier,
  following the file's own two-prior-amendment convention (2026-05-17,
  2026-05-18). Nothing to add.

## Handoffs

For WP-D (code, `package_copy.rs` / `CopyReport`):

- `--description` is invisible in `CopyReport` on every path (ux review A6):
  no field records whether the description travelled, and under `--dry-run`
  the description copy is silently skipped
  (`self.description && !self.dry_run` at `package_copy.rs:142`) with no row
  and no note. `--format json` cannot tell a CI job whether the description
  moved.
- Dry-run rows read `added`/`replaced` — past-tense vocabulary for writes that
  did not happen (ux review A11). The doc now documents this as-is per finding
  7 above; a code fix (e.g. `would add`/`would replace` in `print_plain` under
  `Planned` status, per A11's remediation) will need a matching doc follow-up
  when it lands.

## Unverified

- Finding 5's row now reads `64` per the orchestrator's Correction 2, which
  depends on WP-D changing `publisher/copy.rs:333-335` to raise `UsageError`.
  As read at `dfcdcb98` the code still raised `ClientError::InvalidManifest`
  (→ 65) — I have not re-read the file after the correction to confirm the
  code-side change landed, and cannot from this worktree (sibling branch).
- Finding 2's rewritten **Output** section depends on WP-D collapsing
  `CopyReport::print_plain` to one stdout table plus a stderr summary line.
  As read at `dfcdcb98` the code still had two `print_table` calls — same
  caveat as above.
- The Convergence-check WP9 rows I annotated "assigned to WP-E this fix-loop
  round" are a process note from the task brief, not a verified code claim —
  WP-E's commits are on a sibling branch invisible from this worktree.

## Verification

`task website:build` did not run cleanly: it failed at the `recordings:parallel`
step with `client error (SendRequest): connection closed before message
completed` against `http://localhost:5000` — a registry-fixture connectivity
problem in this worktree, unrelated to any doc content (consistent with prior
project experience of stale/flaky local test-registry containers; see
`project_stale_registry_container.md` in memory). Fell back to the check named
in the task brief for that case: ran `bunx vitepress build` directly from
`website/` (schema generation, recordings and SBOM steps skipped — those don't
touch markdown correctness). Result: clean build, no dead-link or broken-anchor
warnings — `build complete in 5.11s`, only the pre-existing chunk-size
advisory. This covers the four files I edited (`command-line.md`,
`promoting-packages.md`, and the two `.claude/artifacts/*.md` files, which
VitePress does not build but which I hand-checked for anchor/reference
consistency against the file's own existing conventions).
