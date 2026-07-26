#!/usr/bin/env bash
# Requirement (1): the sequenced end-to-end scenario — claimed-namespace
# prerequisite check, publisher tag push, CI build, dev-ocx push + announce,
# fork PR on the real index, validate.yml green, lane classification, merge,
# render-deploy, index.ocx.sh serves the root, the committed objects are the
# registry's own bytes, clean-machine install.
#
# Usage: run_sequence.sh <tag>
#
# The announce itself runs inside the publisher's CI, which holds the
# OCX_ANNOUNCE_TOKEN repo secret; this driver pushes the tag and then observes.
#
# Consumes from env.sh: GH_REPO_PUBLISHER, GH_REPO_INDEX, INDEX_FORK,
# E2E_NAMESPACE, E2E_PACKAGE, BOT_ACTOR_IDS.

set -euo pipefail
IFS=$'\n\t'

# shellcheck source=./env.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/env.sh"

# Scratch for (g2). Both payloads go to disk so `cmp` compares bytes rather
# than a shell-mangled string: a command substitution strips trailing newlines
# and mangles any NUL, which is exactly the difference a format regression
# would show up as.
WORK_DIR="$(mktemp -d)"
cleanup() { rm -rf -- "$WORK_DIR"; }
trap cleanup EXIT

# Both OCI manifest media types. Requesting by digest constrains the content to
# exactly that digest whatever the Accept says; the header only decides whether
# the registry answers at all, and a 406 from a too-narrow Accept would read as
# a mapping defect it is not.
MANIFEST_ACCEPT='Accept: application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json'

# The physical repository (g2) checks against, and its anonymous pull token —
# read once per run from the served root, then read per tag by
# _assert_tag_identical. Assigned by assert_bytes_identical; see both below.
E2E_REG_HOST=""
E2E_REG_PATH=""
E2E_REG_TOKEN=""

# What (g2) proved, for the evidence notes. --cascade aliases several tags onto
# one image index, so the two numbers differ and only the second says how much
# of `o/` was actually read — a binding count reported as an object count
# overstates the gate's reach by exactly the cascade factor.
E2E_BINDINGS_VERIFIED=0
E2E_OBJECTS_VERIFIED=0

# (a0) Ruling R3: announce refuses an unclaimed namespace and never creates a
# root itself. The claim is a manual claim-a-namespace PR the operator opens
# and the human lane (G-04) merges. This step waits on you; it does not fail
# fast.
require_claimed_namespace() {
    ocx_step "(a0) checking that $E2E_NAMESPACE/$E2E_PACKAGE is claimed on $GH_REPO_INDEX"
    if ! _namespace_claimed; then
        ocx_warn "root p/$E2E_NAMESPACE/$E2E_PACKAGE.json is not on $GH_REPO_INDEX main yet."
        ocx_warn "Open a claim-a-namespace PR committing that root with your numeric"
        ocx_warn "github_id in owners[], and get it merged through the human lane (G-04)."
        ocx_warn "This step waits on you — polling for up to ${CLAIM_DEADLINE_SECONDS}s."
        poll_until "$CLAIM_DEADLINE_SECONDS" "the namespace claim to merge" _namespace_claimed ||
            ocx_fail "namespace still unclaimed; announce would hard-error"
    fi
    ocx_done "(a0) namespace claimed"
}

_namespace_claimed() {
    local owners
    owners="$(gh api "repos/$GH_REPO_INDEX/contents/p/$E2E_NAMESPACE/$E2E_PACKAGE.json?ref=main" \
        --header 'Accept: application/vnd.github.raw' \
        --jq '[.owners[]?.github_id] | length' 2>/dev/null)" || return 1
    [[ -n $owners ]] && ((owners > 0))
}

# (a) Tag and push the publisher. Echoes the tagged commit sha.
push_publisher_tag() {
    local tag="$1" checkout sha
    checkout="$(publisher_checkout)"
    ocx_step "(a) tagging $tag in $checkout and pushing to $GH_REPO_PUBLISHER" >&2
    git -C "$checkout" fetch origin >&2
    git -C "$checkout" tag "$tag" origin/main >&2 ||
        ocx_fail "tag $tag already exists — pick a new one, or retract it per README 'Cleaning up a rehearsal'"
    git -C "$checkout" push origin "refs/tags/$tag" >&2
    sha="$(git -C "$checkout" rev-parse "refs/tags/$tag^{commit}")"
    printf '%s\n' "$sha"
}

