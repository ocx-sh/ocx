#!/bin/sh
# shellcheck disable=SC3043  # `local` verified at runtime by has_local()
# install.sh - OCX installer for Unix and macOS
# https://ocx.sh
#
# Usage:
#   curl -fsSL https://ocx.sh/install.sh | sh
#   curl -fsSL https://ocx.sh/install.sh | sh -s -- --no-modify-path
#   curl -fsSL https://ocx.sh/install.sh | sh -s -- --version 0.5.0
#

set -eu

# If `local` is not supported, alias it to `typeset`.
has_local() { local _ 2>/dev/null; }
has_local || alias local=typeset

GITHUB_REPO="ocx-sh/ocx"
GITHUB_DOWNLOAD_URL="https://github.com/${GITHUB_REPO}/releases/download"
GITHUB_API_URL="https://api.github.com/repos/${GITHUB_REPO}/releases"

# --- Output helpers ---

say() {
    printf 'ocx-install: %s\n' "$1"
}

err() {
    say "error: $1" >&2
    exit 1
}

warn() {
    say "warning: $1" >&2
}

# Replace $HOME prefix with ~ for user-facing display
tildify() {
    echo "$1" | sed "s|^${HOME}|~|"
}

# --- Core utilities ---

# Check if a command exists in PATH
check_cmd() {
    command -v "$1" >/dev/null 2>&1
}

need_cmd() {
    if ! check_cmd "$1"; then
        err "required command not found: $1"
    fi
}

# Run a command and exit with error if it fails
ensure() {
    if ! "$@"; then err "command failed: $*"; fi
}

# Intentionally run a command without error checking (e.g., cleanup)
ignore() {
    "$@" || true
}

# Get the home directory in a way that works even if $HOME is not set.
get_home() {
    if [ -n "${HOME:-}" ]; then
        echo "$HOME"
    elif [ -n "${USER:-}" ]; then
        getent passwd "$USER" | cut -d: -f6
    else
        getent passwd "$(id -un)" | cut -d: -f6
    fi
}

# Ensure HOME is set - some minimal environments (containers, cron) omit it.
HOME="${HOME:-$(get_home)}"

# Check if running on Windows with POSIX-compliant shell (CYGWIN, MSYS, MINGW)
test_windows_posix() {
    case "$(uname)" in
        CYGWIN* | MSYS* | MINGW*)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

# Converts to platform native path format (Windows backslashes, escaped for JSON).
to_native_path() {
    local _path="$1"
    if test_windows_posix && check_cmd cygpath; then
        cygpath -w "$_path" | sed 's/\\/\\\\/g'
    else
        echo "$_path"
    fi
}

# TTY/color detection - bold-only, respects NO_COLOR (https://no-color.org/)
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
    _bold=$(tput bold 2>/dev/null || echo "")
    _reset=$(tput sgr0 2>/dev/null || echo "")
else
    _bold=""
    _reset=""
fi

# --- Usage ---

usage() {
    cat <<'EOF'
OCX installer - https://ocx.sh

USAGE:
    curl -fsSL https://ocx.sh/install.sh | sh
    curl -fsSL https://ocx.sh/install.sh | sh -s -- [OPTIONS]

OPTIONS:
    --version <VERSION>   Install a specific version (e.g., 0.5.0)
    --no-modify-path      Don't modify shell profile files
    -h, --help            Print this help message

ENVIRONMENT:
    OCX_HOME              Installation directory (default: ~/.ocx)
    OCX_NO_MODIFY_PATH    Set to 1/true/yes to skip shell profile modification
    GITHUB_TOKEN          GitHub API token - set this if you hit rate limits
                          when resolving the latest version
EOF
}

# --- Platform detection ---

detect_target() {
    local _os _arch _libc

    _os=$(uname -s)
    case "$_os" in
        Linux | Darwin) ;;
        *) err "unsupported operating system: $_os (expected Linux or macOS)" ;;
    esac

    _arch=$(uname -m)
    case "$_arch" in
        x86_64 | amd64) _arch="x86_64" ;;
        aarch64 | arm64) _arch="aarch64" ;;
        *) err "unsupported architecture: $_arch (expected x86_64 or aarch64)" ;;
    esac

    # Rosetta 2 detection: prefer native arm64 binary on Apple Silicon even
    # when the shell runs under Rosetta (which reports x86_64 via uname -m).
    if [ "$_os" = "Darwin" ] && [ "$_arch" = "x86_64" ]; then
        if sysctl -n hw.optional.arm64 2>/dev/null | grep -q '1'; then
            say "Detected Apple Silicon running under Rosetta - using native arm64 binary."
            _arch="aarch64"
        fi
    fi

    case "$_os" in
        Linux)
            _libc="gnu"
            # shellcheck disable=SC2144
            if check_cmd ldd; then
                case "$(ldd --version 2>&1 || true)" in
                    *musl*) _libc="musl" ;;
                esac
            # Glob check catches Void Linux musl, Gentoo musl, and other
            # non-Alpine musl distros where ldd is absent.
            elif ls /lib/ld-musl-*.so.1 >/dev/null 2>&1; then
                _libc="musl"
            elif [ -f /etc/alpine-release ]; then
                _libc="musl"
            fi
            echo "${_arch}-unknown-linux-${_libc}"
            ;;
        Darwin)
            echo "${_arch}-apple-darwin"
            ;;
        *)
            err "unsupported operating system: $_os"
            ;;
    esac
}

# --- Download utilities ---

# Detect curl or wget; sets _downloader
# Snap-packaged curl on Ubuntu has sandbox restrictions that can silently
# break downloads to /tmp - prefer wget if curl is from snap.
# wget must support --secure-protocol, --https-only AND --header-file
# (--header-file requires wget >= 1.17, circa 2015 - the binding constraint).
# On too-old wget without these flags, fail closed: do not silently downgrade
# to unprotected HTTP, do not silently leak GITHUB_TOKEN via argv.
_check_wget_tls_flags() {
    # Probe via --help output - no network call, no side-effects on localhost.
    # grep exits 1 on no match; treat that as flags absent (old wget).
    # Cache --help once: separate invocations could see different binaries.
    local _wh
    _wh=$(wget --help 2>&1) || return 1
    printf '%s' "$_wh" | grep -q -- '--https-only' || return 1
    printf '%s' "$_wh" | grep -q -- '--header-file' || return 1
    return 0
}

