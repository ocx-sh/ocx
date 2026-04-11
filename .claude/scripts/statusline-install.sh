#!/usr/bin/env bash
# Install the OCX statusline to ~/.claude/statusline-command.sh
set -euo pipefail

src_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
src="$src_dir/statusline.sh"
dst="$HOME/.claude/statusline-command.sh"
settings="$HOME/.claude/settings.json"

mkdir -p "$HOME/.claude"
install -m 755 "$src" "$dst"
echo "Installed: $dst"

if [ ! -f "$settings" ] || ! grep -q 'statusline-command.sh' "$settings" 2>/dev/null; then
    cat <<'EOF'

To activate, add this to ~/.claude/settings.json:

  "statusLine": {
    "type": "command",
    "command": "bash $HOME/.claude/statusline-command.sh"
  }
EOF
fi
