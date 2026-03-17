${shebang}

set -euo pipefail
# resolve bin file in RUNFILES
bin_path_short=${bin_path_short}
bin_path=$(
  { [ -f "${RUNFILES_DIR:-/dev/null}/$bin_path_short" ] && echo "${RUNFILES_DIR}/$bin_path_short"; } ||
  grep -sm1 "^$bin_path_short " "${RUNFILES_MANIFEST_FILE:-/dev/null}" 2>/dev/null | cut -f2- -d' ' ||
  { [ -f "$0.runfiles/$bin_path_short" ] && echo "$0.runfiles/$bin_path_short"; } ||
  grep -sm1 "^$bin_path_short " "$0.runfiles_manifest" 2>/dev/null | cut -f2- -d' ' ||
  grep -sm1 "^$bin_path_short " "$0.exe.runfiles_manifest" 2>/dev/null | cut -f2- -d' ' ||
  { echo>&2 "ERROR: cannot find $bin_path_short"; exit 1; }
)

# provide installPath used for interpolation when exporting the env vars based on the image root dir
installPath=${bin_path%%\/layer\/*}/layer/
# export envs - export statements are generated inside of bazel rule
${env}
installPath=

exec "$bin_path" "$@"