detect_downloader() {
    if check_cmd curl; then
        if curl --version 2>&1 | head -1 | grep -qF 'snap'; then
            warn "detected snap-packaged curl (may have sandbox restrictions)"
            if check_cmd wget; then
                if _check_wget_tls_flags; then
                    _downloader="wget"
                    return
                fi
                warn "wget too old to enforce TLS restrictions - falling back to snap curl"
            fi
            warn "no usable wget fallback - continuing with snap curl"
        fi
        _downloader="curl"
    elif check_cmd wget; then
        if ! _check_wget_tls_flags; then
            err "wget found but is too old to enforce TLS-only downloads (need wget >= 1.17).
  Install a newer wget or install curl to continue."
        fi
        _downloader="wget"
    else
        err "either curl or wget is required to download OCX"
    fi
}

# Download URL to file
download_to_file() {
    local _url="$1" _dest="$2"

    if [ "$_downloader" = "curl" ]; then
        curl --proto '=https' --tlsv1.2 -fsSL -o "$_dest" "$_url"
    else
        wget --secure-protocol=TLSv1_2 --https-only -q -O "$_dest" "$_url"
    fi
}

# Download URL to stdout
download() {
    if [ "$_downloader" = "curl" ]; then
        curl --proto '=https' --tlsv1.2 -fsSL "$1"
    else
        wget --secure-protocol=TLSv1_2 --https-only -qO- "$1"
    fi
}

# Download GitHub API URL to stdout - uses GITHUB_TOKEN when set
# curl: token passed via -H @<file> (curl 7.55+) to keep it out of argv/ps list.
#   A chmod-600 temp file holds the header line; deleted immediately after use.
# wget: token also passed via --header-file=<file> (wget >= 1.17) for the same
#   reason - argv is visible in /proc/PID/cmdline and `ps ef` on shared hosts.
download_api() {
    local _url="$1"

    if [ -n "${GITHUB_TOKEN:-}" ]; then
        if [ "$_downloader" = "curl" ]; then
            # Write header to a 0600 temp file; -H @<file> reads it without
            # exposing the token value in the process argument list.
            local _hdr_file
            _hdr_file=$(mktemp)
            chmod 600 "$_hdr_file"
            printf 'Authorization: token %s\n' "${GITHUB_TOKEN}" >"$_hdr_file"
            # Capture return code without relying on set -e - if curl fails,
            # bare statement position would exit before the rm -f, leaking the
            # token file on disk.  if/else always executes rm -f regardless.
            local _rc
            if curl --proto '=https' --tlsv1.2 -fsSL -H "@${_hdr_file}" "$_url"; then
                _rc=0
            else
                _rc=$?
            fi
            rm -f "$_hdr_file"
            return "$_rc"
        else
            # Mirror curl pattern: write Authorization header to a 0600 temp
            # file and pass via --header-file so the token never appears in
            # argv (visible in /proc/PID/cmdline and `ps ef` on shared hosts).
            # --header-file supported since wget 1.17 (2015); _check_wget_tls_flags
            # already ensures we only reach this path on a capable wget.
            local _whdr_file _wrc
            _whdr_file=$(mktemp)
            chmod 600 "$_whdr_file"
            printf 'Authorization: token %s\n' "${GITHUB_TOKEN}" >"$_whdr_file"
            if wget --secure-protocol=TLSv1_2 --https-only -q \
                --header-file="$_whdr_file" -O- "$_url"; then
                _wrc=0
            else
                _wrc=$?
            fi
            rm -f "$_whdr_file"
            return "$_wrc"
        fi
    else
        download "$_url"
    fi
}

# --- Checksum verification ---

