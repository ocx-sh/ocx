# Security review — `plan_index_claim_command.md`

**Reviewer:** security focus, Opus. **Target:** `.claude/artifacts/plan_index_claim_command.md`
(State: plan-approved, tier high). **Baselines opened:** `adr_index_claim_command.md`
§ Security Architecture (1518–1743), § The git recipe (1321–1462), § Validation (1952–2058),
§ NFR (1746–1757); `crates/ocx_lib/src/launch.rs`; `crates/ocx_lib/src/env.rs`;
`crates/ocx_lib/src/launch/child_process.rs`; `crates/ocx_lib/src/utility/child_process.rs`.

**Verdict: 3 Block / 5 High / 5 Warn / 4 Suggest.**

No accepted ADR decision is re-litigated below. Every finding is a **loss in projection** —
something the ADR decided that the plan's contracts, scenarios, work packages or named tests do
not carry.

---

## Audit table A — credential containment, surface by surface

The ADR's promise (§1 `:1536-1537`, NFR `:1750`): the credential never appears in argv, a URL,
`.git/config`, the reflog, shell history, a log line, a forwarded child environment, or a
redacted forge body.

| Surface | Plan contract | WP | Named test | Can that test go red? | Verdict |
|---|---|---|---|---|---|
| argv | C-034 (env transport, never argv) | WP-8 | `test_secret_absent_from_argv_config_url_and_stderr` | yes — recording shim captures argv | **carried** |
| remote URL | C-034 | WP-8 | same | yes | **carried** |
| `.git/config` | C-034 | WP-8 | same | yes | **carried** |
| reflog | — | — | — | — | dropped (see F-15) |
| `git remote -v` | — | — | — | — | subsumed by `.git/config`; not asserted |
| shell history | n/a — `Vec<String>` to `Command`, never a shell (ADR §3 `:1612-1614`) | — | — | — | structurally satisfied |
| stderr / log line | C-019, C-022, C-044 | WP-5, WP-8 | `redactor_masks_all_three_secret_forms`; `test_each_secret_form_proved_red_then_green` | **yes, red-first by name** | **carried, strongest row** |
| forge error body / `CapabilityCheck.detail` | C-011 "`detail` never carries a credential" | WP-5 | **none** | n/a | **F-13** |
| forwarded child environment | C-035 "never passed": `OCX_*`, `CI_JOB_TOKEN`, `GIT_ASKPASS`, `SSH_ASKPASS`, `GIT_CURL_VERBOSE` | WP-8 | **none** (only `GIT_TRACE` has one) | n/a | **F-04** |
| `/proc/<pid>/environ` | — (ADR names it as accepted residual, CWE-522) | — | n/a | n/a | accepted; not carried into the plan (F-15) |

**Three secret forms — all three carried.** C-022 takes a slice and names the API credential,
`OCX_ANNOUNCE_GIT_TOKEN` and the `base64(user:secret)` blob; `redactor_masks_all_three_secret_forms`
(WP-5) plus `test_each_secret_form_proved_red_then_green` (WP-14) prove each red before green.
No finding — this is the part of the plan that is done right.

## Audit table B — the child-environment allowlist, ADR row by ADR row

ADR authoritative table at `:1699-1701`. Plan projection at C-035 (+ C-033, C-034).

| ADR column | ADR item | In C-035's enumeration? | Named test | Verdict |
|---|---|---|---|---|
| passed | `PATH` | yes | — | ok |
| passed | `HOME` / `USERPROFILE` / `HOMEDRIVE` / `HOMEPATH` | yes | — | ok |
| passed | 4 proxy vars, both cases | yes ("in both cases") | `test_ambient_http_proxy_passed_through` | ok |
| passed | `GIT_SSL_CAINFO` / `GIT_SSL_CAPATH` / `SSL_CERT_FILE` / `SSL_CERT_DIR` | yes | — | ok |
| passed | `TMPDIR` / `TEMP` / `TMP` | yes | — | ok |
| passed | `SYSTEMROOT` on Windows | yes | — | ok |
| set | `GIT_TERMINAL_PROMPT=0` | yes (via C-033) | — | ok |
| set | `GIT_CONFIG_NOSYSTEM=1` | yes (via C-033) | — | ok |
| set | `GIT_CONFIG_COUNT` / `KEY_n` / `VALUE_n` | yes (via C-034) | — | ok |
| set | **`GIT_AUTHOR_*`** | **no** | `commit_identity_is_fixed` (indirect) | **F-05** |
| set | **`GIT_COMMITTER_*`** | **no** | `commit_identity_is_fixed` (indirect) | **F-05** |
| set | `LC_ALL=C` | yes (via C-033) | `test_lc_all_c_keeps_classifier_matching` | ok |
| set | `LANGUAGE=` | yes (via C-033) | — | ok |
| never | `GIT_TRACE*` | yes | `test_ambient_git_trace_does_not_reach_child` | ok |
| never | `GIT_CURL_VERBOSE` | yes | **none** | F-04 |
| never | `GIT_ASKPASS` | yes | **none** | F-04 |
| never | `SSH_ASKPASS` | yes | **none** | F-04 |
| never | every `OCX_*` credential | yes | **none** | **F-04** |
| never | `CI_JOB_TOKEN` | yes | **none** | **F-04** |

