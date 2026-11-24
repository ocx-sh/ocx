# Plan: ten open follow-up issues in one linear pull request

## Status

- **Plan:** plan_issue_batch_followups
- **State:** executing
- **Tier:** high
- **Active phase:** 1 — ten pipelines, one wave
- **Step:** awaiting /hex-finalize push gate
- **Feature branch:** `hex/issue-batch-followups`, cut from `origin/main` 9dd9e91e in the worktree `tri`
- **Last update:** 2026-11-24 (fix passes 1 and 2 plus one release-gate fix pass merged; integration gate green on the final tree; last delta review: Needs Work on two Warn items carried to the pull request as residue; loop cap reached)
- **Reviewed:** e926094d
- **Next:** the owner approves the push at the /hex-finalize gate

## Pipeline status

| Pipeline | Issue | Wave | Depends on | Marks | Status |
|---|---|---|---|---|---|
| `import-damaged` | #774 | 1 | — | hard | merged |
| `export-locks` | #772 | 1 | — | hard | merged |
| `website-schemas` | #773 | 1 | — | hard | merged |
| `auth-lone-user` | #762 | 1 | — | hard | merged |
| `login-windows-home` | #763 | 1 | — | hard | merged |
| `auth-registry-fixture` | #764 | 1 | — | hard | merged |
| `contract-env-record` | #765 | 1 | — | hard | merged |
| `secret-read-once` | #766 | 1 | — | hard | merged |
| `secret-ref-followups` | #767 | 1 | — | hard | merged |
| `git-bash-followups` | #755 | 1 | — | hard | merged |

Every pipeline carries `hard` by owner instruction (one Opus agent per issue).

## Context