verify_checksum() {
    local _dir="$1" _file="$2" _sha_cmd _expected _actual

    if check_cmd sha256sum; then
        _sha_cmd="sha256sum"
    elif check_cmd shasum; then
        _sha_cmd="shasum -a 256"
    else
        warn "neither sha256sum nor shasum found - SKIPPING CHECKSUM VERIFICATION"
        warn "install coreutils or set PATH to include sha256sum for verified downloads"
        return 0
    fi

    # Strip the optional leading "*" (binary-mode sha256sum) before comparing.
    _expected=$(awk -v f="$_file" '
        { name = $2; sub(/^\*/, "", name); if (name == f) { print $1; exit } }
    ' "$_dir/sha256.sum")
    if [ -z "$_expected" ]; then
        err "checksum for $_file not found in sha256.sum"
    fi

    # shellcheck disable=SC2086
    _actual=$(cd "$_dir" && $_sha_cmd "$_file" | awk '{print $1}')

    if [ "$_expected" != "$_actual" ]; then
        err "checksum mismatch for $_file
  expected: $_expected
  got:      $_actual"
    fi

    say "Checksum verified."
}

# --- Version resolution ---

get_latest_version() {
    local _release_info _tag

    _release_info=$(download_api "${GITHUB_API_URL}/latest") || {
        # Check if this might be a rate limit issue
        if [ -z "${GITHUB_TOKEN:-}" ]; then
            err "failed to fetch latest release from GitHub
  This may be a rate-limit issue. Try setting GITHUB_TOKEN:
    export GITHUB_TOKEN=ghp_...
    curl -fsSL https://ocx.sh/install.sh | sh"
        else
            err "failed to fetch latest release from GitHub - check your internet connection and token"
        fi
    }

    # Extract tag_name from JSON without jq
    _tag=$(printf '%s' "$_release_info" |
        grep -o '"tag_name"[[:space:]]*:[[:space:]]*"[^"]*"' |
        head -1 |
        grep -o '"[^"]*"$' |
        tr -d '"')

    if [ -z "$_tag" ]; then
        err "could not determine latest version from GitHub"
    fi

    # Strip leading 'v'
    printf '%s' "$_tag" | sed 's/^v//'
}

# --- Shell environment files ---

# Write $OCX_HOME/env.sh - POSIX fail-safe form.
# Prepends the OCX bin directory (resolved through the install candidate's
# `current` symlink) to PATH, then sources the global toolchain env for any
# additional tools the user has declared in $OCX_HOME/ocx.toml. OCX itself
# is NOT a global-toolchain entry - its version source is the install
# candidate, updated via `ocx package install --select ocx.sh/ocx/cli:N`
# or by re-running the install script.
# Idempotency: PATH `case`-match below dedups within a single session
# without needing a top-level guard variable (which can survive shell
# state across reinstalls and silently no-op the source).
create_env_sh() {
    local _ocx_home="${OCX_HOME:-$HOME/.ocx}"

    mkdir -p "$_ocx_home"

    # Emit a thin shim that delegates to `ocx self activate --shell=sh` at
    # runtime.  Every variable reference inside is intentionally verbatim
    # (no install-time substitution) so the file is byte-identical across
    # users regardless of their OCX_HOME path.
    cat >"$_ocx_home/env.sh" <<'EOF'
#!/bin/sh
# Managed by ocx installer - do not edit.

# Double-source guard - prevents PATH duplication on re-source (e.g. user
# re-sources .bashrc).  Set before any side effects so that a re-source after
# a partial failure also short-circuits cleanly.  Idempotent under `set -u`.
if [ -n "${_OCX_ENV_LOADED:-}" ]; then
    return 0 2>/dev/null || exit 0
fi
_OCX_ENV_LOADED=1
export _OCX_ENV_LOADED

# OCX_HOME env-var-with-fallback. Assigns and exports only when unset or empty.
: "${OCX_HOME:=$HOME/.ocx}"
export OCX_HOME

_ocx_bin="$OCX_HOME/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx"

# Detect the real sourcing shell so the right completion backend is chosen.
# This file is sourced by bash AND zsh (not just /bin/sh); `sh` resolves to
# Shell::Dash, which has no clap completion backend - so bash/zsh users would
# get no completions if we hardcoded `--shell=sh`. PATH and global-env-eval
# output are identical across the POSIX arms, so this only changes the
# completion extension.
_ocx_shell=sh
if [ -n "${BASH_VERSION:-}" ]; then
    _ocx_shell=bash
elif [ -n "${ZSH_VERSION:-}" ]; then
    _ocx_shell=zsh
fi

# Decide completions here, where interactivity is known ($- carries `i` for an
# interactive shell), and pass the explicit --completion/--no-completion flag.
# stderr is still redirected (2>/dev/null) to suppress startup diagnostics, but
# the gate no longer depends on the binary probing isatty(2).
if [ -x "$_ocx_bin" ]; then
    case "$-" in
        *i*) eval "$("$_ocx_bin" self activate --shell="$_ocx_shell" --completion 2>/dev/null)" || true ;;
        *) eval "$("$_ocx_bin" self activate --shell="$_ocx_shell" --no-completion 2>/dev/null)" || true ;;
    esac
fi
unset _ocx_bin _ocx_shell
EOF
}

# Write $OCX_HOME/env.fish - fish-syntax per-family file.
# Thin shim that delegates to `ocx self activate --shell=fish` at runtime.
# File is byte-identical across users - no install-time substitution.
create_env_fish() {
    local _ocx_home="${OCX_HOME:-$HOME/.ocx}"

    mkdir -p "$_ocx_home"

    cat >"$_ocx_home/env.fish" <<'EOF'
# Managed by ocx installer - do not edit.
# Double-source guard - prevents PATH duplication on re-source.
# Set before any side effects so re-source after partial failure also short-circuits.
if set -q _OCX_ENV_LOADED
    return
end
set -gx _OCX_ENV_LOADED 1

if not set -q OCX_HOME
    set -gx OCX_HOME "$HOME/.ocx"
end

set -l _ocx_bin "$OCX_HOME/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx"
# Decide completions here via `status is-interactive` and pass the explicit
# --completion/--no-completion flag. stderr is still redirected (2>/dev/null) to
# suppress startup diagnostics, but the gate no longer depends on isatty(2).
if test -x "$_ocx_bin"
    if status is-interactive
        "$_ocx_bin" self activate --shell=fish --completion 2>/dev/null | source
    else
        "$_ocx_bin" self activate --shell=fish --no-completion 2>/dev/null | source
    end
end
EOF
}

# Write $OCX_HOME/env.ps1 - PowerShell per-family file.
# Thin shim that delegates to `ocx self activate --shell=powershell` at
# runtime.  File is byte-identical across users - no install-time substitution.
#
# Must stay byte-identical to install.ps1's Create-EnvFile here-string. The
# exe-name probe uses $env:OS, not $IsWindows: $IsWindows is a PowerShell 6+
# automatic variable, undefined on Windows PowerShell 5.1, and referencing it
# throws under Set-StrictMode. $env:OS ('Windows_NT' on every Windows
# PowerShell, unset elsewhere) is StrictMode-safe. Keep the generated shim free
# of the literal "$IsWindows" token (a token guard in test_install_sh.py enforces it).
create_env_ps1() {
    local _ocx_home="${OCX_HOME:-$HOME/.ocx}"

    mkdir -p "$_ocx_home"

    cat >"$_ocx_home/env.ps1" <<'EOF'
# Managed by ocx installer - do not edit.
# Double-source guard - prevents PATH duplication on re-source.
# Set before any side effects so re-source after partial failure also short-circuits.
if ($env:_OCX_ENV_LOADED) { return }
$env:_OCX_ENV_LOADED = '1'

if (-not $env:OCX_HOME) {
    # $env:USERPROFILE is null on Linux/macOS pwsh; fall back to $HOME so this
    # shim works for PowerShell 7 on every platform, not just Windows.
    $_ocxBase = if ($env:USERPROFILE) { $env:USERPROFILE } else { $HOME }
    $env:OCX_HOME = Join-Path $_ocxBase '.ocx'
}

# Binary name is platform-specific. $env:OS is 'Windows_NT' on every Windows
# PowerShell (Desktop 5.1 + Core 7) and unset on Linux/macOS pwsh; reading an
# unset $env: var is StrictMode-safe (yields $null). Forward slashes are
# accepted by PowerShell on every platform.
$_ocxExe = if ($env:OS -eq 'Windows_NT') { 'ocx.exe' } else { 'ocx' }
$_ocxBin = Join-Path $env:OCX_HOME "symlinks/ocx.sh/ocx/cli/current/content/bin/$_ocxExe"
if (Test-Path $_ocxBin -PathType Leaf) {
    # Build args as an array so the completion flag is appended cleanly - never
    # a $null/empty positional that clap would reject (Windows PowerShell 5.1
    # passes a bare $null arg as an empty string).
    # Request completions only on an interactive PowerShell 5.0+ session: legacy
    # Windows PowerShell <5.0 cannot run clap's `using namespace` /
    # `Register-ArgumentCompleter -Native` completion output, so it opts out with
    # --no-completion while still emitting PATH + global env.
    $_ocxArgs = @('self', 'activate', '--shell=powershell')
    if ([Environment]::UserInteractive -and $PSVersionTable.PSVersion.Major -ge 5) {
        $_ocxArgs += '--completion'
    } else {
        $_ocxArgs += '--no-completion'
    }
    $_ocxActivate = (& $_ocxBin @_ocxArgs 2>$null) | Out-String
    # Guard $null/empty: Out-String of empty/failed output yields $null, and
    # `Invoke-Expression $null` throws "Cannot bind argument ... is null".
    if ($_ocxActivate) { Invoke-Expression $_ocxActivate }
}
Remove-Variable _ocxBase, _ocxExe, _ocxBin, _ocxArgs, _ocxActivate -ErrorAction SilentlyContinue
EOF
}

