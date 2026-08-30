#!/usr/bin/env bash
# Self-check for the (g2) gate's own decision logic in run_sequence.sh.
#
# (g2) cannot be exercised without a real publisher tag push and an owner-gated
# merge, so without this its verdict branches would ship with nothing able to
# fail on them — and a manual check performed once is not a test. This drives
# every branch against fixture roots and a stubbed `curl`.
#
# Covered: the verdict taxonomy's dispatch, the Docker-Content-Digest parse, the
# byte comparison, the two pre-network anchors, the boundary guards on the
# root's `tags[].content` and `repository`, the vacuity and parse refusals, and
# the binding-vs-object counting.
#
# NOT covered — and deliberately so: TLS, real registry auth, whether GHCR
# behaves as assumed, and whether the announce path writes a registry digest at
# all. `curl` is a stub on `PATH` for the duration; only the decision logic is
# under test, never the transport. The live proof is the `1.0.4` run.
#
# Usage: selfcheck_g2.sh    (no network, no credentials, no live state)

set -euo pipefail
IFS=$'\n\t'

TMP="$(mktemp -d)"

# The live-run variables env.sh demands. Nothing here reaches a network: the
# stub curl below is the only I/O, and sourcing run_sequence.sh never runs main.
export GH_REPO_PUBLISHER="stub/publisher" GH_REPO_INDEX="stub/index" \
    INDEX_FORK="stub/fork" E2E_NAMESPACE="ns" E2E_PACKAGE="pkg" \
    BOT_ACTOR_IDS="1" RESULTS_DIR="$TMP/results" INDEX_SITE="https://stub.invalid"

# Only the flags run_sequence.sh actually passes. The URL is whatever is left
# over, so `--` and the bundled `-fsS` need no special case beyond being flags.
mkdir -p "$TMP/bin"
cat >"$TMP/bin/curl" <<'STUB'
#!/usr/bin/env bash
set -uo pipefail
out="" dump="" url="" want_code=""
while (($#)); do
    case "$1" in
        -o) out="$2" && shift 2 ;;
        -D) dump="$2" && shift 2 ;;
        -H | -w) want_code=1 && shift 2 ;;
        --) shift ;;
        -*) shift ;;
        *) url="$1" && shift ;;
    esac
