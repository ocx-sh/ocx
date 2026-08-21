# Review R1 — UX / product consistency: `ocx package copy`

- **Focus:** user-feedback (UX and product consistency of user-facing behaviour)
- **Scope:** `main...evelynn` (baseline `0ed4a446`), restricted to the changed-file list
- **Verdict:** needs work — 11 actionable, 2 deferred. Nothing here is a correctness or
  security defect; the command does what it says. The cluster that matters is the machine
  contract (`--format json`), which ships human prose as field values and is already pinned
  by an acceptance assertion.

Evidence was produced by reading the source and by running `./target/release/ocx package
copy --help` / `-h` against the branch build. Stage 1's two findings (two-table
`print_plain`; the `--platform`-miss exit code) are not re-reported; finding **A2** extends
the second with its message-text half and a single remediation that fixes both.

---

## Verdicts on the questions asked

**1. Flag grammar consistency — mostly clean, two breaks.** `-c/--cascade`,
`-i/--identifier`, `-p/--platform` and `--annotation <KEY=VALUE>` all match
`package_push.rs` exactly, the annotation parser is reused rather than re-written, and the
positional `source` is declared last, after every flag (the project convention). Two
breaks: the `--referrers`/`--no-referrers` pair is not a flattened `options::` struct
(**A3**), and the shared annotation parser lives in a sibling leaf rather than a `_common`
module (**A9**). `-p` being `Vec<Platform>` here where push/sign take a single value is
*not* a break — it is a filter over a set, the short letter carries the same meaning, and
nothing else in the family claims repeatable `-p`.

**2. The `--platform` double meaning — acceptable, not a trap.** Both halves are stated
plainly in the long `--help` (verified). `-h` collapses to `Platform to copy. Repeatable`,
which reveals neither. That is fine, because the digest branch is *enforced* rather than
guessed: `package_copy.rs:100-115` raises two usage errors before a single request goes
out, each naming the flag the user must add. A doubled meaning is a trap when you can fall
into it silently; here you cannot. No finding.

**3. Error-message actionability — three fall short.** The two digest-source errors and the
index-by-digest refusal all name the next action and read well. The `--platform`-miss
message blames the artifact for the user's typo and withholds the one fact that fixes it
(**A2**). The no-tag-target message tells the user to pass a flag they just passed
(**A8**). Offline is undocumented and surfaces as a generic network failure (**A7**). The
referrers-unsupported message (`registry {registry} does not support the OCI Referrers
API`, exit 84) names the problem but not the remedy (`--no-referrers`) — the variant lives
in `crates/ocx_lib/src/oci/client/error.rs`, which is **not** in this diff, so it is
recorded here as context, not raised as a finding.

**4. The disposition vocabulary — right words, wrong place.** `added` / `unchanged` /
`replaced` are self-explanatory. `kept (not in source)` carries its own explanation in the
parenthetical and both doc pages spell out the consequence; as plain-mode prose it is fine.
The defects are that the same prose *is* the JSON value (**A1**) and that the `Digest`
column silently means something different on that row (**A5**).

**5. `--dry-run` — a plan that reads like a receipt.** See **A11** (row vocabulary),
**A6** (`--description` silently dropped), **A10** (the log line), **D1** (push auth).

**6. `describe --from` — consistent.** `conflicts_with_all` against the five field flags
mirrors `copy`'s `--to`/`--identifier` exclusion, the positional stays last, and
replace-not-merge is stated in the help text (`package_describe.rs:19-24`), the reference
(`command-line.md:3723`) and the user guide (`promoting-packages.md:106`) — all three
places a user would look. The only note is **D2**.

**7. Docs as UX — passes.** `promoting-packages.md` opens with the concrete three-registry
pain scenario and the rebuild-produces-different-bytes consequence (lines 7-18) *before*
naming the command (line 20) — the `docs-style.md` idea → problem → solution shape. The
dev → staging → prod walkthrough is followable cold. Link discipline is clean: zero inline
`[text](url)` in the body (verified by grep), all definitions grouped at the bottom under
`<!-- commands -->` / `<!-- in-depth -->` / `<!-- external -->` comments. The referenced
cast and the nav entry both exist. No finding.

