#!/bin/sh
# checks/secret-argv.sh -- SH-SEC-02 completeness: a secret that reaches the argv of curl, wget, git, docker, gh or cargo
# THROUGH A VARIABLE (h="Authorization: Bearer $T"; curl -H "$h"), which the single-line s02-argv grep cannot see.
# Run from the repository root; optional arguments are git pathspecs (default "."), e.g. . ':(exclude,glob)**/vendor/**'. Output is the violation. Exit map (SH-GATE-31):
#   0 clean   1 finding   69 a required tool is missing   74 the scan itself failed   78 not a usable git work tree   79 no files in scope
# Two passes per file, awk only: pass 1 marks variables assigned from secret sources (to a fixpoint); pass 2 flags their use
# in the argument segment of one of the six tools. Waiver: "# sec-ok: <reason>" on any physical line of the command; a bare "# sec-ok:" is a finding.
# LC_ALL=C: gawk warns on invalid multibyte data in a UTF-8 locale (git t/ fixtures), and the warning would read as a failed scan.
# Ceiling: file-level taint, no flow or function scope, positional $1.. not tracked. A pass is not proof of absence.
# shellcheck disable=SC2016 # the single-quoted sh -c and awk programs are literal text by design
set -u
for t in git xargs awk shfmt mktemp tr; do
    command -v "$t" >/dev/null 2>&1 || {
        echo "secret-argv: '$t' not on PATH" >&2
        exit 69
    }
