# hex-state — per-client reach

Where `hex-state` — the bundle's single always-on rule, and the artifact that
carries `hex-discuss`'s stance — lands on each client grim publishes to. This
file is the reach table's single documentation site, not the rule: a reach
table is neither trigger, stance, nor pointer, and every line of it inside the
rule would be permanent instruction budget spent on all clients to describe
the clients. The table is derived from grim's per-client transform table,
never authored independently, and was verified against grim's behavior at
authoring (2026-08-29); re-verify it at each grim minor.

| Reach | Client | How the rule lands |
|---|---|---|
| **Native** | Claude Code | `.claude/rules/hex-state.md`, natively discovered and reloaded |
| | Cursor | `alwaysApply: true` |
| | Copilot | `.github/instructions/`, global `~/.copilot/instructions/` |
| | Kiro | `inclusion: always` |
| **Degraded** | OpenCode | frontmatter stripped; body registered as a managed always-on glob |
| | Junie | project scope only — no per-file activation key |
| **Absent** | Codex, Gemini, Zed, Amp | no ownable on-disk rule path |

Exemplary for these ten clients, not exhaustive: any grim client not listed
defaults to absent-or-degraded per grim's transform table, which stays the
authority. On an absent client the skill runs unchanged; only the stance may
lapse after a compaction, recovering as
[`SKILL.md` § Constraints](../SKILL.md#constraints) states.