**8. `--format json` purity (CLI-02) — holds.** The only two non-report writes on the copy
path are `log::info!` (`package_copy.rs:126`) and `context.ui().warn` (`:146`), both stderr
by the `DataInterface`/`UserInterface` split. `context.api().report(...)` is the single
stdout write. No banner, no progress, no trailing line. No finding.

---

## Actionable

### A1 [High] Human prose shipped as JSON field values

`crates/ocx_cli/src/api/data/package_copy.rs:27,46,66,73`

`status` and `disposition` are `String`, built by calling `Display` at construction
(`:66`, `:73`). A `--format json` consumer therefore has to match the literal
`"kept (not in source)"` — a value with a space and two parentheses — and
`test/tests/test_package_copy.py:206` already pins exactly that, so the prose is now a
tested wire contract.

This is what `subsystem-cli-api.md` "Typed Enums Over Strings" forbids, and the same file's
Plain-Mode Column Budget names the cause verbatim: "The recurring cause is pre-formatting
into `String` at the call site … the report struct holds the **typed** value so `Serialize`
emits the full pinned form while `print_plain` renders the short one."

Two in-tree exemplars already solve it: `api/data/path_kind.rs` (`PathKind`,
`#[serde(rename_all = "lowercase")]` beside a hand-written `Display`) and
`api/data/pull_dry_run.rs` (`PullStatus`, `rename_all = "kebab-case"`, `WouldFetch` →
`would-fetch`).

**Remediation.** Hold `publisher::copy::Disposition` typed on `CopiedPlatformRow`; add
`#[derive(Serialize)] #[serde(rename_all = "kebab-case")]` to it so JSON emits
`kept-not-in-source`, leaving the existing `Display` (`publisher/copy.rs:113-123`) as the
plain rendering. Do the same for `status` with a `CopyStatus { Copied, Planned }` enum.
Update `test/tests/test_package_copy.py:206` in the same commit. Landing this before the
format ships is the cheap moment.

### A2 [High] A user's `--platform` typo is reported as a broken manifest

`crates/ocx_lib/src/publisher/copy.rs:332-336`

A filter that matches nothing raises `ClientError::InvalidManifest`, whose `Display` is
`invalid manifest: {0}` (`oci/client/error.rs:41-42`). The user sees:

```
invalid manifest: staging.example.com/acme/mytool:1.4.2 offers no platform matching the request
```

Two user-visible defects. The manifest is valid — the request was wrong, and the sentence
sends the reader to look at the artifact. And it withholds the one fact that resolves the
situation: which platforms the source *does* offer.

Same root cause as Stage 1's exit-code finding (doc says 64, code gives 65), and one change
fixes both.

**Remediation.** Collect the platform set while walking `index.manifests` (before the
`requested.contains` filter at `:327`) and raise a `UsageError` — which classifies to 64,
matching the documented table:

```rust
UsageError::new(format!(
    "{source} offers no platform matching the request; available: {}",
    available.join(", ")
))
```

### A3 [Warn] The referrers toggle bypasses the standing paired-toggle convention

`crates/ocx_cli/src/command/package_copy.rs:61-66`, resolved at `:134`

`--referrers`/`--no-referrers` is two raw inline `bool` fields, resolved at the call site as
`referrers: !self.no_referrers`. `subsystem-cli.md` "Cross-Cutting: Paired Boolean Toggles
(`--X`/`--no-X`)" records this as a standing owner request: flatten a dedicated struct from
`crates/ocx_cli/src/options/<name>.rs`, resolve through a method on it, and "never read the
two raw booleans at the call site."

The same struct gets it right nine lines earlier — `canonical_tag: options::CanonicalTag`
(`:52-53`) — so the file contains both the convention and its violation. Side effect: the
`referrers: bool` field at `:62` is never read anywhere.

