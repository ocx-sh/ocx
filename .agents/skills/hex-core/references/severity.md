# hex Finding Severity

A topic file of the hex swarm protocol; the spine is
[`protocol.md`](protocol.md).

## Finding severity

The shared severity vocabulary for review findings. **This is the only copy
in the bundle — reviewer.md, the hex-review verdict and RCA rules,
overlays.md, and the tier files link here, never restate it.** Severity is
assigned by the worker that raises the finding (the orchestrator synthesises,
it never invents a grade).

Severity is orthogonal to a finding's actionable/deferred class: class says
who fixes it, severity says how bad it is; every finding carries one of each.
The floor is a floor, not a ceiling — a consumer's own rule may raise a
finding above the label's floor.

| Severity | What it is | Verdict floor |
|---|---|---|
| Block   | Merge-unsafe: correctness, security, data-loss, or contract break. | Request Changes |
| High    | A real defect that should not merge unfixed, not unsafe on its own. | Needs Work |
| Warn    | A minor defect or smell — naming, small duplication, a narrow edge case. | Needs Work |
| Suggest | An optional improvement; no defect. | none — reported, but never gates the verdict |

High-and-above construct: at tier `low` and `medium` the ladder is not
applied — a finding there carries only its class, the tag is absent, and
the verdict runs off its
enumerated triggers. Presence of the tag is the signal that a run graded
severity; there is no schema-version marker.

One defect, highest severity wins: when more than one perspective reports the
same file:line defect it is one finding at the highest severity any
perspective gave it — no averaging. A panel Warn the cross-model pass raises
to Block resolves to Block; the escalation is already visible in the
Cross-Model section it was reported in.

Producer-local grades fold in here: doc-reviewer keeps emitting
Critical / Medium / Accuracy; the orchestrator maps them at synthesis —
Critical → High, Medium → Warn, Accuracy → Block (a doc now wrong about
shipped behavior is a contract defect). No producer defines a fifth level.

Nothing to report → no findings lines, just the verdict — the same
byte-identical-when-clean rule as the Convergence contract.

