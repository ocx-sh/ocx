# Goal: "Secrets from a file or another variable (ocx-sh/ocx#756)"

Source: .agents/discussions/auth-token-file.md · Written: 2026-11-23 by /hex-loop

## Definition of done

- [x] "Four value forms: literal, file://, env://, plain://" — evidence: `b63624aa` with the resolution table in `crates/ocx_env/src/tests.rs` (`93d47b47`) (pre-finalize SHAs on `hex/auth-token-file`; `task verify:scoped --force` exit 0 at `242abbe4`).
  Scope: `file://<path>`, `env://<NAME>` and `plain://<text>` resolve as the source's first requirement states; any other value is the literal, one containing `://` elsewhere included; the three prefixes match case-insensitively; no other scheme is recognised.
- [x] "Every secret variable resolves through SecretVar" — evidence: `f469f626` and the declaration invariant tests in `crates/ocx_env/src/vars/tests.rs` (`57df7843`) (pre-finalize SHAs on `hex/auth-token-file`; `task verify:scoped --force` exit 0 at `242abbe4`).
  Scope: The rule lives at `SecretVar` in `crates/ocx_env/src/lib.rs` and covers the seven secret variables declared in `crates/ocx_env/src/vars.rs`; a secret variable declared later gets it with no extra work.
- [x] "User variables opt in, stay non-secret" — evidence: `ReferenceVar` on `OCX_AUTH_{REGISTRY}_USER` and `OCX_ANNOUNCE_GIT_USERNAME` in `crates/ocx_env/src/vars.rs` (`f469f626`), held by `57df7843` (pre-finalize SHAs on `hex/auth-token-file`; `task verify:scoped --force` exit 0 at `242abbe4`).
  Scope: `OCX_AUTH_{REGISTRY}_USER` and `OCX_ANNOUNCE_GIT_USERNAME` accept the same forms through a declaration property of its own; they are not re-declared as secret, so a username still appears in diagnostics.
- [x] "Other, path-valued and foreign variables untouched" — evidence: the `accepts_reference` invariant tests (`57df7843`) and `4e72a94b` (pre-finalize SHAs on `hex/auth-token-file`; `task verify:scoped --force` exit 0 at `242abbe4`).
  Scope: A non-secret variable that did not opt in keeps `file://…` as its literal value; a path-valued variable cannot opt in; foreign variables OCX merely reads are never interpreted.
- [x] "One-step resolution, no chaining, no error" — evidence: `test_a_reference_is_resolved_one_step_only` in `test/tests/test_secret_reference.py` (`0b1c5a19`) and the guide text (`3abd6ccf`) (pre-finalize SHAs on `hex/auth-token-file`; `task verify:scoped --force` exit 0 at `242abbe4`).
  Scope: File content and an `env://` target's raw value are taken literally and not interpreted again, a declared OCX secret variable as target included; no error and no warning fires; the docs state that references do not chain.
- [x] "Reference checks, each with its own error" — evidence: `b63624aa`, `93ab9dc3`, and `test_an_invalid_reference_fails_the_pull_with_a_config_error` (pre-finalize SHAs on `hex/auth-token-file`; `task verify:scoped --force` exit 0 at `242abbe4`).
  Scope: The `env://` name checks and the `file://` path checks listed in the source's Requirements, with a malformed name worded differently from an unset one; file mode, owner and a directory allowlist are deliberately not checked.
- [x] "file:// spelling matches FileReference" — evidence: the path edge rows of the resolution table (`93d47b47`) and the device-path refusal (`13946e11`) (pre-finalize SHAs on `hex/auth-token-file`; `task verify:scoped --force` exit 0 at `242abbe4`).
  Scope: The spelling `FileReference` in `crates/ocx_util/src/fs/path.rs` parses: case-insensitive prefix, the rest verbatim, so drive-letter and UNC paths work; single-colon `file:` stays unsupported.
- [x] "Bounded regular-file read; trailing CR/LF stripped" — evidence: `b63624aa` and the cap, non-regular and line-ending rows of the resolution table (`93d47b47`) (pre-finalize SHAs on `hex/auth-token-file`; `task verify:scoped --force` exit 0 at `242abbe4`).
  Scope: A 64 KiB cap, regular files only after following symbolic links; trailing carriage returns and line feeds are stripped, other trailing whitespace is kept.