# Write $OCX_HOME/env.nu - Nushell per-family file.
# Thin shim that delegates to `ocx self activate --shell=nushell` at runtime.
# Nushell's `source` is parse-time, so activation output is written to a temp
# file and sourced from there.  File is byte-identical across users - no
# install-time substitution.
# No completion flag: nushell has no clap_complete backend
# (completion_target -> None), so the flag would be a no-op. PATH + global env
# are the only activation output here, and those are session-independent.
create_env_nu() {
    local _ocx_home="${OCX_HOME:-$HOME/.ocx}"

    mkdir -p "$_ocx_home"

    cat >"$_ocx_home/env.nu" <<'EOF'
# Managed by ocx installer - do not edit.
# Double-source guard - prevents PATH duplication on re-source.
# Set before any side effects so re-source after partial failure also short-circuits.
if ($env._OCX_ENV_LOADED? | default '') != '' { return }
$env._OCX_ENV_LOADED = '1'

$env.OCX_HOME = ($env.OCX_HOME? | default ($env.HOME | path join '.ocx'))

let _ocx_bin = ($env.OCX_HOME | path join 'symlinks/ocx.sh/ocx/cli/current/content/bin/ocx')
if ($_ocx_bin | path exists) {
    ^$_ocx_bin self activate --shell=nushell 2>/dev/null | save --force ($nu.temp-path | path join 'ocx_activate.nu')
    source ($nu.temp-path | path join 'ocx_activate.nu')
}
EOF
}

# Write the Nushell vendor autoload file that sources $OCX_HOME/env.nu.
# Nushell auto-sources every .nu file under the vendor/autoload directory at
# startup.  The autoload file sets OCX_HOME via env-var-with-fallback at
# runtime, then computes the env.nu path from it - no literal substitution.
# Note: the inner `source` inside env.nu still requires a literal path
# resolved at startup by env.nu itself (via the temp-file pattern).
create_nu_autoload() {
    local _nu_autoload_dir

    _nu_autoload_dir="${XDG_DATA_HOME:-$HOME/.local/share}/nushell/vendor/autoload"
    mkdir -p "$_nu_autoload_dir"

    cat >"$_nu_autoload_dir/ocx.nu" <<'EOF'
# OCX shell environment - managed by ocx installer.
$env.OCX_HOME = ($env.OCX_HOME? | default ($env.HOME | path join '.ocx'))
source ($env.OCX_HOME + '/env.nu')
EOF
}

# Write $OCX_HOME/env.elv - Elvish per-family file.
# Thin shim that delegates to `ocx self activate --shell=elvish` at runtime.
# File is byte-identical across users - no install-time substitution.
create_env_elv() {
    local _ocx_home="${OCX_HOME:-$HOME/.ocx}"

    mkdir -p "$_ocx_home"

    cat >"$_ocx_home/env.elv" <<'EOF'
# Managed by ocx installer - do not edit.
# Double-source guard - prevents PATH duplication on re-source.
# Set before any side effects so re-source after partial failure also short-circuits.
if (has-env _OCX_ENV_LOADED) {
    return
}
set-env _OCX_ENV_LOADED 1

if (not (has-env OCX_HOME)) {
    set-env OCX_HOME (path:join $E:HOME .ocx)
}

var _ocx_bin = (path:join $E:OCX_HOME symlinks/ocx.sh/ocx/cli/current/content/bin/ocx)
# rc.elv is sourced only for interactive Elvish sessions, so --completion is
# unconditional here. The hook redirects stderr (2>/dev/null), so the flag -
# not an isatty(2) probe - is what gates completion work.
if ?(test -x $_ocx_bin) {
    eval (e:$_ocx_bin self activate --shell=elvish --completion 2>/dev/null | slurp)
}
EOF
}

# Compatibility alias: the old env file had no .sh extension.  Remove it on
# install so upgraders don't source a stale file that calls deleted commands.
remove_legacy_env_file() {
    local _ocx_home="${OCX_HOME:-$HOME/.ocx}"
    local _old="$_ocx_home/env"
    # Defence-in-depth (CWE-367 TOCTOU): use `stat` once to read both the file
    # type and the symlink status from a single inode probe so an attacker
    # can't swap the regular file for a symlink between two independent
    # `test` invocations. `stat -c '%F'` is GNU; fall back to a single `[`
    # composition with -a on platforms that lack GNU stat (still POSIX and
    # narrows the race window vs the previous two-process form).
    if command -v stat >/dev/null 2>&1 && stat -c '%F' /dev/null >/dev/null 2>&1; then
        if [ "$(stat -c '%F' "$_old" 2>/dev/null || true)" = "regular file" ]; then
            rm -f -- "$_old"
        fi
    else
        # shellcheck disable=SC2166  # POSIX -a kept inside a single test for TOCTOU window narrowing
        if [ -f "$_old" -a ! -L "$_old" ]; then
            rm -f -- "$_old"
        fi
    fi
}

