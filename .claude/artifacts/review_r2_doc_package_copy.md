# R2 Doc Drift Review — `ocx package copy` / `ocx package describe --from`

Scope: `git diff dfcdcb98..HEAD`. Verifying each claim against shipped code.


## Claim-by-claim verdicts

### 1. Re-run "not a no-op on the wire" claim

**Verdict: accurate**, with one incompleteness noted under Suggestions.

- Doc: `website/src/docs/reference/command-line.md:3703-3705` (tip) and `website/src/docs/user-guide/promoting-packages.md:80-85`.
- Code: `crates/ocx_lib/src/publisher/copy.rs:322-346` — the `for (platform, source_digest) in &source_leaves` loop calls `oci::copy::copy_leaf` unconditionally for every row, including `Disposition::Unchanged` (comment at `copy.rs:322-332` states the re-verify contract explicitly).
- `crates/ocx_lib/src/oci/copy.rs:184-228` — `copy_leaf` always re-fetches the leaf manifest (`fetch_manifest_raw_bytes_addressed`) and always re-PUTs it (`push_manifest_raw`); confirms "the leaf manifest is re-fetched and re-PUT."
- `crates/ocx_lib/src/oci/copy.rs:242-244, 437-521` — `copy_referrers` builds a fresh `BTreeSet` per `copy_leaf` invocation and has no existence check against the target before `push_referrer_manifest` (`copy.rs:511-513`); every referrer is re-transferred on every run when `--referrers` is set. Confirms "its referrer set is re-copied too."
- `crates/ocx_lib/src/oci/copy.rs:315-326` — `copy_blob` HEADs the target first and returns `BlobOutcome::Present` without re-uploading; confirms "only blob bodies are skipped, via a HEAD."

All four wire-level claims in the rewritten tip check out against the code that runs today. The r1 finding (this tip previously overstated a full no-op) is fixed, not merely reworded differently.

### 2. Plain output: one table + one stderr status line

**Verdict: accurate.**

- Doc: `command-line.md:3670` ("this is the result, on stdout") and `command-line.md:3685` ("go to stderr as one status line, leaving stdout to the table").
- Code: `crates/ocx_cli/src/api/data/package_copy.rs:255-264` — `CopyReport::print_plain` calls `data.print_table` exactly once (Platform/Digest/Result columns).
- `crates/ocx_cli/src/command/package_copy.rs:176-181` — `context.ui().status(report.action(), report.summary())` (stderr) followed by `context.api().report(&report)` (stdout, the one table). The r1 second-table finding is resolved: the code was collapsed to one table + a stderr receipt, and the doc now says exactly that.

### 3. `--dry-run` never computes cascade/canonical tags

**Verdict: accurate.**

- Doc: `command-line.md:3665, 3687` and `promoting-packages.md:153-158`.
- Code: `crates/ocx_lib/src/publisher/copy.rs:360` — `if !request.dry_run { … Phase 2 … }` gates the entire tag-merge block; `cascade_tags`/`canonical_tags` (`copy.rs:304-305`) are never touched when `dry_run` is true, so they serialize as empty arrays regardless of `--cascade`/`--canonical-tag`. Confirmed by the unit test `a_dry_run_writes_nothing` (`copy.rs:1092-1112`), which clears `data.write().calls` and asserts no `push_` call fired.

### 4. `package copy` exit-code table

**Verdict: accurate**, including the row this round changed.

Traced every row in `command-line.md:3691-3701` against source:

| Doc row | Code | Verdict |
|---|---|---|
| digest + `--platform` absent/repeated → 64 | `package_copy.rs:92-99`, `UsageError` → `ExitCode::UsageError` (`cli/error.rs:80-83`) | accurate |
| digest + `--identifier` absent → 64 | `package_copy.rs:100-106`, `UsageError` | accurate |
| image index by digest → 64 | `publisher/copy.rs:78-120` `CopyErrorKind::IndexNamedByDigest` → `UsageError` (`copy.rs:122-138`) | accurate |
| `--to` + `--identifier` → 64 | clap `conflicts_with` (unchanged by this diff) | accurate |
| no matching platform → 64 | `CopyErrorKind::NoMatchingPlatform` → `UsageError` (`copy.rs:126-131`) — this is the row the team-lead flagged as newly changed; the code and doc agree | accurate |
| source does not resolve → 79 | `ClientError::ManifestNotFound` → `ExitCode::NotFound` (`oci/client/error.rs:286`) | accurate |
| auth fails → 80 | `ClientError::Authentication` → `ExitCode::AuthError` (`oci/client/error.rs:285`) | accurate |
| no Referrers API → 84 | `ClientError::ReferrersUnsupported` → `ExitCode::ReferrersUnsupported` (`oci/client/error.rs:301`) | accurate |
| `--offline` set → 81 (new row) | `crates/ocx_cli/src/app/context.rs:591-593` `remote_client()` → `Err(ocx_lib::Error::OfflineMode)`; `crates/ocx_lib/src/error.rs:317` maps `OfflineMode` → `ExitCode::PolicyBlocked` (81) | accurate |