**Remediation.** Add `crates/ocx_cli/src/options/referrers.rs` modelled on
`options/canonical_tag.rs` (bistate, default on, `enabled()`, `overrides_with` both ways,
the same four last-wins unit tests). Flatten it and call `self.referrers.enabled()`.

### A4 [Warn] `copy --help` prints `push`'s wording for `--canonical-tag`

`crates/ocx_cli/src/command/package_copy.rs:50-53`

The doc comment written on the `canonical_tag` field is orphaned. clap renders the
*flattened struct's* own field docs, so `ocx package copy --help` prints
`options/canonical_tag.rs`'s text verbatim:

```
      --canonical-tag
          Push a `sha256.<hex>` tag pointing at each pushed platform manifest (default)
```

"pushed" is `push`'s vocabulary reaching a `copy` user, and the carefully written
"each *copied* platform manifest" at `:50-51` reaches nobody. This is
`quality-cli-help.md`'s render-source gotcha, whose instruction is to put the user-facing
text on the surface clap actually renders and confirm with `--help`.

**Remediation.** Neutralise the wording in `options/canonical_tag.rs` so it reads correctly
for both verbs — "a `sha256.<hex>` tag pointing at each platform manifest this command
writes (default)" — and delete the dead comment in `package_copy.rs`.

### A5 [Warn] The `Digest` column means two different things

`crates/ocx_cli/src/api/data/package_copy.rs:88-128`, contract at
`crates/ocx_lib/src/publisher/copy.rs:129-131`

For `added` / `replaced` / `unchanged` rows the digest is what this copy placed. For a
`kept (not in source)` row it is the digest the target already had — stated in the
`CopiedPlatform::digest` doc comment, invisible in the output. A reader scanning the
column cannot tell which digests this run is responsible for, which is exactly the
distinction the row list exists to make.

**Remediation.** No schema change needed — the disposition already carries the
information, it just is not said anywhere the user reads. Add one sentence to the reference
Output section (`website/src/docs/reference/command-line.md:3670-3679`) and to the
`CopyReport` doc comment.

### A6 [Warn] `--description` is invisible in the report on every path

`crates/ocx_cli/src/command/package_copy.rs:142-148`

Under `--dry-run` the description copy is silently skipped (`self.description &&
!self.dry_run`) with no row and no note, so a dry run of `copy --description` prints a plan
that omits the thing the flag asked for. On a real run the outcome reaches the user only as
a stderr `warn` when the source has none (`:146`), and never as a field — so `--format
json` cannot tell a CI job whether the description travelled. `subsystem-cli-api.md`
"Report Actual Results": commands report what happened, and task return values drive the
report.

**Remediation.** Add `description: Option<DescriptionOutcome>` to `CopyReport`
(`copied` / `absent` / `skipped-dry-run`), and under `--dry-run` report `skipped-dry-run`
rather than dropping the flag on the floor.

### A7 [Warn] Exit 81 is reachable and undocumented

`website/src/docs/reference/command-line.md:3681-3692`

`ocx --offline package copy …` reaches `context.remote_client()?`
(`package_copy.rs:121` → `app/context.rs:592`), which returns `Error::OfflineMode` →
`ExitCode::PolicyBlocked` (81) with the text `network operation attempted in offline mode`.
The exit-code table lists 64, 79, 80 and 84, and no 81.

