#!/bin/sh
# shellcheck disable=SC3043  # `local` verified at runtime by has_local()
# install.sh — OCX installer for Unix and macOS
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

# Ensure HOME is set — some minimal environments (containers, cron) omit it.
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

# TTY/color detection — bold-only, respects NO_COLOR (https://no-color.org/)
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
OCX installer — https://ocx.sh

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
    GITHUB_TOKEN          GitHub API token — set this if you hit rate limits
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
            say "Detected Apple Silicon running under Rosetta — using native arm64 binary."
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
# break downloads to /tmp — prefer wget if curl is from snap.
detect_downloader() {
    if check_cmd curl; then
        if curl --version 2>&1 | head -1 | grep -qF 'snap'; then
            warn "detected snap-packaged curl (may have sandbox restrictions)"
            if check_cmd wget; then
                _downloader="wget"
                return
            fi
            warn "no wget fallback — continuing with snap curl"
        fi
        _downloader="curl"
    elif check_cmd wget; then
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
        wget -q -O "$_dest" "$_url"
    fi
}

# Download URL to stdout
download() {
    if [ "$_downloader" = "curl" ]; then
        curl --proto '=https' --tlsv1.2 -fsSL "$1"
    else
        wget -qO- "$1"
    fi
}

# Download GitHub API URL to stdout — uses GITHUB_TOKEN when set
download_api() {
    local _url="$1"

    if [ -n "${GITHUB_TOKEN:-}" ]; then
        if [ "$_downloader" = "curl" ]; then
            curl --proto '=https' --tlsv1.2 -fsSL -H "Authorization: token ${GITHUB_TOKEN}" "$_url"
        else
            wget -q --header="Authorization: token ${GITHUB_TOKEN}" -O- "$_url"
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
        warn "neither sha256sum nor shasum found — SKIPPING CHECKSUM VERIFICATION"
        warn "install coreutils or set PATH to include sha256sum for verified downloads"
        return 0
    fi

    _expected=$(grep -F "$_file" "$_dir/sha256.sum" | awk '{print $1}')
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
            err "failed to fetch latest release from GitHub — check your internet connection and token"
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

# Write $OCX_HOME/env.sh — POSIX fail-safe form.
# Resolves the ocx binary via the LITERAL install root embedded at install time
# (not a $OCX_HOME runtime fallback), so activation survives fresh shells where
# OCX_HOME is not exported.  The [ -x ] guard + || true ensure a broken/absent
# global ocx.toml never aborts a shell login (nvm fail-safe pattern).
# A same-session re-eval guard (_OCX_ENV_LOADED) prevents duplicate PATH entries
# when a profile is re-sourced mid-session.
create_env_sh() {
    local _ocx_home="${OCX_HOME:-$HOME/.ocx}"

    mkdir -p "$_ocx_home"

    # Write the literal install root so the file works in fresh shells where
    # OCX_HOME is not set.  Single-quoted printf format strings intentionally
    # contain shell variable syntax ($) that must appear verbatim in the
    # generated file — SC2016 is correct to flag them; suppressed here.
    # shellcheck disable=SC2016
    {
        printf '#!/bin/sh\n'
        printf '# Managed by ocx installer — do not edit.\n'
        printf '# Sources the global toolchain env so tools declared in %s/ocx.toml\n' \
            "$_ocx_home"
        printf '# are on PATH for every login shell.\n'
        printf '# Same-session guard: skip if already applied this shell session.\n'
        printf 'if [ -n "${_OCX_ENV_LOADED:-}" ]; then return 0 2>/dev/null || true; fi\n'
        printf 'export _OCX_ENV_LOADED=1\n'
        printf 'export OCX_HOME="%s"\n' "$_ocx_home"
        printf '_ocx_bin="%s/symlinks/ocx.sh/ocx/cli/current/bin/ocx"\n' "$_ocx_home"
        printf 'if [ -x "$_ocx_bin" ]; then\n'
        printf '    eval "$("$_ocx_bin" --global env --shell=sh 2>/dev/null)" || true\n'
        printf 'fi\n'
        printf 'unset _ocx_bin\n'
    } >"$_ocx_home/env.sh"
}

# Write $OCX_HOME/env.fish — fish-syntax per-family file.
# Fish cannot use the POSIX eval form; pipe-to-source is the idiomatic
# equivalent in fish 4.x.  The binary path is the resolved install root
# embedded literally at install time (not a runtime OCX_HOME fallback).
create_env_fish() {
    local _ocx_home="${OCX_HOME:-$HOME/.ocx}"

    mkdir -p "$_ocx_home"

    # Write the literal install root into the file so it works in fresh shells
    # where OCX_HOME is not set.  SC2016 suppressed: single-quoted $ syntax is
    # intentional — fish variable references must appear verbatim in the file.
    # shellcheck disable=SC2016
    {
        printf '# Managed by ocx installer — do not edit.\n'
        printf 'set -l _ocx_bin "%s/symlinks/ocx.sh/ocx/cli/current/bin/ocx"\n' "$_ocx_home"
        printf 'if test -x "$_ocx_bin"\n'
        printf '    set -x OCX_HOME "%s"\n' "$_ocx_home"
        printf '    "$_ocx_bin" --global env --shell=fish 2>/dev/null | source\n'
        printf 'end\n'
    } >"$_ocx_home/env.fish"
}

# Write $OCX_HOME/env.ps1 — PowerShell per-family file.
# $PROFILE is resolved at runtime by the PowerShell profile, never hardcoded
# here. The binary path is the resolved install root embedded literally at
# install time (not a runtime $env:OCX_HOME fallback).
create_env_ps1() {
    local _ocx_home="${OCX_HOME:-$HOME/.ocx}"

    mkdir -p "$_ocx_home"

    # Write the literal install root so the file works in fresh shells where
    # OCX_HOME is not set.  SC2016 suppressed: single-quoted PowerShell $
    # variable references must appear verbatim in the generated file.
    # shellcheck disable=SC2016
    {
        printf '# Managed by ocx installer — do not edit.\n'
        printf '$env:OCX_HOME = "%s"\n' "$_ocx_home"
        printf '$_ocxBin = "%s/symlinks/ocx.sh/ocx/cli/current/bin/ocx"\n' "$_ocx_home"
        printf 'if (Test-Path $_ocxBin -PathType Leaf) {\n'
        printf '    Invoke-Expression ((& $_ocxBin --global env --shell=pwsh 2>$null) | Out-String)\n'
        printf '}\n'
    } >"$_ocx_home/env.ps1"
}

# Compatibility alias: the old env file had no .sh extension.  Remove it on
# install so upgraders don't source a stale file that calls deleted commands.
remove_legacy_env_file() {
    local _ocx_home="${OCX_HOME:-$HOME/.ocx}"
    local _old="$_ocx_home/env"
    if [ -f "$_old" ] && ! [ -L "$_old" ]; then
        rm -f -- "$_old"
    fi
}

create_fish_config() {
    local _fish_conf_dir

    _fish_conf_dir="${XDG_CONFIG_HOME:-$HOME/.config}/fish/conf.d"
    mkdir -p "$_fish_conf_dir"

    cat >"$_fish_conf_dir/ocx.fish" <<'FISHEOF'
# OCX shell environment — managed by ocx installer.
# Sources $OCX_HOME/env.fish which evaluates the global toolchain env.
set -l _ocx_env (string join '' (set -q OCX_HOME; and echo $OCX_HOME; or echo $HOME/.ocx) '/env.fish')
if test -f "$_ocx_env"
    source "$_ocx_env"
end
FISHEOF
}

# --- Legacy profile line migration (W6) ---

# Detect and remove any stale `. "$OCX_HOME/init.<shell>"` lines written by
# the deleted `ocx shell init` command.  These differ from the new block-marker
# block so the awk uninstall path won't catch them.  Applies to both install
# and uninstall so upgraders never keep a dangling broken source line.
remove_legacy_init_lines() {
    local _profile="$1" _ocx_home _tmpfile

    if [ -z "$_profile" ] || ! [ -f "$_profile" ]; then
        return
    fi

    _ocx_home="${OCX_HOME:-$HOME/.ocx}"

    # Match lines written by the deleted `ocx shell init` command.  Those lines
    # had the form:  . "$OCX_HOME/init.bash"  (or init.zsh, init.fish, etc.)
    # Detection anchors to actual dot-source command form (leading whitespace +
    # a dot/period command) so benign user comments containing ".ocx/init." are
    # never treated as legacy source lines (CWE-73 defense).
    if grep -qF "${_ocx_home}/init." "$_profile" 2>/dev/null ||
        grep -qE '^[[:space:]]*\. .*\.ocx/init\.' "$_profile" 2>/dev/null; then
        _tmpfile=$(mktemp)
        # Delete only actual dot-source lines referencing .ocx/init.<word>.
        # Anchored to leading whitespace + dot-command to avoid nuking comments.
        grep -vE '^[[:space:]]*\. .*\.ocx/init\.' "$_profile" >"$_tmpfile" || true
        mv -- "$_tmpfile" "$_profile"
        say "Removed legacy ocx shell init line from $(tildify "$_profile")"
    fi
}

# --- Shell profile modification ---

# Profile target decision tree (login-shell scope, per research §84-89):
#   zsh  → ${ZDOTDIR:-$HOME}/.zprofile  (macOS Terminal opens login by default)
#   bash → ~/.bash_profile if exists else ~/.profile
#   fish → ~/.config/fish/conf.d (managed via conf.d — no block needed here)
#   *    → ~/.profile
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
            ;;
        zsh)
            # .zprofile: sourced for login shells (macOS Terminal default).
            # Respects ZDOTDIR when set.  Reject ZDOTDIR="/" to prevent writing
            # /.zprofile (CWE-22 defense — filesystem root write guard).
            _zdotdir="${ZDOTDIR:-$HOME}"
            if [ "$_zdotdir" = "/" ]; then
                warn "ZDOTDIR is '/' — refusing to write /.zprofile; falling back to ~/.zprofile"
                _zdotdir="$HOME"
            fi
            echo "$_zdotdir/.zprofile"
            ;;
        fish)
            # Fish uses conf.d — no block-marker profile edit needed
            echo ""
            ;;
        *)
            echo "$HOME/.profile"
            ;;
    esac
}

