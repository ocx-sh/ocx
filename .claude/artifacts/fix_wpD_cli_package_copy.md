# WP-D — CLI surface fixes for `ocx package copy`

Review-fix work package D. File set: `crates/ocx_cli/src/command/package_copy.rs`,
`command/package_describe.rs`, `api/data/package_copy.rs`, `error_envelope.rs`, plus the
four specifically authorized files outside it.

Measured baseline before any edit: `cargo nextest run -p ocx_lib -p ocx` →
**5313 tests run: 5313 passed, 8 skipped**. (The brief said 5417; the package is named
`ocx`, not `ocx_cli`, and 5313 is what those two packages actually hold on
`fad2c94c`+WP-A+WP-B. Nothing regressed against the number that exists.)

## Findings

| # | Severity | Finding | Status |
|---|---|---|---|
| 1 | HIGH | `CopyReport::print_plain` makes two tables, second is six columns | fixed |
| 2 | HIGH | `pull_description` has no addressing seam; a mirror read decides a canonical write | fixed |
| 3 | HIGH | `status` / `disposition` are pre-formatted `String` on the wire | fixed |
| 4 | HIGH | `scratch_root: None` — every promoted layer spools to `$TMPDIR` | fixed |
| 5 | WARN | Registry-controlled platform text reaches stdout unsanitized | fixed |
| 6 | WARN | Target registry authenticated before the source-form refusals run | fixed (by deletion) |
| 7 | WARN | ux A3, A4, A5, A6, A10, A11 | all six fixed |
| 8 | WARN | `--format json` copy failures carry no `detail` slug | fixed |
| 9 | WARN | `subsystem-oci.md` invariant 5 states a default that WP-B inverted | fixed |
| 10 | WARN | `StubTransport::push_manifest_raw` records a push before it can fail | fixed; no stronger phase test, reason below |
| 11 | WARN | `describe --from` with an undescribed source exits 1 | fixed |

---

### 10 — `StubTransport::push_manifest_raw` recorded a push before it could fail

`crates/ocx_lib/src/oci/client/test_transport.rs:365-388`.

The double inserted the pushed bytes into `manifests` and *then* popped
`push_results`, so a queued `Err` produced a manifest that had "failed" and landed. No
test could model a failed manifest push. Now the queued outcome is consulted first and
the insert happens only on `Ok`.

**Whether it unlocks a stronger phase-ordering assertion: no, and the reason is in the
double, not the subject.** `push_results` is one FIFO shared by `push_manifest_raw`,
`push_manifest` and `push_blob` (`next_push_result`, `test_transport.rs:179`, called at
`:362` and `:415`). Targeting the index-merge push therefore means counting every blob
push that precedes it — and copy's blob pushes are a fan-out, so that count is not even
deterministic. Such a test would pin an arbitrary push ordinal and break on any
unrelated change without asserting anything new.

The phase boundary is already pinned behaviourally by
`a_promotion_that_dies_before_the_merge_leaves_every_tag_where_it_was`
(`publisher/copy.rs:1263`), which fails phase 2 through the cascade plan rather than
through the transport and asserts both halves: the leaf and referrer are at the target,
and the target's tag holds byte-identical old content. The fix here is still required —
it makes the double honest for whoever *does* need a failing push.

Proof: `5313 tests run: 5313 passed` after the change, `cargo check --workspace
--all-targets` clean.

### 9 — `subsystem-oci.md` invariant 5 stated a default WP-B inverted

`.claude/rules/subsystem-oci.md:544`.

Read "reads are mirror-aware by default (`transport_reference`)". False since WP-B for
`list_tags`, `fetch_manifest` and `fetch_manifest_raw_bytes` — and false for
`pull_description` as of finding 2 below. The invariant's conclusion is unchanged; only
its statement of the default was wrong, so only the two sentences carrying the default
moved: the plain short form is canonical, a mirror is named through `*_addressed`, and a
write-backing read must never name `ReadAddressing::Mirrored`.

### 2 — a mirror decided a canonical write (security F-2, invariant 5)

