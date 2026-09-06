# Review: adr_index_claim_command — security

Summary: needs work
Focus: security
Scope: `.claude/artifacts/adr_index_claim_command.md`, `.claude/artifacts/system_design_index_claim_command.md`
Reviewer: Opus 5 · 2026-09-05 · HEAD `487570fb`

Counts: **1 Block · 4 High · 7 Warn · 2 Deferred**

---

## Credential touch-point audit

Every place a secret exists or flows under the design. "Secret" = `OCX_ANNOUNCE_TOKEN`,
`OCX_ANNOUNCE_GIT_TOKEN`, `CI_JOB_TOKEN`, or the derived `base64(user:secret)` blob.

| Touch point | Exposure class | ADR mitigation (section) | Verdict |
|---|---|---|---|
| `OCX_ANNOUNCE_TOKEN` env read (parent) | CWE-526 — same-UID / root via `/proc` | §1 "same class of exposure … not a regression" | covered (named residual) |
| `OCX_ANNOUNCE_GIT_TOKEN` env read (parent) | CWE-526 | Env-var table; Constitution Check §8 three-edit checklist | covered |
| `CI_JOB_TOKEN` env read | CWE-526 | Precedence list; "Never forwarded to the `git` child" | covered |
| REST header build (`Bearer` / `JOB-TOKEN`) | header on the wire, HTTPS-only | D-T8; `api_base_url` is always `https` (`forge/gitlab.rs:833-836`) | covered |
| `api_is_job_token` derivation | wrong-header selection | D-T8 states a value-equality rule | **gap — F12** |
| `GIT_CONFIG_KEY_n`/`VALUE_n` injection | CWE-522 — child environ | §1 (chosen over a credential helper, with the CVE-class rationale) | covered |
| Child environ (`/proc/<pid>/environ`) | CWE-522 | §1 "Named residual, not assumed away" | covered (accepted) |
| Child environ — ambient leak-through | CWE-526 | §1 claims closure "by construction" via `env_clear()` | **gap — F3** |
| `~/.gitconfig` `credential.helper` | CWE-522 — helper-protocol CVE class | §4 blocks `/etc/gitconfig` only; allowlist passes `HOME` | **gap — F2** |
| argv | CWE-214 | §1 "never enters … argv"; `child_process.rs` passes `Vec<String>`, no shell | covered |
| git **stdout** | needed by the design, not a leak | §4 mandates `spawn_and_wait` | **gap — F1 (unimplementable)** |
| git **stderr** | CWE-532 — raw to terminal, then to error | §3/§4 "redacted and capped" via `status_detail` | **gap — F1 + F5** |
| `.git/config` | CWE-522 at rest | §1 "never … `.git/config`"; §4 "holds no secret at rest" | covered |
| Remote URL / `git remote -v` | CWE-598 | §1 "never enters a remote URL" | covered |
| Reflog | CWE-532 | §1 "never … the reflog" | covered |
| Temp dir at rest | disk residue | §4 mode 0700, RAII removal, SIGKILL residual accepted | covered |
| `GIT_TRACE` / `GIT_CURL_VERBOSE` | CWE-532 — dumps headers | §1 + allowlist "Never passed" column | covered, but rests on F3 |
| Forge error bodies | CWE-532 | D9/D15 via `status_detail` (`forge/error.rs:186-201`) | covered (single-secret, see F5) |
| JSON report | CWE-532 | `credential_kind` is a kind, never a value | covered |
| `tracing` logs | CWE-532 | §1 "neither value is ever logged"; no new telemetry | covered |
| `--out` path (tokenless) | n/a | Header omitted when the credential is empty (`gitlab.rs:163-167`) | covered |

**The four security-lane gaps are all present and adequately specified**: `/proc/<pid>/environ`
residual (ADR §1, CWE-522, explicitly not assumed away); no CA override on the REST client so a
corporate-CA self-managed GitLab is unsupported on the REST half (ADR §2 + Design §7, filed as a
follow-up issue) — see F9 for one thing §2 understates; temp-clone hygiene list (ADR §4, eight
mandatory items); `login:id` rendered for reviewers (ADR §3 + Design owner ladder) — see F4 for
the case that rendering does not cover.

---

## Actionable

