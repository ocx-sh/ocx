---
name: hex-core
description: Shared reference library for the hex swarm-orchestration bundle - worker role registry, model capability matrix, swarm protocol vocabulary, and the .agents/memory/hex.md memory spec. Not invoked directly; the hex-plan, hex-execute, hex-review, hex-architect, and hex-init skills link into it.
license: Apache-2.0
metadata:
  keywords: swarm,orchestration,workers,models,protocol,memory,library
  repository: https://github.com/michael-herwig/arcana
  summary: Shared reference library for the hex swarm bundle
disable-model-invocation: true
---

# hex-core — Shared Reference Library

`hex-core` is the library skill of the **hex** swarm bundle. It holds the
vocabulary and contracts every other hex skill builds on, kept in one
place so they never drift apart. It is **never invoked directly** — there
is no orchestration flow here and no `$ARGUMENTS`. The orchestrator skills
(`/hex-plan`, `/hex-execute`, `/hex-review`, `/hex-architect`) and
`/hex-init` link into these references.

## References

| Reference | Holds |
|---|---|
| [`references/protocol.md`](references/protocol.md) | The bundle's spine — shared shape, tier and overlay grammar, the meta-plan approval gate, spawn-selection precedence, worker coordination (including `### Worker liveness` and degraded no-subagent mode), traceability IDs, untrusted-text echoes, the constitution gate, the handoff contract, and the per-run upkeep step: what every mode needs. Also carries the load map (which mode opens which sibling beyond the spine) and the what-moved-where pointer table into the six sibling files below, which now hold the per-topic contracts protocol.md used to carry directly. |
| [`references/loop.md`](references/loop.md) | The canonical Review-Fix Loop (`### The last-reviewed anchor`, `### Anchor validation`, `### Delta round scope`, `### The diminishing-returns stop`) and the Convergence contract. **Conditional-load** — see [`protocol.md`](references/protocol.md)'s load map for which modes open it. |
| [`references/decompose.md`](references/decompose.md) | Parallel-by-default decomposition and `### The effective tier`. **Conditional-load** — see [`protocol.md`](references/protocol.md)'s load map for which modes open it. |
| [`references/worktree.md`](references/worktree.md) | Worktree work-package mechanics. **Conditional-load** — see [`protocol.md`](references/protocol.md)'s load map for which modes open it. |
| [`references/verify.md`](references/verify.md) | Verification, `### Scoped check`, and `### Checkpoints`. **Conditional-load** — see [`protocol.md`](references/protocol.md)'s load map for which modes open it. |
| [`references/adversary.md`](references/adversary.md) | The adversary contract. **Conditional-load** — opened only when `adversary=on`; see [`protocol.md`](references/protocol.md)'s load map. |
| [`references/severity.md`](references/severity.md) | Finding severity. **Conditional-load** — see [`protocol.md`](references/protocol.md)'s load map for which modes open it. |
| [`references/workers.md`](references/workers.md) | The worker role **index** plus the universal worker protocol; full personas (mission, spawn-prompt template, focus modes, output contract) live one file per role under `references/workers/` — explorer, architecture-explorer, researcher, builder, tester, reviewer, doc-reviewer, architect, coordinator. Orchestrators load only the personas in the resolved spawn set. Workers are prompt blocks the orchestrator copies into a subagent, not shipped agent files. |
| [`references/models.md`](references/models.md) | The one model matrix for the whole bundle: recommended capability class per role × tier, plus the escalation, resolution-order, and instantiation rules. No model guidance lives anywhere else in hex. |
| [`references/memory.md`](references/memory.md) | The `.agents/memory/hex.md` memory spec: the three sections and their per-section ownership, resolution order, a complete example file, plus the editing, destination-choice, and staleness rules. |
| [`references/config.md`](references/config.md) | The `hex.md › Preferences` config vocabulary — keys, defaults, merge rules. **Conditional-load** — read only when `hex.md › Preferences` contains a fenced `yaml` block. |
| [`references/archive.md`](references/archive.md) | The spec fold-back mechanics — delta grammar, destination resolution, the safety envelope and its four commands, halt semantics, idempotence and the fold receipt, revert, and the plan archive. Sole definition site; `hex-execute`, `hex-review`, and `protocol.md` link here. **Conditional-load** — read only when the plan under review carries a `## Spec Deltas` block. |
| [`references/finalize.md`](references/finalize.md) | The remote-rights boundary — the act set and its branch scoping, the consent model, the literal force-push form, the backup-ref armed/inert lifecycle, remote verification and its ceilings, re-entry, the degrade ladder, the trust classes, the pre-flight halt texts, and the placeholder-substitution rule. Sole definition site; every bundle-wide remote-rights qualifier links here. **Conditional-load** — read only when finalizing a branch or resolving a remote-rights qualifier. |
| [`references/resources.md`](references/resources.md) | The resource knob sheet — the measured resource profile, the heavy semaphore, the per-run scratch environment, the containment ladder, the per-ecosystem knob sheet, teardown, and output as a resource signal. Sole definition site; hex never defines how to verify a project. **Conditional-load** — read only when a run will issue a heavy command. |

## How sibling skills reference this

Every hex skill installs as a sibling under the client's skills directory,
so references resolve by relative path:

```markdown
See [`../hex-core/references/protocol.md`](../hex-core/references/protocol.md).
```

If `hex-core` is not installed, add it:

```sh
grim add ghcr.io/michael-herwig/arcana/hex-core:latest
```

Cross-skill mentions elsewhere use the command form: `/hex-init`,
`/hex-discuss`, `/hex-plan`, `/hex-execute`, `/hex-review`,
`/hex-architect`, `/hex-finalize`.
