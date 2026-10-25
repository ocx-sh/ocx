#!/bin/sh
# toolx installer. Template: rename toolx and TOOLX_, set BASE_URL, then run the kit (see the skill).
#
#   curl --proto '=https' --tlsv1.2 -fsSL https://downloads.example.invalid/install.sh | sh -s -- [options]
#
# Floor: POSIX sh (dash 0.5.12, busybox ash 1.37, bash 3.2 --posix). ASCII only.
# Stdout is silent except TOOLX_INSTALL_SH_PRINT=target (prints the target triple).
# Integrity: SHA-256 from a same-origin sidecar, which detects corruption and not a hostile origin.
#   TOOLX_INSTALL_SHA256 pins the digest and makes it a trust root.
# Exit: 0 ok; 1 generic; 2 usage/environment (including a missing tool);
#       3 network; 4 checksum mismatch or unverifiable; 5 extract; 6 setup;
#       7 unsupported platform; 129/130/143 HUP/INT/TERM.
#
# Options: -y, --yes  --no-modify-path  -h, --help
# Environment: TOOLX_INSTALL_DIR (default $HOME/.local/bin), TOOLX_INSTALL_VERSION,
#   TOOLX_INSTALL_BASE_URL, TOOLX_INSTALL_TARGET (skip detection), TOOLX_INSTALL_SHA256 (pinned digest, overrides the sidecar),
#   TOOLX_NO_MODIFY_PATH, NONINTERACTIVE.

set -u

APP=toolx
VERSION=${TOOLX_INSTALL_VERSION:-1.2.3}
BASE_URL=${TOOLX_INSTALL_BASE_URL:-https://downloads.example.invalid/toolx}
tmp=
YES=${NONINTERACTIVE:+1}
NO_MODIFY=${TOOLX_NO_MODIFY_PATH:+1}
DL=
CURL_TLS=0
WGET_TLS=0
RETVAL=
NOVALID=

say() { printf '%s\n' "$APP-install: $*" >&2; }
warn() { printf '%s\n' "$APP-install: warning: $*" >&2; }
err() {
    printf '%s\n' "$APP-install: error: $1" >&2
    exit "${2:-1}"
}
check_cmd() { command -v "$1" >/dev/null 2>&1; }
need_cmd() { check_cmd "$1" || err "need '$1' (command not found)" 2; }

cleanup() { [ -z "$tmp" ] || rm -rf "$tmp"; }
on_exit() {
    rc=$?
    trap - EXIT
    cleanup
    exit "$rc"
}

usage() {
    cat >&2 <<'EOF'
usage: install.sh [-y|--yes] [--no-modify-path] [-h|--help]
EOF
}

parse_args() {
    while [ $# -gt 0 ]; do
        case $1 in
            -y | --yes) YES=1 ;;
            --no-modify-path) NO_MODIFY=1 ;;
            -h | --help)
                usage
                exit 0
                ;;
            *)
                usage
                err "unknown option: $1" 2
                ;;
        esac
        shift
    done
}