- **[Block]** `adr_index_claim_command.md`:§4 "Temp-clone hygiene" and Design §2/§11 — the design
  mandates `spawn_and_wait` for **every** subprocess in the clone's lifecycle, but that helper
  hardcodes `stdin/stdout/stderr` to `inherit` and returns only an `ExitStatus`
  (`crates/ocx_lib/src/utility/child_process.rs:138-151`). Every step of the git recipe needs
  captured stdout — `git --version` for the 2.31 gate and the `git-version` capability `detail`,
  `rev-list --left-right --count` for D-T5's compare, and `hash-object` / `mktree` / `commit-tree`
  for the object shas — and `mktree` additionally needs piped **stdin**. Worse for this review:
  git's stderr reaches the operator's terminal raw and uncapped, so the `(fetch first)` /
  `(stale info)` classifier has nothing to match, every push failure degrades to exit 1, and the
  redaction §3/§4 promise for `GitPushFailed` never executes on the bytes that were already
  printed. [CWE-532] — **Remediation:** add a capturing sibling in `child_process.rs` that keeps
  `env_clear()`, the SIGINT/SIGTERM forwarding and `kill_on_drop(true)`, but pipes all three
  streams and returns `(ExitStatus, Vec<u8>, Vec<u8>)`; mandate *that* function in §4 and confine
  `spawn_and_wait` to the cases where inherited stdio is wanted. Note the ADR's own §4 rationale
  for `spawn_and_wait` (RAII must run) is correct and unaffected — `exec` genuinely diverges
  (`child_process.rs:96-118`).

- **[High]** `adr_index_claim_command.md`:§4 "Child environment allowlist" — the allowlist passes
  `HOME` / `USERPROFILE` / `HOMEDRIVE` / `HOMEPATH` through "so `~/.gitconfig` proxy and CA
  settings apply", while `GIT_CONFIG_NOSYSTEM=1` blocks `/etc/gitconfig` on the stated ground that
  it "could carry an unexpected `credential.helper`". The user-level config is the far likelier
  carrier — `osxkeychain`, `manager` and `store` are the defaults every git install guide sets —
  and it stays fully active, re-admitting the exact credential-helper-protocol CVE class §1
  declares inapplicable (Clone2Leak, CVE-2024-52006 and siblings). `GIT_TERMINAL_PROMPT=0` does
  not close it: a helper with a cached credential never prompts. [CWE-522] — **Remediation:** add
  `-c credential.helper=` (an empty value resets the list) to every clone and push invocation, or
  set `GIT_CONFIG_GLOBAL` to a null path and pass proxy/CA settings as explicit `-c` flags instead
  of via `HOME`. Assert in the fixture that no helper is invoked.

- **[High]** `adr_index_claim_command.md`:§1 and `system_design_index_claim_command.md`:§7 — "
  `spawn_and_wait` calls `env_clear()` before applying the explicit environment, so an ambient
  `GIT_TRACE` cannot reach the child by inheritance — the closure is by construction". `env_clear()`
  closes inheritance from the *process*; the child's environment is then whatever the `Env` value
  carries, and `Env::new()` — which is also the `Default` impl — seeds itself from
  `std::env::vars_os()` (`crates/ocx_lib/src/env.rs:419-430`). The ergonomic constructor therefore
  hands the child the entire ambient environment, `GIT_TRACE`, `CI_JOB_TOKEN` and
  `OCX_ANNOUNCE_TOKEN` included, straight past `env_clear()`. The closure is by caller discipline
  via `Env::clean()` (`env.rs:433-438`), which neither artifact names. [CWE-526] —
  **Remediation:** name `Env::clean()` as mandatory in the §4 hygiene list, forbid
  `Env::new()`/`Env::default()` on this path, and restate the invariant honestly as
  caller-enforced-and-asserted rather than structural. The Validation checklist already lists the
  child-environment assertions; the ADR's claim of structural closure is what must change.