Nothing was **added** to the allowlist and nothing **moved column**. Two "set by ocx" rows are
**dropped** from the enumeration (F-05).

**`Env::clean()` vs `Env::new()` — the plan's claim is accurate.** Opened `crates/ocx_lib/src/env.rs:464-483`:
`Env::new()` is `vars: std::env::vars_os().map(…).collect()` and `impl Default for Env { fn default() -> Self { Self::new() } }`;
`Env::clean()` is `vars: HashMap::new()`. C-019's "builds the child environment from `Env::clean()`;
`Env::new()` / `Env::default()` are **forbidden** on this path" is correct as written.

---

### F-01 · Block · The request title and body have no contract, no scenario and no test

**Where:** Component contracts (no ID exists), C-039, C-046/C-047, the whole S-001…S-037 table,
the Test inventory.

**Problem:** ADR § Security Architecture 3 (`:1636-1647`) states the invariant as a blockquote:

> The claim and announce request title and body are built from a fixed template carrying only
> structured values — the logical name, the physical repository, the branch, and the resolved
> `login:id` pairs. No operator free text is interpolated. `--upstream-disclaimer` reaches the
> **root file only**, where the serializer escapes it, and never the request title or body.

followed by "Owner logins are rendered as plain `login:id` text, deliberately **without** an `@`,
so the body fires no mentions."

None of the three halves reaches the plan. Grepped the whole plan: `template` — **0 hits**;
`mention` — **0 hits**; `disclaimer` — 2 hits, both structural (C-046's `Upstream { … disclaimer }`
field and C-057's `--upstream-disclaimer` flag). `.title` / `.description` appear exactly once,
at C-039, as push-option **key names**; nothing says how their values are composed. D-C4 rule 2's
third provenance surface ("in the request body") survives only as prose inside S-008, with no
contract and no test.

**Attack or failure path:** WP-8 implements C-039 and needs a title and a description. The only
operator strings in scope are `--upstream-disclaimer` and `--upstream-repository-url`. An
implementer, with no contract forbidding it, renders `.description` as "claim for `acme/widget`
… upstream: <org> — <disclaimer>" because it reads as helpful context for the G-04 reviewer. The
disclaimer is free operator text; it lands in a markdown body a human merges. Two consequences,
both reachable: (a) markdown/HTML injected into the artifact the governance reviewer reads —
`[looks like the real repo](https://evil)` — which is precisely the control §3 exists for; (b) if
owners are ever rendered with an `@`, every claim pings unrelated forge accounts. The control-char
check in C-039 does not catch either: `[x](https://evil)` contains no control character.

**Fix:** add a contract to WP-9 (the renderer owns it, beside C-047):

> **C-0NN** — the claim and announce request title and body are a fixed template over structured
> values only (logical name, physical repository, branch, resolved `login:id` pairs,
> `owner_identity_source`). No operator-supplied string — in particular `--upstream-disclaimer`,
> `--upstream-repository-url` and `--upstream-org` — is interpolated into either. Owners render
> as bare `login:id`, never `@login`.

Add two named tests: `request_body_is_a_fixed_template` (WP-9 unit, asserting the body over a
`ClaimRequest` carrying a disclaimer containing `[x](https://evil)` and `@alice` — and asserting
neither substring appears) and `::test_disclaimer_reaches_root_not_request_body` (WP-13). Add the
disclaimer-in-body case to the Edge-case hunt's **Input** bullet.

---

### F-02 · Block · The push-option assertion is count-only; the ADR mandates count **and** exact key set

**Where:** C-039; Test inventory WP-4 / WP-8 / WP-14; S-013.

**Problem:** ADR git recipe `:1399-1405`, verbatim:

> **The push-option set is closed: exactly these four keys, and no others.** This is a security
> boundary, not a style preference. `merge_request.merge_when_pipeline_succeeds` … would auto-merge
> the claim request the moment its pipeline went green — defeating G-04's "a first claim is never
> auto-merged", the single governance control this whole command is built around. … Both are
> **forbidden**, and the fixture's `post-receive` hook asserts `GIT_PUSH_OPTION_COUNT == 4`
> **and the exact key set** rather than merely parsing what arrives.

The plan carries the contract half correctly — C-039 names both forbidden options and the
control-character rejection. It loses the **key-set** half in every test it names:
`push_carries_exactly_four_options` (WP-8), `test_push_delivers_four_options` (WP-4),
`::test_push_carries_exactly_four_options` (WP-14), and S-013's error case "fifth push option
present → the fixture's hook fails the test". All four are stated as a **count**.

**Attack or failure path:** a refactor of the option renderer swaps `merge_request.description`
for `merge_request.merge_when_pipeline_succeeds` — a plausible "help the pipeline along" change,
and one an LLM-written follow-up could make. `GIT_PUSH_OPTION_COUNT` is still 4. Every named test
stays green. The claim request auto-merges the moment its pipeline passes, and G-04 — the single
governance control the whole command exists to respect — is bypassed with no test red anywhere in
the tree. No test in the inventory ever names `merge_when_pipeline_succeeds`, so nothing else
catches it either.

**Fix:** restate the fixture assertion in C-039 and in WP-4's hook contract as **count and exact
key set**: the hook asserts `GIT_PUSH_OPTION_COUNT == 4` and that the sorted set of
`GIT_PUSH_OPTION_{0..3}` keys equals `{merge_request.create, merge_request.target,
merge_request.title, merge_request.description}`. Rename the tests to say so
(`push_carries_exactly_the_four_option_keys`). Add one explicit refusal test,
`::test_merge_when_pipeline_succeeds_is_never_sent`, whose red control is a fixture push that
injects the option — so the assertion has a demonstrated red state.

---

### F-03 · Block · `-c http.followRedirects=false` is scoped to the clone; the ADR puts it on **every** invocation, including the credential-bearing push

**Where:** C-033 ("clone hygiene, all mandatory: … `http.followRedirects=false` …"); C-034; the
git-workspace WP-8 test list.

**Problem:** ADR git recipe `:1337-1341`, immediately before the numbered recipe:

> Every invocation below carries `-c http.followRedirects=false`; every invocation **that injects
> an ocx credential** additionally carries `-c credential.helper=` and the `GIT_CONFIG_*`
> credential pair. … These are elided from the listing for readability and are **not optional**
> (see Security Architecture §1 and §4).

The ADR deliberately gives the two flags **different scopes**: redirects off everywhere, helper
reset only where ocx injects. The plan preserves the scope distinction for `credential.helper=`
(C-034, correctly and with both halves — see the item-2 note below) and loses it for
`followRedirects`: the flag appears exactly once in the plan, inside C-033, a contract whose own
first words are "**clone hygiene**". Steps 4r (the retry fetch) and 5 (the push) are governed by
C-036/C-043 and C-039/C-040, none of which mentions it. No test names it.

**Attack or failure path:** git's default is `http.followRedirects=initial`, so the initial
request of the push **is** followed. The push is the one invocation that carries
`Authorization: Basic <base64(user:secret)>` as an `http.<prefix>.extraHeader`. A custom header set
this way is re-sent by curl on a followed redirect — this is the redirect leg of the Clone2Leak
class the ADR cites at `:1544-1547` as the reason `http.extraHeader` was chosen at all. A
compromised or misconfigured index host (or anything that can answer for it: a corporate MITM
proxy the allowlist deliberately honours, a stale DNS entry, an operator's own
`url.<x>.insteadOf`) answers the `git-receive-pack` POST with a 302 to a host it controls and
receives the push credential in cleartext-decodable base64. This compounds with **F-12**: nothing
tests that the header prefix is path-scoped either, so the "never just the host" mitigation is
also unenforced.

**Fix:** move the flag out of C-033 into its own contract with the ADR's scope, e.g.
"**C-0NN** — every git invocation, without exception, carries `-c http.followRedirects=false`;
the credential pair and `-c credential.helper=` are carried only by invocations that inject an
ocx credential." Add `every_git_invocation_carries_no_redirects` (WP-8 unit over the rendered argv
of each recipe step) and `::test_push_does_not_follow_a_redirect` (WP-14, the fixture answering
the receive-pack POST with a 302 to a second recording endpoint that asserts it saw no
`Authorization` header).

