#!/bin/sh
# ascii-check.sh FILE... : exit 1 when any FILE holds a byte outside printable ASCII, tab, CR and LF (a BOM, a curly
# quote, an em dash in help text). Windows PowerShell 5.1 reads a BOM-less file in the ANSI code page and turns one
# curly quote into a statement delimiter, so a generated PowerShell completion script must pass. Portable: grep in the
# C locale, no GNU options.
[ "$#" -gt 0 ] || {
    echo "usage: ascii-check.sh FILE..." >&2
    exit 64
}
rc=0
for f in "$@"; do
    if LC_ALL=C grep -a -n -e '[^[:print:][:space:]]' -- "$f" >/dev/null 2>&1; then
        echo "non-ASCII byte in $f, first at line $(LC_ALL=C grep -a -n -e '[^[:print:][:space:]]' -- "$f" | head -n 1 | cut -d: -f1)"
        rc=1
    fi
done
exit $rc