`crates/ocx_lib/src/oci/client.rs:1740`, `crates/ocx_lib/src/publisher.rs:248`,
`crates/ocx_cli/src/command/package_copy.rs:144`,
`crates/ocx_cli/src/command/package_describe.rs:82` and `:161`.

`Client::pull_description` built its reference from `transport_reference` directly — no
addressing parameter existed, so WP-B's inversion could not reach it and it stayed
mirror-first. Three call sites read a description and then push it: `copy --description`,
`describe --from`, and the merge in plain `describe`, which pulls the existing
description to preserve the fields the invocation omitted. All three read from a mirror
and wrote to the canonical host (CWE-345/367).

Fixed with the seam WP-B established for `list_tags`:

- `Client::pull_description(identifier, temp_dir)` — canonical, the short form.
- `Client::pull_description_addressed(identifier, temp_dir, addressing)` — `pub(crate)`,
  the host named.
- `Publisher::pull_description` — canonical, signature unchanged, so all three
  write-backing call sites are fixed without touching them.
- `Publisher::pull_description_mirrored` — new, for the one CLI caller that only renders.

Both mirror callers now ask by name and keep their behaviour: `announce/pipeline.rs:431`
(an observation, in-crate, so it names `ReadAddressing::Mirrored`) and
`package_info.rs:78` (`ocx package info` renders a catalog page and stops, so it takes
`pull_description_mirrored`). `ReadAddressing` stays `pub(crate)`; the CLI names the
mirror through a method name instead of the enum.

**Proof.** `pull_description_defaults_to_the_canonical_host`
(`oci/client.rs:6897`) calls the short form on a client with a configured mirror and
asserts the transport was handed `ghcr.io` / `owner/tool` unrewritten. Its positive
control is `pull_description_routes_through_mirror` on the same client and identifier,
recording the same `pull_manifest_raw` call — the only difference is that it names the
host. Red run: delegation flipped to `ReadAddressing::Mirrored` at `client.rs:1758` →
`1 test run: 0 passed, 1 failed`, `right: ("ghcr.io", "owner/tool")`. Restore verified by
reading `client.rs:1756-1760` back, then green: `3 tests run: 3 passed`.

### 11 — `describe --from` against an undescribed source exited 1

`crates/ocx_cli/src/command/package_describe.rs:163`.

`ok_or_else(|| anyhow::anyhow!("{source} has no description to copy"))` carries no
`ClassifyExitCode` type, so the chain walk found nothing and the process exited 1 — which
the pinned table reserves for "unclassified failure, classification fall-through only"
(EXIT-04). "The source has no description" is an expected outcome a CI job should be able
to branch on.

Now `no_description_to_copy(&source)` (`package_describe.rs:194`) carries
`ClientError::ManifestNotFound("{source}:__ocx.desc")` as the anyhow cause, with the
user-facing sentence as context. `ClientError::ManifestNotFound` classifies to
`ExitCode::NotFound` = 79 (`oci/client/error.rs:286`) and is already on the ladder
(`cli/classify.rs:168`), so no new type and no ladder entry — and the cause is literally
the error `pull_description` swallowed into `Ok(None)` on the way here
(`client.rs:1786`), not an invented one.

**"Absent" vs "present but undescribed": not separated, deliberately.**
`pull_description` maps `ManifestNotFound` on the `__ocx.desc` tag to `Ok(None)`, so the
two arrive identical. Telling them apart costs a second round trip (a tag listing) to
produce a distinction the user acts on identically — `--from` cannot proceed either way.
Both are 79, the message names the tag that was missing, and the reference doc says so
(`command-line.md:3723`).

**Proof.** `an_undescribed_source_exits_not_found` (`package_describe.rs:207`) asserts
`classify_error` → `ExitCode::NotFound`, that the value is 79, and that both the prose and
the tag name survive `{err:#}`. Its positive control is in the same test: a bare
`anyhow!` carrying the identical sentence classifies to `ExitCode::Failure`, so the 79
cannot be the walker returning 79 for everything. `1 test run: 1 passed`.

