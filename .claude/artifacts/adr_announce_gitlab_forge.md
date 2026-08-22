# ADR: GitLab as a peer forge for `ocx package announce`

## Metadata

**Status:** Accepted
**Date:** 2026-08-22
**Deciders:** Michael Herwig (owner)
**Tech Strategy Alignment:**
- [x] Follows the Golden Path in `product-tech-strategy.md` — Rust/Tokio, the
      existing `reqwest` client, no new infrastructure. One new direct
      dependency (`percent-encoding`), already present in the tree as `url`'s
      own dependency.
**Domain Tags:** integration, security, api
**Supersedes:** N/A
**Amends:** `adr_announce_publisher_surface.md` (the forge is now a trait, not a
concrete client); `design_spec_announce_initiative.md` S1/S3/S7 (see D0)
**Code:** `crates/ocx_lib/src/forge/api.rs` (`Forge`, `CommitBase`,
`BranchComparison`, `RefUpdate`), `forge/gitlab.rs` (`GitLabForge`),
`forge/kind.rs` (`ForgeKind`), `forge/identity.rs` (both parent guards),
`forge.rs` (`RepoCoordinate`, `ForkIdentity`), `crates/ocx_cli/src/command/package_announce.rs`
(`--forge`), `test/tests/fake_gitlab.py`, `test/tests/test_announce_gitlab.py`

## Context

`ocx package announce` writes a package's rebuilt index entry into the index
repository and opens a pull request. Until now it spoke only GitHub REST, by an
explicit v1 decision (design register S7, "GitHub-only v1"; S1 additionally
dropped every GitLab code path from the grimoire donor).

That closed the door on any publisher whose index lives on GitLab, and on every
self-hosted instance of either forge. It also left one structural problem: the
orchestration in `announce.rs` called a concrete `GitHubForge`, so a second forge
could only have arrived as branching *inside* that client — which would have put
forge-shaped conditionals through the middle of every C-cell decision the
orchestration holds (C4 concurrency, C6 unchanged short-circuit, C8 fork base,
C15 atomic commit).

The owner's requirement is broader than "add GitLab": GitHub and GitLab, each on
both its canonical host and self-hosted; merge-request creation, adoption of an
outdated open request, self-healing of a spent branch, and a guard against a
fork whose default branch has drifted behind the upstream.

Grimoire (`../grimoire`, same owner) ships a working two-forge announce and is
the parts donor. It is a donor, not a template: several of its choices are
actively wrong for OCX, and this record names which and why.

## Decision

### D0 — S1/S3/S7 amended, not overturned

Three design-register decisions are touched:

- **S7 ("GitHub-only v1")** is spent. GitLab is a peer implementation.
- **S1 ("REST only, no git subprocess")** stands unchanged and applies to both
  forges. The donor clones, commits and pushes with `git`; OCX does not.
- **S3 ("always fork")** keeps the narrowing already recorded in
  `announce.rs` — "always a **reviewed pull request**", from a fork or from a
  branch on the index itself. Unchanged by this ADR.

### D1 — The forge is a trait, and the orchestration never learns which one

`Forge` (`forge/api.rs`) is exactly the ten operations `announce()` drives.
`GitHubForge` and `GitLabForge` implement it; `announce()` takes `&dyn Forge`.

The alternative — an enum threaded through the orchestration — was rejected on
one ground: every C-cell decision would then need re-deciding per forge at the
point of use, and the first time the two disagreed the disagreement would be
silent. With a trait, a forge that cannot hold the contract must return an error
rather than approximate it, and the contract has one written statement (the trait
docs) rather than two implementations to compare.

`async-trait` is used, matching `IndexImpl`: the trait is consumed as `&dyn` at
one call site per run, so `async fn` in trait (not `dyn`-compatible) buys nothing.

### D2 — Coordinates are `[HOST/]NAMESPACE/PROJECT`

`RepoCoordinate` was `{owner, repo}` and explicitly rejected a second slash. It
becomes `{host: Option<String>, namespace: String, project: String}`.

- **The namespace may nest.** GitLab groups nest arbitrarily
  (`acme/platform/tooling/index`); a two-segment type makes those index
  repositories unaddressable. The type is forge-neutral and permissive, and
  GitHub — where organizations do not nest — refuses a nested namespace itself,
  in `require_flat_namespace`, before any request. The rule lives at the forge
  that has it.
