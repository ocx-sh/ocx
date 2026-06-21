#!/usr/bin/env bash
# Idempotent setup for the OCX patches manual exploration environment.
#
# Builds and pushes:
#   - Two BASE packages: patches/base-tool:1.0.0 and patches/base-java:1.0.0
#   - Three COMPANION packages (env-only, interface visibility):
#       patches/corp-ca-bundle:1.0.0  — SSL_CERT_FILE, NODE_EXTRA_CA_CERTS, REQUESTS_CA_BUNDLE
#       patches/java-truststore:1.0.0 — JAVA_TOOL_OPTIONS, JVM_TRUSTSTORE_PATH
#       patches/license-server:1.0.0  — LICENSE_SERVER, LM_LICENSE_FILE
#   - Publishes THREE descriptors:
#       1. Global root (--global-root): match="*", companion=corp-ca-bundle, required=true
#       2. Java-specific: match="*/base-java:*", companion=java-truststore, required=true
#       3. Fail-open (base-tool): match="*/base-tool:*", companion=license-server, required=false
#
# Prerequisites:
#   source test/manual/scripts/env.sh      (sets OCX_DEFAULT_REGISTRY, OCX_HOME, etc.)
#   cd test && docker compose up -d        (registry at localhost:5000)
#   cargo build --release                  (binary at ./target/release/ocx)
#
# Idempotent: safe to re-run; each step overwrites previous artifacts.
set -euo pipefail
IFS=$'\n\t'

# ── Pre-flight checks ─────────────────────────────────────────────────────────

if [[ -z "${OCX_DEFAULT_REGISTRY:-}" ]]; then
    echo "error: source test/manual/scripts/env.sh first" >&2
    exit 1
fi
if [[ "${OCX_DEFAULT_REGISTRY}" != localhost:* ]]; then
    echo "error: this rig only targets localhost; OCX_DEFAULT_REGISTRY=${OCX_DEFAULT_REGISTRY}" >&2
    exit 1
fi

REGISTRY="${OCX_DEFAULT_REGISTRY}"
TAG="1.0.0"
PLATFORM="${OCX_MANUAL_PLATFORM:-linux/amd64}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"
MANUAL_ROOT="${REPO_ROOT}/test/manual"
PKG_ROOT="${MANUAL_ROOT}/packages/patches"

# Use the release binary so patch verbs are available.
OCX_BIN="${REPO_ROOT}/target/release/ocx"
if [[ ! -x "${OCX_BIN}" ]]; then
    echo "error: ${OCX_BIN} not found. Run: cargo build --release" >&2
    exit 1
fi

# Pull in the colored ocx wrapper + log helpers from the manual rig.
# shellcheck disable=SC2034  # consumed by ocx() in _lib.sh
# shellcheck source=./_lib.sh
# shellcheck disable=SC1091
source "${SCRIPT_DIR}/_lib.sh"

# ── Config paths ──────────────────────────────────────────────────────────────

# The patch registry is the same local OCI registry (localhost:5000).
# OCX_INSECURE_REGISTRIES=localhost:5000 (set by env.sh) makes it HTTP.
# Patch descriptors are stored under the patch registry using the path template
# which defaults to "{registry}/{repository}" — so descriptors for a base at
# localhost:5000/patches/base-tool:1.0.0 will be stored at:
#   localhost:5000/localhost_5000/patches/base-tool:__ocx.patch  (per-package)
#   localhost:5000:__ocx.patch                                    (global root)
PATCHES_REGISTRY="${REGISTRY}"
CONFIG_DIR="${OCX_HOME}"
CONFIG_FILE="${CONFIG_DIR}/config.toml"

# ── Helpers ───────────────────────────────────────────────────────────────────

# Short (registry-relative) and fully-qualified identifiers.
id_of() { echo "${REGISTRY}/${1}:${TAG}"; }
fq_of() { echo "${REGISTRY}/${1}:${TAG}"; }

