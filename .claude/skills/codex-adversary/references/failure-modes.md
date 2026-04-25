# Failure Modes

Reference for edge cases + error paths in `/codex-adversary`.

## Codex unavailable

Companion return non-zero or report not ready. Stop with companion's diagnostic output. Suggest `/codex:setup`.

## No changes to review

Stop with "nothing to review" after double-check untracked files (Codex can review untracked work).

## Codex output empty or garbled

Return Codex's stdout verbatim. Skip triage. Tell user review appears empty. Ask if rerun wanted.

## `task verify` fails after auto-fix

Revert failing edit. Mark item `⚠️ reverted-and-promoted`. Continue rest.