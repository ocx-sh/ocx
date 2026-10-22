---
# Read on demand through the index. This glob matches no file, so a client never auto-loads the file.
paths:
  - "**/.shell-quality-depth-on-demand"
title: Shell Userland Portability
summary: The SH-USER family. GNU-only and BSD-only forms of sed, readlink, realpath, date, stat, grep, find, mktemp and tar, probing timeout, flock, sha256sum and wget flags, xargs -r, LC_ALL=C on order-observable sorts, temp files and atomic replace, absent environment variables, the awk subset, Git Bash and the userland waiver
---

# Shell Userland Portability

Rows bind to GNU coreutils 8.22 to 9.12 (9.4 on Ubuntu 24.04), uutils coreutils 0.2.2 and 0.10.0 (the Ubuntu 25.10
and 26.04 default), busybox 1.37.0, Git Bash 5.2.15, ShellCheck 0.11.0 and shfmt 3.13.1, measured 2026-10-11 on
21 Linux base images. macOS and BSD behaviour is `unverified: read only`.

Owns the external-command floor: which forms of `sed`, `readlink`, `realpath`, `date`, `stat`, `grep`, `find`,
`mktemp`, `tar` and `wget` work on GNU, BSD, busybox and uutils, which tools must be probed before use, `xargs -r`,
locale pinning for `sort`, temp files and atomic replace, absent environment variables and Git Bash. Not owned
here: the shell language itself, `pipefail` and bash floors are `SH-DIAL`. Quoting and `-0` file lists are
`SH-EXP`. Traps and cleanup on signals are `SH-ERR`. Locking and secret handling are `SH-SEC`. Download flow and
tty-invariant `wget` options in installers are `SH-INST`. The Git Bash CI step is `SH-CI`. Writes to rc files are
`SH-EMIT`. The file list and the verification dialect are `SH-GATE`.

