#!/bin/sh
# shellcheck disable=SC3043  # `local` verified at runtime by has_local()
# install.sh - OCX installer for Unix and macOS
# https://ocx.sh
#
# This is a thin bootstrap: it detects the platform, downloads the published
# release archive, verifies its checksum, and then hands off to the downloaded
# binary's `ocx self setup`. `ocx self setup` owns everything that touches the
# user's machine - the self-install into the package store, the per-shell env
# shims under $OCX_HOME, and the managed shell-profile activation block. Run
# `ocx self setup --help` for the full setup contract.
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

# --- Usage ---

usage() {
    cat <<'EOF'
OCX installer - https://ocx.sh

Bootstraps ocx: downloads the release, verifies it, and hands off to
`ocx self setup` for the package-store install + shell integration.

USAGE:
    curl -fsSL https://ocx.sh/install.sh | sh
    curl -fsSL https://ocx.sh/install.sh | sh -s -- [OPTIONS]

OPTIONS:
    --version <VERSION>   Install a specific version (e.g., 0.5.0)
    --no-modify-path      Don't modify shell profile files (forwarded to
                          `ocx self setup`)
    -h, --help            Print this help message

ENVIRONMENT:
    OCX_HOME              Installation directory (default: ~/.ocx)
    OCX_NO_MODIFY_PATH    Truthy (1/true/yes/on) skips shell profile
                          modification; forwarded to `ocx self setup`
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

# --- Hand off to `ocx self setup` ---

# Run the downloaded binary's `ocx self setup`, which owns the package-store
# self-install, the per-shell env shims under $OCX_HOME, and the managed
# shell-profile activation block. The installer never writes those files
# itself - `ocx self setup` is the single source of truth (see its --help).
#
# Global flags (e.g. --offline) must precede the subcommand, subcommand flags
# (e.g. --no-modify-path) must follow it - clap parses them at different
# levels. The first argument carries the space-separated global pre-flags, the
# second the space-separated `self setup` post-flags; either may be empty.
run_self_setup() {
    # shellcheck disable=SC2086  # deliberate word-split: pre/post are flag lists
    local _bin="$1" _pre="$2" _post="$3"

    say "Running ocx self setup..."
    # shellcheck disable=SC2086  # deliberate word-split of the flag lists
    if ! "$_bin" $_pre self setup $_post; then
        err "'ocx self setup' failed - see the output above for details"
    fi
}

# Export the OCX bin directory to GITHUB_PATH for GitHub Actions.
export_github_path() {
    local _install_path="${OCX_HOME:-$HOME/.ocx}/symlinks/ocx.sh/ocx/cli/current/content/bin"
    if [ -n "${GITHUB_PATH:-}" ]; then
        printf '%s\n' "$_install_path" >>"$GITHUB_PATH" ||
            warn "failed to write to \$GITHUB_PATH"
    fi
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
# exercise real activation/completions from a freshly built binary. After the
# candidate is placed, `ocx self setup --offline` writes the env shims against
# it (offline because no registry is reachable and the candidate is present).
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

# --- Main ---

main() {
    local _no_modify_path _version _target _tmpdir _archive _tag
    local _archive_url _checksum_url _bin _ocx_home

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
        1 | true | yes | on | TRUE | YES | ON) _no_modify_path=1 ;;
        *) _no_modify_path=0 ;;
    esac

    need_cmd uname
    need_cmd mktemp
    need_cmd tar

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
    # Defence-in-depth: $_ocx_home reaches the per-shell env shims and the
    # managed profile block written by `ocx self setup`. Reject shell
    # metacharacters here so a CI-injected OCX_HOME cannot break out of the
    # quoted context downstream.
    case "$_ocx_home" in
        *'"'* | *'$'* | *'`'* | *';'* | *'&'* | *'|'* | *'\'* | *'
'*) err "OCX_HOME contains characters unsafe for shell embedding: $_ocx_home" ;;
    esac

    # Test-mode hatch: install a locally-built binary as the candidate and skip
    # the download + registry bootstrap (and the network version probe) entirely.
    # `ocx self setup --offline` then writes the env shims against that
    # candidate. See install_local_test_binary; __OCX_TESTING_INSTALL_BINARY is
    # test-only.
    if [ -n "${__OCX_TESTING_INSTALL_BINARY:-}" ]; then
        install_local_test_binary "$__OCX_TESTING_INSTALL_BINARY" "$_ocx_home"
        _bin="${_ocx_home}/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx"
        if [ "$_no_modify_path" = "1" ]; then
            run_self_setup "$_bin" "--offline" "--no-modify-path"
        else
            run_self_setup "$_bin" "--offline" ""
        fi
        export_github_path
        return 0
    fi

    detect_downloader

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

    # Hand off to `ocx self setup`: it self-installs the latest published ocx
    # into the package store, writes the per-shell env shims under $OCX_HOME,
    # and (unless --no-modify-path) adds the managed activation block to the
    # login profile. The downloaded archive binary drives its own bootstrap.
    if [ "$_no_modify_path" = "1" ]; then
        run_self_setup "$_bin" "" "--no-modify-path"
    else
        run_self_setup "$_bin" "" ""
    fi

    export_github_path
}

main "$@"