# --- platform -----------------------------------------------------------------
# Sets RETVAL to the target triple. No default arm: anything unknown exits 7.
detect_target() {
    if [ -n "${TOOLX_INSTALL_TARGET:-}" ]; then
        RETVAL=$TOOLX_INSTALL_TARGET
        return 0
    fi
    _os=$(uname -s) || err "uname -s failed" 7
    case $_os in
        Linux | Darwin) ;;
        *) err "unsupported operating system: $_os (supported: Linux, Darwin)" 7 ;;
    esac
    _m=$(uname -m) || err "uname -m failed" 7
    case $_m in
        x86_64 | amd64) _arch=x86_64 ;;
        aarch64 | arm64) _arch=aarch64 ;;
        *) err "unsupported architecture: $_m (supported: x86_64, aarch64)" 7 ;;
    esac
    if [ "$_os" = Darwin ] && [ "$_arch" = x86_64 ]; then
        # Rosetta: compare the value; no pipe, no '||' precedence trap.
        if [ "$(sysctl -n hw.optional.arm64 2>/dev/null || true)" = 1 ]; then
            _arch=aarch64
        fi
    fi
    if [ "$_os" = Darwin ]; then
        RETVAL=$_arch-apple-darwin
        return 0
    fi
    # Three outcomes, never a silent default: ldd names its libc when present (musl's 'ldd --version' prints
    # to stderr and exits 1); else the loader files; else /etc/alpine-release; else exit 7 (static busybox images).
    _libc=
    if check_cmd ldd; then
        case $(ldd --version 2>&1 || true) in
            *musl*) _libc=musl ;;
            *GLIBC* | *"GNU libc"*) _libc=gnu ;;
        esac
    fi
    if [ -z "$_libc" ]; then
        for _f in /lib/ld-musl-*.so.1; do
            if [ -e "$_f" ]; then _libc=musl; fi
        done
        if [ -z "$_libc" ] && [ -f /etc/alpine-release ]; then _libc=musl; fi
    fi
    if [ -z "$_libc" ]; then
        for _f in /lib64/ld-linux-*.so.* /lib/ld-linux-*.so.* /lib/*-linux-gnu/ld-linux-*.so.*; do
            if [ -e "$_f" ]; then _libc=gnu; fi
        done
    fi
    [ -n "$_libc" ] || err "cannot tell glibc from musl (no ldd, no loader file); set TOOLX_INSTALL_TARGET" 7
    RETVAL=$_arch-unknown-linux-$_libc
}

# --- download -----------------------------------------------------------------
# Probe the downloader's flags before using them. 'curl --help' lists only the basic
# category on curl >= 7.73; the TLS flags are under 'curl --help all'.
pick_downloader() {
    if check_cmd curl; then
        DL=curl
        _help=$(curl --help all 2>&1 || true)
        case $_help in
            *--tlsv1.2*) CURL_TLS=1 ;;
            *) warn "curl does not advertise --tlsv1.2; TLS version not enforced" ;;
        esac
    elif check_cmd wget; then
        DL=wget
        _help=$(wget --help 2>&1 || true)
        case $_help in
            *--https-only*) WGET_TLS=1 ;;
            *BusyBox*) warn "BusyBox wget: TLS floor and certificate checking depend on this build; prefer curl, or pin TOOLX_INSTALL_SHA256" ;;
            *) warn "wget does not advertise --https-only; https URLs only" ;;
        esac
    else
        err "need curl or wget" 2
    fi
}

download_once() {
    case $1 in
        https://*) ;;
        *) err "refusing non-https URL: $1" 3 ;;
    esac
    if [ "$DL" = curl ]; then
        if [ "$CURL_TLS" = 1 ]; then
            curl --proto '=https' --tlsv1.2 --fail --silent --show-error --location --retry 3 --retry-delay 1 -o "$2" "$1"
        else
            curl --proto '=https' --fail --silent --show-error --location --retry 3 --retry-delay 1 -o "$2" "$1"
        fi
    else
        # BusyBox wget builds with internal TLS print a note instead of validating the certificate.
        if [ "$WGET_TLS" = 1 ]; then
            wget --https-only -q -O "$2" "$1" 2>"$tmp/wget.err" || {
                cat "$tmp/wget.err" >&2
                return 1
            }
        else
            wget -q -O "$2" "$1" 2>"$tmp/wget.err" || {
                cat "$tmp/wget.err" >&2
                return 1
            }
        fi
        if grep -q 'certificate validation not implemented' "$tmp/wget.err"; then NOVALID=1; fi
    fi
}

download() {
    _n=1
    while :; do
        download_once "$1" "$2" && return 0
        if [ "$_n" -ge 3 ]; then err "download failed: $1" 3; fi
        say "download failed (attempt $_n/3), retrying"
        sleep 1
        _n=$((_n + 1))
    done
}

# --- integrity ----------------------------------------------------------------
# Sets RETVAL (no subshell, so err really exits the installer).
sha256_of() {
    if check_cmd sha256sum; then
        RETVAL=$(sha256sum "$1" | awk '{print tolower($1)}')
    elif check_cmd shasum; then
        RETVAL=$(shasum -a 256 "$1" | awk '{print tolower($1)}')
    elif check_cmd openssl; then
        RETVAL=$(openssl dgst -sha256 "$1" | awk '{print tolower($NF)}')
    else
        err "no sha256 tool (sha256sum, shasum or openssl); cannot verify $1" 4
    fi
}

verify_payload() {
    if [ -n "${TOOLX_INSTALL_SHA256:-}" ]; then
        _want=$(printf '%s' "$TOOLX_INSTALL_SHA256" | tr 'A-F' 'a-f')
        say "integrity: pinned digest from the environment"
    else
        if [ -n "$NOVALID" ]; then
            err "wget did not validate the server certificate, so a same-origin checksum proves nothing: install curl or set TOOLX_INSTALL_SHA256" 4
        fi
        download "$BASE_URL/$2.sha256" "$tmp/$2.sha256"
        _want=$(awk '{print tolower($1)}' "$tmp/$2.sha256")
        say "integrity: same-origin sidecar (detects corruption, not a hostile origin)"
    fi
    [ -n "$_want" ] || err "empty expected digest for $2" 4
    sha256_of "$1"
    _got=$RETVAL
    [ "$_want" = "$_got" ] || err "checksum mismatch for $2: expected $_want, got $_got" 4
    say "checksum verified"
}

# --- PATH edit ----------------------------------------------------------------
rc_block() {
    printf '%s\n' "# >>> $APP >>>" "export PATH=\"$1:\$PATH\"" "# <<< $APP <<<"
}

edit_rc() {
    # In place (cat >), so symlinks and modes survive; written only when content changes.
    _rc=$1
    _new="$tmp/rc.new"
    {
        if [ -f "$_rc" ]; then
            awk -v o="# >>> $APP >>>" -v c="# <<< $APP <<<" \
                '$0 == o { skip = 1; next } $0 == c { skip = 0; next } !skip { print }' "$_rc"
        fi
        rc_block "$2"
    } >"$_new"
    if [ -f "$_rc" ] && cmp -s "$_rc" "$_new"; then
        return 0
    fi
    cat "$_new" >"$_rc"
    say "updated $_rc"
}

# Sets RETVAL to the one file the login shell reads for PATH. Returns 1 for a shell this installer does not edit.
login_rc() {
    case ${SHELL:-} in
        */zsh)
            case ${ZDOTDIR:-} in
                /* | '') ;;
                *) ZDOTDIR= ;;
            esac
            case ${ZDOTDIR:-} in *..*) ZDOTDIR= ;; esac
            RETVAL=${ZDOTDIR:-$HOME}/.zprofile
            ;;
        */bash)
            RETVAL=$HOME/.profile
            for _f in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile"; do
                if [ -f "$_f" ]; then
                    RETVAL=$_f
                    break
                fi
            done
            ;;
        */fish | */nu | */elvish) return 1 ;;
        *) RETVAL=$HOME/.profile ;;
    esac
}