- **The host is optional**, `None` meaning the forge's canonical host.
- **A leading segment is a host by the rule OCI identifiers already use** —
  contains `.` or `:`, or is `localhost` — now exported as
  `oci::identifier::segment_is_host` and called from both places rather than
  copied. Two spellings of "that looks like a host" would drift the first time
  either was relaxed.
- **A host is recognised only when a `namespace/project` remains after it.**
  `host.example.com/owner` is therefore a two-segment path, not a host with no
  project. A host-shaped namespace is legal on both forges, so this is a real
  path rather than a near-miss worth rejecting.

**Interface impact.** `--index-repo` and `--fork` accept the longer form. The old
two-segment form parses to the same value, so nothing that works today stops
working. The changelog line is the commit subject, per the repository rule.

### D3 — The forge kind is declared for a self-hosted host, never probed

`ForgeKind::from_host` recognises exactly `github.com` and `gitlab.com` (and
"no host", which is the default index and therefore GitHub). Anything else
requires `--forge github|gitlab`, and refuses with a message naming the flag.

Probing was rejected. No unauthenticated request distinguishes the forges
reliably; hostnames carry no convention (`git.example.com` is equally likely to
be either); and a wrong guess sends the announce credential to the wrong API **in
the wrong header** — a GitLab `PRIVATE-TOKEN` shipped to a GitHub-shaped endpoint
is a credential disclosed to a host that was never meant to see it. Every
surveyed tool that supports both forges makes the operator declare the platform
for a self-hosted host.

An explicit `--forge` overrides even a canonical host, since an instance can sit
behind a reverse proxy whose name looks canonical, and the operator's word beats
the heuristic.

**Base URLs.** GitHub.com serves a dedicated API origin (`https://api.github.com`)
while Enterprise Server serves `/api/v3` on the instance itself; GitLab serves
`/api/v4` on the instance in both cases, so gitlab.com is not a special case
beyond the default hostname. Always `https`.

### D4 — Concurrency on GitLab is `last_commit_id`, and the donor's force-push is refused

C4 requires that a concurrent announce be **preserved, never overwritten**. On
GitHub that is a fast-forward-only ref update — a genuine compare-and-swap.
GitLab has no ref-level equivalent.

It has `last_commit_id` on a file action: the commit is refused when that file's
last commit is not the one the editor based on. Every announce commit rewrites
the package root, so the guard applies to exactly the file whose staleness
matters, and it is a stricter check than a ref CAS (a branch advancing without
touching the root is not a conflict, and is correctly allowed).

Two details the implementation depends on:

- `last_commit_id` is the **file's** last commit, not the branch head. Passing a
  branch sha would reject every commit whose head did not happen to touch that
  file. The value is read from the non-raw files endpoint at the base ref.
- A rejection must surface as `ForgeError::NonFastForward` so `announce()`'s
  existing re-read-and-regenerate retry fires unchanged. `is_stale_base`
  classifies it, scoped to 400 — the same body on a 5xx is a forge fault, and
  reading it as a lost race would send the caller into a regeneration that cannot
  help.

**Rejected: the donor's approach.** `grim` pushes the announce branch with
`git push --force` (`index_announce.rs:566`) on both forges. That silently
discards a concurrent announce. `test_a_concurrent_announce_is_unioned_not_clobbered`
is red against exactly that behaviour.

`RefUpdate::Reset` — the deliberate rewrite of a spent branch — maps to GitLab's
`force: true` with `start_sha`, the one sanctioned force, and only where the
GitHub path already forces.

### D5 — `CommitBase` carries the base repository, which is the stale-fork guard

`commit_files` took a base **sha**. It now takes `CommitBase { repo, sha }`.

The sha alone is not enough, and the gap is the owner's stated hazard: a fresh or
rebuilt announce branch must start from the **upstream** index's default branch,
never from the fork's own copy of it, which on a long-lived fork is routinely far
behind. Basing there would re-propose content the index already merged, and the
merge request would conflict on the very file every announce edits.

The orchestration already read the base sha from the upstream; naming the
repository alongside it makes that explicit at the trait boundary and gives
GitLab the `start_project` it needs to reach an object outside the target
project. `test_a_spent_branch_is_rebuilt_on_the_upstream_head_not_the_forks_own`
asserts the resulting commit's parent is the upstream head, with the fork's own
`main` deliberately left behind so the two cannot be confused.

### D6 — `sync_fork` is a documented no-op on GitLab