Contents: [Group 1: A Denylist Decides](#group-1-a-denylist-decides) · [Group 2: A Probe, Argument or Waiver Grep Decides](#group-2-a-probe-argument-or-waiver-grep-decides) · [Group 3: Measured, Read by Hand](#group-3-measured-read-by-hand) · [What Agents Get Wrong Here](#what-agents-get-wrong-here)

Confirmed 2026-10-11 (confirmation round): the cells and check scripts of this file were replayed on 25 fresh trees under dash 0.5.12-2 and busybox ash 1.37.0 (identical output on 4,144 paired runs), GNU grep 3.12, git 2.54.0 (2.43.0 in a container), ShellCheck 0.11.0 and shfmt 3.13.1, and the hits were read on a sample. Cell defect fixed: a dangling or deleted tracked file broke `files`. The `u` cells print real sites (for example GNU `cp --archive --parents` at `linuxdeploy-plugin-gtk.sh:64`).

## Group 1: A Denylist Decides

Cell `u01` prints `file:line:text` for each GNU-only or BSD-only form and drops comment lines, lines with `command -v`
and lines with a reasoned waiver. Empty stdout and stderr is the pass. A word boundary precedes each command name, so
`mystat -c` is not a hit, and a call hidden behind a variable is invisible. ShellCheck 0.11.0 reports 1 of 18 planted
forms at default severity (`echo -e`, SC3037, out of scope here, also SH-EXP-09), so this cell is the gate. Watched red
(measured 2026-10-11) on every listed form, empty on the compliant twin, identical under bash, dash, zsh and busybox 1.37.

```sh
for t in git xargs grep awk shfmt; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
files() { git ls-files -z --cached --others --exclude-standard | xargs -0 -r sh -c 'for f; do if [ -f "$f" ]; then printf "%s\0" "$f"; fi; done' _ | xargs -0 -r shfmt -f -- | tr '\n' '\0'; }
# u01
b='(^|[^[:alnum:]_.-])'
files | xargs -0 -r grep -H -n -E \
    -e "${b}sed[[:space:]].*-i([[:space:]]|\$)" \
    -e "${b}sed[[:space:]].*-z" \
    -e "${b}readlink[[:space:]]+-[A-Za-z]*[fm]" \
    -e "${b}realpath[[:space:]]+-[A-Za-z]*[ms]" \
    -e "${b}realpath[[:space:]].*--relative-" \
    -e "${b}date[[:space:]]+(.*[[:space:]])?-[dv]" \
    -e "${b}date[[:space:]].*-r[[:space:]]+[0-9]" \
    -e "${b}stat[[:space:]]+(.*[[:space:]])?-c" \
    -e "${b}stat[[:space:]]+-f[[:space:]]" \
    -e "${b}grep[[:space:]]+-[A-Za-z]*P" \
    -e "${b}grep[[:space:]].*--(include|exclude|exclude-dir)=" \
    -e "${b}find[[:space:]].*-printf" \
    -e "${b}mktemp[[:space:]].*(--suffix|--tmpdir|-t[[:space:]])" \
    -e "${b}head[[:space:]]+-n[[:space:]]*-[0-9]" \
    -e "${b}cut[[:space:]].*--complement" \
    -e "${b}tar[[:space:]].*--(exclude-vcs|owner|sort|zstd)" \
    -e "${b}cp[[:space:]].*--parents" \
    -e "${b}ls[[:space:]].*--color" \
    -e "${b}base64[[:space:]].*-D" \
    -e '--version.*grep' \
    -e '(^|[;&(]|[$][(]|then[[:space:]])[[:space:]]*which[[:space:]]' \
    -- | grep -v -e ':[0-9]*:[[:space:]]*#' -e 'command -v' -e 'userland-ok: *[^ ]'
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-USER-01 | Use no GNU-only or BSD-only external-command form without a probe or a `# userland-ok: REASON` on the same line. The forms and their replacements are in the table below. | Of 105 probes on 21 images only 11 forms are universal, each other form loses 1 to 4 images, and busybox 1.37 alone rejects 20. **Scope:** TOOL, CI, INST, EMIT, all listed userlands. | u01 | MUST |
| SH-USER-02 | Edit in place with `sed -i.bak EXPR f && rm -f f.bak`, or write a temp file and `mv` it onto the name. Never `sed -i ''` and never a bare `sed -i`. | GNU sed 4.9 exits 2 and busybox exits 1 on `sed -i '' ...` with the file unchanged. BSD sed takes the word after `-i` as the suffix, so a bare `-i` eats `-e` (unverified: read only). The `.bak` form returned rc 0 and the new content on Ubuntu 24.04, 26.04 and Alpine 3.22. **Scope:** TOOL, CI, INST. | u01 (`-i.bak` is not matched) | MUST |
| SH-USER-03 | Resolve a path with a helper that tries `realpath`, then `readlink -f`, then a `cd -P` and `readlink` loop. Never `readlink -m`, `realpath -m`, `realpath -s` or `--relative-to`. | `readlink -f` of an existing file and plain `realpath` work on 21 of 21 images, while `-m`, `-s` and `--relative-to` fail on busybox 1.37. POSIX.1-2024 has `readlink [-n]` and `realpath` with `-E` or `-e` only. macOS has `readlink -f` from 12.3 and `realpath` from 13.0 (unverified: read only). **Scope:** TOOL, CI, INST. | u01 for the flags. The helper below for behaviour: it passes with each strategy hidden in turn | MUST |
| SH-USER-04 | Use `command -v NAME`, never `which`. | `which` is absent on 8 of 21 images (Alma 8/9/10, AL2, AL2023, Arch, Fedora, Leap) and `command -v` works on 21 of 21. **Scope:** all kinds. | u01, and ShellCheck with `enable=deprecate-which` (SC2230, also SH-GATE-02) | SHOULD |
| SH-USER-05 | Never choose a code path by parsing a tool's `--version` banner or its stderr text. Probe the behaviour. | GNU 9.4 and uutils differ on 30 to 37 of 304 cases, from error text to `sort --version` printing `uutils coreutils`. Busybox grep, sort, date and awk reject `--version` and `dash --version` does not exist. **Scope:** all kinds. | u01 (`--version` followed by `grep`). Stderr-text parsing is a reading heuristic | MUST |
| SH-USER-11 | Create temp paths with `mktemp -d "${TMPDIR:-/tmp}/name.XXXXXX"`: six X, no suffix, no `-t`, no `--tmpdir`. Honour `TMPDIR` for large staging. | It is the one template GNU, busybox and uutils all accept (mode 700 or 600, `TMPDIR` honoured, a missing `TMPDIR` directory is an error with no fallback). A suffix or fewer than six X fails on busybox and no X fails on 21 of 21. Debian 13 mounts `/tmp` as tmpfs, up to 50% of memory. **Scope:** all kinds (also SH-ERR-15 and SH-ERR-16 for cleanup on signals). | u01 (the `mktemp` patterns) | SHOULD |

Portable replacements, with the images each incorrect form fails on (measured, the BSD column unverified: read only).
`readlink -f` of an existing file, `realpath`, `date -d @N`, `date -d 'YYYY-MM-DD HH:MM:SS'`, `stat -c`, `sort -V`,
`sort -h`, `mktemp -d`, `sha256sum`, `base64 -d` and `install -D` work on all 21 images.

| Incorrect | Correct | Fails on |
|---|---|---|
| `sed -i 's/a/b/' f`, `sed -i '' ...` | `sed -i.bak ... && rm -f f.bak` | the `''` form on GNU and busybox, the bare form on BSD |
| `sed -z`, `sed -r` | `sed -E`, and avoid `-z` | `-z` on Alpine and busybox |
| `readlink -m`, `realpath -m`, `-s`, `--relative-to` | the helper below | busybox 1.37 |
| `date -d '2020-01-02T03:04:05Z'`, `date -d '... +1 day'`, `date +%N` | `date -u -d @N` then `date -r N`, and `date +%s` | busybox rejects ISO `T..Z` and relative forms, weekday words differ between GNU and uutils |
| `stat -c %s f` | `wc -c < f`, or `stat -c` then `stat -f` for the mtime | on GNU `-f` means filesystem status, Git Bash returns 644 after `chmod 600` |
| `grep -P '\d'` | `grep -E '[0-9]'` | Alpine, busybox, Git Bash (rc 2) |
| grep with a recursive include or exclude option | `find DIR -type f -name 'GLOB' -exec grep -l x {} +` | busybox, and the error hides behind a pipe |
| `find -printf` | `find -print` | busybox. AL2023 and Leap have no find |
| `mktemp --suffix`, `-t`, `--tmpdir` | `mktemp "${TMPDIR:-/tmp}/name.XXXXXX"` | a suffix or fewer than six X on busybox |
| `head -n -1`, `cut --complement`, `ln -sr` | `sed '$d'`, `cut -c2-`, `ln -s ../a b` | BSD head rejects non-positive counts, busybox and Git Bash lack the other two |
| `tar --exclude-vcs --owner --sort --zstd` | plain `tar -cf`, probe for reproducible archives | busybox, AL2, AL2023. `--zstd` works on 2 of 21 images |
| `env -S`, `uname -o`, `base64 -D` | avoid, `uname -s`, `base64 -d` | `env -S` on busybox, AL2, Ubuntu 25.10 |

```sh
# wrong: GNU-only, fails on busybox, and the '' form fails on GNU
sed -i '' 's/a/b/' "$f"
here=$(readlink -m "$0")
# right: the .bak form, and one helper for symlink resolution
sed -i.bak 's/a/b/' "$f" && rm -f "$f.bak"
resolve_path() {
    command -v realpath >/dev/null 2>&1 && realpath "$1" 2>/dev/null && return 0
    (readlink -f / >/dev/null 2>&1) && readlink -f "$1" && return 0
    rp_p=$1 rp_n=0
    while [ -L "$rp_p" ] && [ "$rp_n" -lt 40 ]; do
        rp_t=$(readlink "$rp_p") || return 1
        case $rp_t in /*) rp_p=$rp_t ;; *) rp_p=$(dirname "$rp_p")/$rp_t ;; esac
        rp_n=$((rp_n + 1))
    done
    if [ -d "$rp_p" ]; then
        (cd "$rp_p" && pwd -P)
    else
        rp_d=$(cd "$(dirname "$rp_p")" 2>/dev/null && pwd -P) || return 1
        printf '%s/%s\n' "$rp_d" "$(basename "$rp_p")"
    fi
}
here=$(resolve_path "$0")
```

## Group 2: A Probe, Argument or Waiver Grep Decides

One block, cells `u06` to `u10` and `u16`. The two file-level cells (`u06`, `u10`) are cleared by one probe anywhere in
the file. `xargs` exits 123 when a child `grep -L` found nothing and busybox inverts it, so judge by output (also
SH-GATE-26). Each cell was watched red on its plant and empty on its twin (measured 2026-10-11, same four shells).

```sh
for t in git xargs grep awk shfmt; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
files() { git ls-files -z --cached --others --exclude-standard | xargs -0 -r sh -c 'for f; do if [ -f "$f" ]; then printf "%s\0" "$f"; fi; done' _ | xargs -0 -r shfmt -f -- | tr '\n' '\0'; }
# u06
files | xargs -0 -r grep -l -E -e '^[[:space:]]*(timeout|flock|sha256sum)[[:space:]]' -e '[;&(][[:space:]]*(timeout|flock|sha256sum)[[:space:]]' -e '[$][(](timeout|flock|sha256sum)[[:space:]]' -- | tr '\n' '\0' | xargs -0 -r grep -L -e 'command -v' -e 'userland-ok: *[^ ]' --
# u07
files | xargs -0 -r grep -H -n -E -e '^[[:space:]]*(rg|jq|python3)[[:space:]]' -e '[;&(][[:space:]]*(rg|jq|python3)[[:space:]]' -e '[$][(](rg|jq|python3)[[:space:]]' -- | grep -v -e ':[0-9]*:[[:space:]]*#' -e 'command -v' -e 'userland-ok: *[^ ]'
# u08
files | xargs -0 -r grep -H -n -E -e '(^|[|;&(])[[:space:]]*xargs([[:space:]]|$)' -- | grep -v -E -e ':[0-9]+:[[:space:]]*#' -e 'xargs([[:space:]]+-[^[:space:]]+)*[[:space:]]+-[^[:space:]]*r' -e '--no-run-if-empty' -e 'userland-ok: *[^ ]'
# u09
files | xargs -0 -r grep -H -n -E -e '^[[:space:]]*(sort|comm|join)[[:space:]]' -e '[^[:alnum:]_./-](sort|comm|join)[[:space:]]' -- | grep -v -e ':[0-9]*:[[:space:]]*#' -e 'LC_ALL=C' -e 'userland-ok: *[^ ]'
# u10
files | xargs -0 -r grep -l -E -e 'wget[^|;&]*--(https-only|secure-protocol)' -- | tr '\n' '\0' | xargs -0 -r grep -L -e 'wget --help' -e 'userland-ok: *[^ ]' --
# u16
files | xargs -0 -r grep -H -n -E -e 'userland-ok([^:]|$)' -e 'userland-ok:[[:space:]]*$' --
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-USER-06 | Probe a tool that is absent somewhere before using it: `timeout`, `flock`, `sha256sum`, `curl`, `wget`, `find`, `xargs`, `tar`. A file that calls `timeout`, `flock` or `sha256sum` contains a `command -v` probe or a `# userland-ok: REASON`. Treat `timeout` statuses 124, 137 and 143 as timed out. | `flock` is absent on AL2, AL2023, nixos/nix and Git Bash, `find` and `xargs` on AL2023 and Leap, `tar` on AL2 and AL2023. `sha256sum` is on 21 of 21 Linux images and `shasum` on none, macOS has the reverse (unverified: read only). Busybox `timeout` exits 143 and uutils `timeout -s KILL` exits 124 where GNU 9.4 gives 137. **Scope:** INST (MUST), CI, TOOL. | u06 | MUST |
| SH-USER-07 | Probe or declare every use of `rg`, `jq`, `python3`, `git` and `perl`: `command -v jq` tested at the top with `exit 69` when it fails, or `# userland-ok: requires jq (declared in WHERE)`. | Base images carry `rg` on 0 of 21, `jq` on 0, `git` on 1, `python3` on 3, `perl` on 9. Awk is the only scripting tool on 20 of 21. **Scope:** INST, TOOL (shipped checks), CI. Exempt dev scripts need the declaration, not a rewrite. | u07 (`rg`, `jq`, `python3`. `git` and `perl` by reading) | MUST (INST, shipped checks), SHOULD (dev tooling) |
| SH-USER-08 | Every `xargs` carries `-r` (or `--no-run-if-empty`). | GNU findutils, busybox 1.37 `xargs` and the Ubuntu 26.04 build run the command once on empty input (19 of 19 images that have `xargs`). `-r` is in POSIX.1-2024 and a no-op on BSD (unverified: read only). `-I{}` already skips empty input. **Scope:** all kinds. | u08 (requires `-r` as an `xargs` option, so `xargs rm -rf` is a hit) | MUST |
| SH-USER-09 | Put `LC_ALL=C` as a per-command prefix on each order-observable `sort`, `comm` and `join`, and on an `ls` whose order is captured. Do not `export LC_ALL=C` in a script that handles UTF-8 text. | uutils 0.10.0 uses ICU collation for any `*.UTF-8` or `*.utf8` locale name even when that locale is not installed, where GNU 9.4 silently falls back to bytes. On Ubuntu 26.04 (`ubuntu-latest` moves there by 2026-11-19) `LANG=en_US.UTF-8` sorts `a A b B`. A blanket export changes `${#x}`, `wc -m`, `awk length` and `cut -c` on UTF-8 text. **Scope:** TOOL, CI, INST. A `sort -u` feeding `wc -l` is order-independent, so waive it with a reason. | u09 | MUST |
| SH-USER-10 | Probe the flags before passing `--https-only` or `--secure-protocol` to `wget`: run `wget --help` and test it with `grep -q -e '--https-only'`, or prefer `curl` and reach `wget` only after the probe (also SH-INST-22). | Busybox 1.37.0 `wget` answers `unrecognized option: https-only` and `unrecognized option: secure-protocol=TLSv1_2` and fetches nothing. `curl` is on 9 of 21 base images, `wget` on 4, neither on the 9 Debian and Ubuntu images. rustup's `check_help_for` is the model. **Scope:** INST, busybox 1.37.0. | u10 | MUST (INST, bootstrap), SHOULD (elsewhere) |
| SH-USER-16 | A userland waiver is `# userland-ok: REASON` on the same line as the construct. A bare `userland-ok` or an empty reason is a defect. | The waiver replaces a probe, and without a reason it silences SH-USER-01 and SH-USER-06 to SH-USER-10 for good. **Scope:** all kinds. | u16 | SHOULD |

```sh
# wrong: sha256sum is absent on macOS (unverified: read only)
sha256sum "$f" | cut -d' ' -f1
# right: probe the tool, fall back along the measured chain
sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d' ' -f1
    elif command -v openssl >/dev/null 2>&1; then
        openssl dgst -sha256 "$1" | sed 's/^.*= //'
    else
        echo "sha256_of: no sha256 tool" >&2
        return 69
    fi
}
```

## Group 3: Measured, Read by Hand

No static check exists for these rows. Each names a measurement and a reading heuristic.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-USER-12 | Replace a file the script owns atomically: create the temp file with `mktemp` in the destination's own directory, then `mv` it onto the name. A user-owned or possibly symlinked config file follows the rc-file rule instead (also SH-EMIT-19). | A cross-file-system `mv` is copy and unlink. A reader saw 1966080 of 419430400 bytes at the destination mid-move and the inode changed, and `ln` across the boundary fails. Debian 13's tmpfs `/tmp` makes this the default case. **Scope:** TOOL, CI, INST. | Reading heuristic: a `mv` from `TMPDIR` or a default `mktemp` path into another directory | SHOULD |
| SH-USER-13 | In awk use `sub`, `gsub`, `substr`, `index`, `split` and `printf`. Probe `gensub`, `strftime`, ERE intervals `{n}` and `length(array)`. | Mawk is the awk on 9 of 21 images. `gensub` fails on all 9 and old mawk (Debian 11/12, Ubuntu 20.04/22.04) rejects `/a{3}/`. **Scope:** TOOL, INST. Relaxes when Debian 11/12 and Ubuntu 20.04/22.04 leave support. | Reading heuristic: scan awk programs for the four names | CONSIDER |
| SH-USER-14 | Default every possibly-absent variable before use: `TMPDIR` to `/tmp`, `XDG_CONFIG_HOME` to `$HOME/.config`, `USER` from `id -un`. In sh never use `$UID`, `$EUID`, `$HOSTNAME` or `$OSTYPE`, and never `set -u` before defaulting. | In a bare container only `HOME`, `HOSTNAME`, `PWD` and `PATH` are set. `USER`, `LOGNAME`, `SHELL`, `TMPDIR`, `XDG_*`, `LANG`, `LC_ALL`, `TERM` and `CI` are not, and `$UID` is empty under dash. **Scope:** INST, CI, TOOL. | Reading heuristic: print `env` inside an Ubuntu 24.04 and an Alpine 3.22 container and compare | SHOULD |
| SH-USER-15 | Under Git Bash do not rely on `ln -s`, `chmod 600`, case-sensitive names or LF output from native tools. Call `cmd` as `cmd //c` (conversion on) or `MSYS_NO_PATHCONV=1 cmd /c` (conversion off), never both (also SH-CI-14 for the CI step). | Git Bash 5.2.15: `cmd /c echo hi` hangs, `cmd //c echo hi` prints `hi`, `MSYS_NO_PATHCONV=1 cmd //c` prints the banner. `ln -s` copies without the symlink privilege, `chmod 600` leaves 644 and native tools emit CRLF through pipes. **Scope:** TOOL, CI on `windows-*`, Git Bash 5.2.15 (5.3.15 on `windows-2025`). | Host measurement on Windows only, not portable to a runner | CONSIDER |

## What Agents Get Wrong Here

Ranked by how likely a coding agent emits the form unprompted. Each row has one mechanical check.
1. **`sed -i 's/…/…/' f` written for GNU, or `sed -i ''` written for BSD.** Each fails on the other, and on busybox. Use the `.bak` form. `SH-USER-02`, cell `u01`.
2. **`xargs` without `-r`.** It runs the command once on empty input on GNU, busybox and uutils, so `xargs rm` with an empty list runs `rm` with no operand. `SH-USER-08`, cell `u08`.
3. **`readlink -f`, `readlink -m`, `realpath -m` or `--relative-to` for the script's own directory.** Busybox rejects the last three. Use the resolver helper. `SH-USER-03`, cell `u01`.
4. **`date -d`, `stat -c`, `grep -P`, `find -printf`, a recursive `grep` with include options, `mktemp --suffix` or `mktemp -t`.** Each passes on the author's Linux host and breaks on Alpine or macOS. `SH-USER-01`, `SH-USER-11`, cell `u01`.
5. **An unprobed `timeout`, `flock` or `sha256sum`.** Absent on macOS, AL2023, nixos/nix or Git Bash, and the call fails after the work has started. `SH-USER-06`, cell `u06`.
6. **`sort` or `comm` with no `LC_ALL=C` where order matters.** Equal on Ubuntu 24.04 and different on 26.04. `SH-USER-09`, cell `u09`.
7. **`which curl`.** Absent on 8 of 21 images. `SH-USER-04`, cell `u01`.
8. **`wget --https-only` or `--secure-protocol` with no probe.** Busybox `wget` prints usage and fetches nothing. `SH-USER-10`, cell `u10`.
9. **Choosing a branch by `tool --version | grep GNU`.** uutils and busybox answer differently or not at all. `SH-USER-05`, cell `u01`.
10. **`# userland-ok` with no reason, or on a different line from the construct.** `SH-USER-16`, cell `u16`.
