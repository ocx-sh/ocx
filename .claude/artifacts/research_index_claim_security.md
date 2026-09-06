# Research: index claim command — security & compliance

## Metadata

- Date: 2026-09-04
- Expires: 2027-03-04
- Domain: security
- Triggered by: /hex-architect index claim command (dossier .agents/discussions/index-claim-command.md)
- Author: hex-architect research lane, axis security & compliance

## Direct Answer

The design in the dossier (REST reads always, git-subprocess writes under `--transport git`,
credential via `GIT_CONFIG_KEY_n`/`GIT_CONFIG_VALUE_n` scoped to the index host, job-token
pickup gated on `--transport git` inside a GitLab job) is the right shape and matches how
GitLab's own docs and every surveyed prior-art tool (Renovate, git-credential-manager) handle
this class of problem. Four gaps are not yet covered by the dossier's requirements and should
be closed in the ADR:

1. **`GIT_CONFIG_KEY_n`/`VALUE_n` is visible in `/proc/<pid>/environ`** to the same user and
   to root — it is not argv-visible (good, closes the `ps aux` class) but it is not
   root/same-uid-safe either. On a shared CI runner or a compromised sibling process under the
   same UID this is a real exposure. No mitigation exists that keeps the value in the child's
   environment at all; the residual risk should be named, not silently assumed away.
2. **TLS trust is fully disjoint between the two transports.** `forge/http.rs` embeds a fixed
   Mozilla root set and offers **no** override — confirmed by grep, no `SSL_CERT_FILE` /
   custom-CA hook exists on the forge client path (`crates/ocx_lib/src/forge/http.rs`,
   `crates/ocx_lib/src/forge/api.rs`). A self-managed GitLab behind an internal CA will need the
   `git` subprocess's CA trust configured completely independently of ocx's REST reads — the ADR
   should say this out loud as a deployment prerequisite, since "REST works, `--transport git`
   TLS-fails" (or vice versa) is otherwise an confusing bug report.
3. **Push-option content is not a shell/command-injection surface** (GitLab requires `\n` as a
   literal two-character escape, not a raw newline, and push options travel as pkt-line
   capability strings, not as a git-config value or shell argument) — but it **is** an MR-content
   injection surface: whatever the operator/CI puts in `--owner`-adjacent title/description
   fields lands verbatim (after GitLab's own `\n`→newline substitution) in an MR that a human
   reviewer merges under G-04. The dossier does not yet say the claim command must not
   interpolate untrusted (e.g. package-declared `upstream` disclaimer text) into
   `merge_request.description` without the same care given to REST body fields.
4. **GitHub/GitLab bot-flag detection is reliable but not exhaustive** — `type: "Bot"` and
   `bot: true` are both official, documented fields (not heuristics), but a **project access
   token's** GitLab user has `bot: true` while a **classic PAT** minted by a human on a shared
   "release" account does not — the refusal correctly stops the first case and correctly cannot
   stop the second (the dossier already frames this as a credential/posture choice, which is the
   right call — flagging it here only so the ADR states the boundary explicitly rather than
   implying the bot check is a complete impersonation guard).

## Trends

- **CI-native identity over long-lived PATs.** GitLab's job-token push (GA 18.4) and GitHub's
  OIDC-based immutable subject claims (GA 2026-07-15, see Key Findings) are both 2025–2026
  moves toward "the pipeline's own ambient identity authors the change" instead of a
  provisioned secret — exactly the shape #411 asks for. ocx's design (no fallback, transport
  chosen explicitly, credential inferred only under an explicit `--transport git`) tracks this
  trend rather than fighting it.
- **Fine-grained job-token permissions.** GitLab shipped fine-grained CI/CD job-token
  permissions to GA in 18.3 (2025-08-26, see Key Findings) specifically because "pipelines
  inherit overprivileged permissions... if pipelines are compromised or tokens are leaked" —
  i.e., GitLab itself now treats the job token's *ambient* scope as a known risk, reinforcing
  that ocx's job-token pickup should stay opt-in-by-transport, never a silent default.
- **Credential-helper protocol hardening is still landing.** The Clone2Leak disclosures
  (2025-01) and their follow-on CVEs continued into 2025 for individual tools (Git LFS
  CVE-2024-53263, GitHub CLI CVE-2024-53858, GitHub Desktop CVE-2025-23040) — this is a live,
  multi-vendor bug class, not a closed chapter, which argues for ocx's git subprocess to avoid
  credential helpers entirely (env/config injection, never `credential.helper`) as the dossier
  already specifies.

## Key Findings

### 1. Injecting a credential into a `git` subprocess

- **`GIT_CONFIG_COUNT`/`GIT_CONFIG_KEY_n`/`GIT_CONFIG_VALUE_n`** were introduced in **Git
  2.31** (2021-03-15) as one of "two new ways to feed configuration variable-value pairs via
  environment variables" — [Git 2.31 release notes](https://github.com/git/git/blob/master/Documentation/RelNotes/2.31.0.adoc), [git-config docs](https://git-scm.com/docs/git-config). Pairs are
  zero-indexed; a missing key or value is an error; they override file-based config but are
  themselves overridden by `git -c`. This matches and confirms the dossier's "git ≥ 2.31"
  requirement.
