#!/bin/bash
set -e

cd "$CLAUDE_PROJECT_DIR/.claude/hooks"
cat | bun skill-activation-prompt.ts
