# Quality review — `plan_index_claim_command.md`

**Reviewer:** reviewer (focus: quality), Opus 5
**Target:** `.claude/artifacts/plan_index_claim_command.md` (State: plan-approved, tier high)
**Standards:** `quality-core.md`, `quality-rust.md`, `quality-rust-errors.md`,
`quality-rust-exit_codes.md`, `arch-principles.md`, `subsystem-cli*.md`, `subsystem-tests.md`,
`CLAUDE.md`
**Rules of engagement:** accepted ADR decisions are not re-litigated. Every ruling below opened
the file it names.

---

### F-01 · Block · `ClaimError` never reaches the exit-code classifier — `cli/classify.rs` is in no work package

**Where:** Component contracts § C-052; Parallelization § Work packages, WP-9 and WP-11 Expected files.

**Problem:** `crates/ocx_lib/src/cli/classify.rs:99-102` states the registration contract in its
own doc comment:

> Add a new `try_downcast!` entry here whenever a new top-level error type gains a
> `ClassifyExitCode` impl.

`AnnounceError` is registered there (`classify.rs:181`), and so are `ForgeError` (`:180`),
`SsrfError`, `ConfigError` and thirty-odd others. The plan introduces `ClaimError` with an
explicit `ClassifyExitCode` impl (C-052) but `crates/ocx_lib/src/cli/classify.rs` appears in
**no** work package's Expected files column and in no contract. Without a `try_downcast!(ClaimError)`
row, `classify_error` walks the `source()` chain, never downcasts `ClaimError`, and falls through
to `ExitCode::Failure` (`classify.rs:95`).

The consequence is not partial. Codes that live on `ClaimError`'s *own* variants — 65 for an
already-committed root (C-050, S-004), 79 for `OwnerUnknown` (C-048, S-006), 64 for a bot
identity (C-049, S-009) and for an id mismatch — all become exit 1. Only the forge-derived codes
survive, because `ForgeError` is separately registered and reachable through `#[source]`. S-037
("a script branches on exit codes") is the scenario this breaks, and the acceptance tests that
would catch it (`test_existing_root_refused_65`, `test_owner_unknown_79`, `test_no_acting_identity_64`)
sit in WP-13 — two waves after WP-9 stubs the type, so the defect surfaces late.

Second-order: the plan's own merge protocol re-validates each branch's file set with
`git diff --name-only <base>..<wp-branch>` against the Expected Files column. A necessary edit
to a file no package owns fails that gate rather than merging quietly.

**Fix:** Add `crates/ocx_lib/src/cli/classify.rs` to WP-9's Expected files, and add a contract:
"**C-052a** — `ClaimError` is registered in `cli::classify`'s `try_downcast!` ladder; a unit test
drives `classify_error` over a boxed `ClaimError` and asserts the claim-owned codes (65/79/64),
not `ClaimError::classify` directly." Testing `classify` in isolation is exactly the green that
cannot tell registration from its absence.

---

### F-02 · High · C-041 mints a second backoff schedule; `forge/poll.rs` already produces `1, 2, 4, 8, 15` exactly

**Where:** Component contracts § C-041; Test inventory WP-8 (`poll_backoff_is_1_2_4_8_15`).

**Problem:** C-041 specifies "a bounded poll at 1s, 2s, 4s, 8s, 15s, giving up near 30s of wall
clock". `crates/ocx_lib/src/forge/poll.rs` already exists — 144 lines, in the same module tree the
plan is editing — and is a parametrised, deterministic, clock-free schedule built for exactly this:

```rust
pub struct PollSchedule { initial_interval, max_interval, deadline, request_timeout }
pub fn backoff_delays(schedule: &PollSchedule) -> Vec<Duration>
```

Its doc comment states the design intent verbatim: *"Deterministic and clock-free: the readiness
loop drives off this list, and tests assert the schedule without sleeping."*

Running C-041's numbers through the existing function: with `initial_interval = 1s`,
`max_interval = 30s`, `deadline = 30s`, `backoff_delays` yields `1, 2, 4, 8` (cumulative 15) and
then clamps the fifth delay to the remaining `15s` — **`[1, 2, 4, 8, 15]`, total 30s**. Byte-for-byte
C-041's schedule, from a config literal and zero new code.

The plan never mentions `forge/poll.rs`. WP-8 owns `git_workspace.rs` alone, so the second
schedule lands inside it as a private constant plus its own loop. That is a second clock in the
same module the ADR was emphatic must have one home, and `quality-core.md` § DRY names it:
"Single source of truth for business logic — rule in two places, one go stale." It is also
ponytail rung 2 — a helper a few files over, re-implemented.

**Fix:** Rewrite C-041 to name the existing type: "the merge-request confirmation drives
`forge::poll::backoff_delays` with `PollSchedule { initial_interval: 1s, max_interval: 30s,
deadline: 30s, .. }`, which yields `1, 2, 4, 8, 15`. No second schedule." Add `forge/poll.rs` to
WP-8's Expected files only if `PollSchedule`'s defaults must be re-documented; the yielded list
needs no change. Replace `poll_backoff_is_1_2_4_8_15` with an assertion over
`backoff_delays(&claim_schedule())` so the test pins the *configuration*, not a re-derivation.

---

### F-03 · High · Putting `OCX_ANNOUNCE_GIT_TOKEN` in `CREDENTIAL_KEYS` breaks the ocx-mirror plugin path, and C-066 does not record the asymmetry

**Where:** Component contracts § C-066; Cross-repository closeout (WP-16).

**Problem:** C-066 requires `OCX_ANNOUNCE_GIT_TOKEN` to enter `ocx_lib::env::keys::CREDENTIAL_KEYS`.
`crates/ocx_cli/src/app/plugin_dispatch.rs:192-194` removes every `CREDENTIAL_KEYS` entry from the
plugin child's environment:

```rust
for credential in ocx_lib::env::keys::CREDENTIAL_KEYS {
    cmd.env_remove(credential);
}
```

`.claude/rules/subsystem-cli.md` § Credential exemption records the sibling variable on the
**opposite** side of that line, with the reason:

> `OCX_ANNOUNCE_TOKEN` | `command/package_announce.rs` | **Known-open, cross-repo decision.** A
> forge PAT, so it meets the rule — but `ocx-mirror` announces from a plugin process and inherits
> it deliberately. Adding it would break that. Owner's call.

So after C-066 lands, a plugin-dispatched `ocx-mirror` announce inherits the API half of the
credential pair and **not** the push half. Under `--transport git` the push credential silently
falls to precedence step 3 ("nothing injected", C-063), git's ambient helpers authenticate, and
the merge request is authored by whatever identity the runner's helper produces — the exact
failure C-064's warning exists to prevent, arriving through a path C-064 never sees. It is latent
today only because `AnnounceConfig` carries no `transport` field (the plan files that as a WP-16
issue), which is precisely why it must be written down now.

