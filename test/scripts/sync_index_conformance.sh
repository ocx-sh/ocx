#!/usr/bin/env bash
# Re-vendor the ocx-sh/index golden serializer fixtures (CONTRACTS §14 byte
# vectors) into crates/ocx_lib/tests/fixtures/index_wire/{root,observation}.
# Prints a diff summary and NEVER commits — review and commit the result.
#
#   Fast path : a local ocx-sh/index checkout ($INDEX_REPO_PATH, else ../index)
#               that actually carries the fixtures — copies from its working
#               tree and repins SOURCE_COMMIT to its HEAD.
#   Fallback  : fetch each file from GitHub pinned to the committed SOURCE_COMMIT
#               (a fixed commit, never a moving branch — so a bare re-run only
#               verifies, it never silently fast-forwards).
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "${script_dir}/../.." && pwd)"
dest_rel="crates/ocx_lib/tests/fixtures/index_wire"
dest="${repo_root}/${dest_rel}"
src_rel="bot/tests/golden/serializer"
commit_file="${dest}/SOURCE_COMMIT"

die() {
    echo "sync_index_conformance: $*" >&2
    exit 1
}

copy_subdir() {
    # $1 = index serializer dir, $2 = subdir to mirror (root | observation)
    local src="$1" sub="$2"
    [ -d "${src}/${sub}" ] || die "index source missing ${sub}/ under ${src}"
    rm -rf -- "${dest:?}/${sub}"
    cp -R -- "${src}/${sub}" "${dest}/${sub}"
}

from_local_checkout() {
    local index_repo="$1" head
    head="$(git -C "${index_repo}" rev-parse HEAD)" || die "cannot read HEAD of ${index_repo}"
    echo "fast path: local index checkout ${index_repo} @ ${head}"
    copy_subdir "${index_repo}/${src_rel}" root
    copy_subdir "${index_repo}/${src_rel}" observation
    printf '%s\n' "${head}" >"${commit_file}"
}

fetch_leaf() {
    # $1 = pinned commit, $2 = leaf dir under $src_rel (root | observation/sha256)
    local commit="$1" leaf="$2" listing name url
    listing="$(gh api "repos/ocx-sh/index/contents/${src_rel}/${leaf}?ref=${commit}" \
        --jq '.[] | [.name, .download_url] | @tsv')" ||
        die "SOURCE_COMMIT ${commit} not found on ocx-sh/index at ${src_rel}/${leaf}. If the index PR has not merged yet, re-pin SOURCE_COMMIT to the merged SHA and retry."
    rm -rf -- "${dest:?}/${leaf}"
    mkdir -p -- "${dest}/${leaf}"
    while IFS=$'\t' read -r name url; do
        [ -n "${name}" ] || continue
        curl -fsSL -- "${url}" -o "${dest}/${leaf}/${name}" ||
            die "failed to download ${leaf}/${name} from ${url}"
    done <<<"${listing}"
}

from_github() {
    local commit
    [ -f "${commit_file}" ] || die "no local index checkout and no SOURCE_COMMIT to pin a fetch to"
    commit="$(head -n1 -- "${commit_file}")"
    [ -n "${commit}" ] || die "SOURCE_COMMIT is empty"
    command -v gh >/dev/null 2>&1 ||
        die "no local index checkout with fixtures found. Set INDEX_REPO_PATH to an ocx-sh/index checkout, or install the gh CLI for the pinned fallback."
    echo "fallback: fetching ${src_rel} from ocx-sh/index @ ${commit}"
    fetch_leaf "${commit}" root
    fetch_leaf "${commit}" observation/sha256
}

main() {
    local index_repo="${INDEX_REPO_PATH:-${repo_root}/../index}"
    if [ -d "${index_repo}/.git" ] && [ -d "${index_repo}/${src_rel}" ]; then
        from_local_checkout "${index_repo}"
    else
        from_github
    fi
    echo "--- vendored tree changes (review, then commit yourself) ---"
    git -C "${repo_root}" status --porcelain -- "${dest_rel}" || true
}

main "$@"