modify_path() {
    [ -z "$NO_MODIFY" ] || return 0
    case ":$PATH:" in *":$1:"*) return 0 ;; esac
    if login_rc; then
        edit_rc "$RETVAL" "$1"
        say "restart your shell or log in again to pick up PATH"
    else
        say "add $1 to PATH for your shell ($SHELL) yourself"
    fi
}

# Prompts read /dev/tty, never stdin (stdin is the script when piped to sh).
confirm_path_edit() {
    [ -z "$YES" ] || return 0
    [ -t 1 ] || return 0
    if (: </dev/tty) 2>/dev/null; then
        printf '%s' "Add $1 to PATH in your shell profile? [Y/n] " >&2
        read -r _ans </dev/tty || return 0
        case $_ans in n | N | no | NO) NO_MODIFY=1 ;; esac
    fi
}

# --- main ---------------------------------------------------------------------
main() {
    trap on_exit EXIT
    trap 'exit 129' HUP
    trap 'exit 130' INT
    trap 'exit 143' TERM

    parse_args ${1+"$@"}
    [ -n "${HOME:-}" ] || err "HOME is not set" 2
    detect_target
    _target=$RETVAL
    if [ "${TOOLX_INSTALL_SH_PRINT:-}" = target ]; then
        printf '%s\n' "$_target"
        return 0
    fi

    need_cmd tar
    need_cmd mktemp
    pick_downloader
    tmp=$(mktemp -d) || err "mktemp failed" 2

    _asset="$APP-$VERSION-$_target.tar.gz"
    download "$BASE_URL/$_asset" "$tmp/$_asset"
    verify_payload "$tmp/$_asset" "$_asset"

    mkdir "$tmp/x" || err "mkdir failed" 5
    tar -xzf "$tmp/$_asset" -C "$tmp/x" || err "extract failed: $_asset" 5
    [ -f "$tmp/x/$APP" ] || err "archive has no $APP binary" 5

    _dir=${TOOLX_INSTALL_DIR:-$HOME/.local/bin}
    mkdir -p "$_dir" || err "cannot create $_dir" 6
    cp "$tmp/x/$APP" "$_dir/$APP.new" || err "cannot write $_dir" 6
    chmod 755 "$_dir/$APP.new"
    mv -f "$_dir/$APP.new" "$_dir/$APP" || err "cannot install into $_dir" 6
    say "installed $APP $VERSION to $_dir/$APP"

    confirm_path_edit "$_dir"
    modify_path "$_dir"
}

main ${1+"$@"}
