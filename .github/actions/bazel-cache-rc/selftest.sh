#!/usr/bin/env bash
#
# Red/green proof for `.github/actions/bazel-cache-rc`.
#
# Run it directly: `bash .github/actions/bazel-cache-rc/selftest.sh`. It touches
# no network and no real credential — the shared cache realm is deliberately not
# reachable from a check (plan C-029), so every property below is established
# against a scratch `$RUNNER_TEMP` and a fake credential this script invents.
#
# Seven properties, each shown **both** red and green, because a green whose red
# was never reachable is indistinguishable from a check that never ran
# (quality-core.md § Unchecked Green):
#
#   1. a non-empty write credential writes the rc and the helper, owner-only
#   2. an empty credential writes nothing at all
#   3. the cleanup step removes both, and is green when there was nothing to
#      remove — the state a job that failed before the write leaves behind
#   4. the credential reaches no command line, so `set -x` cannot log it
#   5. nothing already at those paths is ever written through
#   6. the read credential is a quoted `--remote_header` and no helper at all
#   7. a second credential source is refused, and the refusal is not the leak
#
# The step bodies are read out of `action.yml` with a YAML parser rather than
# copied here, so this script cannot drift into proving something the action no
# longer does. Every mutation is checked to have landed before its red is
# believed: the harness is not exempt.

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

# Every run below is hermetic in the two places `write_rc.py`'s DX-81 guard
# reads: the rc chain is `$GITHUB_WORKSPACE/.bazelrc`, `$HOME/.bazelrc` and
# whatever they import. Without these, a developer whose own `~/.bazelrc` carries
# the read credential — the spelling `.bazelrc` documents for a workstation —
# would red properties 1 through 6 on a tree with nothing wrong with it, and the
# script would be measuring their machine rather than the action. The empty
# workspace also keeps this repository's own `.bazelrc` out of the walk.
#
# The scrub is for the SUBJECT, not for this harness's own tooling. `python3`
# resolves pyyaml out of a user site keyed to `$HOME` on a normal workstation
# (`~/.local/lib/python3.*/site-packages`), so reading `action.yml` after the
# scrub dies with ModuleNotFoundError on exactly the machines where the rest of
# this script is most worth running - and passes on a runner with a system-wide
# pyyaml, which is the worst of both. Keep the real one for the parser.
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