---

### F-04 · High · No named test asserts the child environment block against the allowlist — the ADR's own Validation item names the assertion the plan drops

**Where:** C-035 (WP-8); C-019 (WP-5); Test inventory WP-5 and WP-14.

**Problem:** ADR Validation `:1999-2002`:

> **Child-environment assertions** (same shim): `GIT_TRACE`/`GIT_CURL_VERBOSE`/`GIT_ASKPASS` and
> **every `OCX_*` credential absent from the child**; an ambient `GIT_TRACE` set in the parent
> does **not** reach the child (the `Env::clean` requirement, **proved red by constructing with
> `Env::new()`**) …

The plan's WP-14 carries three of that item's assertions —
`::test_ambient_git_trace_does_not_reach_child`, `::test_lc_all_c_keeps_classifier_matching`,
`::test_ambient_http_proxy_passed_through` — and drops the credential-absence half entirely. Per
audit table B: 2 of the 19 allowlist rows have a named test, and **neither of the two rows that
carry a secret** (`every OCX_* credential`, `CI_JOB_TOKEN`) is one of them. The plan's only other
env test, `git_child_env_is_built_from_clean`, sits in **WP-5** over `forge/git_command.rs`, while
C-035 — the allowlist itself — is scoped to **WP-8** over `forge/git_workspace.rs`. So the seed is
tested in one package and the contents are tested in neither. The ADR is explicit that this is the
one invariant with no structural backing: "**The invariant is caller-enforced and asserted by
test, not delivered by the structure**" (`:1583-1584`).

**Attack or failure path:** WP-8 builds the env by starting from `Env::clean()` (WP-5's test stays
green) and then, for convenience on the fetch, copies through a small set of ambient variables with
a prefix loop — `for (k, v) in ambient { if k.starts_with("GIT_") || k.starts_with("HTTP") { … } }`
— which is the shape a developer reaches for when the explicit list is 15 entries long. That loop
passes `GIT_ASKPASS` (a helper-shaped credential path) and, on a slightly wider variant,
`CI_JOB_TOKEN` and `OCX_ANNOUNCE_TOKEN`. Every named test stays green. On a shared runner the git
child, and anything it spawns (`GIT_ASKPASS`, a pager, a hook), now sees the operator's API token
in its environment — the exact "forwarded child environment" the NFR promises against, and the
`/proc/<pid>/environ` residual widened from one derived value to the raw tokens.