That table's own instruction is the standard being missed: *"They are recorded so a reviewer does
not read their absence as an oversight."*

**Fix:** Amend C-066 to require the exemption-table row to state the asymmetry explicitly — "on
the list, unlike its `OCX_ANNOUNCE_TOKEN` sibling; a plugin-dispatched announce does **not**
inherit it" — and add one line to the WP-16 `ocx-mirror transport fields` issue: any transport
wiring there must pass the push credential explicitly rather than relying on inheritance.
(`OCX_ANNOUNCE_GIT_USERNAME` staying **off** the list is correct and checked: the membership rule
in `env.rs:205-209` is "if holding the string authenticates you", and a username does not.)

---

### F-04 · High · The git-version gate's *accept* boundary has no test, and the repo's own `Version::Ord` gets it backwards

**Where:** Component contracts § C-013, C-020; Test inventory WP-5
(`probe_git_binary_refuses_below_2_31`, `probe_git_binary_refuses_absent`).

**Problem:** C-013 types the gate as `GitBinary { path: PathBuf, version: Version }` without
naming which `Version`. The workspace has exactly one (`crates/ocx_lib/src/package/version.rs:15`),
with `TryFrom<&str>` (`:427`) and `Ord` (`:335`) — so an implementer reading C-013 will reach for
it, which is the right reflex under ponytail rung 2 and `quality-core.md` § "Don't Own Non-Domain
Code".

It is the wrong type here, and the failure is silent. `Version::cmp` implements **rolling-parent**
semantics: at the patch step, a version with no patch returns `Ordering::Greater`
(`version.rs:381-388`). So `Version::try_from("2.31")` compares **greater than**
`Version::try_from("2.31.0")`, and a gate written as `parsed >= minimum` rejects git 2.31.0 — the
exact boundary release the gate is named for.

The named tests cannot catch it. Both are refusal tests (`refuses_below_2_31`, `refuses_absent`);
there is no test that 2.31.0 (or 2.31, or 2.54.0) is **accepted**. `quality-core.md` § Unchecked
Green: "demonstrate **both** outcomes. Show it red, show it green… Either one alone is half a
proof." A gate that refuses everything passes both named tests.

**Fix:** Two edits. (1) C-013 names the type path, and C-020 states the comparison is a plain
`(major, minor)` tuple compare over the extracted digits — **not** `package::version::Version`,
with the rolling-parent divergence named as the reason so the next reader does not "simplify" it
back. (2) Add `probe_git_binary_accepts_the_boundary_release` to WP-5, asserting `2.31`, `2.31.0`
and a real-world `2.54.0.windows.1` are all accepted, and `2.30.9` is not.

---

### F-05 · High · `capability_checks_never_empty` is a self-consistent green — the property belongs at the producer, or in the type

**Where:** Component contracts § C-011, C-060; Test inventory WP-11
(`capability_checks_never_empty`); S-011, S-036.

**Problem:** C-060 makes `capability_checks` "**non-empty on every run**, inapplicable checks
carrying `status: "skipped"` rather than being omitted", and S-036 sells that to pipeline authors
as an assertion that the preflight ran. The named unit test lives in **WP-11**, the CLI package —
two waves downstream of the producers (`ensure_push_access` in WP-6 GitHub / WP-7 GitLab).

A unit test in WP-11 can only assert over a `ClaimOutcome` it constructs itself. If the fixture
carries a non-empty vector, the assertion is a tautology; if it carries an empty one, the test is
asserting the CLI rejects it, which is not the contract. Either way the *producers* — the only
code that can return an empty `Vec<CapabilityCheck>` — are untouched by it. This is the shape
`quality-core.md` warns about: a check whose passing state is indistinguishable from the check
never having run.

The exposure is real, not theoretical: `PushAccess { checks: Vec<CapabilityCheck> }` permits the
empty vector by construction, and the `--out` / no-credential path (S-011) is the one where an
implementation most plausibly returns early with nothing.

**Fix:** Make it unrepresentable rather than tested. Amend C-011: "`PushAccess` is constructed
through `PushAccess::skipped_all()`, which seeds one row per `CapabilityName` at
`CheckStatus::Skipped`; implementations upgrade rows, never build the vector from empty. The
struct has no public constructor that can produce an empty `checks`." Then `capability_checks_never_empty`
becomes redundant and the real coverage is `github_ensure_push_access_emits_rows` (WP-6) plus its
missing GitLab twin — add `gitlab_ensure_push_access_emits_rows` to WP-7, which currently has no
such row.

---

### F-06 · Warn · `from_exit_code_is_exhaustive_without_wildcard` is a self-matching guard over a file whose own doc comment carries the needle

**Where:** Component contracts § C-002; Test inventory WP-1.

**Problem:** C-002's property — "The match stays **wildcard-free**: adding C-001 without C-002
must fail to build" — is already enforced by the compiler and already documented in place.
`crates/ocx_lib/src/cli/error_category.rs:41-48` says so, and `error_category_total_over_exit_codes`
(`:118-124`) already spells out why no test is needed for totality: *"Totality itself is the
compiler's job… an unclassified `ExitCode` variant is an E0004 build failure, not a silent
`internal`."*

Writing `from_exit_code_is_exhaustive_without_wildcard` as a source-text guard walks straight into
the trap `quality-rust.md` § "Structural guards" names first:

> **Strip comments before scanning.** A denylist that quotes the forms it forbids — the right
> thing for a comment to do — matches its own comment.

The needle is already in the scanned file. `error_category.rs:47` reads: *"the former cross-crate
form needed a `_ => Internal` arm, under which a new exit code compiled clean"*. A guard scanning
for `_ =>` fires on that line on day one; the natural fix (delete the comment, or narrow the
needle to a formatting-dependent literal) either destroys the explanation or produces a needle
that stops matching after `cargo fmt` and reports green forever.

**Fix:** Drop `from_exit_code_is_exhaustive_without_wildcard` from WP-1. Restate C-002's second
half as a build-time property, not a test: "adding `ExitCode::ForgeCapabilityUnavailable` without
the `ErrorCategory` arm is an E0004 build failure — this is the compiler's guard, and no test
duplicates it." If the plan wants belt-and-braces, the honest form is the existing table's shape:
add the 86 row to `error_category_total_over_exit_codes` and bump its count (see F-18).

---

### F-07 · Warn · `spawn_allowlist_row_present` / `spawn_allowlist_has_no_stale_rows` duplicate two guards that already exist

**Where:** Component contracts § C-021; Test inventory WP-5; Per-package notes (WP-5 firewall proof).

**Problem:** Both tests already exist in `crates/ocx_lib/src/launch.rs`, under different names and
with adversarial detail the new names would not reproduce:

- `no_process_spawn_outside_launch` (`launch.rs:1114`) — sweeps every `.rs` under `crates/`,
  strips `//` lines (`mentions`, `:1035-1041`), excludes the seam (`is_seam`, `:1046-1048`), and
  fails naming the offending path. It reds precisely when `forge/git_command.rs` spawns without a
  row — for the right reason.
- `every_allowlisted_file_still_exists` (`launch.rs:1057`) — is the stale-row check verbatim.

`spawn_allowlist_row_present` as a fresh assertion would read `SPAWN_ALLOWED.iter().any(|(p, _)| p == "…")`
over a constant declared in the same file — its needle is a literal in the file it scans, which is
`quality-core.md`'s "a detector that can match its own invocation is measuring itself". It passes
whether or not `git_command.rs` exists, whether or not it spawns, and whether or not the firewall
is still enforced.

Worse, adding a second stale-row guard creates the situation `quality-core.md` names as a
corollary: *"Two independent guards defending one property both pass when either alone is
deleted."* The WP-5 per-package note demands the row be proved red — with two guards watching one
row, deleting either one still leaves a green suite, so the proof records less than it appears to.

**Fix:** Delete both names from WP-5's test inventory. Rewrite C-021 as: "`SPAWN_ALLOWED` gains a
row naming `ocx_lib/src/forge/git_command.rs` with its reason. The existing
`no_process_spawn_outside_launch` and `every_allowlisted_file_still_exists` are the guards; WP-5's
obligation is to **run** the first one red (row removed, file spawning) and green (row present),
and record both outputs in its worktree notes." That is a real red/green proof; a new tautology is not.

---

### F-08 · Warn · C-007's `exactly_one_reader_of_announce_clock_env` names neither comment-stripping nor self-exclusion

**Where:** Component contracts § C-007; Test inventory WP-2.

**Problem:** C-007 says only "Exactly one site in `crates/` reads the environment variable." As a
source-text sweep for `__OCX_TESTING_ANNOUNCE_CLOCK` that is three ways wrong before it runs:

1. **The test's own source contains the needle.** Without excluding its own file the count is
   permanently ≥ 1, so deleting the production reader can still leave the assertion satisfied
   depending on how the count is phrased.
2. **A doc comment already names the variable.** `crates/ocx_lib/src/announce/pipeline.rs:103`
   reads *"The `__OCX_TESTING_ANNOUNCE_CLOCK` env seam (test / `__testing` only) pins it…"*. WP-2
   moves the accessor down to `oci::index` and will carry a comparable comment there, plus the
   delegating comment left behind in `announce::pipeline`. A guard that does not strip `//` lines
   reds on documentation — exactly the failure `quality-rust.md` records.
3. **A needle counted across whole files, not call sites,** cannot tell one reader from one reader
   plus one mention.

The repository already has the correct shape one file over: `launch.rs:1035-1041`'s `mentions()`
filters `//`-prefixed lines, and `is_seam()` excludes the declaring file.

**Fix:** Restate C-007: "a structural guard sweeps `crates/**/*.rs`, **filtering `//`-prefixed
lines and excluding the guard's own file**, and asserts exactly one file contains
`__OCX_TESTING_ANNOUNCE_CLOCK`; it reuses the `mentions`/`is_seam` shape from `launch.rs`. The
guard asserts the count is **exactly** one, not merely non-zero, so a needle that stops matching
reds instead of reporting green."

---

### F-09 · Warn · WP-8 puts thirteen contracts in one file, against "one concept per file" — and is the plan's largest single-file package

**Where:** Parallelization § Work packages, WP-8; Component contracts C-033…C-045.

**Problem:** WP-8's Expected files is exactly one file, `crates/ocx_lib/src/forge/git_workspace.rs`,
carrying: clone hygiene and the unwind guard (C-033), credential injection via `GIT_CONFIG_*`
(C-034), the child-environment allowlist (C-035), the filtered fetch (C-036), the four-way compare
(C-037), the index-file commit chain (C-038), push-option rendering and control-character refusal
(C-039), lease policy (C-040), the merge-request poll (C-041), the refresh-commit path (C-042),
the re-fetch retry (C-043), the stderr classifier (C-044) and commit identity (C-045). That is
thirteen contracts and at least four genuinely separable concepts.

`arch-principles.md` § Code Style Conventions is explicit, and marks the deviation a bug:

> **Module structure** | One concept per file, deep nested modules (`platform/operating_system.rs`)
> — no `mod.rs`, use named module files | **Deviation = Bug**: Monolithic files

The precedent to avoid is in the same tree: `announce/pipeline.rs` is 2040 lines and is where the
`NonFastForward`-retry bug of C-056 hid (`announce.rs:293` / `:308` / `:406` — three sites the
plan itself had to trace by line number to find). WP-8 is on course to produce the second one.

It is also the sizing problem. WP-8 is `L`, `panel` review, and sits on the critical path's
consumer side; two of its concepts are pure and independently testable without any of the rest.

**Fix:** Split the file, which splits the package for free and keeps file-disjointness:

- `forge/git_stderr.rs` — C-044's classifier. Pure `(&[u8], &PushAccess) -> ForgeError`. Its whole
  test inventory (`classifier_maps_each_phrase`, `capability_row_needs_unknown_preflight`) moves
  with it, and it becomes a small wave-3 package with **no** dependency on the workspace at all.
- `forge/git_push_options.rs` — C-039's rendering and control-character refusal. Pure, small,
  security-relevant, and the one piece a reviewer most wants to read alone.
- `forge/git_workspace.rs` keeps C-033…C-038, C-040, C-042, C-043, C-045 and drives the two above
  plus `forge::poll` (F-02).

That turns one `L` panel-reviewed package into one `M` and two `S`, all file-disjoint, all in
wave 3.

---

### F-10 · Warn · WP-5 is two packages wearing one hat, and the cut shortens the critical path the plan says it wants to shorten

**Where:** Parallelization § Work packages, WP-5; § Critical path; § Under-parallelization justification.

**Problem:** WP-5's Expected files is ten entries spanning three unrelated jobs: (a) the trait and
vocabulary surface (`forge.rs`, `api.rs`, `error.rs`, `kind.rs`) plus stub bodies in two files
totalling 2714 lines (`github.rs` 1628, `gitlab.rs` 1086) and a stub `git_workspace.rs`; (b) a
**fully implemented** capturing spawn seam with redaction and version probing (`git_command.rs` —
the per-package note is explicit that this one is not stubbed); (c) a new credentials module whose
`api_is_job_token` is derived from an environment snapshot (`credentials.rs`), and (d) the
firewall row in `launch.rs`. Eleven new error variants with their `ClassifyExitCode` arms ride
along.

