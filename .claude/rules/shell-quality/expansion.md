---
# Read on demand through the index. This glob matches no file, so a client never auto-loads the file.
paths:
  - "**/.shell-quality-depth-on-demand"
title: Shell Quoting and Expansion
summary: The SH-EXP family. Quoting every expansion, argv built from parts, hostile file names, destructive paths, read loops, printf versus echo, test and arithmetic operators, the sh subset, eval, NUL-safe pipelines, dot-globs, parser counting, option parsing, special variable names, validated arithmetic, unmatched globs, literal text and final-state verification
---

# Shell Quoting and Expansion

Rows bind to ShellCheck 0.11.0, shfmt 3.13.1, bash 3.2.57 to 5.3, dash 0.5.12, busybox ash 1.35 and 1.37, mksh R59c,
ksh93u+m, yash 2.52, posh 0.14.1 and zsh 5.0.8 to 5.9, measured 2026-10-11, with fish 3.6.0 to 4.9.3 where a row names
it. macOS and BSD behaviour is `unverified: read only`.

Owns quoting and word splitting, argument lists, hostile file names, destructive paths, `read` loops, `printf` and
`echo`, test and arithmetic operators, brace and glob expansion, `eval`, NUL-safe pipelines, parser counting, option
parsing, special variable names and verifying all of these by final state. Kinds are INST, CI, TOOL, LIB, RC and EMIT as
the index defines them. Not owned here: the sh-versus-bash denylist (`$'..'`, `read -d`, `-a`/`-o`, `local`, pipefail)
is `SH-DIAL`. Empty arrays under `set -u` and the glob shopts are `SH-ERR`. The ShellCheck profile and file discovery
are `SH-GATE`. `${{ }}` injection is `SH-CI`. Per-target emitter quoters are `SH-EMIT`. Splitting rules of zsh, fish and
nu are `SH-ZSH`, `SH-FISH` and `SH-NU`. Secrets in argv and validation of dynamic names are `SH-SEC`.
SH-EXP-20 is retired and not reused.