# (e) The lane outcome is read off live state, not assumed: a tag refresh
# against an already-claimed namespace auto-merges via G-19 when the
# announcer's github_id is already in the root's owners[], and routes to the
# human lane otherwise. Both outcomes are legitimate here.
print_governance_checklist() {
    ocx_step "(e) governance lane — check both outcomes on PR #$1"
    cat >&2 <<EOF
  [ ] Open https://github.com/$GH_REPO_INDEX/pull/$1 and read the applied labels.
  [ ] Machine lane (G-19): the PR auto-merges, no human click. Expected when the
      announcer's numeric github_id is already in the root's owners[] AND the
      diff stays inside the touched roots plus their own CAS objects.
  [ ] Human lane: the PR waits for review. Expected for a first claim, an
      owners[] change, or any diff that leaves the machine-lane path allowlist.
  [ ] Neither matches what you expected? PLAYBOOKS.md playbook 3.
EOF
}

# (g) Both index surfaces must serve, and the root must name THIS run's tag.
# Grepping for `sha256:` instead would pass against a root a previous rehearsal
# populated, so a no-op render would still report a served render.
assert_served() {
    local tag="$1"
    local root="$INDEX_SITE/p/$E2E_NAMESPACE/$E2E_PACKAGE.json"
    curl -fsS "$root" | grep -q "\"$tag\"" ||
        ocx_fail "$root served, but does not name tag $tag — the render did not pick up this run"
    curl -fsS "$INDEX_SITE/c/index.json" >/dev/null ||
        ocx_fail "$INDEX_SITE/c/index.json did not serve"
    ocx_done "(g) $INDEX_SITE serves the root carrying $tag, and the catalog"
}

# The tag names a served root commits, one per line. Never `print(*keys)`: an
# empty tags object prints one empty line that way, and an empty line reads
# downstream as a tag named "".
_root_tag_names() {
    printf '%s' "$1" |
        python3 -c 'import json,sys; sys.stdout.writelines(t + "\n" for t in json.load(sys.stdin)["tags"])'
}

# The content digest a served root records for one tag. Args: <root-json> <tag>.
_root_tag_content() {
    printf '%s' "$1" |
        python3 -c 'import json,sys; print(json.load(sys.stdin)["tags"][sys.argv[1]]["content"])' "$2"
}

# The physical repository a served root points at, as "<host> <path>". Read,
# never assumed: the ADR's Validation bullet names the root's own `repository`
# as the registry to check against, and `<ns>/<pkg>` is only a convention the
# publisher is free not to follow.
_root_repository() {
    printf '%s' "$1" |
        python3 -c 'import json,sys; u=json.load(sys.stdin)["repository"]; h,_,p = u.removeprefix("oci://").partition("/"); print(h, p) if u.startswith("oci://") and h and p else sys.exit("root.repository is not an oci://<host>/<path> URL: " + u)'
}

# An anonymous pull token from the registry's own token endpoint. Args:
# <host> <repo-path>. ponytail: ghcr.io's convention, which registry:2 also
# implements — a registry that delegates auth to a different host advertises it
# in WWW-Authenticate, and parsing that challenge is the upgrade path if a pilot
# package ever moves off ghcr.
_pull_token() {
    curl -fsS -- "https://$1/token?service=$1&scope=repository:$2:pull" |
        python3 -c 'import json,sys; print(json.load(sys.stdin)["token"])'
}

# The digest a registry says it served, out of a dumped header block. Args:
# <header-file>. Header names are case-insensitive per RFC 9110 and the line
# endings are CRLF, so neither is assumed. Empty when the header is absent,
# which _assert_tag_identical reports rather than treating as a match.
_content_digest_header() {
    tr -d '\r' <"$1" | awk 'tolower($1) == "docker-content-digest:" { print $2 }'
}