# Append a block-marker idempotent section to the login profile.
# Pattern: conda-style # BEGIN ocx / # END ocx block.
# - Idempotent: grep -qF the BEGIN marker before append (never double-appends).
# - Dot (.) not source: POSIX dash-safe.
# - Legacy detection: remove old $OCX_HOME/init.* source lines first (W6).
modify_shell_profile() {
    local _profile _ocx_home _shell_name

    _ocx_home="${OCX_HOME:-$HOME/.ocx}"
    _shell_name=$(basename "${SHELL:-sh}")

    # Fish: conf.d config handles activation; no block-marker profile edit.
    if [ "$_shell_name" = "fish" ]; then
        create_fish_config
        say "Created Fish configuration."
        return
    fi

    _profile=$(detect_profile)
    if [ -z "$_profile" ]; then
        return
    fi

    # W6: strip any legacy `ocx shell init`-written lines before inserting block.
    remove_legacy_init_lines "$_profile"

    # Idempotent: skip if block already present.
    if grep -qF "# BEGIN ocx" "$_profile" 2>/dev/null; then
        say "Shell profile already configured ($(tildify "$_profile"))."
        return
    fi

    # Append the block-marker section.  Uses . (dot) not source — POSIX dash-safe.
    # The install root is embedded as a literal resolved path (not a runtime
    # ${OCX_HOME:-...} fallback) so the block works in fresh shells where
    # OCX_HOME is not exported.
    printf '\n# BEGIN ocx\n. "%s/env.sh"\n# END ocx\n' \
        "$_ocx_home" >>"$_profile"
    say "Added OCX to $(tildify "$_profile")"
}