# 0 when the pair is present with the contracted modes and the rc names exactly
# the helper that was written. The rc is compared for **equality** with the one
# line it must hold, never searched for a substring: a comment saying
# `--credential_helper` is absent contains the needle just as well as the flag
# does.
assert_shape() {
    local rc="$1/bazel-cache.rc" helper="$1/bazel-cache-helper"
    [ -f "$rc" ] || return 1
    [ -f "$helper" ] || return 1
    [ "$(stat -c %a "$rc")" = "600" ] || return 1
    [ "$(stat -c %a "$helper")" = "700" ] || return 1
    [ "$(cat "$rc")" = "build --credential_helper=bazel-cache.ocx.sh=$helper" ] || return 1
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
printf '\n[1] a non-empty credential writes the rc and the helper, owner-only\n'
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
        auth = "authorization=Basic "  # MUTANT
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
for variable, expression in (("WRITE_AUTH", "write-auth"), ("READ_AUTH", "read-auth")):
    bindings = [s.get("env", {}).get(variable) for s in steps]
    if "${{ inputs.%s }}" % expression not in bindings:
        raise SystemExit(f"no step binds {variable} through env: {bindings}")
if "post" in document["runs"] or "post-if" in document["runs"]:
    raise SystemExit("composite runs declare no post hook; the caller supplies if: always()")
' "$ACTION_YML" || fail "action.yml does not bind the credential through env:"
green 'no `run:` body interpolates; WRITE_AUTH is bound in `env:`'

# ---------------------------------------------------------------------------
printf '\n[5] nothing already at those paths is ever written through\n'
# ---------------------------------------------------------------------------
# Beyond C-015, and the reason `write_rc.py` unlinks before `O_EXCL` rather than
# calling `write_text`: `$RUNNER_TEMP` is not private by construction, so an
# entry already sitting at the helper's path — a leftover, or on a reused
# self-hosted runner something an earlier job planted — must be replaced, never
# followed. A credential written through a symlink lands somewhere the cleanup
# step does not delete.
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
printf '\n[6] the read credential is a quoted --remote_header and no helper at all\n'
# ---------------------------------------------------------------------------
# A read credential is a value Bazel *sends*, so BZL-CACHE-04's executed-helper
# hazard does not reach it and no helper is written. What does reach it is the
# rc tokenizer: measured on the pinned 9.2.0, an unquoted
# `build --remote_header=authorization=Basic <b64>` is split on whitespace and
# `<b64>` arrives as a positional argument (`ERROR: unknown key(s)`, exit 2).
# The emitted line is therefore checked with a **tokenizer**, not a substring —
# a `grep` for the credential passes on exactly the broken form this forbids.
run_read() {
    GITHUB_ACTION_PATH="$1" RUNNER_TEMP="$2" READ_AUTH="$3" bash -c "$WRITE_BODY"
}

# 0 when the rc holds one `--remote_header` whose value survives tokenization
# whole, at mode 600, with no helper beside it.
assert_read_shape() {
    local rc="$1/bazel-cache.rc"
    [ -f "$rc" ] || return 1
    [ ! -e "$1/bazel-cache-helper" ] || return 1
    [ "$(stat -c %a "$rc")" = "600" ] || return 1
    EXPECTED="$SECRET" python3 -c '
import os, shlex, sys

tokens = shlex.split(open(sys.argv[1], encoding="utf-8").read())
if tokens != ["build", "--remote_header=" + os.environ["EXPECTED"]]:
    raise SystemExit(f"rc tokenizes to {tokens!r}")
' "$rc"
}

temp="$(scratch t6)"
run_read "$SUBJECT" "$temp" "$SECRET" >/dev/null
assert_read_shape "$temp" || fail "a read-only credential did not produce the contracted rc"
green "stat -c %a bazel-cache.rc -> $(stat -c %a "$temp/bazel-cache.rc")"
green "rc holds exactly: $(cat "$temp/bazel-cache.rc")"
green "no bazel-cache-helper written: $(ls -A "$temp" | tr '\n' ' ')"

# Red (a): the unquoted spelling. It contains the credential and reads fine, and
# Bazel splits it into a flag plus a stray argument.
mutant="$(scratch m6)"
mutate 'f"build --remote_header={shlex.quote(auth)}\n"' \
    'f"build --remote_header={auth}\n"  # MUTANT' <"$SUBJECT/write_rc.py" >"$mutant/write_rc.py"
temp="$(scratch t6r)"
run_read "$mutant" "$temp" "$SECRET" >/dev/null
red "the unquoted rc holds: $(cat "$temp/bazel-cache.rc")"
grep -qF "$SECRET_VALUE" "$temp/bazel-cache.rc" ||
    fail "the unquoted mutant did not even contain the credential — nothing is being measured"
red "a substring check for the credential PASSES on it, which is why this uses a tokenizer"
if assert_read_shape "$temp"; then
    fail "the unquoted mutant passed the tokenizer assertion — it cannot discriminate"
fi
red "the tokenizer assertion refused it"

# Red (b): a value Bazel's rc tokenizer would re-decode. It unescapes a
# backslash inside single quotes where POSIX does not, so `shlex.quote` is not
# sufficient on its own and such a value is refused rather than mangled.
temp="$(scratch t6b)"
if run_read "$SUBJECT" "$temp" 'authorization=Basic a\b==' >/dev/null 2>&1; then
    fail "a backslash-bearing credential was accepted — it would reach Bazel altered"
fi
assert_nothing_written "$temp" || fail "the refused credential still left a file behind"
red "a backslash-bearing read credential exits non-zero and writes nothing"

# Red (c): the shape check is the write half's, reached through the read half.
temp="$(scratch t6c)"
if run_read "$SUBJECT" "$temp" 'not-a-header-value' >/dev/null 2>&1; then
    fail "a credential with no \`=\` was accepted"
fi
red "a read credential that is not \`<header>=<value>\` exits non-zero"

# Red (d): the same `set -x` detector as [4], on the read binding. `env:`-borne
# is never a traced word; the interpolated spelling is.
temp="$(scratch t6d)"
traced="$(GITHUB_ACTION_PATH="$SUBJECT" RUNNER_TEMP="$temp" READ_AUTH="$SECRET" \
    bash -x -c "$WRITE_BODY" 2>&1)"
grep -q 'umask 077' <<<"$traced" || fail "the trace does not show the step running at all"
if grep -qF "$SECRET_VALUE" <<<"$traced"; then
    fail "the read credential appears in the trace of the compliant step"
fi
green "$(wc -l <<<"$traced" | tr -d ' ') traced lines, none containing the read credential"

# ---------------------------------------------------------------------------
printf '\n[7] a second credential source is refused, and the refusal is not the leak\n'
# ---------------------------------------------------------------------------
# DX-81. Bazel `add`s an `Authorization` header per configured source rather
# than replacing it, and sends every one of them on the same request — measured
# against a local sink with both routes live, one cache `PUT` carried three.
# Which one an origin honours is that origin's business, and a design that needs
# to know is the defect, so the action refuses to become the second source.
#
# Two ways the second one arrives and each gets its own red: the caller passing
# both inputs, and an rc file Bazel already reads. `$HOME` and
# `$GITHUB_WORKSPACE` are the scratch pair exported at the top of this script,
# so each case builds exactly the rc chain it is about.

# `$1`=action dir, `$2`=$RUNNER_TEMP, `$3`=READ_AUTH, `$4`=WRITE_AUTH. stderr
# folded into stdout, because the refusal is a `sys.exit` message.
run_guarded() {
    GITHUB_ACTION_PATH="$1" RUNNER_TEMP="$2" READ_AUTH="$3" WRITE_AUTH="$4" \
        bash -c "$WRITE_BODY" 2>&1
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

# Green: one source and a clean chain still writes. Without this the reds below
# would be satisfied by an action that refuses everything.
temp="$(scratch t7)"
run_guarded "$SUBJECT" "$temp" "$SECRET" "" >/dev/null
[ -f "$temp/bazel-cache.rc" ] || fail "a single source with a clean rc chain wrote nothing"
green "one source, empty rc chain -> $(ls -A "$temp" | tr '\n' ' ')"

# Red (a): both inputs. The `main` push presents the write credential alone,
# every other trigger the read credential alone — never the two together.
temp="$(scratch t7a)"
if output="$(run_guarded "$SUBJECT" "$temp" "$SECRET" "$SECRET")"; then
    fail "both credentials at once were accepted — the lane would send two headers"
fi
assert_refused "$output" "$temp" || fail "the refusal is not the contracted one: $output"
red "both inputs -> exit non-zero, nothing written: $(head -c 96 <<<"$output")…"

# And the proof that it is *this* guard refusing: with it removed the two-source
# state is reachable, and the one rc file then holds both a header and a helper.
mutant="$(scratch m7a)"
mutate '    if read_auth and write_auth:' '    if False:  # MUTANT' \
    <"$SUBJECT/write_rc.py" >"$mutant/write_rc.py"
temp="$(scratch t7ar)"
run_guarded "$mutant" "$temp" "$SECRET" "$SECRET" >/dev/null
grep -q -- '--remote_header=' "$temp/bazel-cache.rc" &&
    grep -q -- '--credential_helper=' "$temp/bazel-cache.rc" ||
    fail "the guard-less mutant did not produce the two-source rc — nothing is being measured"
red "without the guard one rc file carries $(wc -l <"$temp/bazel-cache.rc" | tr -d ' ') credential lines"

# Red (b): the source Bazel already reads. `.bazelrc` documents `~/.bazelrc` as
# where a developer's read credential lives, and a runner that acquired one
# would make every request carry two headers with nothing failing.
temp="$(scratch t7b)"
printf "build --remote_header='%s'\n" "$SECRET" >"$HOME/.bazelrc"
if output="$(run_guarded "$SUBJECT" "$temp" "$SECRET" "")"; then
    fail "a credential was written beside an existing \$HOME/.bazelrc header"
fi
assert_refused "$output" "$temp" || fail "the refusal is not the contracted one: $output"
grep -qF "$HOME/.bazelrc:1" <<<"$output" ||
    fail "the refusal does not name the file and line it found: $output"
red "an existing \$HOME/.bazelrc header -> refused, naming $HOME/.bazelrc:1 and not its value"

# Red (c): the same source one `import` away. This repository's `.bazelrc` ends
# in `try-import %workspace%/.bazelrc.user`, which is gitignored — so a guard
# reading only the files it was handed is green over the one rc file a developer
# is most likely to put a credential in.
rm -f "$HOME/.bazelrc"
printf 'try-import %%workspace%%/.bazelrc.user\n' >"$GITHUB_WORKSPACE/.bazelrc"
printf "build --remote_header='%s'\n" "$SECRET" >"$GITHUB_WORKSPACE/.bazelrc.user"
temp="$(scratch t7c)"
if output="$(run_guarded "$SUBJECT" "$temp" "$SECRET" "")"; then
    fail "a credential reached only through an \`import\` was not seen"
fi
assert_refused "$output" "$temp" || fail "the refusal is not the contracted one: $output"
grep -qF "$GITHUB_WORKSPACE/.bazelrc.user:1" <<<"$output" ||
    fail "the refusal does not name the imported file: $output"
red "a header behind \`try-import\` -> refused, naming .bazelrc.user:1"

# And the same red state, to show the chain reader is what found it: with the
# second guard removed the credential is written beside the header that was
# already there.
mutant="$(scratch m7c)"
mutate '    configured = other_credential_sources()' '    configured = []  # MUTANT' \
    <"$SUBJECT/write_rc.py" >"$mutant/write_rc.py"
temp="$(scratch t7cr)"
run_guarded "$mutant" "$temp" "$SECRET" "" >/dev/null
[ -f "$temp/bazel-cache.rc" ] ||
    fail "the chain-blind mutant wrote nothing — the red is not measuring the chain reader"
red "without the chain reader the rc lands beside the imported header"

# Green again, and the discriminator that keeps the reader from being a bare
# substring search: a commented-out header configures nothing, so it is not a
# source. A guard reporting it would red every tree carrying the documentation.
printf "# build --remote_header='%s'\n" "$SECRET" >"$GITHUB_WORKSPACE/.bazelrc"
rm -f "$GITHUB_WORKSPACE/.bazelrc.user"
temp="$(scratch t7d)"
run_guarded "$SUBJECT" "$temp" "$SECRET" "" >/dev/null
[ -f "$temp/bazel-cache.rc" ] || fail "a commented-out rc header was read as a live source"
green "a commented-out header is not a source; the rc was written"
rm -f "$GITHUB_WORKSPACE/.bazelrc"

printf '\nselftest: every property above shown red and green\n'
