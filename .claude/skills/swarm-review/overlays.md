# Overlay Axis Definitions — /swarm-review

Overlays are single-axis adjustments layered on top of the chosen tier.
They let `auto` mode pick mixed configurations (e.g., "high base with
adversarial breadth for a medium-size diff that touches the OCI subsystem
and wants SOTA gap check") without introducing compound-name tiers.

The classifier (`classify.md`) decides *when* to apply an overlay based
on diff metrics, paths, and PR signals. This file defines *what each
axis means* and how it affects the pipeline.

## `--base` is not an axis

`--base=<git-ref>` is a **pipeline input**, not an overlay axis. It sets
the diff baseline for the entire run — everything downstream (metrics,
tier selection, review scope, Codex scope) reads from it. Listing it as
an axis would mis-model it: axes are per-dimension pipeline adjustments
that stack; `--base` is a single pre-pipeline resolution step.

Defaults for `--base`:
- PR targets: `gh pr view <N> --json baseRefName -q .baseRefName`
- Everything else: `main`
- User `--base=<ref>` overrides both of the above

## Axis grammar (flag values)

Matches `SKILL.md`'s argument parser.

```
--breadth=minimal|full|adversarial
--reviewer=haiku|sonnet|opus
--doc-reviewer=haiku|sonnet
--rca=on|off
--codex / --no-codex
```

## Axis definitions

### breadth axis

Controls the Stage 2 perspective set. Stage 1 (spec-compliance +
test-coverage) runs at every tier regardless of breadth.

| Value | Effect |
|---|---|
| `minimal` | Stage 2 launches **only** `worker-reviewer` (focus: `quality`). No security / performance / docs / architect / researcher. Used at tier=low. |
| `full` | Stage 2 launches `worker-reviewer` (quality / security / performance) + `worker-doc-reviewer` when doc triggers match. Today's default for tier=high. |
| `adversarial` | Stage 2 adds `worker-architect` (SOLID, boundary, dependency direction) + `worker-researcher` (SOTA gap check) + `worker-reviewer` (focus: `quality`) with CLI-UX lens. Default for tier=max. |

Per-tier defaults:
- low → `minimal`
- high → `full`
- max → `adversarial`

### reviewer axis

Controls the model for every `worker-reviewer` launch — Stage 1 (spec-compliance + test-coverage) and Stage 2 (quality / security / performance / CLI-UX). All reviewer invocations in a single `/swarm-review` run share the resolved value.

Evidence from `.claude/artifacts/research_model_capability_matrix.md`: Opus 4.7 leads Sonnet 4.6 by 8.0pp on SWE-bench Verified; the gap is largest on multi-step agentic chains (the adversarial breadth profile at tier=max). The Qodo 400-PR study places Haiku 4.5 ahead of Sonnet 4.5 on single-pass code review for narrow-scope diffs.

| Value | Effect |
|---|---|
| `haiku` | `worker-reviewer` with model=haiku. Narrow-scope review at tier=low with no security/structural markers. |
| `sonnet` | `worker-reviewer` with model=sonnet. Default at tier=high and tier=max (non-adversarial). |
| `opus` | `worker-reviewer` with model=opus. Used at tier=max when `--breadth=adversarial` fires — CLI-UX, architecture-boundary, and SOTA-gap perspectives benefit from deeper reasoning. |

Per-tier defaults:
- low → `haiku` (→ `sonnet` when structural markers from `classify.md:48-61` fire: `oci/**`, `package_manager/**`, auth/crypto/signing, new `crates/*/Cargo.toml`, `deny.toml`, `ocx_schema/**`, public API breakage)
- high → `sonnet`
- max → `sonnet` (→ `opus` when `--breadth=adversarial`)

**Security floor**: Haiku never runs reviewer on structural-marker paths. The floor is Sonnet minimum whenever any marker from `classify.md:48-61` is present in the diff.

### doc-reviewer axis

Controls the model for `worker-doc-reviewer` when it launches (Stage 2 at tier=high/max when doc triggers match). Single-pass narrow-scope doc audit is the workload where Haiku 4.5 beats Sonnet 4.5 per the Qodo 400-PR study (see `.claude/artifacts/research_model_capability_matrix.md`). Fenced to narrow doc diffs to avoid Haiku's 200k context cap binding on full user-guide audits.

| Value | Effect |
|---|---|
| `haiku` | `worker-doc-reviewer` with model=haiku. Triggered when the diff touches ≤2 doc files AND does not touch `website/src/docs/user-guide.md`. |
| `sonnet` | `worker-doc-reviewer` with model=sonnet. Default at all tiers; used whenever the haiku trigger does not fire. |

Per-tier defaults (all tiers share the same default — axis only toggles when the scope trigger fires):
- low → `sonnet` (moot: doc-reviewer does not launch at tier=low)
- high → `sonnet` (→ `haiku` when narrow-scope doc trigger fires)
- max → `sonnet` (→ `haiku` when narrow-scope doc trigger fires)

### rca axis

Controls the depth of root-cause analysis applied to findings.

| Value | Effect |
|---|---|
| `off` | No Five Whys chains. Reviewers report issues with proximate cause + remediation only. Used at tier=low where churn cost > value. |
| `on` | Reviewers apply Five Whys to systemic findings. Scope varies by tier: high tier applies to Block/High findings; max tier applies to everything above Suggest. |

Per-tier defaults:
- low → `off`
- high → `on` (Block/High findings)
- max → `on` (all findings above Suggest)

### codex axis (code-diff scope)

Controls whether `codex-adversary` runs as a cross-model gate against
the diff. Same entry point as `/swarm-execute`'s Codex overlay — same
scope (`code-diff`), different caller. Review-mode invocation reports
findings without the builder fix-pass (review is read-only).

| Value | Effect |
|---|---|
| `off` | No Codex diff review. |
| `on` | Invoke `codex-adversary` with scope `code-diff --base <base>` once after the Claude panel converges. One-shot, no looping. Triage into actionable / deferred / stated-convention / trivia; all surface in the report. No builder fix pass (review is read-only). |

Per-tier defaults:
- low → `off` (Two-Way Door — cost > value)
- high → `off` by default; auto-on when `classify.md` fires `--codex` for One-Way Door signals (breaking change, security-sensitive paths, protocol change, new crate, public API change)
- max → `on` (mandatory — cross-model pass is the final gate before verdict)

Triage mirrors `codex-adversary`:

- **Actionable** — reported in the Cross-Model section of the output; the caller (human or `/swarm-execute`) acts on them. swarm-review itself never auto-fixes.
- **Deferred** — reported in Deferred Findings with reason.
- **Stated-convention** — critiques a load-bearing project convention; dropped, count mentioned.
- **Trivia** — wording, formatting; dropped, count mentioned.

Unavailable path: if `CLAUDE_PLUGIN_ROOT` is unset or the companion
returns non-zero, log `Cross-model gate skipped: <reason>` and continue.
Gate, not blocker (at max-tier surface the skip prominently in the
verdict summary so the reader knows one layer was missed).

## Flag precedence

User-supplied flags always override classifier-inferred overlays. When
`classify.md` picks `--breadth=adversarial` from a cross-subsystem diff
but the user passed `--breadth=full`, the user wins. Announcement in
SKILL.md step 6 prints the final resolved config with source attribution
per axis (`tier default` / `classifier: <signal>` / `user flag`).

## Per-tier defaults (cheat-sheet)

| Axis | low | high | max |
|---|---|---|---|
| breadth | minimal | full | adversarial |
| reviewer | haiku (→ sonnet on structural markers) | sonnet | sonnet (→ opus on adversarial breadth) |
| doc-reviewer | sonnet | sonnet (→ haiku on narrow doc scope) | sonnet (→ haiku on narrow doc scope) |
| rca | off | on (Block/High) | on (>Suggest) |
| codex | off | off (auto-on for One-Way Door signals) | on (mandatory) |