- **Visibility.** These variables are **not** in argv, so they do not appear in `ps aux` or
  `/proc/<pid>/cmdline` — this is the property the dossier is relying on, and it holds. They
  **are** in the child's environment block, hence readable via `/proc/<pid>/environ` by the
  same UID (and root) for the process's lifetime — [procfs environ background](https://codywu2010.wordpress.com/2014/09/14/procfs-environ-explained-in-depth-1/), general env-var
  exposure summary at [env.dev security guide](https://env.dev/guides/env-vars-security). CWE-522
  (Insufficiently Protected Credentials) applies to this residual window; it is the same class
  of exposure `OCX_ANNOUNCE_TOKEN` already has as a parent-process env var, so scoping the
  credential to `GIT_CONFIG_VALUE_n` does not make it *worse* than the status quo, but it does
  not eliminate the `/proc` exposure either — worth one sentence in the ADR's threat model
  rather than silence.
- **`credential.<url>.helper`, `GIT_ASKPASS`, credential files** — all three keep the secret out
  of argv too ([gitcredentials(7)](https://www.man7.org/linux//man-pages/man7/gitcredentials.7.html)); `GIT_ASKPASS` writes the secret to stdout on request rather than storing
  it, `credential.helper` needs a helper program or script (adds a moving part, and stock
  helpers like `store`/`cache` **write the credential to disk or a cache daemon socket** —
  neither is wanted here). `GIT_CONFIG_VALUE_n` is the simplest of the three because it needs no
  extra process or file. `.git/config`, `git remote -v`, reflog, and shell history are correctly
  identified in the dossier as things the chosen approach avoids — confirmed: none of
  `GIT_CONFIG_KEY_n`/`VALUE_n` touch any repo-persisted file, so a `git remote -v` or `cat
  .git/config` on the temp clone shows nothing.
- **`http.<url>.extraHeader`** is the config key `http.extraHeader` gets rendered as under
  `GIT_CONFIG_KEY_n`; it is the mechanism, not an alternative to it — i.e. the dossier's
  "`Authorization: Bearer`-equivalent for git" is `GIT_CONFIG_KEY_0=http.<url-with-path>.extraHeader`,
  `GIT_CONFIG_VALUE_0="Authorization: Bearer <token>"`, scoped by URL prefix exactly as D3's
  redirect concern requires — the URL prefix must include the full path to the project, not
  just the host, or the header is sent to every path on that host (still same host, but broader
  than necessary; low severity, still worth being precise about in the implementation).
- **`credential.useHttpPath`** changes credential-helper *matching* only (whether the path
  component of the URL is part of the cache key) — it has no bearing on the `extraHeader`
  approach ocx is taking, since that mechanism does not go through a credential helper at all.
  This is a **negative finding**: `useHttpPath` is irrelevant to this design, and the ADR should
  not need to mention it.
- **`GIT_TRACE`/`GIT_CURL_VERBOSE`.** Both dump request headers (including `Authorization`) to
  stderr — [gitcredentials(7)](https://www.man7.org/linux//man-pages/man7/gitcredentials.7.html) and general git tracing docs. If ocx (or a CI system wrapping it) ever sets
  either for debugging, the injected header is fully exposed in build logs — this is exactly the
  D15 redaction hazard the REST path already treats as load-bearing (`adr_announce_gitlab_forge.md`
  D15) and the ADR should extend the same "never let debug tracing leak the credential" posture
  to the git subprocess explicitly, since nothing in `child_process.rs` currently sets or
  strips `GIT_TRACE*` from the child env.
- **Clone2Leak class (CVE-2024-52006, CVE-2024-50349, and 2025 follow-ons).** Fetched
  [flatt.tech's Clone2Leak writeup](https://flatt.tech/research/posts/clone2leak-your-git-credentials-belong-to-us/) directly (primary source, 2025-01). The bug class is carriage-return /
  newline smuggling **into the credential-helper protocol** — a malicious remote URL (or, for
  the LFS variant CVE-2024-53263, a repository-controlled `.lfsconfig`) injects extra
  newline-delimited fields that make a credential helper answer with the wrong host or leak the
  cached credential to an attacker-controlled URL. Table of related CVEs found:

  | CVE | Component | Flaw |
  |---|---|---|
  | CVE-2024-52006 | Git core | Defense-in-depth CR-smuggling guard added (`credential.protectProtocol=true`, default since the patched release) |
  | CVE-2024-50349 | Git Credential Manager | Escape-sequence trickery in URLs shown to the user, tricking them into approving the wrong host |
  | CVE-2024-53263 | Git LFS | Newline injection via `.lfsconfig`, bypassing Git's own URL validation |
  | CVE-2024-50338 | Git Credential Manager | .NET `StreamReader` splits on `\r`/`\n`/`\r\n`, enabling CR-smuggling |
  | CVE-2024-53858 | GitHub CLI | Host-matching bypass, leaking tokens to non-GitHub domains |
  | CVE-2025-23040 | GitHub Desktop | Regex treats CR as a line separator in multiline mode, same smuggling class |

  **Does host-scoping fully prevent this?** No, and the fetched source says so explicitly: host
  scoping (e.g. `credential.https://github.com.helper=...`) "does **not** fully prevent leakage
  via submodule URLs or redirects if the credential helper doesn't independently validate the
  requesting host" — the fix requires the helper itself to check the host on every answer, not
  just the config key it was registered under. **This is the strongest argument for ocx's
  chosen approach over any credential-helper approach**: `GIT_CONFIG_VALUE_n` with
  `http.extraHeader` scoped by URL prefix is not a credential-helper-protocol participant at
  all — there is no newline-delimited handshake for an attacker to smuggle into, because the
  header is attached by git's HTTP transport directly, not negotiated. The entire Clone2Leak
  class is therefore inapplicable to the chosen mechanism, and the ADR can state that as a
  design rationale rather than leaving it implicit.
- **Redirects and submodules specifically.** `http.extraHeader` set for a URL prefix is sent to
  every request under that prefix, including a redirect target if git followed one — but the
  claim/announce flow does not process untrusted submodules (the temp clone is of the index
  repository only, cloned to a known URL, not recursively with `--recurse-submodules`), so the
  submodule vector in the CVE table above does not apply here as long as the implementation
  never adds `--recurse-submodules` to the clone. Worth a one-line invariant in the ADR: *the
  temp clone never recurses submodules*.

### 2. TLS trust divergence

- Confirmed by reading `crates/ocx_lib/src/forge/http.rs` and `crates/ocx_lib/src/utility/tls.rs`
  directly: ocx's forge REST client is built with `reqwest` (`rustls` feature, no
  `native-tls`), no redirects, and a **fixed, vendored** `webpki-root-certs::TLS_SERVER_ROOT_CERTS`
  set added via `add_root_certificate` — there is no `SSL_CERT_FILE`, no config key, no flag
  that lets an operator add a corporate root to this client. A grep across
  `crates/ocx_lib/src/forge/` and `crates/ocx_lib/src/oci/` for any CA-override mechanism
  returned nothing on the forge path.
- Meanwhile `git` (the subprocess under `--transport git`) uses whatever the **host** trusts:
  the system CA bundle on Linux (`/etc/ssl/certs/ca-certificates.crt` on Debian/Ubuntu,
  `/etc/pki/tls/certs/ca-bundle.crt` on RHEL), overridable per-host with
  `http.<url>.sslCAInfo` or globally via `GIT_SSL_CAINFO` — [general git-proxy/CA guidance](https://oneuptime.com/blog/post/2026-02-26-argocd-git-http-https-proxy/view), and on
  Windows optionally the OS store via `http.sslBackend=schannel` — [schannel guidance](https://www.alertmend.io/blog/git-config-global-http-sslbackend-schannel). GitLab
  Runner and GitLab Omnibus installs both document adding a corporate root to a *system*
  trust location precisely because git (and other system tools) read it from there —
  [GitLab self-signed CA docs](https://docs.gitlab.co.jp/runner/configuration/tls-self-signed.html).
- **Consequence for this design.** A self-managed GitLab instance sitting behind an internal CA
  is a normal, expected deployment (`adr_announce_gitlab_forge.md` D3 exists precisely for
  self-managed instances) but the two transports resolve trust from *completely disjoint*
  sources: reqwest trusts only the compiled-in Mozilla set (never the OS store, by ocx's own
  design comment: seeding the roots is what stops reqwest's rustls path from touching the OS
  store at all), while git trusts the OS/system store unless separately configured. Two
  concrete failure modes follow directly: (a) REST reads under `--transport git` (branches,
  commits, MRs) fail with a TLS error the *operator* cannot fix inside ocx at all — there is no
  ocx-level knob, only "get this CA into the public Mozilla set" or fork/patch, which is not
  realistic for an internal CA; (b) if the operator instead fixes only the OS trust store (the
  natural first move, since that's what every git/CA guide says to do), REST reads still fail
  because reqwest never looks there. **This is a genuine gap for the self-managed-GitLab
  persona the dossier explicitly targets**, and it predates the claim/git-transport work — it's
  inherited from the existing `forge/http.rs` hardening. It is reasonable to leave fixing it out
  of scope for the claim ADR (the dossier's "out of scope" list is already long), but the ADR
  should say explicitly that self-managed GitLab behind an internal CA is **not yet supported by
  either transport's REST half**, so it isn't discovered as a surprise bug later, and should
  file it as a named follow-up issue rather than let it stay implicit.
- **How other Rust tools reconcile this.** `reqwest` 0.13's own newer default is a
  platform-verifier path that *does* read the OS store by default (`rustls-tls-native-roots`,
  or the new native platform verifier introduced for 0.13 — [reqwest v0.13 blog post](https://seanmonstar.com/blog/reqwest-v013-rustls-default/)) — the pattern
  other tools converge on is "read the OS/native store, and let the *user's OS* be the one
  place a corporate CA is installed," which is exactly the model `git` already follows. ocx's
  choice to embed a fixed set instead was made deliberately for minimal-CI-runner
  compatibility (per the doc comment in `tls.rs`) — that trade-off is reasonable for OCI
  registry pulls but is now colliding with a persona (self-managed GitLab, corporate CA) the
  forge feature specifically wants to serve. Flagging the tension is in scope for this ADR even
  if resolving it is not.
- **`http.sslBackend`/schannel** is Windows-only and orthogonal to the reqwest question — it
  only affects which trust store the *git* half consults, not the REST half; not decision-
  relevant here.

### 3. Push-option injection

- **Not a shell/command-injection vector.** Push options travel as pkt-line capability strings
  during the git push protocol, not as shell arguments and not through a shell — [pkt-line format spec](https://git-scm.com/docs/protocol-common) caps a single pkt-line at 65516 bytes of
  payload (65520 with the 4-byte length header). `git push -o <value>` passes `<value>` as a
  single argument to the `git` binary via `Command::args` (per `child_process.rs`'s existing
  `spawn_and_wait`, which never invokes a shell) — CWE-78 (OS Command Injection) does not apply
  because there is no shell interpolation step anywhere in this path, confirmed by reading
  `crates/ocx_lib/src/utility/child_process.rs` (uses `std::process::Command`/
  `tokio::process::Command` directly, args passed as a `Vec<String>`, never joined into a
  string).
- **Newlines specifically.** Git's push-option transport itself does not carry a raw newline in
  a single value; GitLab's own docs instruct using the *literal two-character sequence* `\n`
  in `merge_request.description=` and have GitLab-side code convert that to a real newline for
  rendering — [GitLab push options docs](https://docs.gitlab.com/topics/git/commit/#push-options-for-merge-requests), corroborated by [GitLab merge request !87020](https://gitlab.com/gitlab-org/gitlab/-/merge_requests/87020/commits) ("Convert newline symbols in
  description push options to actual newlines") and issue history at
  [gitlab-org/git#66](https://gitlab.com/gitlab-org/git/-/issues/66) and
  [gitlab-org/gitlab#241710](https://gitlab.com/gitlab-org/gitlab/-/issues/241710) confirming
  this was a known limitation GitLab worked around server-side, not a protocol feature. **No
  length limit specific to push-option values is documented** by GitLab beyond the generic
  pkt-line ceiling above — this is a negative/dead-end: searched GitLab admin/instance-limits
  docs and found no push-option-specific cap, only general repository/file-size limits.
- **The actual risk is content injection, not code injection.** Since GitLab renders
  `merge_request.title`/`.description` largely as-is (markdown) into the MR, any value ocx
  passes through unsanitized (e.g., forwarding an untrusted `--upstream-org`/`--disclaimer`
  string, or a package-declared field, verbatim into the description) becomes attacker-
  controlled markdown/text in a PR a human reviewer merges under G-04's human-lane trust — the
  concern is social-engineering/markdown-injection (e.g. a misleading link, an `@mention` spam,
  a rendered image tracking pixel) rather than RCE. This is the same class of risk D9/D15
  already treat for REST error bodies (redact/cap before display); the git-transport equivalent
  is simpler because ocx itself constructs the title/description strings from known-shape
  inputs (namespace, package name, owner list) rather than passing through arbitrary
  attacker-supplied text — but the ADR should still say explicitly that any *future* addition of
  a free-text field to the claim/announce MR body must be treated the same way REST body
  construction already is (escape/validate before interpolation), so this isn't rediscovered
  the hard way later.
- **Logging.** No evidence found that GitLab logs push-option *values* to a project-visible
  audit trail by default; searched GitLab audit-event and system-notes docs
  ([audit events](https://docs.gitlab.com/development/audit_event_guide/), [system notes](https://docs.gitlab.com/user/project/system_notes/)) without finding a
  push-option-specific audit record — this is a negative finding, not a confirmed absence
  (GitLab's job-execution logs may still show the invoking `git push` command line if a CI
  script echoes it, which is an ocx-adjacent operational concern: **do not `echo`/log the
  literal argv containing `-o merge_request.description=...` if that string is ever built from
  anything sensitive**, though today it is not).

### 4. Job token specifics

- **`CI_JOB_TOKEN` as HTTPS password, username `gitlab-ci-token`.** This is exactly the pattern
  the dossier already codifies as the default `OCX_ANNOUNCE_GIT_USERNAME`. GitLab's own docs
  describe the token as carrying "the same access permissions as the user who started the job"
  for the read endpoints and for push (confirmed independently in the discussion doc's own
  fact-check, corroborated here) — [GitLab CI/CD job token docs](https://docs.gitlab.com/ci/jobs/ci_job_token/).
- **Scope of a leaked job token.** GitLab's docs are explicit that "if a job token is leaked, it
  could potentially be used to access private data accessible to the user that ran the CI/CD
  job" and that the token "might give extra permissions that aren't necessary" — this is CWE-269
  (Improper Privilege Management) framed as an acknowledged platform trade-off rather than a
  bug: the token inherits the *triggering user's* full permissions, scoped only by (a) the
  job-token allowlist (which projects/groups may be *reached*, capped at 200 groups + 200
  projects, counted separately) and (b) the token's own lifetime (masked in job logs, granted
  only while the job runs) — [GitLab CI/CD job token docs](https://docs.gitlab.com/ci/jobs/ci_job_token/). Fine-grained job-token
  permissions (narrowing scope below "full user permissions") went GA in **GitLab 18.3**
  (2025-08-26) specifically to address this — [GitLab fine-grained job tokens GA announcement](https://about.gitlab.com/blog/fine-grained-job-tokens-ga/). This is a
  meaningful mitigation an operator can layer on independently of anything ocx does; worth a
  doc-surface mention (the dossier already lists a docs pass) recommending fine-grained
  job-token scopes as a hardening option for the git-transport posture.
- **The job-token allowlist as a security boundary — with a caveat.** The allowlist is the
  mechanism that stops project A's job token from reaching project B's index repo by default.
  One GitLab-tracked issue found during research: a user could add a project to a job-token
  scope allowlist "without having any role in allowed project" via
  `POST /api/v4/projects/{id}/job_token_scope/allowlist` — [GitLab CI/CD job token docs](https://docs.gitlab.com/ci/jobs/ci_job_token/) references this as a
  known/tracked issue rather than a live unpatched hole; flagging it here as a primary-source
  caveat rather than treating the allowlist as an absolute boundary. Not something ocx can
  mitigate — it's the index-repo operator's configuration to get right, and the "named error
  citing the missing setting" the dossier already requires (decision 9 / open question on exit
  codes) is the correct ocx-side response when the allowlist rejects the push.
- **`JOB-TOKEN` header vs `Authorization: Bearer`.** GitLab's REST auth docs (fetched by the
  discussion's own research, corroborated here) confirm PATs/project/group tokens and OAuth all
  use `Authorization: Bearer`; only a CI job token uses the dedicated `JOB-TOKEN` header, and
  only on the allowed-endpoint subset. **Selecting the header by value-equality against the
  environment's own `CI_JOB_TOKEN`** (rather than a user-facing "kind" flag) is safe *given* the
  dossier's stated precondition that this only ever runs "inside a GitLab CI job" — the
  equality check can only ever be true if the process is actually inside the job whose
  `CI_JOB_TOKEN` it's comparing against (an attacker who wants ocx to send `JOB-TOKEN` for a
  token that *isn't* the job's own would first need to set `CI_JOB_TOKEN` in the environment to
  match their chosen value, at which point they already control the environment ocx runs in and
  have no need to trick the header choice). No externally-documented risk found with this
  approach; it degrades gracefully to `Authorization: Bearer` for every other token kind, which
  is the safe default per GitLab's docs. **Deploy tokens cannot call the API at all** (confirmed
  by the discussion's own fetch) — the dossier already routes those through
  `OCX_ANNOUNCE_GIT_TOKEN` correctly, since they only need the push half.
- **GitHub `GITHUB_TOKEN`.** Default in "trusted" (non-fork-PR) contexts is read/write across
  most scopes, restricted to read-only automatically for workflows triggered by a fork's pull
  request — [GitHub Actions permissions docs](https://docs.github.com/en/actions/writing-workflows/choosing-what-your-workflow-does/controlling-permissions-for-github_token). It authors as `github-actions[bot]`
  because it is an installation/app-scoped token, not a user token — there is no GitHub
  equivalent of "the token carries the triggering human's identity," which is exactly why the
  dossier correctly scopes the git-transport/job-token-authorship story to GitLab only and
  leaves GitHub's "author = invoker" case as an open indexbot-governance question rather than
  something ocx can solve today.
- **Trend, not settled fact: immutable OIDC subject claims.** GitHub shipped immutable
  subject-claim support for Actions OIDC tokens, GA for new repos from **2026-07-15** — [GitHub changelog, 2026-04-23](https://github.blog/changelog/2026-04-23-immutable-subject-claims-for-github-actions-oidc-tokens/), corroborated by [Microsoft Entra migration guidance](https://learn.microsoft.com/en-us/entra/workload-id/workload-identities-github-immutable-subjects>). This is adjacent, not
  directly applicable to `GITHUB_TOKEN`-authored PR authorship, but it is the closest analog to
  the "author = invoker, verifiably" property GitLab's job-token git-push already gives —
  worth a forward-pointer in the indexbot governance issue the dossier already carries as an
  open question (owner adds owners / GitHub invoker identity).

### 5. Owner identity

- **`bot`/`type` fields are documented API fields, not heuristics.** GitLab's Users API returns
  a `"bot": false/true` field per user — [GitLab Users API docs](https://docs.gitlab.com/api/users/), corroborated by a tracked feature
  request to add a bot-only query filter, [GitLab issue #267140](https://gitlab.com/gitlab-org/gitlab/-/issues/267140), and GitLab's dedicated
  service-accounts doc confirms "service accounts are always marked as external users" —
  [GitLab service accounts docs](https://docs.gitlab.com/user/profile/service_accounts/). GitHub's `type: "Bot"` on the Users API is likewise a first-class,
  documented field. The dossier's exit-64 refusal on a bot identity is checking real,
  server-asserted fields, not a name-pattern heuristic — this is the right implementation choice
  over (say) regex-matching a login for `-bot`/`[bot]` suffixes.
- **What the bot check does *not* catch.** A **classic personal access token** minted by a human
  on a shared/service "release" account is indistinguishable from a real human via `bot`/`type`
  — GitLab's own service-accounts doc frames "service accounts" as a *separate*, explicitly
  opted-in account type precisely because plenty of automation still runs as an ordinary human
  account with `bot: false`. The dossier's posture (c) — project/group access token, ownership
  managed by repository access, `--owner` names whoever the operator passes — is the correct
  design response to this gap: it does not rely on the bot check to prevent
  "automation-pretends-to-be-a-person" impersonation, because that's a policy/credential-
  provisioning decision the reviewer (a human, per G-04) is trusted to catch by looking at *who*
  is listed in `--owner`, not by trusting the MR author field.
- **Login → numeric id resolution reliability.** GitHub's `GET /users/{username}` and GitLab's
  `GET /users?username=` are the documented, stable primary lookups; no evidence found that
  either forge exposes a *bulk* rename-history endpoint for third-party lookup. GitHub
  explicitly **retires** an owner+repo name combination when the repo crosses a popularity
  threshold (Marketplace action, or >100 clones/Action-uses in the preceding week) — [GitHub username reference docs](https://docs.github.com/en/enterprise-cloud@latest/account-and-profile/reference/username-reference)
  — but this retirement is *repository*-scoped, not *account*-scoped: a renamed GitHub account's
  **old login stops resolving to that account** (the old login becomes available for a
  different person to claim, unless retired), while the **numeric id is immutable** and
  continues to resolve via `GET /users/{username}` → `id` at claim time, or directly via
  `GET /user/{id}` if ocx ever needs id-first lookup. This is the precise justification for the
  dossier's design: resolve at claim time and persist `login:id`, never `login` alone, because a
  `login` can be recycled to a *different* human after the original owner renames — a stale
  `login`-only root would then misattribute ownership to whoever claims the freed name. No
  primary-source confirmation was found of GitLab recycling freed usernames the same way (GitLab
  forum threads on renaming exist, e.g. [GitLab forum: change username and email](https://forum.gitlab.com/t/change-username-and-email-in-all-aspects-of-the-instance/85298), but none
  confirm or deny reuse of a freed GitLab username by a different account) — **negative
  finding**: GitLab's username-recycling behavior after a rename is not confirmed by any primary
  source found in this pass; treat the `login:id` persistence requirement as justified by the
  GitHub case alone, which is sufficient, and do not claim a GitLab-specific recycling risk that
  wasn't verified.
- **Impersonation via `--owner`.** Since `--owner <login>` (without `:id`) triggers a live
  lookup against the forge at claim time, an operator naming the wrong login only ever adds the
  *real* account behind that login as an owner — there is no separate "impersonate as" surface
  here beyond "the operator typed the wrong name," which is a human-reviewer catch, not a
  cryptographic one. **What the reviewer needs to see** (recommendation): the rendered PR/MR
  body should show the resolved `login:id` pair for every owner, not just the login the operator
  typed — so a reviewer can visually confirm "this is who I think it is" against the forge's own
  profile link, catching a typo'd-but-still-valid login before merge. This is not yet stated as
  a requirement in the dossier's `--format json`/root-rendering description and is a low-cost
  addition worth folding into the ADR's UX section.

### 6. Temp clone hygiene

- **`tempfile` crate (already an ocx dependency per the council's simplicity seat — zero new
  deps).** `TempDir` cleans up via `Drop`, which — as multiple sources note — **does not run**
  on an unhandled signal (SIGKILL always, SIGTERM/SIGINT if not caught) or on `abort()`/panic
  unwinding disabled — general Rust destructor-on-signal caveat, corroborated across tempfile
  ecosystem discussion. `child_process.rs`'s existing `spawn_and_wait` already forwards
  SIGINT/SIGTERM to the child and *returns* rather than diverging specifically so the caller can
  run Drop-based cleanup afterward — the module's own doc comment states this is "what makes
  [`spawn_and_wait`] ... Use this instead of [`exec`] when RAII guards must run after the child
  exits." **The claim/announce git-transport path must use `spawn_and_wait`, never `exec`, for
  every subprocess it runs against the temp clone** — this is already an explicit contract in
  the existing code, just worth restating as a hard requirement in the ADR since it's easy to
  reach for `exec`'s simplicity by mistake on a code path that until now never needed a
  cleanup-after-child guarantee. A SIGKILL (uncatchable) still leaves the tempdir on disk; that
  residual is accepted by every tool surveyed (cargo, git itself) and is not a security issue on
  its own — the directory contains no long-lived secret at rest (see below) — only a disk-hygiene
  one, appropriately out of scope for a security ADR beyond noting it.
- **Permissions/umask.** `tempfile::TempDir` creates directories with the platform-default mode
  under the process umask (typically `0700` effective on most Linux distros' default umask of
  `022` only if the crate itself restricts it — recommendation: verify at implementation time
  that the crate's directory-creation path does not rely on umask alone, i.e. explicitly set
  `0700` via `std::os::unix::fs::PermissionsExt` after creation if the crate's default isn't
  already owner-only, since a shared multi-tenant CI runner with a permissive umask would
  otherwise make the clone (including the injected `GIT_CONFIG_VALUE_n`'s target file, if any
  local config file were ever written instead of using the pure-env approach) world-readable).
  This is CWE-732 (Incorrect Permission Assignment) territory; the dossier's "never in ...
  `.git/config`" already avoids writing the secret to any file inside the clone, which sidesteps
  most of this concern — the residual is only the *directory listing* of what was fetched
  (branch names, file contents of the index repo — not secret), so the severity here is low, but
  worth one line confirming the tempdir mode explicitly rather than trusting the umask.
- **Shallow/partial clone flags.** `--depth`, `--single-branch`, `--filter=blob:none` all reduce
  what's fetched — [git-clone docs on `--depth`](https://git-scm.com/docs/git-clone/2.47.0) confirm `--depth` implies `--single-branch` unless
  overridden. The council's premortem/operability seats already flag that `--depth 1` on the
  *compare* operation specifically is unsafe (reintroduces D13's "unclassifiable compare read as
  Identical") — that finding stands: shallow flags are a fine hygiene default for the clone used
  to *push*, but the clone used to *compute `compare_branch`* needs full history of both refs
  (as the dossier already specifies: "both refs fetched"), so `--depth`/`--filter` must not be
  applied to that fetch. This is already correctly scoped in the dossier's Requirements section;
  confirming no additional nuance was found that would change it.
- **`core.symlinks`, `safe.directory`.** `core.symlinks=false` is a documented hardening step
  against symlink-based writes escaping the worktree — [Snyk symlinks writeup](https://snyk.io/blog/symlinks-are-still-scary/) — relevant here because
  the cloned index repo's content is forge-controlled (any contributor's prior commits) even
  though the write path is a *fresh* branch off a known base; recommend passing
  `-c core.symlinks=false` on the clone as defense-in-depth against a symlink planted by a past
  commit in the repo's history being materialized on disk. `safe.directory` is an ownership
  check (CVE-2022-24765 mitigation) that matters when git runs as a different UID than the
  directory owner — since ocx creates and owns its own tempdir in-process, this should never
  trigger, but a CI runner that maps UIDs unusually (rootless containers) could still hit it;
  the fix is to ensure ocx creates the tempdir as the same user that will run the child `git`
  process, which is already implied by `spawn_and_wait` running in the same process tree — no
  code-level action needed, just confirmed as a non-issue given the current architecture.
- **Non-interactive/deterministic env.** `GIT_TERMINAL_PROMPT=0` stops git from ever falling
  back to an interactive credential prompt (which would hang a CI job or silently wait forever)
  — general git CI-hardening guidance corroborates this is the standard flag; `GIT_CONFIG_NOSYSTEM`
  additionally stops the system-wide `/etc/gitconfig` (which an operator does not control and
  could contain an unexpected `credential.helper` or proxy override) from being consulted at
  all, forcing every relevant setting to come from ocx's own `GIT_CONFIG_KEY_n`/`VALUE_n` and
  the temp clone's local config, which is exactly the "ambient config must still be honoured
  selectively" boundary the research question asked about: **proxy settings and CA settings
  (`http.proxy`, `http.sslCAInfo`) are the two categories that *should* still flow from the
  ambient/system config** (a corporate runner's proxy is legitimate and necessary, per the TLS
  section above), while **credential and identity settings should not** (a stray
  `credential.helper` or `user.email` in `/etc/gitconfig` should never silently participate).
  `GIT_CONFIG_NOSYSTEM=1` combined with explicit `-c http.proxy=...`/`GIT_SSL_CAINFO`
  passthrough (read once from the ambient environment and re-applied explicitly, not inherited
  wholesale) achieves exactly that split, and is the recommended env-construction rule for the
  child's `Env` in `child_process.rs` terms. `HOME` should be left as-is (needed for the system
  git to find `~/.gitconfig` only if that's desired — recommend **not** clearing `HOME`, since
  clearing it can break an operator's legitimate `~/.gitconfig`-level proxy/CA settings, which
  contradicts the "ambient proxy/CA must still be honoured" requirement above).

### 7. Supply-chain angle

- **Relying on `git` found via `PATH`.** No CVE or advisory found specific to "PATH-hijacked
  `git` binary in CI" as a named incident — this is a **negative finding**; the broader
  supply-chain literature searched (imposter commits, compromised Actions, runner credential
  theft) is about different attack classes, not about a malicious binary shadowing `git` on
  `PATH`. That absence is not the same as "not a risk": a CI image with an attacker-writable
  early-`PATH` directory (e.g. a compromised build step that writes to a directory earlier in
  `PATH` than `/usr/bin`) could still shadow `git`, and this is architecturally identical to any
  other PATH-hijack (CWE-427, Uncontrolled Search Path Element) — no git/CI-specific research
  found beyond the generic class.
- **How other tools handle it.** `cargo` invokes the system `git` (for git-dependency fetches)
  via `PATH` lookup with no absolute-path requirement and no version pin beyond a documented
  minimum; `gh` (GitHub CLI) does the same for its `gh repo clone` wrapper. No surveyed tool in
  this space requires an absolute path or pins a git binary hash — the ecosystem norm is to
  trust `PATH` resolution, consistent with trusting the CI image/runner in general (if an
  attacker can shadow `PATH` before ocx runs, they can already do far more damage than
  redirecting one `git` invocation, since they control the same process's ambient environment
  and every other tool invocation too). **Recommendation: match ecosystem norm — `PATH` lookup,
  no absolute-path requirement — but do fail closed with a named error before any network call**
  (the dossier already specifies this: "absent binary → named error at the start of a
  `--transport git` run, exit 69, before any network call"), which is the right-sized response;
  requiring an absolute path or a version/hash pin would be inventing a control the rest of the
  ecosystem does not have and that a CI operator has no standard way to configure, for a threat
  model (attacker already controls early `PATH` entries in your own job) where the marginal
  benefit is small.
- **Version pinning.** The dossier already requires git ≥ 2.31 (for `GIT_CONFIG_KEY_n`) as a
  correctness gate, not a security one; no additional version-specific *security* floor was
  found beyond "ship the CR-smuggling fix" (`credential.protectProtocol`, default-on since the
  patched release addressing CVE-2024-52006) — since ocx's chosen mechanism doesn't go through
  the credential-helper protocol at all (see §1), this particular protection is moot for ocx's
  own credential path, but still relevant if the *ambient* git config on the runner has any
  credential helper configured for the same host (defense-in-depth: an old git binary with the
  CR-smuggling bug is a risk to whatever *else* touches that runner, not specifically to ocx's
  injected header).

## Design Patterns Worth Considering

- **Env/config injection over credential helpers, always.** The single clearest pattern from
  this research: every credential-helper-protocol participant (GitHub Desktop, Git Credential
  Manager, GitHub CLI, Git LFS) has had a newline/CR-smuggling CVE in the last 18 months; no
  tool using pure `http.extraHeader`/`GIT_CONFIG_VALUE_n` injection has. Treat "never register a
  credential helper for the injected token" as an invariant, not an implementation preference.
- **Resolve-and-persist identity, never persist-a-name.** GitHub's own repo-retirement policy
  and immutable-subject-claim rollout are both responses to the same root problem the dossier's
  `login:id` decision already solves — a name can be recycled, an id is the durable key. This
  pattern generalizes: anywhere ocx persists a forge identity long-term (owners, future
  reviewer-allowlists), persist the id, resolve the login for display only, refresh on read
  where cheap.
- **Explicit transport, no inference except one narrow, named exception.** GitLab's own
  fine-grained-job-token GA and OIDC/immutable-claims work both point the same direction the
  council already converged on: an ambient CI credential is powerful and should be *opted into*
  by an explicit flag (`--transport git`), not detected and used implicitly — the dossier's "no
  fallback, ever" and "job-token pickup gated on explicit `--transport git`" match the direction
  the platforms themselves are moving, not just an ocx-internal preference.
- **Show the resolved identity, not just the input, in anything a human reviews.** Applies to
  the owner-rendering recommendation in §5 and generalizes to any future field resolved from an
  external system before being written into a human-reviewed artifact.

## Sources

- Git config env vars: [git-config docs](https://git-scm.com/docs/git-config), [Git 2.31 release notes](https://github.com/git/git/blob/master/Documentation/RelNotes/2.31.0.adoc)
- Clone2Leak primary writeup (fetched directly, 2025-01): [flatt.tech Clone2Leak](https://flatt.tech/research/posts/clone2leak-your-git-credentials-belong-to-us/)
- Clone2Leak press coverage: [BleepingComputer](https://www.bleepingcomputer.com/news/security/clone2leak-attacks-exploit-git-flaws-to-steal-credentials/), [The Hacker News](https://thehackernews.com/2025/01/github-desktop-vulnerability-risks.html), [SecurityWeek](https://www.securityweek.com/git-vulnerabilities-led-to-credentials-exposure/)
- `/proc/pid/environ` exposure: [env.dev security guide](https://env.dev/guides/env-vars-security), [procfs environ internals](https://codywu2010.wordpress.com/2014/09/14/procfs-environ-explained-in-depth-1/)
- Credential-in-argv / GIT_ASKPASS: [gitcredentials(7)](https://www.man7.org/linux//man-pages/man7/gitcredentials.7.html)
- GitLab push options: [GitLab push-options docs](https://docs.gitlab.com/topics/git/commit/#push-options-for-merge-requests), [GitLab MR !87020 (newline conversion)](https://gitlab.com/gitlab-org/gitlab/-/merge_requests/87020/commits), [gitlab-org/git#66](https://gitlab.com/gitlab-org/git/-/issues/66), [gitlab-org/gitlab#241710](https://gitlab.com/gitlab-org/gitlab/-/issues/241710)
- Git pkt-line limits: [git protocol-common docs](https://git-scm.com/docs/protocol-common)
- GitLab job token: [GitLab CI/CD job token docs](https://docs.gitlab.com/ci/jobs/ci_job_token/), [GitLab fine-grained job tokens GA (2025-08-26)](https://about.gitlab.com/blog/fine-grained-job-tokens-ga/)
- GitHub Actions token: [GitHub Actions permissions docs](https://docs.github.com/en/actions/writing-workflows/choosing-what-your-workflow-does/controlling-permissions-for-github_token), [GitHub immutable OIDC subject claims changelog (2026-04-23)](https://github.blog/changelog/2026-04-23-immutable-subject-claims-for-github-actions-oidc-tokens/), [Microsoft Entra migration guidance](https://learn.microsoft.com/en-us/entra/workload-id/workload-identities-github-immutable-subjects)
- Bot/service-account detection: [GitLab Users API docs](https://docs.gitlab.com/api/users/), [GitLab issue #267140 (bot filter request)](https://gitlab.com/gitlab-org/gitlab/-/issues/267140), [GitLab service accounts docs](https://docs.gitlab.com/user/profile/service_accounts/)
- GitHub username retirement: [GitHub username reference docs](https://docs.github.com/en/enterprise-cloud@latest/account-and-profile/reference/username-reference)
- TLS/CA divergence: [reqwest v0.13 rustls-by-default post](https://seanmonstar.com/blog/reqwest-v013-rustls-default/), [webpki-roots crate](https://github.com/rustls/webpki-roots), [GitLab Runner self-signed CA docs](https://docs.gitlab.co.jp/runner/configuration/tls-self-signed.html), [git http.sslBackend/schannel guidance](https://www.alertmend.io/blog/git-config-global-http-sslbackend-schannel)
- Temp dir hygiene: [tempfile crate docs](https://docs.rs/tempfile/), [git-clone `--depth` docs](https://git-scm.com/docs/git-clone/2.47.0), [Snyk symlink hardening writeup](https://snyk.io/blog/symlinks-are-still-scary/)
- Local (data, not instruction): `crates/ocx_lib/src/forge/http.rs`, `crates/ocx_lib/src/utility/tls.rs`, `crates/ocx_lib/src/utility/child_process.rs`, `Cargo.toml`, `.claude/rules/subsystem-cli.md` (Credential exemption table), `.agents/discussions/index-claim-command.md`, `.claude/artifacts/adr_announce_gitlab_forge.md` (D3, D9, D15), `.claude/artifacts/research_index_claim_council_transport.md`, `.claude/artifacts/research_index_claim_prior_art.md`

## Recommendation

Proceed with the dossier's transport/credential design as specified — it is already more
conservative than every prior-art tool surveyed and sidesteps the one live, multi-vendor CVE
class (credential-helper-protocol smuggling) entirely by never registering a helper. Add four
things to the ADR before implementation:

1. Name the `/proc/<pid>/environ` residual exposure of `GIT_CONFIG_VALUE_n` in the threat model
   (same class as the existing `OCX_ANNOUNCE_TOKEN` exposure — not a regression, but not zero).
2. State plainly that self-managed GitLab behind a corporate/internal CA is **not supported by
   either transport's REST half today** (reqwest has no CA-override knob at all), file it as a
   named follow-up issue, and don't let the git-transport work imply it fixes this.
3. Require `spawn_and_wait` (never `exec`) for every subprocess in the temp-clone lifecycle, an
   explicit `0700`/owner-only tempdir mode, `-c core.symlinks=false`, `GIT_TERMINAL_PROMPT=0`,
   `GIT_CONFIG_NOSYSTEM=1` with explicit re-application of ambient `http.proxy`/CA settings, no
   `--recurse-submodules` ever, and never set/forward `GIT_TRACE`/`GIT_CURL_VERBOSE` to the
   child.
4. Render `login:id` (not just `login`) for every owner in the PR/MR body and `--format json`
   output, so the human reviewer can visually confirm identity against the forge's own profile
   before merging.

None of these block the design; they are hardening additions and one explicit scope
acknowledgment, sized to land in the same ADR.

## negative:

- `credential.useHttpPath` is irrelevant to the chosen mechanism (no credential helper is used).
- No documented GitLab-specific push-option length limit beyond the generic git pkt-line ceiling
  (~65KB per line).
- No primary source found confirming or denying GitLab recycles a freed username after a
  rename — do not extend the GitHub-sourced recycling argument to GitLab without further
  verification.
- No CVE or named incident found for "PATH-hijacked `git` binary in CI" specifically; the risk
  is architecturally real (CWE-427) but not evidenced by a documented exploit in this space.
- No GitLab-documented server-side audit log specifically for push-option *values* was found;
  absence of evidence, not evidence of absence.