create_fish_config() {
    local _fish_conf_dir

    _fish_conf_dir="${XDG_CONFIG_HOME:-$HOME/.config}/fish/conf.d"
    mkdir -p "$_fish_conf_dir"

    cat >"$_fish_conf_dir/ocx.fish" <<'FISHEOF'
# OCX shell environment - managed by ocx installer.
# Sources $OCX_HOME/env.fish which evaluates the global toolchain env.
set -l _ocx_env (string join '' (set -q OCX_HOME; and echo $OCX_HOME; or echo $HOME/.ocx) '/env.fish')
if test -f "$_ocx_env"
    source "$_ocx_env"
end
FISHEOF
}

# --- Legacy profile line migration (W6) ---

# Detect and remove any stale `. "$OCX_HOME/init.<shell>"` lines written by
# the deleted `ocx shell init` command, plus older `. "$OCX_HOME/env"` lines
# (extensionless legacy env file). The current installer writes `env.sh`;
# stale source lines for the extensionless file silently skip via [[ -r ]]
# guards and leave ocx off PATH. Detection anchors to actual dot-source
# command form (leading whitespace + a dot/period command) so benign user
# comments are never modified (CWE-73 defense).
remove_legacy_init_lines() {
    local _profile="$1" _ocx_home _tmpfile

    if [ -z "$_profile" ] || ! [ -f "$_profile" ]; then
        return
    fi

    _ocx_home="${OCX_HOME:-$HOME/.ocx}"

    # Detection: file references one of the legacy patterns.
    if grep -qF "${_ocx_home}/init." "$_profile" 2>/dev/null ||
        grep -qE '^[[:space:]]*\. .*\.ocx/init\.' "$_profile" 2>/dev/null ||
        grep -qE '\.ocx/env"' "$_profile" 2>/dev/null; then
        _tmpfile=$(mktemp)
        # State machine. state: 0 = pass-through, 1 = saw `# OCX` header
        # (buffered, not yet committed), 2 = inside multi-line legacy guard.
        # Transitions:
        #   0 -> 1   on `# OCX` heading
        #   1 -> 2   on `if [[ -r "..ocx/env" ]] ...`  (discard header + opener)
        #   1 -> 0   on any other line (flush buffered header, then print line)
        #   2 -> 0   on `fi`                          (discard closer)
        # Bare dot-source legacy lines drop in any state.
        awk '
            state==0 && /^[[:space:]]*#[[:space:]]*OCX[[:space:]]*$/ {
                header=$0; state=1; next
            }
            state==1 && /^[[:space:]]*if[[:space:]]+\[\[[[:space:]]*-r[[:space:]]+"[^"]*\.ocx\/env"[[:space:]]*\]\]/ {
                # Inline single-line form: `... ]]; then . "..."; fi`
                if ($0 ~ /;[[:space:]]*fi[[:space:]]*$/) { state=0; header=""; next }
                state=2; next
            }
            state==1 {
                # Buffered `# OCX` did not introduce a legacy guard - emit it.
                print header; header=""; state=0
            }
            state==2 && /^[[:space:]]*fi[[:space:]]*$/ { state=0; next }
            state==2 { next }
            # Bare legacy dot-source lines (no surrounding guard).
            /^[[:space:]]*\. .*\.ocx\/init\./ { next }
            /^[[:space:]]*\. .*\.ocx\/env"?[[:space:]]*$/ { next }
            { print }
            END { if (state==1 && header != "") print header }
        ' "$_profile" >"$_tmpfile" || true
        if ! cmp -s -- "$_profile" "$_tmpfile"; then
            mv -- "$_tmpfile" "$_profile"
            say "Removed legacy OCX activation lines from $(tildify "$_profile")"
        else
            rm -f -- "$_tmpfile"
        fi
    fi
}

# --- Shell profile modification ---

# Profile target decision tree - covers BOTH login and interactive rc files
# so the activation block fires regardless of how the terminal is launched.
# Login-only targets (.zprofile, .bash_profile) miss Linux/WSL/VSCode
# terminals which open interactive non-login shells; interactive-only
# targets (.zshrc, .bashrc) miss macOS Terminal's default login shells.
# Writing to both is safe - env.sh's PATH `case`-match makes the second
# source a no-op (idempotent dedup).
#
#   bash -> ~/.bash_profile (or ~/.profile if no .bash_profile) + ~/.bashrc
#   zsh  -> ${ZDOTDIR:-$HOME}/.zprofile + ${ZDOTDIR:-$HOME}/.zshrc
#   fish -> ~/.config/fish/conf.d (managed via conf.d - no block needed here)
#   *    -> ~/.profile
#
# Returns one path per line.
detect_profile() {
    local _shell_name _zdotdir

    _shell_name=$(basename "${SHELL:-sh}")

    case "$_shell_name" in
        bash)
            if [ -f "$HOME/.bash_profile" ]; then
                echo "$HOME/.bash_profile"
            else
                echo "$HOME/.profile"
            fi
            echo "$HOME/.bashrc"
            ;;
        zsh)
            # Respect ZDOTDIR when set. Reject ZDOTDIR="/" to prevent writing
            # /.zprofile (CWE-22 defense - filesystem root write guard).
            _zdotdir="${ZDOTDIR:-$HOME}"
            if [ "$_zdotdir" = "/" ]; then
                warn "ZDOTDIR is '/' - refusing to write under /; falling back to \$HOME"
                _zdotdir="$HOME"
            fi
            echo "$_zdotdir/.zprofile"
            echo "$_zdotdir/.zshrc"
            ;;
        fish)
            # Fish uses conf.d - no block-marker profile edit needed
            echo ""
            ;;
        nu)
            # Nushell uses vendor/autoload - no block-marker profile edit needed
            echo ""
            ;;
        elvish)
            # Elvish uses rc.elv as the login profile
            echo "${XDG_CONFIG_HOME:-$HOME/.config}/elvish/rc.elv"
            ;;
        *)
            echo "$HOME/.profile"
            ;;
    esac
}