`copy` is registry → registry and can never succeed offline. The sibling in that class,
`package sign`, treats this as a deliberate refusal rather than a passive network failure
(`package_sign.rs:110-113`, comment: "offline sign is a deliberate rejection, NOT a passive
network-access failure") and documents the code (`command-line.md:3732`).

**Remediation.** Add the 81 row to the table. Optionally follow the sign precedent
(`package_sign_common::refuse_when_offline`) so the refusal names `--offline` and the
command rather than surfacing as a generic network error.

### A8 [Suggest] "pass `--identifier`" told to someone who passed `--identifier`

`crates/ocx_cli/src/command/package_copy.rs:116-118`

`resolve_target` returns a supplied `--identifier` unchanged (`:163-165`), so a tagless one
falls into this check and the user who ran `-i prod.example.com/acme/mytool` is told
`target prod.example.com/acme/mytool has no tag; pass --identifier with one`.

**Remediation.** Branch the message. When `self.identifier.is_some()`, say "add a tag to
`--identifier`, e.g. `-i {target}:1.4.2`"; keep the current wording for the `--to` path,
where it is accurate.

### A9 [Suggest] Shared helper reached across a sibling leaf module

`crates/ocx_cli/src/command/package_copy.rs:81`

`value_parser = super::package_push::parse_annotation` reaches into another leaf for a
helper now shared by two of them. `subsystem-cli.md` "Command Module Structure" puts
helpers shared by 2+ leaves in `<command>_common.rs`; `package_sign_common.rs` is the
established instance in this very directory. Reusing the parser is right — its location is
what drifts.

**Remediation.** Move `parse_annotation` (`package_push.rs:403`) into a `package_common.rs`
and have both leaves call it.

### A10 [Suggest] The log line asserts an action a dry run will not take

`crates/ocx_cli/src/command/package_copy.rs:126`

`log::info!("copying {source} to {target}")` fires before the dry-run branch, so
`--dry-run -l info` states that a copy is happening.

**Remediation.** `if self.dry_run { "planning copy of" } else { "copying" }`, or move the
log below the dry-run split.

### A11 [Warn] Dry-run rows use the vocabulary of a completed copy

`crates/ocx_cli/src/api/data/package_copy.rs:88-106`, pinned by
`test/tests/test_package_copy.py:256`

Under `--dry-run` the per-platform rows read `added` and `replaced` — indicative, past
tense, describing writes that did not happen. The only thing distinguishing a plan from a
finished promotion is the word `planned` in a `Status` cell of the *second* table, which
Stage 1 has already flagged for removal on other grounds; fold that table into the first
and the signal disappears entirely.

The in-tree precedent moves the vocabulary into the row itself: `api/data/pull_dry_run.rs`
reports `would-fetch`, never `fetched`.

**Remediation.** With A1's typed `Disposition` in place, render `would add` / `would
replace` / `unchanged` in `print_plain` when the status is `Planned`. Plain-mode only —
JSON keeps the stable slug plus the top-level `status`, so no consumer branches on prose.
`subsystem-cli-api.md`'s column budget explicitly permits a plain-only divergence
("Plain-only. Never changes a JSON key, shape, or value").

---

## Deferred

### D1 [Warn] Should `--dry-run` pre-flight push credentials at the target?

`crates/ocx_cli/src/command/package_copy.rs:122-124`

`ensure_auth` — a `RegistryOperation::Push` pre-flight (`publisher.rs:107-109`) — is
skipped under `--dry-run`. The dry run is *not* auth-free though: `read_target_entries`
still reads the target index, so pull credentials are exercised and push credentials never
are. Against a private production registry the plan prints cleanly and the real run can
still fail 80, while the user guide sells the flag as "so a release job can show the plan
before it acts" (`promoting-packages.md:141-147`).

**Why a human decides:** both readings are defensible and the question is policy, not
correctness — whether a preview may make an authenticated (side-effect-free) probe against
production, or must stay strictly read-only. That is a call about touching production, not
one a reviewer should make.

### D2 [Suggest] `copy` and `describe --from` invert which side is positional

`crates/ocx_cli/src/command/package_copy.rs:89` vs
`crates/ocx_cli/src/command/package_describe.rs:25,53`, read together at
`website/src/docs/user-guide/promoting-packages.md:106`

`copy --to <DEST> <SOURCE>` versus `describe --from <SOURCE> <DEST>`. Each is locally
right — `copy` follows `cp SOURCE DEST`, and `describe`'s positional was already the target
before this diff — but the user guide places the two commands four lines apart, so a reader
meets both orders in one sitting.

**Why a human decides:** there is no correct fix inside this diff. Changing `describe`'s
positional is a CLI-contract break on a shipped command; adding a `--to` alias to
`describe` so the promotion pair reads the same way is a surface-expansion call. Both are
owner decisions.
