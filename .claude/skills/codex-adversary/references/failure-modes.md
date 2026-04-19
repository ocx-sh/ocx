# Failure Modes

Reference for edge cases and error paths in `/codex-adversary`.

## Codex unavailable

Companion returns non-zero or reports not ready. Stop with the companion's diagnostic output and suggest `/codex:setup`.

## No changes to review

Stop with "nothing to review" after double-checking untracked files (Codex can review untracked work).

## Codex output empty or garbled

Return Codex's stdout verbatim and skip triage. Tell the user the review appears empty and ask if they want to rerun.

## `task verify` fails after auto-fix

Revert the failing edit, mark that item `⚠️ reverted-and-promoted`, continue with the rest.