- [x] "Unresolvable reference is a hard error, never anonymous" — evidence: `336a4821`, `39932d7a`; `test_a_missing_token_file_fails_the_pull_instead_of_going_anonymous` failed at `f469f626` (`assert 0 == 74`, the anonymous pull succeeded) and passes at `984e1589` (pre-finalize SHAs on `hex/auth-token-file`; `task verify:scoped --force` exit 0 at `242abbe4`).
  Scope: Its own classification; for a registry credential it never becomes the warning-plus-anonymous fallback of `get_or_fallback` in `crates/ocx_oci/src/auth.rs`; exit codes follow the split in `crates/ocx_trust/src/key_ref.rs`.
- [x] "Errors print the reference, never a value" — evidence: `test_a_missing_user_file_fails_the_pull_without_leaking_the_token` and `test_readable_user_and_token_files_let_the_pull_succeed_without_printing_either` (`1cc26024`) (pre-finalize SHAs on `hex/auth-token-file`; `task verify:scoped --force` exit 0 at `242abbe4`).
  Scope: An error may name the path or the variable; it never prints a literal value or file content.
- [x] "Three existing bounds stay in place" — evidence: `4e72a94b` (a package cannot plant an `OCX_AUTH` token reference) and the cap and regular-file rows of the resolution table (`93d47b47`) (pre-finalize SHAs on `hex/auth-token-file`; `task verify:scoped --force` exit 0 at `242abbe4`).
  Scope: Project and package environment surfaces still cannot set `OCX_*` (`is_reserved_ocx_key`), the size cap holds, and the read stays regular-files-only.
- [x] "Lookup stays in Auth; no new crate" — evidence: `git diff origin/main -- scripts/crate_map.toml` is empty at `984e1589`; the fail-closed lookup is in `crates/ocx_oci/src/auth.rs` (`39932d7a`) (pre-finalize SHAs on `hex/auth-token-file`; `task verify:scoped --force` exit 0 at `242abbe4`).
  Scope: Per-host credential lookup keeps going through `Auth` in `crates/ocx_oci/src/auth.rs`; `scripts/crate_map.toml` gains no member.
- [x] "Docs state the model once, with a batch-job example" — evidence: `website/src/content/docs/guides/authenticate.mdx` (`3abd6ccf`) and the generated environment reference (`51e74b31`, `3e0588e0`) (pre-finalize SHAs on `hex/auth-token-file`; `task verify:scoped --force` exit 0 at `242abbe4`).
  Scope: `website/src/content/docs/guides/authenticate.mdx` gives the order (`OCX_AUTH_<HOST>_*`, then Docker config and helpers, then anonymous) and a batch-job example with an ACL-locked token file; the generated `website/src/content/docs/reference/environment.md` shows the value forms.
- [ ] PR merge-ready (DONE block only) — evidence: the PR URL, and the
  PR is not a draft.
- [ ] CI green per job (DONE block only) — evidence: every check run on
  the PR head SHA with its conclusion; every skipped job states its
  skip reason (its `if:` or path filter).
  A skip with no reason is not green.
- [ ] Deep verify passed (DONE block only) — evidence: the full documented verification result.

## Autonomy

- Prompting: never — no question waits for a human; a doubt runs
  § Issue resolution.
- Granted acts (authority: the pasted prompt): none
- Forbidden or narrowed acts: None.

## Issue resolution

Every doubt — an ambiguous requirement, a design question, a failure
with an unclear cause — runs this protocol:

1. Delegate the research to a sub-orchestrator.
2. Record question → research → decision in this file or in the PR.
3. Defer to a GitHub issue only in hard cases — the research ends with no
   decision, or the decision needs an act outside the grants; the issue
   link is the recorded decision.
4. Every doubt not deferred ends in an action.
5. A pre-existing failure that blocks done is in scope and runs this
   protocol.
