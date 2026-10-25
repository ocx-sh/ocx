# hex Model Matrix

The single source of model guidance for the whole hex bundle. No other
file recommends models — [`workers.md`](workers.md) and the skill files
link here.

## The ladder

Four **capability classes**, never literal model names — the model behind
a class differs per client harness, so the shipped file stays portable.

| Class | Use |
|---|---|
| `light` | search, inventory, mechanical edits |
| `standard` | **default**: steps, fixers, review seats |
| `standard-high` | escalation; plan-marked hard steps |
| `deep` | architect and plan design; last-resort escalation |

`deep` is the strongest *worker-appropriate* class, never the model that
runs the session. Tier never changes a class; it scales seat counts only.

## The matrix

| Role | Class |
|---|---|
| explorer | `light` |
| architecture-explorer | `standard` |
| researcher | `standard` |
| builder (every focus) | `standard` |
| tester | `standard` |
| reviewer (every focus) | `standard` |
| doc-reviewer | `standard` |
| simulator | `standard` |
| architect | `deep` |

`deep` review seats and simulators run only when the user asks.

## Rules

1. **A class above its row has a stated reason** — escalation, a plan mark,
   or a user request. Announce blocks and quiet-form lines print each
   spawn's resolved literal model
   ([`protocol.md`](protocol.md#the-meta-plan-approval-gate)).
2. **Escalation is the orchestrator's alone**, on repeated failure of the
   **same step** — it returned incomplete, or its output was rejected. A
   red TDD phase is not a failure. Sequence: retry at the same class with
   the failure attached → one class up → defer as residue while the loop
   continues. Workers never escalate themselves.
3. **Plan marks reach `standard-high` only**, on at most 1 in 4 pipelines.
4. **Instantiation.** `/hex-init` writes each class as an agent definition
   with `model` and `effort` frontmatter. Claude Code supports per-agent
   `effort` (`low`…`max`); the Agent tool has no per-spawn effort, hence
   the definitions. On a Claude harness, for example: `light` → Haiku,
   `standard` → Sonnet, `standard-high` → Sonnet at `effort: high`,
   `deep` → Opus. Per-role overrides: `models.overrides`
   ([`config.md`](config.md#key-vocabulary)), a map `role[:focus]` → class.
5. **The ladder outranks any harness-global routing table** for hex
   spawns; global routing applies outside hex runs.
6. **Never map a class to an orchestrator-class model** — one above the
   strongest worker model. The session model is the user's or harness's
   choice, outside this matrix.
