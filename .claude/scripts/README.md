# `.claude/scripts/`

User-facing bash tooling for the OCX Claude Code setup. (Hooks live in `.claude/hooks/` — Python + uv.)

## `statusline.sh`

Cross-platform Claude Code status line. Robbyrussell-inspired. Reads JSON from
stdin, renders 1–2 colored lines:

```
ocx-evelynn git:(evelynn*)  Sonnet 4.6 ████░░░░░░ 38% ⏱ 12m
current ████░░░░░░ 42% ⟳ 6:00pm   weekly ██░░░░░░░░ 18% ⟳ apr 18, 12:00am
```

Line 1 always renders: directory, git branch (with `*` when dirty), model,
context-window bar+%, session duration. Line 2 renders only when Claude Code
provides `rate_limits` on stdin (newer versions).

### Stdin fields consumed

- `workspace.current_dir` / `cwd`
- `model.display_name`
- `context_window.context_window_size`
- `context_window.current_usage.{input_tokens, cache_creation_input_tokens, cache_read_input_tokens}`
- `session.start_time` (ISO8601)
- `rate_limits.five_hour.{used_percentage, resets_at}`
- `rate_limits.seven_day.{used_percentage, resets_at}`

All fields are tolerant of missing/null — the script degrades gracefully.

### Platform support

Linux, macOS, WSL2 — anything with GNU `date` *or* BSD `date`. The date helpers
try GNU syntax first, fall back to BSD. Alpine BusyBox is best-effort: time
strings will be omitted if neither branch works.

## `statusline-install.sh` (script) / `statusline:install` (task)

One-shot installer. Copies `statusline.sh` to `~/.claude/statusline-command.sh`
with mode 755. Idempotent. If `~/.claude/settings.json` does not yet reference
the statusline, prints the activation snippet.

### Install

```sh
task claude:statusline:install
```

Or directly:

```sh
bash .claude/scripts/statusline-install.sh
```

Future related tasks (lint, render-test, etc.) will live under the same
`statusline:*` namespace.

### Activation snippet

Add this to `~/.claude/settings.json` once:

```json
"statusLine": {
  "type": "command",
  "command": "bash $HOME/.claude/statusline-command.sh"
}
```
