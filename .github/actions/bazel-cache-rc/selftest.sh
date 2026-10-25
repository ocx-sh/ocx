#!/usr/bin/env bash
# Red/green proof for `.github/actions/bazel-cache-rc`: `bash .github/actions/bazel-cache-rc/selftest.sh`.
# No network and no real credential: every property runs against a scratch `$RUNNER_TEMP` and an
# invented credential, and is shown both red and green. Step bodies are read out of `action.yml`
# with a YAML parser, so the proof cannot drift from what the action does.

set -euo pipefail

HERE="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ACTION_YML="$HERE/action.yml"
SUBJECT="$HERE"

# A fake credential in the contracted shape. The base64 half ends in `==`
# padding on purpose: a split on the last `=` rather than the first truncates it
# into something that authenticates nothing, and that bug is invisible to any
# fixture whose base64 happens not to need padding.
SECRET='authorization=Basic b2N4LWNpOnMzY3IzdC1wNHNzdzByZA=='
SECRET_VALUE='b2N4LWNpOnMzY3IzdC1wNHNzdzByZA=='

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# `write_rc.py`'s existing-credential guard reads `$GITHUB_WORKSPACE/.bazelrc` and `$HOME/.bazelrc`; both point at
# empty scratch dirs, or a developer's own `~/.bazelrc` credential would red properties 1-5 on a clean tree.
# Only the subject is scrubbed: `python3` finds pyyaml in a user site keyed to the real `$HOME`, so the
# parser keeps `REAL_HOME` or `action.yml` cannot be read on a workstation.
REAL_HOME="$HOME"
mkdir -p "$WORK/home" "$WORK/workspace"
export HOME="$WORK/home" GITHUB_WORKSPACE="$WORK/workspace"

fail() {
    printf 'selftest: FAIL: %s\n' "$*" >&2
    exit 1
}

green() { printf '    green: %s\n' "$*"; }
red() { printf '    red:   %s\n' "$*"; }

# stdin -> stdout, replacing $1 with $2. Refuses unless the needle occurs
# exactly once and the result really changed: a needle that has drifted would
# otherwise leave the compliant subject standing in for a mutant, and every
# "red" below would then be measuring the unmutated thing.
mutate() {
    NEEDLE="$1" REPLACEMENT="$2" python3 -c '
import os, sys

source = sys.stdin.read()
needle, replacement = os.environ["NEEDLE"], os.environ["REPLACEMENT"]
count = source.count(needle)
if count != 1:
    raise SystemExit(f"mutation needle occurs {count} times, expected exactly 1: {needle!r}")
mutated = source.replace(needle, replacement)
# The count check above already proves the one occurrence was rewritten, so the
# landing proof is that the text changed and the replacement is in the result.
# `needle in mutated` is deliberately *not* asserted: a mutation that wraps its
# needle rather than removing it is still a mutation.
if mutated == source or replacement not in mutated:
    raise SystemExit("mutation did not land")
sys.stderr.write(f"    mutation landed: {needle!r} -> {replacement!r}\n")
sys.stdout.write(mutated)
'
}

# The `run:` body of the one step whose name contains $1, read out of
# `action.yml` by a YAML parser. A missing or ambiguous match is an error, not
# an empty string: an empty body would make every assertion below vacuous.
step_body() {
    HOME="$REAL_HOME" python3 -c '
import sys, yaml

document = yaml.safe_load(open(sys.argv[1], encoding="utf-8"))
matches = [s for s in document["runs"]["steps"] if sys.argv[2] in s["name"]]
if len(matches) != 1:
    raise SystemExit(f"expected 1 step matching {sys.argv[2]!r}, found {len(matches)}")
sys.stdout.write(matches[0]["run"])
' "$ACTION_YML" "$1"
}

WRITE_BODY="$(step_body Write)"
CLEANUP_BODY="$(step_body Remove)"