# One tag's byte identity, with every outcome kept distinguishable — a
# transport artefact must not read as a mapping defect. Args:
# <tag> <content-digest>. Reads E2E_REG_HOST / E2E_REG_PATH / E2E_REG_TOKEN.
_assert_tag_identical() {
    local tag="$1" content="$2" hex="${2#sha256:}"
    local cas="$WORK_DIR/cas" reg="$WORK_DIR/reg" hdr="$WORK_DIR/hdr" status served

    curl -fsS -o "$cas" -- "$INDEX_SITE/p/$E2E_NAMESPACE/$E2E_PACKAGE/o/sha256/$hex.json" ||
        ocx_fail "tag $tag records $content but nothing is committed at o/sha256/$hex.json — D2/D3 says every tags[].content has an object"

    # Anchor 1: the committed object hashes to its own filename. A CAS that
    # disagrees with itself makes every later comparison meaningless.
    [[ "$(sha256sum <"$cas" | cut -d' ' -f1)" == "$hex" ]] ||
        ocx_fail "committed o/sha256/$hex.json does not hash to its own filename"

    # Anchor 2: the registry the root itself points at. Requested BY DIGEST, so
    # Docker-Content-Digest separates a content-negotiating registry from a real
    # mapping defect. Deliberately no -f: the HTTP status is the diagnosis.
    status="$(curl -sS -o "$reg" -D "$hdr" -w '%{http_code}' \
        -H "Authorization: Bearer $E2E_REG_TOKEN" \
        -H "$MANIFEST_ACCEPT" \
        -- "https://$E2E_REG_HOST/v2/$E2E_REG_PATH/manifests/$content")" ||
        ocx_fail "the request for $content against $E2E_REG_HOST never completed — a transport failure, not a mapping defect"

    case "$status" in
        200) ;;
        404)
            ocx_fail "$E2E_REG_HOST does not serve $content for tag $tag (404) — THE BUG CLASS: the index committed a digest no registry can serve. PLAYBOOKS.md 'Registry Byte Identity', row 1"
            ;;
        *)
            ocx_fail "$E2E_REG_HOST answered $status for $content — a transport or auth failure, not a mapping defect. PLAYBOOKS.md 'Registry Byte Identity', the paragraph below the taxonomy table"
            ;;
    esac

    served="$(_content_digest_header "$hdr")"
    [[ $served == "$content" ]] ||
        ocx_fail "$E2E_REG_HOST served ${served:-<no Docker-Content-Digest header>} for a request for $content — the registry content-negotiated. A REGISTRY-BEHAVIOUR finding, not a format regression. PLAYBOOKS.md 'Registry Byte Identity', row 2"

    # Reachable only when the registry lies about its own content: anchor 1
    # established sha256(committed) == the requested digest, and the header
    # check just established the registry claims that same digest. A
    # re-serialization in the announce path cannot land here — it commits an
    # object that hashes to its own filename and 404s on the registry, which is
    # row 1.
    cmp -s "$cas" "$reg" ||
        ocx_fail "the object committed for tag $tag differs from the bytes $E2E_REG_HOST serves under $content — A REGISTRY INTEGRITY FAILURE: the registry advertised $content and served bytes that do not hash to it. PLAYBOOKS.md 'Registry Byte Identity', row 3"
}