# Build a minimal stub payload tree for a package.
# Companions (env-only) have NO binaries — the content dir carries only the
# files referenced in env entries so installPath values remain valid paths.
scaffold_companion() {
    local name="$1"
    local dir="${PKG_ROOT}/${name}/build"
    mkdir -p "${dir}"
    # No bin/ for companions; create any referenced sub-dirs so paths exist.
}

scaffold_corp_ca() {
    local dir="${PKG_ROOT}/corp-ca-bundle/build"
    mkdir -p "${dir}/certs"
    # Write a minimal self-signed PEM stub so SSL_CERT_FILE points at a real
    # (though non-functional) file — useful for inspection.
    cat >"${dir}/certs/corp-ca.pem" <<'PEMEOF'
-----BEGIN CERTIFICATE-----
MIICpDCCAYwCCQDmockCorpRootCA0AkGA1UEBhMCVVMxEzARBgNVBAgMClN0
YXRlIEhlcmUxHDAaBgNVBAoME0NvcnBvcmF0ZSBDQSBTYW1wbGUxEDAOBgNV
BAMMBkNvcnBDQTAeFw0yNjAxMDEwMDAwMDBaFw0yNzAxMDEwMDAwMDBaME4x
CzAJBgNVBAYTAlVTMRMwEQYDVQQIDApTdGF0ZSBIZXJlMRwwGgYDVQQKDBND
b3Jwb3JhdGUgQ0EgU2FtcGxlMQwwCgYDVQQDDANDQTCBnzANBgkqhkiG9w0B
AQEFAAOCAY8AMIIBigKCAYEA1dummySELF1SIGNEDforDEMOpurposes0nlyDo
NOT1USE1IN1productionPLEASE1ignore1invalid1base64here1padding==
-----END CERTIFICATE-----
PEMEOF
}

scaffold_java_truststore() {
    local dir="${PKG_ROOT}/java-truststore/build"
    mkdir -p "${dir}/truststore"
    # Write a placeholder JKS file (binary stub, non-functional).
    printf 'STUB_TRUSTSTORE' >"${dir}/truststore/corp-trust.jks"
}

scaffold_license_server() {
    local dir="${PKG_ROOT}/license-server/build"
    mkdir -p "${dir}"
    # License server companion has no binary files — all env entries are string
    # constants. A placeholder file is required so the tar layer has a content/
    # subdir after extraction; OCX install expects content/ to exist.
    touch "${dir}/.keep"
}

scaffold_base_tool() {
    local dir="${PKG_ROOT}/base-tool/build"
    mkdir -p "${dir}/bin"
    cat >"${dir}/bin/mytool" <<'EOF'
#!/usr/bin/env bash
echo "mytool (TOOL_HOME=${TOOL_HOME:-unset}, SSL_CERT_FILE=${SSL_CERT_FILE:-NOT SET})"
EOF
    chmod 0755 "${dir}/bin/mytool"
}

scaffold_base_java() {
    local dir="${PKG_ROOT}/base-java/build"
    mkdir -p "${dir}/bin"
    cat >"${dir}/bin/java" <<'EOF'
#!/usr/bin/env bash
echo "java stub (JAVA_HOME=${JAVA_HOME:-unset}, JAVA_TOOL_OPTIONS=${JAVA_TOOL_OPTIONS:-NOT SET})"
EOF
    chmod 0755 "${dir}/bin/java"
}