On GitHub, an announce commit parents off a sha read from the upstream but is
written to the fork, so the object reaches it only through the shared fork
network; a fork far enough behind makes those writes fail. `sync_fork` lands the
base object in the fork's own history first.

GitLab's commits API names its source project explicitly (`start_project`), so
reachability is the server's problem and there is no race to pre-empt. The method
returns after a debug line. Making it a no-op — rather than removing it from the
trait — keeps the GitHub path's mitigation where it belongs and states, at the
implementation, why the other forge does not need it.

### D7 — Fork identity: parent verified by immutable id, existing fork found by enumeration

Both guards from the donor survive, sharpened:

- **Parent verification** (both create and reuse paths) refuses a project at the
  conventional fork path that is not a fork *of the upstream* — a same-named
  stranger, which would otherwise receive the announce branch and anything the
  write carries. GitHub compares `parent.full_name` case-insensitively; GitLab
  compares `forked_from_project.id`, which is immutable and therefore strictly
  stronger than a path a rename can change. A missing answer is a refusal.
- **Identity is read, never composed.** Every field comes from the response
  body's own path, so a renamed or nested fork resolves to where it really is.
- **The 409 reuse path enumerates** `GET /projects/:id/forks?owned=true` and
  matches the namespace, then re-verifies the parent from the project's own
  document. The donor guesses `{username}/{basename}` here and fails for a
  renamed, group-hosted, or concurrently-created fork — after the packages are
  already published.
- **Namespace comparison is on the whole path**, not its root: a fork at
  `acme/other-group/index` is not the fork requested at `acme/index`.

### D8 — A self-fork is refused by name

Forking into the namespace that already owns the upstream is impossible on every
forge. GitHub answers with an opaque 403; GitLab answers 409, whose reuse path
would then spend the whole enumeration budget hunting a fork-of-itself. Both
clients refuse up front with `SelfForkRefused`, whose message names the path that
actually works ("omit `--fork`"). The comparison is ASCII-case-insensitive, since
one half is spelled by the publisher and the other comes from the API.

### D9 — Errors carry the forge's reason

`ForgeError::Status` gained a `detail`: the response body, trimmed and capped at
300 characters, empty when there is nothing to say. A bare status code sends the
reader to the forge's web UI to discover what a 422 or a 400 meant — and on
GitLab the *entire* distinction between a stale compare-and-swap and an ordinary
validation failure lives in that body. Adopted from the donor's `status_error`.

### D10 — Acceptance: two surfaces, one object graph

The fake forge serves GitHub under `/repos/...` and GitLab under `/projects/...`
from one process over **one** in-memory git object graph.

Two independent fakes would let each client drift into agreeing with its own
fixture and with nothing else. One graph makes `test_both_forges_commit_the_same
_root` a real comparison of what each client committed. Everything the clients
depend on is modelled: numeric-id and encoded-path addressing, per-file
`last_commit_id`, `start_project`/`start_sha`/`force`, comparison as a directed
commit list, and asynchronous forks reported through `import_status`.

## Consequences

**Positive**

- A publisher on GitLab — or on a self-hosted instance of either forge — can
  announce, which was previously impossible.
- Nested GitLab group paths are addressable; the coordinate grammar widens
  without breaking the existing form.
- The concurrency guarantee is now stated once in the trait and proved per forge,
  rather than being an artefact of the GitHub API's shape.
- Three fixes land for both forges at once: status errors carry a reason, a
  self-fork is refused by name, and the commit base names its repository.

**Negative / risks**

- **GitLab's stale-file rejection is classified from a message substring.**
  GitLab reports it as a 400 whose body names the condition; there is no
  machine-readable code. The classifier is scoped to 400 and pinned by a test
  shown red before green, and an unrecognised 400 is a hard error rather than a
  retry — but a GitLab rewording would degrade the CAS to a hard failure (safe:
  it fails loudly, it does not clobber).
- **The GitLab commit path costs one existence probe per file** (GitLab has no
  upsert, so `create` versus `update` must be known before the batch). That is
  the root plus a small number of CAS objects per announce.
- **`--forge` is required for self-hosted hosts**, which is friction the probe
  would have avoided — deliberately traded for not disclosing a credential to a
  misidentified host.
- **No live GitLab end-to-end run was executed.** `glab` is not installed on this
  machine and no GitLab credential is available here; the coverage is the shared
  fake plus the mutation proof. A live harness is a follow-up.

