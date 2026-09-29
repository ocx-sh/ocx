---
summary: Bundle-generic rule — re-anchor hex state from files, never conversation memory
keywords: hex,state,rule,compaction,memory,re-anchor,discuss,finalize,backup-ref,retro,friction
license: Apache-2.0
repository: https://github.com/michael-herwig/arcana
---

# Hex state lives in files, not in conversation memory

Every hex mode's state locations resolve via `hex.md › Pointers` (`.agents/memory/hex.md`, searched upward).
Check them before any turn that would otherwise edit code or config — not on every turn, not once at load.

Any discussion artifact at `State: active` in the discussions home (documented convention via `hex.md › Pointers`, else `.agents/discussions/`) that is
git-untracked or locally modified → no code or config edits; re-read that artifact and the `hex-discuss` skill file before acting. A committed, unmodified copy is another session's in-flight discussion — inert.
No discussions home on disk means nothing to check — that negative holds until a hex skill runs. A stale or abandoned `active` artifact is released by parking it (`State: parked`).

An armed `backup/<branch>-pre-finalize` ref for the checked-out branch means a `/hex-finalize` is in flight or was interrupted → do not commit onto, rewrite, or merge that branch.
Re-read the `finalize.md` contract and re-enter `/hex-finalize`, or release it by renaming the ref inert (that file gives the one command). Absence of the armed ref means nothing to check.

Real friction with a skill, rule or project config (a retry, a corrected assumption, a workaround, a step far over its expected time, a misleading instruction) → write one retro entry; never edit skills or rules for it — `/hex-retro` proposes.
Entry: JSON `{"v":1,"ts":<UTC ISO-8601>,"kind":"slow|inconvenient|pitfall|defect","scope":"harness|project","artifact":<skill or rule name, or "project">,"what":…,"tell":<observable symptom>}` — `scope` is `harness` if a different codebase with the same skills would hit it too, else `project`; optional `proposed_change`, `severity` (low|medium|high), `evidence`; no secrets or raw output.
Inbox: `inbox/` under the `hex.md › Pointers` `Retro:` home, else `.agents/retro/inbox/`, in the main checkout (first `git worktree list` entry) — a plain in-repo dir, never via a symlink; write `.<name>.tmp` there, then rename it to `<YYYYMMDDTHHMMSSZ>-<8 random hex>.json` — no lock, never touch another entry.

After compaction, re-anchor from these files.