6. Findings outside this goal's scope become follow-up issues.
7. Secrets and credential-bearing logs are never written to any committed
   file, commit message, PR text, PR comment or review comment, or issue.
   Security findings are never filed as issues:
   this file records them by reference only — location and class, no
   secret value or exploit detail — and only the DONE block reports them
   in full.

### Recorded decisions

- 2026-11-23, after `/hex-plan` (`.claude/artifacts/plan_secret_references.md`, commit `684bf6e7`). The
  plan's own docket answers and divergences (its § Decisions, D-7, D-8, D-10, and the `!` subject for the
  `cli.json` `accepts_reference` field) are accepted as recorded there.
- Question: the plan's act M-1 upgrades four read sites in `ocx-sh/ocx-mirror` in the same change series,
  and the pasted prompt grants no act in another repository. Research: the plan's trade-off table rejects
  a source-compatible `get` because the anonymous fallback stays reachable by omission; the satellite job
  in `.github/workflows/deep-repo.yml` is deep-tier and `continue-on-error`, so no pull-request check
  depends on it. Decision: keep the breaking signature. This run prepares the mirror upgrade as a patch
  file under `.claude/artifacts/`, proves it with `task satellite:verify` against a scratch clone of the
  mirror, and defers landing it in `ocx-sh/ocx-mirror` to an issue on this repository. Nothing in the
  mirror repository or its checkout is changed.
- Follow-up candidates from planning (F-1, F-2, F-4, F-5, F-6 in the sub-orchestrator's report) become
  issues on this repository before the DONE block; F-3 is closed by `adr_key_reference_grammar.md`.
- 2026-11-23, after `/hex-execute` (plan `State: review`; review round 2 left no Block and no Warn open;
  `task verify:scoped --force` exit 0 at `242abbe4`). Deferrals and follow-ups, each an issue on this
  repository: the ocx-mirror upgrade (the M-1 deferral above; patch at
  `.claude/artifacts/ocx_mirror_secret_references.patch`, `task satellite:verify` exit 0 against a scratch
  clone) https://github.com/ocx-sh/ocx/issues/761; a lone `_USER` and the credential helpers
  https://github.com/ocx-sh/ocx/issues/762; the Docker config directory on Windows
  https://github.com/ocx-sh/ocx/issues/763; an authenticated registry fixture
  https://github.com/ocx-sh/ocx/issues/764; the contract ledger and environment variables
  https://github.com/ocx-sh/ocx/issues/765; secret reads on async paths
  https://github.com/ocx-sh/ocx/issues/766; small review suggestions
  https://github.com/ocx-sh/ocx/issues/767.
- Question: the open Warn "a permission-denied read exits 74, not 77". Research: the key-file read this
  goal names as the model maps every I/O error to 74 (`key_backend_row` in
  `crates/ocx_sign/src/sign/error.rs`); `crates/ocx_trust/src/key_ref.rs` reads no file. Decision: no
  change, the secret reference read already matches.
- Question: `/hex-finalize` pre-flight halt (2) refuses a checkout that is not the first worktree entry,
  and this run's branch lives in a linked owner worktree. Research: the halt exists so a run never
  rewrites a branch its session did not open; this session created `hex/auth-token-file` in this
  checkout, the checkout is the owner's own long-lived worktree and not one under `.agents/worktrees/`,
  and the halt's `Fix:` cannot run because the first worktree entry holds the owner's `main`. The same
  friction is already in the retro inbox (entry of 2026-11-16). Decision: finalize from this checkout and
  disclose it at the gate.
- Question: halt (3) fires on `.serena/project.yml`, a tracked file a tool rewrote, which this run may
  neither commit nor discard. Decision: mark it `skip-worktree` for the length of the finalize run and
  clear the mark afterwards; its content is never staged, stashed or changed.
- Question: the release gate's level. Research: `workflow-git.md` § Verification Levels gives finalize
  "what the series touched, then `task verify:mark`; CI is the full gate", and `hex.md` Preferences
  withhold the deep dispatch. Decision: `task verify:scoped --force` on the final tree is the local
  release gate; the pull request's own checks are the remote gate; no workflow is dispatched.
- Security-class findings outside this goal's scope, by reference only (no issue; the DONE block reports
  them): two fail-open paths in `crates/ocx_oci/src/auth.rs` that predate this work and involve no
  reference; a missing deadline on the bounded file read in `crates/ocx_env/src/reference.rs`
  (availability); secret buffers are not zeroed after use (hardening).