Ten open issues are small follow-ups left over from three recent changes
(export/import [#771](https://github.com/ocx-sh/ocx/pull/771), secret references
[#756](https://github.com/ocx-sh/ocx/issues/756), Git Bash activation
[#754](https://github.com/ocx-sh/ocx/pull/754)). The owner wants them closed in one
pass: one Opus agent per issue in parallel, one `/hex-review` + fix loop over the
merged result, then `/hex-finalize` into a pull request ready to merge and release.

Owner decisions taken at planning (2026-11-23):

- **Series shape:** one commit per issue, linear. Overrides the `hex.md` "squash to
  one commit" preference for this run only.
- **[#755](https://github.com/ocx-sh/ocx/issues/755):** everything in this repo
  (items 1–10). Item 11 lives in `ocx-sh/www-setup` and becomes an issue there.
  This is the owner's grant to dispatch `verify-deep.yml` against the branch for
  the Windows proof.
- **[#765](https://github.com/ocx-sh/ocx/issues/765):** document only. No gate change.

## Shape

- Feature branch `hex/issue-batch-followups`, cut from `origin/main` (`9dd9e91e`),
  checked out in this worktree (`tri`).
- Plan artifact for `/hex-execute`: `.claude/artifacts/plan_issue_batch_followups.md`
  (this plan, with a `## Status` block and the table below as its Parallelization
  table); `.claude/state/current_plan.md` repointed to it.
- **No contract wave**: the ten pipelines share no contract, only file ownership.
- Ten pipelines, one wave, each **one step run by one Opus agent** (owner
  instruction; overrides the hex "hard on 1 pipeline in 4" cap) in its own worktree
  `.agents/worktrees/<slug>` on branch `hex/issue-batch-followups--<slug>`.
- Memory: all ten start together with `CARGO_BUILD_JOBS=3`; each worktree is seeded
  with the warm `target/` and the `external/` submodule trees (learned 2026-11-19).
  If the host swaps, the orchestrator holds back the next cold build, never raises
  the cap.

## Pipelines

| # | Slug | Owns (must edit) | Proof the agent runs | Commit subject (changelog line) |
|---|---|---|---|---|
| [#774](https://github.com/ocx-sh/ocx/issues/774) | `import-damaged` | `crates/ocx_package_manager/src/tasks/import.rs` (`copy_into_home` :810, call sites :223 strict, :786 lenient); `test/tests/test_package_import.py` | unit + one acceptance case | `fix(import): a damaged patch descriptor layer in an export is a data error instead of "not found"` |
| [#772](https://github.com/ocx-sh/ocx/issues/772) | `export-locks` | `crates/ocx_oci/src/blob_lookup/export_marker.rs` (`merge_registry` :129, `ExportMarkerError`); test in `tasks/export.rs` | `cargo test -p ocx_oci -p ocx_package_manager` (filtered while iterating) | `fix(export): a registry whose directory name is "locks" is refused at export` |
| [#773](https://github.com/ocx-sh/ocx/issues/773) | `website-schemas` | `website/site.bzl` (:176), `website/BUILD.bazel` (`_SITE_SRCS`), `website/tests/schemas-index.test.mjs` | confirm on `origin/main` first; `task website:bazel:build` red then green | `fix(website): the Bazel site build includes the schemas index page` |
| [#762](https://github.com/ocx-sh/ocx/issues/762) | `auth-lone-user` | `crates/ocx_oci/src/auth.rs` (:199–203), `crates/ocx_env/src/vars.rs` (:500 doc), `website/.../guides/authenticate.mdx` (:61, :122, :130) | `cargo test -p ocx_oci auth` | `fix(auth)!: OCX_AUTH_<REGISTRY>_USER without a token is a configuration error and no credential helper runs` |
| [#763](https://github.com/ocx-sh/ocx/issues/763) | `login-windows-home` | submodule `external/docker_credential/src/lib.rs` (:121, :242) + gitlink | pure resolver test, Windows-shaped env, red then green on Linux | `fix(auth): on Windows a credential stored by ocx login is found when HOME is not set` |
| [#764](https://github.com/ocx-sh/ocx/issues/764) | `auth-registry-fixture` | `test/src/zot.py`, `test/conftest.py`, new static htpasswd fixture, `test/BUILD.bazel` (`module_data`), `test/tests/test_secret_reference.py`, `test/lint/test_zot.py` | the module under pytest and its Bazel target | `test(auth): a pull with a token read from a file is proven against a registry that requires authentication` |
| [#765](https://github.com/ocx-sh/ocx/issues/765) | `contract-env-record` | `.claude/artifacts/adr_ocx_interface_contract.md` (§ Env contract, § Communicating breaks), `.claude/rules/subsystem-interface-contract.md` (IC-16, semantic-break checklist), `crates/ocx_schema/contract/ledger.toml` header comment | `task claude:tests` | `chore(contract): record how a change in meaning of an OCX_* variable is published` |
| [#766](https://github.com/ocx-sh/ocx/issues/766) | `secret-read-once` | `crates/ocx_package_manager/src/tasks/{sign,attest}.rs`, `crates/ocx_cli/src/command/{package_sign,package_attest,package_push,package_announce,package_claim}.rs` | counting test: N items, one password read | `fix(sign): signing many items reads the key password once, and announce reads its credentials off the async runtime` |
| [#767](https://github.com/ocx-sh/ocx/issues/767) | `secret-ref-followups` | `crates/ocx_env/src/{lib.rs,reference.rs,tests.rs}`, `crates/ocx_schema/src/docs/render.rs` (`SECRET_REFERENCES` :658), `crates/ocx_announce/src/forge/credentials.rs` (test helper :1027) | `cargo test -p ocx_env -p ocx_announce -p ocx_schema` | `docs(env): the environment reference states how a byte order mark, a non-Unicode value and exec --env treat a secret reference` |
| [#755](https://github.com/ocx-sh/ocx/issues/755) | `git-bash-followups` | `crates/ocx_shell/**`, `crates/ocx_setup/src/lib.rs`, `crates/ocx_package_manager/src/activation/**` (`plan.rs`, `activation.rs`), `test/tests/test_toolchain_activate.py`, `test/src/shell_matrix.py`, `test/limits/windows/SKIP_CEILING`, `.github/workflows/basic-repo.yml` (`unit-native`), `website/.../shell-integration.mdx`, `environments.mdx`, `troubleshooting/shell.mdx` | Linux: simulated `Dialect::Msys` unit + live-bash tests; Windows: CI only | `fix(shell): Git Bash and MSYS2 activation removes PATH entries without regard to case, keeps its ledger equal to the shell, and is tested on Windows` |

### Ownership rules that keep the ten file-disjoint

- `tasks/sign.rs` belongs to #766; the missing doc comment on `build_signer_blocking`
  (a #767 bullet) is written there. #767 does not open that file.
- #767 states the `exec --env` behaviour in `render.rs` only. It does **not** reword
  the `--env` help (`options/env_override.rs`), so no ledger entry is needed.
- Generated files are nobody's: `reference/environment.md`, `cli.json` and the
  schema goldens are regenerated once by the orchestrator after the merges
  (`task website:reference:generate`, schema generation).
- `authenticate.mdx` belongs to #762; `test_secret_reference.py` to #764;
  `ledger.toml` to #765.

### Defaults handed to the agents (each is a decision, stated in its commit body)

- **#762:** a lone `USER` is a configuration error naming the missing `TOKEN`; never
  anonymous, never the helper. The three doc passages are aligned, including the
  contradiction at `authenticate.mdx:130`. Subject carries `!`.
- **#763:** the reader resolves the directory the way `ocx_env::home_dir` does
  (`USERPROFILE` on Windows). The resolution becomes a pure function of an env
  lookup and a host flag so the red state is shown on Linux. Commit lands on the
  fork branch `ocx/integration`; the gitlink bump lands here.
- **#764:** zot with a committed pre-hashed htpasswd (no bcrypt dependency);
  readiness probe accepts 401 for this instance; the test uses `TYPE=basic`,
  `USER`, `TOKEN=file://<path>`. Verify the config keys against zot 2.1.21 first.
- **#767:** byte order mark is already stripped (`reference.rs:355`): document it.
  `exec --env NAME` keeps passing the raw text: document it. An `env://` target
  that is not valid Unicode stays an unresolvable-reference failure: add the test.
  A non-Unicode value with no prefix stays unset: document it.
- **#755:** item 3 fix (take the lock's own `set`); item 4 keep converting every
  path-kind variable, document it; item 5 make plan admission dialect-aware so the
  ledger matches the shell; item 6 keep accepting a POSIX-absolute entry, pin it in
  a test and the docs; item 7 case-insensitive removal gated on the MSYS dialect;
  item 8 document the mount-prefix limit; item 9 review fish, nushell and elvish
  emitters and fix what the same defect class reaches; item 10 `ocx self setup`
  prints a hint when a Git Bash home differs; item 1 port the 20 skipped tests and
  lower `SKIP_CEILING`; item 2 add zsh to the Windows `unit-native` live-shell list
  (the cheaper route, no new job). Item 11 → new issue in `ocx-sh/www-setup`.

Every agent follows the bug-fix sequence (reproduce → root cause → failing test →
fix), runs only the narrowest tests it touched, commits with `--no-verify` using the
subject above and `fixes ocx-sh/ocx#<n>` in the body, and returns instead of waiting.
A same-step failure retries once on Opus with the failure attached, then is deferred
as residue.

## After the pipelines

1. **Merge** each pipeline branch onto the feature branch, one at a time,
   `--no-ff --no-verify`, `cargo check` after each. Remove its worktree and branch.
2. **Regenerate** the generated files once and commit them.
3. **Integration gate ∥ `/hex-review`** (one call, tier auto; the security seat fires
   on `crates/ocx_oci/**` and `.github/workflows/**`; the cross-model adversary runs
   under whichever seat is available, reported as `ran` or `absent`).
   Gate = what the series touched, then `task verify:mark`:
   - `task verify:scoped --force`
   - `cargo test` for `ocx_oci`, `ocx_env`, `ocx_package_manager`, `ocx_shell`,
     `ocx_setup`, `ocx_announce`, `ocx_schema`, `ocx_sdkgen`
   - acceptance modules: `test_package_import`, `test_package_export`,
     `test_secret_reference`, `test_toolchain_activate`, `test_login`
   - `task website:bazel:build` + `website/tests/schemas-index.test.mjs`, `task website:check`
   - `task claude:tests`; `task cross:build TARGET=x86_64-pc-windows-gnullvm`
   - `OCX_MIRROR_DIR=/home/mherwig/dev/ocx-sh/ocx-mirror task satellite:verify`
     (ecosystem crates `ocx_oci`, `ocx_env` are touched)
4. **One fix pass serves both**, Sonnet fixers per pipeline in parallel, gate again;
   at most two passes, residue is reported, not hidden.
5. **`/hex-finalize`**: rebase onto the fetched `origin/main`, recompose into ten
   commits in the table's order (bugs, then tests, then the chore), run the
   message-matches-diff check, stop at the push gate.
6. **After the owner approves the push**: push the `docker_credential` fork commit
   first, then the branch; open one pull request closing the ten issues; watch
   `verify-basic` (rerun infrastructure failures once); dispatch `verify-deep.yml`
   against the branch for the Windows proof of #755 items 1 and 9, watch fail-fast,
   fix and re-dispatch on red. File the `www-setup` issue for #755 item 11.

## Verification

- Each issue's acceptance list is checked off in the pull request body with the test
  name that proves it; every new check is shown red before its fix.
- Scope stated beside each result: Linux results are local; every Windows claim
  (#763 on the `unit-native` leg, #755 items 1, 2, 3, 9) is proven only by the named
  CI run, and is reported as unproven until that run is green.
- Done = pull request open, `verify-basic` green, the deep run green, ten issues
  linked. The merge stays the owner's.

## Risks

- #755 is large and Windows-only in parts: expect more than one CI round-trip.
- #762 changes behaviour for anyone who set only `USER` and relied on a helper.
- #763 needs a push to the `ocx-sh/docker_credential` fork before CI can resolve
  the gitlink.
- Ten cold Rust builds at once: bounded by the job cap, watched for swap.