The plan already identifies the cost — *"WP-5 is the single serialization point and the one worth
shortening: everything in wave 3 waits on it"* — and then declines to shorten it, on the ground
that "there is no cut that makes it two". There is, and it falls along the dependency edge the
plan has already drawn.

**Fix:** Cut at the contract/implementation seam:

| New WP | Files | Wave | Depends on |
|---|---|---|---|
| **WP-5A** — forge contract surface | `forge.rs`, `forge/api.rs`, `forge/error.rs`, `forge/kind.rs`, `forge/github.rs` (stubs), `forge/gitlab.rs` (stubs), `forge/git_workspace.rs` (signatures) | 2 | WP-1 |
| **WP-5B** — spawn seam + credentials + firewall row | `forge/git_command.rs`, `forge/credentials.rs`, `crates/ocx_lib/src/launch.rs` | 3 | WP-5A |

File sets are disjoint. WP-5B needs only the `ForgeError` variants WP-5A lands. Wave 3 then holds
WP-5B alongside WP-6/7/9/10, and only WP-8 and WP-11 wait on WP-5B — neither of which starts before
wave 3 anyway. The critical path becomes `WP-1 → WP-5A → WP-9 → WP-11 → WP-12 → WP-14 → WP-16` with
a materially smaller serialization point, and wave 2 stops being a package that reviews 2714 lines
of stub edits and a live subprocess implementation under one `panel`.

---

### F-11 · Warn · The plan states no `#[non_exhaustive]` policy, for the new error enums or the eight new internal ones

**Where:** Component contracts § C-012, C-014, C-018, C-046 (`ClaimError`, `WriteTransport`,
`CapabilityName`, `CheckStatus`, `OwnerIdentitySource`, `ClaimTarget`, `ClaimStatus`, `OwnerSpec`).

**Problem:** `#[non_exhaustive]` appears nowhere in the plan (grep: 0 hits). Two rules pull in
opposite directions and the plan resolves neither, so a worker will guess:

- `arch-principles.md` § Code Style Conventions: *"**Internal enum exhaustiveness** | Omit
  `#[non_exhaustive]` on internal non-error enums so matches stay total across workspace… |
  **Deviation = Bug**: `#[non_exhaustive]` on closed internal enum"*. The carve-out — *"Error enums
  exempt"* — is half a sentence at the end of the same cell and is easy to miss.
- `quality-rust-errors.md` § Warn-tier: *"Missing `#[non_exhaustive]` on public error enums"*.

The tree is consistent with both: `ForgeError` (`forge/error.rs:13`) and `AnnounceError`
(`announce/error.rs:39`) both carry it; the internal enums in `forge/api.rs` (`BranchComparison`,
`Mergeability`, `RefUpdate`) do not. A `ClaimError` without it, or a `CheckStatus` with it, is a
graded bug under the constitution in either direction — for a package the plan gives `panel`
review, which is where an unstated convention costs the most.

**Fix:** One line in Component contracts: "**C-018a** — `ClaimError` and any new error enum carry
`#[non_exhaustive]`, matching `ForgeError` and `AnnounceError`. `WriteTransport`, `CapabilityName`,
`CheckStatus`, `OwnerIdentitySource`, `ClaimTarget`, `ClaimStatus` and `OwnerSpec` are internal
non-error enums and carry **no** `#[non_exhaustive]`, per `arch-principles.md` § Internal enum
exhaustiveness."

---

### F-12 · Warn · No error-message style contract for eleven new `ForgeError` variants and every `ClaimError` variant

**Where:** Component contracts § C-018, C-052.

**Problem:** C-018 names eleven new `ForgeError` variants and their exit codes; C-052 introduces
`ClaimError`. Neither states anything about the `#[error("…")]` strings or `#[source]`, and
`quality-rust-errors.md` makes several of the possible mistakes **Block-tier**:

> - **Sentence-case or trailing-punctuation `#[error("...")]` strings** in library crates
> - **Missing `#[source]` on wrapping error variants**: every variant wrapping inner error must
>   return it via `source()`. Without it, chain walking breaks for logging, diagnostics, downcasting.

The second is not cosmetic here. `GitUnavailable` wraps an `io::Error` (git absent → `ENOENT`),
`GitCommandFailed` wraps a captured child failure, and `MergeRequestUnconfirmed` may wrap a
transport error from the poll. A missing `#[source]` on any of them breaks exactly the chain walk
that F-01 depends on, and the plan's exit-code contracts are stated as if the chain were sound.

There is also a live interaction the plan is half-aware of: C-052 correctly names the
`#[error(transparent)]` trap, and the precedent is one file over —
`crates/ocx_lib/src/announce/error.rs:19-35` documents it and `:208-215` implements the explicit
delegation. C-052 should point at that precedent rather than restate the mechanism.

**Fix:** Add "**C-018b** — every new variant's `#[error("…")]` message is a lowercase phrase with
no trailing punctuation (`quality-rust-errors.md` § The Canonical Rule); every variant that wraps
an inner error carries `#[source]`; `ForgeError::GitCommandFailed` and `::GitPushFailed` carry the
**redacted, capped** stderr as a plain field, never the raw bytes." Point C-052 at
`announce/error.rs:19-35` as the implemented precedent.

---

### F-13 · Warn · C-022's redaction and C-034's injection can agree on a *wrong* base64, and the named test cannot tell

**Where:** Component contracts § C-022, C-034; Test inventory WP-5
(`redactor_masks_all_three_secret_forms`), WP-14 (`test_secret_absent_from_argv_config_url_and_stderr`).

**Problem:** C-034 puts the credential on the wire as `Authorization: Basic <base64>` over
`user:secret`; C-022 requires the redactor to mask "the `base64(user:secret)` blob the credential
exists as on the wire". Two independent constructions of the same string, in two different
modules, with nothing binding them.

If they disagree — a different username default (C-063 has *two*: `gitlab-ci-token` for the
`OCX_ANNOUNCE_GIT_TOKEN` rung **and** for the API-credential rung), a padding or alphabet
difference, a trailing newline — the redactor masks a string that never appears and the real blob
passes through into stderr and logs. `redactor_masks_all_three_secret_forms` is a WP-5 unit test:
it will construct the blob the same way the redactor does, so it stays green for the wrong reason.
The acceptance test (`test_secret_absent_from_argv_config_url_and_stderr`) only greps for the
*raw* secret unless it is told to grep for the encoded form too — and a test that greps for a
string it built itself has the same defect.

`base64 = "0.22.1"` is already a workspace dependency and `crates/ocx_lib/Cargo.toml:88` already
pulls it, so there is no reason for two spellings to exist.

