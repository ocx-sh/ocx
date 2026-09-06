# Cross-model adversary pass — `plan_index_claim_command.md`

**Model:** Codex, `gpt-5.6-terra` (job `task-mtnoooqg-ljpg5q`, completed, 2m11s)
**Scope:** `plan-artifact` — one-shot, no loop, per the hex adversary contract
**Date:** 2026-09-05
**Ran against:** the plan after the four-reviewer panel and the spec re-validation round, so
every Block and High from those five artifacts was already fixed.
**Raw result:** `.tmp/adversary_result_plan_index_claim.txt` (transient; the finding is
reproduced in full below)

**Its verdict:** 0 Block / 1 High / 0 Warn / 0 Suggest.

---

## 4-way triage

| Finding | Class | Disposition |
|---|---|---|
| F-01 · High · `ForgeKind::client` is stubbed in WP-5 but no later package can implement it | **actionable** | Fixed, by a different edit than proposed — see below |

Counts: **1 actionable, 0 deferred, 0 stated-convention, 0 trivia.**

---

## F-01 — actionable, premise verified, fix diverges from the suggestion

**What it found.** `ForgeKind::client` is the single place a concrete forge is named. It lives
in `crates/ocx_lib/src/forge/kind.rs`, which **only WP-5** declares. The planned constructor
must consume WP-6's `ForgeCredentials` and `GitBinary` and select the GitLab write path that
WP-8 and WP-13 fill. Since no descendant package owns `kind.rs`, the plan's own merge predicate
forbids any of them from finishing it — so WP-5 would have to leave a permanently
`unimplemented!()` constructor, or a later package would have to violate its declared file set.

**Premise verified independently.** Opened `crates/ocx_lib/src/forge/kind.rs`: `client` is
declared at `:117` and its body is one `match` returning `Box::new(GitHubForge::new(token,
host)?)` at `:120` and `Box::new(GitLabForge::new(token, host)?)` at `:121`. `GitHubForge::new`
(`github.rs:86`) and `GitLabForge::new` (`gitlab.rs:109`) both exist with the shape quoted. The
finding's description of the code is accurate.

**Why the proposed fix was not taken.** Codex recommended adding `kind.rs` to WP-13, adding a
`WP-8 → WP-13` edge, and letting WP-13 replace WP-5's stub. That works, but it buys a second
cross-wave doubled hub file, a new dependency edge, and a wave-4 package reaching back into the
contract surface — for a symbol that does not need any of it.

**What the analysis missed:** `client` is a **factory over constructors, not over trait
methods**. `GitHubForge::new` and `GitLabForge::new` are field-storing functions; widening them
to take a `ForgeCredentials`, a `WriteTransport` and a `GitBinary` requires none of the
behaviour WP-6, WP-8 or WP-13 supply. So WP-5 can implement `validate_transport`, `client` and
both `new` functions **for real** in wave 2, while every *trait method* body stays
`unimplemented!()` — which is what the contract-first split actually needs.

**The finding is still valuable**, because the plan did not say that. "WP-5 lands stub bodies"
was ambiguous across exactly this boundary, and the ambiguity's failure mode is the one Codex
named: a constructor stranded in a file nobody downstream may touch.

**Applied edits.**

- `C-017` gains: "**It ships implemented, not stubbed, in the package that owns `kind.rs`** …
  All four are therefore real in WP-5 while every *trait method* body stays `unimplemented!()`.
  This is stated because **no later package owns `kind.rs`**: a stub left there could never be
  finished without breaking the file-set rule, and the ADR's premise that 'one constructor stays
  the single place a concrete forge is named' would be lost."
- WP-5's per-package note gains: "**'Stub bodies' means trait-method bodies only.** Four things
  in WP-5 ship implemented: `ForgeKind::validate_transport`, `ForgeKind::client`,
  `GitHubForge::new` and `GitLabForge::new` (C-017). No later package owns `kind.rs`, so
  anything left `unimplemented!()` there is unreachable by the rest of the plan."

No dependency edge, file set or wave assignment changed.

---

## What the adversary checked and could not break

Recorded so the next reader does not re-verify them:

- **C-041's poll schedule.** `forge::poll::backoff_delays` with the plan's `PollSchedule`
  genuinely yields `1, 2, 4, 8, 15` — the final delay is clamped to the 15-second remainder of
  the 30-second deadline. DV-7's reuse claim holds.
- **DV-6's branch-existence read.** `GitLabForge::get_ref_sha` strips `heads/` and delegates to
  the existing branch lookup, so reusing it rather than adding a client method is accurate.
- **C-021's allowlist path.** `forge/git_command.rs` matches the `crates/`-relative path
  convention `SPAWN_ALLOWED` actually uses in `launch.rs`.

---

## Assessment of the gate itself

One High from a one-shot pass over a plan that had already absorbed 8 Blocks and 18 Highs is a
proportionate yield, and the finding is in the class this gate exists for: a **cross-package
interaction** — a symbol whose owner and whose dependencies sit in different packages — that no
single package's reviewer is positioned to see. The Claude panel checked file-set disjointness
and dependency-edge soundness and found both correct; neither check asks *"can every symbol
still be finished by someone?"*, which is the question that caught this.

Its proposed remedy was heavier than necessary, which is the recurring shape: the adversary
locates the defect reliably and reasons about the fix from less context. Opening `kind.rs` is
what separated the two.