### 8 — `--format json` copy failures carried no `detail` slug (spec A5, ADR item 6)

`crates/ocx_cli/src/error_envelope.rs:243`, `:289`.

`collect_context` and `collect_detail` were hardcoded to `SignError`/`VerifyError`, so a
copy refusal serialized with `detail` absent and `context` empty — a CI job could match
only on prose. WP-B had already built both halves: `CopyErrorKind` implements
`ClassifyErrorKind` with a frozen snake_case `kind_detail()`, and `CopyError` carries both
identifiers, with the kind on the `source()` path.

Added `downcast_ref::<CopyErrorKind>()` to `collect_detail`, and a `CopyError` arm to
`collect_context`. **Two keys, not one**: `source` and `target`, because a copy failure is
about a pair of repositories and `identifier` alone cannot say which end refused. That is
an addition to the envelope's `context` map, which is an open `BTreeMap` — no key changes
meaning, so no schema bump. `crates/ocx_lib/src/publisher.rs:15` now re-exports
`CopyErrorKind` alongside `CopyError`; it was the only member of the pair not exported.

**Proof.** `a_copy_refusal_carries_its_slug_and_both_endpoints` (`error_envelope.rs:392`)
renders end to end through `render_error_envelope` — a hand-built `ErrorEnvelope` never
calls the collectors, which is where the defect was — and asserts
`"detail":"index_named_by_digest"`, both endpoint keys, and `"exit_code":64`. Two
controls: an unrelated `anyhow!` carrying the same identifiers renders with no `detail`
and `"context":{}`; and gating the new arm off (`if false &&`) fails the test
(`1 test run: 0 passed, 1 failed`). Restore verified by grep at `error_envelope.rs:289`,
then green.

### 6 — the target was authenticated before the source-form refusals ran (spec A4)

`crates/ocx_cli/src/command/package_copy.rs:121-124`.

The ADR claims every source-form violation exits 64 "with the target registry provably
never contacted". `publisher.ensure_auth(&target)` ran first, and the three refusals it
covers (`IndexNamedByDigest`, `PlatformRequired`, `PlatformAmbiguous`, plus
`NoMatchingPlatform`) are raised inside `Publisher::copy` — after a real token exchange.

**Resolved by deleting the call, not by moving it.** The brief said "move it to after
`resolve_source_leaves`"; that position already has an `ensure_auth` in it. `run()` calls
`resolve_source_leaves` before its first target contact (`publisher/copy.rs:279-280`), and
every write on the far side authenticates itself as its first action:
`copy_blob` (`oci/copy.rs:321`, before any byte moves), the leaf manifest PUT
(`oci/copy.rs:222`), and `merge_platform_into_index` (`oci/client.rs:530`, pinned by its
own `must call ensure_auth` test). The CLI call was a duplicate token exchange whose only
distinct effect was falsifying the ADR line.

Cost of the delete: a bad target credential is now reported after the source index and
leaf manifest are read, instead of before. No bytes are transferred either way —
`copy_blob` authenticates before it downloads.

**Test gap, handed to WP-E.** The library already pins "nothing may be written"
(`an_index_named_by_digest_is_refused`, `publisher/copy.rs:1047`), but the deleted call
was above the library boundary, so no unit test could ever have caught it. The assertion
that closes it is an acceptance one: a source-form refusal makes *no request at all* to
the target registry. `publisher/copy.rs` is outside my authorized edit scope
(`scratch_root` only), so I have not extended that test with an `auth_calls` assertion —
it would be the cheap second half if someone owns that file next.

### 4 — every promoted layer spooled to `$TMPDIR` (WP-A handoff H1)

`crates/ocx_lib/src/publisher/copy.rs:180` (new field), `:333` (threaded),
`crates/ocx_cli/src/command/package_copy.rs:127`.

WP-A capped the spool by bytes and left `scratch_root: None` at the one call site, with a
`// ponytail:` note naming this as the open half. On a memory-backed `$TMPDIR` the byte
cap bounds the file and not the medium, so the bounded-memory argument the cap exists to
serve did not hold.

