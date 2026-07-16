# Codex Model Selection (tiered)

Reference for the `--model` flag of `/codex-adversary`. Match model weight
to review weight — heavier diffs / One-Way-Door changes earn a stronger
reviewer. The tier→model **policy** (which caller tier picks which model)
lives in [`workflow-swarm.md`](../../../rules/workflow-swarm.md)
"Cross-model model tiers"; this file is the operational alias→slug map.

| Alias | Slug | Claude analogue | Codex description | Use |
|---|---|---|---|---|
| `luna` | `gpt-5.6-luna` | Haiku | Fast and affordable | explicit cheap override; light inline passes. Codex CLI default. |
| `terra` | `gpt-5.6-terra` | Sonnet | Balanced everyday | default (tier `low` opt-in + tier `high` default-on); **this skill's default when `--model` omitted**. |
| `sol` | `gpt-5.6-sol` | Opus | Frontier agentic coding | tier `max`; security / protocol / breaking-API One-Way-Door diffs. |

## Resolution rules

- `--model` accepts an **alias** (`luna|terra|sol`) or a full slug.
- The companion (`codex-companion.mjs adversarial-review`) passes the
  value straight to `codex -m` and does **not** know the aliases — resolve
  the alias to its slug (table above) before building the command.
- No `--model` → `gpt-5.6-terra`.
- Callers (`/swarm-review`, `/swarm-execute`, `/swarm-plan`, bugfix /
  refactor cross-model passes) pass `--model <alias>` per their tier;
  a user `--codex-model` / `--model` override always wins.

Model slugs verified accepted on the current auth via `codex exec -m
<slug>` (2026-07-10). Update this table when Codex rotates its tier names
(prior generations: `gpt-5.4` / `gpt-5.4-mini`, `gpt-5.3-codex-spark`).