**Fix:** Amend C-034 to name one producer: "`ForgeCredentials::basic_blob()` renders
`base64::engine::general_purpose::STANDARD.encode(format!("{user}:{secret}"))`; the injector and
the redactor both call it, and nothing else constructs the blob." Then amend C-022's test to
assert the redaction against the string `basic_blob()` returns, and add to
`test_secret_absent_from_argv_config_url_and_stderr` an explicit grep for the encoded form
extracted from the fixture's **received `Authorization` header**, not from a locally rebuilt copy.

---

### F-14 · Warn · C-062's "all of it lives in `deprecated.rs`" is not achievable, and the stated 0.7 removal is not one file deletion

**Where:** Component contracts § C-062; Deviations § DV-3; Per-package notes (WP-12).

**Problem:** C-062 ends "All of it lives in `deprecated.rs`", and the WP-12 note repeats it: *"it
lands the whole flag→positional window there so the 0.7 removal is one file deletion."*

`crates/ocx_cli/src/command/deprecated.rs` (33 lines, read in full) is scoped to whole-subcommand
renames, and says so:

> Every name renamed in 0.6 dispatches through this module, so one grep finds the whole set and
> 0.7 removes it by deleting this file together with the hidden `Command` / `Package` variants that
> call it. **Nothing else may depend on it.**

A field-level window cannot fit that shape and the plan knows it — DV-3 correctly establishes that
the mechanism is "two `Arg` ids (`--package` hidden, positional canonical) in one `ArgGroup` with
`required = true`, merged in code". Those two `Arg` ids and the `ArgGroup` live on the announce
args struct in `crates/ocx_cli/src/command/package_announce.rs`, which is in WP-12's file set.
Deleting `deprecated.rs` in 0.7 removes the warning helper and leaves the hidden `--package` arg
and the `ArgGroup` behind, still accepted, now silently.

Note also that the existing helper's message is command-shaped —
`"`ocx {old}` is renamed to `ocx {new}` and is removed in {REMOVAL_RELEASE}"` — so the new helper
is a new message, not a reuse of `warn_renamed`; DV-3's "`warn_renamed`'s signature… reused" is
imprecise about which part is reused (the channel and `REMOVAL_RELEASE`, not the string).

**Fix:** Restate C-062's last sentence: "the warning helper and the `REMOVAL_RELEASE` constant
live in `deprecated.rs`; the hidden `Arg` id and the `ArgGroup` live on the announce args struct
and are marked with a `// 0.7 removal:` comment naming `deprecated.rs`. The 0.7 removal is a file
deletion **plus** those two clap declarations — record both in the module doc so one grep still
finds the whole set." Reusing `REMOVAL_RELEASE` (rather than re-typing "0.7") is correct as
written and worth keeping explicit.

---

### F-15 · Warn · C-059's arg-id test must derive the shared set from the struct, or it cannot go red for the right reason

**Where:** Component contracts § C-059; Risks table ("Two write commands drift apart").

