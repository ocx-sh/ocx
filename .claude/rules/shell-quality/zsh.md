---
# Read on demand through the index. This glob matches no file, so a client never auto-loads the file.
paths:
  - "**/.shell-quality-depth-on-demand"
title: Zsh Scripting
summary: The SH-ZSH family. The emulation prologue, bash residue that is wrong in native zsh, reserved variable names, word splitting and array base, glob no-match, pasted command blocks, errexit and EXIT-trap scope, bridging bash code, floors, startup files, the parse gate and zsh in CI and ShellCheck
---

# Zsh Scripting

Rows bind to zsh 5.0.8, 5.3.1, 5.8 and 5.9 on Linux (measured 2026-10-11), the 5.10 development tree where a row names it,
ShellCheck 0.11.0 and shfmt 3.13.1. macOS ships zsh 5.8 (macOS 12) and 5.9 (macOS 14), and that claim is
`unverified: read only`. Pinned decision (adopter may override): authored scripts target zsh 5.8, emitted rc code keeps 5.0.8.

Owns zsh code that is authored, sourced or pasted: the prologue, bash residue, option scope, globs, startup files, the
parse gate and the ShellCheck ban. Script kinds are INST, TOOL (dev tooling and the command blocks a human or agent pastes),
CI, LIB, RC and EMIT. Not owned here: `[[ ]]` operators and `echo` in sh code are `SH-EXP`, errexit and trap policy for sh and
bash is `SH-ERR`, the gate script and its zsh row are `SH-GATE`, workflow step shells are `SH-CI`, what an emitter writes
into an rc file is `SH-EMIT`, and dialect floors for sh and bash are `SH-DIAL`. The fleet authors almost no zsh, so this
family is mostly review and port work.

