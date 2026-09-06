# hex Adversary Contract

A topic file of the hex swarm protocol; the spine is
[`protocol.md`](protocol.md).

## Adversary contract

A pluggable cross-model review, named in the `hex.md › Preferences` section.
The contract is symmetric across harnesses — a user on one model family
points at an adversary skill from another.

- **Scopes:** `code-diff` (the branch diff versus a base) and
  `plan-artifact` (a markdown plan or ADR file).
- **One-shot — never loops.** Two-family stylistic thrash is the failure
  mode it exists to avoid.
- **`adversary` may name a list** (`config.md` v4, `adr_0017` C-995). At
  tier `max` **every** listed skill launches in the same batch under the
  concurrent-launch bullet below, and their findings are union-triaged
  with the same duplicate merge; below `max` only the first entry runs.
  One configured entry at `max` runs it and announces
  `max: 1 adversary configured`. Each listed skill resolves its own
  observation mode and bound; the join waits for all of them.
- **Concurrent launch — a member of the join's batch, never a serial tail**
  (`adr_0016` C-987). When the axis is on, the adversary is launched **in
  the same batch as the native seat of the join it gates**: the `L2`
  aggregate seat, or the sole leaf's `L1` at `N = 1`, in `/hex-execute`
  ([`loop.md`](loop.md#review-by-join-level)); the Stage 2 batch in
  `/hex-review`; the Round 1 panel batch in `/hex-plan` and
  `/hex-architect`. **Batch order is native seats first, adversary last**,
  so a mode-(c) blocking call blocks the orchestrator only after every
  native seat is already running. Where the harness cannot issue the call
  inside the batch at all, the launch degrades to sequential and the
  existing `Degraded: blocking adversary call …` line takes the suffix
  `; launched sequentially` — no new degrade axis. **The adversary occupies
  no `max-workers` slot**: it is not a hex worker (the heartbeat bullet
  below). **The two clocks are orthogonal**: `review.<level>.budget-minutes`
  bounds the native seat and nothing else; the bound this contract resolves
  bounds the adversary and nothing else — neither truncates the other,
  because hex can stop its own worker and cannot terminate an external
  skill. The join waits for both; wall clock is `max(native, adversary)`,
  which is the whole gain.
- **The bound comes from the adversary's own published contract where it
  states one, and otherwise from how hex invoked the call.** Neither is a
  claim about the skill in the abstract: one is what the skill publishes,
  the other is what hex can observe of its own call. Three terms, used
  exactly: a **stall bound** fires on silence over a signal hex can read;
  its **stall window** is how long that silence must last before it fires;
  a **backstop** is a fixed wall clock hex runs where it is measuring no
  silence at all — under (c) because there is no readable signal, under (a)
  once the skill's own bound has elapsed without a verdict.

  **Resolving the mode, in order.** **(a)** where the adversary skill's own
  published contract documents a timeout or silence outcome the skill
  enforces itself; else **(b)** where hex invoked it as a background task
  whose output hex can read without blocking; else **(c)**, the fail-safe.
  (a) is read off the skill's published contract and nothing else; (b) and
  (c) are read off the call hex made. (a) **takes precedence over the
  invocation mode rather than composing with it** — a skill that bounds
  itself is not bounded twice, and hex adds only the backstop below against
  a process that never returns.

  | Observation mode | What hex observes | hex's own bound |
  |---|---|---|
  | **(a) skill-enforced** | nothing directly — the skill enforces and reports its own silence policy | the same fixed **15-minute** backstop (c) uses, its clock started by the elapse of the skill's own published bound — a skill *process* that never returns; settable-timeout condition below |
  | **(b) pollable background** | `byte_activity` — output bytes growing, nothing structured — read without blocking | a **stall window** from the last output growth — the one bound in this table a project sets, so its figure is [`limits.adversary-timeout`](config.md#key-vocabulary)'s and is not repeated here |
  | **(c) foreground blocking** | `process_only` — that the process exists, nothing else | a fixed **15-minute** backstop; settable-timeout condition below |

  **(a) skill-enforced.** The skill runs under its own liveness policy and
  reports the outcome — `nox-review` is the shipped example: its published
  contract (§ How long a review takes) states the wall clock it allows a
  review and the silence after which it kills one, and reports that as
  `error` with `reason: timed_out`. Read those figures there, never here:
  a project may raise them. hex trusts that verdict, and hex's own bound is
  **only** the same fixed **15-minute** wall clock (c) uses, whose clock
  **starts when the skill's own published bound elapses** — its published
  wall clock where it states one, else its silence bound — and never at
  invocation. **The two bounds never double-count**, and that is why: hex
  cannot start counting until the skill's own has run out, so the backstop
  only ever catches a process that never returned after its own bound
  should have fired — a hung process, which is all it claims to bound. A
  project that raises the skill's bound moves hex's start with it, so
  raising it can never truncate a review. The backstop announces nowhere:
  it degrades no capability and governs no call the skill still bounds.

  **(b) pollable background.** The skill runs as a background task whose
  output the orchestrator can read **without blocking** — a background agent
  or shell task, its output file's mtime, a non-blocking read of the task's
  output. hex observes `byte_activity`, and the stall window is measured
  from the **last output growth, never from invocation**: total elapsed time
  is unbounded while output keeps growing, because an adversarial review may
  legitimately run long and a total cap cannot tell a thorough run from a
  hung one. **Observing it is an action, not a wait:** the window elapses
  when the time since the last observed growth exceeds it, so re-read the
  output on a cadence no coarser than the window — a coarser one measures a
  silence hex never looked for. Waiting on a completion notification
  measures nothing and resolves no stall.
  [`limits.adversary-timeout`](config.md#key-vocabulary) is that window; its
  default and its ceiling are config.md's, restated nowhere here beyond the
  worked render in the [clamp grammar](protocol.md#the-meta-plan-approval-gate), under
  which a clamp announces like every other limit's.

  **(c) foreground blocking.** The skill runs as a blocking call: it returns
  once, and between the call and the return there is nothing to poll, so
  silence is unobservable and no stall window exists. hex sees
  `process_only` — that the process exists, nothing else — and a fixed
  **15-minute** backstop stands in. It is not a stall bound and it is not
  `limits.adversary-timeout`, which has nothing to govern where there is
  nothing to observe. It announces at the gate on its own `Degraded:` line —
  `Degraded: blocking adversary call — no pollable output; process_only
  backstop 15 min` — which stacks with the other degraded axes like any
  other. Those 15 minutes are not a guarantee: they bind only under the
  condition the *terminates nothing* bullet below states. Where the spawn
  carries no settable timeout the same line renders
  `… process_only backstop 15 min (nominal)`. **Prefer (b) wherever the
  harness offers it**: a pollable background call converts a wall clock
  hex cannot justify into a stall window it can. The capability is
  **detected per run and announced, never stored**, like every other
  degrade axis ([§ Worker coordination](protocol.md#worker-coordination)).

  **(b) and (c) are properties of the call, never claims about the skill.**
  `codex:rescue` spawned as a foreground subagent is mode (c); the same
  skill launched as a background task is mode (b).

  On elapse of whichever bound the mode resolved to, the orchestrator
  **stops waiting**, takes the graceful skip below and proceeds.
- **hex writes no heartbeat for an external skill, and terminates nothing.**
  An adversary is a pluggable external skill, not a hex worker: it emits no
  hex heartbeat, and hex builds no progress surface for it — mode (b)'s
  signal is output the harness already exposes, nothing more.
  [§ Worker liveness](protocol.md#worker-liveness) weighed reading a child's output
  stream and demoted it; that section's heartbeat contract governs hex's
  **own** workers and is not restated here, this being the external-skill
  case it cannot reach. When a bound elapses
  hex discards whatever the call may still return and **does not attempt to
  terminate the underlying process** — no shipped file gives an orchestrator
  a capability to terminate an external skill's process. It follows that
  **the (a)/(c) backstop fires only where the invocation itself carries a
  settable timeout** — a spawn hex handed a
  deadline it can enforce. Where the spawn carries none, hex regains
  control only when the call returns, and the backstop is **nominal**: a
  bound hex documents but cannot enforce. `codex:rescue` as a foreground
  subagent — the worked example above — is exactly that case. The accepted
  cost is stated: an adversary that hangs under such a call blocks the run
  until it returns.
- **4-way triage** of its findings:
  - **actionable** — folded into the join's **single builder fix pass**
    together with the native seat's actionable findings, one pass, one
    re-verify by the join's own resolved verification (`adr_0016` C-988).
    A finding naming the same defect as a native finding merges into it,
    attribution noted, never counted twice. If that merged pass fails
    verification: **revert it, re-run it native-only, and promote every
    adversary finding to deferred** — the adversary is never re-invoked.
    In a read-only mode (`/hex-review`) the finding is reported, never
    fixed;
  - **deferred** — handed off with context;
  - **stated-convention** — dropped, counted;
  - **trivia** — dropped, counted.
- **Graceful skip whenever the adversary produced no review.** Two cases,
  one rule: the named skill is **unavailable**, or it **ran and did not
  complete a review** — it could not reach its harness, was refused
  credentials or quota, ran out of time, or returned something it could not
  itself classify. Each adversary skill states its own outcome vocabulary;
  read it there rather than guessing at one here. In **both** cases log
  "Cross-model review skipped: `<reason>`", carrying the reason the skill
  itself gave, and continue — it is a gate, not a blocker. The skip is
  surfaced **prominently at tier `xhigh` and `max`**, where the adversary pass is a
  default part of the flow.
- **Reason attribution — orthogonal under (a) and (b), a disclosed race
  under (c).** The skip-line grammar above is unchanged, and in every mode an
  outcome the skill reported is logged with the skill's own word. Under
  **(a)** hex defers to the skill's verdict outright: `timed_out` is the
  logged reason, and hex writes no `deadline` for a call the skill's own
  bound reached — hex's clock does not start until that bound has elapsed
  unheard, so a `deadline` under (a) reports only a skill process that
  never returned at all. Under **(b)** the two are genuinely orthogonal —
  a skill's own
  `timed_out` bounds *its* run from the inside, hex's `deadline` is silence
  *hex* actually observed over the output — so neither can launder the
  other, and `deadline` is written **only** where the stall window elapsed
  with no output growth. **(c) is the disclosed race:** with no observable
  output the backstop measures the same quantity a skill's own timeout
  would, so the two genuinely race, and a `deadline` logged under (c)
  asserts only that **hex stopped waiting** — never that the adversary
  failed, timed out, or found nothing. Neither reason is ever a clean pass.
- **An empty finding list is never a clean pass.** An adversary that did not
  review has nothing to report, so zero findings is the *expected* shape of
  a failure, not evidence of agreement. A "triage is complete" gate is
  therefore satisfied by exactly two things — the triage of a review that
  completed, or the skip logged above — and never by an untriaged emptiness.