# Append a block-marker idempotent section to each profile target.
# Pattern: conda-style # BEGIN ocx / # END ocx block.
# - Idempotent per file: grep -qF the BEGIN marker before append.
# - Dot (.) not source: POSIX dash-safe.
# - Legacy detection: remove old $OCX_HOME/init.* source lines first (W6).
# - detect_profile may return multiple paths (one per line) so the block
#   reaches both login (.zprofile/.bash_profile) and interactive
#   (.zshrc/.bashrc) entry points.
modify_shell_profile() {
    local _profiles _profile _ocx_home _shell_name

    _ocx_home="${OCX_HOME:-$HOME/.ocx}"
    _shell_name=$(basename "${SHELL:-sh}")

    # Fish: conf.d config handles activation; no block-marker profile edit.
    if [ "$_shell_name" = "fish" ]; then
        create_fish_config
        say "Created Fish configuration."
        return
    fi

    # Nushell: vendor/autoload handles activation; no block-marker profile edit.
    if [ "$_shell_name" = "nu" ]; then
        create_nu_autoload
        say "Created Nushell autoload configuration."
        return
    fi

    _profiles=$(detect_profile)
    if [ -z "$_profiles" ]; then
        return
    fi

    # Always strip legacy OCX activation lines from .zshenv (sourced for
    # every zsh invocation - most aggressive cleanup target). Older
    # installers wrote a `[[ -r $HOME/.ocx/env ]] && . ...` block here that
    # silently swallows the missing extensionless env file and leaves ocx
    # off PATH.
    if [ -f "${ZDOTDIR:-$HOME}/.zshenv" ]; then
        remove_legacy_init_lines "${ZDOTDIR:-$HOME}/.zshenv"
    fi

    # Iterate over each candidate profile file.
    echo "$_profiles" | while IFS= read -r _profile; do
        [ -z "$_profile" ] && continue

        # W6: strip any legacy `ocx shell init`-written lines before inserting block.
        remove_legacy_init_lines "$_profile"

        # Idempotent per file: skip if block already present.
        if grep -qF "# BEGIN ocx" "$_profile" 2>/dev/null; then
            say "Shell profile already configured ($(tildify "$_profile"))."
            continue
        fi

        # Append the block-marker section. The install root is embedded as a
        # literal resolved path so the block works in fresh shells where
        # OCX_HOME is not exported. Elvish uses `eval (slurp < ...)` syntax;
        # all other shells use the POSIX dot-source form (dash-safe).
        if [ "$_shell_name" = "elvish" ]; then
            printf '\n# BEGIN ocx\neval (slurp < "%s/env.elv")\n# END ocx\n' \
                "$_ocx_home" >>"$_profile"
        else
            printf '\n# BEGIN ocx\n. "%s/env.sh"\n# END ocx\n' \
                "$_ocx_home" >>"$_profile"
        fi
        say "Added OCX to $(tildify "$_profile")"
    done
}

# Remove the block-marker section from the profile (uninstall path).
# Uses POSIX awk - avoids non-portable `sed -i`.
# Also strips legacy $OCX_HOME/init.* lines (W6).
remove_shell_profile() {
    local _profiles _profile _ocx_home _tmpfile _shell_name

    _ocx_home="${OCX_HOME:-$HOME/.ocx}"
    _shell_name=$(basename "${SHELL:-sh}")

    # Fish: remove conf.d config.
    if [ "$_shell_name" = "fish" ]; then
        local _fish_conf="${XDG_CONFIG_HOME:-$HOME/.config}/fish/conf.d/ocx.fish"
        if [ -f "$_fish_conf" ]; then
            rm -f -- "$_fish_conf"
            say "Removed Fish configuration."
        fi
        return
    fi

    # Nushell: remove vendor/autoload file.
    if [ "$_shell_name" = "nu" ]; then
        local _nu_autoload="${XDG_DATA_HOME:-$HOME/.local/share}/nushell/vendor/autoload/ocx.nu"
        if [ -f "$_nu_autoload" ]; then
            rm -f -- "$_nu_autoload"
            say "Removed Nushell autoload configuration."
        fi
        return
    fi

    _profiles=$(detect_profile)
    if [ -z "$_profiles" ]; then
        return
    fi

    # Iterate over each candidate profile file. The block-strip form:
    # BEGIN marker sets p=1 and is itself skipped; END marker resets p=0 and
    # is itself skipped; only non-suppressed (!p) lines are printed.
    echo "$_profiles" | while IFS= read -r _profile; do
        [ -z "$_profile" ] && continue
        [ -f "$_profile" ] || continue

        # W6: strip legacy init.* lines first.
        remove_legacy_init_lines "$_profile"

        if grep -qF "# BEGIN ocx" "$_profile" 2>/dev/null; then
            _tmpfile=$(mktemp)
            awk '/^# BEGIN ocx/{p=1;next} /^# END ocx/{p=0;next} !p{print}' \
                "$_profile" >"$_tmpfile" && mv -- "$_tmpfile" "$_profile"
            say "Removed OCX from $(tildify "$_profile")"
        fi
    done
}

# --- Bootstrap: OCX installs itself ---

bootstrap_ocx() {
    local _bin="$1" _version="$2"

    say "Bootstrapping OCX into its own package store..."
    if ! "$_bin" --remote package install --select "ocx.sh/ocx/cli:$_version"; then
        err "bootstrap failed: 'ocx --remote package install --select ocx.sh/ocx/cli:$_version'
  Ensure ocx v${_version} is published to the ocx.sh registry.
  If this is a first install and the registry is not yet populated,
  please wait for the release pipeline to complete."
    fi
}

# --- Success message ---

print_success() {
    local _version="$1" _ocx_home _env_display _old_version="${2:-}"

    _ocx_home="${OCX_HOME:-$HOME/.ocx}"
    _env_display=$(tildify "$_ocx_home/env.sh")

    if [ -n "$_old_version" ] && [ "$_old_version" != "$_version" ]; then
        printf '\n  %socx upgraded: %s -> %s%s\n' "$_bold" "$_old_version" "$_version" "$_reset"
    else
        printf '\n  %socx %s installed successfully!%s\n' "$_bold" "$_version" "$_reset"
    fi

    cat <<EOF

  To get started, restart your shell or run:

    . "$_ocx_home/env.sh"

  Then verify with:

    ocx about

  To uninstall, remove the OCX home directory:

    rm -rf $_ocx_home

EOF
}