## Loop shape

- Entry point: Run the /hex-plan skill on "Secrets from a file or another variable (ocx-sh/ocx#756), per .agents/discussions/auth-token-file.md".
- Refinement rounds: 2 — counts outer cycles: each review ⇄ execute
  pass is one, and so is every failed repair or retry cycle — a red
  gate's fix pass, an execute or finalize retry, a post-finalize CI fix ⇄
  re-finalize pass. Inner review-fix rounds do not count, and their limit
  is untouched.
  Past `2`, the DONE block reports every remaining criterion `not met`
  and the run stops.
- Ticks: /hex-loop commits nothing. The session creates or switches to
  the branch the pasted prompt's I9 names and commits this file first on
  it. A box is ticked only between hex-mode runs, never during one, and
  each tick is committed at once; every tick lands before the final
  /hex-finalize, none after it. The `(DONE block only)` criteria are
  evidenced only in the closing DONE block, never ticked.

## Rules

None.

## Emphasis

From the source, as pointers; the source's own text governs.

- The source's `## Decisions` are the owner's closed positions: a reference in the value and no suffix variable, all secrets, the two user variables, `plain://` in, `env://` kept, no chaining and no error for it, the file read as a small std-only bounded read inside `crates/ocx_env`.
- The source's `## Open questions` are this run's docket, six entries, each with a recommended answer. Three of them are side defects in existing behaviour and are separate from this work.
- The source's `## Intent` lists what is out of scope: a command source, a `_TOKEN_FILE` suffix variable, `--env-file`, age, a new crate, index authentication itself, any change to `ocx login`.
- The fallback is the security point. An unresolvable reference must fail the command; a test shows the anonymous pull does not happen, and is shown red first.
- A literal that begins with one of the three prefixes changes meaning. That is an interface change to the `OCX_*` variables: environment reference, `cli.json`, and the contract-ledger question in the source's docket.
- `crates/ocx_env` stays std-only plus `ocx_exit`, and it is an ecosystem crate: a change to its API obliges `task satellite:verify`.
- The source's `## Verification` names the test table at the `SecretVar` seam and the cases in `crates/ocx_oci/src/auth.rs`.

## Context

- Source discussion: `.agents/discussions/auth-token-file.md` (untracked; read it from the working tree of this checkout).
- Issue: https://github.com/ocx-sh/ocx/issues/756
- Research, untracked; copy a file into `.claude/artifacts/` before an artifact cites it: `.agents/research/research_auth_token_file_codebase_recon.md`, `.agents/research/research_auth_token_file_prior_art.md`, `.agents/research/research_auth_token_file_vendor_credential_providers.md`, `.agents/research/research_auth_token_file_community_threads.md`, `.agents/research/research_auth_token_file_dual_transport_credential_models.md`, `.agents/research/research_auth_token_file_value_reference_vs_suffix.md`.
- Rules and decisions: `.claude/artifacts/adr_key_reference_grammar.md`, `.claude/artifacts/adr_ocx_login_credential_store.md`, `.claude/artifacts/adr_ocx_interface_contract.md`, `.claude/rules/subsystem-interface-contract.md`, `.claude/rules/subsystem-oci.md`, `.claude/rules/quality-security.md`.
- Code: `crates/ocx_env/src/lib.rs`, `crates/ocx_env/src/vars.rs`, `crates/ocx_oci/src/auth.rs`, `crates/ocx_trust/src/key_ref.rs`, `crates/ocx_util/src/fs/path.rs`, `scripts/crate_map.toml`.
- Docs: `website/src/content/docs/guides/authenticate.mdx`, `website/src/content/docs/reference/environment.md`.
- Satellite checkout: `/home/mherwig/dev/ocx-sh/ocx-mirror` (https://github.com/ocx-sh/ocx-mirror).