`CopyRequest` now carries `scratch_root: Option<&'a Path>`, threaded straight into
`copy_leaf`. The CLI supplies `context.file_structure().temp.root()`, `create_dir_all`d
once — the same shape `patch_test.rs:166` uses — and the `--description` tempdir at
`package_copy.rs:149` moved from `tempfile::tempdir()` to `tempdir_in(&scratch_root)`
alongside it. `None` survives for callers with no store: the unit tests, and a library
consumer with no OCX home. The stale `// ponytail:` note in `oci/copy.rs:169` is replaced
by what is now true (one comment, no logic — the only edit in that file).

**Proof.** `a_supplied_scratch_root_is_the_one_each_leaf_spools_into`
(`publisher/copy.rs:781`) asserts through absence: a spool directory is created and
dropped inside `copy_leaf` and is never observable from outside, but a root that does not
exist makes `tempfile::tempdir_in` fail and the error carries the path back. Its positive
control is the same copy with `scratch_root: None`, which succeeds. Red run: threading
reverted to a literal `None` at `copy.rs:333` → `1 test run: 0 passed, 1 failed`. Restore
verified by grep at `copy.rs:333`, then green.

### 1, 3, 5, and ux A5/A6/A11 — the report

All six land in `crates/ocx_cli/src/api/data/package_copy.rs` and its one caller, so they
are one change; each is listed separately below with what it did.

**1 — two tables became one (spec A6).** `print_plain` made a second, six-column
`print_table` call; `subsystem-cli-api.md`'s Single-Table Rule allows one table and a
five-column budget, and a sweep found no other report in the tree with two. The
per-platform table stays on stdout — it is the result — and the receipt moved to
`context.ui().status(...)` on stderr (`package_copy.rs:186`), per the Channel Rules'
"receipts and steps-along-the-way are diagnostics". `UserInterface::status` documents that
OCX emits one status line per command, so the receipt is one line, not six.
**The JSON shape did not change** except by addition (see A6).

**3 — `status` and `disposition` are typed (ux A1, CLI half).** The row holds
`Disposition` itself, and `status` is a new `CopyStatus { Copied, Planned }`. Both derive
`Serialize` with `rename_all = "kebab-case"`; both keep a `Display` for the prose.
Exemplars followed: `api/data/pull_dry_run.rs` (`PullStatus`) and `api/data/path_kind.rs`.

**The exact serialized strings — WP-E's assertions, now confirmed:**

| Field | Values on the wire |
|---|---|
| `status` | `copied`, `planned` |
| `platforms[].disposition` | `added`, `unchanged`, `replaced`, `kept-not-in-source` |
| `description` | `copied`, `absent`, `skipped-dry-run`, or `null` |

Pinned by `the_serialized_vocabulary_is_the_one_scripts_match_on`
(`api/data/package_copy.rs:299`), which asserts each one against `serde_json::to_string`.

**5 — registry text is neutralized (security F-7, CWE-150).** Platform strings come off
the source index and cascade tags off the target's own tag list. Both, plus the digest and
the target in the status line, now route through the existing
`crate::api::data::sanitize_for_terminal` — the one sanitizer, per SEC-34 and IDIOM-11;
no second one was written. `print_plain` was split so the columns come from a
`plain_rows()` seam, the same shape `api/data/index.rs` uses to make this assertable.

**A11 — dry-run rows say `would`.** `result_cell` renders `would add` / `would replace`
under `Planned`. `unchanged` and `kept (not in source)` describe the target as it already
is and read correctly either way, so they do not move. **Plain output only** — the JSON
slug is untouched, which is the whole reason the row is typed.

**A6 — `--description` is reported.** New `description: Option<DescriptionOutcome>`.
Under `--dry-run` the copy is still skipped, but it now says so (`skipped-dry-run`)
instead of dropping the flag on the floor; a source with no description is `absent`
rather than a stderr `warn` no JSON consumer can see. `None` (flag not passed) serializes
as `null`, so the key is always present to branch on.

