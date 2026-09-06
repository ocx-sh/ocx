# hex resource knob sheet

## 1. Scope

**Conditional-load: read this file only when a run will issue a heavy
command.** A gate the resource profile (§ 2) classed `light` never reaches
any rule below — no slot is taken, no lock file is created, and this file is
never loaded at all. A parse-only project therefore pays nothing for the
whole resource contract.

**This is a knob sheet, not a policy.** Every row here records what a tool's
knob is *called* and where its artifacts land — never which value a project
should choose, and never which command a project should run. **hex never
defines how to verify a project**: that rule has one home, in
[`verify.md` § Verification](verify.md#verification), and is linked from
here rather than copied.

Sole definition site for §§ 2, 3 and 5–9. Preflight is not one of them (§ 4).

## 2. The measured resource profile

**Measured once, at `/hex-init`, and cached as a pointer.** The run executes
**the project's own documented verification gate one time** under a peak-RSS
measurement and records four values in `hex.md › Pointers`: **peak RSS,
wall time, a class of `light` or `heavy`, and the heavy-command ceiling
derived from them below**. `light` means parse-only — arcana's own
`grim build <skill-dir>` is the worked example.

**Portable measurement, probed rather than inferred.** The ladder is
`/usr/bin/time -v` (GNU, kbytes) → `/usr/bin/time -l` (BSD/macOS, bytes) →
`gtime -v`, and **each rung is probed with a no-op first**
(`/usr/bin/time -v true`) rather than branched on `uname` — a minimal Linux
image can carry only the BSD-flavoured binary, so a `uname` branch picks a
rung that is not there.

**Where no rung answers, never fabricate a number.** The profile is recorded
**absent**, the [derived heavy-command ceiling](config.md#key-vocabulary) —
`config.md`'s key vocabulary names the key that configures it — falls back
to **1**,
and the degrade is announced as a `Degraded:` line. An absent measurement is
never reported as a measured one.

**A project that never ran `/hex-init` has no profile pointer at all**, and
that is not the absent-probe case above: there is nothing cached to read.
The run **suggests `/hex-init`, falls back to a derived heavy-command
ceiling of 1, and proceeds** — it **never measures inline**, because
measuring means running the project's full verification gate mid-flight,
which is the one cost this whole contract exists to schedule.

**Derivation — this section is its sole source:**

```
heavy    = clamp( floor( (RAM − headroom) / peakRSS ), 1, nproc )
headroom = max(2 GB, 25 % of RAM)
RAM      = min(hostRAM, cgroupLimit)
```

**`RAM` is cgroup-effective, never host-total.** `cgroupLimit` is read from
`/sys/fs/cgroup/memory.max` (v2) or
`/sys/fs/cgroup/memory/memory.limit_in_bytes` (v1) where either is present
**and finite** — `max`, or a v1 sentinel near `2^63`, means unlimited and
the host total stands. Without this clamp a 4 GB container on a 128 GB host
derives a cap sized for the host and is OOM-killed by its own cgroup, which
is the pre-container-aware-JVM bug class reproduced inside a devcontainer.

**A CPU quota is read in the same spirit, but never from the same files and
never with `nproc`.** `nproc(1)` honours the affinity mask but **not** the
quota, so inside a quota'd container it reports the host's cores. Read
`/sys/fs/cgroup/cpu.max` (v2 — a `quota period` pair, `max` meaning
unlimited) or `/sys/fs/cgroup/cpu/cpu.cfs_quota_us` with
`cpu.cfs_period_us` (v1, a `-1` quota meaning unlimited), and where a finite
quota is present use **`ceil(quota / period)`** in place of `nproc` in the
clamp above. Where neither file is present or finite, `nproc` stands.

**The headroom and the clamp are both load-bearing.** A bare fraction of
host RAM misfires at both ends of the range — too little slack on a small
host, absurd concurrency on a large one — and the `[1, nproc]` clamp is what
bounds it.

**A `light` gate is unbounded.** It never takes a slot, the cap is not
consulted for it, and § 3 is not read. **All four values are recorded
anyway** — a `light` profile carries its derived ceiling like any other,
unconsulted rather than absent, so a re-measurement that flips the class to
`heavy` has nothing to backfill.

**The pointer is a cache, never authoritative**, and is re-measured at
upkeep when it drifts, per [`memory.md` § Staleness](memory.md#staleness)'s
verify-on-consumption rule. The cap it derives is a **ceiling measured on
this host**, never a policy about how much concurrency a project deserves,
and never a cut applied to agent spawns — an agent *reasoning* costs no
local RAM, so throttling spawns pays full throughput for partial protection.

## 3. The heavy semaphore

### What takes a slot

**Takes one:** `builder:implement`, `tester`, the **merge and checkpoint
verification gates**, and a **decomposing** coordinator's **sub-WP join
verification gate** — the in-worktree run of this project's full documented
verification its Join clause funnels every sub-WP through
([`workers/coordinator.md`](workers/coordinator.md)), a heavy command like
any other. **Never:** reviewers, explorers, researchers, doc-writers,
architects, or a coordinator *waiting* between phases — the line is running
a gate versus sitting idle, never the role itself.

**The slot is held around the documented verification command, not for the
worker's lifetime.** A builder *reasoning about code* costs nothing and
takes no slot; a builder *running the build* costs a slot for the duration
of that command. That is what keeps the cap small without starving the
fleet, and it is why the semaphore is worker-side rather than a spawn-time
gate.

### Where the slots live

`N` zero-byte slot files at
**`${XDG_CACHE_HOME:-$HOME/.cache}/hex/locks/heavy-{1..N}`**, `N` = the
resolved `heavy` cap. **Host-global: never worktree-local, and never
run-root either.** The semaphore protects a **host** resource — RAM and
cores — so its slots must be host-scoped. Any path inside a checkout scopes
them to that checkout: a lock under `.agents/worktrees/<wp>/` is invisible
to a sibling worktree, and one inside a clone is invisible to a second clone
of the same project, so both silently become independent no-op locks —
worse than having no semaphore, because it looks correct.

**The orchestrator creates the lock directory and the `heavy-{1..N}` tokens
once, at run start, before the first heavy spawn** — `N` = the resolved cap.
**A worker never mints the directory** — a missing token file the `9>`
redirect below creates is harmless, being the same host-global path every
run resolves. It is handed `$LOCKS` absolute and exits `70`
if the directory is not there (§ The idiom), because a worker that creates
its own lock dir cannot tell an unprepared environment from a prepared one
and would take a slot nobody else is counting.

**Two projects deriving different `N` do not sum.** A project with `N = 2`
only ever locks slots 1–2 and a project with `N = 4` locks 1–4, so a host's
maximum concurrent heavy commands is **`max(N)` across live runs, never
their sum**. Sessions that do not share a `$HOME` are the residual, and it
is accepted rather than papered over.

The resolve-once, pass-absolute rule for this path lives in
[`protocol.md` § Worker liveness](protocol.md#worker-liveness). What is
specific to this section is the *consequence* of breaking it: a worker
expanding `${XDG_CACHE_HOME:-$HOME/.cache}` for itself lands inside its own
run's scratch (§ 5) and gets **per-run slots** — precisely the no-op
semaphore this section exists to prevent. The lock
directory and the per-run scratch roots are siblings under
`${XDG_CACHE_HOME:-$HOME/.cache}/hex/`; a run id is a timestamp-slug and
never collides with `locks`.

### The idiom

The slot is held **around the documented command**, in the same shell
invocation, never in a wrapper that backgrounds it. (*Authorized deviation
from `adr_0013` § The heavy-slot idiom, recorded as ADR errata: acquisition
is signalled **out of band** rather than inferred from exit status `111`,
because the documented command is repository-authored and can return `111`
itself. Verbatim from the ADR in every other respect — a reader diffing
against it should find this difference and no other.*)

```sh
# $N = the resolved heavy cap; $LOCKS = the host-global lock dir, absolute,
# from the spawn prompt: ${XDG_CACHE_HOME:-$HOME/.cache}/hex/locks — never
# re-derived by the worker, whose own XDG_CACHE_HOME is redirected (§ 5);
# the orchestrator created $LOCKS and its N tokens at run start (§ 3)
# $WALL      default 1800  (4 x the measured gate wall time, floor 600)
# $QUEUE_WAIT default 60   (per bounded attempt; total queue budget = $WALL)
# bound() = the wall-clock backstop, resolved ONCE PER RUN by the portability
# ladder (§ 3) into a shell function `bound SECS CMD…` — a function, not a
# string, because the perl rung's quoting does not survive word-splitting
# fd 8 = the acquisition channel: an anonymous pipe the wrapper owns and the
# command cannot name, write to, or erase (property 1 below)
# 70 = environment not prepared. Checked first: none of these is repairable
# by the worker, and each would otherwise spin the loop to $WALL and report
# 75 for a queue that never existed.
command -v flock >/dev/null || exit 70             # rung 1 form; else § 3 ladder
command -v bound >/dev/null 2>&1 || exit 70        # no backstop resolved (§ 3)
[ -d "$LOCKS" ] && [ -w "$LOCKS" ] || exit 70      # tokens never minted here
[ -w "$XDG_CACHE_HOME" ] || exit 70                # this run's scratch (§ 5)
: "${WALL:=1800}" "${QUEUE_WAIT:=60}" "${N:=1}"    # the documented defaults
exec 3>&1                                # save the run's real stdout: inside
deadline=$(( $(date +%s) + WALL ))       # $( ) fd 1 IS the fd-8 pipe
while :; do
  for i in $(seq 1 "$N"); do                       # pass: non-blocking 1..N scan
    tok=$( ( flock -n 9 || exit 111                # 111 = flock did not acquire
             printf A >&8 || exit 111              # acquired, before the command
             bound "$WALL" "$@" >&3 8>&- 9>&-      # descendants inherit neither
           ) 8>&1 9>"$LOCKS/heavy-$i" )
    rc=$?; [ -n "$tok" ] && exit "$rc"             # acquired: its rc, any value
  done
  [ "$(date +%s)" -lt "$deadline" ] || exit 75     # 75 = queue budget exhausted
  tok=$( ( flock -w "$QUEUE_WAIT" 9 || exit 111    # FAIL CLOSED — no fall-through
           printf A >&8 || exit 111
           bound "$WALL" "$@" >&3 8>&- 9>&-
         ) 8>&1 9>"$LOCKS/heavy-1" )
  rc=$?; [ -n "$tok" ] && exit "$rc"               # timed out → rescan all 1..N
done
```

**This block is the contract, not an illustration of it.** Four properties
are load-bearing and each is visible above.

1. **Fail closed.** *Every* acquisition is exit-checked — `flock -n 9 ||
   exit 111` on the scan and **`flock -w "$QUEUE_WAIT" 9 || exit 111`** on
   the bounded wait. **There is no fall-through, and the wrapper never runs
   the command unlocked.** An unchecked `flock -w` followed by the command
   is the defect this form exists to close: a timed-out wait would run
   unlocked, so under exactly the saturation the semaphore exists for every
   waiter falls through at once and the OOM class returns in full.
   **Acquisition is signalled out of band — the discriminator can never be
   an exit status.** The subshell writes a token to **fd 8** after `flock`
   returns and before the command starts; the caller branches on that token,
   and where it arrives the wrapper exits with the
   command's own status **whatever that status is, `111` and `75`
   included**. An exit status cannot serve, because the documented command
   is repository-authored and picks its own: a gate exiting `111` would be
   read as a busy slot, re-run against every remaining slot, re-run again on
   every later pass, and finally reported as `75` — queue budget exhausted —
   for what was a failing gate, which is amplification through the very
   semaphore that exists to bound load. `flock -E` does not close this: that
   flag sets the code **`flock` itself** returns on non-acquisition, which
   `|| exit 111` already supplies, and it cannot stop the child returning
   the same value. **The channel is a descriptor and not a path, and that
   distinction is the whole of the fix.** The documented command is
   repository-authored and runs as this same user, so anything it can *name*
   it can also write, truncate or unlink — a marker file under the run's
   scratch sits squarely inside the reach of the very command it
   discriminates. Erasing it after acquisition makes the wrapper read *not
   acquired*, advance to the next slot and run the command again: the exact
   amplification the out-of-band signal exists to prevent, re-entered
   through the fix. Fd 8 is an anonymous pipe the wrapper opens **before**
   the command starts and closes **to** the command (`8>&-` beside `9>&-`),
   so the command has no name for it, no descriptor onto it, and no way to
   forge or erase a token on it. **The pipe is per invocation by
   construction** — each command substitution opens its own — so two
   concurrent invocations inside one shell cannot share a channel the way a
   `$$`-named marker file would, one reading the other's record and
   returning a fabricated status for a command it never ran. A write that
   cannot happen exits `111` with the command not yet started. `111`
   survives as
   `flock`'s own non-acquisition code inside the subshell, and on every path
   the command runs under the lock or does not run at all.
2. **Both descriptors are closed for descendants** — `8>&-` and `9>&-` on
   the command, with the subshell still holding the lock. A build tool that
   daemonizes (Gradle, testcontainers) cannot inherit fd 9 and hold the slot
   after this shell exits; with the cap clamped to 1 on a small host, one
   leaked slot wedges every later run sharing `$HOME`. **Recovery, when a
   slot is leaked anyway:** identify the holder with `fuser`/`lsof` on the
   token and stop it — **never unlink the token**
   ([Token lifetime](#token-lifetime)).
3. **No lock convoy.** Scanning with `-n` first means `N` racing processes
   take `N` distinct slots in one pass with no wake storm. The overflow
   waiter blocks only **briefly** (`$QUEUE_WAIT`, 60 s) and **on timeout
   re-enters the full 1..N scan** rather than staying pinned to `heavy-1` —
   a waiter pinned to one slot collapses N-way capacity to 1-way throughput
   under precisely the contention the semaphore exists to relieve.
4. **The terminal outcome on expiry is surfaced, never silently proceeded
   past.** When the total queue budget `$WALL` is spent the wrapper exits
   `75` and runs nothing: the worker writes a `failed` beat naming the
   exhausted wait, returns that failure in its structured result, and the
   orchestrator surfaces it. **It never retries the command unlocked.**

**Exit `70` is the environment, never the gate.** The preconditions at the
top of the block — `flock` on `PATH`, a lock directory the orchestrator
minted and left writable, a writable scratch root, and a resolved `bound`
backstop ([Portability ladder](#portability-ladder)) — are the
four things a worker cannot repair, and each exits `70` **before** the loop
is entered — with one routing rule: a `70` from the `flock` probe **selects
rung 2 of the [portability ladder](#portability-ladder) (`python3`/`fcntl`),
it does not fail the run**. Without that check every acquisition fails instantly, the
wrapper spins for the whole `$WALL` and reports `75` — queue budget
exhausted — for a queue that never existed. `70` is therefore distinct from
`75` (budget genuinely spent) and `111` (slot genuinely busy): it says the
run was never prepared. **The documented defaults are assigned, not
assumed**, for the same reason: an unset `$WALL` puts `deadline` at *now*
and hands `bound` an empty duration, which exits `125` with the acquisition
token already on fd 8 — a fabricated status for a gate that never ran.

**A `blocked` beat carrying `blocked_on` is written *before* the first
bounded wait**, never after it — the wait is exempt from the concurrency cap
only while the state is declared. This adds no beat obligation: the cadence
has one home, in [`protocol.md` § Worker
liveness](protocol.md#worker-liveness), and this line only names which of
its existing obligations a bounded wait triggers — the beat written before a
tool call the agent expects to outlast its current deadline.

### Portability ladder

Each rung is detected per run, and each degrade is announced.

| Rung | Take it when | Mechanism |
|---|---|---|
| 1 | `command -v flock` answers | `flock(1)`. Its crash-safety is a kernel guarantee: the lock attaches to the open file description and is released when the last referencing descriptor closes, **including on `SIGKILL`**, so an OOM-killed build never wedges a slot. |
| 2 | else `command -v python3` answers | `python3 -c` with `fcntl.flock` — the same kernel guarantee, at the cost of one process spawn. |
| 3 | else | `mkdir` slot directories **plus a mandatory PID-file staleness check** (`kill -0`). Crash-unsafe, announced as a degrade, and shipped with its documented manual unwedge (`rm -rf` the slot directory), because a contract that can silently wedge itself with no recovery path is worse than one with no semaphore. |

**The wall-clock backstop is mandatory and has no acceptable absent
outcome.** The ladder is `timeout --kill-after=10s` → `gtimeout
--kill-after=10s` → `perl -e 'alarm shift; exec @ARGV'`, and it is walked
**once per run**, before the first heavy command, resolving into the
`bound SECS CMD…` shell function the block above calls — a function rather
than a variable holding a command string, because the perl rung's quoting
does not survive word-splitting. Perl with `alarm` ships on every target
platform's base install. The Perl form loses the two-stage TERM-then-KILL
escalation, and that is stated rather than claimed as parity. **Failing to
fill it fails closed:** where no rung answers, `bound` is left undefined,
the block exits `70` before it touches a slot, and the run does not proceed.
An unbounded heavy command is not an outcome the ladder can produce — not
as a degrade, not as a fallback, not at all.

### Two probes, two degrades

- **Filesystem.** Probe the resolved lock directory's mount type once.
  `$HOME/.cache` is not guaranteed local — a user-set `XDG_CACHE_HOME`, an
  NFS or SMB home, a WSL2 `$HOME` under `/mnt/c` — and over NFS `flock` is
  emulated as fcntl byte-range locks, which do **not** carry the
  open-file-description release semantics rung 1 rests on. Announce
  `Degraded: heavy semaphore on a non-local filesystem — crash-release not
  guaranteed`. **No relocation rule**: relocating would reintroduce the
  per-checkout scoping this section exists to remove.
- **Write permission is not assumed.** A failed token creation is announced
  as a `Degraded:` line and the run proceeds **without** heavy concurrency —
  **never as a silent unlocked run**. **The orchestrator is what serializes,
  and it does so at spawn time: one heavy work package per wave.** Every
  rung of the ladder above needs a writable lock directory, so once token
  creation fails **no worker-side serializer remains** — there is nothing
  left to force the cap to 1 from inside a worker, and a worker that tried
  would only spin to its deadline. Workers exit `70` on the missing
  precondition (§ The idiom) instead.

### Token lifetime

The `heavy-{1..N}` files are **zero-byte and permanent — never deleted, by
teardown, by the start-of-run sweep, or by any cleanup added later** (§ 8).
Unlinking a token another agent currently holds breaks mutual exclusion
outright: that holder's lock lives on the open file description of an inode
that then has no name, the next `open()` of the same path creates a **fresh
inode**, and two agents both believe they own that slot. Empty files cost
nothing; deleting them costs mutual exclusion.

## 4. Preflight

Preflight before every spawn wave is scheduling, not a resource knob, and lives in [`protocol.md` § Worker coordination](protocol.md#worker-coordination) — this file never restates it.

## 5. Per-run scratch environment

**Three variables, and only three.** `TMPDIR`, `XDG_CACHE_HOME` and
`XDG_STATE_HOME` are redirected to a **disk-backed per-run root**,
`${XDG_CACHE_HOME:-$HOME/.cache}/hex/<run-id>/<wp>/` — a sibling of the run's
heartbeat directory, both under the one run-scoped root that teardown
already owns (§ 8).

**`hb` and `locks` are reserved: a `<wp>` component can never take either
name.** `<wp>` comes from a plan cell, so it is orchestrator-minted and
slugified like every other path component (§ 8 rule 3) — and these two names
are refused on top of that. `…/hex/<run-id>/hb/` is the run's own heartbeat
directory, so a work package slugged `hb` would be handed it as its
`XDG_CACHE_HOME`: write access to every beat the orchestrator reads back,
including the `checkpoint` beat that re-enters a spawn prompt. `locks` is
refused for the same reason § 3 keeps a run id off it. A refused slug takes
a numeric suffix (`hb-2`), never the reserved name.

**Disk-backed is the point, not an implementation detail.** `/tmp` is
commonly a tmpfs sized at 50 % of RAM — a 64 GB tmpfs on 31 GB of RAM on the
measured host — so "the disk filled with test artifacts" is an
out-of-memory event wearing a disguise.

**`XDG_CONFIG_HOME` is deliberately not in the set, and the reason is
supply-chain rather than convenience.** Redirecting it silently detaches git
and the package managers from the developer's own configuration:
`credential.helper`, `commit.gpgsign`, and — the two that matter most —
`url.*.insteadOf` rewrites and registry/index pinning. Losing `insteadOf`
and index pinning means a run resolving dependencies from somewhere the
developer deliberately redirected *away* from; that is a **supply-chain
downgrade**, not merely a broken push, and it is not worth the handful of
config-directory writes the redirect would have contained.

**`HOME` is not in the set either.** Redirecting it breaks every tool that
reads real credentials from it — git identity, `gh` auth, cargo registry
tokens, ssh — and a run that fails to push because hex moved `HOME` is a
worse outcome than a suite that writes a few files under the real one.
**The trade-off runs in both directions and both are stated.** Not
redirecting `HOME` is availability-positive and **security-negative**: hex
runs a possibly hostile repository's own documented verification command,
N-way concurrent and unattended, with read access to `~/.ssh`,
`~/.config/gh` and `~/.aws`. The default is judged right — breaking the
common case for every project in order to contain the uncommon one is the
worse error — but the exposure is **traded, not absent**. It is offered as a
per-project opt-in through a `/hex-init` audit item, and that item names
**two** reasons to take it: a suite known to write to `$HOME`, and
credential exposure to a verification command the project does not fully
trust.

**The scratch root is deleted only by the top orchestrator that minted
`<run-id>`, in its teardown** — never by a worker and never by a decomposing
coordinator, **by `trap` or otherwise** (§ 8).

## 6. Containment ladder

Four rungs, detected per run, each degrade announced. Take the highest rung
whose probe answers; every rung below rung 1 is a reduction in blast radius,
not an enforced bound.

| Rung | Mechanism | Detection probe |
|---|---|---|
| 1 | `systemd-run --user --scope -p MemoryMax=<G> -p MemoryHigh=<G> -p CPUQuota=<N>00%` — cgroup v2, RSS-and-cache aware | `command -v systemd-run` **and** `[ -d /run/systemd/system ]` **and** a user-bus probe (`systemctl --user show-environment`); the directory test is the man-page-blessed shell equivalent of `sd_booted(3)` |
| 2 | Raw cgroup v2 writes on an **already-delegated** user subtree: write `memory.max`, write `cgroup.procs`, exec | the delegated subtree exists and is writable |
| 3 | `nice -n 19 ionice -c3` — blast-radius reduction, **no cap** | always reachable |
| 4 | Nothing but the wall-clock backstop (§ 3) | — |

**Never `ulimit -v` / `RLIMIT_AS` for compiled languages** — it breaks
rustc's parallel front end and JVM/Go address reservation and surfaces as a
spurious "Resource temporarily unavailable"; cgroup `memory.max` is the
correct bound. **Never `cgcreate`** — libcgroup is v1-only and deprecated
under the unified hierarchy.

Containment is never a prerequisite: rung 1 needs a live user session bus,
which is absent by default on WSL2 and in almost every container and absent
entirely on macOS, so requiring it would make hex unrunnable on two of its
three target platforms.

## 7. Per-ecosystem knob sheet

What the orchestrator sets per heavy command. `N_heavy` = concurrent heavy
slots. **Every cell names a knob, never a value a project should choose.**

| Ecosystem | Parallelism default | Cap knob | Per-worktree artifact | Share / redirect | Retention |
|---|---|---|---|---|---|
| cargo | `-j nproc` | `CARGO_BUILD_JOBS`, `[build] jobs` | `target/`, 2–20 GB | do **not** share the target dir; `sccache` + `SCCACHE_BASEDIRS` | `cargo sweep --time N`; delete with the worktree |
| nextest / `cargo test` | ncpu | `--test-threads`, `--jobs`, `RUST_TEST_THREADS`, `threads-required` | tempfiles under `TMPDIR` | per-run `TMPDIR` | — |
| pytest | xdist `-n auto` | `-n K` | `/tmp/pytest-of-*`, `.hypothesis`, `.coverage.*`, reports | `--basetemp`, `TMPDIR`, `HYPOTHESIS_DATABASE_FILE` | `tmp_path_retention_policy=failed`, `count=1`; `coverage combine && erase` |
| Jest | `cores-1` | `--maxWorkers`, `--workerIdleMemoryLimit` | `cacheDirectory` (OS tmp) | per-worktree `cacheDirectory` | `--clearCache` |
| Node deps | — | — | `node_modules`, ~2 GB | pnpm store + `node-linker=hardlink` | `pnpm store prune` |
| tsc | one V8 heap per process | `NODE_OPTIONS=--max-old-space-size` | — | — | — |
| Gradle | workers = ncpu; one daemon per JVM-args combo | `--max-workers`, `maxParallelForks`, `--no-daemon` | `.gradle/` | identical `org.gradle.jvmargs`; `~/.gradle/caches` shared | `./gradlew --stop` in teardown |
| Go | `-p NumCPU` | `GOFLAGS=-p=K` | — | `GOCACHE` / `GOMODCACHE` shared by default | `go clean -cache` |
| Docker / testcontainers | — | — | containers, anonymous volumes, build cache | label every container the run starts | label-filtered container prune in teardown (§ 8) |
| Playwright | — | — | 1.2 GB of browsers *if* `PLAYWRIGHT_BROWSERS_PATH=0` | leave the default `~/.cache/ms-playwright` | — |

**`--no-daemon` and `gradlew --stop` are part of the semaphore's surface,
not decoration**: a daemonizing build tool is exactly the descendant § 3's
`9>&-` keeps off the lock descriptor.

## 8. Teardown

**Only the top orchestrator that minted `<run-id>` runs this checklist.**
A worker never does, and **a decomposing coordinator never does either** —
it owns sub-WPs, not the run, so it tears down nothing. **The prohibition is
on the actor, not the mechanism — by `trap` or otherwise.** That a `trap` is
also useless here — `SIGKILL` skips it — is the documented reason orphan
cleanup cannot live in the worker, but it is the second reason, not the
first.

**The checklist.** Teardown owns the **worktree**, **redirected build
directories**, **caches**, the **scratch root** (§ 5), the run's
**heartbeat directory**, **daemons** (`gradlew --stop`), **containers**, and
**the process groups it recorded** — and nothing else.

**Three safety rules, in full, because each is a line an implementer will
transcribe literally.**

1. **Containers, scoped by the run's own label.** Only
   `docker container prune -f --filter label=<the run's own label>`, and
   only where **the run itself set that label** on the containers it
   started. Where it did not, teardown **reports the leftovers and deletes
   nothing**. **Never `docker system prune -f --volumes`** — a machine-wide
   prune is not a scoped command: it destroys every unused volume, network,
   dangling image and stopped container on the machine, including a
   developer's unrelated work.
2. **Process groups: only groups this run created.** "Kills the process
   group" is undefined and dangerous — an agent's shell typically shares a
   process group with the harness, so `kill 0` or `kill -- -$$` kills the
   harness or the user's own shell. Instead: every heavy command is started
   under its own process group (`setsid`), its group id is recorded at
   spawn, and **teardown signals only recorded group ids. It never signals a
   process group it did not create.**
   **A recorded group id is a name the kernel reuses, so identity is
   confirmed before the signal, never inferred from the number.** Between
   the recording and the teardown the operating system can recycle that id
   onto an unrelated group, and a signal sent on the number alone reaches
   whatever now holds it. The spawn therefore records the leader's
   **creation-time identity** alongside the id — its start time and the
   command it was started as — and teardown re-reads both and signals only
   on a match. **A group whose identity does not match is reported and left
   alone, never signalled**, which is the same sweep-and-report rule this
   section applies to every other ambiguity.
3. **Every delete target is minted and prefix-checked.** The minting and
   slugify rule for `<run-id>`, `<agent-id>` and every slug is stated once,
   in [`protocol.md` § Worker liveness](protocol.md#worker-liveness), and is
   not restated here. What this section owns is the *consequence* of
   breaking it: an unslugified id reaches an `rm -rf` target and a composed
   shell. Teardown therefore **refuses any delete target that does
   not resolve — after symlink resolution — under the prefix of the scope
   being torn down.** For the whole-run teardown the orchestrator performs,
   that prefix is its own run root
   `${XDG_CACHE_HOME:-$HOME/.cache}/hex/<run-id>/`, or the worktree root the
   run created. **For a delete scoped to one work package it is the narrower
   `${XDG_CACHE_HOME:-$HOME/.cache}/hex/<run-id>/<wp>/`** — or that work
   package's own worktree — because the run root there admits a **sibling
   work package's live scratch and the run's whole `hb/` directory** (§ 5),
   which is the exact deletion this backstop exists to refuse. A target
   resolving outside the prefix that applies is reported and
   skipped, never deleted. This backstop holds even where the minting rule
   is bypassed by a later caller.
   **Resolution and deletion are one operation, or the target is refused.**
   Resolving the path, then deleting it, leaves a window in which any
   component can be swapped for a symlink pointing outside the
   run-root prefix the check just approved — the check passes and the
   delete lands elsewhere. Teardown therefore descends **no-follow and
   descriptor-relative**: each component is opened directory-only and
   without following symlinks, and every unlink is issued relative to the
   descriptor already held, so the kernel never re-resolves a name the check
   validated (`openat`/`unlinkat` semantics; an `rm -rf` handed a
   re-resolved path is not equivalent, and neither is a second
   `realpath`). **Where that walk is not available, teardown refuses the
   target and reports it.** It never falls back to resolve-then-delete, and
   it never accepts a target whose path components are not owned by this run
   for the whole of its lifetime.

**Sweep and report, never delete, on ambiguity.** A run deletes **only its
own `<run-id>` subtree**. Another run's directory — a killed run's leftovers
included — is **swept and reported by the next run, never deleted**.
`<run-id>` is in the path (§ 5), so "unambiguously this run's own" is a
string comparison rather than an inference, which is what makes the rule
decidable instead of a judgment call.

**One permanent exception, and it is a correctness rule rather than an
oversight:** the heavy-slot tokens at
`${XDG_CACHE_HOME:-$HOME/.cache}/hex/locks/heavy-{1..N}` are **never deleted
— not by teardown, not by the start-of-run sweep, not by any cleanup this or
a later contract adds** (§ 3).

## 9. Output as a resource signal

A wait on a lock is not one situation but three, and grading them alike
would collapse the cap to 1 on the first busy wave of **every** run, because
hex's own semaphore produces lock waiting by design. **The three-way triage
is this section's spine.**

| Class | What it means | What the orchestrator does |
|---|---|---|
| **1 — a wait on hex's own heavy slot** | The mechanism working. The agent is already in state `blocked` with `blocked_on` naming the slot (§ 3). | **Nothing. It is not a signal at all** — no log entry as one, and the cap is unchanged. |
| **2 — a wait on the build tool's own lock** | A *configuration* fault, not a memory fault: two agents are sharing one build directory, which § 7's per-worktree artifact column exists to prevent. Cargo's `Blocking waiting for file lock`, a Gradle daemon lock, and their siblings. | **Warn, naming the shared directory. Never lower the cap** — lowering concurrency does not un-share a target directory; it only makes the run slower while the misconfiguration stands, and hides the actual fault behind a throughput loss. |
| **3 — resource-exhaustion evidence** | An out-of-memory kill, a V8 `heap out of memory`, a `dmesg` OOM line, inotify `ENOSPC`, or a heavy command exceeding its measured profile (§ 2) × 1.5. | **The only class that may move the cap**, and only after the corroboration below: lower it by one for the remainder of the run and log it. The floor is 1. |

**Corroboration from a host source is required before the cap moves.** The
classified token alone is never enough: the orchestrator additionally reads
`/proc/pressure/memory`, `dmesg`, or the measured-profile overshoot —
sources the **host** owns, not the repository — and lowers the cap only when
one of them agrees. Without this, text a repository plants in its own build
output could walk a run's concurrency down to 1: a denial of service against
every later work package for the cost of one string. **Planted repository
text alone can never move the cap.** Where no host source is readable — a
platform with no `/proc/pressure`, no `dmesg` access — the evidence is
**logged and surfaced and the cap does not move**; a missing corroborator is
never a passed one.

**The worker classifies; the orchestrator never re-parses raw build
output.** Build output is repository-controlled text, and routing it into
the orchestrator's context to key a control decision on is exactly the shape
[`protocol.md` § Untrusted-text echoes](protocol.md#untrusted-text-echoes)
forbids — that is the bundle's single copy of the rule, linked here and
never restated. So the **worker** decides which of the three classes it is
in and returns **one bounded, classified token**: `oom-evidence` for class
3, `lock-evidence` naming the shared directory for class 2, and **nothing at
all** for class 1. Never raw output for the orchestrator to re-parse, and
never an unbounded echo. **The bound governs the token's own text, not
merely the output it replaces**: `lock-evidence` carries a
repository-controlled directory path into an orchestrator warning, so that
path is slugified and truncated to the same character bound before it is
emitted.

**None of the three is ever retried as a flake** — a retry under the same
conditions reproduces the same exhaustion and burns a second full run to
learn nothing. **The lowered value is run-scoped and never written back to
config**: the configured cap is the user's, and a single bad run does not
get to edit it.
