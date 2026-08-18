#!/usr/bin/env bash
# Regenerate the test-only key material for the local Sigstore stack.
#
# The output of this script is COMMITTED to the repository. These are throwaway
# keys for a stack that only ever runs on localhost against a test registry;
# they are deliberately not secret. Committing them is what makes the trust
# root (trusted_root.json) a static file instead of something the acceptance
# suite has to mint at runtime.
#
# Re-run only when the material must change (e.g. the CA expires). Regenerating
# invalidates trusted_root.json -- run generate-trusted-root.sh afterwards.
#
# Usage: ./generate-test-keys.sh [output-dir]   (default: ./keys)
set -euo pipefail

OUT="${1:-$(cd "$(dirname "$0")" && pwd)/keys}"
mkdir -p "$OUT"

# --- Fulcio CA ---------------------------------------------------------------
# Fulcio runs with `--ca fileca`, not `ephemeralca`: the CT log needs Fulcio's
# root at ITS startup, so the root cannot be minted at Fulcio's startup.
openssl ecparam -name prime256v1 -genkey -noout -out "$OUT/fulcio-ca.key.pem"
openssl ec -in "$OUT/fulcio-ca.key.pem" -out "$OUT/fulcio-ca.enc.pem" \
    -aes256 -passout pass:ocxtest
openssl req -x509 -new -key "$OUT/fulcio-ca.key.pem" -sha256 -days 3650 \
    -subj '/O=ocx test/CN=ocx test fulcio CA' \
    -addext 'basicConstraints=critical,CA:TRUE' \
    -addext 'keyUsage=critical,keyCertSign,cRLSign' \
    -out "$OUT/fulcio-ca.crt.pem"

# --- CT log signer (TesseraCT, static-ct-api) --------------------------------
# A plain P-256 PEM: TesseraCT takes the signer directly, no note-format tooling.
openssl ecparam -name prime256v1 -genkey -noout -out "$OUT/ct.key.pem"
openssl ec -in "$OUT/ct.key.pem" -pubout -out "$OUT/ct.pub.pem"

# --- Rekor signer ------------------------------------------------------------
openssl ecparam -name prime256v1 -genkey -noout -out "$OUT/rekor.key.pem"
openssl ec -in "$OUT/rekor.key.pem" -pubout -out "$OUT/rekor.pub.pem"

# The stack containers run as uid 65532 and only ever read these.
chmod 644 "$OUT"/*.pem

echo "wrote test key material to $OUT"
openssl x509 -in "$OUT/fulcio-ca.crt.pem" -noout -subject -dates