**Fix:** add to WP-14 `::test_child_env_matches_the_allowlist`, driven off the recording shim's
captured environment block: assert the child's key set is a subset of the allowlist, and assert
by name that `OCX_ANNOUNCE_TOKEN`, `OCX_ANNOUNCE_GIT_TOKEN`, `CI_JOB_TOKEN`, `GIT_CURL_VERBOSE`,
`GIT_ASKPASS` and `SSH_ASKPASS` are absent while all three are set in the parent. Carry the ADR's
red proof verbatim into the Red/green discipline section: the mutation is **constructing with
`Env::new()`**, which must turn this test red. Move C-035 and its test into the same work package
as the code that builds the environment, or state in WP-8's notes that it owns the test for a
contract WP-5's helper enforces.

---

### F-05 · High · C-035's allowlist omits `GIT_AUTHOR_*` / `GIT_COMMITTER_*`, which the ADR's table lists in the "Set by ocx" column

**Where:** C-035 vs ADR `:1701` (audit table B, rows 10–11); C-045.

**Problem:** C-035 defines the child environment by enumeration — "the ocx-set variables of C-033
and the credential pair of C-034 — set". C-033's list is `GIT_TERMINAL_PROMPT`,
`GIT_CONFIG_NOSYSTEM`, `LC_ALL`, `LANGUAGE` (plus three `-c` flags and the tempdir mode); C-034's
is `GIT_CONFIG_COUNT`/`KEY_n`/`VALUE_n`. Neither names `GIT_AUTHOR_*` or `GIT_COMMITTER_*`, which
the ADR's authoritative table puts squarely in the middle column and which the recipe reinforces
at `:1457-1461`: "injected explicitly (a minimal CI image has no `user.*` config and the commit
would fail outright). A fixed `ocx <noreply@ocx.sh>` … must not track `--owner`". They survive in
the plan only at C-045, in the Git-workspace block, disconnected from the allowlist that defines
what the child may see.

**Attack or failure path:** WP-8 implements C-035 literally, because C-035 is written as *the*
allowlist and F-04's missing test means nothing contradicts it. The four identity variables are
not set. `commit-tree` then falls back to config — and `HOME` **is** passed through, so on a
developer's machine it silently picks up the operator's `user.name`/`user.email` from
`~/.gitconfig`. The commit in the governance artifact is now authored by whoever ran the command
rather than by `ocx <noreply@ocx.sh>`, which is the identity-provenance property D-C4 and the
STRIDE Repudiation row rest on; on a bare CI image with no `user.*` it instead fails the whole
run at `commit-tree` with a git error the classifier has no arm for (exit 1). `commit_identity_is_fixed`
would catch the first case only if it runs with a populated `~/.gitconfig`, which is not stated.

**Fix:** add `GIT_AUTHOR_NAME`/`GIT_AUTHOR_EMAIL`/`GIT_COMMITTER_NAME`/`GIT_COMMITTER_EMAIL` to
C-035's "set" clause explicitly rather than by reference to C-033/C-034, and cross-reference C-045.
State in WP-8's notes that `commit_identity_is_fixed` must run with a `~/.gitconfig` carrying a
*different* identity, so the assertion discriminates between "ocx set it" and "git read it from HOME".

---

### F-06 · High · WP-16's scope says "three cross-repo issues"; the closeout says five and the table lists six — and two of the six are security controls the ADR leans on

**Where:** Work-package table WP-16 row; § Cross-repository closeout (WP-16) header and table;
§ Scope / Out of scope.

**Problem:** three counts, three different numbers, in one plan:

- WP-16's Scope cell: "blobless-clone measurement, partial-clone proof, **three cross-repo issues**, the two issue-body posts"
- Closeout section header: "**Five issues** and two posts, none of which can land in this repository"
- The closeout table: **six** issue rows (Corporate-CA REST client, ocx-mirror transport fields, ocx-catalog owner href, indexbot dual-emit stop date, `actor_id` governance, Reviewer checklist) plus one posts row.

Two of the six are not housekeeping. **Reviewer checklist** is the STRIDE **Spoofing** row's stated
control — ADR `:1524`: "The real control is posture (c) plus the **G-04 human reviewer reading the
rendered `login:id` list**", and the plan's own row says "The STRIDE Spoofing row leans on that
control. If absent, open an issue to add it." **Corporate-CA REST client** is the §2 unfixed-gap
follow-up the ADR calls out at `:1602-1608` as the thing that must not be described as fixed.

**Attack or failure path:** WP-16 runs against its own Scope cell — the field a merge check reads —
files the three most obviously code-adjacent issues (ocx-mirror, ocx-catalog, corporate-CA, or
whichever three the executor picks), and closes. The reviewer-checklist verification is never done.
The plan then ships a spoofing mitigation whose only real control is a checklist item nobody
confirmed exists, and the ADR's own instruction to *verify during execution whether
`governance-contracts.md` already requires checking `owners[]` against forge profiles* is silently
skipped. Nothing reds: WP-16 is a release gate with no test.

**Fix:** correct WP-16's Scope cell to the table's actual six issues plus two posts, and mark the
Reviewer-checklist and Corporate-CA rows as **release-blocking**, not best-effort. Add both to the
Release gates list (which currently has five numbered gates, none of which is an issue filing), so
they inherit the "before 0.6.1 ships" bar.

