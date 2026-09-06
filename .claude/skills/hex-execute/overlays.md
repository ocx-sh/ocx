# Overlay Axis Definitions

Overlays are single-axis adjustments layered on the tier
[`classify.md`](classify.md) chose. They let `auto` mode assemble a mixed
config (e.g. a `high` base that still runs `adversarial` review breadth for
a security-sensitive diff) without compound tier names. [`classify.md`](classify.md)
decides *when* an overlay fires from signals; this file defines *what each
axis means* and how it changes the pipeline. Grammar only lives here — the
shared overlay rules are in
[`protocol.md`](../hex-core/references/protocol.md#overlay-grammar).

## Axis grammar (flag values)

Matches the [`SKILL.md`](SKILL.md) parser:

```
--review=minimal|full|adversarial
--loop-rounds=1|2|3
--adversary / --no-adversary
```

## review axis

Selects the **checklist breadth of the `L2` aggregate seat** in the
[Review-Fix Loop](../hex-core/references/loop.md#review-by-join-level) — one
deep-reasoning `reviewer`, one brief; the axis grows what that brief asks
for, never the seat count. `L1` always runs the `minimal` sections and this
axis does not touch it — `review.l1.checklist` is `L1`'s knob
([`config.md`](../hex-core/references/config.md#key-vocabulary)).

| Value | `L2` checklist — sections of [`checklist.md`](../hex-core/references/checklist.md#composition) |
|---|---|
| `minimal` | `spec` + `quality`. Nothing else. |
| `full` | the `minimal` set, plus `security` when the diff touches security-sensitive paths (auth/crypto/signing, a new dependency manifest, a CI workflow file), `performance` when the diff touches a hot path or async code, and `docs` when doc-drift triggers match. |
| `adversarial` | the `full` set, plus `architecture` (the seat reads the named ADR in full) and `pitfalls`. |

Per-tier defaults:

| Tier | review default |
|---|---|
| low | `minimal` — the inline orchestrator's own `spec` + `quality` pass; no `L2` at one WP |
| medium | `minimal` |
| high | `full` |
| xhigh | `adversarial` (mandatory) |
| max | `adversarial` (mandatory) |

The axis reads the plan tier `T`, never a WP's effective tier — the `L2` seat
reviews an aggregate, not a WP. A `sec`, `hot` or `door` flag on any joined
WP forces the security and performance items on at every value
([`loop.md`](../hex-core/references/loop.md#review-by-join-level)).

## loop-rounds axis

Caps the [Review-Fix Loop](../hex-core/references/loop.md#review-by-join-level)
round count at every join level.

| Value | Effect |
|---|---|
| `1` | Single round at every level: one seat, one builder fix pass, one re-verification. The shipped `L1` and `L2` value. |
| `2` | Up to two rounds where a level's `review.<level>.rounds` allows it. |
| `3` | The hard maximum. |

Per-tier default: `1` at every tier — rounds are a per-level setting
(`review.<level>.rounds`, [`config.md`](../hex-core/references/config.md#key-vocabulary)),
not a tier one. This axis and a stored `loop rounds` limit in `hex.md ›
Preferences` are both **ceilings** over a level's `rounds`: the effective
cap is the lowest of the three
([`loop.md`](../hex-core/references/loop.md#the-review-fix-loop)).

**The plan's `Review` cell touches neither axis.** It is a risk hint that
raises a WP's review one join level
([`decompose.md`](../hex-core/references/decompose.md#parallel-by-default-decomposition));
it never lowers breadth, never forces a round count, and a legacy
`self`/`light` cell is inert.

## adversary axis (code-diff scope)

Controls whether the configured cross-model adversary skill runs against the
branch diff, launched **in the same batch as the terminal join's native
seat** — the `L2` seat, or the sole leaf's `L1` at `N = 1` — never after it
([`adversary.md`](../hex-core/references/adversary.md#adversary-contract),
`adr_0016` C-987). The skill name is read from
the Preferences section of `.agents/memory/hex.md` (`codex-adversary` is only
an example value); the full contract — scopes, one-shot rule, 4-way triage,
graceful skip, stall bound and backstop — is in
[`adversary.md`](../hex-core/references/adversary.md#adversary-contract). This is
the `code-diff` scope; `/hex-plan` runs the same skill in `plan-artifact`
scope.

**This axis reads the plan tier `T`, never a WP's effective tier.** A WP that
derived `medium` inside a `xhigh` plan **still runs the cross-model gate**: the pass
is a run-level assurance decision, and a per-WP size estimate must not become a
global skip switch
([the effective tier](../hex-core/references/decompose.md#the-effective-tier)).

| Value | Effect |
|---|---|
| `off` | No cross-model diff review. |
| `on` | Invoke the adversary skill once in `code-diff` scope against the branch diff versus base, launched last in the terminal join's batch. One-shot, no loop. Triage its findings 4-way (actionable / deferred / stated-convention / trivia); actionable findings join that join's single `builder` (focus `implement`) fix pass, one re-verify; on failure the pass is reverted and re-run native-only with the adversary findings deferred (C-988). |

Per-tier defaults:

| Tier | adversary default |
|---|---|
| low | `off` (inline tier); an explicit `--adversary` runs it, batched with the optional `L1` backstop or alone |
| medium | `off` (two-way door — cost outweighs value) |
| high | `off`, auto-on when [`classify.md`](classify.md) fires `adversary=on` for one-way-door signals; explicit via `--adversary` |
| xhigh | `on` (a default part of the flow; a skip is surfaced prominently) |
| max | `on` — **every** entry of a list-valued `adversary` key launches in the same batch, findings union-triaged with duplicate merge; below `max` the first entry only (`adr_0017` C-995) |

When the adversary produces no review — the named skill is unavailable, or it
ran and did not complete one — log
`Cross-model review skipped: <reason>` and continue — a gate, not a blocker
([`adversary.md`](../hex-core/references/adversary.md#adversary-contract)).

## Precedence

User-supplied flags always override classifier-inferred overlays, which in
turn fold in `hex.md › Preferences` hints on top of the tier baseline —
later wins
([spawn-selection precedence](../hex-core/references/protocol.md#spawn-selection-precedence)).
When [`classify.md`](classify.md) infers `review=adversarial` but the user
passes `--review=full`, the user wins — including at `xhigh` tier, where
`review=adversarial` and `adversary=on` are the defaults. A downward
override there is honored but never silent: the announce block flags it
("xhigh tier recommends adversarial review — running `full` per user flag")
so the risk the tier signalled stays visible at the gate.
`tiers.hex-execute.<tier>.overlays` sets a per-project default for an
overlay axis — it rewrites this axis's tier baseline (a layer-1 rewrite, not a project hint — see [`config.md` § tiers](../hex-core/references/config.md#tiers)), below any user flag.
[`SKILL.md`](SKILL.md) step 6 prints the final resolved config with each
axis's source.