# Push a simple single-layer package. Args: subpath (relative to PKG_ROOT).
push_patch_pkg() {
    local subpath="$1"
    local bundle_dir="${PKG_ROOT}/${subpath}"
    local out_dir="${bundle_dir}/out"
    local bundle="${out_dir}/${subpath##*/}-${TAG}.tar.xz"

    cp "${bundle_dir}/metadata.in.json" "${bundle_dir}/metadata.json"

    ocx_step "${subpath}: create + push"
    (
        ocx_cd "${bundle_dir}"
        mkdir -p out
        ocx package create --force -m metadata.json -o "${bundle}" build
        ocx package push -n -c -p "${PLATFORM}" -m metadata.json \
            -i "$(id_of "patches/${subpath##*/}")" "${bundle}"
    )
}

manifest_digest() {
    local repo="$1"
    local accept digest
    for accept in \
        "application/vnd.oci.image.index.v1+json" \
        "application/vnd.oci.image.manifest.v1+json"; do
        digest=$(curl -fsSI -H "Accept: ${accept}" \
            "http://${REGISTRY}/v2/${repo}/manifests/${TAG}" \
            2>/dev/null | tr -d '\r' | awk -F': ' '/^Docker-Content-Digest/ {print $2}')
        if [[ -n "${digest}" ]]; then
            echo "${digest}"
            return 0
        fi
    done
    echo "error: could not fetch manifest digest for ${REGISTRY}/${repo}:${TAG}" >&2
    return 1
}

# ── Step 0: Write [patches] config to OCX_HOME/config.toml ───────────────────

ocx_step "writing [patches] config to ${CONFIG_FILE}"
mkdir -p "${CONFIG_DIR}"
cat >"${CONFIG_FILE}" <<EOF
# Manual patches exploration config — written by setup-patches.sh.
# Points OCX at the local registry for patch descriptor storage.
# The path template "{registry}/{repository}" (the default) means patch
# descriptors for localhost:5000/patches/base-tool:1.0.0 will be stored at:
#   localhost:5000/localhost_5000/patches/base-tool:__ocx.patch  (per-package)
#   localhost:5000:__ocx.patch                                    (global root)

[patches]
registry = "${PATCHES_REGISTRY}"
required = true
EOF
ocx_done "config written: registry = ${PATCHES_REGISTRY}"

# ── Step 1: Build and push companion packages (env-only) ──────────────────────

ocx_step "scaffolding companion payloads"
scaffold_corp_ca
scaffold_java_truststore
scaffold_license_server

push_patch_pkg corp-ca-bundle
push_patch_pkg java-truststore
push_patch_pkg license-server

# ── Step 2: Build and push base packages ──────────────────────────────────────

ocx_step "scaffolding base payloads"
scaffold_base_tool
scaffold_base_java

push_patch_pkg base-tool
push_patch_pkg base-java

# ── Step 3: Publish patch descriptors ─────────────────────────────────────────
#
# NOTE ON GLOBAL ROOT: The `ocx patch publish --global-root` command pushes
# the global descriptor to the OCI registry with an empty repository path.
# Docker's registry:2 rejects empty-path repositories (returns 404 for
# `POST /v2//blobs/uploads/`). This is a registry:2 limitation, not an OCX bug.
# In production, the patch registry is a separate host (e.g.
# patches.corp.internal) that supports the empty-path root.
#
# WORKAROUND FOR LOCAL RIG: publish the "global" corp CA bundle descriptor to
# each base package's path individually with match="*" so it matches any base.
# For two bases (base-tool, base-java) this is two publish operations.
# A real corp operator would use a proper patch registry host that supports
# the global root path.

DESCRIPTORS="${PKG_ROOT}/descriptors"

# 3a. Corp CA bundle published to base-tool's path (match=*, required=true).
#     In production this would be `--global-root`; here we publish per-base.
ocx_step "publishing corp-ca-bundle descriptor to base-tool path (match=*, required=true)"
ocx patch publish \
    --descriptor-file "${DESCRIPTORS}/global.json" \
    "$(id_of "patches/base-tool")"

# 3b. Corp CA bundle published to base-java's path as well.
ocx_step "publishing corp-ca-bundle descriptor to base-java path (match=*, required=true)"
ocx patch publish \
    --descriptor-file "${DESCRIPTORS}/global.json" \
    "$(id_of "patches/base-java")"

# 3c. Java-specific descriptor: only base-java gets java-truststore (in addition to CA).
ocx_step "publishing java-truststore descriptor to base-java path"
# NOTE: Two descriptors exist at base-java's path from steps 3b and 3c.
# OCX replaces the descriptor on publish — there is only ONE descriptor per path.
# To combine corp-ca + java-truststore for base-java we put both rules in one descriptor.
# The java-specific.json only has the truststore rule; we publish it separately
# (overwriting 3b) so the walkthrough shows "re-publish a companion" (see PATCHES.md).
# The PATCHES.md walkthrough uses `ocx patch test` to preview before publishing.
ocx patch publish \
    --descriptor-file "${DESCRIPTORS}/java-specific.json" \
    "$(id_of "patches/base-java")"

# 3d. Fail-open license descriptor: base-tool optionally gets license-server.
#     This ALSO overwrites the corp-ca descriptor at base-tool's path!
#     For the walkthrough, we'll use `ocx patch test` to preview the combined descriptor.
#     We create a combined descriptor inline for base-tool below.
ocx_step "creating combined descriptor for base-tool (corp-ca + license-server)"
COMBINED_TOOL_DESC="${DESCRIPTORS}/base-tool-combined.json"
cat >"${COMBINED_TOOL_DESC}" <<EOF
{
  "version": 1,
  "rules": [
    {
      "match": "*",
      "packages": ["localhost:5000/patches/corp-ca-bundle:1.0.0"],
      "required": true
    },
    {
      "match": "localhost:5000/patches/base-tool*",
      "packages": ["localhost:5000/patches/license-server:1.0.0"],
      "required": false
    }
  ]
}
EOF

ocx_step "publishing combined descriptor to base-tool path (corp-ca required + license fail-open)"
ocx patch publish \
    --descriptor-file "${COMBINED_TOOL_DESC}" \
    "$(id_of "patches/base-tool")"

# ── Step 4: Update the local index ───────────────────────────────────────────

ocx_step "updating local index"
ocx index update "patches/base-tool" >/dev/null
ocx index update "patches/base-java" >/dev/null
ocx index update "patches/corp-ca-bundle" >/dev/null
ocx index update "patches/java-truststore" >/dev/null
ocx index update "patches/license-server" >/dev/null

# ── Done ─────────────────────────────────────────────────────────────────────

echo
ocx_done "patches setup complete."
echo
echo "PATCHES_REGISTRY : ${PATCHES_REGISTRY}"
echo "OCX_HOME         : ${OCX_HOME}"
echo "config.toml      : ${CONFIG_FILE}"
echo
echo "Published packages:"
echo "  localhost:5000/patches/corp-ca-bundle:1.0.0   (companion: SSL_CERT_FILE)"
echo "  localhost:5000/patches/java-truststore:1.0.0  (companion: JAVA_TOOL_OPTIONS)"
echo "  localhost:5000/patches/license-server:1.0.0   (companion: LICENSE_SERVER, required=false)"
echo "  localhost:5000/patches/base-tool:1.0.0        (base: mytool binary)"
echo "  localhost:5000/patches/base-java:1.0.0        (base: java binary)"
echo
echo "Published descriptors:"
echo "  base-tool path : corp-ca-bundle (required=true) + license-server (required=false)"
echo "  base-java path : java-truststore (required=true; corp-ca-bundle NOT included — see PATCHES.md)"
echo
echo "Now try — CONSUMER perspective:"
echo
echo "  # Use-case 1 — Corp CA bundle (global match=*, required):"
echo "  ${OCX_BIN} package install patches/base-tool:${TAG}"
echo "  ${OCX_BIN} package exec patches/base-tool:${TAG} -- env | grep SSL_CERT_FILE"
echo "  ${OCX_BIN} package env patches/base-tool:${TAG} --show-patches"
echo
echo "  # Use-case 2 — JDK truststore (java-specific, required):"
echo "  ${OCX_BIN} package install patches/base-java:${TAG}"
echo "  ${OCX_BIN} package exec patches/base-java:${TAG} -- env | grep JAVA_TOOL_OPTIONS"
echo "  ${OCX_BIN} package env patches/base-java:${TAG} --show-patches"
echo
echo "  # Use-case 3 — License server (fail-open, required=false):"
echo "  ${OCX_BIN} package exec patches/base-tool:${TAG} -- env | grep LICENSE_SERVER"
echo "  ${OCX_BIN} package env patches/base-tool:${TAG} --show-patches"
echo
echo "  # Freeze companion digests for reproducible builds:"
echo "  ${OCX_BIN} --global patch freeze"
echo
echo "Read test/manual/PATCHES.md for the full maintainer + consumer walkthrough."