- **[High]** `adr_index_claim_command.md`:CLI grammar table and
  `system_design_index_claim_command.md`:"Owner-resolution ladder" arm 1 — `--owner LOGIN:ID` "is
  always accepted", and a `Resolved { .. }` "is taken as given and needs no call". Nothing binds
  the login to the id. `owners[]` feeds two consumers that read different halves: G-19 auto-merge
  matches the **numeric id**, and the G-04 reviewer reads the rendered **`login:id`** text. An
  asserted pair like `alice:<attacker-id>` therefore shows the reviewer a name they recognise
  while granting auto-merge to a different account. The security lane's finding that `--owner`
  opens "no separate impersonate-as surface" (§5) was written for the `LOGIN`-only form, whose
  live lookup is exactly what this escape hatch removes; the ADR names the shared-release-account
  boundary explicitly but never names this sharper one on the same control. [CWE-345] —
  **Remediation:** when the users API is reachable, resolve the supplied login and refuse a
  mismatched id at exit 64; when it is not, carry the pair as unverified in the report and label
  it as asserted rather than resolved in the request body, so the reviewer can see which owners
  the forge confirmed.

- **[High]** `adr_index_claim_command.md`:§3 and the exit-code table — git stderr enters
  `ForgeError::GitPushFailed` "redacted through `status_detail`'s rule". `status_detail(body,
  token)` takes exactly one secret and does a plain substring replace
  (`crates/ocx_lib/src/forge/error.rs:186-201`). Under `--transport git` there are two independent
  secrets, because D-T7 precedence step 1 lets `OCX_ANNOUNCE_GIT_TOKEN` differ from the API token
  — and the push secret exists on the wire only as `base64(user:secret)`, which no plaintext
  needle matches. Passing the API token, as the existing call sites do, redacts neither.
  [CWE-532] — **Remediation:** widen the redactor to a slice of secrets and include the base64
  `user:secret` form; prove it red by seeding each form into a fixture stderr before asserting
  the green.

- **[Warn]** `adr_index_claim_command.md`:"The git recipe" and "Test fixture" — the four
  `merge_request.*` push options are enumerated, but no invariant says the set is **closed**.
  `merge_request.merge_when_pipeline_succeeds` is a supported GitLab push option, and setting it
  would auto-merge the claim request the moment the pipeline goes green, defeating G-04's "a first
  claim is never auto-merged" — the single governance control this entire command is built around.
  `remove_source_branch` is the same shape, lower impact. [CWE-284] — **Remediation:** state the
  allowlist as closed and name the two forbidden options; assert `GIT_PUSH_OPTION_COUNT == 4` and
  the exact key set in the fixture's `post-receive` hook, which already parses those variables.

- **[Warn]** `system_design_index_claim_command.md`:"Owner-resolution ladder" — arm 2 (CI
  environment) outranks arm 3 (`authenticated_identity`), so the bot-refusal security decision is
  made from `GITLAB_USER_LOGIN` / `GITLAB_USER_ID` before the server-asserted `bot` field is
  consulted. A GitLab **service account** carries `bot: true` server-side but an operator-chosen
  login that need not match the `project_<n>_bot*` / `group_<n>_bot*` shapes arm 2's heuristic
  knows, and under a bare job token the users API is unreachable, so the weak form is the only
  check that runs. The bot then lands in `owners[]` as a person. The ADR cites GitLab's
  service-accounts doc in its Links but never connects it to the arm-2 shape list. [CWE-807] —
  **Remediation:** consult the server-asserted field first whenever any credential in the run can
  reach the users API, and add an `owner_identity_source` field to the report so the reviewer can
  see which rule produced the list.

- **[Warn]** `adr_index_claim_command.md`:NFR "Security" and Validation — "Asserted by test, not
  by review: the acceptance suite captures child argv, the temp clone's `.git/config`, the remote
  URL and stderr on every failure path" names no mechanism, and none exists.
  `test/tests/fake_forge.py` is an 831-line in-process `http.server` fake with no subprocess
  visibility: it cannot see ocx's `git` child's argv, environ or stderr. This sentence is the
  load-bearing justification for the whole credential-containment NFR. — **Remediation:** specify
  the mechanism (a recording `git` shim placed earlier on the child's `PATH`, capturing argv and
  environ to a file the test reads), and add the assertion the fixture genuinely *can* make once
  `git http-backend` lands: that `Authorization: Basic` arrives on the git HTTP request and
  appears in no other captured surface.