done
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || {
    echo "secret-argv: not a usable git work tree" >&2
    exit 78
}
all=$(mktemp) || exit 74
list=$(mktemp) || exit 74
sh0=$(mktemp) || exit 74
out=$(mktemp) || exit 74
err=$(mktemp) || exit 74
trap 'rm -f "$all" "$sh0" "$list" "$out" "$err"' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
[ "$#" -gt 0 ] || set -- .
git ls-files -z --cached --others --exclude-standard -- "$@" >"$all" || {
    echo "secret-argv: git ls-files failed" >&2
    exit 74
}
xargs -0 -r sh -c 'for f; do if [ -f "$f" ]; then printf "%s\0" "$f"; fi; done' _ <"$all" | xargs -0 -r shfmt -f -- 2>"$err" | tr '\n' '\0' >"$sh0"
[ -s "$err" ] && {
    cat "$err" >&2
    exit 74
}
git ls-files -z --cached --others --exclude-standard -- "$@" | xargs -0 -r sh -c 'for f; do case $f in *.yml | *.yaml) printf "%s\0" "$f" ;; esac; done' _ >>"$sh0" || exit 74
xargs -0 -r sh -c 'for f; do if [ -f "$f" ]; then printf "%s\0" "$f"; fi; done' _ <"$sh0" >"$list" || exit 74
[ -s "$list" ] || {
    echo "secret-argv: no files in scope" >&2
    exit 79
}
PROG='
function secname(n,   u) {
    u = toupper(n)
    if (u ~ /^(HAS|HAVE|IS|NO|SKIP|USE|WITH|WITHOUT)_/ || u == "GOOGLE_APPLICATION_CREDENTIALS") return 0   # the latter holds a path
    if (u ~ /(^|_)(FILE|PATH|DIR|URI|URL|NAME|ID|USER|USERNAME|HOST|PORT|REGISTRY|ENDPOINT|SET|PRESENT|ENABLED|REQUIRED|EXISTS|LENGTH|LEN|COUNT)$/) return 0
    if (u ~ /(^|_)(TOKEN|SECRET|PASSWORD|PASSWD|APIKEY|API_KEY|CREDENTIALS?|BEARER|PRIVATE_KEY|ACCESS_KEY|SECRET_KEY)(_|$)/) return 1
    return (n !~ /[a-z]/ && u ~ /(^|_)(PASS|PAT|AUTH)(_|$)/)
}
# endsecret(n): the name ENDS in a secret word (GITHUB_TOKEN, feed_credential), which is what a direct use must show
function endsecret(n) { return secname(n) && toupper(n) ~ /(TOKEN|SECRET|PASSWORD|PASSWD|PASS|PAT|APIKEY|API_KEY|CREDENTIALS?|BEARER|AUTH|KEY)$/ }
function reset() { nl = 0; na = 0; delete LL; delete LN; delete AL; delete AR; delete AK; delete AN; delete T; delete SRC; delete FN; curfn = "" }
# refs(s): fill R[1..n] with the variable names expanded in s ("@" for $@ and $*); single-quoted text is not an expansion
function refs(s,   n, r) {
    n = 0; delete R
    if (gsub(Q, Q, s) % 2 == 0) gsub(Q "[^" Q "]*" Q, "", s)
    while (match(s, /[$][{]?[A-Za-z_][A-Za-z_0-9]*|[$][{]?[@*]/)) {
        r = substr(s, RSTART, RLENGTH); s = substr(s, RSTART + RLENGTH)
        sub(/^[$][{]?/, "", r); if (r ~ /^[@*]/) r = "@"
        R[++n] = r
    }
    return n
}
# consumers whose output is not the secret: a command substitution that runs one of them (first word or after a pipe) does not propagate its inner refs
function dropcs(s,   head, tail) {
    while (match(s, /[$][(]([^)]*[|;&(])?[ \t]*(curl|wget|git|docker|gh|cargo|ssh|aws|gcloud|az|kubectl|helm|npm|pip|oras|skopeo|crane)[ \t][^)]*[)]/)) {
        head = substr(s, 1, RSTART - 1); tail = substr(s, RSTART + RLENGTH); s = head " " tail
    }
    # an unterminated substitution (a quoted argument runs on to the next physical line) is consumed to the end of the line
    if (match(s, /[$][(]([^)]*[|;&(])?[ \t]*(curl|wget|git|docker|gh|cargo|ssh|aws|gcloud|az|kubectl|helm|npm|pip|oras|skopeo|crane)[ \t]/)) s = substr(s, 1, RSTART - 1)
    return s
}
# cmdpos(pre): does the text before a tool name leave the tool in command position (not inside a log string or an echo)?
function cmdpos(pre) {
    while (pre ~ /(^|[ \t])[A-Za-z_][A-Za-z_0-9]*=("[^"]*"|[^ \t"]*)[ \t]*$/) sub(/[A-Za-z_][A-Za-z_0-9]*=("[^"]*"|[^ \t"]*)[ \t]*$/, "", pre)
    sub(/[ \t]+$/, "", pre)
    if (pre == "") return 1
    return (pre ~ /([;&|({`]|[$][(]|:)$/ || pre ~ /(^|[ \t;&|(])(then|do|else|elif|if|while|until|exec|sudo|command|time|xargs|nohup|env|!|-)$/)
}
# secname_ok(x): a "secrets.NAME" expression that names a user, id or other public value
function secname_ok(x) { sub(/^[ \t]*secrets[.]/, "", x); return toupper(x) ~ /(USER|USERNAME|ID|NAME|HOST|URL|EMAIL|REGISTRY)$/ }
function srcroot(rhs) {
    if (rhs ~ /[$][{][{][ \t]*secrets[.]/) return 1
    if (rhs ~ /[$][(] *(cat|head|tail|tr|<) *[^)]*(token|secret|passw|credential|api_?key|[.]netrc|id_rsa|[.]pat)/) return 1
    if (rhs ~ /[$][(] *(pass show|op read|vault kv get|gh auth token|security find-generic-password|aws secretsmanager|gcloud auth print|az account get-access-token|kubectl get secret|bw get|doppler secrets|keyctl)/) return 1
    return 0
}
function flush(   i, k, n, changed, m, j, line, seg, rest, tool, name, cls, dup, nt, w, fnn, src, pre) {
    if (nl == 0) return
    # pass 1: assignments and read -s, to a fixpoint
    for (i = 1; i <= nl; i++) {
        line = LL[i]
        if (match(line, /(^|[;&|(][ \t]*|^[ \t]*)read[ \t]+-[A-Za-z]*s[A-Za-z]*[ \t]/)) {
            rest = substr(line, RSTART); sub(/[<;&|].*$/, "", rest); n = split(rest, W, /[ \t]+/)
            if (W[n] != "" && W[n] !~ /^-/) { na++; AL[na] = W[n]; AR[na] = ""; AK[na] = 1; AN[na] = LN[i] }
            continue
        }
        w = line; sub(/^[ \t]*/, "", w)
        if (w ~ /^set[ \t]+--/) { sub(/^set[ \t]+--/, "", w); name = "@"; rest = w }
        else {
            sub(/^(export|local|declare|typeset|readonly)[ \t]+(-[A-Za-z-]+[ \t]+)*/, "", w)
            if (!match(w, /^[A-Za-z_][A-Za-z_0-9]*[+]?=/)) continue
            name = substr(w, 1, RLENGTH - 1); sub(/[+]$/, "", name); rest = substr(w, RLENGTH + 1)
        }
        na++; AL[na] = name; AN[na] = LN[i]
        rest = dropcs(rest); AK[na] = srcroot(rest)
        m = refs(rest); w = ""
        for (j = 1; j <= m; j++) w = w " " R[j]
        AR[na] = w
    }
    do {
        changed = 0
        for (k = 1; k <= na; k++) {
            if (AL[k] in T) continue
            cls = AK[k]
            if (!cls) { n = split(AR[k], W, " "); for (j = 1; j <= n; j++) if (secname(W[j]) || (W[j] in T)) { cls = 1; break } }
            if (cls) { T[AL[k]] = 1; SRC[AL[k]] = AN[k]; changed = 1 }
        }
    } while (changed)
    # functions that echo/printf a secret: $(fname) in a tool segment counts as a tainted ref
    for (i = 1; i <= nl; i++) {
        line = LL[i]
        if (match(line, /^[ \t]*(function[ \t]+)?[A-Za-z_][A-Za-z_0-9-]*[ \t]*[(][)]/)) {
            curfn = substr(line, RSTART, RLENGTH); sub(/^[ \t]*(function[ \t]+)?/, "", curfn); sub(/[ \t]*[(][)]$/, "", curfn)
        } else if (line ~ /^}/) curfn = ""
        if (curfn != "" && line ~ /(echo|printf)[ \t]/) {
            m = refs(line); for (j = 1; j <= m; j++) if (secname(R[j]) || (R[j] in T)) FN[curfn] = LN[i]
        }
    }
    # pass 2: argument segments of the six tools
    for (i = 1; i <= nl; i++) {
        line = LL[i]; rest = line; dup = ""
        while (match(rest, /(^|[^A-Za-z0-9_.\/-])(curl|wget|git|docker|gh|cargo)([ \t]|$)/)) {
            tool = substr(rest, RSTART, RLENGTH); m = RSTART; gsub(/[^a-z]/, "", tool)
            pre = substr(rest, 1, m - 1 + index(substr(rest, m, RLENGTH), tool) - 1)
            seg = substr(rest, RSTART + RLENGTH); rest = seg
            if (!cmdpos(pre)) continue
            sub(/[|;].*$|&&.*$/, "", seg)
            gsub(/<[(][^)]*[)]/, "", seg); sub(/<<<.*$/, "", seg); sub(/[ \t]<[ \t]*[^ \t]*/, "", seg)
            cls = ""; name = ""
            if (match(seg, /[$][{][{][ \t]*secrets[.][A-Za-z_0-9]*/) && !secname_ok(substr(seg, RSTART + 3, RLENGTH - 3))) { cls = "expr"; name = "secrets" }
            m = refs(seg)
            for (j = 1; j <= m && cls == ""; j++) {
                if (R[j] in T) { cls = "derived"; name = R[j] }
                else if (endsecret(R[j])) { cls = "direct"; name = R[j] }
            }
            if (cls == "") for (fnn in FN) if (index(seg, "$(" fnn) || index(seg, "`" fnn)) { cls = "derived"; name = fnn "()" }
            if (cls != "") {
                src = (name in SRC) ? " (tainted at line " SRC[name] ")" : ""
                printf "%s:%d: [%s] %s argv carries %s%s: %s\n", cur, LN[i], cls, tool, name, src, substr(LL[i], 1, 140)
            }
        }
    }
    reset()
}
BEGIN { Q = sprintf("%c", 39); reset() }
FNR == 1 { if (cur != "") flush(); cur = FILENAME; sub(/^[.]\//, "", cur); acc = ""; accn = 0 }
{
    line = $0
    if (acc == "") accn = FNR
    if (line ~ /\\[ \t]*$/) { sub(/\\[ \t]*$/, " ", line); acc = acc line; next }
    line = acc line; acc = ""
    if (line ~ /sec-ok:[ \t]*[^ \t]/) next
    if (length(line) > 1500) next    # a data blob (a prompt corpus, minified text), not a command
    if (line ~ /^[ \t]*#/) next
    sub(/[ \t]#[ \t].*$/, "", line)
    nl++; LL[nl] = line; LN[nl] = accn
}
END { flush() }
'
LC_ALL=C xargs -0 -r awk "$PROG" <"$list" >"$out" 2>"$err"
rc=$?
if [ -s "$err" ]; then
    cat "$err" >&2
    exit 74
fi
[ "$rc" -le 123 ] || exit 74
if [ -s "$out" ]; then
    cat "$out"
    exit 1
fi
exit 0
