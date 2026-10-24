#!/bin/sh
# checks/ps-enc-bytes.sh: PS-ENC-01, PS-ENC-02, PS-ENC-08 and PS-ENC-11, the byte-level encoding scans for PowerShell sources.
# Usage: sh ps-enc-bytes.sh MODE [PATHSPEC...]   (run from the repository root; PATHSPEC are git pathspecs that replace the default)
#   nonascii    PS-ENC-01   a file with a byte above 0x7F and no UTF-8 BOM (default pathspecs *.ps1 *.psm1 *.psd1)
#   bom         PS-ENC-02   a UTF-8 BOM in front of a #! line (default *.ps1 *.psm1 *.psd1)
#   bom-served  PS-ENC-02   any UTF-8 BOM, for files an irm | iex line fetches (pathspecs required)
#   quote       PS-ENC-08   U+2018..U+201E before any # on a line, a code position (default *.ps1 *.psm1); help blocks and here-strings are hits to read
#   dash        PS-ENC-11   U+2013 or U+2014 after a blank and before a letter in a file without a BOM (default *.ps1 *.psm1)
# Output: FILE:LINE: message, one per finding. Exit: 0 clean, 1 a finding, 64 usage, 69 a required tool is missing, 74 the scan itself failed,
# 78 not a usable git work tree, 79 no files in scope.
# POSIX awk under LC_ALL=C (bytes, not characters), no PCRE. Ceiling: a # inside a string before the character hides a quote hit.
# Watched red and green on gawk 5.3.2, mawk 1.3.4 and busybox awk 1.37, the script on dash 0.5.12, busybox ash 1.37 and bash 3.2 --posix (measured 2026-10-11).
# shellcheck disable=SC2016 # the single-quoted sh -c and awk programs are literal text by design
set -u
LC_ALL=C
export LC_ALL
mode=${1:-}
case $mode in
    nonascii | bom | quote | dash | bom-served) shift ;;
    *)
        echo 'usage: ps-enc-bytes.sh nonascii|bom|bom-served|quote|dash [PATHSPEC...]' >&2
        exit 64
        ;;
esac
if [ "$#" -eq 0 ]; then
    case $mode in
        bom-served)
            echo 'ps-enc-bytes: bom-served needs the pathspecs of the iex-served files' >&2
            exit 64
            ;;
        nonascii | bom) set -- '*.ps1' '*.psm1' '*.psd1' ;;
        *) set -- '*.ps1' '*.psm1' ;;
    esac
fi
for t in git xargs awk mktemp; do # vd-ok: xargs here is a tool name, not an invocation
    command -v "$t" >/dev/null 2>&1 || {
        echo "ps-enc-bytes: '$t' not on PATH" >&2
        exit 69
    }
done
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || {
    echo "ps-enc-bytes: not a usable git work tree" >&2
    exit 78
}
all=$(mktemp) || exit 74
list=$(mktemp) || exit 74
out=$(mktemp) || exit 74
err=$(mktemp) || exit 74
trap 'rm -f "$all" "$list" "$out" "$err"' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
git ls-files -z --cached --others --exclude-standard -- "$@" >"$all" || {
    echo "ps-enc-bytes: git ls-files failed" >&2
    exit 74
}
# a tracked file deleted from the work tree is not a finding and must not reach awk
xargs -0 -r sh -c 'for f; do if [ -f "$f" ]; then printf "%s\0" "$f"; fi; done' _ <"$all" >"$list" || exit 74
[ -s "$list" ] || {
    echo "ps-enc-bytes: no files in scope" >&2
    exit 79
}
PROG='
BEGIN {
    BOM = sprintf("%c%c%c", 239, 187, 191)
    E2 = sprintf("%c%c", 226, 128)
    HI = sprintf("[%c-%c]", 128, 255)
    QUOTES = sprintf("%c%c%c%c%c%c%c", 152, 153, 154, 155, 156, 157, 158)
    DASHES = sprintf("%c%c", 147, 148)
}
FNR == 1 { bom = (substr($0, 1, 3) == BOM); flagged = 0 }
{
    line = $0
    if (FNR == 1 && bom) line = substr(line, 4)
}
MODE == "nonascii" && !bom && !flagged && line ~ HI { print FILENAME ":" FNR ": non-ASCII byte and no BOM"; flagged = 1; found = 1 }
MODE == "bom" && FNR == 1 && bom && substr(line, 1, 2) == "#!" { print FILENAME ":1: BOM before the shebang"; found = 1 }
MODE == "bom-served" && FNR == 1 && bom { print FILENAME ":1: BOM on an irm | iex file"; found = 1 }
MODE == "quote" || (MODE == "dash" && !bom) {
    hash = index(line, "#"); if (hash == 0) hash = length(line) + 1
    s = line; off = 0
    while ((i = index(s, E2)) > 0) {
        pos = off + i; b = substr(line, pos + 2, 1)
        if (MODE == "quote" && pos < hash && index(QUOTES, b) > 0) { print FILENAME ":" FNR ": curly quote in a code position"; found = 1; break }
        if (MODE == "dash" && index(DASHES, b) > 0 && substr(line, pos - 1, 1) ~ /[ \t]/ && substr(line, pos + 3, 1) ~ /[A-Za-z]/) { print FILENAME ":" FNR ": en or em dash before a letter, BOM-less"; found = 1; break }
        off = pos; s = substr(s, i + 1)
    }
}
END { exit found ? 1 : 0 }
'
xargs -0 -r awk -v MODE="$mode" -- "$PROG" <"$list" >"$out" 2>"$err"
rc=$?
if [ -s "$err" ]; then
    cat "$err" >&2
    exit 74
fi
[ "$rc" -le 1 ] || [ "$rc" -eq 123 ] || exit 74 # 123: some batch exited 1, which is a finding
if [ -s "$out" ]; then
    cat "$out"
    exit 1
fi
exit 0