- **[Warn]** `adr_index_claim_command.md`:§2 and `system_design_index_claim_command.md`:§7 — the
  corporate-CA gap is stated accurately, but understated in one way that changes how the follow-up
  issue should be scoped. §2 frames the divergence as inheritance (git trusts the host store, the
  REST client does not). In fact the ADR's own §4 allowlist **actively forwards**
  `GIT_SSL_CAINFO`, `GIT_SSL_CAPATH`, `SSL_CERT_FILE` and `SSL_CERT_DIR` to the git child, while
  the REST client ignores all four: `forge/http.rs:33-41` seeds a fixed vendored set through
  `utility::tls::seed_embedded_roots`, whose own doc states the extra-roots branch "never touches
  the system store". So ocx configures the divergence rather than merely inheriting it. —
  **Remediation:** say this in §2 and in the follow-up issue, so the fix is scoped as "make the
  REST client honour the same four variables" rather than "document a prerequisite".

- **[Warn]** `adr_index_claim_command.md`:CLI grammar table — `--repository` is validated only as
  "a well-formed `oci://host/path`", with no parser named, and the value then reaches both a
  push-option pkt-line and the request body. The strict parser already exists:
  `oci::index::parse_physical_repository` (`crates/ocx_lib/src/oci/index/ocx_index.rs:461-486`)
  requires an exact round-trip through the Identifier grammar and therefore rejects whitespace,
  control characters, smuggled tags and digests — precisely the inputs that would make §3's "no
  raw newline traverses a single option value" false. A second hand-rolled `strip_prefix` +
  `split_once` would carry none of that. [CWE-20] — **Remediation:** mandate reuse of that
  function by name, per `quality-core.md` § Don't Own Non-Domain Code.

- **[Warn]** `adr_index_claim_command.md`:§1 env-var table and Constitution Check §8 —
  `OCX_ANNOUNCE_GIT_TOKEN` joins `CREDENTIAL_KEYS` while `OCX_ANNOUNCE_TOKEN` deliberately stays
  out (`crates/ocx_lib/src/env.rs:238` currently holds three keys, none of them the announce
  token). But the push credential falls back to the API credential (D-T7 precedence step 2), so in
  the default posture the identical secret value is scrubbed from plugin and launcher children
  under one name and forwarded under the other. The containment the checklist buys is nominal for
  the value that actually matters. — **Remediation:** state that plainly next to the checklist, or
  close the announce-token exemption in the same pull request.

- **[Warn]** `adr_index_claim_command.md`:D-T8 and "API Contract — the constructor" —
  `api_is_job_token` is a *derived* fact stored as a plain `bool` on `ForgeCredentials`, with the
  derivation living in the CLI (Design §3: `credentials.rs` "Holds no environment-reading logic").
  Header selection is therefore correct only while every constructor derives it, and D-T8 reads as
  a value-equality rule when it is really a caller-supplied flag. Both mis-derivations fail closed
  at GitLab — a PAT presented as `JOB-TOKEN` and a job token presented as `Bearer` are both
  rejected — so this is containment, not correctness. — **Remediation:** derive the flag inside a
  `ForgeCredentials` constructor that takes the environment snapshot, so no caller can set it
  independently of the value it describes.

---

## Deferred

- **[Warn]** `adr_index_claim_command.md`:Security Architecture — **SSRF on the forge path.**
  Neither the REST client nor the proposed git remote goes through `oci/ssrf.rs`: there is no
  `ssrf` or `transport_policy` reference anywhere under `crates/ocx_lib/src/forge/`. The host
  validator that does run, `is_valid_host` (`crates/ocx_lib/src/forge.rs:129-152`), rejects
  userinfo, IPv6 literals, paths, queries and fragments — closing the `gitlab.com@evil.example`
  class it was written for — but accepts `127.0.0.1`, `0x7f000001`, `127.1`, `10.0.0.1` and
  `169.254.169.254` as ordinary labels. My assessment is that **not** applying the guard here is
  correct: the coordinate comes from argv, `oci/ssrf.rs`'s own module doc scopes it to
  remote-controlled registry pointers, and enforcing it would refuse the self-managed-GitLab-on-a-
  private-network deployment this ADR exists to serve. The ADR is silent rather than wrong.
  **Question for the human:** can a forge coordinate ever reach these commands from anything other
  than argv — a `[managed]` config payload, `ocx.toml`, or an ocx-mirror spec? If yes, the guard
  becomes mandatory and this flips to Block. Either way the ADR should record the reasoning, since
  a future reader will ask why the announce path guards its registry pointer and not its forge.

