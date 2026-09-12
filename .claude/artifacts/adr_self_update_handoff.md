# ADR: `ocx self update` hands off to the new binary's own setup

## Metadata

**Status:** Accepted
**Date:** 2026-09-10
**Deciders:** Michael Herwig (owner-approved plan, executed multi-agent)
**Beads Issue:** N/A
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md` (Rust 2024, lib-hosts-substance)
**Domain Tags:** infrastructure | devops | cli
**Supersedes:** N/A (amends Decision 4C of `adr_self_setup.md`; renames the config key in `adr_toolchain_activation.md`)
**Superseded By:** N/A

## Context

`ocx self setup` had to be run by hand after `ocx self update` 0.6.0 → 0.6.1.

`refresh_shell_integration_after_swap` was a hardcoded 2-of-6 subset of
`setup::run` — `env.*` shims plus a heal-only RC block. 0.6.1 added phase 3.5
and the session-PATH stores; the subset had never heard of them, so after the
swap the machine had none.

The proximate fix (add phase 3.5 to the list) does not address the cause. The
subset **ran in the old binary**, so an update *to* version N always applied
version N−1's setup contract: any obligation introduced by N is unreachable
from the process installing it, because that process predates the code that
knows about it. Patching the list leaves the same hole open for the next phase
anyone adds.

Silence hid it. `shims.rs` and `rc_block.rs` are byte-identical across
0.6.0↔0.6.1, so both phases that *did* run returned `Unchanged` and no
advisory fired.

## Decision

**`self update` stops enumerating setup phases.** It pulls **without
selecting**, then spawns the newly pulled binary as
`ocx self setup <tag>@sha256:<hex> --handoff`. The child's phase 1 performs the
select; phases 2–5 write every setup surface using the new version's own code.

Four supporting decisions:

1. **Select last, as the commit point.** The new version's setup runs while
   `current` still names the old binary, so a failed setup leaves the machine
   byte-identical. The old order swapped first, and a failed refresh stranded a
   half-migrated machine.

2. **The verdict is the symlink, never the exit code.** The select is phase 1
   and phases 2–5 follow it, so a child exiting 82 (`DirtyRcBlock`) has
   *already* repointed `current` — that update succeeded. The parent
   canonicalizes `current` and compares it against the pulled package root.
   Keying on exit status would report a completed update as a failure. A new
   `Pulled` outcome carries `Option<HandoffFailure>` and exits 75
   (`EX_TEMPFAIL`); the `Option` refuses to fabricate a failure for a child that
   exited cleanly but left `current` unmoved.

3. **A hidden `--handoff` flag, not an inferred mode.** An update must never
   *introduce* a managed RC block into a profile that has none, but a
   first-time `ocx self setup` is indistinguishable from a hand-off from inside
   the process — no config, no block, no shims. The parent says so explicitly.
   It threads to `apply_target`'s existing `heal_only` parameter, and is passed
   **unconditionally**: on an already-set-up machine "introduce none" costs
   nothing, because the heal branch still rewrites a drifted block and still
   migrates a legacy footprint.

4. **The setup opt-outs persist.** `--no-modify-path` and
   `--profile`/`--no-profile` now write `[shell] modify_path` and
   `[shell] profiles`, **only when explicitly given**, so the run `self update`
   triggers honours what the machine was set up with. `profiles` absent means
   auto-detect fresh each run; `profiles = []` means write no blocks — a
   detection result is never snapshotted, or it goes stale the day the user
   installs fish. A set list replaces detection, never unions with it.

Separately, `toolchain-dir` was renamed `toolchain_dir`, removing the last
kebab-case config key.

## Consequences

### The fix does not fix its own installing update

The parent performing the first update *to* a version carrying this change is
the **old** binary running the **old** flow. The hand-off therefore takes
effect only from the update *after* the one that installs it — the same
structure as the original bug, now applying to its own fix. Unavoidable, and
the reason the release note must say so.

### Accepted breaks and limitations

- **Exit 75 is a new observable outcome.** A script doing
  `ocx self update && …` now sees 75 where it previously saw a hard error or a
  completed swap. It is the correct signal — pulled, not activated, retry is
  meaningful and idempotent.
- **A published managed-config payload carrying `toolchain-dir` loses the key
  silently.** The managed tier parses through the same `Config` struct and
  nothing uses `deny_unknown_fields`, so the fleet's toolchain root reverts to
  `$OCX_HOME/toolchain`. Owner-accepted.
- **Flags do not forward to the child.** It inherits the environment (so
  `OCX_*` passes through, including `OCX_INDEX`), but `PackageManager` holds no
  `OcxConfigView`, so `--index` and friends do not reach it. Such a run fails
  loudly as `pulled`/75, never silently. Threading a config view into the
  library is deferred.
- **No deadline on the child**, deliberately: it is the foreground continuation
  of the user's own command, and a timeout could only be honoured by killing a
  setup mid-write.
- **A downgrade degrades loudly.** A new parent resolving an older child gets
  `--handoff` rejected as an unknown argument (exit 64), observes `current`
  unmoved, and reports `Pulled`/75. No version probe was added; the loud
  failure is the correct behaviour.
- **`select = false` is covered only by the acceptance suite.** Nothing at the
  unit level reaches `install_all`, and a regression to `select = true` would
  still report `Installed` — it only re-arms the half-migration hazard.

### Rejected alternatives

- **A `SetupMode`/heal flag, a version stamp, or contract generation.**
  `self update` is the only path where an old binary applies a new version's
  contract — the install script and `setup-ocx` already bootstrap and then call
  `setup::run` with the new binary. A stamp would have solved a class of one.
  `SHIM_CONTRACT_VERSION`, the reserved stub for that design, was deleted.
- **Routing the hand-off through `setup::refresh_profiles`.** That wrapper
  hardcodes `dry_run = false` and empty overrides, so `--handoff --dry-run`
  would write bytes and `--handoff --profile X` would heal every detected
  profile. The parameter is used directly; the wrapper is deleted.
- **`Launch::exempt`** returns `ExemptionRefused` under a fail-closed
  `[records]` posture, which would make `ocx self update` fail outright for a
  child that has nothing to record. **`Launch::recording`** would need a fifth
  `record::Scope` variant and its persisted twin — a published wire-format
  change — for a frame with no resolved package closure to describe. The
  existing `SPAWN_ALLOWED` entry for `update_check.rs` was widened instead.
- **A `--no-lock` / `--inherit-lock` flag on `self setup`.** Empirically moot:
  no advisory lock is held across `install_all`. "Inherit" would also misname
  what happens — `File::open` sets `O_CLOEXEC` and Windows has no fork, so the
  child would run unlocked, not inherit.

## Verification

The end-to-end proof is the acceptance test that serves two ocx versions from
the loopback registry via the `__OCX_SELF_IMAGE` seam and asserts the machine
is fully set up after an update. Note that a presence assertion alone cannot
discriminate `select = false` from `select = true` — the child runs and writes
the same surfaces either way, and only the *ordering* differs. The
discriminating case is a **failing** child: under `select = false` the parent
reports `pulled`/75 with `current` still resolving to the old package root;
under a `select = true` regression it has already repointed `current` and
reports `installed`/0.
