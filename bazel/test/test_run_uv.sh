set -e

function assert_uv_version() {
    local expected_version=$1
    local uv_binary=$2

    version_out=$($uv_binary --version)
    if [[ "$version_out" != "uv $expected_version" ]]; then
        echo "Expected version '$expected_version' but got '$version_out' for binary '$uv_binary'"
        exit 1
    fi
}

# This test verifies that the uv binaries built by rules_ocx can be executed correctly.
assert_uv_version "0.10.10" "$UV_LATEST_BIN"
assert_uv_version "0.10.10" "$UV_0_10_BIN"
assert_uv_version "0.10.10" "$UV_0_10_10_BIN"