## D11–D16 — decided in adversarial review

Six defects were found by the review panel (a cross-model adversary plus a
targeted docs and architecture pass) after the implementation was verified
green. Each is recorded here because the fix changed a contract, not just a
line.

### D11 — the coordinate host is validated, and a malformed one is refused

`RepoCoordinate::from_str` recognised a leading host by shape alone
(`segment_is_host`: "has a dot or a colon"), then interpolated it straight into
the API base URL that carries the credential. `gitlab.com@evil.example/acme/index`
therefore built `https://gitlab.com@evil.example/api/v4`, whose URL *authority*
is `evil.example` — the token would have been sent there. The coordinate comes
from the operator's own command line, so this is hardening rather than a remote
attack, but a CI script interpolating a variable into `--index-repo` is exactly
the shape that turns it into one.

A segment that looks like a host and is not a well-formed one is now **refused**,
never demoted to a namespace segment. Failing closed matters: reinterpreting it
would silently address a different repository. Ports are validated through
`u16::from_str`, which a digits-and-length check would have let `80443` past.

### D12 — the root is read at a resolved commit, never through a ref name

The committed root was read via `get_file_contents(index, path, "main")` and the
commit base resolved separately via `get_ref_sha(index, "heads/main")`. Between
those two calls `main` can advance, and the commit is then based on a head whose
version of the root it never saw. On GitLab this is worse than a lost update: the
`last_commit_id` guard is evaluated against the *newer* commit, which the client
just read, so the CAS **passes** and the concurrent announce is overwritten.

`read_committed_root` now resolves the ref first and reads the bytes at the
resolved SHA, returning it as `RootRead::base_sha`. The fix is in the shared
announce pipeline, so it holds on both forges. One consequence is deliberate: the
`--out` path now also resolves the base ref, so an index repository that does not
exist fails as "no base ref" rather than as "namespace unclaimed".

### D13 — a compare that cannot classify ancestry fails closed

GitLab publishes no ahead/behind verdict, so `compare_branch` derives one from
two directed commit counts. `ahead_count` read a missing or non-array `commits`
field as **zero**, and two zeroes read as `Identical` — the verdict that
classifies a live branch as spent and rebuilds it on the upstream head,
discarding unmerged work. The `compare_timeout: true` case was already refused;
this was the same hazard one step earlier, in the shape a proxy or an API change
produces. A response that does not carry the documented field is now an error.

### D14 — a malformed invocation exits 64

`ForgeKindUnknown`, `NestedNamespaceUnsupported`, `SelfForkRefused`,
`InvalidRepoCoordinate` and the new `ForkHostMismatch` all fell through to the
sysexits default (exit 1), while the reference documentation claimed 64. Each is
fixed by editing the command line, which is what `EX_USAGE` means, and a CI
wrapper must be able to tell "your flags are wrong" from "the forge said no". The
code was changed rather than the documentation: 64 was the right answer.

Two gaps in the same family closed with it. A **nested `--index-repo` on GitHub**
was never checked — `require_flat_namespace` guarded only the fork coordinate, so
a nested index reached the wire and came back as the bare 404 the rule exists to
prevent; `ForgeKind::validate_coordinate` now applies it to every coordinate a
run names. And **`--fork` on a different host than `--index-repo`** was silently
ignored, because the client is built for the index's host alone — the fork was
addressed on the index's instance, writing to a repository the operator never
named. That is now `ForkHostMismatch`, refused rather than reinterpreted.

### D15 — the credential is redacted out of a forge's error body

