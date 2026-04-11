# Deferred: Add GitHub + Context7 MCP servers

Phase A4 is blocked because `.mcp.json` is protected by `.claude/hooks/pre_tool_use_validator.py`
(entry in `_PROTECTED_PATTERNS`). This is intentional — MCP config may contain auth tokens.

**Action required from Michael:** Create `.mcp.json` at the repo root with the following content,
or merge into existing if already present:

```json
{
  "mcpServers": {
    "github": {
      "command": "sh",
      "args": [
        "-c",
        "GITHUB_PERSONAL_ACCESS_TOKEN=$(gh auth token) exec npx -y @modelcontextprotocol/server-github"
      ]
    },
    "context7": {
      "command": "npx",
      "args": ["-y", "@upstash/context7-mcp"]
    }
  }
}
```

Notes:
- `settings.json` already has `enableAllProjectMcpServers: true` — no changes needed there.
- `mcp__context7__*` tools are already in the allow list, so they will work once the server is added.
- **On the `gh auth token` wrapper**: `.mcp.json` is pure JSON with `${VAR}` env-var
  expansion only; it does NOT support shell command substitution inside the JSON itself.
  The pattern above works because `command: "sh"` launches a shell that evaluates
  `$(gh auth token)` before `exec`-ing the real MCP binary. The token is fetched
  fresh on every server start, so `gh` credential rotation is picked up automatically.
  This only works for stdio MCP servers (command+args), not HTTP servers.
- Alternatives if you prefer HTTP transport or want to avoid the wrapper:
  1. Export `GITHUB_PERSONAL_ACCESS_TOKEN="$(gh auth token)"` in `~/.zshrc` and use
     `"${GITHUB_PERSONAL_ACCESS_TOKEN}"` in the JSON directly (token cached at shell start).
  2. Use a SessionStart hook to populate the env var before Claude Code parses `.mcp.json`.
- After adding, run `/mcp` in Claude Code to confirm both servers load and validate
  the context cost is <2% of budget (reject if either overflows).

Downstream dependencies (Phase A5) have been executed speculatively — they reference
the MCP tools as "preferred path" with `gh`/WebFetch as fallback, so they remain
valid even until the servers land.
