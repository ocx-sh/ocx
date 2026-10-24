# Classification Signals

Signal-to-tier map for `/hex-review` when `tier=auto`, plus the overlay
triggers that stack on the chosen tier. Unlike `/hex-plan` (classifies from
a prompt), hex-review classifies from **actual diff metrics** against the
resolved baseline. The classifier emits a tier — **only** `low`, `medium`,
`high`, or `xhigh` — with zero or more overlays. It picks the **lowest band
whose limits all hold**; markers and labels only raise it.

The classifier **never emits `max`** — that tier is explicit only,
`--tier=max` or a plan whose Status block says `Tier: max`
([`protocol.md`](../hex-core/references/protocol.md#tier-grammar)). When
signals split across adjacent tiers, or the overlay mix is unusual, mark
**low-confidence** — that forces the meta-plan gate in
[`SKILL.md`](SKILL.md) step 5 (in loop mode it is announced, not asked).
Never fire a mid-flow question; ambiguity is resolved at the single gate.

## Primary signal: diff metrics

Computed once at classification start, against the baseline
[`SKILL.md`](SKILL.md) step 2 resolved:

```
git diff <base>...<target> --name-only    # → changed-file list
git diff <base>...<target> --shortstat    # → lines added/removed
```

Derive:

- **file_count** — count of the changed-file list.
- **lines_changed** — added + removed from `--shortstat`.
- **areas_touched** — match each changed path against the project's own
  module/area boundaries, discovered from project rules and structure
  (project context, cached in `hex.md › Pointers`) — **never** a hardcoded
  table.
- **structural_markers** — see the table below.
- **pr_labels** — only when the target resolved to a PR.

## Tier metric table

Take the **lowest** row whose limits all hold.

| Tier | file_count | lines_changed | areas_touched |
|---|---|---|---|
| **low** | 1, or a docs-only delta of any size | ≤30 (docs-only: any) | 1 (docs-only: any) |
| **medium** | ≤3 | ≤100 | 1 |
| **high** | ≤15 | ≤500 | any |
| **xhigh** | >15 | >500 | any |

- **Docs-only** — every changed path is documentation (prose files, docs
  directories, changelog) and no path is behaviour: agent, skill, rule,
  config, manifest, and code paths are never docs.
- **Area count never raises past `high`.** A second area fails the `medium`
  row; it never reaches `xhigh`. Only size and the `xhigh` markers below do.

## Structural marker signals

Language-agnostic globs — never a hardcoded per-project path table. Project
hints in `hex.md › Preferences` (always-review paths, e.g. "security review
mandatory under `src/auth/**`") fold in on top of these before the gate.
Markers only **raise**: each sets a floor, never lowers a tier the metrics
gave.

| Marker | Tier impact |
|---|---|
| New package/crate manifest appears (`Cargo.toml`, `package.json`, `pyproject.toml`, `go.mod`, …) | → **xhigh** (new module/package) |
| CI workflow changes (`.github/workflows/**`, `.gitlab-ci.yml`, …) | Adds `breadth=full` minimum |
| Dependency-manifest changes (lockfiles: `Cargo.lock`, `package-lock.json`, `poetry.lock`, `go.sum`, …) | Adds `breadth=full` (supply-chain scrutiny) |
| Auth / crypto / signing paths — a path **word** is `auth`, `crypto`, `sign`, `signing`, `token`, `tokens`, `secret`, or `secrets` (words split on `/ . _ -`; `author` and `design` do not match) | → at least **high**; security perspective required |
| Generated-file churn (matches the project's documented generated-file markers) | Adds `breadth=full`; note in output as low review value — don't nitpick generated content |
| Public API surface files (exported/public entry points added, changed, or removed — from the project's own module boundaries) | → **xhigh** |
| `hex.md › Preferences` `perspectives.always` / `never` rule | `always`: adds the named perspective when its `when:` glob matches; `never`: removes the named perspective (a role-name list, no glob — `reviewer:security` additionally fail-closed, merge rule 5) ([`config.md` § Perspectives](../hex-core/references/config.md#perspectives)) |

The project may widen hex's sensitivity and may never subtract from it.

## PR label signals

When the target resolves to a PR, read its labels and apply:

| Label | Effect |
|---|---|
| `breaking-change` | → **xhigh** |
| `security` | Adds `breadth=full` or `adversarial` |
| `epic` | → **xhigh** |

Labels only raise — a `small` label on a 30-file diff still runs `xhigh`
(size beats label).

## Confidence rules

- **Confident** — the metrics land in one row and no marker or label
  competes. Announce and proceed.
- **Low-confidence** — a marker or label raises the tier past the metrics'
  row (e.g. metrics say `medium`, a structural marker says `xhigh`), the diff
  is metadata-only (a rename), or the target is ambiguous. Flag it;
  [`SKILL.md`](SKILL.md) routes into the meta-plan gate.

Never manufacture a question when confident: *announce and proceed*, or
*let the gate handle it*.

## Overlay triggers

Overlays adjust a single axis on top of the chosen tier and stack — several
may fire. Axis definitions and per-tier defaults are in
[`overlays.md`](overlays.md).

| Overlay | Triggered by |
|---|---|
| `breadth=full` | tier `high` default; CI-workflow, dependency-manifest, or generated-file markers at tier `medium` (escalation) |
| `breadth=adversarial` | tier `xhigh` and `max` default; a `security` label at tier `high` |
| `rca=on` | tier `high`+ (default) — scope differs per tier, see [`overlays.md`](overlays.md) |
| `adversary=on` | tier `medium`+ (default); tier `low` only by `--adversary` |

## Baseline interaction with `auto` tier

`--base` changes what the classifier sees — **baseline controls effort**:

| Invocation | Typical diff size | Typical auto tier |
|---|---|---|
| loop mode (no target — `anchor..HEAD`) | what landed since the last review | any |
| `/hex-review` (no target, no `--base` — long-lived branch vs `main`) | often large | `high` or `xhigh` |
| `/hex-review --base=HEAD~1` | ≤3 files | `medium` |
| `/hex-review --base=<parent-branch>` | a few commits | `medium` or `high` |
| `/hex-review --base=<older-tag>` | an entire release delta | `xhigh` |
| `/hex-review <PR>` (base auto-resolved to the PR base) | PR-sized diff | matches PR scope |

A quick re-check of the last commit: pass `--base=HEAD~1`. Reviewing a
release cut: let the default baseline expand scope.

## Plan and artifact targets

When the target is a markdown plan, ADR, or spec file (not a diff), skip
diff-metric classification entirely — there is no `<base>...<target>` to
measure. Default to tier `high` unless the user passes an explicit tier
or flag; the tier's breadth and RCA defaults still apply. The adversary
axis, when it fires, runs in `plan-artifact` scope instead of `code-diff`
([adversary contract](../hex-core/references/adversary.md#adversary-contract)).
Confidence is always **confident** for an explicit artifact path — there is
no metric ambiguity to flag.

## Examples

1. `/hex-review` on a 2-commit branch, 5 files in one area → tier
   **high**, default overlays, confident.
2. `/hex-review --base=HEAD~1` on a one-line flag change → tier **low**
   (one file, ≤30 lines) — the orchestrator reviews inline, zero spawns;
   the same change across three files → tier **medium**,
   `breadth=minimal`, `rca=off`, `adversary=off`, confident.
3. `/hex-review 143` where PR #143 carries `breaking-change` +
   `enhancement` and touches a new package manifest → tier **xhigh**,
   `breadth=adversarial`, `adversary=on`, confident.
4. `/hex-review --base=v0.5.0` on a branch 30 commits ahead → tier **xhigh**
   by metrics; the meta-plan gate fires (`xhigh` auto-fires the gate).
5. `/hex-review` with 3 files changed across two areas → fails the
   one-area `medium` row, so **high**; area count never raises past `high`.
6. `/hex-review` on 12 changed `*.md` doc files → docs-only, tier **low**.
7. A 20-line change under `src/auth/` → the security marker raises it from
   `low` to **high**; `author.md` would not.
8. `/hex-review docs/plans/2026-07-add-export.md` (a plan artifact, no
   diff) → skip diff metrics; tier defaults to **high**; an `on`
   adversary axis runs in `plan-artifact` scope.