---

### F-07 · High · The `credential.helper=` negative case can pass vacuously — the pair is split across two tests with independent setup

**Where:** C-034; WP-14 `::test_no_helper_invoked_on_injecting_run`, `::test_helper_invoked_on_step_three_run`.

**Problem — first, the part that is right.** The scope question in item 2 is carried correctly:
C-034 says the reset rides "**every invocation that injects an ocx credential and only those** —
under push-credential precedence step 3 nothing is injected and the operator's own helpers stay in
charge", and both halves are named tests. The Edge-case hunt's **Environment** bullet names "a
credential helper present and a run that must not invoke one, **plus its complement that must**".
That is the ratified-fallback trap closed. No finding on scope.

The defect is in how the pair is proved. ADR `:1561-1563` describes **one** fixture with two cases:
"The fixture therefore has two cases — a credential-injecting run with no helper invocation, and a
step-3 run where the helper *is* invoked and no `extraHeader` is present." The plan splits them
into two pytest functions and says nothing about shared setup, and drops the "no `extraHeader` is
present" half of the complement entirely.

**Attack or failure path:** `::test_no_helper_invoked_on_injecting_run` asserts "the recording
helper shim was not executed". If that test's `HOME` fixture never wrote a `credential.helper`
into `~/.gitconfig` — the easy omission, because the injecting run is *supposed* not to use one —
the assertion is true in every state of the code, including with `-c credential.helper=` deleted.
That is the repo's own Block-tier "green that cannot be told from never-ran"
(`quality-core.md` § Unchecked Green). The complement test does not rescue it: it is a separate
function with a separate fixture, so it proves the shim *can* fire under *its* setup, not under the
first test's. Deleting `-c credential.helper=` from the implementation then leaves the entire
credential-helper CVE class (the reason `http.extraHeader` was chosen at all, ADR `:1539-1548`)
re-admitted with a green suite.

**Fix:** state in C-034 and in WP-14's notes that both cases share **one** fixture `HOME` with a
configured recording `credential.helper`, and that the injecting case is proved red by removing
`-c credential.helper=` from the argv under test. Extend `::test_helper_invoked_on_step_three_run`
to assert the ADR's other two halves: no `extraHeader` in the child's `GIT_CONFIG_*` block, and
`push_credential_kind: "git-helper"` in the report.

---

### F-08 · High · Widening `SPAWN_ALLOWED` surrenders the firewall's only real property, and the Constitution-deviations row rejects an alternative that is not the one available

**Where:** DV-1; § Constitution deviations (the single row); C-021; WP-5.

**Ruling on the question asked: the widening is defensible on its stated ground and
under-argued on the one that matters.** The ground is sound — `git hash-object` is not a tool
launch, and `SPAWN_ALLOWED`'s own doc (`launch.rs:906-913`) says exactly that: "Every entry below
spawns a program *ocx itself* chose for its own purposes, so none of them is a tool launch". A
twelfth row beside `codesign.rs`, `host_capabilities.rs` and `setup/profiles.rs` is in-family. And
DV-1's premise checks out against HEAD: I opened `utility/child_process.rs` (135 lines) and it
holds only `exit_code_from_status` / `propagate_exit_code`, with its own module doc saying "The
spawn primitives themselves are **not** here: they live in `launch/child_process.rs` as a private
submodule of [`crate::launch`]".

**Problem:** the deviations row is not honest about what is given up, and the alternative it
rejects is not the alternative that exists. `launch.rs:40-46`:

> Those two are **searches over source text**, not proofs. **Privacy is what actually holds**:
> `launch::child_process` is unreachable from outside this module, so no other file can call the
> primitives it wraps. What the searches add is catching a file that builds its *own* `std`/`tokio`
> `Command` — for the spellings `SPAWN_TOKENS` recognises. **A spelling nobody anticipated would
> pass them**, so read the claim as "the primitives are private and the common escapes are caught",
> never as "spawning outside the seam is impossible".

The row argues only against "routing the git child through `Launch`" — the recording type, which
would indeed mint false execution records for `git hash-object`. It never evaluates the option
that keeps the strong property: **adding a capturing primitive to the private
`launch/child_process.rs` submodule and exposing a narrow non-recording entry point from `launch`**.
That mints no record, adds no `ExemptionReason`, adds no `SPAWN_ALLOWED` row, and leaves privacy —
the property that actually holds — intact for the new code. (A third option, `Launch::exempt` with
a new `ExemptionReason`, is also unmentioned; it is worse, because `every_launch_exemption_is_enumerated`
would then have to sanction a non-command file.) The plan's "simpler alternative rejected because"
column therefore rejects a strawman.

**Attack or failure path:** with `ocx_lib/src/forge/git_command.rs` allowlisted, the file that
spawns the credential-bearing `git` is governed by a source-text search rather than by module
privacy. A later change that moves the spawn behind a re-exported alias — `pub(crate) type GitCommand
= tokio::process::Command;` in the allowlisted file, used from `git_workspace.rs` — puts a live
spawn site in a file that names none of `SPAWN_TOKENS` (`["process::Command", "process::{",
"process as ", "CommandExt"]`, `launch.rs:904`), passes `no_process_spawn_outside_launch`, and is
never reviewed as a spawn site. That new site can then build its env however it likes, which is
where F-04's missing assertion bites.