# Run a write body with $1 as the action directory and $2 as `$RUNNER_TEMP`.
run_write() {
    GITHUB_ACTION_PATH="$1" RUNNER_TEMP="$2" WRITE_AUTH="$SECRET" bash -c "$3"
}

# 0 when the pair is present with the contracted modes and the rc is, byte for
# byte, the golden text built from this directory. `cmp`, never `$(cat)`: command
# substitution drops trailing newlines, and a substring search passes on a third
# line, a comment header or a missing final newline. The here-string supplies the
# one trailing newline.
assert_shape() {
    local rc="$1/bazel-cache.rc" helper="$1/bazel-cache-helper"
    [ -f "$rc" ] || return 1
    [ -f "$helper" ] || return 1
    [ "$(stat -c %a "$rc")" = "600" ] || return 1
    [ "$(stat -c %a "$helper")" = "700" ] || return 1
    cmp -s "$rc" - <<<"build --credential_helper=bazel-cache.ocx.sh=$helper
build --remote_upload_local_results=true" || return 1
}

# 0 when the directory is untouched. Non-existence, not emptiness — and over the
# whole directory, so a mutant writing under some other name is still caught.
assert_nothing_written() {
    [ ! -e "$1/bazel-cache.rc" ] || return 1
    [ ! -e "$1/bazel-cache-helper" ] || return 1
    [ -z "$(ls -A "$1")" ] || return 1
}

scratch() {
    local dir="$WORK/$1"
    mkdir -p "$dir"
    printf '%s' "$dir"
}

# ---------------------------------------------------------------------------
printf '\n[1] a non-empty credential writes the golden rc and the helper, owner-only\n'
# ---------------------------------------------------------------------------
temp="$(scratch t1)"
run_write "$SUBJECT" "$temp" "$WRITE_BODY" >/dev/null
assert_shape "$temp" || fail "the compliant action did not produce the contracted pair"
green "stat -c %a bazel-cache.rc      -> $(stat -c %a "$temp/bazel-cache.rc")"
green "stat -c %a bazel-cache-helper  -> $(stat -c %a "$temp/bazel-cache-helper")"
green "rc holds exactly: $(cat "$temp/bazel-cache.rc")"

# The helper is checked by running it and parsing what it prints, not by reading
# its text: the property is "Bazel gets the credential back", and only a parser
# establishes that. The request is fed on stdin exactly as Bazel feeds it.
printf '{"uri":"https://bazel-cache.ocx.sh/v1"}' |
    "$temp/bazel-cache-helper" get |
    EXPECTED="$SECRET_VALUE" python3 -c '
import json, os, sys

document = json.load(sys.stdin)
expected = {"authorization": ["Basic " + os.environ["EXPECTED"]]}
if document != {"headers": expected}:
    raise SystemExit(f"helper answered {document!r}, expected headers {expected!r}")
