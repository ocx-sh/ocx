# WP-8 — CI setup-ocx action (#388)

Branch `hex/issue-sweep--wp8`, base `52eecc15`. Files touched:
`.github/workflows/verify-basic.yml`, `.github/actions/setup-ocx/` (deleted).
`ocx.toml` is **unchanged** — see C-073.

## Contract results

| Contract | Verdict | Evidence |
|---|---|---|
| C-070 | **DONE** | `:47` → `ocx-sh/setup-ocx@25fa771f8572572dc64528db89560de68a163a0e # v1.4.0`. SHA re-verified independently (below). |
| C-070a | **DONE** | `cache: false` on both steps. `setup.ts:21` reads `core.getBooleanInput("cache")` *unconditionally*, so it lands on `:114` too, where it also disables the ocx-binary tool cache. |
| C-071 | **DONE** | `:54` → `run: task ci:actionlint`; the `ocx run` wrapper is gone and the `:51` comment moved with it. Confirmed the only `ocx run` in `.github/`, `taskfiles/`, `taskfile.yml` — remaining hits are `.claude/artifacts/*.md` prose. |
| C-071a | **DONE** | `version: '0.5.8'` on both steps. v0.5.8 ships `ocx-x86_64-unknown-linux-gnu.tar.gz` (asset list verified). The action needs only `ocx pull` + `ocx env --ci=github` (`>= 0.3.5` per `project.ts`), never `ocx exec`. |
| C-072 | **DONE** | `git rm -r .github/actions/setup-ocx` — 8 files. `.github/actions/build-rust` is untouched, so no generic directory reference breaks. **A third reference exists** — see Out-of-scope below. |
| C-073 | **DECLINED — the mechanism does not work at v1.4.0.** | Measured, both directions, on ocx 0.5.8 (the pinned release). Detail below. |
| C-074 | **DONE** | `:114` → same pinned action with `project: ''`. `project.ts:readProjectInputs()` returns `null` on an empty `project` ("Explicit opt-out"), giving binary-only mode — today's behaviour for the `ocx --remote package env` step. |

## Upstream verification (`gh api`)

SHA — lightweight tags, so `.object.sha` **is** the commit:

```
repos/ocx-sh/setup-ocx/git/ref/tags/v1.4.0
  {"ref":"refs/tags/v1.4.0","sha":"25fa771f8572572dc64528db89560de68a163a0e","type":"commit"}
repos/ocx-sh/setup-ocx/git/ref/tags/v1
  {"ref":"refs/tags/v1","sha":"25fa771f8572572dc64528db89560de68a163a0e","type":"commit"}
```

Every input passed exists in `action.yml` at that SHA (an input that does not exist is
silently ignored by Actions — this is the check that matters):

| Input | Declared upstream | Upstream default | We pass |
|---|---|---|---|
| `version` | yes | `"latest"` | `'0.5.8'` (both steps) |
| `cache` | yes | `"true"` | `false` (both steps) |
| `project` | yes | `"ocx.toml"` | `''` (`:114` only) |
| `groups` | yes | `""` | *not passed* (C-073 declined) |

## C-073 — why the group is declined

The contract's goal is "Workflow Lint pre-warms three tools rather than eight". The
upstream action's activation sequence (`project.ts:loadProject`) is:

```
3. ocx --project <file> pull [-g ...]      <- `groups:` reaches only this
4. ocx --project <file> env --ci=github    <- NO -g, and --pull is the default
```

`ocx env` root defaults to `[tools]` only, and "a tool missing from the local object
store is auto-installed as part of composition". So step 4 installs all eight
default-group tools whatever step 3 pre-warmed. Measured on ocx 0.5.8, clean object
store, `--no-pull` used to enumerate the composition set without downloading:

```
OCX_HOME=<empty>  ocx env --no-pull        -> 8 tools reported "not installed"
OCX_HOME=<empty>  ocx env --no-pull -g ci  -> 3
GITHUB_PATH=<f>   ocx env --ci=github      -> 8 PATH entries   (the action's step 4)
                  ocx pull -g ci --dry-run -> 3                (the action's step 3)
```

