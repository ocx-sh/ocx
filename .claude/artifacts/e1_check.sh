#!/usr/bin/env bash
# E1 (g2) assertion, run by hand against the PR head — same protocol E1-pre used.
# Proves: the committed CAS object is byte-identical to what GHCR serves under
# the same digest, and the registry agrees on that digest.
set -euo pipefail

SP="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO=michael-herwig/ocx-e2e-hello
OBJ=sha256:50e02438d1d8e4968ad9a663d29185638931b2771e7e4f68cc9923926ccb5ee1

TOK="$(curl -fsS "https://ghcr.io/token?scope=repository:${REPO}:pull&service=ghcr.io" |
    python3 -c 'import json,sys;print(json.load(sys.stdin)["token"])')"

http="$(curl -sS -D "$SP/hdr.txt" -o "$SP/served.json" -w '%{http_code}' \
    -H "Authorization: Bearer $TOK" \
    -H "Accept: application/vnd.oci.image.index.v1+json,application/vnd.docker.distribution.manifest.list.v2+json" \
    "https://ghcr.io/v2/${REPO}/manifests/${OBJ}")"

served_digest="$(grep -i '^docker-content-digest' "$SP/hdr.txt" | tr -d '\r' | awk '{print $2}')"

echo "HTTP status            : $http"
echo "requested digest       : $OBJ"
echo "Docker-Content-Digest  : ${served_digest:-<absent>}"
echo "committed bytes        : $(wc -c <"$SP/committed.json")"
echo "served bytes           : $(wc -c <"$SP/served.json")"

[ "$http" = 200 ] || {
    echo "VERDICT: FAIL — registry did not serve the object by digest"
    exit 1
}
[ "$served_digest" = "$OBJ" ] || {
    echo "VERDICT: FAIL — registry returned a different digest (conversion)"
    exit 1
}
cmp "$SP/committed.json" "$SP/served.json" || {
    echo "VERDICT: FAIL — bytes differ"
    exit 1
}
echo "VERDICT: PASS — committed object is byte-identical to the registry's own bytes"