Contents: [Group 1: A Grep Decides](#group-1-a-grep-decides) · [Group 2: A Parse or a Run Decides](#group-2-a-parse-or-a-run-decides) ·
[What Agents Get Wrong Here](#what-agents-get-wrong-here)

Confirmed 2026-10-11 (confirmation round): the cells and check scripts of this file were replayed on 25 fresh trees under dash 0.5.12-2 and busybox ash 1.37.0 (identical output on 4,144 paired runs), GNU grep 3.12, git 2.54.0 (2.43.0 in a container), ShellCheck 0.11.0 and shfmt 3.13.1, and the hits were read on a sample. Cell defect fixed: the `.github/workflows` pathspec warning. Z2 to Z8 ran on the four fresh zsh trees (prezto, zinit, Homebrew/brew, jenv), and the hits were read.

## Group 1: A Grep Decides

Every command prints the violation and empty output is the pass. Judge `grep -L` by the listed files, never by exit status.
Z3 and Z4 are listings: a hit is a site to read, and the pass is every hit fixed or justified.

```sh
for t in git xargs grep; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
zfiles() { git ls-files -z --cached --others --exclude-standard -- ':(glob)**/*.zsh' ':(glob)**/*.zsh-theme' ':(exclude,glob)**/vendor/**'; }
# Z2 hard tier: spellings never right in native zsh
zfiles | xargs -0 -r grep -H -n -E -e 'read( +-[a-zA-Z]+)* +-[a-zA-Z]*[ap]( |$)' -e '\$\{!' -e '\$\{[A-Za-z_0-9]+(\^\^|,,|\^|,)\}' -e '(^|[[:space:];])(mapfile|readarray)[[:space:]]' -e '(^|[[:space:];])shopt[[:space:]]' -e 'export -f[[:space:]]' -e '(^|[;{(][[:space:]]*)(local|typeset|declare) +(-[a-zA-Z]+ +)?(path|fpath|cdpath|manpath|status|pipestatus|argv)(=|$|[[:space:];}])' -e '(^|[;{(][[:space:]]*)(path|fpath|cdpath|manpath|status|pipestatus|argv)=[^(]' -e '(^|[^[])\[ [^]]* == ' --
zfiles | xargs -0 -r grep -H -n -w -F -e PIPESTATUS -e BASH_SOURCE -e FUNCNAME --
# Z3 triage tier: legal in zsh, wrong in code sourced into sh or needs a reader
zfiles | xargs -0 -r grep -H -n -E -e 'echo -[a-zA-Z]*[en]' -e '\$\{[A-Za-z_0-9]+\[0\]\}' -e 'trap .* EXIT' --
zfiles | xargs -0 -r grep -H -n -w -F -e BASH_VERSION -e BASH_REMATCH --
# Z4 prologue: files without it (tests and vendored paths dropped), then a bare emulate with no options
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/*.zsh' ':(exclude,glob)**/vendor/**' ':(exclude,glob)**/test/**' ':(exclude,glob)**/tests/**' ':(exclude,glob)**/test-data/**' | xargs -0 -r grep -L -e 'emulate -L zsh' --
zfiles | xargs -0 -r grep -H -n -E -e 'emulate -L zsh[[:space:]]*$' -e 'emulate -L zsh[[:space:]]*;' --
# Z5 startup file: no setopt, no output, no input
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/.zshenv' ':(glob)**/zshenv' | xargs -0 -r grep -H -n -E -e '^[[:space:]]*(setopt|unsetopt|bindkey|autoload|compinit|source|read|print|printf|echo)([[:space:]]|$)' -e '^[[:space:]]*\.[[:space:]]' --
# Z8 CI and directive hygiene
git ls-files -z --cached --others --exclude-standard -- ':(glob).github/workflow[s]/*.yml' ':(glob).github/workflow[s]/*.yaml' ':(glob)**/action.yml' ':(glob)**/action.yaml' | xargs -0 -r grep -H -n -e 'shell: *zsh' --
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/*.zsh' ':(glob)**/.zshrc' ':(glob)**/.zshenv' | xargs -0 -r grep -H -n -E -e 'shellcheck +(shell|source)=(bash|sh|dash|ksh)' --
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-ZSH-01 | Start every executable zsh script with `#!/usr/bin/env zsh` and `emulate -L zsh -o err_exit -o no_unset -o pipe_fail` as the first statement. A function that calls `emulate -L zsh` restates `-o err_return -o no_unset -o pipe_fail` in the same call. A library sourced into a user shell quotes its builtins, sets `no_aliases` and restores in `{ body } always { restore }`. Scope: TOOL, LIB, zsh 5.8 to 5.9. Test and test-data paths are out of scope and an entry point that only sources is exempt. | A user's `.zshenv` and options change what a script means, and `emulate -L zsh` alone resets `ERR_EXIT`: `set -e; f() { emulate -L zsh; false; echo X }` prints `X` on 5.0.8, 5.3.1, 5.8 and 5.9. | Cell Z4 and probe P5. The Z4 list is a reading aid (26 of 29 files in one sample were pure, 300 of 301 in another were tests), so drop libraries in the quoted-builtins shape. Watched red (measured 2026-10-11): bare plant listed and bare-emulate line hit, prologue twin silent. | MUST |
| SH-ZSH-02 | Write the zsh spelling, never the bash one. Use `pipestatus`, `read -A`, `read -r '?prompt'`, `${(P)n}`, `${(k)h}`, `${(U)x}`, `${(L)x}`, `${(%):-%x}` or `${0:A:h}` for the file's own path (`$0` in a function is the function name), `funcstack`, `setopt` and `lines=("${(@f)$(<f)}")`. Never `PIPESTATUS`, `read -a`, `read -p`, `${!n}`, `${!h[@]}`, `${x^^}`, `BASH_SOURCE`, `FUNCNAME`, `shopt`, `mapfile`, `readarray`, `export -f` or `[ a == b ]` (also SH-EXP-08). Scope: TOOL, LIB, EMIT, zsh 5.0.8 to 5.9. | Each is empty, `bad substitution`, `bad option` or `command not found` in zsh, and `read -p` means coprocess. | Cell Z2. Watched red (measured 2026-10-11): 14 plant lines, twin 0. | MUST |
| SH-ZSH-03 | Never declare, `local` or scalar-assign `path`, `fpath`, `cdpath`, `manpath`, `status`, `pipestatus` or `argv`. Array forms (`path=(~/bin $path)`, `typeset -U path`) are the idiom. Scope: TOOL, LIB, zsh 5.0.8 to 5.9. | `local path=/z` makes `ls` vanish (status 127) on 5.3.1 to 5.9 and is a hard error on 5.0.8, and `status=1` is a read-only error on every build. | Cell Z2 (its last two patterns, which also catch the one-line form `f() { local path=/tmp; status=$?; }`) and probes P3, P4. Watched red (measured 2026-10-11). | MUST |
| SH-ZSH-04 | Do not rely on word splitting or 0-based arrays. Hold lists in arrays, split on purpose with `${=v}`, `${(z)v}` or `${(s: :)v}`, index from 1 (`${a[1]}`, `${a[-1]}`, `$#a`), remove an element with `a[1]=()` and never `unset 'a[1]'`, and keep `sh_word_split` and `ksh_arrays` inside a `local_options` function. Quote `"$v"` for the empty-argument case. Scope: TOOL, LIB, zsh 5.0.8 to 5.9. | `for w in $list` runs once on every build, `${a[0]}` is empty, and `unset 'a[1]'` leaves `count=3 elems=[\|y\|z]` ([zsh FAQ 3.1](https://zsh.sourceforge.io/FAQ/zshfaq03.html)). | Cell Z3 (`${name[0]}`) and probes P1, P2. No static check sees unquoted splitting (ShellCheck cannot parse zsh), so a reader flags an unquoted `$name` holding a list in `for`, `set --` or command position. A deliberate `${=v}` satisfies. Watched red (measured 2026-10-11). | MUST |
| SH-ZSH-08 | Keep `trap ... EXIT` out of function bodies. Arm it once at top level, or use `{ body } always { cleanup }` or `setopt local_traps`. Code shared with bash uses `emulate sh -c` or `setopt posix_traps`. Scope: LIB, TOOL, zsh 5.0.8 to 5.9. MUST in any file bash also sources, SHOULD in zsh-only files (resolves SH-ERR-27). | `mk_work() { work=$(mktemp -d); trap 'rm -rf "$work"' EXIT; }` deletes the directory it just made when the function returns, on every zsh build, while bash runs the trap at shell exit (8 of 8 bash versions, 3.2 to 5.3). | Cell Z3 (`trap .* EXIT`, judge function against top level) and probe P6. Watched red (measured 2026-10-11): plant line listed, twin silent, probe prints `trap-GONE`. | MUST |
| SH-ZSH-11 | Keep `.zshenv` silent and tiny. It holds exports only (no `setopt`, no stdin, no output) behind `[[ -o rcs ]] \|\| return 0`. PATH and login environment go in `.zprofile`, interactive setup in `.zshrc`. CI, agents and test harnesses run `zsh -f script`, and a harness that runs a zsh init sets `ZDOTDIR` to a throwaway directory (also SH-CORE-16). Scope: RC, CI, TOOL, zsh 5.0.8 to 5.9. | `.zshenv` is read by every zsh including `zsh -c` and scripts, so one that prints corrupts the stdout of every `zsh -c`. `zsh -f` skips every rc file and `zsh -l -c` reads `.zprofile` and `.zlogin` but not `.zshrc` ([zsh startup files](https://zsh.sourceforge.io/Doc/Release/Files.html)). | Cell Z5. Watched red (measured 2026-10-11): plant 4 lines, twin 0. | MUST |
| SH-ZSH-13 | Never run ShellCheck on a zsh file and never paste `# shellcheck shell=bash` into one. Scope: CI, TOOL, ShellCheck 0.11.0. | ShellCheck answers SC1071 (error, exit 1) on a defective file and on its compliant twin alike, `-s bash` gives 12 or more false positives (`SC2296`, `SC2082`, `SC2168`) on the twin and exits 0 on 200 of 572 corpus files, and `shell=zsh` is rejected (SC1103). | Cell Z8, second grep. Watched red (measured 2026-10-11): plant 2 lines, twin 0. | MUST |
| SH-ZSH-14 | Exercise zsh code from a `bash` step with `zsh -f ...` and never write `shell: zsh` in a workflow (step shells are `SH-CI`). Scope: CI, GitHub Actions. | GitHub Actions has no built-in zsh `shell:` and actionlint reports "shell name is invalid". | Cell Z8, first grep. `shell: [bash, dash, zsh]` as matrix data does not match. Watched red (measured 2026-10-11): plant 1 line, data twin 0. | SHOULD |
| SH-ZSH-15 | Scope option changes, register hooks the documented way and print portably. Use `setopt local_options ...` or `emulate -L` inside functions, `autoload -Uz add-zsh-hook; add-zsh-hook precmd fn` (also SH-EMIT-25), and `print -r --` or `printf` in files that may be sourced into sh. In `.zshrc`, code that may read the console runs above a Powerlevel10k instant-prompt block. Scope: RC, LIB, zsh 5.0.8 to 5.9. Native zsh `echo -e` is not a defect (also SH-EXP-09). | A bare `setopt err_return` in a function leaks to the caller, `echo` interprets backslashes in zsh, and Powerlevel10k redirects stdin and output during init. | Cell Z3 (`echo -[a-zA-Z]*[en]`), the rest is a reading heuristic. Watched red for Z3 (measured 2026-10-11). | SHOULD |

## Group 2: A Parse or a Run Decides

Run these on a real zsh. The floor is 5.0.8 (emitted code) and the ceiling is 5.9. Probe results below are the red output,
the compliant twin of each prints the intended value. Run the globs in an empty directory so they match nothing.

```sh
for t in git xargs zsh docker shfmt; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
# Z1 parse gate, zsh 5.3.1 or newer only. The docker line runs it in the 5.9 image on a host without one.
zsh -fc 'autoload -Uz is-at-least; is-at-least 5.3.1 "$ZSH_VERSION" || echo "PREFLIGHT: zsh older than 5.3.1"'
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/*.zsh' ':(exclude,glob)**/vendor/**' | xargs -0 -r -n 1 zsh -fn --
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/*.zsh' ':(exclude,glob)**/vendor/**' | xargs -0 -r -n 1 docker run --rm -v "$PWD:$PWD" -w "$PWD" zshusers/zsh:5.9 zsh -fn --
# advisory formatting on the explicit list, never shfmt -f discovery for zsh
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/*.zsh' ':(exclude,glob)**/vendor/**' | xargs -0 -r shfmt -ln zsh -i 2 -l --
# Z6 a script on the floor and ceiling builds. ZSCRIPT is its absolute path.
for v in 5.0.8 5.9; do docker run --rm -v "$PWD:$PWD" -v "$ZSCRIPT:$ZSCRIPT:ro" -w "$PWD" "zshusers/zsh:$v" zsh -f "$ZSCRIPT"; done
# Z6 probes, red output in the comment
for v in 5.0.8 5.9; do
  docker run --rm "zshusers/zsh:$v" zsh -f -c 'list="a b c"; n=0; for w in $list; do n=$((n+1)); done; print -r -- "n=$n"' # P1 n=1
  docker run --rm "zshusers/zsh:$v" zsh -f -c 'a=(x y z); print -r -- "[${a[0]}]"' # P2 []
  docker run --rm "zshusers/zsh:$v" zsh -f -c 'f() { local path=/tmp; ls >/dev/null; }; f; print -r -- "rc=$?"' # P3 rc=127, 5.0.8 refuses the assignment
  docker run --rm "zshusers/zsh:$v" zsh -f -c 'status=1' # P4 read-only variable: status
  docker run --rm "zshusers/zsh:$v" zsh -f -c 'set -e; f() { emulate -L zsh; false; print -r -- X; }; f' # P5 X
  docker run --rm "zshusers/zsh:$v" zsh -f -c 'mk() { w=$(mktemp -d); trap "rm -rf \"$w\"" EXIT; }; mk; [[ -d $w ]] && print alive || print GONE' # P6 GONE
  docker run --rm "zshusers/zsh:$v" zsh -f -c 'f() { local arr=(x y z); print -r -- "${#arr}"; }; f' # P7 5.0.8 only: unknown file attribute
  docker run --rm "zshusers/zsh:$v" zsh -f -c 'setopt err_exit; f() { true; }; ! f; print -r -- after' # P8 no output, exit 1
  docker run --rm "zshusers/zsh:$v" zsh -f -c "emulate sh -c 'w=\"a b c\"; n=0; for x in \$w; do n=\$((n+1)); done; echo n=\$n'" # P9 n=3 is the bridge working
  docker run --rm "zshusers/zsh:$v" zsh -f -c 'print -r -- *.zzz; print -r -- after' # P10 no matches found, no after
done
# Z7 a pasted block, text in BLOCK, run in a directory where no glob matches
zsh -f -c "$BLOCK"
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-ZSH-05 | Give every glob an explicit no-match policy. Use `(N)` or `setopt local_options null_glob` for an optional glob, quote a glob that is an operand of another tool, never `ls *.x(N)` (an empty list lists the cwd) and never `unsetopt nomatch` file-wide. Scope: TOOL, LIB, EMIT, zsh 5.0.8 to 5.9. | An external command with an unmatched glob is skipped (status 1), but `echo *.zzz`, `for f in *.zzz` and any function body abort the whole script. | Z6 on a script in an empty directory, expecting no `no matches found`, and probe P10. No static check exists, so it is a reading heuristic for the rest. Watched red (measured 2026-10-11): `(N)` twin passes. | MUST |
| SH-ZSH-06 | A pasted command block in a rule, skill or README quotes every glob it hands to a tool, prefers `grep -n -e PAT -- FILE` over a glob operand, and is tested in a directory where no glob matches. Scope: TOOL (docs), zsh 5.0.8 to 5.9. | A developer's login shell is often zsh, which aborts the block on an unmatched glob before `grep` runs, while bash runs it with a literal operand: `grep -n -e foo .bazelrc* .github/` prints `no matches found: .bazelrc*`. | Cell Z7 on the block in an empty directory, then in a populated one. No general static check exists, so the run is the judge. Watched red (measured 2026-10-11): unquoted plant status 1, quoted twin runs grep. | MUST |
| SH-ZSH-07 | Under `set -e` or `err_exit` never write `! cmd` as a statement. Write `if ! cmd; then` or `cmd \|\| ...`. Do not lean on exemptions the 5.10 development tree changes (`{ false && true } always { }`, a function ending in a compound `&&` or `\|\|`, `{ cmd; ... } \|\| true` under `err_return`). Scope: TOOL, LIB, zsh 5.0.8 to 5.9. The 5.10 half is SHOULD and was read from the [zsh README](https://raw.githubusercontent.com/zsh-users/zsh/master/README). | `set -e; f() { true }; ! f` exits 1 on 5.0.8 to 5.9.2 and continues on the 5.10 tree, and `! false \| true` exits 1 on 5.0.8 only, while `if ! f` is reached on every build. | Probe P8. Re-run on the 5.10 release. Watched red (measured 2026-10-11). | MUST |
| SH-ZSH-09 | Source POSIX or bash code into zsh only inside an emulation, after grepping it for residue the bridge cannot repair. Use `emulate sh -c '. lib.sh'` (sticky for the functions it defines) or a wrapper `w() { emulate -L sh; . lib.sh; call; }`. Defining natively and calling under `emulate -L sh` repairs nothing. Run Z2 and Z3 with `'*.sh'` in the pathspec of the library first, and any hit means a zsh sibling or a `${ZSH_VERSION-}` branch. Scope: LIB, RC, zsh 5.3.1 to 5.9. | Six native mismatches become `all-ok` under the bridge and the wrapper on 5.3.1, 5.8 and 5.9, but the bridge cannot repair `PIPESTATUS`, `read -a`, `local path`, `status`, `${x^^}`, `BASH_REMATCH`, `FUNCNAME`, `mapfile` or `export -f`, and on 5.0.8 even `local arr=(x y z)` fails. | Cells Z2 and Z3 on the library, and probe P9 (`n=3`) against the same loop run natively (`n=1`). Watched red (measured 2026-10-11). | MUST |
| SH-ZSH-10 | State and keep the floor with a `# zsh >= 5.8` header. Authored scripts target 5.8 and emitted rc code 5.0.8, where `local -a arr; arr=(...)` replaces `local arr=(...)` and `printf -v`, `typeset -A h=(...)`, `--emulate`, `wait -n` and named references are absent. Pinned decision (adopter may override). Scope: TOOL, LIB, EMIT, zsh 5.0.8 to 5.9. | 5.0.8 fails five constructs (`local a=(...)` until 5.1, `typeset -A h=([k]=v)` until 5.5). | Z6 on the floor and ceiling builds and probe P7. Watched red (measured 2026-10-11). | SHOULD |
| SH-ZSH-12 | Gate `*.zsh` with `zsh -fn` on 5.3.1 or newer, the two grep tiers (Z2, Z3), one runtime run on the floor and ceiling builds, and advisory `shfmt -ln zsh -i 2 -l` on an explicit file list. Never `zsh -n` on 5.0.8, never `shfmt -f` discovery for zsh (shfmt 3.13.0 or newer), and do not adopt zshellcheck 1.8.0. Scope: CI, TOOL, zsh 5.3.1 to 5.9 (also SH-GATE-11). | 5.0.8 rejects valid code with `bad substitution` or a parse error: `${a[1]}`, `typeset -gA w=()` inside `if` in a function and `${A[x]:=y}` flagged 3 pure files and 5 zsh-syntax-highlighting files. `shfmt -ln zsh` parses 498 of 572 exemplar files and misses `always`, `foreach`, `repeat` and `for k v in`. ShellCheck and zshellcheck 1.8.0 (7 of 17 plants) cannot discriminate zsh defects. | Cell Z1 and Z6. Watched red (measured 2026-10-11): a broken plant exits 1 on 5.9, the valid twin `a=(x y)` then `${a[1]}` exits 0 on 5.9 and exits 1 on 5.0.8, which is why the gate floor is 5.3.1. Shfmt on the same list reports the broken plant and is silent on the twin. | MUST |

```zsh
# wrong: bare emulate keeps no errexit, and $files is one word in zsh
emulate -L zsh
files="a.txt b.txt"
for f in $files; do print -r -- "$f"; done

# right: the full prologue, an array holds the list
emulate -L zsh -o err_exit -o no_unset -o pipe_fail
files=(a.txt b.txt)
for f in $files; do print -r -- "$f"; done
```

## What Agents Get Wrong Here

| Rank | Mistake | Rule |
|---|---|---|
| 1 | Writes bash in a `.zsh` file or a zsh block: `PIPESTATUS`, `read -a`, `${!n}`, `${x^^}`, `BASH_SOURCE`, `mapfile`, `shopt`. | SH-ZSH-02 |
| 2 | Hands an unquoted glob to `grep`, `ls` or `find` in a pasted block, which aborts under zsh before the tool runs. | SH-ZSH-06, SH-ZSH-05 |
| 3 | Expects `for w in $list` to split and `${a[0]}` to be the first element. | SH-ZSH-04 |
| 4 | Names a variable `path`, `status`, `argv` or `fpath`, which zsh reserves. | SH-ZSH-03 |
| 5 | Writes `emulate -L zsh` and assumes `set -e` survives it. | SH-ZSH-01 |
| 6 | Runs ShellCheck on a zsh file or adds `# shellcheck shell=bash` to quiet it. | SH-ZSH-13 |
| 7 | Sources a bash library into zsh with no emulation, or trusts the bridge to repair bash-only names. | SH-ZSH-09 |
| 8 | Puts `trap ... EXIT` in a function and loses the temp directory at return. | SH-ZSH-08 |
| 9 | Gates with `zsh -n` on 5.0.8, which rejects valid subscripts. | SH-ZSH-12 |
| 10 | Prints or calls `setopt` from `.zshenv`, which every `zsh -c` reads. | SH-ZSH-11 |
| 11 | Writes `shell: zsh` in a workflow, or `! cmd` as a statement under `set -e`. | SH-ZSH-14, SH-ZSH-07 |