# Remove the block-marker section from the profile (uninstall path).
# Uses POSIX awk — avoids non-portable `sed -i`.
# Also strips legacy $OCX_HOME/init.* lines (W6).
remove_shell_profile() {
    local _profile _ocx_home _tmpfile _shell_name

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

    _profile=$(detect_profile)
    if [ -z "$_profile" ] || ! [ -f "$_profile" ]; then
        return
    fi

    # W6: strip legacy init.* lines first.
    remove_legacy_init_lines "$_profile"

    # Remove the # BEGIN ocx … # END ocx block.
    # Correct block-strip form: BEGIN marker sets p=1 and is itself skipped;
    # END marker resets p=0 and is itself skipped; only non-suppressed (!p)
    # lines are printed.  This ensures both markers AND all interior lines are
    # removed and the END marker correctly resets the flag.
    if grep -qF "# BEGIN ocx" "$_profile" 2>/dev/null; then
        _tmpfile=$(mktemp)
        awk '/^# BEGIN ocx/{p=1;next} /^# END ocx/{p=0;next} !p{print}' \
            "$_profile" >"$_tmpfile" && mv -- "$_tmpfile" "$_profile"
        say "Removed OCX from $(tildify "$_profile")"
    fi
}

# --- Bootstrap: OCX installs itself ---