Total materialised is 8 either way. The group changes only *which step* pays, and adds
three `[group.ci]` entries to the lock.

Second, independent blocker — **C-073 cannot be delivered inside WP-8's file scope.**
`declaration_hash` covers `[tools]` and `[group.*]`, so adding the group invalidates
`ocx.lock`, which WP-8 does not own. Demonstrated red:

```
$ ocx pull --dry-run          # [group.ci.tools] added to ocx.toml, lock not regenerated
exit=65
ERROR ocx.lock is stale (ocx.toml changed since last `ocx lock`); run `ocx lock`
```

`ocx lock` fixes it (11 `[[tool]]` entries, 3 with `group = "ci"`), but that is an
`ocx.lock` write. Shipping the `ocx.toml` half alone would fail the action's step 3 and
red the whole Workflow Lint job.

Both experiments were reverted; `ocx.toml` and `ocx.lock` are byte-identical to `52eecc15`.

**The patch that would make C-073 real** is upstream, not here: `setup-ocx` should pass
`-g` to the `ocx env` call in `project.ts:139-146`, not only to `ocx pull`. Worth an
`ocx-sh/setup-ocx` issue. Until then `groups:` is dead weight, so the step omits it and
takes the default (pull every lock entry — the same eight step 4 installs anyway).

## Local gate

`task ci:actionlint`, red and green both demonstrated on this file:

```
green (final state)   exit=0
red   (mutation)      exit=201
  .github/workflows/verify-basic.yml:49:24: undefined variable "nonexistent_context_xyz" [expression]
  .github/workflows/verify-basic.yml:121:24: undefined variable "nonexistent_context_xyz" [expression]
green (after restore) exit=0
```

The red fired at lines 49 and 121 — the two steps this package rewrites — so the green is
evidence the linter reached both changed steps, not evidence it skipped the file.

## Out of scope, needs another owner

**`renovate.json:47`** was a **third** reference to the deleted directory; the plan and the
dispatch brief both say "exactly two". **Fixed** — scope was extended for this one line and
it is amended into the same commit, so `"npm": { "managerFilePatterns": [...] }` now reads
`["/^website/"]`. Not a breakage either way (an unmatched pattern matches no files), just
stale config. `grep -rn "actions/setup-ocx"` over the tree now returns nothing outside
`.claude/artifacts/`.

**`plan_issue_sweep_2026-08-30.md`** "Deferred / out of scope" still reads *"#388's second
`setup-ocx` pin at `verify-basic.yml:114` (C-074)"*, contradicting C-074, which mandates it.
C-074 is implemented; the deferred line is stale. Plan file is out of WP-8's scope.

## What this package's local evidence does NOT establish

Everything above is static verification plus local behaviour of ocx 0.5.8. **None of it
runs the pipeline.** Unproven until the PR's Workflow Lint job executes:

1. **That the upstream action works at all in this repo.** No `ocx-sh/setup-ocx` step has
   ever run here — every prior run used the deleted local action, which shares no code
   with it. Download, install, `ocx pull`, `ocx env --ci=github` and the `$GITHUB_PATH`
   handoff are all first-execution-in-CI.
2. **That `task` is on `PATH` for the unwrapped `run:` step.** This is C-071's whole risk.
   The job has no `go-task/setup-task` step; `task` must arrive from the activated project
   toolchain via `$GITHUB_PATH`. Locally the tools come from direnv, which is a different
   channel — the CI channel is untested here.
3. **That `cache: false` does not break either step**, and that the two runs of the action
   in one workflow (both with the binary cache off) behave.
4. **That `project: ''` at `:114` leaves the smoke job's `ocx --remote package env
   nushell elvish fish-shell/fish` step working.** Binary-only mode is read from source,
   not observed.
5. **That 0.5.8 is sufficient in the runner environment** — verified only as "the release
   exists and ships the right asset".

A red Workflow Lint or smoke job on the sweep PR should be read against this list first.