- **[Suggest]** `system_design_index_claim_command.md`:"Owner-resolution ladder" — the design says
  each `Login(l)` is "resolved through `Forge::resolve_user`" but never says **which** login is
  persisted: the operator's argv spelling, or the canonical spelling from the forge's response
  body. GitHub logins are case-insensitive and homoglyph-confusable, and the pair is rendered to a
  human reviewer as the identity check. `ForkIdentity` already sets the precedent — "every field
  is read from a forge API **response body** — never composed" (`crates/ocx_lib/src/forge.rs:246`).
  [CWE-1007] **Question for the human:** should the governance field record what the operator
  typed, or what the forge says that id is called? I recommend the latter, matching `ForkIdentity`.

---

## Verified claims (path:line evidence)

- **Exit code 85 is taken; 86 is the first free slot.** `UnsupportedKeyBackend = 85` at
  `crates/ocx_lib/src/cli/exit_code.rs:93` — the ADR cites that exact line. Highest value in use
  is 85. ✅
- **The `Forge` doc-count correction is real.** `crates/ocx_lib/src/forge/api.rs:7` says "ten
  operations" in the module doc; the trait declares 11 `async fn`. The ADR's parenthetical
  "the correct interim value is *eleven*" is right. ✅
- **The contract text the ADR leans on exists verbatim.** `crates/ocx_lib/src/forge/api.rs:113-117`
  — "an implementation that cannot hold it must return an error rather than approximate it". ✅
- **The GitLab job-token comment is wrong as claimed.** `crates/ocx_lib/src/forge/gitlab.rs:150-157`
  asserts a job token reaches "none of repository files, commits, branches, merge requests or
  forking" — true for writes, false for reads. `PRIVATE-TOKEN` is sent unconditionally at
  `gitlab.rs:166`, and omitted when the token is empty at `gitlab.rs:163-165`. ✅
- **No CA override on the REST client.** `crates/ocx_lib/src/forge/http.rs:33-41` builds with
  `Policy::none()` plus `utility::tls::seed_embedded_roots`; no `SSL_CERT_FILE`, no config key, no
  flag, no `danger_accept_invalid_certs`. The redirect-disabled claim is additionally guarded by a
  structural test at `http.rs:56-81`. ✅
- **`exec` really does skip Drop.** `crates/ocx_lib/src/utility/child_process.rs:96-118` — `execvp`
  on Unix, `process::exit` on Windows. §4's "never `exec`" mandate is correctly reasoned. ✅
- **`env_clear()` is called.** `child_process.rs:141` (and `:98` for `exec`) — the mechanism the
  ADR cites exists; see F3 for why it does not deliver the claimed closure. ✅
- **`CREDENTIAL_KEYS` membership.** `crates/ocx_lib/src/env.rs:238` —
  `&[OCX_IDENTITY_TOKEN, OCX_KEY_PASSWORD, OCX_SIGNING_KEY]`. `OCX_ANNOUNCE_TOKEN` is absent, as
  the ADR states. ✅
- **Push options are not a command-injection surface.** `child_process.rs:97-98` and `:139-140`
  pass a `&[String]` to `Command::args`; no shell is involved on either path. §3's CWE-78
  dismissal is correct. ✅
- **No cross-transport write path found.** Under `git`, the only REST call after the push is
  step 6's `GET /merge_requests?source_branch=` — a read. `ensure_push_access` is a read under both
  transports. D-T3 holds against the described flows; it is enforced by discipline plus one
  fixture assertion ("zero REST write calls on the failure path"), not by type. ✅
- **All four security-lane gaps are folded in.** `/proc` residual → ADR §1; corporate-CA/REST →
  §2 + Design §7; MR content injection → §3; bot-check boundary → STRIDE Spoofing row + Design's
  two-rule table. ✅

---

## Changelog

| Date | Author | Change |
|---|---|---|
| 2026-09-05 | Reviewer (Opus 5) | Initial security review. Full credential touch-point audit, 12 actionable findings (1 Block, 4 High, 7 Warn), 2 deferred questions, 11 verified code claims. |