`ForgeError::Status` embeds up to 300 characters of the forge's response body so
a 422 says what it meant. That body is forge-controlled text, and a reverse proxy
that reflects request headers can put `PRIVATE-TOKEN` in it; the value is then
rendered by `Display` and logged on the retry path. The cap bounds the volume of
such a leak, not its content. `status_detail` now takes the token and replaces
every occurrence with `[redacted]` before anything else, and `status_error`
became a method so each client can supply its own. The documented promise ("never
logged") is now backed by a guard rather than by the absence of a known path.

### D16 — a dotted top-level group is disambiguated by writing the host out

GitLab group paths may contain dots, so `acme.team/platform/index` is genuinely
ambiguous under a `[HOST/]NAMESPACE/PROJECT` grammar and is read as the host
`acme.team`. This is **not** a silent misroute: `acme.team` is not a forge OCX
recognises, so the run stops and asks for `--forge` rather than sending the
credential anywhere. The grammar is not changed — every disambiguation rule
considered (a sigil, a separate `--host` flag, probing) costs more than the case
is worth. Instead the escape is documented and tested:
`gitlab.com/acme.team/platform/index` names the group `acme.team/platform` on
gitlab.com, and the `ForgeKindUnknown` message now names that escape.

### D17 — a path-prefixed GitLab install is out of scope, and said so

A GitLab served under a path prefix (`https://example.com/gitlab/`) puts its API
at `https://example.com/gitlab/api/v4`. The `[HOST/]NAMESPACE/PROJECT` grammar
has nowhere to put the prefix: `example.com/gitlab/acme/index` parses as host
`example.com`, namespace `gitlab/acme`, project `index` — wrong, and silently so
were it not for the fact that the resulting requests simply 404.

Supporting it means a second input naming the API base, which is a grammar
decision rather than a bug fix, and `glab` itself has the same gap open
(`gitlab-org/cli#7920`, `#8146`). Instances served at the host root — every
standard install — work. The limitation is stated in the command reference
rather than left for someone to discover from a 404.

### Round two — three defects in the round-one fixes

A second adversarial pass over the fix commit found three, two of them created by
the fixes themselves. The third — that the no-redirect structural guard matched
its own assertion string and could never go red — had already been found and
fixed independently by mutation testing before the report arrived.

1. **The host-mismatch guard (D14) refused ordinary invocations.** It compared
   two `Option<String>` hosts directly, but an omitted host *means* the forge's
   canonical host — so `--index-repo ocx-sh/index --fork github.com/me/index`
   names one instance twice and was rejected with a message that contradicted
   itself ("--fork is on github.com but --index-repo is on the canonical host").
   That combination worked before the fix. Comparison now goes through
   `ForgeKind::same_host`, which resolves `None` to `canonical_host()` and folds
   case — the same two normalisations `from_host` and the API base-URL builders
   already apply, so the guard cannot disagree with where requests actually go.

2. **Path segments were still interpolated raw on GitHub.** D11 validated the
   *authority*; the GitHub client interpolates `full_path()` into the URL
   unencoded (GitLab percent-encodes), so `--index-repo 'acme?x=1/index'` parsed
   and addressed `/repos/acme` with the rest as a query string. Segments are now
   restricted at parse time to the character set both forges accept
   (`[A-Za-z0-9._-]`, and never `.`/`..`). One check for both clients rather than
   encoding at sixteen GitHub call sites — sixteen chances to miss one.

3. **The credential check ran before every usage guard**, so a malformed command
   line reported a missing token (80) instead of what was wrong with it (64) —
   sending the operator to set a credential and only then meet the real error.
   Argv faults are now diagnosed first.

### What the review did **not** change

- **The `last_commit_id` CAS design** (D2) survived adversarial review intact;
  the only defect near it was D12's read window, which is upstream of the guard.
- **The no-redirect policy** was confirmed correct by the adversary. It moved
  into `forge/http.rs` because it existed **twice**, byte-identical, in the two
  clients — a security control that exists twice is one that can drift. What
  legitimately differs per forge (`is_retryable`: GitHub also replays a 404 for
  fork propagation, GitLab never does) stayed in each client.
- **URL segment encoding** was found clean: every dynamic project, file, ref and
  branch segment goes through `encode_segment`.

## Links

- `../grimoire/.agents/adr/adr_announce_fork.md` — the donor's fork ADR (D1–D10);
  its D4/D5/D6/D7 guards are adopted, its force-push and its basename fork guess
  are not
- `.claude/artifacts/research_grimoire_announce_port.md` — the original
  copy-and-own inventory
- `.claude/artifacts/research_gitlab_forge_api.md` — the GitLab REST facts this
  implementation was written against
- `.claude/artifacts/design_spec_announce_initiative.md` — S1/S3/S7, C3–C15, X1–X6
- `.claude/rules/quality-security.md` — the credential-leak posture D3 and D9 serve

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-08-22 | Michael Herwig | Initial record, accepted (D0–D10) |
| 2026-08-22 | Michael Herwig | D11–D16 added after adversarial review; D2's CAS design unchanged |
| 2026-08-22 | Michael Herwig | D17 added; late research corrected the `compare_timeout` and CI-job-token claims |
| 2026-08-22 | Michael Herwig | Round-two review: host-comparison regression, raw path interpolation, guard ordering |