### 5. `describe --from` exit-code table

**Verdict: DRIFTED — Block.** See Critical Gaps below.

### 6. `description` JSON field values

**Verdict: accurate.**

- Doc: `command-line.md:3685` — `copied`, `absent`, `skipped-dry-run`, `null`.
- Code: `crates/ocx_cli/src/api/data/package_copy.rs:52-62` — `#[serde(rename_all = "kebab-case")]` on `DescriptionOutcome` yields exactly those three strings; confirmed byte-for-byte by the test `the_serialized_vocabulary_is_the_one_scripts_match_on` (`package_copy.rs:319-330`). `Option<DescriptionOutcome>` (`package_copy.rs:117`) serializes `None` as `null` when `--description` was never passed (`command/package_copy.rs:161-162`).

### 7. `Disposition` plain prose vs JSON kebab-case

**Verdict: accurate, no conflation.**

- Doc: `command-line.md:3672-3677` states the plain-mode prose table (`added`/`unchanged`/`replaced`/`kept (not in source)`) and never claims this is also the JSON spelling; `command-line.md:3683` discusses only the `added`/`replaced` dry-run rewrite (`would add`/`would replace` in plain vs unchanged `disposition` slug in JSON), which is the one place plain and JSON diverge for those two values.
- Code: `crates/ocx_lib/src/publisher/copy.rs:206-231` — `Disposition` derives `Serialize` with `#[serde(rename_all = "kebab-case")]` (→ `kept-not-in-source`) and a separate hand-written `Display` (→ `"kept (not in source)"`); confirmed by the dual-form test `a_disposition_serializes_as_a_token_and_displays_as_prose` (`copy.rs:1505-1526`). The doc's Result table only ever shows the `Display` prose, matching what `print_plain` actually renders (`api/data/package_copy.rs:224-230, 237-252`), and never states the JSON string form anywhere — so there is nothing to conflate.

## Critical Gaps (user-visible behavior undocumented / documented wrong)

- [ ] `website/src/docs/reference/command-line.md:3743` and `:3748` → `#package-describe` exit-code table — the row "`--from <SOURCE>` names a repository with no published description … | 1 |" and the following paragraph both state exit code **1**. The shipped code classifies this to **79** (`NotFound`): `crates/ocx_cli/src/command/package_describe.rs:187-198` (`no_description_to_copy`) wraps the failure in `ClientError::ManifestNotFound`, which `crates/ocx_lib/src/oci/client/error.rs:286` maps to `ExitCode::NotFound`. The code's own unit test proves it: `crates/ocx_cli/src/command/package_describe.rs:211-238` (`an_undescribed_source_exits_not_found`) asserts `classify_error(...) == ExitCode::NotFound` (`as u8 == 79`), with an explicit "positive control" showing the *old* bare-`anyhow!` shape (what the doc still describes) is the one that classifies to 1. This row and its explanatory paragraph were both added in this diff (`git diff dfcdcb98..HEAD` shows them as new lines) — the fix to the code (79) and the fix to the docs did not land in sync. Remediation: change the table row to `79`, and rewrite the trailing paragraph to say only "nothing to update" (the no-flags-at-all case) falls through to the generic 1; "no description to copy" now has its own classification via `ClientError::ManifestNotFound` and exits 79.

## Medium Gaps

None found beyond the r1 carryover (already resolved this round).

## Accuracy Issues (existing docs now incorrect)

None beyond the Critical item above — every other claim re-verified this round (see per-claim verdicts) is accurate against the shipped code.

## Suggestions

- [ ] `command-line.md:3703-3705` / `promoting-packages.md:80-85` — the re-run tip enumerates "the cost" of a retry as "a HEAD per blob and a manifest re-PUT per platform," but Phase 2 (`publisher/copy.rs:360-406`) also unconditionally re-merges and re-`PUT`s the index for the primary tag and every cascade tag on *every* run, including a pure re-run where every platform is `Unchanged` — the loop over `copied_leaves` and the `merge_platform_into_index` call (`crates/ocx_lib/src/oci/client.rs:518-617`) carry no disposition gate and no before/after byte comparison, so a write always goes out over the wire even when the result is byte-identical. The tip's claim that nothing "moves" is still true (the digest the tag resolves to is unchanged), but the wire-cost enumeration is incomplete: a re-run also costs one index `PUT` per tag per platform, not just the blob HEAD and the leaf re-PUT. Consider adding a clause noting the index-merge round trip, or softening "the cost is X, not Y" to avoid implying an exhaustive list.