**Problem:** C-059 is the plan's own answer to a named risk — *"the one thing that makes the risk
row a check rather than a hope"* — so it is worth being adversarial about. As written
("a test enumerates each command's argument ids and asserts the two sets agree on the shared
block"), the phrase "the shared block" is unbound. If the test hardcodes the list of shared ids,
then adding a flag to `ForgeWriteOptions` **and** to only one command still passes: the hardcoded
list never learned about the new flag. That is the drift the risk row describes, surviving the
mitigation.

The set has to come from `ForgeWriteOptions` itself. clap can produce it:
`<ForgeWriteOptions as clap::Args>::augment_args(clap::Command::new("probe"))`, then read the
resulting `Command`'s argument ids. Nothing else can drift.

**Fix:** Amend C-059: "the expected set is **derived** from
`<ForgeWriteOptions as clap::Args>::augment_args` on an empty `Command`, never hardcoded; the test
asserts every derived id is present on both `claim` and `announce`, and additionally asserts the
derived set is non-empty so a refactor that empties the struct reds rather than passing vacuously."
The non-emptiness assertion is the same guard `plugin_dispatch.rs:264-266` already applies to
`CREDENTIAL_KEYS` ("an empty credential list would make the loop below vacuous") — reuse the shape.

---

### F-16 · Warn · The Windows arm of C-035 has no reachable test anywhere in the plan

**Where:** Component contracts § C-035; Test inventory WP-14.

**Problem:** C-035's allowlist has Windows-only members — `USERPROFILE`, `HOMEDRIVE`, `HOMEPATH`,
`SYSTEMROOT` — and Windows-only exclusions. `SYSTEMROOT` appears nowhere in `crates/` today
(grep: 0 hits), so there is no precedent to copy and no existing coverage. Every named test for
C-035 is a pytest in WP-14 that spawns real git; the acceptance suite runs on Linux, and the one
platform-conditional test in the inventory is `::test_tempdir_mode_0700_unix_only`, which covers
C-033's Unix half, not C-035's Windows half.

A `git` child on Windows with no `SYSTEMROOT` fails to resolve DNS and to open the certificate
store — a total failure of `--transport git` on a whole platform, discovered by a user. The
repository already knows this failure mode and labels it: `utility/child_process.rs:120` carries a
`#[cfg(windows)]` test with the comment *"**Unexercised on this repo's Linux CI** — compiled only
on Windows."* That is honest, but it is the state C-035 would ship in with nothing at all.

**Fix:** Make the allowlist a data table rather than inline `cfg` branches — a
`const PASSTHROUGH: &[&str]` per platform plus `const NEVER: &[&str]` — and add a WP-5 unit test
`git_child_env_allowlist_tables_are_complete` that asserts **both** tables' contents by `#[cfg]`,
so at minimum the Windows list is pinned and reviewable on Linux. Keep the behavioural WP-14
tests for the Unix arm. Record the residual gap the way `child_process.rs` does, in the test's own
doc comment.

---

### F-17 · Warn · WP-4's chunked-body decoder is a hand-rolled wire-format codec with no recorded library search and no negative test

**Where:** Research table (`research_plan_index_claim_git_fixture.md`); Parallelization WP-4;
Test inventory WP-4 (`::test_chunked_receive_pack_body_decoded`).

**Problem:** `quality-core.md` § "Don't Own Non-Domain Code" escalates to **Block** for "anything
parsing/emitting an external wire format (serializers, codecs, escaping) — those fail silently,
past local fixtures", and its bar #1 is *"No library implements the requirement, **verified by
searching, not assumed**."*

The research artifact records that search for the *git protocol* half and it is convincing:
`dulwich` was read at source level and rejected because it never negotiates `push-options`;
`CGIHTTPRequestHandler` is removal-bound. That bar is met, and hand-rolling the CGI bridge over
`git http-backend` is the right call — I would keep it.

The **chunked transfer-decoding** half has no recorded search. `http.server`'s
`BaseHTTPRequestHandler` genuinely does not decode chunked bodies (which is why `fake_forge.py`
`test/tests/fake_forge.py:71` never had to), but Python has maintained pure-Python HTTP/1.1
request parsers that do (`h11` being the small one), and the test venv is `uv`-managed so a dev
dependency is nearly free. Bar #1 is not satisfied by silence.

Whichever way it is decided, the test inventory has the wrong shape for it. `::test_chunked_receive_pack_body_decoded`
is the only named test, and it is a positive: it shows the decoder can produce a body. The failure
mode that matters is a decoder that **truncates** — returning the first chunk and dropping the
rest — under which `::test_push_delivers_four_options` sees three options and the assertion
"exactly four" can pass or fail for reasons unrelated to ocx. There is also nothing in the plan
saying how the fixture is made to see a chunked body at all: git sends `Content-Length` for small
pushes, and a claim root is small, so without forcing (`-c http.postBuffer=…`) the chunked path may
never execute against real git and the decoder ships untested by anything but its own unit test.

**Fix:** Either name `h11` in the research artifact as searched-and-rejected with a reason, or keep
the hand-roll and contract it: the decoder refuses chunk extensions and trailers, caps total body
length, and raises rather than truncating. Add two tests to WP-4:
`::test_truncated_chunked_body_is_an_error` (the negative), and make
`::test_chunked_receive_pack_body_decoded` state how it forces chunked framing from real git.

---

### F-18 · Suggest · `error_category_total_over_exit_codes`'s hardcoded `cases.len() == 16` is a fourth edit C-004 does not name

**Where:** Component contracts § C-004.

**Problem:** C-004 names one table ("the frozen `error_kind` inventory test gains the
`forge_capability_unavailable` row"). `crates/ocx_lib/src/cli/error_category.rs` has **two**
frozen tables, and the second carries a hardcoded count:

```rust
assert_eq!(cases.len(), 16,
    "a row was removed from the table above; restore it rather than lowering this count");
```

Adding exit code 86 makes that 17. The failure is loud (a red test on a file WP-1 already owns), so
this is not a risk — it is an under-specified contract on a package whose whole job is that file.
The count assertion's own doc comment explains why it cannot self-heal: *"`cases` is an array
literal, so `len()` is a compile-time constant. Forcing that is the wildcard-free match's job, not
this assertion's."*

**Fix:** C-004 reads: "the two frozen tables in `error_category.rs` both gain the row —
`error_category_serializes_snake_case` and `error_category_total_over_exit_codes` — and the
latter's `cases.len()` assertion moves from 16 to 17."

---

### F-19 · Suggest · `forge/api.rs`'s ownership sentence goes stale, not only its operation count

**Where:** Documentation surfaces table, row `crates/ocx_lib/src/forge/api.rs` ("Stop naming an
operation count", owned by WP-5).

**Problem:** The plan spotted half of it. `crates/ocx_lib/src/forge/api.rs:6-8` reads:

> [`Forge`] is the whole surface [`crate::announce::announce`] needs: ten operations, no forge
> named in any of them.

C-009/C-010/C-011 add three methods, so the count is wrong — the plan handles that. But
`authenticated_identity` and `resolve_user` are **claim** operations; announce (C-061) explicitly
gains neither `owners` nor `author`. After the change the sentence's *subject* is false too: the
trait is the surface announce **and claim** need, and two of its methods announce never calls.

**Fix:** The documentation-surfaces row reads: "the module doc drops the operation count **and**
restates ownership — the trait is the surface `announce` and `claim` both drive; `authenticated_identity`
and `resolve_user` serve claim alone." Worth noting for the reviewer of that WP: the trait reaches
thirteen methods, below `quality-core.md`'s 15-method god-object threshold, and splitting it for
two claim-only methods would be a premature abstraction over two implementations — so keeping one
trait is right, and the doc comment is where the seam gets recorded instead.

---

### F-20 · Suggest · `check_status_has_no_failed_variant` asserts almost nothing

**Where:** Component contracts § C-012; Test inventory WP-5.

**Problem:** C-012's real content is the wire spellings (`passed` / `unknown` / `skipped`,
`git-version` / `push-access` / `job-token-push` / `job-token-allowlist`), which
`capability_name_wire_spellings` covers, and which are correctly flagged one-way. The companion
test asserts an enum does not have a variant — a property visible in the three lines above it, in
the same file, that no code path can violate without the author typing the variant name. It is a
comment with a test harness around it.

The property C-012 actually wants protected is the *inverse*: that no future code path emits a
failing status by folding one into `Unknown`'s spelling. That is a behavioural property of
`ensure_push_access`, already covered by `preflight_unknown_field_does_not_fail` (WP-7).

**Fix:** Drop `check_status_has_no_failed_variant`; move C-012's "no `Failed`" rationale into
`CheckStatus`'s doc comment where the next person to add a variant will read it.

---

### F-21 · Suggest · `test_each_secret_form_proved_red_then_green` names a process, not a property

**Where:** Test inventory WP-14; Red/green discipline section.

**Problem:** A test cannot assert that it was itself observed red. The name promises evidence that
lives in a worker's transcript, and a reader of the suite will take the green as that evidence —
which is the confusion `quality-core.md` § Verification Honesty exists to prevent ("Stating
'verified' without citing evidence: **Block-tier**"). The plan's Red/green discipline section
already states the obligation correctly, in prose, as a procedure.

**Fix:** Rename to what it asserts — e.g. `::test_no_secret_form_appears_in_any_captured_stream` —
and add one line to the WP-14 per-package notes: "the red run for each of the three secret forms is
recorded in the worktree notes with the mutated line and the failing output."

---

### F-22 · Suggest · WP-3 is below the overhead floor; WP-16's `Verify: full` buys no signal

**Where:** Parallelization § Work packages, WP-3 and WP-16.

**Problem:** Two ends of the sizing distribution.

**WP-3** is two markdown edits to `.claude/artifacts/` files, `Review: self`, no tests, `Size: S`.
It still costs a worktree, a branch, a merge and a file-set re-validation. The plan's own
under-parallelization justification applies the overhead-floor argument to a hypothetical ~50-line
package ("splitting the struct into its own ~50-line package would fall below the overhead floor")
but not to this one. It has no dependents and no dependencies, so folding it into WP-1 (also wave
1, also `S`, also two files, disjoint set) costs nothing and removes a merge.

**WP-16** carries `Verify: full` for a package whose only Expected file is
`test/manual/measure-index-clone.sh` — a manual measurement script that nothing imports. The
plan's own justification for `full` is "**WP-16** is the terminal gate", but a serialized
`task verify --force` on the integration branch already runs after WP-15; re-running it because a
shell script under `test/manual/` was added measures nothing new and takes the plan's own
one-at-a-time verify slot. WP-16's five other items are cross-repo issue posts, a live run and a
measurement — none of them is verified by `task verify` at all.

**Fix:** Fold WP-3 into WP-1 (file sets stay disjoint; the WP-1 row's Expected files gains the two
artifacts). Change WP-16's `Verify` to `scoped` and state the real terminal gate separately: "one
serialized `task verify --force` on `hex/index-claim-command` after WP-15 merges, before the
release gates run."

---

### F-23 · Suggest · `test_two_refspec_form_fails_without_branch` controls the fixture, not the assertion

**Where:** Test inventory WP-14 ("the red control"); Component contracts § C-036.

**Problem:** The control is well-intentioned and half-placed. C-036 forbids ocx from ever naming a
second refspec when C-030 found the branch absent, so the control cannot be an ocx invocation —
it must drive `git fetch` directly with two refspecs and observe a failure. What that proves is
that **git** refuses a missing remote ref, which is a property of git, not of ocx and not of the
assertion under test.

The thing that needs a reachable red state is the *assertion* in
`::test_first_claim_absent_branch_one_refspec` — whatever inspects the fetch (the fixture's
request log, a `GIT_TRACE_PACKET` capture, the fixture's recorded refspec list). If that inspector
looks at the wrong record, or at a log that is empty for an unrelated reason, the test passes with
no fetch having happened at all. `quality-core.md`'s prescription is to mutate the consumer arm:
seed the inspected record with a two-refspec entry and watch the assertion red.

**Fix:** Keep the test, rename it to what it controls
(`::test_fixture_rejects_a_two_refspec_fetch_for_an_absent_branch`), and add the assertion-level
control: `::test_refspec_assertion_reds_on_a_seeded_two_refspec_record`. Related: S-026's
"the fixture records **zero** REST write calls on **every** git failure path" is unbounded as
written — a single test covers the injections it drives. Enumerate them in C-026's scenario row
(clone failure, fetch failure, commit-chain failure, push rejection, poll exhaustion) so the green
has a stated scope.

---

### F-24 · Suggest · The new doc script's filename does not follow the section-slug convention and will not bind to its page

**Where:** Parallelization WP-15 Expected files; Documentation surfaces table
(`test/doc_scripts/claiming-a-namespace.sh`).

**Problem:** `test/src/doc_binding.py:296` derives a page path from a script stem with
`stem.replace("__", "/", 1)`. Every script for a page inside a documentation section uses the
double-underscore form — `user-guide__attestations.sh`, `user-guide__ci-oci.sh`,
`in-depth__signing.sh`, `getting-started__install.sh` — and only genuinely top-level pages are bare
(`deps.sh`, `index.sh`).

The plan's new page is `website/src/docs/user-guide/claiming-a-namespace.md`, but its script is
named `test/doc_scripts/claiming-a-namespace.sh` in both the WP-15 Expected files column and the
Documentation surfaces table. That stem binds to a top-level `claiming-a-namespace` page, which
does not exist, so the one-tree gate DV-4 exists to buy either fails or (worse, depending on how
absence is handled) silently covers nothing — the "quietly does less" failure `quality-core.md`
names.

**Fix:** Rename to `test/doc_scripts/user-guide__claiming-a-namespace.sh` in both places.

---

### F-25 · Suggest · `crates/ocx_cli/src/options.rs` already carries WP-numbered comments from a different plan

**Where:** Parallelization WP-11 Expected files (`crates/ocx_cli/src/options.rs`).

**Problem:** `options.rs:12-15` and `:19-28` carry live comments referring to "WP-11", "WP-9" and
"WP5" from earlier plans (`meta-plan_cosign_parity.md`). This plan also has a WP-11 and a WP-9,
both of which touch this file's neighbourhood. A worker opening `options.rs` under this plan's
WP-11 will read *"`Hook` has no consumer until WP-11 flattens it into `self activate`"* as an
instruction addressed to them.

Also worth carrying forward from that file's own doctrine, which the plan does not state: the
`pub mod` idiom exists there specifically so two parallel efforts need not both edit `options.rs`
(*"two of them editing one options file is the collision this layout exists to prevent. Do not add
a `pub use` here later either: the path is the contract"*). `ForgeWriteOptions` has exactly that
shape — one declaring module, two consuming commands in different work packages.

**Fix:** Add to the WP-11 per-package notes: "declare `pub mod forge_write;` and let WP-12 import
`crate::options::forge_write::ForgeWriteOptions` by path — no `pub use`, per that file's own
convention. Ignore the `WP-*` comments already in `options.rs`; they belong to
`meta-plan_cosign_parity.md`." Optionally, a one-line `chore:` renaming those stale references to
their plan slug would stop this recurring.

---

### F-26 · Warn · `Forge` grows to thirteen methods, and C-031 makes two of them refuse based on a runtime field

**Where:** Component contracts § C-009, C-010, C-011, C-031.

**Problem:** After the change `Forge` has thirteen methods (ten today, verified by reading
`forge/api.rs`: `get_file_contents`, `get_ref_sha`, `compare_branch`, `find_open_pull_request`,
`pull_request_mergeability`, `find_fork`, `ensure_fork`, `sync_fork`, `commit_files`,
`open_or_update_pull_request`). Thirteen is under `quality-core.md`'s 15-method god-object
threshold, and splitting the trait for two claim-only methods would be a premature abstraction over
two implementations — so the count is not the finding.

C-031 is. It says `find_fork` / `ensure_fork` "return `TransportOperationUnsupported` under `git`;
`sync_fork` is a logged no-op". That is the LSP violation signal `quality-core.md`'s SOLID table
names verbatim: *"Implementation that panics/throws where contract promises success"*, against a
trait whose own doc comment (`forge/api.rs:115-118`) promises the opposite: *"an implementation
that cannot hold it must return an error rather than approximate it"* — where "error" was meant as
a transport failure, not "this method does not exist in this mode".

It is also two guards on one property, unremarked: C-058 already refuses `--transport git` + `--fork`
at the CLI boundary with exit 64 (S-020), which makes the trait-level refusal unreachable through
the CLI. `fork_ops_refused_under_git` (WP-7) therefore tests a path no user invocation reaches, and
deleting either guard leaves the other green — the corollary in `quality-core.md` § Unchecked Green.

That is defensible defence-in-depth, but the plan should say which is the enforcement point and
which is the backstop, or a later reader will delete the "redundant" one.

**Fix:** One sentence on C-031: "C-058's CLI refusal is the enforcement point; the trait-level
`TransportOperationUnsupported` is a backstop for a non-CLI caller (`ocx-mirror`) and is documented
as such at both sites. `sync_fork`'s no-op logs at `debug` with the transport named." And note in
the WP-7 test list that `fork_ops_refused_under_git` covers the backstop, not the user path.

---

### F-27 · Suggest · Checked and found correct — recorded so the next reviewer does not re-derive them

These looked wrong and are not. Each was ruled by opening the file.

**DV-1's placement of the capturing helper in `crates/ocx_lib/src/forge/git_command.rs` is right.**
Three checks. (a) `crates/ocx_lib/src/utility/child_process.rs` is 135 lines and holds only
`exit_code_from_status` and `propagate_exit_code`; its module doc (`:8-13`) states the spawn
primitives moved to `launch/child_process.rs`. DV-1's premise holds. (b)
`crates/ocx_lib/src/launch/child_process.rs` offers `exec` (diverging) and `spawn_and_wait`, and
`spawn_and_wait` inherits stdio (`:220-223`) — there is **no** capturing primitive to reuse, so the
helper is genuinely new code, not a reinvention. (c) The tempting alternative — a shared capturing
helper in `utility/` reused by `codesign.rs`, `host_capabilities.rs`, `setup/profiles.rs` and the
new git path — would be **worse**: `SPAWN_ALLOWED` names *files that spawn*, so collapsing four
named rows into one anonymous `utility/` row converts a per-site firewall into a blanket hole. The
plan's placement keeps the firewall's design intact and pays one honest allowlist line, exactly as
`SPAWN_ALLOWED`'s own doc comment (`launch.rs:906-912`) prescribes.

**`crates/ocx_lib/src/claim.rs` + `claim/{request,error,root,owners}.rs` is the right layout.**
It mirrors `announce.rs` + `announce/{error,pipeline,request}.rs`, and mirrors it *better*:
`announce/pipeline.rs` is 2040 lines, which is the monolith `arch-principles.md` forbids, and the
claim split has four named concepts instead. A crate-root `claim.rs` matches `announce.rs` and
`activation.rs`. No `mod.rs` anywhere. Correct as written.

**`crates/ocx_cli/src/options/forge_write.rs` matches the `options/` convention.** `options.rs`
is a pure `mod` + `pub use` hub with 23 sibling modules; `options/lazy_mode.rs` is the reference
shape (`#[derive(clap::Args, Clone, Debug, Default)]`, flattened by consumers, resolution behind a
method rather than a public field). One caveat carried into F-25.

**C-047's "no second serializer" is grounded.** `oci::index::serialize_root` exists
(`oci/index.rs:24`, implemented in `wire_writer.rs:59`), takes a `serde_json::Value`, and
`crates/ocx_lib/Cargo.toml:61` enables `serde_json`'s `preserve_order` — so insertion-order field
emission is real and C-047's ordering contract is achievable through the existing emitter.
`parse_physical_repository` (C-057) likewise exists at `oci/index.rs:18`.

**Every factual citation in the plan checks out.** `utility/child_process.rs` is 135 lines;
`deprecated.rs` is 33; `SPAWN_ALLOWED` is at `launch.rs:914`; `no_process_spawn_outside_launch` is
at `launch.rs:1114`; `ExitCode::UnsupportedKeyBackend = 85` is at `cli/exit_code.rs:93` and 86 is
genuinely the first free slot; `AnnounceOutcome` at `announce/request.rs:115-141` carries neither
`branch` nor `capability_checks` (C-055 "verified absent today" is true).

**Verification honesty: one candidate phrase, not a violation.** DV-5's "it is expected to pass" is
a prediction about an unrun release gate, stated alongside a named proof mechanism and a negative
control — not a claim of having verified something. No banned phrase appears anywhere else in the
plan, and no unevidenced "verified" was found.

**The batched-window carve-out is applied correctly.** `announce --package` → positional joins the
already-open 0.6 → 0.7 window rather than opening a second one, which is what `CLAUDE.md`'s "one
window per release pair, not one per rename" requires. Worth adding, for the record, the file count
that justifies invoking the carve-out at all (`CLAUDE.md` gates it on a rename "reaching dozens of
files across docs, tests and downstream repos") — the docs, three acceptance modules, and the
mirror fleet's publisher CI plausibly clear it, but the plan asserts rather than counts.

**"Never edit `CHANGELOG.md`" is honoured.** The plan states the rule explicitly and routes every
release-note line into a commit subject, with `!` marked on the three breaking ones. Correct.

---

## Summary

| ID | Severity | Title |
|---|---|---|
| F-01 | Block | `ClaimError` never reaches the exit-code classifier — `cli/classify.rs` in no WP |
| F-02 | High | C-041 mints a second backoff schedule; `forge/poll.rs` already yields `1,2,4,8,15` |
| F-03 | High | `OCX_ANNOUNCE_GIT_TOKEN` in `CREDENTIAL_KEYS` breaks the ocx-mirror plugin path |
| F-04 | High | git-version gate's accept boundary untested; `Version::Ord` inverts it |
| F-05 | High | `capability_checks_never_empty` is a self-consistent green |
| F-06 | Warn | `from_exit_code_is_exhaustive_without_wildcard` is a self-matching guard |
| F-07 | Warn | Two spawn-allowlist tests duplicate two that already exist in `launch.rs` |
| F-08 | Warn | C-007's clock guard names no comment-strip and no self-exclusion |
| F-09 | Warn | WP-8 puts thirteen contracts in one file |
| F-10 | Warn | WP-5 is two packages; the cut shortens the critical path |
| F-11 | Warn | No `#[non_exhaustive]` policy for the new error or internal enums |
| F-12 | Warn | No error-message style / `#[source]` contract for the new variants |
| F-13 | Warn | Redaction and injection can agree on a wrong base64 |
| F-14 | Warn | C-062's "all of it lives in `deprecated.rs`" is not achievable |
| F-15 | Warn | C-059's arg-id set must be derived from the struct |
| F-16 | Warn | Windows arm of C-035 has no reachable test |
| F-17 | Warn | Hand-rolled chunked decoder, no library search, no negative test |
| F-26 | Warn | `Forge` grows to 13 methods; C-031's transport refusal is an unremarked backstop |
| F-18 | Suggest | `cases.len() == 16` is a fourth edit C-004 does not name |
| F-19 | Suggest | `forge/api.rs`'s ownership sentence goes stale, not only its count |
| F-20 | Suggest | `check_status_has_no_failed_variant` asserts almost nothing |
| F-21 | Suggest | `test_each_secret_form_proved_red_then_green` names a process |
| F-22 | Suggest | WP-3 below the overhead floor; WP-16's `Verify: full` buys no signal |
| F-23 | Suggest | The two-refspec control controls the fixture, not the assertion |
| F-24 | Suggest | Doc-script filename misses the `<section>__<page>` binding convention |
| F-25 | Suggest | `options.rs` carries WP-numbered comments from a different plan |
| F-27 | Suggest | Checked-and-correct record (DV-1 placement, `claim/` layout, citations, honesty) |

**1 Block · 4 High · 13 Warn · 9 Suggest**
