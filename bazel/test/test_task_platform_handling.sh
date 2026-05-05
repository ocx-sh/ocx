function get_bin() {
    for bin in "$@"; do
        if [[ "$bin" =~ .*/task(\.exe)?$ ]]; then
            echo $bin
            return
        fi
    done
    exit 1
}

task_windows_bin=$(get_bin $TASK_WINDOWS_X86_64)
task_linux_bin=$(get_bin $TASK_LINUX_AARCH64)

function assert_file_type() {
    local expected_type=$1
    local file_path=$2
    file_path=$(realpath "$file_path")
    file_out=$(file -b "$file_path")
    if [[ "$file_out" != *"$expected_type"* ]]; then
        echo "Expected file type '$expected_type' but got '$file_out' for file '$file_path'"
        exit 1
    fi
}

assert_file_type "PE32+" "$task_windows_bin"
assert_file_type "x86-64" "$task_windows_bin"

assert_file_type "ELF" "$task_linux_bin"
assert_file_type "aarch64" "$task_linux_bin"