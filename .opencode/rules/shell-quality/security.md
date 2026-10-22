---
# Read on demand through the index. This glob matches no file, so a client never auto-loads the file.
paths:
  - "**/.shell-quality-depth-on-demand"
title: Shell Security
summary: The SH-SEC family. Secrets on argv, xtrace, PATH, bare sourcing, sudo environment, locks, option and pattern injection, dynamic names, sourced config, startup variables, parallel output, and git and cargo credential forms
---

# Shell Security

Rows bind to ShellCheck 0.11.0, shfmt 3.13.1, bash 3.2.57 to 5.3, zsh 5.0.8 to 5.9, dash 0.5.12, busybox ash 1.35 and 1.37, mksh R59c, posh 0.14.1, git 2.43.0 to 2.54.0 and cargo 1.93.1 to 1.99.0 on Linux (measured 2026-10-11). macOS and BSD behaviour is `unverified: read only`.

Owns the channels that carry a secret or hostile data other than through quoting: argv, xtrace, imported environment, PATH, sourced files, option positions, dynamic names, locks, the sudo environment, parallel output, and the credential forms of git and cargo. Not owned here: quoting and `eval` are `SH-EXP`. GitHub Actions expression injection and GitLab variables are `SH-CI`. Emitter quoting and idempotent PATH edits in emitted code are `SH-EMIT`. Temp files and atomic replace are `SH-USER`, cleanup traps are `SH-ERR`, and curl-to-shell installers are `SH-INST`. Gate wiring and the waiver tokens are `SH-GATE` and the index. PowerShell secrets are `PS-SEC`. Kinds are the index's: INST, TOOL, CI, LIB, RC, EMIT, GATE. A hit closes by a fix or by `# sec-ok: <reason>` on the same line, and a bare `# sec-ok:` is still a hit.