**A5 — the `Digest` column means two things.** No schema change; the information was
already in `disposition` and simply was not said. One sentence added to the `CopyReport`
doc comment and one to the reference Output section (`command-line.md:3676`).

**Proof.** Four tests in `api/data/package_copy.rs`, each with a control in the same test:

- `the_serialized_vocabulary_is_the_one_scripts_match_on` — every wire value above.
- `a_dry_run_says_would_in_prose_and_keeps_the_slug_in_json` — plain reads
  `["would add", "would replace", "unchanged", "kept (not in source)"]` while the JSON
  still carries `"disposition":"added"` and `"replaced"`. Control: the identical rows
  under `Copied` render `["added", "replaced"]`, so the rewrite is conditional on the
  status and not on the disposition.
- `registry_text_is_neutralized_before_it_reaches_the_terminal` — a platform, a cascade
  tag and a target carrying ESC/`\n`/NUL/U+202E leave no control or bidi character in any
  cell or in the status line. Control: `linux/amd64` and `1.4` pass through verbatim, so
  the assertion cannot be satisfied by emptying the output. Red run: the platform column's
  `sanitize_for_terminal` replaced by `row.platform.clone()` →
  `1 test run: 0 passed, 1 failed`; restore verified by reading
  `package_copy.rs:237-250` back, then green.
- `the_summary_carries_the_receipt_the_second_table_used_to` — every value the deleted
  six-column table carried is in the status line, and `action()` is `Copied` / `Planned`.

`4 tests run: 4 passed`.

### 7 (partial) — ux A10: the log line asserted an action a dry run will not take

`crates/ocx_cli/src/command/package_copy.rs:132`. `--dry-run -l info` logged
`copying …`. Now `planning a copy of {source} to {target}` under `--dry-run`.

### 7 (rest) — ux A3 and A4

**A3 — the referrers toggle now follows the paired-toggle convention.**
`--referrers`/`--no-referrers` were two raw inline `bool` fields resolved as
`referrers: !self.no_referrers` at the call site — the exact shape
`subsystem-cli.md` "Paired Boolean Toggles" records as a standing owner request not to
write, nine lines below a correct `options::CanonicalTag` in the same struct. Side effect
of the old shape: the `referrers: bool` field was never read anywhere.

New `crates/ocx_cli/src/options/referrers.rs`, modelled on `options/canonical_tag.rs`:
bistate, default on, `enabled()`, `overrides_with` both ways, and the same four last-wins
unit tests (`default_is_enabled`, `no_referrers_disables`, `explicit_referrers_enables`,
`last_wins`). `package_copy.rs:60` flattens it and calls `self.referrers.enabled()`.

**A4 — `copy --help` no longer prints `push`'s vocabulary.** clap renders the *flattened
struct's* field docs, so the carefully written "each **copied** platform manifest" on the
`canonical_tag` field reached nobody while `options/canonical_tag.rs`'s "each **pushed**
platform manifest" reached every `copy` user. Neutralised the wording in the option struct
so it reads correctly for both verbs ("each platform manifest published"), deleted the
orphaned comment, and left a comment in its place saying why nothing may be written there.

Verified by running the binary, not by reading the source:
`cargo run --bin ocx -- package copy --help` now prints
`Write a `sha256.<hex>` tag for each platform manifest published (default)` and
`Carry the signatures, SBOMs and attestations anchored to each manifest (default).`
The two clap gates still pass — `cli_help_text_is_ascii` and
`cli_help_text_has_no_internal_references` — alongside all eight toggle tests:
`10 tests run: 10 passed`.

### Acceptance suite: one stale assertion WP-E's kebab-case pass missed

`test/tests/test_package_copy.py:206` still read
`assert dispositions[other] == "kept (not in source)"` — the prose form. It is the only
multi-word disposition in the suite, so the other kebab-case values (`added`, `replaced`,
`unchanged`) are spelled identically either way and the pass looked complete. Against
finding 3's typed `Disposition` that assertion is red, so I corrected it to
`kept-not-in-source` rather than hand over a known-failing suite.