Contents: [A. ShellCheck at Default Severity](#a-shellcheck-at-default-severity) ·
[B. The sh Subset](#b-the-sh-subset) · [C. A Grep Lists the Sites](#c-a-grep-lists-the-sites) ·
[D. A Hostile-Value Run Decides](#d-a-hostile-value-run-decides) · [E. A Reading Heuristic Decides](#e-a-reading-heuristic-decides) ·
[What Agents Get Wrong Here](#what-agents-get-wrong-here)

Confirmed 2026-10-11 (confirmation round): the cells and check scripts of this file were replayed on 25 fresh trees under dash 0.5.12-2 and busybox ash 1.37.0 (identical output on 4,144 paired runs), GNU grep 3.12, git 2.54.0 (2.43.0 in a container), ShellCheck 0.11.0 and shfmt 3.13.1, and the hits were read on a sample. Cell defect fixed: a dangling or deleted tracked file broke `files` (stat error from `shfmt -f`). `A1` prints SC2148 for shebang-less library files (43 on Homebrew/brew), which is the finding the rule expects, and the grep cells print real sites.

A hit of a listing cell is fixed or carries a same-line `tell-ok: REASON`. Pass is empty stdout and empty stderr. Each
row's Scope sentence names the kinds and shells it binds.

## A. ShellCheck at Default Severity

No `-S`, no `--severity`, in CI `run:` bodies too (actionlint at default severity). `-S warning` hides SC2086, SC2162,
SC2035, SC2059, SC2028, SC2029, SC2030, SC2031 and SC2012. zsh files cannot be linted (SC1071), so the block skips them.

```sh
for t in git xargs grep awk shfmt shellcheck; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
files() { git ls-files -z --cached --others --exclude-standard -- . ':(exclude,glob)**/*.zsh' | xargs -0 -r sh -c 'for f; do if [ -f "$f" ]; then printf "%s\0" "$f"; fi; done' _ | xargs -0 -r shfmt -f -- | tr '\n' '\0'; }
# A1
files | xargs -0 -r shellcheck --
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-EXP-01 | Double-quote every parameter, command and arithmetic expansion (`"$v"`, `"$(cmd)"`, `"$((n))"`, `"$@"`, `"${a[@]}"`). Write `"$*"` only to join on purpose. Leave an expansion bare only through SH-EXP-02. Never use an `IFS` prologue as a substitute. Scope: all kinds, sh, bash, ksh. zsh is a reading check. | Splitting and globbing hit every bare expansion. With `IFS=$'\n\t'` a glob value still expands (argc 3 for `*.txt`), an empty value still vanishes and a tab in a name still splits, on bash 3.2 to 5.3 (8 of 8). SC2086 is the top finding of a 41-repo corpus (4,387 of 13,865). | A1: SC2086 (info), SC2046 SC2048 SC2206 SC2207 (warning), SC2068 SC2070 (error). Watched red (measured 2026-10-11). | MUST |
| SH-EXP-02 | Build argv as an array (bash, ksh, zsh) or `set --` inside a function (sh), never as a string. One optional flag is `${v:+--flag}`. Split a delimited list only as `set -f; IFS=,; set -- $list; IFS=$old; set +f` with `# shellcheck disable=SC2086 # set -f active`. On a 3.2 floor fill an array with `while IFS= read -r l \|\| [ -n "$l" ]; do a+=("$l"); done < <(cmd)`, never `mapfile` (bash 4.0) or `a=($(cmd))` (also SH-DIAL-06). Scope: all kinds. `+=` needs bash 3.1, `set --` is POSIX. | A string re-parses every value: `name='a b; touch PWNED #'` ran the injected command on 11 of 11 POSIX-mode shells. ShellCheck does not model `set -f`, so SC2086 fires on the sanctioned split and needs the reasoned disable. Without `\|\| [ -n ]` the loop drops an unterminated last line (bash 3.2 to 5.3). | A1: SC2086 SC2206 SC2207, SC2294 for `eval "${arr[@]}"`. C1 for `eval`, where ShellCheck is silent. Reading: every `disable=SC2086` carries its reason. Watched red (measured 2026-10-11). | MUST |
| SH-EXP-03 | Treat file names as hostile. Glob as `./*` or put `--` before the operand, quote every name, guard an unmatched glob with `[ -e "$f" ] \|\| continue`. Never parse `ls`, never iterate `$(ls ...)` or `$(find ...)`. Take names from `find ... -exec cmd -- {} +` or a NUL stream (SH-EXP-14). Scope: all kinds. | A leading `-`, a space, a newline or a glob character breaks `for f in $(ls)` and `rm *`: 0 of 3 files copied and 3 of 3 left after `rm -f *` on 11 of 11 shells, and `for f in $(find ...)` sees 7 names for 5 files. The same class for non-file operands (`grep "$pat"`) is unmeasured. | A1: SC2035 SC2012 (info), SC2045 (error), SC2044 SC2038 SC2010 (warning). C2 lists `$(ls` and backtick `ls` loops. Watched red (measured 2026-10-11). | MUST |
| SH-EXP-04 | Root every `rm -rf`, `mv`, `chmod -R` and `find -delete` that uses a variable with `${v:?}`, as in `rm -rf -- "${dir:?}/out"`. Run `cd "$d" \|\| exit` before any relative delete. Never write `rm -rf "$v/"*` or `"$v"/*`. Scope: all kinds, most of all INST, CI and `trap` handlers. | An empty `$root` expands `"${root}/"*` to `/bin /boot /dev /etc ...` on 11 of 11 shells. `rm -rf "$home/packages"` is not flagged (empty gives `/packages`), hence SHOULD. | A1: SC2115 (warning, survives `-S warning`) on `"$v/"*`, `"$v"/*`, `"$v/"`, `"$v/$w"`, and SC2164. C3 lists the whole-variable shape SC2115 misses. Watched red (measured 2026-10-11). | MUST (SC2115 shapes), SHOULD (whole-variable or suffixed path) |
| SH-EXP-05 | Read lines with `while IFS= read -r line \|\| [ -n "$line" ]; do ...; done < "$file"`, with `</dev/null` on any child that reads stdin (`ssh`, `ffmpeg`, `cat`). Read NUL names in bash with `while IFS= read -r -d '' f; do ...; done < <(find ... -print0)`, under `LC_ALL=C` on bash 5.0 to 5.2. In sh let `find -exec sh -c '... "$1" ...' _ {} +` batch. Scope: all kinds. `-d ''` needs bash 3.2. | `read` without `-r` eats backslashes (11 of 11). The unterminated last line is lost without `\|\| [ -n ]` (11 of 11). A child steals the rest of the input. `read -d` is missing in dash, yash and posh, and bash 5.2.15 under `C.UTF-8` loses a multibyte record from `read -rd ''` (fixed in 5.3). | A1: SC2162 (info). B1: SC3045 for `read -d`. Reading for the last-line and stdin guards. Watched red (measured 2026-10-11). | MUST (`IFS= read -r`), SHOULD (the guards, `LC_ALL=C`) |
| SH-EXP-06 | Do not pipe into `while read` when a variable must survive. Feed the loop from a redirect or here-doc (sh) or `< <(cmd)` (bash 3.2+). Use `shopt -s lastpipe` only in a non-interactive bash 4.2+ script that says so. Scope: all kinds. | The loop runs in a subshell: the counter stays 0 on dash, busybox ash x2, mksh, yash, posh, bash --posix x2 and zsh sh-mode (9 of 11), and is 2 only on ksh93u+m and with a here-doc (11 of 11). | A1: SC2030 and SC2031 (info) on sh files. A `set -euo pipefail` header silences both on a bash file (ShellCheck 0.11.0, `set -e` alone does not), so C12 lists every pipe into `while`. Watched red (measured 2026-10-11). | MUST |
| SH-EXP-07 | Give `printf` a constant format and pass data through `%s`: `printf '%s\n' "$v"`, never `printf "$v"`. Use `%b` only for strings the script wrote. In sh use no `%n$` and no `%q`. Scope: all kinds. | `printf "$v"` with `v=100%` is an error in 10 of 11 shells and correct only in ksh93u+m. `%2$s` works only in ksh93u+m and zsh although POSIX.1-2024 specifies it. `%q` is an error in dash, ash and yash and its output differs per shell. | A1: SC2059 (info). B1: SC3050 for `%q`. Reading for `%n$`. Watched red (measured 2026-10-11). | MUST (constant format), SHOULD (no `%n$`, `%q` in sh) |
| SH-EXP-08 | Choose `[ ]` or `[[ ]]` by dialect, neither is a defect in bash. In `[[ ]]` quote the right side of `==` for a literal compare and leave it bare only for a glob. Keep a `=~` regex in a variable (`[[ $s =~ $re ]]`), never quoted. Compare numbers with `-lt`, not `<`. Never write `[ a == b ]` in a zsh file. Scope: bash, ksh, zsh. sh files follow SH-EXP-10. | `[[ abc =~ "a.c" ]]` is a literal compare and does not match on bash 3.2 to 5.3 (8 of 8) while `$re` matches. `[[ 10 < 9 ]]` is lexicographic. `[ a == a ]` is an error in zsh 5.0.8 to 5.9. busybox `[[` has no glob matching. | A1: SC2076 (warning), SC2071 SC2330 (error), SC2254. C9 for `==` in zsh files. Watched red (measured 2026-10-11). | SHOULD |

```sh
cmd="rsync -a $src $dst"; $cmd                  # wrong: the string is re-parsed, a space in $src splits it
set -- rsync -a -- "$src" "$dst"; "$@"          # right in sh, in bash: args=(rsync -a -- "$src" "$dst")
```

## B. The sh Subset

`shfiles` selects files whose first line is an sh shebang (spaces and CR tolerated). `nonbash` selects files whose
shebang is not bash. A shebang-less library sourced from sh is covered by A1 when it declares `# shellcheck shell=sh`.

```sh
for t in git xargs grep awk shfmt shellcheck; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
files() { git ls-files -z --cached --others --exclude-standard | xargs -0 -r sh -c 'for f; do if [ -f "$f" ]; then printf "%s\0" "$f"; fi; done' _ | xargs -0 -r shfmt -f -- | tr '\n' '\0'; }
shfiles() { files | xargs -0 -r awk 'FNR == 1 && /^#![ ]*\/(usr\/)?bin\/(env )?sh([ \r]|$)/ { print FILENAME }' | tr '\n' '\0'; }
nonbash() { files | xargs -0 -r awk 'FNR == 1 && /^#!/ && !/^#!.*bash/ { print FILENAME }' | tr '\n' '\0'; }
# B1
shfiles | xargs -0 -r shellcheck -s sh --
# B2
shfiles | xargs -0 -r grep -H -n -e 'echo -[neE]' -- | grep -v -e 'tell-ok: *[^ ]' -e 'userland-ok: *[^ ]'
# B3
shfiles | xargs -0 -r grep -H -n -e '@Q' --
# B4
shfiles | xargs -0 -r grep -H -n -e '{[0-9]*\.\.[0-9]*}' --
# B5
nonbash | xargs -0 -r grep -H -n -e '\[ [^]]* == ' --
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-EXP-09 | Print with `printf '%s\n' "$v"` for anything variable or containing a backslash. In sh-dialect code and any file sourced into sh or zsh never write `echo -e`, `echo -n`, `echo -E` or `echo` of a backslash or a variable. In a bash-only file a literal `echo -n` or `echo -e` is tolerated, `echo "$data"` never. This is a drafting and reading rule, B2 gates only the sh-file flag shape. Scope: INST, LIB shell=sh, RC under login sh, EMIT sh targets (MUST), TOOL bash (SHOULD). macOS `/bin/sh` echo: `unverified: read only`. | `\t` is interpreted by dash, mksh, yash, posh and native zsh but not by ash x2, ksh93, bash --posix and zsh sh-mode. `-e` is printed by dash, yash and posh. A data value of `-n` or `-e` vanishes in 9 of 11 shells. | B2 (sh files only, also SH-DIAL-12). B1: SC3037 (warning), SC2028 (info), `-s dash` SC3036. Nothing sees `echo "$hostile"`: read "is the argument data?". Watched red (measured 2026-10-11). | MUST (sh scope, data), SHOULD (literal flag in bash-only) |
| SH-EXP-10 | In `#!/bin/sh` files and `shell=sh` libraries use none of: `==` or `-v` in `[ ]`, `[[ ]]`, `=~`, `<<<`, `$(<file)`, `${x//a/b}`, `${x:0:1}`, `${x,,}`, `${x^^}`, `${x@Q}`, arrays, `<( )`, `**`, `++`. Use `[ ]` with `=`, `-n -z -e -f -d`, `&&` and `\|\|` between tests, `case`, `${x#pat}`, `${x%pat}`, `${#x}`, `${x:-d}`, `printf` and `$(cmd)`. B1 also enforces SH-DIAL-09, SH-DIAL-10, SH-DIAL-11 and SH-DIAL-17. Scope: INST, LIB shell=sh, EMIT sh targets, RC under login sh. Not bash TOOL files. | Each is an error or a silent wrong answer in at least one of dash, ash, yash, posh: `[ a == a ]` exits 2 in dash and posh. `$(<file)` is empty in dash, ash x2, yash and posh. busybox ash silently ignores `${x/#he/HE}`. `${x@Q}` works only in bash 4.4+ and mksh. | B1: SC3014 SC3010 SC3015 SC3011 SC3016 SC3034 SC3045 SC3057 SC3059 SC3060 SC3030 SC3054 SC2166. B3 for `@Q`, on which 0.11.0 is silent. B5 for `==` under a non-bash shebang. Watched red (measured 2026-10-11). | MUST |
| SH-EXP-11 | Keep sh arithmetic portable: no `**`, `++`, `--`, `10#` or leading-zero literals. Strip zeros from external input first: `y=${x#"${x%%[!0]*}"}; y=${y:-0}; echo $((y + 1))`. Keep values below 2^31 when mksh or ksh93 are targets. Scope: sh. In bash `10#` is the fix. | `**` errors in dash, mksh, yash, posh. `$((i++))` errors in dash. `$((010))` is 8 but 10 in mksh and ksh93. `$((08))` errors in dash, ash x2, posh, bash and zsh sh-mode. `10#` errors in dash and yash. mksh wraps at 32 bits and ksh93 turns to float. | B1: SC3018 (`++`), SC3019 (`**`), SC3006. The strip idiom prints `008->9 08->9 0->1 000->1 010->11 7->8` on all 11 shells. Watched red (measured 2026-10-11). | MUST (`**`, `++`, `10#`), SHOULD (the zero strip) |
| SH-EXP-12 | Count in sh with `i=1; while [ "$i" -le 3 ]; do ...; i=$((i + 1)); done`, never brace expansion. In bash `{1..$n}` never expands (SC2051) and `{01..03}` and `{1..7..3}` need bash 4.0. Scope: INST, LIB shell=sh, EMIT sh targets. | `for i in {1..3}` runs once with the literal word on dash, ash x2, mksh, yash, posh and zsh sh-mode (7 of 11). | B1: SC3009. B4 on sh-shebang files. Watched red (measured 2026-10-11). | MUST |
| SH-EXP-22 | Generate literal text with a quoted here-doc (`cat <<'EOT'`, where `<<-` strips tabs only). Keep trailing newlines of captured output with `x=$(cmd; printf x); x=${x%x}`. Use `$(<file)` and `$'..'` only in bash, ksh, zsh and mksh files. Scope: sh scope. `$'..'` in sh is also SH-DIAL-09. | Command substitution strips all trailing newlines (11 of 11). `$(<f)` is empty in dash, ash x2, yash and posh. `$'a\tb'` prints the literal `$a\tb` on dash 0.5.12, yash 2.52 and posh 0.14.1. | B1: SC3034 SC3003. Reading for the sentinel, a measured pin only. | CONSIDER |

## C. A Grep Lists the Sites

ShellCheck is silent or cannot discriminate on these. Each cell prints sites to read.

```sh
for t in git xargs grep awk shfmt; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
files() { git ls-files -z --cached --others --exclude-standard | xargs -0 -r sh -c 'for f; do if [ -f "$f" ]; then printf "%s\0" "$f"; fi; done' _ | xargs -0 -r shfmt -f -- | tr '\n' '\0'; }
# C1 (also SH-CORE-07 T5)
files | xargs -0 -r grep -H -n -w -e eval -- | grep -v -e 'eval "[$](' -e 'tell-ok: *[^ ]'
# C2
files | xargs -0 -r grep -H -n -e '[$](ls' -e 'for [A-Za-z_]* in .ls' -- | grep -v -e 'tell-ok: *[^ ]'
# C3
files | xargs -0 -r grep -H -n -E -e 'rm( -[a-zA-Z]+)+ (-- )?"[$]([A-Za-z_]|[{][A-Za-z_0-9]*[}])' -- | grep -v -e 'tell-ok: *[^ ]'
# C4 xargs without -0
files | xargs -0 -r grep -H -n -w -e xargs -- | grep -v -E -e 'xargs([[:space:]]+-[A-Za-z0-9]+)*[[:space:]]+-[A-Za-z0-9]*0'
# C5 xargs -0 without -r
files | xargs -0 -r grep -H -n -w -e xargs -- | grep -E -e 'xargs([[:space:]]+-[A-Za-z0-9]+)*[[:space:]]+-[A-Za-z0-9]*0' | grep -v -E -e 'xargs([[:space:]]+-[A-Za-z0-9]+)*[[:space:]]+-[A-Za-z0-9]*r'
# C6
files | xargs -0 -r grep -H -n -e '[[:space:]]\.\*' -- | grep -v -e 'tell-ok: *[^ ]'
# C7
files | xargs -0 -r grep -H -n -e 'sh -c "' -e 'ssh [a-z]* "' -- | grep -v -e 'tell-ok: *[^ ]'
# C8
files | xargs -0 -r grep -H -n -w -e getopt -- | grep -v -e 'tell-ok: *[^ ]'
# C9
git ls-files -z --cached --others --exclude-standard -- '*.zsh' | xargs -0 -r grep -H -n -E -e '(^|[^[])\[ [^]]* == ' --
# C10
git ls-files -z --cached --others --exclude-standard -- '*.zsh' | xargs -0 -r grep -H -n -e '^[[:space:]]*local path=' -e '^[[:space:]]*status=' -e '^[[:space:]]*local status=' -e '^[[:space:]]*fpath=' -e '^[[:space:]]*cdpath=' -e '^[[:space:]]*argv=' --
# C11 glob loops, read each for a guard
files | xargs -0 -r grep -H -n -E -e '(^|[;[:space:]])for[[:space:]]+[A-Za-z_][A-Za-z_0-9]*[[:space:]]+in[[:space:]]+[^;#]*[*?[]' -- | grep -v -e 'tell-ok: *[^ ]'
# C12
files | xargs -0 -r grep -H -n -E -e '[|][[:space:]]*while[[:space:]]' -- | grep -v -e 'tell-ok: *[^ ]'
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-EXP-13 | Run `eval` only on `"$(trusted-tool ...)"`, as in `eval "$(tool env --shell=bash)"`. Never run it on a variable, argument, file content or user-influenced string, and never execute `$cmd`. For a dynamic name use arrays, `set --`, or `printf -v` and `read` with a literal or validated name, every shell. A captured form needs a status check and `# tell-ok: captured from TOOL, status checked` (also SH-EMIT-14, SH-SEC-09). Scope: all kinds. | ShellCheck has no check for `eval "$string"` or a bare `$cmd`, at default severity or with `-o all`. The red plant ran injected code on 11 of 11 shells. | C1, then read each operand, which must be `"$( trusted command )"`. SC2294 covers only `eval "${arr[@]}"`. Watched red (measured 2026-10-11). | MUST |
| SH-EXP-14 | Pass names NUL-safe. First choice `find ... -exec cmd -- {} +`, otherwise `find ... -print0 \| xargs -0 -r cmd --` and `git ls-files -z \| xargs -0 -r`. Every `xargs` carries `-0` and `-r` or a comment saying why not. In minimal containers probe `command -v find xargs`. Scope: all kinds. BSD and macOS xargs: `unverified: read only`. | `xargs` without `-0` fails on a quote (`unmatched single quote`) and splits on spaces. `xargs -0` without `-r` still runs the command once on empty input on GNU and busybox 1.37. findutils is absent on 2 of 21 images (AL2023, openSUSE Leap 16.0). | C4 prints a missing `-0`, C5 a `-0` without `-r`. Both pass `-0r`, `-r0` and `-exec {} +`. Watched red (measured 2026-10-11). | MUST |
| SH-EXP-15 | Never glob `.*`. Use `.[!.]* ..?*` with an existence guard, or `find . -mindepth 1 -maxdepth 1` (GNU and busybox, `-mindepth` is not POSIX). Never feed `.*` to `rm`, `cp`, `chmod`, `chown` or a `for`. Scope: all kinds. | `.*` matches `.` and `..` on dash 0.5.12, busybox ash 1.35 and 1.37, yash 2.52 and bash 5.1 and older. bash 5.2+ (`globskipdots`), mksh, ksh93u+m, posh and zsh skip them. POSIX.1-2024 still allows both. | C6 (ShellCheck has no code). Watched red (measured 2026-10-11). Other `rm` builds than GNU were not run. | MUST |
| SH-EXP-16 | Count the parsers between the author and `exec`: local shell, ssh remote shell, `sh -c`, `docker exec`, YAML or workflow expression. Give each boundary one quoting layer, or pass the value outside the string: `sh -c 'cmd "$1"' _ "$v"`, `find -exec sh -c '... "$1" ...' _ {} +`, stdin or environment. When a string must cross, quote it for the same shell with a quoter that uses no command substitution and put `--` before the operand. Scope: all kinds. `${{ }}` into `run:` is SH-CI-03. | Each parser re-splits: `ssh host "touch $file"` with `file='a b;touch PWNED'` made 3 files on 11 of 11 shells. The command-substitution quoter loses trailing newlines (66 of 88 round trips), the quoter below keeps all 88 of 88. | C7, then read each interpolation. D1 asserts final state (SH-EXP-23). SC2156 (warning). SC2029 fires on the plant and on the quoted twin, so it is not admissible alone. Watched red (measured 2026-10-11). | MUST |
| SH-EXP-17 | Parse options with `getopts` (POSIX, no long options, reset `OPTIND=1` before a second parse, never parse its error text) or a `case "$1"` loop that honours `--`. Use GNU `getopt` only behind `getopt -T; [ $? -eq 4 ]`. Scope: all kinds. macOS and BSD getopt: `unverified: read only`. | All 11 shells parse `-a 'x y' -v -- file one` identically but print different error text. Enhanced getopt exists on util-linux and busybox 1.37 and fails on 5 of 21 images. | C8 lists the sites to check for the probe. Watched red (measured 2026-10-11). | SHOULD |
| SH-EXP-18 | Keep script variables lowercase and off the shell specials. In zsh never assign `path status argv pipestatus fpath cdpath manpath`. In bash never assign `UID GROUPS RANDOM SECONDS LINENO`. UPPER_CASE names are the environment namespace. Scope: zsh files and libraries sourced by zsh (RC, LIB). | `local path=/z` in zsh 5.9 leaves `ls` not found (rc 127), `status=1` is read-only, and `UID=5` is a readonly error in bash. | C10 over `*.zsh`. ShellCheck cannot parse zsh. Watched red (measured 2026-10-11). | SHOULD |
| SH-EXP-21 | Make unmatched-glob behaviour explicit. In `#!/bin/sh` and bash guard the loop (`[ -e "$f" ] \|\| [ -L "$f" ] \|\| continue`) or test the first word (`set -- ./*.x; [ -e "$1" ] \|\| [ -L "$1" ] \|\| die`) and give "no input" its own exit status (79). Never put `shopt -s nullglob` or `failglob` in a prologue, a sourced library or a function body. Where bash arrays are wanted, wrap the glob in a subshell or a save/restore pair (also SH-ERR-29, SH-ERR-30, SH-ERR-31). In zsh use `*.x(N)`. In fish expect an error. Scope: all kinds. | The pattern stays literal on all 11 POSIX-mode shells and bash 3.2 to 5.3 (the loop runs once). A dangling symlink fails `[ -e ]`. Under `nullglob` `ls *.zzz` becomes plain `ls`. zsh 5.0.8 to 5.9 aborts with `no matches found` and fish 3.6 to 4.9 errors. | C11 lists glob loops, read each for its guard. No lint code exists. A differential matrix run, not a red/green pair. | SHOULD |

```sh
ssh host "touch $file"                          # wrong: the remote shell re-splits $file
q() { # POSIX single-quote quoter, no $(...), so trailing newlines survive
    _r=$1 _o=
    while :; do case $_r in *\'*) _o="$_o${_r%%\'*}'\\''"; _r=${_r#*\'} ;; *) break ;; esac; done
    printf "'%s'" "$_o$_r"
}
ssh host "touch -- $(q "$file")"                # right
```

## D. A Hostile-Value Run Decides

D1 is `checks/expansion-hostile.sh` (copy `checks/` to `scripts/`). It runs the command under test with seven hostile
values through argument, stdin and environment. Exit 0 and no output is the pass, exit 1 lists the files that appeared.
`SUBJECT` is the executable under test.

```sh
for t in git xargs grep awk mktemp; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
# D1
sh scripts/expansion-hostile.sh "$SUBJECT"
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-EXP-19 | Validate external text before arithmetic. Before `$(( ))`, `(( ))`, `let`, `declare -i`, an array subscript or `[[ ... -eq ... ]]` (and `[ ... -eq ... ]` under mksh or posh) check the value with `case $x in '' \| *[!0-9]*) die ;; esac`, adding a sign branch if needed. Write names without `$` inside `(( ))`. Scope: bash (every version), mksh, posh. | `x='a[$(touch PWNED)]'; (( x > 1 ))` created `PWNED` on bash 3.2, 4.0, 4.2, 4.4, 5.0, 5.1, 5.2 and 5.3 (8 of 8), and on mksh R59c and posh 0.14.1 even through `[ -gt ]`. Not on dash, ash, ksh93u+m, yash or zsh for this payload. | D1 on every external input. No ShellCheck code exists as of 0.11.0 and ShellCheck did not go red. Reading: every arithmetic operand is a literal, a loop counter or has passed a `case` check. Watched red (measured 2026-10-11). | MUST |

```bash
(( n > 1 ))                                                  # wrong: n='a[$(cmd)]' runs cmd
case $n in '' | *[!0-9]*) echo "bad number" >&2; exit 2 ;; esac; (( n > 1 ))   # right
```

## E. A Reading Heuristic Decides

No command decides this row. The reviewer asks the question of each test.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-EXP-23 | Verify by final state, not exit status. A test for any quoting or injection rule feeds hostile values (`a b`, `-dash`, a newline, `'`, `$`, `a[$(cmd)]`) and asserts the resulting files, argv or bytes. Scope: test and verification code of every kind. | 23.4 to 47.0% of failing executions exit 0 while leaving the wrong final state (QuoteBench). | Reading: does the test assert files, argv or bytes, or only a status? D1 is the reference shape. Heuristic, no red run. | SHOULD |

## What Agents Get Wrong Here

Ranked by measured frequency, then by damage. The check column names the cell that catches it.

| Rank | Mistake | Rule | Check |
|---|---|---|---|
| 1 | Leaves `$var` bare, or relies on an `IFS=$'\n\t'` prologue to make that safe | SH-EXP-01 | A1 |
| 2 | Builds a command in a string and runs `$cmd` or `eval "$cmd"` | SH-EXP-02, SH-EXP-13 | A1, C1 |
| 3 | Writes `echo "$data"`, `echo -e` or `echo -n` in sh-dialect code | SH-EXP-09 | B2 |
| 4 | Puts external text in `(( ))` or `$(( ))` with no `case` check | SH-EXP-19 | D1 |
| 5 | Writes `rm -rf "$dir/"*` with no `${dir:?}` | SH-EXP-04 | A1, C3 |
| 6 | Pipes into `while read` and expects the counter afterwards, and trusts ShellCheck under `set -euo pipefail` | SH-EXP-06 | C12 |
| 7 | Loops over `$(ls)` or an unguarded glob | SH-EXP-03, SH-EXP-21 | A1, C2, C11 |
| 8 | Runs `xargs` without `-0 -r`, or globs `.*` | SH-EXP-14, SH-EXP-15 | C4, C5, C6 |
| 9 | Quotes the right side of `=~`, or writes `<` between numbers | SH-EXP-08 | A1 |
| 10 | Writes `printf "$v"`, `read` without `-r`, or `{1..3}` in sh | SH-EXP-07, SH-EXP-05, SH-EXP-12 | A1, B4 |
| 11 | Interpolates a value into `sh -c "..."` or an `ssh` command string | SH-EXP-16 | C7 |
| 12 | Tests a quoting rule by exit status alone | SH-EXP-23 | D1 |