# (g2) E1, the acceptance gate. ADR "Validation" bullet 2: sha256(bytes) ==
# <hex> AND a GET by that digest against the root's own `repository` returns
# byte-identical content — the platform mapping is a copy of the registry's
# bytes, not an assertion about them. Pre-change, tags[].content was a digest
# the announcer minted, so this GET 404s; that 404 is the reproduction.
#
# Every tag in the served root, not only this run's: --cascade aliases
# 1.0.4 / 1.0 / 1 / latest onto one index, so a per-tag divergence is exactly
# what a single-tag check would let through.
assert_bytes_identical() {
    local root_json="$1" repository names tag content
    local -a tags=()
    local -A content_of=()

    # A command substitution, not `done < <(...)`: a process substitution's exit
    # status is unobservable, so an unparseable root would run zero iterations
    # and reach the vacuity refusal below wearing the wrong diagnosis.
    names="$(_root_tag_names "$root_json")" ||
        ocx_fail "could not read the served root's tags — (g2) refuses to judge a root it failed to parse"

    # A root with no tags satisfies every assertion below by never running one.
    # The gate says so rather than reporting a pass.
    [[ -n $names ]] ||
        ocx_fail "the served root commits no tags — (g2) would pass vacuously"
    mapfile -t tags <<<"$names"

    # Read and validate the whole root before touching the network, so a
    # malformed root fails as a malformed root rather than as a registry error
    # one round-trip later.
    for tag in "${tags[@]}"; do
        content="$(_root_tag_content "$root_json" "$tag")"
        # The digest lands in a URL. Validated here at the boundary, not
        # trusted because a schema somewhere else says it is well-formed.
        if [[ ! $content =~ ^sha256:[0-9a-f]{64}$ ]]; then
            ocx_fail "the root records a malformed content digest for tag $tag: $content"
        fi
        content_of["$tag"]="$content"
    done

    repository="$(_root_repository "$root_json")" ||
        ocx_fail "the served root does not name a usable physical repository"
    # IFS is $'\n\t' script-wide, so the space is NOT a separator unless said
    # here — same idiom as env.sh's `IFS='|' read`. Without it the host would
    # swallow the path and every URL below would be malformed.
    IFS=' ' read -r E2E_REG_HOST E2E_REG_PATH <<<"$repository"

    # Both halves land in every registry URL below, and the root is
    # publisher-controlled — same boundary the content digest is validated at.
    # A `?` or `#` in the path would silently truncate the request into one that
    # never asked for a manifest, and its 404 would read as the bug class; a
    # host nobody constrained lets an attacker-named registry serve the
    # committed bytes back and turn the gate green.
    [[ $E2E_REG_HOST =~ ^[a-zA-Z0-9.-]+(:[0-9]+)?$ ]] ||
        ocx_fail "the served root names $E2E_REG_HOST, which is not a host[:port] — refusing to build a registry URL from it"
    [[ $E2E_REG_PATH =~ ^[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*(/[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*)*$ ]] ||
        ocx_fail "the served root names $E2E_REG_PATH, which is not an OCI repository name — refusing to build a registry URL from it"

    E2E_REG_TOKEN="$(_pull_token "$E2E_REG_HOST" "$E2E_REG_PATH")" ||
        ocx_fail "could not obtain an anonymous pull token for $E2E_REG_PATH on $E2E_REG_HOST — a credential problem, not a mapping defect"

    for tag in "${!content_of[@]}"; do
        _assert_tag_identical "$tag" "${content_of[$tag]}"
    done

    E2E_BINDINGS_VERIFIED="${#content_of[@]}"
    E2E_OBJECTS_VERIFIED="$(printf '%s\n' "${content_of[@]}" | sort -u | wc -l)"
    ocx_done "(g2) all $E2E_BINDINGS_VERIFIED tag→object bindings, over $E2E_OBJECTS_VERIFIED distinct o/ objects, are byte-identical to what $E2E_REG_HOST serves under the same digest"
}

# (g2) E2a, D7: no reserved tag class reaches the index. Free — the pilot
# repository genuinely carries both classes, since push_canonical_tag is
# default-on and the publisher's `Describe package` step writes __ocx.desc.
#
# This does NOT prove the filter fired. The publisher announces via
# --announce-file, which under D7 never carries a canonical tag, so a clean
# root is consistent with no filter at all. E2a is a regression tripwire; E2b
# and the local acceptance cases are the proof. README.md says the same.
assert_no_reserved_tags() {
    local root_json="$1" names tag
    local -a tags=()

    # A command substitution, not `done < <(...)`: a process substitution's exit
    # status is unobservable, so an unparseable root would run zero iterations
    # and attest D7 against a root this never read.
    names="$(_root_tag_names "$root_json")" ||
        ocx_fail "could not read the served root's tags — (g2) cannot attest D7 against a root it failed to parse"
    [[ -n $names ]] ||
        ocx_fail "the served root commits no tags — (g2) would attest D7 vacuously"
    mapfile -t tags <<<"$names"

    for tag in "${tags[@]}"; do
        if [[ ${tag,,} == __ocx* ]]; then
            ocx_fail "the served root carries reserved internal tag $tag — D7: announce drops __ocx*, case-insensitively"
        fi
        if [[ $tag =~ ^sha256\.[0-9a-f]{64}$ ]]; then
            ocx_fail "the served root carries canonical digest-alias tag $tag — D7: announce drops sha256.<hex>"
        fi
    done
    ocx_done "(g2) no reserved tag class reached the root over ${#tags[@]} tags (D7)"
}

main() {
    local tag="${1:?usage: run_sequence.sh <tag>}"
    local sha t0 publisher_run pr merge_line merge_sha merged_at deploy_run latency
    local publish_floor pr_base deploy_floor
    local root_json
    local secret_args=()

    require_claimed_namespace

    # Freshness floors, all read before the push: what GitHub had already
    # numbered, so nothing an earlier rehearsal left behind can answer for this
    # run. t0 stays a timestamp — it is only ever reported as latency.
    t0="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    publish_floor="$(run_floor "$GH_REPO_PUBLISHER" e2e-publish "$tag")"
    deploy_floor="$(run_floor "$GH_REPO_INDEX" render-deploy main)"
    pr_base="$(pr_floor)"
    sha="$(push_publisher_tag "$tag")"

    ocx_step "(b) waiting for the publisher's build + push + announce at ${sha:0:7}"
    publisher_run="$(poll_run "$GH_REPO_PUBLISHER" e2e-publish "$tag" "$sha" "$publish_floor")" ||
        ocx_fail "publisher CI did not conclude successfully — see PLAYBOOKS.md"

    ocx_step "(c) waiting for the tag-refresh PR on $ANNOUNCE_BRANCH"
    pr="$(poll_pr "$pr_base")" || ocx_fail "no pull request appeared on $ANNOUNCE_BRANCH"
    ocx_done "(c) pull request #$pr"

    ocx_step "(d) waiting for validate.yml on PR #$pr"
    poll_check "$pr" ||
        ocx_fail "validate.yml is not green on PR #$pr — see PLAYBOOKS.md playbook 2"

    print_governance_checklist "$pr"

    ocx_step "(f) waiting for PR #$pr to merge, then for render-deploy"
    merge_line="$(poll_merge "$pr")" || ocx_fail "PR #$pr did not merge"
    merge_sha="${merge_line%% *}"
    merged_at="${merge_line##* }"
    deploy_run="$(poll_run "$GH_REPO_INDEX" render-deploy main "$merge_sha" "$deploy_floor")" ||
        ocx_fail "render-deploy did not conclude successfully at ${merge_sha:0:7}"

    ocx_step "(g) checking that $INDEX_SITE serves the root"
    assert_served "$tag"

    ocx_step "(g2) proving the committed objects are the registry's own bytes"
    root_json="$(curl -fsS -- "$INDEX_SITE/p/$E2E_NAMESPACE/$E2E_PACKAGE.json")" ||
        ocx_fail "could not read the served root for (g2)"
    assert_no_reserved_tags "$root_json"
    assert_bytes_identical "$root_json"

    ocx_step "(h) clean-machine install"
    "$E2E_SCRIPT_DIR/clean_install_check.sh" ||
        ocx_fail "clean-machine install did not resolve $E2E_NAMESPACE/$E2E_PACKAGE"

    latency="$(evidence latency --start "$t0" --end "$merged_at")"
    mapfile -t secret_args < <(evidence_secret_args)
    evidence record --scenario sequenced --status pass \
        --pr-url "$(gh pr view "$pr" --repo "$GH_REPO_INDEX" --json url --jq '.url')" \
        --run-urls "$publisher_run,$deploy_run,$INDEX_SITE/p/$E2E_NAMESPACE/$E2E_PACKAGE.json" \
        --latency "$latency" \
        --notes "tag $tag; PR #$pr merged at $merged_at; tag-push to merge ${latency}s; (g2) $E2E_BINDINGS_VERIFIED tag→object bindings over $E2E_OBJECTS_VERIFIED distinct o/ objects, byte-identical to $E2E_REG_HOST/$E2E_REG_PATH" \
        "${secret_args[@]}" \
        --out "$RESULTS_DIR/${RUN_ID}-sequenced.record.json"

    ocx_done "sequenced scenario passed (PR #$pr, ${latency}s tag-push to merge)"
}

# Guarded so `selfcheck_g2.sh` can source this file and drive every (g2) verdict
# against fixture roots, and so an operator can call assert_bytes_identical by
# hand against an unmerged pull request's root (README, the E1-pre run).
# Sourcing must never push a tag.
if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
    main "$@"
fi