# --- Temp directory cleanup ---

cleanup() {
    if [ -n "${_tmpdir:-}" ]; then
        ignore rm -rf "$_tmpdir"
    fi
}

# --- Test-mode install (CI / local dev) ---

# Install a pre-built ocx binary as the candidate, bypassing download + registry
# bootstrap. Driven by __OCX_TESTING_INSTALL_BINARY (double-underscore prefix =
# test-only, never a supported public knob). Lets the cross-shell test harness
# exercise real activation/completions from a freshly built binary.
install_local_test_binary() {
    local _src="$1" _ocx_home="$2" _cand_dir
    [ -f "$_src" ] || err "__OCX_TESTING_INSTALL_BINARY does not point to a file: $_src"
    _cand_dir="${_ocx_home}/symlinks/ocx.sh/ocx/cli/current/content/bin"
    say "Test mode: installing local binary as the candidate (no download, no bootstrap)."
    mkdir -p "$_cand_dir"
    cp "$_src" "${_cand_dir}/ocx"
    chmod +x "${_cand_dir}/ocx"
    say "Installed to $(tildify "${_cand_dir}/ocx")"
}

# Shared post-acquisition tail: write the per-family env files, optionally modify
# the login profile, export the CI path, and print the success banner. Called by
# both the normal download path and the __OCX_TESTING_INSTALL_BINARY test path so
# activation wiring is identical regardless of how the candidate was placed.
# remove_legacy_env_file strips the old extensionless $OCX_HOME/env file written
# by older installers so upgraders don't source a stale file.
finalize_install() {
    local _version="$1" _old_version="$2" _no_modify_path="$3"

    remove_legacy_env_file
    create_env_sh
    create_env_fish
    create_env_ps1
    create_env_nu
    create_env_elv

    # Create Fish conf.d / Nushell autoload if those shells are installed
    # (regardless of the default shell).
    if check_cmd fish; then
        create_fish_config
    fi
    if check_cmd nu; then
        create_nu_autoload
    fi

    if [ "$_no_modify_path" = "1" ]; then
        say "Skipping shell profile modification (--no-modify-path)."
    else
        modify_shell_profile
    fi

    export_github_path
    print_success "$_version" "$_old_version"
}

# --- Main ---

