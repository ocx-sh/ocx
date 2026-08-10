#!/usr/bin/env bash
# Vendor the ocx-sh/index conformance fixtures into
# crates/ocx_lib/tests/fixtures/index_wire/. Prints the resulting working-tree
# changes and NEVER commits — review and commit the result yourself.
#
#   (no args)     re-vendor at the committed SOURCE_COMMIT pin, so a bare re-run
#                 verifies and never silently fast-forwards
#   --ref <ref>   vendor at <ref> (branch, tag or SHA) and re-pin SOURCE_COMMIT
#                 to the commit it resolves to
#   --check       drift gate: compare ocx-sh/index@main against the vendored
#                 tree without writing to it; any difference exits non-zero, as
#                 does an upstream fixture family that no leaf claims
#
# One source root (bot/tests/golden) with explicit leaves, so the sync and the
# drift gate compare trees of the same shape. The source is a local ocx-sh/index
# checkout ($INDEX_REPO_PATH, else ../index) when it carries the requested ref,
# otherwise the GitHub contents API. --check always reads GitHub: a stale local
# mirror cannot answer "did upstream move?".
set -euo pipefail
IFS=$'\n\t'

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "${script_dir}/../.." && pwd)"
dest_rel="crates/ocx_lib/tests/fixtures/index_wire"
dest="${repo_root}/${dest_rel}"
commit_file="${dest}/SOURCE_COMMIT"
src_rel="bot/tests/golden"

# Vendored leaves, "<path under ${src_rel}>|<path under ${dest}>". Directory
# leaves are mirrored whole and are flat upstream. The one rename is
# `serializer/root` → `root`: ocx keeps the root vectors at the top of its
# fixture tree and `index_wire_conformance.rs` reads them there.
dir_leaves=(
    "serializer/root|root"
    "dispatch/sha256|dispatch/sha256"
)
file_leaves=(
    "tag_verdicts.json|tag_verdicts.json"
    "dispatch/expected_platforms.json|dispatch/expected_platforms.json"
    "render/normal/expected/dist/c/index.json|catalog/normal.json"
    "render/normal/expected/dist/config.json|config/normal.json"
)

# Paths under ${src_rel} that exist upstream and are deliberately NOT vendored.
# Every upstream path must match a leaf above or an entry here — see
# assert_every_upstream_path_is_claimed. Claiming is OR'd there, so "render"
# still covers the two `render/normal` files the catalog/config leaves vendor
# above; the overlap is deliberate, and narrowing it has no syntax in this list.
unvendored=(
    "render"               # site-render goldens: no ocx counterpart, bot-only
    "serializer/README.md" # upstream provenance notes, read at the pinned commit
    "dispatch/README.md"
)

# Names under ${dest} that ocx authors itself. They share the fixture root with
# the vendored leaves but have no upstream counterpart, so `diff -r` would report
# every one of them as drift forever. Declaring them here is what keeps the gate
# answering "did upstream move?" rather than "did ocx add a fixture?" — a new
# entry is a deliberate statement that these bytes are ours.
#
# These become `diff -x` patterns, which match a BASENAME at any depth, not only
# at the top level. So an upstream file named exactly `README.md`, `SOURCE_COMMIT`
# or `cpython` *inside* a vendored leaf would be skipped here too. Keep the names
# specific enough that this stays theoretical, and never add a generic one.
ocx_authored=(
    "README.md"     # vendoring provenance, written by hand
    "SOURCE_COMMIT" # the pin this script maintains
    "cpython"       # escaping truth table generated against CPython's json
)

die() {
    echo "sync_index_conformance: $*" >&2
    exit 1
}

# Extract ${src_rel} at commit $2 from the local checkout $1 into staging dir $3.
stage_from_local() {
    local index_repo="$1" commit="$2" stage_dir="$3"
    git -C "${index_repo}" archive --format=tar "${commit}" -- "${src_rel}" | tar -x -C "${stage_dir}" ||
        die "cannot extract ${src_rel} at ${commit} from ${index_repo}"
}