Contents: [ShellCheck Decides](#shellcheck-decides) · [A Grep or Script Fires](#a-grep-or-script-fires) · [A Grep Lists Sites and Reading Decides](#a-grep-lists-sites-and-reading-decides) · [What Agents Get Wrong Here](#what-agents-get-wrong-here)

Confirmed 2026-10-11 (confirmation round): the cells and check scripts of this file were replayed on 25 fresh trees under dash 0.5.12-2 and busybox ash 1.37.0 (identical output on 4,144 paired runs), GNU grep 3.12, git 2.54.0 (2.43.0 in a container), ShellCheck 0.11.0 and shfmt 3.13.1, and the hits were read on a sample. No cell defect. `s02-argv`, `s02-argv-yaml` and `s02-argv-var` print real credential-on-argv sites (for example `curl -u "$user"` at `zinit-autoload.zsh:1269`), and the listing cells print sites to read.

## ShellCheck Decides

Run the block from the repository root. Any `PREFLIGHT:` line means the host cannot judge, so fix the host and rerun. A cell passes with empty stdout and empty stderr. `s01-site-listing` prints sites to read. Every cell in this file printed the same output on busybox 1.37, GNU grep and findutils, and uutils (measured 2026-10-11).

```sh
for t in git xargs grep awk tr shfmt shellcheck; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
files() { git ls-files -z --cached --others --exclude-standard | xargs -0 -r sh -c 'for f; do if [ -f "$f" ]; then printf "%s\0" "$f"; fi; done' _ | xargs -0 -r shfmt -f -- | tr '\n' '\0'; }
yamls() { git ls-files -z --cached --others --exclude-standard -- '*.yml' '*.yaml'; }
# s01-rc
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/.shellcheckrc' ':(glob)**/shellcheckrc' | xargs -0 -r grep -H -n -E -e 'SC(2254|2053|2295|2035|2156)' --
# s01-site-listing
{ files; yamls; } | xargs -0 -r grep -H -n -E -e 'disable=[^#]*SC(2254|2053|2295|2035|2156)([^0-9]|$)' --
# s01-shellcheck
files | xargs -0 -r shellcheck -f gcc -i SC2254,SC2053,SC2295,SC2035,SC2156 --
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-SEC-01 | Fix the five ShellCheck findings that are injection and never disable them: SC2254 (unquoted `case` pattern), SC2053 (`[[ $x == $p ]]`), SC2295 (`${v#$p}`), SC2035 (a glob that can read as an option: `rm *`, `tar cf x *`), SC2156 (`find -exec sh -c '...{}...'`). No `.shellcheckrc` line names them. A per-site disable carries the same-line reason (also SH-GATE-05) and is listed for review. | `allow='*'` makes `case`, `[[ == ]]` and `${v##...}` match anything, and `rm *` deleted the directory behind a file named `-rf`. ShellCheck reports no hidden suppression and `-i` does not override an rc `disable`, so a hidden disable passes green. | `s01-rc` and `s01-shellcheck`: any hit is a violation. `s01-site-listing`: each inline disable needs its reason. Watched red (measured 2026-10-11): rc 1 line, ShellCheck 7 findings, site listing 3 lines, twin 0. | MUST |

- Scope SH-SEC-01: all kinds, sh and bash, ShellCheck 0.11.0 (other versions not re-run for these codes). ShellCheck is silent on the rest of this family even at `--enable=all`, so the other rows carry their own cells.

## A Grep or Script Fires

Same host rules as above. Copy `checks/secret-argv.sh` to `scripts/` (or set `SECRET_ARGV`). It is POSIX awk behind `#!/bin/sh` with the exit map 0 clean, 1 finding, 69 tool missing, 74 the scan itself failed, 78 not a work tree, 79 no files in scope.

```sh
for t in git xargs grep awk tr shfmt; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
SECRET_ARGV=${SECRET_ARGV:-scripts/secret-argv.sh}
[ -f "$SECRET_ARGV" ] || echo "PREFLIGHT: $SECRET_ARGV missing"
files() { git ls-files -z --cached --others --exclude-standard | xargs -0 -r sh -c 'for f; do if [ -f "$f" ]; then printf "%s\0" "$f"; fi; done' _ | xargs -0 -r shfmt -f -- | tr '\n' '\0'; }
yamls() { git ls-files -z --cached --others --exclude-standard -- '*.yml' '*.yaml'; }
s02() { xargs -0 -r grep -H -n -E -e '(-H|--header)[ =]+"[^"]*(Authorization|Proxy-Authorization|X-[A-Za-z-]*(Token|Key|Secret))[^"]*[$]' -e '(curl|wget)[^|#]*[[:space:]](-u|--user)[ =]+"?[$]' -e '--user[ =]+"?[$][{]?[A-Za-z_0-9]+[}]?:[$"]' -e '--(password|token|secret|api-key|client-secret)[ =]+.?[$]' -e 'docker login[^|#]*(-p|--password)[ =]+.?[$]' -e '[[:space:]](-d|--data[a-z-]*)[ =]+.?[^|#]*[$][{]?[A-Za-z_]*(TOKEN|SECRET|PASSWORD|PASSWD|API_KEY|CREDENTIAL)' -e '://[^/@[:space:]"]*:[$][{]?[A-Za-z_0-9]+[}]?@' -- | grep -v -e 'sec-ok: *[^ ]'; }
# s02-argv
files | s02
# s02-argv-yaml
yamls | s02
# s02-argv-var
sh "$SECRET_ARGV" . ':(exclude,glob)**/vendor/**'
# s04-path-literal
files | xargs -0 -r grep -H -n -E -e '(^|[^A-Za-z_])PATH=.?:' -e '(^|[^A-Za-z_])PATH=.?[.](:|[^A-Za-z0-9_/.-]|$)' -e '(^|[^A-Za-z_])PATH=.?[^[:space:]]*(::|:[.](:|[^A-Za-z0-9_/.-]|$)|:.?($|[[:space:];]))' -- | grep -v -e 'sec-ok: *[^ ]'
# s04-path-prefix-listing
files | xargs -0 -r grep -H -n -E -e '(^|[^A-Za-z_])PATH="?[^[:space:]"]*:[$][{]?PATH[}]?"?([[:space:];]|$)' -- | grep -v -e 'PATH:+' -e 'sec-ok: *[^ ]'
# s05-bare-source
files | xargs -0 -r grep -H -n -E -e '^[[:space:]]*(\.|source)[[:space:]]+"?[^/$<"[:space:]~-][^/"[:space:]]*"?[[:space:]]*([;&|#].*)?$' -- | grep -v -e 'sec-ok: *[^ ]' -e '^[^:]*[.]bats:'
# s06-sudo
files | xargs -0 -r grep -H -n -E -e 'sudo([[:space:]]+-[A-Za-z-]+)*[[:space:]]+(-[A-Za-z]*E[A-Za-z]*|--preserve-env)([[:space:]]|$)' -- | grep -v -e 'sec-ok: *[^ ]'
# s07-locks
files | xargs -0 -r grep -H -n -E -e '\[ -[ef] [^]]*(lock|[.]pid)[^]]*\][[:space:]]*([|][|]|&&)' -e 'if[[:space:]]+\[ ! -[ef] [^]]*(lock|[.]pid)' -- | grep -v -e 'sec-ok: *[^ ]'
# s13-git-extraheader-argv
{ files; yamls; } | xargs -0 -r grep -H -n -i -E -e 'git[^#]*[[:space:]]-c[[:space:]]+"?[^[:space:]]*extraheader[^#]*[$]' -e 'git[[:space:]]+config[^#]*extraheader[^#]*[$]' -- | grep -v -e 'sec-ok: *[^ ]'
# s13-git-extraheader-write-listing
{ files; yamls; } | xargs -0 -r grep -H -n -i -E -e 'git[[:space:]]+config[^#]*extraheader' -- | grep -v -e '--unset' -e '--get' -e '--remove' -e 'sec-ok: *[^ ]'
# s14-cargo-token-argv
{ files; yamls; } | xargs -0 -r grep -H -n -E -e 'cargo[[:space:]]+publish[^#]*--token' -e 'cargo[[:space:]]+owner[^#]*--token' -e 'cargo[[:space:]]+yank[^#]*--token' -e 'cargo[[:space:]]+login[[:space:]]+("?[$]|--registry[ =][^[:space:]]+[[:space:]]+"?[$])' -- | grep -v -e 'sec-ok: *[^ ]'
# s14-cargo-registry-token-with-registry-listing
{ files; yamls; } | xargs -0 -r grep -H -n -E -e 'CARGO_REGISTRY_TOKEN[^#]*--registry' -e '--registry[^#]*CARGO_REGISTRY_TOKEN' -- | grep -v -e 'sec-ok: *[^ ]'
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-SEC-02 | Never put a secret on a command line by any route. Routes: a literal, a variable (`h="Authorization: Bearer $T"` then `curl -H "$h"`), an array (`args+=(-H "Authorization: Bearer $T")`), `set -- "$@" -H ...`, a `read -s` variable, `-u "$user:$pw"`, `--password "$p"`, `--token "$t"`, `docker login -p "$p"`, `-d "...$TOKEN..."`, `docker run -e K=V`, `docker build --build-arg K=V`, `cargo publish --token`, and a credential inside a URL (`https://user:$TOKEN@host/`, which `git remote add` also writes to `.git/config`). Renaming the variable fixes nothing. Remedies in order: stdin, a mode-0600 file, then only when the tool offers neither the environment of that one command (`VAR=... cmd`, not exported). Right forms: `curl -K -`, `curl -H @-` or `-H @file` (curl 7.55.0 and later), `docker login --password-stdin`, `docker build --secret id=NAME,env=VAR`, `K=$V docker run -e K img`. git is SH-SEC-13 and cargo SH-SEC-14. | `/proc/PID/cmdline` is mode 0444 and `ps -o args=` prints it to every local user. The variable, array, `set --`, `read -s` and userinfo shapes each leaked in the process list while a single-line grep and ShellCheck saw nothing: 26 of 71 corpus lines and 32 of 80 held-out lines were invisible to the single-line cell. `docker run -e API_TOKEN=tok` leaked in the client argv, `-e API_TOKEN` with an environment prefix and `--secret` did not. 15 of 40 exemplar clones carry the single-line form. | `s02-argv`, `s02-argv-yaml`, `s02-argv-var`. None is a superset of another. Any hit is a violation. Watched red (measured 2026-10-11): `s02-argv` 10 lines, `s02-argv-yaml` 4, `s02-argv-var` 13 on one plant and 18 on the variable-shape plant, twin 0. A bare `# sec-ok:` is printed by `checks/secret-argv.sh` and a reasoned one is silent. | MUST |
| SH-SEC-04 | PATH never gets an empty, `.` or relative entry. Extend it as `PATH="$d${PATH:+:$PATH}"`, never `PATH="$d:$PATH"`, `PATH=$PATH:.`, `PATH=":$PATH"` or `PATH=`. A script that runs privileged or in a directory another user can write sets `PATH=/usr/bin:/bin` first, or refuses with exit 78 (ConfigError) when `:$PATH:` contains `::` or `:.:`, or calls standard tools through `command -p`. | A zero-length PATH entry is the current directory. `PATH=:/usr/bin`, `/usr/bin:` and `/usr/bin::/bin` ran a planted `./planted` on dash, ash, mksh, ksh93u+m, yash, posh, bash 3.2 to 5.3 and zsh 5.9. `PATH="$d:$PATH"` with an empty inherited PATH leaves a trailing empty entry, `${PATH:+:$PATH}` does not. The bash 5.3 NEWS item documents what 3.2 already did. | `s04-path-literal`: any hit is a violation. `s04-path-prefix-listing`: each `PATH="$x:$PATH"` without `PATH:+` is safe only when PATH cannot be empty, so read it. Watched red (measured 2026-10-11): 4 and 4 lines, twin 0. | MUST |
| SH-SEC-05 | `.` and `source` name a path: `. ./lib.sh`, `. "$dir/lib.sh"`. Never a bare name (`. lib.sh`, `. .env`, `source env.sh`) unless it is meant to be found on a PATH you control, and then the line carries `# sec-ok: found on PATH by design` (git's `. git-sh-setup`). An executed script derives its directory with `dir=$(cd "$(dirname "$0")" && pwd)`. A sourced bash library uses the directory of `${BASH_SOURCE[0]}`, because a sourced file's `$0` is the caller's. A sourced POSIX library takes its directory from the caller. | A bare name is sourced from the current directory by bash 3.2 to 5.3, busybox ash 1.37, yash 2.52 and `bash 3.2 --posix`, and dash 0.5.12, mksh, ksh93u+m, posh and zsh refuse, so the bug hides on Debian CI. A planted `lib.sh` or `.env` then runs as code. | `s05-bare-source`: any hit is a violation or carries the waiver. `.bats` files are skipped. Watched red (measured 2026-10-11): 5 lines (`. .env`, `source env.sh`, `. lib.sh && init`, `. lib.sh`, `source conf.env`), twin 0. | MUST |
| SH-SEC-06 | Do not write `sudo -E` or a bare `sudo --preserve-env`. Name the variables: `sudo --preserve-env=VAR cmd` (sudo 1.9.15p5, sudo-rs 0.2.7 and later) or `sudo env VAR="$VAR" cmd`. Probe with `sudo -n`, give a password on stdin with `sudo -S -p ''`, and decide on exit codes, never on message text. | sudo-rs 0.2.8 and 0.2.13 answer bare `-E` with `warning: preserving the entire environment is not supported, '-E' is ignored`, exit 0, and run the command without the variable. The denial text also differs from sudo's. | `s06-sudo`: any hit is a violation. Watched red (measured 2026-10-11): 3 lines including `sudo -nE`, twin 0. | MUST |
| SH-SEC-07 | A lock is `mkdir`, `set -C` (noclobber) or `ln`, never test-then-create such as `[ -e lock ]` followed by `touch lock`. Do not break a stale lock automatically: print the holder (`lock/pid`) and exit 75 (TempFail). Use `flock` only after `command -v flock` (also SH-USER-06), and remember that a backgrounded child inheriting the lock descriptor keeps it held after the parent exits. | 30 contenders over 5 rounds: test-then-create gave 8 to 30 winners per round on every shell, `mkdir`, noclobber and `ln` gave exactly one on 10 of 11. Stale takeover by `rmdir` plus `mkdir` or by `mv` gave 1 to 7 winners. | `s07-locks`: any hit is a violation. Watched red (measured 2026-10-11): 2 lines, twin 0. | MUST |
| SH-SEC-13 | git credentials never go in a URL, in `git -c http.extraheader=...`, or in `git config http.extraHeader` with a secret value. Use, strongest first: a `GIT_ASKPASS` script that reads a mode-0600 file, then `credential.helper store --file=F` primed on stdin by `git credential approve` with F at mode 600 and deleted at job end, then only when no file is acceptable `GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=http.extraHeader GIT_CONFIG_VALUE_0="Authorization: Bearer $T" git ...` in the environment of the one command (git 2.31 or later). Never enable `GIT_TRACE_CURL` in a step whose secret rides a header other than `Authorization` or `Proxy-Authorization`, and never set `GIT_TRACE_REDACT=0`. | No remedy puts the token on argv except the forms in the table below. A userinfo URL is argv in three processes plus `.git/config`, and `git -c http.extraheader=...` is argv in the parent. `GIT_TRACE_CURL` redacted `Authorization` (0 token occurrences) and printed a custom `X-Token` header 5 times. | `s13-git-extraheader-argv`: any hit is a violation. `s13-git-extraheader-write-listing`: each write is closed by an unset in the same job or a waiver. Userinfo URLs are `s02-argv` and `s02-argv-var`. Watched red (measured 2026-10-11): 3 and 2 lines, twin 0. | MUST |
| SH-SEC-14 | cargo tokens never ride `--token` (`cargo publish`, `owner`, `yank`), and `cargo login` never takes the token as an argument. crates.io: `CARGO_REGISTRY_TOKEN=$T cargo publish`. Any other registry: `CARGO_REGISTRIES_NAME_TOKEN=$T cargo publish --registry name`, with the name upper-cased and `-` becoming `_`. `CARGO_REGISTRY_TOKEN` does not authenticate another registry (exit 101, `no token found`). `cargo login --registry name` reads the token on stdin and leaves a mode-600 plaintext `credentials.toml`, so delete it in CI. | `--token` is in the cargo process argv on 1.93.1 to 1.99.0. The environment forms and `cargo login` on stdin keep it off argv. The silent-failure shape is `CARGO_REGISTRY_TOKEN` next to `--registry`. | `s14-cargo-token-argv`: any hit is a violation. `s14-cargo-registry-token-with-registry-listing`: each site needs the per-registry variable. Watched red (measured 2026-10-11): 4 and 1 lines, twin 0. | MUST |

- Scope SH-SEC-02: INST, TOOL, CI, GATE. Linux and same-user Windows (WinPS 5.1, `Get-CimInstance Win32_Process`) measured, macOS `unverified: read only`. `/proc/PID/environ` is mode 0400 (same uid and root), which is why the environment ranks last. The wget and `gh auth login --with-token` stdin forms were read, not run.
- Scope SH-SEC-04: all kinds. The cwd-search claim was run on the interpreters named. `command -p` as a fix was run on bash only.
- Scope SH-SEC-05: INST, TOOL, CI, LIB, sh and bash on Linux. `. .env` falls back to the cwd on exactly busybox ash 1.37, yash 2.52 and `bash 3.2 --posix`.
- Scope SH-SEC-06: INST, TOOL, CI on Linux. Whether a stock Ubuntu 25.10 or later makes sudo-rs the default `sudo` is unverified here. The behaviour is a measurement, not a plant and twin.
- Scope SH-SEC-07: any script that claims mutual exclusion, sh and bash, local filesystems (NFS not measured). Amazon Linux 2023 has no `flock`. macOS none (unverified: read only).
- Scope SH-SEC-13: INST, TOOL, CI, git 2.43.0 to 2.54.0 on Linux. Windows Git Credential Manager and macOS Keychain helpers are `unverified: read only`.
- Scope SH-SEC-14: TOOL, CI, cargo 1.93.1 to 1.99.0. Credential providers (`cargo:libsecret`, `cargo:macos-keychain`) were not run.

`s02-argv-var` is file-level taint over `curl`, `wget`, `git`, `docker`, `gh` and `cargo`. It misses a secret held in a non-secret-shaped name such as `T`, a positional `$1`, function scope, `eval`, sourced files, embedded Python or Node, heredoc semantics, lines over 1500 characters and every other tool. A pass is not proof of absence, so keep the reading step: does a variable that holds a secret reach an argument? A bare `sec-ok:` waiver is a finding in all three cells.

```sh
# wrong: the token is world-readable in /proc/PID/cmdline for the life of the process
curl -H "Authorization: Bearer $TOKEN" "$url"
# right: the header rides stdin
printf 'header = "Authorization: Bearer %s"\n' "$TOKEN" | curl -K - "$url"
```

How each git form leaks (git 2.43.0 to 2.54.0, each right form authenticated against a stub server):

| Form | argv | environ (0400) | at rest |
|---|---|---|---|
| userinfo URL | git, `git remote-http` | none | `.git/config` holds it |
| `git -c http.extraheader="Authorization: Bearer $T"` | git (the parent) | git, git-remote-http | none |
| `GIT_ASKPASS` script, token from the environment | none | git, git-remote-http | none |
| `GIT_ASKPASS` script, token from a 0600 file | none | none | the file |
| `credential.helper store --file=F` | none | none | F, mode 600, plaintext |
| `credential.helper cache --timeout=60` | none | none | daemon memory and a Unix socket |
| `GIT_CONFIG_COUNT`, `KEY_0`, `VALUE_0` | none | git, git-remote-http | none |
| `git config --global http.extraHeader "Authorization: Bearer $T"` | none | none | `~/.gitconfig` holds it |

The `GIT_CONFIG_*` triple keeps the token off argv but both children carry it in environ. If a same-uid process is in the threat model, only askpass-from-file or `store` keep it out of every environ. `cache` is Unix-only and `unverified: read only` on Windows.

How each cargo form leaks (four cargo versions against a stub registry with an authenticated PUT):

| Form | argv | environ | at rest | publish |
|---|---|---|---|---|
| `cargo publish --registry stub --token "$T"` | cargo | none | none | exit 0 |
| `CARGO_REGISTRIES_STUB_TOKEN="$T" cargo publish --registry stub` | none | cargo | none | exit 0 |
| `cargo login --registry stub` with the token on stdin, then `cargo publish` | none | none | `credentials.toml`, mode 600 | exit 0 |
| `CARGO_REGISTRY_TOKEN="$T" cargo publish --registry stub` | none | none | none | exit 101 |

## A Grep Lists Sites and Reading Decides

Same host rules. Every cell here prints sites to read. The pass is every hit fixed or waived with a reason. A grep cannot decide the bracket, the validator or the owner, so the reading step in each row is part of the rule.

```sh
for t in git xargs grep awk tr shfmt; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
files() { git ls-files -z --cached --others --exclude-standard | xargs -0 -r sh -c 'for f; do if [ -f "$f" ]; then printf "%s\0" "$f"; fi; done' _ | xargs -0 -r shfmt -f -- | tr '\n' '\0'; }
# s03-xtrace-listing
files | xargs -0 -r grep -H -l -E -e '^[[:space:]]*set[[:space:]]+(-[a-z]*x|-o[[:space:]]+xtrace)' -e '^#!.*[[:space:]]-[a-z]*x([[:space:]]|$)' -- | tr '\n' '\0' | xargs -0 -r grep -H -n -E -e '[$][{]?[A-Za-z_]*(TOKEN|SECRET|PASSWORD|PASSWD|API_KEY|CREDENTIAL)' -- | grep -v -e 'sec-ok: *[^ ]'
# s08-operands-listing
files | xargs -0 -r grep -H -n -E -e '(^|[;&|(]|then |do )[[:space:]]*(rm|mv|cp|chmod|chown|tar|ln)[[:space:]]+([^|;&#]*[[:space:]])?"[$](\{?[0-9]+\}?|[@*])"([[:space:]]|$)' -e '(^|[;&|(])[[:space:]]*(ssh|scp|rsync)[[:space:]]+"?[$]\{?[A-Za-z_0-9@*]+\}?"?[[:space:]]' -e 'git (clone|ls-remote|fetch|pull) +"?[$][{A-Za-z]' -- | grep -v -e ' -- ' -e 'sec-ok: *[^ ]'
# s09-dynamic-names-listing
files | xargs -0 -r grep -H -n -E -e '(^|[[:space:];&|(])(declare|typeset|local)([[:space:]]+-[A-Za-z]+)*[[:space:]]+"[$]' -e '(^|[[:space:];&|(])(declare|typeset|local)[[:space:]]+-n[[:space:]]+[A-Za-z_]+="?[$]' -e 'printf -v "[$]' -e '(^|[[:space:];&|(])read( -[A-Za-z]+( [0-9A-Za-z.]+)?)*[[:space:]]+"[$]' -e '(\[\[|\[|test)[[:space:]]+-v[[:space:]]+"?[$]' -e '[$][{]![A-Za-z_][A-Za-z_0-9]*[}]' -e '^[[:space:]]*export[[:space:]]+(-[A-Za-z]+[[:space:]]+)*"[$][{]?[A-Za-z_]' -e '[{;&(][[:space:]]*export[[:space:]]+(-[A-Za-z]+[[:space:]]+)*"[$][{]?[A-Za-z_]' -e '^[[:space:]]*readonly[[:space:]]+(-[A-Za-z]+[[:space:]]+)*"[$][{]?[A-Za-z_]' -e '[{;&(][[:space:]]*readonly[[:space:]]+(-[A-Za-z]+[[:space:]]+)*"[$][{]?[A-Za-z_]' -e '^[[:space:]]*unset[[:space:]]+(-[A-Za-z]+[[:space:]]+)*"?[$][{]?[A-Za-z_]' -e '[{;&(][[:space:]]*unset[[:space:]]+(-[A-Za-z]+[[:space:]]+)*"?[$][{]?[A-Za-z_]' -- | grep -v -e 'sec-ok: *[^ ]'
# s10-source-variable-listing
files | xargs -0 -r grep -H -n -E -e '^[[:space:]]*(\.|source)[[:space:]]+"?[$][{]?[A-Za-z_][A-Za-z_0-9]*[}]?"?[[:space:]]*([;&|#].*)?$' -- | grep -v -e 'sec-ok: *[^ ]'
# s11-env-export-listing
files | xargs -0 -r grep -H -n -E -e '^[[:space:]]*(export|declare -x|typeset -x)[[:space:]]+(-[A-Za-z]+[[:space:]]+)*(BASH_ENV|ENV|SHELLOPTS|BASHOPTS|PS4|POSIXLY_CORRECT|CDPATH|GLOBIGNORE)([=[:space:]]|$)' -e '(^|[;&|(])[[:space:]]*export[[:space:]]+-f[[:space:]]' -- | grep -v -E -e 'sec-ok: *[^ ]' -e 'export[[:space:]]+-[A-Za-z]*n'
# s12-wait-listing
files | xargs -0 -r grep -H -n -E -e '^[[:space:]]*wait[[:space:]]*(#.*)?$' -- | grep -v -e 'sec-ok: *[^ ]'
# s12-xargs-p-listing
files | xargs -0 -r grep -H -n -E -e 'xargs[^|;&]*[[:space:]]-[A-Za-z0-9]*P[A-Za-z0-9]*' -e 'xargs[^|;&]*--max-procs' -- | grep -v -e 'sec-ok: *[^ ]'
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-SEC-03 | No `set -x`, `set -o xtrace`, `bash -x` or `-x` shebang in a file that expands a secret variable. To trace anyway, bracket the secret line with `{ set +x; } 2>/dev/null` before it and `set -x` after it (fish: `set -e fish_trace` first). The `x` of a copied `set -euxo pipefail` counts. | xtrace prints the expanded value on 11 of 11 POSIX-family shells, including `export API_TOKEN="$TOKEN"`. The brace bracket leaked nothing. `BASH_XTRACEFD` (bash 4.1 and later) only moves the trace into a file that holds the token, and fish 3.6.0 to 4.9.3 `fish_trace` prints environment values. | `s03-xtrace-listing`: files that enable xtrace, then their lines that expand a secret-named variable. Discard a path-valued name (`API_KEY_FILE`) and a `set -x` inside a heredoc that writes another script, and close the rest by the bracket or by removing the trace. Watched red (measured 2026-10-11): 1 line, twin 0. | MUST |
| SH-SEC-08 | Data never reaches an option position. Put `--` before every variable operand of a program that runs code, opens connections or writes files (`ssh -- "$host"`, `scp`, `rsync`, `git clone -- "$url"`, `git ls-remote -- "$url"`, `env`, `tar`, `rm`, `mv`, `cp`), `./` before a glob, `-F -e "$q" --` for `grep`, and `printf '%s\n' "$x"` for text. curl takes exactly one validated URL and `--globoff`, because `--` does not stop later URLs. For a git ref write `git checkout "$ref" --` or `git switch -- "$ref"`, never `git checkout -- "$ref"`, which reads the ref as a pathspec (also SH-CI-29, SH-EXP-03). | Option injection executed on current tools: `ssh -oProxyCommand=` (OpenSSH 10.0p2), `scp -oProxyCommand=`, `rsync --rsh=`, GNU `env -S`, `curl -oFILE`, `git ls-remote --upload-pack=` (git 2.54.0), GNU `tar --checkpoint-action=exec=` from a `*` glob (tar 1.35), and `rm *` with a file named `-rf`. `--` is honoured by GNU coreutils 9.4, uutils 0.2.2 and busybox 1.37, except busybox `realpath`. | `s08-operands-listing` prints unterminated `$1` and `$@` operands of `rm`, `mv`, `cp`, `chmod`, `chown`, `tar` and `ln`, a `$var` host operand of `ssh`, `scp` and `rsync`, and `git clone`, `ls-remote`, `fetch` or `pull` with `$var`, each without ` -- `. `s01-shellcheck` covers the glob form. A named-variable operand (`rm "$dir"`) is not greppable, so read every variable operand of a file or network command for a preceding `--`. Watched red (measured 2026-10-11): 4 lines, twin 0. | MUST |
| SH-SEC-09 | Validate an identifier or subscript that comes from data before it reaches any construct that takes or evaluates a variable name, on every shell, with one `case` that rejects an empty, digit-leading or non-identifier name (snippet below). The constructs are `local`, `typeset` and `declare "$k=v"`, `export`, `readonly` and `unset "$k"`, `read -r "$k"`, `printf -v "$k"`, `typeset -n`, `local -n` and `declare -n r="$k"` followed by `$r`, `[ -v "$k" ]` and `[[ -v $k ]]`, `${!k}` and `${arr[$k]}`, and the arithmetic contexts including `[ "$x" -eq 1 ]` on mksh and posh. `shopt -s array_expand_once` (bash 5.3) narrows `let`, `printf -v` and `read` and closes nothing else. This narrows SH-EXP-13's `printf -v` advice to a literal or validated name (also SH-EXP-13, SH-EXP-19). | `k='a[$(touch CANARY)]'` executed through the constructs in the exposure table, a different subset per interpreter. A script written for dash is still exposed when `/bin/sh` is bash in POSIX mode, mksh or posh. The validator turned every column green. | `s09-dynamic-names-listing`: each site shows a `case` validator within reach or carries `# sec-ok: validated at LINE`. Hostile-value run: `a[$(touch CANARY)]` through each construct, judged by whether `CANARY` appears. Watched red (measured 2026-10-11): 8 and 10 lines on two plants, twin 0 on both. | MUST |
| SH-SEC-10 | Never `.`, `source` or `eval` a file another principal can write. Parse `KEY=VALUE` with `read -r` and a `case` allow-list on key and on value (reject `*[!A-Za-z0-9._:/-]*`). A file under the invoking user's own `$HOME`, or in a root-owned directory, is that user's own code. When a file must be sourced, check ownership and mode first (`[ -O "$f" ]`, a reading step). | Sourcing runs every `$(...)` and `;` in the file. A config with `EVIL=$(touch CANARY)` and `RETRIES=3; touch CANARY2` executed on 11 of 11 POSIX-family shells, and the allow-list parser ignored `EVIL` and rejected the bad value. | `s10-source-variable-listing`: each `. "$var"` site answers "who can write this path?". Bare names are SH-SEC-05 and `eval` is SH-EXP-13. Watched red (measured 2026-10-11): 3 lines, twin 0. | MUST |
| SH-SEC-11 | Do not export startup-altering variables: `BASH_ENV`, `ENV`, `SHELLOPTS`, `BASHOPTS`, `PS4`, `POSIXLY_CORRECT`, `CDPATH`, `GLOBIGNORE`, and no `export -f`. An invoker that runs a script in a privileged or shared context (cron, a CI step that may write `$GITHUB_ENV`, `su`) scrubs them with `env -u BASH_ENV -u ENV bash script`. An `unset BASH_ENV` inside the script runs after bash read the file and does nothing. A debug gate or test harness that must export one carries `# sec-ok: <reason>`. | bash 3.2 to 5.3 source `BASH_ENV` for every non-interactive run, including `--noprofile --norc`. `SHELLOPTS=xtrace PS4='$(cmd)'` ran `cmd` in a child bash as uid 1000 on 3.2, 4.2, 4.4, 5.0, 5.2 and 5.3, and an exported `BASH_FUNC_ls%%` replaced `ls` on 3.2 to 5.3. `GLOBIGNORE` and `IFS` are not imported. SHOULD, because 49 sites in 6 of 40 clones were 0 defects (test harnesses and debug gates). | `s11-env-export-listing`: sites to read. `export -n` lines are filtered. Watched red (measured 2026-10-11): 8 lines, twin 0, and a planted `export -n CDPATH` is filtered. | SHOULD |
| SH-SEC-12 | Parallel children write each record with one `printf` or into their own file. Collect statuses per PID (`wait "$pid"` then test its status), because a bare `wait` is a join and not a status check. Read `xargs -P` exit 123 or 124 as "some child failed", not as the child's code. | A job printing a record in two `printf` calls under `xargs -P8` interleaved 400 of 400 lines on GNU findutils 4.9 and 4.10 and busybox 1.37, and formatting the line once gave 0. Bare `wait` returned 0 after a failed job on bash 3.2 to 5.3. `wait -n PID...` ignores the list on bash 4.4 and 5.0. | `s12-wait-listing` and `s12-xargs-p-listing`: sites to read. Watched red (measured 2026-10-11): 2 and 1 lines, twin 0 (waived). | SHOULD |

- Scope SH-SEC-03: INST, TOOL, CI when the file expands a secret. Every POSIX-family shell and fish 3.6.0 to 4.9.3. Whether a CI product masks the value in its log is the product's behaviour and was not measured.
- Scope SH-SEC-08: INST, TOOL, CI, LIB. GNU, uutils and busybox 1.37 measured, BSD and macOS `unverified: read only`.
- Scope SH-SEC-09: TOOL, CI, LIB, any POSIX-family interpreter. Exposed: bash 3.2 to 5.3, zsh 5.0.8 to 5.9, mksh R59c, posh 0.14.1. Not exposed in 26 constructs: dash, busybox ash 1.35 and 1.37, ksh93u+m (2023 and 1.0.10) and yash 2.52. The rule still binds every interpreter, because the interpreter that runs is not the author's.
- Scope SH-SEC-10: INST, TOOL, CI, LIB, RC.
- Scope SH-SEC-11: INST, TOOL, CI, LIB (not RC or EMIT, which set variables on purpose). `BASH_ENV` is ignored by bash-as-`sh`, `--posix`, dash, ash, mksh, ksh93u+m, yash, posh and `zsh --emulate sh`. zsh always reads `$ZDOTDIR/.zshenv`.
- Scope SH-SEC-12: TOOL, CI. GNU and busybox `xargs`, macOS `unverified: read only`.

```sh
# wrong: a name from data reaches read, and k='a[$(cmd)]' runs cmd on bash, zsh, mksh and posh
read -r "$k" < "$f"
# right: reject anything that is not an identifier first
case $k in '' | [0-9]* | *[!A-Za-z0-9_]*) die "bad name" ;; esac
read -r "$k" < "$f"
```

Which interpreters run `a[$(cmd)]` through each construct when the name comes from data (27 cases, 23 interpreter columns):

| Construct with `$k` from data | Runs the payload on |
|---|---|
| `local "$k=v"` | bash 3.2 to 5.3, mksh, posh |
| `local "$k"`, `typeset "$k"` without a value | mksh only |
| `typeset "$k=v"` | bash 3.2 to 5.3, mksh |
| `declare "$k=v"` | bash 3.2 to 5.3 |
| `export "$k=v"`, `export "$k"` | mksh, posh, zsh 5.0.8 to 5.9 |
| `readonly "$k"`, `unset "$k"` | mksh, posh |
| `read -r "$k"` | bash 3.2 to 5.3, mksh, posh, zsh |
| `typeset -n`, `local -n`, `declare -n` then `$r` | bash from 4.4, mksh |
| `printf -v "$k"` | bash from 4.2, zsh from 5.3.1 |
| `${!k}` | bash 3.2 to 5.3 |
| `[ -v "$k" ]`, `[[ -v $k ]]` | bash from 4.4, mksh |
| `$(( k ))`, `(( k ))`, `let "$k"`, `${arr[$k]}` | bash 3.2 to 5.3, mksh (posh for `$(( k ))`) |
| `[ "$k" -eq 1 ]` | mksh, posh |

## What Agents Get Wrong Here

Ranked by measured prevalence times how likely an agent emits it unprompted.
1. **A secret in argv: `curl -H "Authorization: Bearer $TOKEN"`, `-u "$user:$pw"`, `docker login -p`, `docker run -e K=V`, an `https://user:$TOKEN@` URL, or the same value routed through a variable, array or `set --`.** The single-line cell misses the routed forms. `SH-SEC-02`.
2. **`set -euxo pipefail` copied into a script that expands a token.** xtrace prints the value on 11 of 11 shells. `SH-SEC-03`.
3. **`git clone https://x-access-token:$TOKEN@...`, `git -c http.extraheader="Authorization: ..."`, or `git config http.extraHeader` left behind.** Argv in three processes and `.git/config`. `SH-SEC-13`, `SH-SEC-02`.
4. **`cargo publish --token "$T"`, or `CARGO_REGISTRY_TOKEN` against an alternative registry.** Argv, or exit 101 with `no token found`. `SH-SEC-14`.
5. **`rm *`, `ssh "$host"`, `git clone "$url"` or `git ls-remote $url` with no `--`, and `git checkout -- "$ref"` for a ref.** Data becomes an option or a pathspec. `SH-SEC-08`, `SH-SEC-01`.
6. **`. lib.sh` or `. .env` as a bare name.** Dash refuses, so Debian CI is green while bash, busybox ash and yash run the cwd copy. `SH-SEC-05`.
7. **`PATH="$d:$PATH"`, `PATH=$PATH:.` or an empty PATH entry.** The current directory is on PATH. `SH-SEC-04`.
8. **`declare "$k=$v"`, `printf -v "$k"`, `export "$k=$v"` or `read -r "$k"` with a name from data, passed as "safe on dash".** It executes on bash, zsh, mksh and posh. `SH-SEC-09`.
9. **`source "$CONFIG"` or `eval "$(cat conf)"` on a writable path.** Every `$(...)` in the file runs. `SH-SEC-10`.
10. **`sudo -E`, or `[ -e lock ] || touch lock`.** sudo-rs drops the variable and exits 0, and the lock gives 8 to 30 winners of 30. `SH-SEC-06`, `SH-SEC-07`.
11. **Disabling SC2254, SC2053, SC2295, SC2035 or SC2156 to get a green run.** They are the injection findings. `SH-SEC-01`.
12. **Exporting `BASH_ENV`, `PS4` or `export -f`, a bare `wait` used as a status check, and two-`printf` records under `xargs -P`.** Low prevalence, SHOULD. `SH-SEC-11`, `SH-SEC-12`.