done
hex="${url##*/}"
hex="${hex%.json}"
hex="${hex#sha256:}"
case "$url" in
    */token\?*)
        printf '{"token": "stub"}\n'
        ;;
    */o/sha256/*)
        [[ -f "$STUB_DIR/cas/$hex" ]] || exit 22
        cp -- "$STUB_DIR/cas/$hex" "$out"
        ;;
    */manifests/*)
        status=200
        [[ -f "$STUB_DIR/status/$hex" ]] && status="$(<"$STUB_DIR/status/$hex")"
        dcd="sha256:$hex"
        [[ -f "$STUB_DIR/dcd/$hex" ]] && dcd="$(<"$STUB_DIR/dcd/$hex")"
        : >"$out"
        [[ -f "$STUB_DIR/reg/$hex" ]] && cp -- "$STUB_DIR/reg/$hex" "$out"
        {
            printf 'HTTP/1.1 %s stub\r\n' "$status"
            printf 'Content-Type: application/vnd.oci.image.index.v1+json\r\n'
            [[ -n $dcd ]] && printf 'Docker-Content-Digest: %s\r\n' "$dcd"
            printf '\r\n'
        } >"$dump"
        [[ -n $want_code ]] && printf '%s\n' "$status"
        ;;
    *)
        printf 'stub curl: unexpected url %s\n' "$url" >&2
        exit 1
        ;;
esac
exit 0
STUB
chmod +x "$TMP/bin/curl"
PATH="$TMP/bin:$PATH"
export PATH

# shellcheck source=./run_sequence.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/run_sequence.sh"

# run_sequence.sh installs its own EXIT trap for WORK_DIR; replace it with one
# that clears both, since sourcing means only one trap survives.
trap 'rm -rf -- "$TMP" "$WORK_DIR"' EXIT

CASES=0
FAILURES=0
STUB_DIR=""
export STUB_DIR

# A fresh stub state. Defaults are the passing case: status 200 and a
# Docker-Content-Digest echoing the requested digest.
new_case() {
    CASES=$((CASES + 1))
    STUB_DIR="$TMP/case-$CASES"
    mkdir -p "$STUB_DIR/cas" "$STUB_DIR/reg" "$STUB_DIR/status" "$STUB_DIR/dcd"
}

# Register one object, served both as the committed CAS entry and as the
# registry's manifest body. Args: <content-bytes>. Echoes sha256:<hex>.
serve_object() {
    local hex
    hex="$(printf '%s' "$1" | sha256sum | cut -d' ' -f1)"
    printf '%s' "$1" >"$STUB_DIR/cas/$hex"
    printf '%s' "$1" >"$STUB_DIR/reg/$hex"
    printf 'sha256:%s\n' "$hex"
}

# A served-root fixture. Args: <repository-url> <tag>=<content-digest>...
root_json() {
    local repo="$1" pair
    local -a members=()
    shift
    for pair in "$@"; do
        members+=("\"${pair%%=*}\": {\"content\": \"${pair#*=}\"}")
    done
    local IFS=,
    printf '{"repository": "%s", "tags": {%s}}' "$repo" "${members[*]}"
}

report() {
    if [[ $1 == pass ]]; then
        printf 'ok   %s\n' "$2"
    else
        FAILURES=$((FAILURES + 1))
        printf 'FAIL %s\n     %s\n' "$2" "${3//$'\n'/$'\n'     }"
    fi
}

# Args: <description> <substring the success message must carry> <command...>
expect_ok() {
    local what="$1" pat="$2" out
    shift 2
    if ! out="$("$@" 2>&1)"; then
        report fail "$what" "expected success, got: $out"
    elif [[ $out == *"$pat"* ]]; then
        report pass "$what"
    else
        report fail "$what" "expected '$pat' in: $out"
    fi
}

# Args: <description> <substring the failure message must carry> <command...>
expect_fail() {
    local what="$1" pat="$2" out
    shift 2
    if out="$("$@" 2>&1)"; then
        report fail "$what" "expected failure, got: $out"
    elif [[ $out == *"$pat"* ]]; then
        report pass "$what"
    else
        report fail "$what" "expected '$pat' in: $out"
    fi
}

REPO="oci://registry.stub/ns/pkg"

main_selfcheck() {
    local d e root

    # --- E1, the taxonomy. One row per verdict. ---

    new_case
    d="$(serve_object '{"index": 1}')"
    expect_ok "row 4 — proof" "1 tag→object bindings, over 1 distinct" \
        assert_bytes_identical "$(root_json "$REPO" "1.0.4=$d")"

    new_case
    d="$(serve_object '{"index": 1}')"
    rm -- "$STUB_DIR/reg/${d#sha256:}"
    printf '404\n' >"$STUB_DIR/status/${d#sha256:}"
    expect_fail "row 1 — 404 is the bug class" "THE BUG CLASS" \
        assert_bytes_identical "$(root_json "$REPO" "1.0.4=$d")"

    new_case
    d="$(serve_object '{"index": 1}')"
    printf '429\n' >"$STUB_DIR/status/${d#sha256:}"
    expect_fail "transport — 429 is not a taxonomy row" "the paragraph below the taxonomy table" \
        assert_bytes_identical "$(root_json "$REPO" "1.0.4=$d")"

    new_case
    d="$(serve_object '{"index": 1}')"
    printf 'sha256:%064d\n' 0 >"$STUB_DIR/dcd/${d#sha256:}"
    expect_fail "row 2 — content negotiation" "REGISTRY-BEHAVIOUR" \
        assert_bytes_identical "$(root_json "$REPO" "1.0.4=$d")"

    new_case
    d="$(serve_object '{"index": 1}')"
    : >"$STUB_DIR/dcd/${d#sha256:}"
    expect_fail "row 2 — absent digest header" "no Docker-Content-Digest header" \
        assert_bytes_identical "$(root_json "$REPO" "1.0.4=$d")"

    new_case
    d="$(serve_object '{"index": 1}')"
    printf '{"index": 2}' >"$STUB_DIR/reg/${d#sha256:}"
    expect_fail "row 3 — registry integrity" "REGISTRY INTEGRITY FAILURE" \
        assert_bytes_identical "$(root_json "$REPO" "1.0.4=$d")"

    # --- The two anchors that fire before the registry is contacted. ---

    new_case
    d="$(serve_object '{"index": 1}')"
    rm -- "$STUB_DIR/cas/${d#sha256:}"
    expect_fail "anchor — no committed object" "nothing is committed at" \
        assert_bytes_identical "$(root_json "$REPO" "1.0.4=$d")"

    new_case
    d="$(serve_object '{"index": 1}')"
    printf '{"index": 9}' >"$STUB_DIR/cas/${d#sha256:}"
    expect_fail "anchor — CAS disagrees with its filename" "does not hash to its own filename" \
        assert_bytes_identical "$(root_json "$REPO" "1.0.4=$d")"

    # --- Boundary guards on the publisher-controlled root. ---

    new_case
    expect_fail "boundary — malformed content digest" "malformed content digest" \
        assert_bytes_identical "$(root_json "$REPO" '1.0.4=sha256:nothex')"

    new_case
    d="$(serve_object '{"index": 1}')"
    expect_fail "boundary — query string in the repository path" "not an OCI repository name" \
        assert_bytes_identical "$(root_json 'oci://registry.stub/ns/pkg?x=1' "1.0.4=$d")"

    new_case
    d="$(serve_object '{"index": 1}')"
    expect_fail "boundary — illegal character in the repository host" "is not a host" \
        assert_bytes_identical "$(root_json 'oci://registry!stub/ns/pkg' "1.0.4=$d")"

    new_case
    d="$(serve_object '{"index": 1}')"
    expect_fail "boundary — repository is not an oci:// URL" "does not name a usable physical repository" \
        assert_bytes_identical "$(root_json 'https://registry.stub/ns/pkg' "1.0.4=$d")"

    new_case
    expect_fail "refusal — a root with no tags" "would pass vacuously" \
        assert_bytes_identical "$(root_json "$REPO")"

    new_case
    expect_fail "refusal — a root that does not parse" "failed to parse" \
        assert_bytes_identical '{"repository": "'"$REPO"'"}'

    # --- Cascade: bindings and distinct objects are counted separately. ---

    new_case
    d="$(serve_object '{"index": 1}')"
    e="$(serve_object '{"index": 2}')"
    expect_ok "cascade — 3 bindings over 2 objects" "3 tag→object bindings, over 2 distinct" \
        assert_bytes_identical "$(root_json "$REPO" "1.0.4=$d" "1.0=$d" "latest=$e")"

    # --- E2a, D7. ---

    new_case
    d="$(serve_object '{"index": 1}')"
    expect_ok "D7 — a clean root" "over 2 tags" \
        assert_no_reserved_tags "$(root_json "$REPO" "1.0.4=$d" "latest=$d")"

    new_case
    d="$(serve_object '{"index": 1}')"
    expect_fail "D7 — __ocx.desc, upper-cased" "reserved internal tag" \
        assert_no_reserved_tags "$(root_json "$REPO" "1.0.4=$d" "__OCX.DESC=$d")"

    new_case
    d="$(serve_object '{"index": 1}')"
    root="$(root_json "$REPO" "1.0.4=$d" "sha256.$(printf '%064d' 0)=$d")"
    expect_fail "D7 — legacy keep tag" "legacy keep tag" \
        assert_no_reserved_tags "$root"

    new_case
    expect_fail "D7 — never attested against an unparseable root" "failed to parse" \
        assert_no_reserved_tags '{"repository": "'"$REPO"'"}'

    new_case
    expect_fail "D7 — never attested vacuously" "attest D7 vacuously" \
        assert_no_reserved_tags "$(root_json "$REPO")"

    printf '\n%d cases, %d failures\n' "$CASES" "$FAILURES"
    ((FAILURES == 0))
}

main_selfcheck
