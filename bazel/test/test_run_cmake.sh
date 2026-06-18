set -e

function assert_cmake_version() {
    local expected_version=$1
    local cmake_binary=$2

    version_out=$($cmake_binary --version)
    if [[ "$version_out" != *"$expected_version"* ]]; then
        echo "Expected version '$expected_version' but got '$version_out' for binary '$cmake_binary'"
        exit 1
    fi
}

# This test verifies that the uv binaries built by rules_ocx can be executed correctly.
assert_cmake_version "3.31.11" "$CMAKE3_BIN"
assert_cmake_version "4.2.3" "$CMAKE4_BIN"
