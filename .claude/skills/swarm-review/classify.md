# Classification Signals — /swarm-review

Signal-to-tier mapping used when `/swarm-review` runs with tier=`auto`.
Also defines overlay triggers that stack on top of the chosen tier.

**Primary signals: diff metrics.** Unlike `/swarm-plan` (which classifies
from a prompt) and `/swarm-execute` (which reads the plan header), review
classifies from the **actual diff against the configured baseline**.
`--base=<ref>` is the single biggest lever on auto tier selection — a
tight baseline (recent commit, sibling branch) produces a small diff and
lands on `low`; a wide baseline (`main` on a long-lived branch, an older
tag) produces a large diff and lands on `high` or `max`.

When signals split across adjacent tiers, or the overlay mix is unusual,
mark the classification **low-confidence** — this forces the meta-plan
gate in SKILL.md step 5. Do **not** fire a mid-flow `AskUserQuestion`.

## Primary: diff metrics

Compute once at the start of classification:

```
git diff <base>...HEAD --name-only    # → changed-file list
git diff <base>...HEAD --shortstat    # → lines added/removed
```

Then derive:

- **file_count** — `wc -l` of the name-only output
- **lines_changed** — `added + removed` from `--shortstat`
- **subsystems_touched** — match each path against the subsystem path
  scopes in `.claude/rules.md` "By subsystem" table
- **structural_markers** — see table below
- **pr_labels** — present only when the target resolves to a PR

## Tier metric table

| Tier | file_count | lines_changed | subsystems | structural markers |
|------|-----------|---------------|------------|-------------------|
| **low** | ≤3 | ≤100 | 1 | None from the adversarial list |
| **high** | ≤15 | ≤500 | 1–2 | No One-Way Door High signals |
| **max** | >15 or any One-Way Door High signal | any | ≥2 or cross-subsystem | Any One-Way Door High signal |

A diff may match multiple rows — pick the **highest** tier for which at
least one clear signal fires. A small file count does not demote a diff
that introduces a new crate.

## Structural marker signals

| Marker | Tier impact |
|---|---|
| New `crates/*/Cargo.toml` | → **max** (new crate) |
| New top-level directory under `crates/` or `mirrors/` | → **max** |
| `.github/workflows/**` changes | Adds `--breadth=full` minimum; security review required |
| `crates/ocx_lib/src/oci/**` | Adds `--breadth=full`; `--codex` auto-on at high (security-sensitive) |
| `crates/ocx_lib/src/package_manager/**` | Adds `--breadth=adversarial` at high+ |
| Auth, crypto, signing paths | Adds `--codex`; security review required |
| `Cargo.toml` dependency changes | Adds `--breadth=full` (supply-chain scrutiny) |
| `deny.toml`, `.licenserc.toml` | Adds `--breadth=full` |
| Public API breakage (removed `pub` items) | → **max**, adds `--codex` |
| `crates/ocx_schema/**` | → **max** if changed (metadata schema is load-bearing) |

## PR label signals

When the target resolves to a PR, read labels and apply:

| Label | Effect |
|---|---|
| `breaking-change` | → **max**; `--codex` on |
| `security` | Adds `--breadth=adversarial`; `--codex` on |
| `epic` | → **max** |
| `small` | Hint toward **low** (but metrics can still escalate) |
| `docs` | Hint toward **low** if no code paths touched |
| `chore` | Hint toward **low** |

Labels never *demote* below what the metrics dictate — a `small` label
on a 30-file diff still classifies as high (size wins over label).

## Confidence rules

- **Confident**: one tier has ≥2 matching signals (from metrics OR
  markers OR labels) and no competing signals from an adjacent tier.
  Proceed without the meta-plan gate.
- **Low-confidence**: signals split across adjacent tiers (e.g.,
  metrics say `low`, one structural marker says `high`), or the diff is
  empty-but-metadata-only (e.g., rename-only), or the target is
  ambiguous. Flag; SKILL.md routes into the meta-plan gate.

Never manufacture a question when confident. *Announce and proceed*, or
*let the meta-plan gate handle it*.

## Overlay triggers

Overlays adjust a single axis on top of the chosen tier. They stack —
multiple triggers may fire. Axis definitions live in `overlays.md`.

| Overlay | Triggered by |
|---|---|
| `--breadth=full` | tier=high (default); `.github/workflows/**`, `Cargo.toml`, or dependency paths touched at tier=low (escalation) |
| `--breadth=adversarial` | tier=max (default); `package_manager/` touched at tier=high; `security` label; `--rca=on` together with ≥2 subsystems |
| `--reviewer=haiku` | tier=low AND NO structural markers from `classify.md:48-61` present in the diff |
| `--reviewer=opus` | tier=max AND `--breadth=adversarial` |
| `--doc-reviewer=haiku` | Diff touches ≤2 doc files (`website/**/*.md` or `CHANGELOG.md`) AND does not touch `website/src/docs/user-guide.md` |
| `--rca=on` | tier=high+ (default) — scope differs per tier (see overlays.md) |
| `--codex` | One-Way Door structural marker; `breaking-change` or `security` label; public API change; new crate; protocol change |

Defaults per tier (before overlays apply):

| Axis | low | high | max |
|---|---|---|---|
| breadth | minimal | full | adversarial |
| reviewer | haiku (→ sonnet on structural markers) | sonnet | sonnet (→ opus on adversarial breadth) |
| doc-reviewer | sonnet | sonnet (→ haiku on narrow doc scope) | sonnet (→ haiku on narrow doc scope) |
| rca | off | on (Block/High) | on (>Suggest) |
| codex | off | off (auto-on for One-Way Door signals) | on (mandatory) |

## Baseline interaction with auto-tier

`--base` changes what the classifier sees:

| Invocation | Typical diff size | Typical auto tier |
|---|---|---|
| `/swarm-review` (no base → `main`, long-lived branch) | 50+ files | **high** or **max** |
| `/swarm-review --base=HEAD~1` | ≤3 files | **low** |
| `/swarm-review --base=<parent-branch>` | a few commits | **low** or **high** |
| `/swarm-review --base=<older-tag>` | entire release delta | **max** |
| `/swarm-review <PR>` (base auto-resolved to PR base) | PR-sized diff | tier matches PR scope |

This is the intended design: **baseline controls effort**. A user who
wants a quick re-check of their last commit should pass
`--base=HEAD~1`; a user reviewing a release-cut should let the default
baseline expand the scope.

## Examples

1. `/swarm-review` on a 2-commit branch with 5 files in
   `crates/ocx_cli/src/command/` → tier=**high**, default overlays,
   confident. (Single subsystem, medium size.)
2. `/swarm-review --base=HEAD~1` on a one-line flag change →
   tier=**low**, `--breadth=minimal`, `--rca=off`, `--codex=off`,
   confident.
3. `/swarm-review 143` where PR #143 has labels
   `breaking-change` + `enhancement` and touches `crates/ocx_schema/`
   → tier=**max**, `--breadth=adversarial`, `--codex=on`, confident.
4. `/swarm-review --base=v0.5.0` on a branch that's 30 commits ahead
   → tier=**max** by metrics, meta-plan gate fires (max auto-fires
   gate).
5. `/swarm-review` with 4 files changed across `crates/ocx_lib/` and
   `crates/ocx_cli/` → metrics say `low` (size) but `high` (≥2
   subsystems); low-confidence → meta-plan gate fires.