# Download every leaf at commit $1 from GitHub into staging dir $2.
stage_from_github() {
    local commit="$1" stage_dir="$2" leaf src listing name url
    for leaf in "${dir_leaves[@]}"; do
        src="${leaf%%|*}"
        mkdir -p -- "${stage_dir}/${src_rel}/${src}"
        listing="$(gh api "repos/ocx-sh/index/contents/${src_rel}/${src}?ref=${commit}" \
            --jq '.[] | [.name, .download_url] | @tsv')" ||
            die "cannot list ${src_rel}/${src} on ocx-sh/index at ${commit}"
        while IFS=$'\t' read -r name url; do
            [[ -n ${name} ]] || continue
            curl -fsSL -o "${stage_dir}/${src_rel}/${src}/${name}" -- "${url}" ||
                die "failed to download ${src}/${name} from ${url}"
        done <<<"${listing}"
    done
    for leaf in "${file_leaves[@]}"; do
        src="${leaf%%|*}"
        mkdir -p -- "$(dirname -- "${stage_dir}/${src_rel}/${src}")"
        url="$(gh api "repos/ocx-sh/index/contents/${src_rel}/${src}?ref=${commit}" --jq '.download_url')" ||
            die "cannot read ${src_rel}/${src} on ocx-sh/index at ${commit}"
        curl -fsSL -o "${stage_dir}/${src_rel}/${src}" -- "${url}" ||
            die "failed to download ${src} from ${url}"
    done
}

# Stage ${src_rel} at ref $1 into $2; print the resolved commit SHA on stdout.
stage() {
    local ref="$1" stage_dir="$2" index_repo commit
    index_repo="${INDEX_REPO_PATH:-${repo_root}/../index}"
    if [[ -d ${index_repo}/.git ]] &&
        commit="$(git -C "${index_repo}" rev-parse --verify --quiet "${ref}^{commit}")"; then
        echo "local checkout ${index_repo} @ ${ref} (${commit})" >&2
        stage_from_local "${index_repo}" "${commit}" "${stage_dir}"
    else
        command -v gh >/dev/null 2>&1 ||
            die "no local ocx-sh/index checkout carries '${ref}'; set INDEX_REPO_PATH or install the gh CLI"
        commit="$(gh api "repos/ocx-sh/index/commits/${ref}" --jq '.sha')" ||
            die "cannot resolve '${ref}' on ocx-sh/index"
        echo "GitHub ocx-sh/index @ ${ref} (${commit})" >&2
        stage_from_github "${commit}" "${stage_dir}"
    fi
    printf '%s\n' "${commit}"
}

# Copy every leaf from staged source root $1 into destination tree $2.
place_leaves() {
    local source_root="$1" target="$2" leaf src dst
    for leaf in "${dir_leaves[@]}"; do
        src="${leaf%%|*}"
        dst="${leaf##*|}"
        [[ -d ${source_root}/${src} ]] || die "index source is missing ${src}/"
        rm -rf -- "${target:?}/${dst}"
        mkdir -p -- "$(dirname -- "${target}/${dst}")"
        cp -R -- "${source_root}/${src}" "${target}/${dst}"
    done
    for leaf in "${file_leaves[@]}"; do
        src="${leaf%%|*}"
        dst="${leaf##*|}"
        [[ -f ${source_root}/${src} ]] || die "index source is missing ${src}"
        mkdir -p -- "$(dirname -- "${target}/${dst}")"
        cp -- "${source_root}/${src}" "${target}/${dst}"
    done
}