' || fail "the credential helper did not answer Bazel's \`get\` with the credential"
green "the helper answers \`get\` with headers.authorization = [Basic <base64>]"

mutant="$(scratch m1)"
mutate '0o700' '0o755' <"$SUBJECT/write_rc.py" |
    mutate '0o600' '0o644' >"$mutant/write_rc.py"
temp="$(scratch t1r)"
run_write "$mutant" "$temp" "$WRITE_BODY" >/dev/null
red "stat -c %a bazel-cache.rc      -> $(stat -c %a "$temp/bazel-cache.rc")"
red "stat -c %a bazel-cache-helper  -> $(stat -c %a "$temp/bazel-cache-helper")"
if assert_shape "$temp"; then
    fail "the loosened-mode mutant passed the shape assertion — it cannot discriminate"
fi
red "the shape assertion refused it"

# The golden compare must refuse a rc that lacks the upload grant, and one that
# carries anything beyond the two contracted lines.
mutant="$(scratch m1b)"
mutate '"build --remote_upload_local_results=true\n"' '""' \
    <"$SUBJECT/write_rc.py" >"$mutant/write_rc.py"
temp="$(scratch t1rb)"
run_write "$mutant" "$temp" "$WRITE_BODY" >/dev/null
red "rc without the upload grant: $(tr '\n' '|' <"$temp/bazel-cache.rc")"
if assert_shape "$temp"; then
    fail "the grant-less mutant passed the shape assertion — it cannot discriminate"
fi
red "the shape assertion refused it"

mutant="$(scratch m1c)"
mutate '"build --remote_upload_local_results=true\n"' \
    '"build --remote_upload_local_results=true\n"
        "# MUTANT\n"' <"$SUBJECT/write_rc.py" >"$mutant/write_rc.py"
temp="$(scratch t1rc)"
run_write "$mutant" "$temp" "$WRITE_BODY" >/dev/null
red "rc with a third line: $(tr '\n' '|' <"$temp/bazel-cache.rc")"
if assert_shape "$temp"; then
    fail "the extra-line mutant passed the shape assertion — it cannot discriminate"
fi
red "the shape assertion refused it"

# ---------------------------------------------------------------------------
printf '\n[2] an empty credential writes nothing at all\n'
# ---------------------------------------------------------------------------
temp="$(scratch t2)"
GITHUB_ACTION_PATH="$SUBJECT" RUNNER_TEMP="$temp" WRITE_AUTH="" bash -c "$WRITE_BODY" >/dev/null
assert_nothing_written "$temp" || fail "an empty credential left something behind in $temp"
green "\$RUNNER_TEMP after an empty credential holds: $(ls -A "$temp" | wc -l) entries"

# The naive implementation this guards against: fall through on empty and write
# a placeholder credential, leaving an rc file on a lane that must have none.
mutant="$(scratch m2)"
mutate '
        return
' '
        write_auth = "authorization=Basic cGxhY2Vob2xkZXI="  # MUTANT
' <"$SUBJECT/write_rc.py" >"$mutant/write_rc.py"
temp="$(scratch t2r)"
GITHUB_ACTION_PATH="$mutant" RUNNER_TEMP="$temp" WRITE_AUTH="" bash -c "$WRITE_BODY" >/dev/null
red "\$RUNNER_TEMP after an empty credential holds: $(ls -A "$temp" | wc -l) entries"
if assert_nothing_written "$temp"; then
    fail "the fall-through mutant passed — the assertion cannot see an rc file it must forbid"
fi
red "the no-file assertion refused it"

# ---------------------------------------------------------------------------
printf '\n[3] the cleanup removes both, and is green with nothing to remove\n'
# ---------------------------------------------------------------------------
temp="$(scratch t3)"
run_write "$SUBJECT" "$temp" "$WRITE_BODY" >/dev/null
assert_shape "$temp" || fail "could not set up the cleanup case"
RUNNER_TEMP="$temp" bash -c "$CLEANUP_BODY"
assert_nothing_written "$temp" || fail "the cleanup left something behind in $temp"
green "both files gone after the cleanup body ran"

# The `if: always()` case that matters: the job failed before the write, so
# there is nothing to delete and the cleanup must still exit 0. Anything else
# turns a failed build into two failures and hides the first.
temp="$(scratch t3b)"
if ! RUNNER_TEMP="$temp" bash -c "$CLEANUP_BODY"; then
    fail "the cleanup body is not green when the credential was never written"
fi
green "exit 0 with nothing to remove"

# Red (a): a cleanup that forgets the helper leaves the credential on the disk.
temp="$(scratch t3r)"
run_write "$SUBJECT" "$temp" "$WRITE_BODY" >/dev/null
printf '%s\n' "$CLEANUP_BODY" |
    mutate ' "$RUNNER_TEMP/bazel-cache-helper"' '' >"$WORK/cleanup-partial.sh"
RUNNER_TEMP="$temp" bash "$WORK/cleanup-partial.sh"
red "after the helper-forgetting cleanup, \$RUNNER_TEMP holds: $(ls -A "$temp" | tr '\n' ' ')"
if assert_nothing_written "$temp"; then
    fail "the partial cleanup passed — the assertion does not look at the helper"
fi
red "the no-file assertion refused it"

# Red (b): `rm` without `-f` is non-zero on an absent file, so the cleanup would
# redden every job that failed before the write.
temp="$(scratch t3rb)"
printf '%s\n' "$CLEANUP_BODY" | mutate 'rm -f ' 'rm ' >"$WORK/cleanup-strict.sh"
if RUNNER_TEMP="$temp" bash "$WORK/cleanup-strict.sh" 2>/dev/null; then
    fail "the \`rm\` mutant exited 0 — the nothing-to-remove case is not being measured"
fi
red "\`rm\` without \`-f\` exits non-zero when the credential was never written"

# ---------------------------------------------------------------------------
printf '\n[4] the credential reaches no command line, so `set -x` cannot log it\n'
# ---------------------------------------------------------------------------
# `set -x` traces *expanded* arguments, which is why it is the mechanical
# detector for the interpolated spelling: an `env:`-borne secret is already in
# the environment and is never a traced word, while a `${{ }}` one is rendered
# into the script text and traced on the line that uses it.
trace() {
    GITHUB_ACTION_PATH="$SUBJECT" RUNNER_TEMP="$2" WRITE_AUTH="$SECRET" \
        bash -x -c "$1" 2>&1
}

temp="$(scratch t4)"
traced="$(trace "$WRITE_BODY" "$temp")"
# Floor the reader before trusting its silence: a trace that never happened
# contains no credential either.
grep -q 'umask 077' <<<"$traced" || fail "the trace does not show the step running at all"
if grep -qF "$SECRET_VALUE" <<<"$traced"; then
    fail "the credential appears in the trace of the compliant step"
fi
green "$(wc -l <<<"$traced" | tr -d ' ') traced lines, none containing the credential"

# The interpolated variant is built here and dies with $WORK: the spelling this
# forbids never lives in the tree, and its red stays runnable.
interpolated="$(
    printf '%s\n' "$WRITE_BODY" |
        mutate 'python3 "$GITHUB_ACTION_PATH/write_rc.py"' \
            "WRITE_AUTH='$SECRET' python3 \"\$GITHUB_ACTION_PATH/write_rc.py\""
)"
temp="$(scratch t4r)"
traced="$(trace "$interpolated" "$temp")"
grep -q 'umask 077' <<<"$traced" || fail "the trace does not show the mutant running at all"
if ! grep -qF "$SECRET_VALUE" <<<"$traced"; then
    fail "the interpolated variant did not leak — the detector cannot see what it is for"
fi
red "the credential is in the trace: $(grep -F "$SECRET_VALUE" <<<"$traced" | head -1)"

# The same property, asserted structurally over `action.yml`: no `run:` body may
# carry a `${{ … }}` at all, and the credential must be bound in `env:`.
HOME="$REAL_HOME" python3 -c '
import sys, yaml

document = yaml.safe_load(open(sys.argv[1], encoding="utf-8"))
steps = document["runs"]["steps"]
if not steps:
    raise SystemExit("action.yml declares no steps — the reader found nothing")
interpolating = [s["name"] for s in steps if "${{" in s["run"]]
if interpolating:
    raise SystemExit(f"run bodies carrying an expression: {interpolating}")
bindings = [s.get("env", {}).get("WRITE_AUTH") for s in steps]
if "${{ inputs.write-auth }}" not in bindings:
    raise SystemExit(f"no step binds WRITE_AUTH through env: {bindings}")
if "read-auth" in document["inputs"]:
    raise SystemExit("action.yml still declares the retired read-auth input")
if "post" in document["runs"] or "post-if" in document["runs"]:
    raise SystemExit("composite runs declare no post hook; the caller supplies if: always()")
' "$ACTION_YML" || fail "action.yml does not bind the credential through env:"
green 'no `run:` body interpolates; WRITE_AUTH is bound in `env:`'

# ---------------------------------------------------------------------------
printf '\n[5] nothing already at those paths is ever written through\n'
# ---------------------------------------------------------------------------
# `$RUNNER_TEMP` is not private by construction: an entry already at the helper's path (a leftover, or planted by an
# earlier job on a reused self-hosted runner) must be replaced, never followed. A credential written through a
# symlink lands where the cleanup step does not delete it, hence `write_rc.py` unlinks before `O_EXCL`.
plant() {
    local dir="$1"
    : >"$WORK/victim-$2"
    ln -sf "$WORK/victim-$2" "$dir/bazel-cache-helper"
}

temp="$(scratch t5)"
plant "$temp" green
run_write "$SUBJECT" "$temp" "$WRITE_BODY" >/dev/null
[ ! -L "$temp/bazel-cache-helper" ] || fail "the planted symlink survived"
[ ! -s "$WORK/victim-green" ] || fail "the credential was written through the symlink"
green "the link was replaced by a regular file; its target is still $(wc -c <"$WORK/victim-green") bytes"

mutant="$(scratch m5)"
mutate '    path.unlink(missing_ok=True)
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, mode)
    with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
        stream.write(text)' '    path.write_text(text, encoding="utf-8")  # MUTANT' \
    <"$SUBJECT/write_rc.py" >"$mutant/write_rc.py"
temp="$(scratch t5r)"
plant "$temp" red
run_write "$mutant" "$temp" "$WRITE_BODY" >/dev/null
red "the credential landed outside \$RUNNER_TEMP: $(wc -c <"$WORK/victim-red") bytes in the symlink's target"
if [ ! -s "$WORK/victim-red" ]; then
    fail "the write_text mutant did not follow the symlink — the check measures nothing"
fi

# ---------------------------------------------------------------------------
printf '\n[6] a credential already configured in an rc Bazel reads is refused\n'
# ---------------------------------------------------------------------------
# Bazel `add`s an `Authorization` header per configured source and sends every one on the same request
# (measured against a local sink: one cache `PUT` carried three), so the action refuses to be a second source.
# `$HOME` and `$GITHUB_WORKSPACE` are the scratch pair exported at the top, so each case builds exactly its rc chain.

# `$1`=action dir, `$2`=$RUNNER_TEMP, `$3`=WRITE_AUTH. stderr folded into stdout,
# because the refusal is a `sys.exit` message.
run_guarded() {
    GITHUB_ACTION_PATH="$1" RUNNER_TEMP="$2" WRITE_AUTH="$3" bash -c "$WRITE_BODY" 2>&1
}

# 0 when the step refused for the contracted reason and left no trace of the
# credential: the named cause is present, the credential is not, and nothing was
# written. All three, because a refusal that still wrote the rc file, or one that
# quoted the value it refused, is the defect wearing the fix's exit code.
assert_refused() {
    grep -qF 'two credential sources on one lane' <<<"$1" || return 1
    if grep -qF "$SECRET_VALUE" <<<"$1"; then
        return 1
    fi
    assert_nothing_written "$2"
}

# Green: a clean chain still writes. Without this the reds below would be
# satisfied by an action that refuses everything.
temp="$(scratch t6)"
run_guarded "$SUBJECT" "$temp" "$SECRET" >/dev/null
assert_shape "$temp" || fail "a clean rc chain did not produce the contracted pair"
green "empty rc chain -> $(ls -A "$temp" | tr '\n' ' ')"

# Red (a): the source Bazel already reads. `.bazelrc` documents `~/.bazelrc` as
# where a developer's read credential lives, and a runner that acquired one
# would make every request carry two headers with nothing failing.
temp="$(scratch t6a)"
printf "build --remote_header='%s'\n" "$SECRET" >"$HOME/.bazelrc"
if output="$(run_guarded "$SUBJECT" "$temp" "$SECRET")"; then
    fail "a credential was written beside an existing \$HOME/.bazelrc header"
fi
assert_refused "$output" "$temp" || fail "the refusal is not the contracted one: $output"
grep -qF "$HOME/.bazelrc:1" <<<"$output" ||
    fail "the refusal does not name the file and line it found: $output"
red "an existing \$HOME/.bazelrc header -> refused, naming $HOME/.bazelrc:1 and not its value"

# Red (b): the same source one `import` away. This repository's `.bazelrc` ends
# in `try-import %workspace%/.bazelrc.user`, which is gitignored — so a guard
# reading only the files it was handed is green over the one rc file a developer
# is most likely to put a credential in.
rm -f "$HOME/.bazelrc"
printf 'try-import %%workspace%%/.bazelrc.user\n' >"$GITHUB_WORKSPACE/.bazelrc"
printf "build --remote_header='%s'\n" "$SECRET" >"$GITHUB_WORKSPACE/.bazelrc.user"
temp="$(scratch t6b)"
if output="$(run_guarded "$SUBJECT" "$temp" "$SECRET")"; then
    fail "a credential reached only through an \`import\` was not seen"
fi
assert_refused "$output" "$temp" || fail "the refusal is not the contracted one: $output"
grep -qF "$GITHUB_WORKSPACE/.bazelrc.user:1" <<<"$output" ||
    fail "the refusal does not name the imported file: $output"
red "a header behind \`try-import\` -> refused, naming .bazelrc.user:1"

# And the same red state, to show the chain reader is what found it: with the
# guard removed the credential is written beside the header that was already
# there.
mutant="$(scratch m6b)"
mutate '    configured = other_credential_sources()' '    configured = []  # MUTANT' \
    <"$SUBJECT/write_rc.py" >"$mutant/write_rc.py"
temp="$(scratch t6br)"
run_guarded "$mutant" "$temp" "$SECRET" >/dev/null
[ -f "$temp/bazel-cache.rc" ] ||
    fail "the chain-blind mutant wrote nothing — the red is not measuring the chain reader"
red "without the chain reader the rc lands beside the imported header"

# Green again, and the discriminator that keeps the reader from being a bare
# substring search: a commented-out header configures nothing, so it is not a
# source. A guard reporting it would red every tree carrying the documentation.
printf "# build --remote_header='%s'\n" "$SECRET" >"$GITHUB_WORKSPACE/.bazelrc"
rm -f "$GITHUB_WORKSPACE/.bazelrc.user"
temp="$(scratch t6c)"
run_guarded "$SUBJECT" "$temp" "$SECRET" >/dev/null
[ -f "$temp/bazel-cache.rc" ] || fail "a commented-out rc header was read as a live source"
green "a commented-out header is not a source; the rc was written"
rm -f "$GITHUB_WORKSPACE/.bazelrc"

# Two rcs importing each other: the chain walk ends only because `seen` stops it. Both runs are bounded, so
# the mutant's endless walk is a red (exit 124) rather than a hung job.
printf 'import %%workspace%%/.bazelrc.user\n' >"$GITHUB_WORKSPACE/.bazelrc"
printf 'import %%workspace%%/.bazelrc\n' >"$GITHUB_WORKSPACE/.bazelrc.user"
run_bounded() {
    local status=0
    GITHUB_ACTION_PATH="$1" RUNNER_TEMP="$2" WRITE_AUTH="$SECRET" timeout 10 bash -c "$WRITE_BODY" >/dev/null 2>&1 || status=$?
    printf '%s' "$status"
}
temp="$(scratch t6d)"
status="$(run_bounded "$SUBJECT" "$temp")"
[ "$status" = 0 ] || fail "an import cycle did not end cleanly (exit $status)"
assert_shape "$temp" || fail "an import cycle with no credential in it did not produce the contracted pair"
green "a two-file import cycle with no credential ends, and the rc was written"

mutant="$(scratch m6d)"
mutate 'if path in seen:' 'if False:  # MUTANT' <"$SUBJECT/write_rc.py" >"$mutant/write_rc.py"
temp="$(scratch t6dr)"
status="$(run_bounded "$mutant" "$temp")"
[ "$status" = 124 ] || fail "the seen-less mutant did not hang (exit $status) - the cycle does not reach the guard"
assert_nothing_written "$temp" || fail "the seen-less mutant wrote while it should have been looping"
red "without \`seen\` the chain walk never ends: killed by the 10 s bound (exit 124)"
rm -f "$GITHUB_WORKSPACE/.bazelrc" "$GITHUB_WORKSPACE/.bazelrc.user"

# Red (c)-(e): the spellings of a configured credential that (a) and (b) do not
# reach: a helper rather than a header, its space-separated flag, and a header
# name in another case (`Authorization=` is `authorization=` on the wire).
# `$1` = case id, `$2` = the line planted in `$HOME/.bazelrc`.
assert_rc_line_refused() {
    local temp output
    temp="$(scratch "t6-$1")"
    printf '%s\n' "$2" >"$HOME/.bazelrc"
    if output="$(run_guarded "$SUBJECT" "$temp" "$SECRET")"; then
        fail "a credential was written beside: $2"
    fi
    assert_refused "$output" "$temp" || fail "the refusal is not the contracted one: $output"
    grep -qF "$HOME/.bazelrc:1" <<<"$output" || fail "the refusal does not name the file and line it found: $output"
    red "$2 -> refused, naming \$HOME/.bazelrc:1"
}

# Red by mutation: the same rc line with the named piece of the reader removed.
# The mutant must write the rc beside the line, or the red measures nothing.
assert_rc_line_mutant_writes() {
    local temp
    temp="$(scratch "t6-$1-mutant")"
    printf '%s\n' "$2" >"$HOME/.bazelrc"
    run_guarded "$3" "$temp" "$SECRET" >/dev/null
    [ -f "$temp/bazel-cache.rc" ] || fail "the $1 mutant wrote nothing - the red is not measuring that branch"
    red "without the $1 branch the rc lands beside: $2"
}

HELPER_LINE='build --credential_helper=bazel-cache.ocx.sh=/tmp/elsewhere/helper'
assert_rc_line_refused helper-equals "$HELPER_LINE"
assert_rc_line_refused helper-space 'build --credential_helper bazel-cache.ocx.sh=/tmp/elsewhere/helper'
mutant="$(scratch m6c)"
mutate 'if AUTHORIZATION_HEADER.search(line) or CREDENTIAL_HELPER.search(line)' \
    'if AUTHORIZATION_HEADER.search(line)' <"$SUBJECT/write_rc.py" >"$mutant/write_rc.py"
assert_rc_line_mutant_writes helper-equals "$HELPER_LINE" "$mutant"
assert_rc_line_mutant_writes helper-space \
    'build --credential_helper bazel-cache.ocx.sh=/tmp/elsewhere/helper' "$mutant"

UPPER_LINE="build --remote_header=Authorization=Basic b3RoZXI6c2VjcmV0"
assert_rc_line_refused header-upper-case "$UPPER_LINE"
mutant="$(scratch m6e)"
mutate ', re.IGNORECASE)

#: A credential helper' ')

#: A credential helper' <"$SUBJECT/write_rc.py" >"$mutant/write_rc.py"
assert_rc_line_mutant_writes header-upper-case "$UPPER_LINE" "$mutant"
rm -f "$HOME/.bazelrc"

# ---------------------------------------------------------------------------
printf '\n[7] no RUNNER_TEMP, or a credential that is not <header>=<value>, is refused before anything is written\n'
# ---------------------------------------------------------------------------
# A run with a credential but no `$RUNNER_TEMP` would resolve both files against
# the working directory, which is the workspace on a runner. The run starts in an
# empty scratch directory so a mutant that writes there is seen.
temp="$(scratch t7a)"
if output="$(cd "$temp" && GITHUB_ACTION_PATH="$SUBJECT" WRITE_AUTH="$SECRET" env -u RUNNER_TEMP bash -c "$WRITE_BODY" 2>&1)"; then
    fail "a credential was accepted with no RUNNER_TEMP"
fi
grep -qF 'RUNNER_TEMP is unset' <<<"$output" || fail "the refusal is not the contracted one: $output"
assert_nothing_written "$temp" || fail "the refusal still wrote into the working directory: $(ls -A "$temp")"
green "no RUNNER_TEMP -> refused, and the working directory is still empty"

mutant="$(scratch m7a)"
mutate '    if not runner_temp:
        sys.exit("bazel cache: RUNNER_TEMP is unset; this action only runs on a runner")' \
    '    pass  # MUTANT' <"$SUBJECT/write_rc.py" >"$mutant/write_rc.py"
temp="$(scratch t7ar)"
(cd "$temp" && GITHUB_ACTION_PATH="$mutant" WRITE_AUTH="$SECRET" env -u RUNNER_TEMP bash -c "$WRITE_BODY" >/dev/null 2>&1) || true
red "without the guard the working directory holds: $(ls -A "$temp" | tr '\n' ' ')"
if assert_nothing_written "$temp"; then
    fail "the guard-less mutant wrote nothing - the check cannot see the working-directory write"
fi

# The three shapes `split_credential` refuses, each with the text a leak would
# repeat and the length a diagnostic may state instead.
assert_malformed_refused() {
    local temp output
    temp="$(scratch "t7-$1")"
    if output="$(run_guarded "$SUBJECT" "$temp" "$2")"; then
        fail "a credential $1 was accepted"
    fi
    grep -qF 'is not `<header>=<value>`' <<<"$output" || fail "the refusal is not the contracted one: $output"
    grep -qF "(${#2} characters)" <<<"$output" || fail "the refusal does not state the credential's length: $output"
    if [ -n "$3" ] && grep -qF "$3" <<<"$output"; then
        fail "the refusal repeats the credential: $output"
    fi
    assert_nothing_written "$temp" || fail "the refusal still wrote: $(ls -A "$temp")"
    red "$1 -> refused, stating its length and not its value"
}

# Red by mutation: the same malformed credential, a guard removed. It must write.
assert_malformed_mutant_writes() {
    local temp
    temp="$(scratch "t7-$1-mutant")"
    run_guarded "$3" "$temp" "$2" >/dev/null
    [ -f "$temp/bazel-cache.rc" ] || fail "the mutant for a credential $1 wrote nothing - the red is not measuring that clause"
    red "with that guard removed, a credential $1 reaches the helper"
}

NO_SEPARATOR='NoSeparatorSecretValue'
NO_HEADER='=EmptyHeaderSecretValue'
NO_VALUE='authorization='
assert_malformed_refused "without a separator" "$NO_SEPARATOR" NoSeparatorSecretValue
assert_malformed_refused "without a header" "$NO_HEADER" EmptyHeaderSecretValue
assert_malformed_refused "without a value" "$NO_VALUE" ""

GUARD='if not separator or not header.strip() or not value.strip():'
mutant="$(scratch m7b)"
mutate "$GUARD" 'if False:  # MUTANT' <"$SUBJECT/write_rc.py" >"$mutant/write_rc.py"
assert_malformed_mutant_writes "without a separator" "$NO_SEPARATOR" "$mutant"
assert_malformed_mutant_writes "without a header" "$NO_HEADER" "$mutant"
assert_malformed_mutant_writes "without a value" "$NO_VALUE" "$mutant"

# One clause each, for the two shapes only that clause refuses. `not separator`
# is not mutated alone: a credential with no `=` has no value either, so the
# value clause refuses it too, and deleting one of two guards over one property
# leaves the check green.
mutant="$(scratch m7c)"
mutate ' or not header.strip()' '' <"$SUBJECT/write_rc.py" >"$mutant/write_rc.py"
assert_malformed_mutant_writes "without a header" "$NO_HEADER" "$mutant"
mutant="$(scratch m7d)"
mutate ' or not value.strip()' '' <"$SUBJECT/write_rc.py" >"$mutant/write_rc.py"
assert_malformed_mutant_writes "without a value" "$NO_VALUE" "$mutant"

printf '\nselftest: every property above shown red and green\n'
