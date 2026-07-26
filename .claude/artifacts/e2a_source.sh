#!/usr/bin/env bash
# E2a is only non-vacuous if the REGISTRY actually carries the reserved tag
# classes the root must not contain. A clean root over a registry that never
# had them would prove nothing.
set -euo pipefail
REPO=michael-herwig/ocx-e2e-hello
TOK="$(curl -fsS "https://ghcr.io/token?scope=repository:${REPO}:pull&service=ghcr.io" |
    python3 -c 'import json,sys;print(json.load(sys.stdin)["token"])')"
curl -fsS -H "Authorization: Bearer $TOK" "https://ghcr.io/v2/${REPO}/tags/list?n=200" |
    python3 -c "
import json,sys,re
tags=json.load(sys.stdin).get('tags',[])
ocx=[t for t in tags if re.match(r'(?i)^__ocx',t)]
canon=[t for t in tags if re.match(r'^sha256\.[0-9a-f]{64}$',t)]
print('registry tags total :', len(tags))
print('__ocx* present      :', ocx or 'NONE')
print('sha256.<hex> present:', f'{len(canon)} (e.g. {canon[0][:20]}...)' if canon else 'NONE')
print()
print('E2a is non-vacuous:', 'YES' if (ocx or canon) else 'NO — tripwire had nothing to catch')
"