bootstrap_ocx() {
    local _bin="$1" _version="$2"

    say "Bootstrapping OCX into its own package store..."
    if ! "$_bin" --remote install --select "ocx.sh/ocx/cli:$_version"; then
        err "bootstrap failed: 'ocx --remote install --select ocx.sh/ocx/cli:$_version'
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
        /*) ;; # absolute path — OK
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
        *'"'* | *'$'* | *'`'* | *';'* | *'&'* | *'|'* | *'
'*) err "OCX_HOME contains characters unsafe for shell embedding: $_ocx_home" ;;
    esac

    _target=$(detect_target)
    say "Detected platform: $_target"

    # Resolve version
    if [ -z "$_version" ]; then
        say "Fetching latest version..."
        _version=$(get_latest_version)
    fi

    # Validate version format — reject shell metacharacters and suspicious input
    if echo "$_version" | grep -q '[^0-9a-zA-Z.+-]'; then
        err "invalid version format: $_version (expected semver like 1.2.3 or 1.0.0-rc.1)"
    elif echo "$_version" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]'; then
        : # valid
    else
        err "invalid version format: $_version (expected semver like 1.2.3)"
    fi

    # Detect existing installation for upgrade messaging
    _old_version=""
    _bin_path="${_ocx_home}/symlinks/ocx.sh/ocx/cli/current/bin/ocx"
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

    # Extract archive
    if ! tar xf "$_tmpdir/$_archive" -C "$_tmpdir" 2>/dev/null; then
        err "failed to extract ${_archive} — ensure tar and xz-utils are installed"
    fi

    # Locate binary — cargo-dist puts it in a target-named subdirectory
    if [ -f "$_tmpdir/ocx-${_target}/ocx" ]; then
        _bin="$_tmpdir/ocx-${_target}/ocx"
    elif [ -f "$_tmpdir/ocx" ]; then
        _bin="$_tmpdir/ocx"
    else
        err "could not find ocx binary in archive"
    fi

    chmod +x "$_bin"

    # Smoke-test the binary before installing — detects noexec /tmp
    if ! "$_bin" version >/dev/null 2>&1; then
        warn "binary failed to execute in temp directory ($(dirname "$_bin"))"
        warn "your /tmp may be mounted with noexec — try: TMPDIR=\$HOME/.tmp $0"
    fi

    # PATH shadowing: warn if a different `ocx` already exists on PATH
    if check_cmd ocx; then
        local _existing_ocx
        _existing_ocx=$(command -v ocx)
        case "$_existing_ocx" in
            "${_ocx_home}"/*) ;; # our own install — expected
            *)
                warn "an existing ocx was found at $_existing_ocx"
                warn "the new install may be shadowed — check your PATH order"
                ;;
        esac
    fi

    # Bootstrap: OCX installs itself into its own package store
    bootstrap_ocx "$_bin" "$_version"
    say "Installed to $(tildify "${_ocx_home}/symlinks/ocx.sh/ocx/cli/current/bin/ocx")"

    # Create shell environment files (POSIX + per-family variants).
    # remove_legacy_env_file strips the old extensionless $OCX_HOME/env file
    # written by older installers so upgraders don't source a stale file.
    remove_legacy_env_file
    create_env_sh
    create_env_fish
    create_env_ps1

    # Create Fish conf.d config if Fish is installed (regardless of default shell).
    if check_cmd fish; then
        create_fish_config
    fi

    # Modify shell profile
    if [ "$_no_modify_path" = "1" ]; then
        say "Skipping shell profile modification (--no-modify-path)."
    else
        modify_shell_profile
    fi

    # Export GitHub Actions path if in CI
    export_github_path

    print_success "$_version" "$_old_version"
}

# Export the OCX bin directory to GITHUB_PATH for GitHub Actions.
export_github_path() {
    local _install_path="${OCX_HOME:-$HOME/.ocx}/symlinks/ocx.sh/ocx/cli/current/bin"
    if [ -n "${GITHUB_PATH:-}" ]; then
        printf '%s\n' "$_install_path" >>"$GITHUB_PATH" ||
            warn "failed to write to \$GITHUB_PATH"
    fi
}

main "$@"