# Fail when ocx-sh/index@main grew a fixture family no leaf claims. `diff -r`
# only ever looks inside the staged leaves, so an added sibling upstream would
# read as "no drift" forever — the exact silence this gate exists to break.
# One recursive tree call; its first output line is the API's truncation flag,
# because a truncated listing would prove nothing.
assert_every_upstream_path_is_claimed() {
    local commit="$1" listing path pattern claimed
    listing="$(gh api "repos/ocx-sh/index/git/trees/${commit}?recursive=1" \
        --jq '(.truncated | tostring), (.tree[] | select(.type == "blob") | .path)')" ||
        die "cannot list the ocx-sh/index tree at ${commit}"
    [[ "$(head -n1 <<<"${listing}")" == "false" ]] ||
        die "the ocx-sh/index tree listing was truncated; cannot prove no fixture family was added"

    while read -r path; do
        [[ ${path} == "${src_rel}/"* ]] || continue
        path="${path#"${src_rel}/"}"
        claimed=""
        for pattern in "${dir_leaves[@]}" "${file_leaves[@]}"; do
            pattern="${pattern%%|*}"
            if [[ ${path} == "${pattern}" || ${path} == "${pattern}/"* ]]; then
                claimed="yes"
            fi
        done
        for pattern in "${unvendored[@]}"; do
            if [[ ${path} == "${pattern}" || ${path} == "${pattern}/"* ]]; then
                claimed="yes"
            fi
        done
        [[ -n ${claimed} ]] ||
            die "ocx-sh/index@${commit} carries ${src_rel}/${path}, which no vendored leaf claims; vendor it or list it in 'unvendored' in this script"
    done <<<"$(tail -n +2 <<<"${listing}")"
}

# Fail when ocx-sh/index@main's fixtures differ from the vendored copy. The
# vendored tree is never written — this is a gate, not a sync.
check_drift() {
    local stage_dir="$1" commit scratch name
    local -a excludes=()
    command -v gh >/dev/null 2>&1 || die "the drift gate needs the gh CLI"
    commit="$(gh api repos/ocx-sh/index/commits/main --jq '.sha')" ||
        die "cannot resolve main on ocx-sh/index"
    assert_every_upstream_path_is_claimed "${commit}"
    stage_from_github "${commit}" "${stage_dir}"
    scratch="${stage_dir}/expected"
    mkdir -p -- "${scratch}"
    place_leaves "${stage_dir}/${src_rel}" "${scratch}"
    echo "comparing ${dest_rel} against ocx-sh/index@main (${commit})"
    for name in "${ocx_authored[@]}"; do
        excludes+=(-x "${name}")
    done
    diff -r "${excludes[@]}" -- "${scratch}" "${dest}" ||
        die "vendored fixtures differ from ocx-sh/index@main (${commit}); re-run with '--ref main' and review the diff"
    echo "no drift: vendored fixtures match ocx-sh/index@main (${commit})"
}

main() {
    local mode="sync" ref="" stage_dir commit

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --check) mode="check" ;;
            --ref)
                [[ $# -ge 2 ]] || die "--ref needs a value"
                ref="$2"
                shift
                ;;
            *) die "unknown argument '$1'" ;;
        esac
        shift
    done

    stage_dir="$(mktemp -d)"
    # shellcheck disable=SC2064 # expand stage_dir now, while it is still in scope
    trap "rm -rf -- '${stage_dir}'" EXIT

    if [[ ${mode} == "check" ]]; then
        check_drift "${stage_dir}"
        return
    fi

    if [[ -z ${ref} ]]; then
        [[ -f ${commit_file} ]] || die "no SOURCE_COMMIT to pin a fetch to; pass --ref"
        ref="$(head -n1 -- "${commit_file}")"
        [[ -n ${ref} ]] || die "SOURCE_COMMIT is empty"
    fi

    commit="$(stage "${ref}" "${stage_dir}")"
    place_leaves "${stage_dir}/${src_rel}" "${dest}"
    printf '%s\n' "${commit}" >"${commit_file}"
    echo "--- vendored tree changes (review, then commit yourself) ---"
    git -C "${repo_root}" status --porcelain -- "${dest_rel}" || true
}

main "$@"