**Fix:** either (a) put the capturing primitive in `launch/child_process.rs` and export a narrow
`launch::capture(program, args, env)` that mints no record — no allowlist row needed, privacy
preserved; or (b) keep DV-1 and rewrite the Constitution-deviations row to name what is given up
in the ADR's own words ("privacy is what actually holds; this file moves to the weaker source-text
guarantee") and to record why (a) was rejected. If (b), add to C-021 that `forge/git_command.rs`
is the **only** file permitted to name a `Command` on this path and that it must not export a type
alias or wrapper that lets a sibling spawn without naming one.

---

### F-09 · Warn · C-021 does not pin both directions of the firewall, and mischaracterises the companion check

**Where:** C-021; DV-1's "a companion check reds on a **stale** allowlist entry"; WP-5's test names
`spawn_allowlist_row_present`, `spawn_allowlist_has_no_stale_rows`.

**Problem:** opened both tests. `no_process_spawn_outside_launch` (`launch.rs:1114-1131`) filters
with `!SPAWN_ALLOWED.iter().any(|(allowed, _)| allowed == path)` — a **subset** check. The
companion, `every_allowlisted_file_still_exists` (`launch.rs:1057-1073`), asserts only that each
allowlisted **path exists on disk**. Contrast `EXEMPTION_ALLOWED`, which *is* checked as an
equality in both directions (`launch.rs:1142-1168`, with the explicit rationale "a sanctioned site
that silently stops claiming its exemption leaves an entry here promising a hole that no longer
exists. Both directions are drift."). So for `SPAWN_ALLOWED` there is no "this row is still needed"
direction at all: a row whose file survives but stops spawning is invisible, and DV-1's claim that
"a companion check reds on a stale allowlist entry" is true only for the deleted-file case.

Separately, the two named WP-5 tests duplicate tests that already exist under those semantics; and
the WP-5 note "must prove the firewall both ways: the `SPAWN_ALLOWED` row present and green, and
the row removed and red" is a **mutation procedure**, correctly stated in the notes but not
promoted into C-021, which is the contract a reviewer checks.

**Attack or failure path:** WP-5 lands the row and two tests that assert the row's presence in an
array — both trivially green, and both green after the implementation stops spawning or after a
later refactor moves the spawn elsewhere (F-08's alias path). The reviewer reads C-021 as "both
directions pinned" and does not run the mutation.

**Fix:** restate C-021 as the mutation, not the state: "removing the `forge/git_command.rs` row from
`SPAWN_ALLOWED` turns `no_process_spawn_outside_launch` **red**, demonstrated in the WP-5 review
notes with the observed failure text; renaming the file without updating the row turns
`every_allowlisted_file_still_exists` red." Drop `spawn_allowlist_row_present` as a separate test —
it asserts an array literal — and keep the mutation as the evidence.

---

### F-10 · Warn · The recording `git` shim — the mechanism the ADR names as load-bearing for the entire credential NFR — is owned by no work package

**Where:** WP-4 and WP-14 Expected-files columns; ADR NFR `:1750`.

**Problem:** ADR NFR Security:

> **Asserted by test, and the mechanism is named** — the existing in-process HTTP fake has no
> subprocess visibility, so the assertion rides a **recording `git` shim placed earlier on the
> child's `PATH`** (capturing argv and the environment block to a file) plus the `git http-backend`
> side asserting `Authorization: Basic` arrives on the git request and nowhere else. **Without both
> halves this NFR would be an unbacked sentence.**

The plan's file sets name the HTTP half (`test/tests/git_http_fixture.py`, `test/tests/fake_forge.py`,
WP-4) and never name the shim. WP-14's files are `test/tests/test_transport_git.py` and
`test/tests/announce_helpers.py`. Since "**File sets are disjoint by construction and by ownership**",
an artifact no row names has no owner, and the four WP-14 tests that depend on it
(`::test_secret_absent_from_argv_config_url_and_stderr`, `::test_ambient_git_trace_does_not_reach_child`,
`::test_no_helper_invoked_on_injecting_run`, plus F-04's new one) each assume it exists.

**Attack or failure path:** two failure modes. (a) WP-14 discovers it has no shim, and the tests
degrade to whatever the HTTP side can see — which cannot see argv or the child environment at all,
so the NFR becomes the "unbacked sentence" the ADR warns about. (b) Worse for the tree: the shim is
written somewhere durable and early on `PATH`. This repo already stages `test/bin/ocx` and puts
`~/.ocx/**` on `PATH` via direnv; a `git` shim dropped into either shadows the real `git` for every
other acceptance test, for `task verify`, and for the developer's own shell — a self-inflicted
PATH-hijack of the exact CWE-427 shape ADR §5 declines to defend against.

**Fix:** give the shim a named home in WP-4's Expected files (e.g. `test/tests/git_shim.py`, a
fixture that writes an executable `git` into a per-test `tmp_path` directory and prepends **only
that directory** to the child `PATH`), and state in WP-4's notes that the shim is never written
under `test/bin/`, `~/.ocx/` or any path that outlives the test. Add its own fixture test to WP-4:
`::test_shim_records_argv_and_env` — otherwise every assertion riding it is unverified plumbing.

---

### F-11 · Warn · C-066 falsifies `env.rs`'s own "Known non-members" narrative and leaves a fourth edit site unlisted

**Where:** C-066; `crates/ocx_lib/src/env.rs:199-258`.

**Problem:** C-066 states the three-edit checklist for adding `OCX_ANNOUNCE_GIT_TOKEN` to
`CREDENTIAL_KEYS`, matching the module's documented rule (`env.rs:211-219`). But the same doc block
carries a **fourth** thing the change falsifies — a "Known non-members" section (`env.rs:229-247`)
that explains why the announce family is out:

> - `OCX_ANNOUNCE_TOKEN` (read in `command/package_announce.rs`) — a forge personal access token,
>   so holding it authenticates you. **Open: a cross-repo decision, not an oversight.** `ocx-mirror`
>   announces from a plugin process, and a plugin inherits the ambient environment, so adding this
>   entry would stop that working. The owner's call.

Verified the current set is `&[OCX_IDENTITY_TOKEN, OCX_KEY_PASSWORD, OCX_SIGNING_KEY]`
(`env.rs:258`), and `subsystem-cli.md:323` carries the same open row. After C-066 the module will
document a rationale for excluding the announce family while a member of that family is in the set,
with no note explaining the asymmetry. The behavioural consequence is real and unstated:
`app/plugin_dispatch.rs` removes every `CREDENTIAL_KEYS` entry explicitly, so `ocx-mirror` invoked
as a plugin will see `OCX_ANNOUNCE_TOKEN` and **not** `OCX_ANNOUNCE_GIT_TOKEN`.

**Attack or failure path:** benign today — WP-16's own issue records that `AnnounceConfig` has no
`forge` field and gains no `transport` field, so no mirror pipeline uses the git transport yet. The
failure is later: the mirror repo adds `transport = "git"`, sets `OCX_ANNOUNCE_GIT_TOKEN`, and the
plugin silently falls back to the API credential for the push (precedence rung 2) or to git's own
helpers (rung 3) — authoring the merge request as the wrong identity, which is the exact #411 defect
this work exists to fix, reappearing through a scrub nobody connected to it.

**Fix:** make C-066 a **four**-edit checklist, the fourth being the `Known non-members` block in
`env.rs`: record that `OCX_ANNOUNCE_GIT_TOKEN` is in the set while `OCX_ANNOUNCE_TOKEN` stays out,
and why (the git transport is not reachable from a plugin today). Add one line to WP-16's
ocx-mirror issue: a mirror-invokable git transport must handle the scrub.

---

### F-12 · Warn · Nothing tests that the `extraHeader` URL prefix is path-scoped, so "never just the host" is an unenforced sentence

**Where:** C-034; WP-8 and WP-14 test lists.

**Problem:** C-034 requires `GIT_CONFIG_KEY_n=http.<prefix>.extraHeader` "where `<prefix>` includes
the **full project path**, never just the host" — the mitigation ADR §1 `:1534-1536` states for
scoping the push credential. No test names it. This is the class flagged by the "a textual guard
must judge the transport's normalised form" rule: `<prefix>` is a **string** ocx composes from
`--index-repo`, and the entity that decides whether it matches is git's own URL normaliser
(lowercased host, default port elided, matching at path-component boundaries). A prefix that is too
*narrow* fails closed (auth fails, loud). A prefix that is too *broad* fails open and silently.

**Attack or failure path:** the prefix is rendered from the parsed coordinate's host only, or the
project path is dropped by a `.host()`-shaped helper, or a trailing slash / an explicit `:443` /
an uppercase host makes the intended prefix not match and a fallback host-only prefix is emitted
as a "make it work" fix. Every named test still passes, because they all push to the one index
project. The credential is now attached to every request to that host — combined with **F-03**'s
missing redirect guard, or with a corporate proxy that serves multiple projects on the index host,
that is a credential disclosure to a sibling project.

**Fix:** add `extra_header_prefix_carries_the_project_path` (WP-8 unit over the rendered
`GIT_CONFIG_KEY_n`, with cases for a trailing slash, an uppercase host and an explicit port) and
`::test_header_not_sent_to_a_sibling_project_on_the_same_host` (WP-14 — the fixture serves a second
project path and asserts no `Authorization` header arrives there). The second is the only one that
tests git's normalisation rather than ocx's string.

---

### F-13 · Warn · C-011's "`detail` never carries a credential" has no test

**Where:** C-011 (WP-5); Test inventory WP-5, WP-6, WP-7.

**Problem:** `CapabilityCheck.detail` is an `Option<String>` that flows straight into the JSON
report (C-060: `capability_checks` is non-empty on every run) and therefore into CI logs and
artifacts. C-011 makes the promise; nothing tests it. The plan's other detail-bearing sources are
covered — git stderr goes through the redactor (C-019, C-044) — but `detail` is populated from REST
response fields by WP-6 and WP-7, and neither package's test list mentions redaction.
`github_ensure_push_access_emits_rows` and `preflight_unknown_field_does_not_fail` assert shape,
not content.

**Attack or failure path:** a preflight arm sets `detail` from a forge error body to make an
`unknown` row diagnosable ("403: token `glpat-…` lacks scope"), which is exactly the helpful thing
to do with an unreadable field. GitLab and GitHub both echo credential fragments in some auth error
bodies. The token fragment lands in `capability_checks[].detail`, in the JSON report, in the CI
job log. Nothing reds.

**Fix:** state in C-011 that `detail` is built only from a closed set of values ocx already holds
(the parsed git version, the numeric access level, the field name that was unreadable, a project
path) and never from a response body; add `capability_detail_is_never_response_derived` to WP-5's
list, and route any body-derived string through the C-022 redactor if that closure is ever widened.

---

### F-14 · Suggest · The temp-clone removal guard has no named test, and the SIGKILL residual is not carried into the plan

**Where:** C-033 ("The directory is removed by a guard that runs on every path that unwinds");
WP-8 / WP-14 test lists; ADR §4 `:1690-1692`.

**Problem:** the guard is declared mandatory and never tested. The plan's only tempdir assertion is
`::test_tempdir_mode_0700_unix_only`. The ADR's accepted residual — "A SIGKILL still leaves the
directory. Accepted: it holds no secret at rest (the credential lives only in the environment,
never in `.git/config`)" — appears nowhere in the plan, so the reasoning that makes the residual
acceptable is not available to whoever later adds a `.git/config` write.

**Fix:** add `::test_tempdir_removed_on_every_failure_path` to WP-14 (parametrised over a push
rejection, a classifier miss and a poll exhaustion), and one sentence to C-033 recording that the
directory is acceptable to leak on SIGKILL **only because nothing secret is ever written into it** —
so the next change that writes into `.git/config` reds against a stated premise rather than a habit.

---

### F-15 · Suggest · S-030 is narrower than the ADR promise it projects

**Where:** S-030; ADR §1 `:1536-1537`, NFR `:1750`, ADR Validation `:1994-1998`.

**Problem:** the ADR promise names argv, remote URL, `git remote -v`, `.git/config`, **the reflog**,
shell history, log lines, the child environment and the forge body. S-030 names "argv, `.git/config`,
the remote URL, stderr or a log", and the test name matches. The reflog and `git remote -v` drop out;
so does the ADR Validation's "on **every** failure path" (`:1995-1996`), which the plan's single
test name does not encode. The CWE-522 `/proc` residual (`:1568-1574`) is not carried anywhere in
the plan either — it needs no work, but it is the residual an operator on a shared runner needs
documented, and the WP-15 documentation surfaces do not list it.

**Fix:** extend S-030's expected outcome to the ADR's full surface list, parametrise
`::test_secret_absent_from_argv_config_url_and_stderr` over the failure paths rather than one, and
add the `/proc/<pid>/environ` residual to the new use-case page's security note (WP-15).

---

### F-16 · Suggest · `HOME` is passed through, and only `credential.helper` is reset

**Where:** C-033 / C-034 / C-035; ADR §1 `:1550-1566`.

**Problem:** the ADR reasons carefully about one `~/.gitconfig` key. `HOME` passing through also
admits `url.<base>.insteadOf` (silently rewrites the push URL), `http.<url>.proxy` (routes the
credential-bearing request through an operator-chosen proxy), `core.askPass`, and `include.path`
(pulls in an arbitrary further config file). The ADR's trust model — a corporate runner's proxy and
CA settings are legitimate — makes most of these acceptable, but the reasoning is recorded for the
helper only, so the next reader cannot tell "considered and accepted" from "not considered".

**Fix:** one sentence in C-033 recording that `~/.gitconfig` reaches the child by design, that
`credential.helper` is the only key reset, and that `insteadOf` / `http.proxy` / `include.path` are
accepted because the operator's own HOME is inside the trust boundary. If that is not the intent,
`GIT_CONFIG_GLOBAL` pointed at a null path is the alternative the ADR already priced at `:1564-1566`.

---

### F-17 · Suggest · Distinguish git-emitted classifier phrases from fixture-authored ones, and stop short of calling `LC_ALL=C` a security control

**Where:** C-044; WP-4 `::test_rejection_hook_writes_literal_refusal_line`; WP-14
`::test_two_signal_old_instance_86`, `::test_same_line_with_passed_preflight_77`; the Risks table row
on localised runners.

**Problem:** two of C-044's phrases — `(fetch first)` and `(stale info)` — are emitted by the local
`git` binary, so the fixture exercises real evidence. The 86/77 phrases are written **by the
fixture's own hook**: ADR Validation `:2013-2014` specifies the hook "writes the literal line
`remote: You are not allowed to push code to this project.`". A test that asserts ocx recognises a
string the test wrote is a self-matching detector for the *phrase* half. The plan is not wrong here —
C-044 makes the **preflight** the discriminator and release gate 4 names the live #411 run as the
authority for these texts — but nothing in the plan marks which half of the classifier has real
evidence behind it, so a later reader will read three green cases as three proofs.

Separately: `LC_ALL=C` is an **operability** control, not a security one. A localised runner
degrades every push failure to exit 1; it grants no privilege and bypasses no gate (the 86 promotion
still requires the preflight `unknown`). The Risks table already frames it correctly; C-033 does not,
and the new use-case page should not imply otherwise.

**Fix:** annotate C-044's phrase table with the source of each phrase (git client vs server hook),
and note in C-044 that the server-side phrases are proved only against a fixture-authored line until
release gate 4 runs. Add one clause to C-033 stating that `LC_ALL=C` protects exit-code fidelity,
not confidentiality or integrity.