Added two assertions to `test_the_description_travels_only_when_asked` (`:449`, `:459`)
while I was there, because that test already exercises `--description` and nothing else
pins finding A6's new field: `description is None` on the plain copy, `== "copied"` when
the flag is passed.

These are the only edits in WP-E's file. Not run here — the acceptance suite needs the
test registry container.

---

## Verification

| Gate | Result |
|---|---|
| `cargo check --workspace --all-targets` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo nextest run -p ocx_lib -p ocx` | 5325 passed, 8 skipped (baseline 5313 — 12 new, 0 regressed) |
| `cargo nextest run --workspace` | 5429 passed, 8 skipped |
| `cargo fmt` | run before every commit |

Twelve tests added, every one demonstrated red before it was demonstrated green, or
carrying a positive control in the same test body — for four of them, both:

| Test | Red run |
|---|---|
| `pull_description_defaults_to_the_canonical_host` | delegation flipped to `Mirrored` → 1 failed |
| `a_supplied_scratch_root_is_the_one_each_leaf_spools_into` | threading reverted to a literal `None` → 1 failed |
| `a_copy_refusal_carries_its_slug_and_both_endpoints` | new `collect_detail` arm gated off → 1 failed |
| `registry_text_is_neutralized_before_it_reaches_the_terminal` | one `sanitize_for_terminal` call removed → 1 failed |
| `an_undescribed_source_exits_not_found` | control in-test: the bare `anyhow!` it replaced classifies to 1 |
| `a_dry_run_says_would_in_prose_and_keeps_the_slug_in_json` | control in-test: the same rows under `Copied` render unchanged |

Every restore was verified by reading the file back, not by assuming the edit reverted.

## Self-review against the Always-Apply anchors

- No `.unwrap()`/`.expect()` added outside `#[cfg(test)]`.
- No blocking I/O in an async path: the one new filesystem call is
  `tokio::fs::create_dir_all` (`package_copy.rs:137`).
- No `MutexGuard` across an `.await`; no new lock taken.
- No `ReferenceManager` / symlink work in this package.
- Nothing auto-committed; nothing pushed. Every commit `-c commit.gpgsign=false`, none
  `--no-verify`.
- `CHANGELOG.md` untouched.

## Handoffs

1. **WP-E — the acceptance suite is now confirmed, and one assertion had to move.**
   `kept-not-in-source` is the wire value (`test_package_copy.py:206` corrected here); the
   full frozen vocabulary is the table under finding 3. Two `description` assertions added
   at `:449` and `:459`. Not run — the suite needs the test registry container.
2. **Whoever owns `publisher/copy.rs` next — one cheap assertion is still missing.**
   Finding 6 deleted a CLI-level `ensure_auth` that no unit test could reach. The library
   half, "a source-form refusal makes no request at all to the target registry", is one
   `auth_calls` assertion on `an_index_named_by_digest_is_refused` (`copy.rs:1047`, which
   today asserts only that nothing was *written*). Out of my authorized scope for that file.
3. **`ReadAddressing` stays `pub(crate)`.** `package_info` reaches a mirror through
   `Publisher::pull_description_mirrored` rather than by naming the enum. A third CLI
   caller wanting a mirror gets another named method, not a wider enum — that is what
   keeps "asked for by name" checkable from the CLI crate.
4. **Docs touched outside WP-G's copy sections**, both because the reference would
   otherwise be wrong about behaviour introduced here: the `copy` **Output** section
   (`command-line.md:3676`) gained the `Digest`-column sentence, the dry-run vocabulary,
   and the `description` field; the `describe --from` bullet (`:3723`) gained the exit-79
   sentence. The `copy` exit-code table is unchanged — no exit code moved.
5. **ux A7, A8, A9 were not in this work package** and are untouched: exit 81 undocumented
   (A7), the "pass `--identifier`" message told to someone who passed it (A8), and
   `parse_annotation` reached across a sibling leaf module (A9).