main() {
    local _no_modify_path _version _target _tmpdir _archive _tag
    local _archive_url _checksum_url _bin _ocx_home _old_version

    _no_modify_path="${OCX_NO_MODIFY_PATH:-0}"
    _version=""

    # Parse arguments
    while [ $# -gt 0 ]; do
        case "$1" in
            --no-modify-path)
                _no_modify_path=1
                ;;
            --version)
                if [ $# -lt 2 ]; then
                    err "--version requires a value"
                fi
                _version="$2"
                shift
                ;;
            --version=*)
                _version="${1#--version=}"
                ;;
            -h | --help)
                usage
                exit 0
                ;;
            *)
                err "unknown option: $1 (use --help for usage)"
                ;;
        esac
        shift
    done

    # Normalize truthy values for OCX_NO_MODIFY_PATH
    case "$_no_modify_path" in
        1 | true | yes | TRUE | YES) _no_modify_path=1 ;;
        *) _no_modify_path=0 ;;
    esac

    need_cmd uname
    need_cmd mktemp
    need_cmd tar
    detect_downloader

    _ocx_home="${OCX_HOME:-$HOME/.ocx}"

    # CWE-22 path-traversal guard: OCX_HOME must be an absolute path and must
    # not contain any ".." components.  Reject early so every downstream use of
    # $_ocx_home is safe.
    case "$_ocx_home" in
        /*) ;; # absolute path - OK
        *) err "OCX_HOME must be an absolute path: $_ocx_home" ;;
    esac
    case "$_ocx_home" in
        */../* | */..) err "OCX_HOME must not contain '..' components: $_ocx_home" ;;
        ../*) err "OCX_HOME must not contain '..' components: $_ocx_home" ;;
    esac
    # Defence-in-depth: $_ocx_home is embedded literally into generated
    # env.sh/env.fish/env.ps1 and the profile block. Reject shell
    # metacharacters so a CI-injected OCX_HOME cannot break out of the
    # quoted context in the generated activation files.
    case "$_ocx_home" in
        *'"'* | *'$'* | *'`'* | *';'* | *'&'* | *'|'* | *'\'* | *'
'*) err "OCX_HOME contains characters unsafe for shell embedding: $_ocx_home" ;;
    esac

    # Test-mode hatch: install a locally-built binary as the candidate and skip
    # the download + registry bootstrap (and the network version probe) entirely.
    # See install_local_test_binary; __OCX_TESTING_INSTALL_BINARY is test-only.
    if [ -n "${__OCX_TESTING_INSTALL_BINARY:-}" ]; then
        install_local_test_binary "$__OCX_TESTING_INSTALL_BINARY" "$_ocx_home"
        # `tr` swallows the exit status of a binary that ran but printed nothing,
        # so guard the empty case explicitly (no `pipefail` under POSIX sh).
        _version=$("${_ocx_home}/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx" version 2>/dev/null | tr -d '\n' || true)
        [ -n "$_version" ] || _version="local"
        finalize_install "$_version" "" "$_no_modify_path"
        return 0
    fi

    _target=$(detect_target)
    say "Detected platform: $_target"

    # Resolve version
    if [ -z "$_version" ]; then
        say "Fetching latest version..."
        _version=$(get_latest_version)
    fi

    # Validate version format - reject shell metacharacters and suspicious input
    if echo "$_version" | grep -q '[^0-9a-zA-Z.+-]'; then
        err "invalid version format: $_version (expected semver like 1.2.3 or 1.0.0-rc.1)"
    elif echo "$_version" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]'; then
        : # valid
    else
        err "invalid version format: $_version (expected semver like 1.2.3)"
    fi

    # Detect existing installation for upgrade messaging
    _old_version=""
    _bin_path="${_ocx_home}/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx"
    if [ -x "$_bin_path" ]; then
        _old_version=$("$_bin_path" version 2>/dev/null || echo "")
    fi

    say "Installing ocx v${_version}..."

    # Temporary directory with cleanup
    _tmpdir=$(mktemp -d)
    trap cleanup EXIT INT TERM HUP

    # Download archive and checksums
    _archive="ocx-${_target}.tar.xz"
    _tag="v${_version}"
    _archive_url="${GITHUB_DOWNLOAD_URL}/${_tag}/${_archive}"
    _checksum_url="${GITHUB_DOWNLOAD_URL}/${_tag}/sha256.sum"

    say "Downloading ${_archive}..."
    download_to_file "$_archive_url" "$_tmpdir/$_archive" ||
        err "failed to download ${_archive_url}
  Ensure v${_version} is a valid release with a binary for ${_target}.
  Available releases: https://github.com/${GITHUB_REPO}/releases"

    download_to_file "$_checksum_url" "$_tmpdir/sha256.sum" ||
        err "failed to download checksums from ${_checksum_url}"

    # Verify checksum
    verify_checksum "$_tmpdir" "$_archive"

    # Pre-scan archive for path-traversal and symlink-escape entries before extraction.
    # Rejects: absolute paths (leading /), parent-component (..) traversal, and
    # symlinks whose target resolves outside the extraction directory.
    # Two-pass scan:
    #   Pass 1 (entry names): catches absolute paths and ".." traversal in entry names.
    #   Pass 2 (symlink targets): tar -tv lists "link -> target"; awk extracts the
    #     target (last field after "->") and rejects absolute or parent-escaping targets.
    #     Without -v, tar --list only emits entry names - symlink targets are invisible.
    local _bad_entry
    _bad_entry=$(tar --list -f "$_tmpdir/$_archive" 2>/dev/null |
        grep -E '(^|/)\.\.(^|/|$)|^/' || true)
    if [ -n "$_bad_entry" ]; then
        printf 'ocx-install: error: archive contains unsafe path entry: %s\n' \
            "$_bad_entry" >&2
        exit 1
    fi
    # Reject symlinks pointing outside the extraction directory.  Catches:
    #   - absolute paths (leading /)
    #   - parent-relative prefix (../...)
    #   - middle-relative escapes (e.g. `subdir/../../etc/passwd`) which the
    #     prior regex `^(\.\.|/)` missed (CWE-22 / CWE-59 - symlink-target
    #     path traversal).  The awk normalizer walks each '/'-split component
    #     and tracks depth: any '..' that would take depth below zero means
    #     the target resolves outside the extraction root.
    # Use field-split on ' -> ' instead of a greedy sub() so that symlink
    # targets containing a literal ' -> ' substring are preserved intact and
    # checked correctly.  Fields $2..$NF are joined back with ' -> ' so the
    # full target string reaches the guard even in the (rare) edge case.
    local _bad_target
    _bad_target=$(tar -tvf "$_tmpdir/$_archive" 2>/dev/null |
        awk -F ' -> ' '
            /->/ {
                target=""
                for (i=2; i<=NF; i++) target = target (i==2 ? "" : " -> ") $i
                # Absolute path - always rejected.
                if (substr(target, 1, 1) == "/") { print target; next }
                # Walk components, track resolved depth from extraction root.
                n = split(target, parts, "/")
                depth = 0
                for (j=1; j<=n; j++) {
                    if (parts[j] == "" || parts[j] == ".") continue
                    if (parts[j] == "..") {
                        depth--
                        if (depth < 0) { print target; next }
                    } else {
                        depth++
                    }
                }
            }
        ' || true)
    if [ -n "$_bad_target" ]; then
        printf 'ocx-install: error: archive contains symlink targeting outside extraction dir: %s\n' \
            "$_bad_target" >&2
        exit 1
    fi

    # Extract with owner/permission isolation to prevent setuid/setgid attacks.
    # --no-overwrite-dir (CWE-59 / CWE-22 defence-in-depth) prevents a
    # malicious archive from replacing permissions/ownership on a pre-existing
    # directory in $_tmpdir. Flag is standard on GNU tar and BSD tar.
    if ! tar xf "$_tmpdir/$_archive" -C "$_tmpdir" \
        --no-same-owner --no-same-permissions --no-overwrite-dir 2>/dev/null; then
        err "failed to extract ${_archive} - ensure tar and xz-utils are installed"
    fi

    # Locate binary - cargo-dist puts it in a target-named subdirectory
    if [ -f "$_tmpdir/ocx-${_target}/ocx" ]; then
        _bin="$_tmpdir/ocx-${_target}/ocx"
    elif [ -f "$_tmpdir/ocx" ]; then
        _bin="$_tmpdir/ocx"
    else
        err "could not find ocx binary in archive"
    fi

    chmod +x "$_bin"

    # Smoke-test the binary before installing - detects noexec /tmp
    if ! "$_bin" version >/dev/null 2>&1; then
        warn "binary failed to execute in temp directory ($(dirname "$_bin"))"
        warn "your /tmp may be mounted with noexec - try: TMPDIR=\$HOME/.tmp $0"
    fi

    # PATH shadowing: warn if a different `ocx` already exists on PATH
    if check_cmd ocx; then
        local _existing_ocx
        _existing_ocx=$(command -v ocx)
        case "$_existing_ocx" in
            "${_ocx_home}"/*) ;; # our own install - expected
            *)
                warn "an existing ocx was found at $_existing_ocx"
                warn "the new install may be shadowed - check your PATH order"
                ;;
        esac
    fi

    # Bootstrap: OCX installs itself into its own package store
    bootstrap_ocx "$_bin" "$_version"
    say "Installed to $(tildify "${_ocx_home}/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx")"

    # Write env files, modify the login profile, export the CI path, and print
    # the banner (shared with the __OCX_TESTING_INSTALL_BINARY test path).
    finalize_install "$_version" "$_old_version" "$_no_modify_path"
}

# Export the OCX bin directory to GITHUB_PATH for GitHub Actions.
export_github_path() {
    local _install_path="${OCX_HOME:-$HOME/.ocx}/symlinks/ocx.sh/ocx/cli/current/content/bin"
    if [ -n "${GITHUB_PATH:-}" ]; then
        printf '%s\n' "$_install_path" >>"$GITHUB_PATH" ||
            warn "failed to write to \$GITHUB_PATH"
    fi
}

main "$@"
