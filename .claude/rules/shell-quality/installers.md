---
# Read on demand through the index. This glob matches no file, so a client never auto-loads the file.
paths:
  - "**/.shell-quality-depth-on-demand"
title: Shell Installers
summary: The SH-INST family. The install.sh delivered by curl and sh, the truncation guard, fail-closed integrity, TLS and downloader flags, BusyBox wget, platform detection, prompts, privilege, PATH edits, the exit table, retry, latest-version lookup, the test hatch and generated installers
---

# Shell Installers

Rows bind to dash 0.5.12, busybox ash 1.35 and 1.37, mksh R59c, ksh93u+m 2023 and 1.0.10, yash 2.52, posh 0.14.1, bash 3.2 and 5.2 `--posix`, zsh 5.9 `--emulate sh`, curl 7.61.1 to 8.22.0, BusyBox v1.37.0 wget, ShellCheck 0.11.0 and cargo-dist 0.33.0, measured 2026-10-11 on Linux. macOS and Windows behaviour is `unverified: read only`.

Owns the `install.sh` that arrives by `curl | sh` or download-then-run (kind INST), and the audit of a generated one: the truncation guard, integrity and its trust root, TLS and downloader flags, platform detection, prompts, privilege, PATH edits, the exit table, retry, latest-version lookup and the test hatch. Not owned here: the generic wget and tool probes and temp files are `SH-USER`. Traps, cleanup, `${1+"$@"}` forwarding and the exit-code ranges are `SH-ERR`. The rc-file marker block and emitted PATH code are `SH-EMIT`. Non-ASCII bytes and shebangs are `SH-GATE` and `SH-DIAL`. Secrets on argv are `SH-SEC`. The `install.ps1` pair is `PS-SEC`. Retired IDs are not reused: SH-INST-02, 10, 13, 14, 15, 18 and 21.

Contents: [Group 1: The Installer Kit Decides](#group-1-the-installer-kit-decides) · [Group 2: A Grep Decides or Lists](#group-2-a-grep-decides-or-lists) · [Group 3: A Container Run or an Audit Decides](#group-3-a-container-run-or-an-audit-decides) · [What Agents Get Wrong Here](#what-agents-get-wrong-here)

Confirmed 2026-10-11 (confirmation round): the cells and check scripts of this file were replayed on 25 fresh trees under dash 0.5.12-2 and busybox ash 1.37.0 (identical output on 4,144 paired runs), GNU grep 3.12, git 2.54.0 (2.43.0 in a container), ShellCheck 0.11.0 and shfmt 3.13.1, and the hits were read on a sample. No cell defect. The kit checks `flags`, `root`, `rc` and `tty` were re-run on the installer twin and the plants (dash, busybox ash and bash `--posix`), and the grep aids on 25 fresh trees plus plants.

## Group 1: The Installer Kit Decides

Copy `installer-kit.py` from the `installer-script` skill's `references/` to `scripts/`. It runs the installer against stub curl, wget, uname, ldd, sysctl and sudo in a throwaway HOME, never touching the network. Checks: `trunc`, `tamper`, `hash`, `flags`, `platform`, `root`, `rc`, `tty`, `trap` (`parse` is also SH-GATE-16 and is not run here). A profile (`--profile FILE`, JSON) names the asset pattern, the installed path and `args` for an installer that needs `--force`. The kit is judged by exit status (0 pass, 1 finding, 69 tool or shell missing, 74 I/O, 78 bad invocation) and prints its evidence. Run the matrix under dash, busybox ash and bash `--posix` (also SH-DIAL-04). `root` starts as an unprivileged user: started as root it drops to uid 65534, the installer fails before it reaches `sudo`, and the check reports a vacuous green.

```sh
for t in git grep python3 id; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree"
IKIT=${IKIT:-scripts/installer-kit.py}
INSTALLER=${INSTALLER:-install.sh}
[ -f "$IKIT" ] || echo "PREFLIGHT: $IKIT missing"
[ -f "$INSTALLER" ] || echo "PREFLIGHT: $INSTALLER missing"
! id -u | grep -q -x -e 0 || echo "PREFLIGHT: run the root check as an unprivileged user"
# k-matrix: prints one RED line per failing check and interpreter
for sh in dash 'busybox ash' 'bash --posix'; do
    for c in trunc tamper hash flags platform root rc tty; do
        python3 -I "$IKIT" "$c" "$INSTALLER" --shell "$sh" >/dev/null || echo "RED($?): $c under $sh"
    done
done
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-INST-01 | Put every executable statement in a function, leave only assignments at top level, and end the file with `main ${1+"$@"}` (or a call ending `\|\| exit 1`, or the close of one `{ ... }` group wrapping the whole script). Document delivery as `curl --proto '=https' --tlsv1.2 -fsSL URL \| sh -s -- [options]` or download-then-run, never `sh -c "$(curl ...)"` for a script without the guard. | A cut `curl \| sh` runs every complete line it received, and command substitution runs the parsed prefix, then fails. rustup and docker guard (docker with a comment saying why), 5 of 17 audited installers are linear. **Scope:** INST. The sweep is vacuous on an installer that needs the network unless the downloaders are stubbed, which the kit does. posh 0.14.1 rejects `-s`, so its coverage is unproven. | `k-matrix` check `trunc`: every line prefix, four byte prefixes and the last 60 byte prefixes piped to the shell, against a seeded previous install. PARTIAL or VACUOUS exits 1, and an installer that refuses to overwrite needs `args` `--force` in the profile. Reading aid `i01-last`. Watched red (measured 2026-10-11) on dash, busybox ash 1.37 and bash 3.2 `--posix` for a linear plant and a top-level-statement plant, twin 0 partial cuts. A 60% cut alone is not a check. | MUST |
| SH-INST-03 | Verify the payload's SHA-256 before extracting or executing it and fail closed (exit 4) in three cases: no hash tool, no expected digest (absent or blank), digest mismatch. Probe `sha256sum`, then `shasum -a 256`, then `openssl dgst -sha256`, and compare the hex strings in the shell, never `sha256sum -c`. A signature checked against a key embedded in the script may replace the digest. Without the verifying tool, fail closed unless the digest is pinned (`*_INSTALL_SHA256`), and say in the header that the signature was skipped. | cargo-dist 0.33.0 prints "skipping sha256 checksum verification" and installs a tampered archive with rc 0 when `sha256sum` is off PATH. `sha256sum -c` needs a filename on the line and accepts uppercase hex. Stock Linux images carry `sha256sum` (5 of 5, 8 of 8 earlier), so fail-closed costs nothing there. **Scope:** INST on 11 POSIX interpreters. The `shasum` branch exists for macOS (unverified: read only). | `k-matrix` check `tamper`: a swapped archive, every hash tool off PATH, a blank digest, and a good-payload control. Watched red (measured 2026-10-11) on three interpreters for a no-check plant and a fail-open plant, twin 0. An earlier 11-interpreter run was red 11 of 11, and a profile removing only one tool reads red because another remains. Check `hash` removes all but one hash tool per run so the `sha256sum`, `shasum` and `openssl` branches each run alone (a swapped archive refuses, the good payload installs); watched red on a plant whose openssl branch trusts the sidecar and on one whose openssl branch parses the wrong field, green on the template (measured 2026-10-11, dash). The chezmoi install.sh openssl branch fails it by source (`cut -d ' ' -f a`); that is evidence for the check and is not ours to fix. | MUST |
| SH-INST-05 | Add `--tlsv1.2` to curl only after `curl --help all` lists it, always pass `--proto '=https'`, and branch explicitly: one `curl` call per flag set, no unquoted flag variable, no SC2086 disable. The wget half is SH-INST-22. | `--tlsv1.2` is listed only under `--help all` on curl 7.76.1 to 8.22.0 and under plain `--help` on 7.61.1 alone. A plain-`--help` probe passes the check and never sends the flag. `--proto` is curl-only. **Scope:** INST using curl. | `k-matrix` check `flags` (stub curl whose help lacks or lists the flag). Aids `i05-tls` and `i05-proto` list installers that pass `--tlsv1.2` without a `help all` probe or never pass `--proto`. Watched red (measured 2026-10-11) for a plain-`--help` probe plant on three interpreters and for both aids on a plant, silent on the twin. | MUST |
| SH-INST-06 | Refuse a non-https download URL, including every URL an environment variable can override (exit 3). | `--proto '=https'` is curl-only, so wget and an overridden base URL need a script-level assertion. **Scope:** INST with an overridable base URL. | `k-matrix` check `flags`, scenario "http url". Aids `i06-literal` (a literal http URL assigned) and `i06-assert` (an overridable URL and no `https://*` case). Watched red (measured 2026-10-11) for an http-accepting plant on three interpreters, and both aids on a plant. | MUST |
| SH-INST-08 | Make platform detection total: every `uname -s` and `uname -m` pair maps to a target or exits 7. Offer a `*_INSTALL_TARGET` override. Detect libc with three outcomes (glibc evidence, musl evidence, neither exits 7). Detect Rosetta by value: `[ "$(sysctl -n hw.optional.arm64 2>/dev/null \|\| true)" = 1 ]`, never `sysctl ... \|\| true \| grep -q ': 1'`. | A default arm maps unknown input to a binary (12 of 26 cells wrong in two audited installers). The static `busybox:1.37-musl` image has no `ldd` and no `/lib`. `\|\|` binds looser than `\|`, so the second form tests sysctl's exit status, leaks the stray `hw.optional.arm64: 1` line, and an oid answering 0 yields the wrong architecture. **Scope:** INST. A real Mac's answer is unverified: read only. | `k-matrix` check `platform`: 14 stubbed scenarios, including `linux i686` and `riscv64` expecting rc 7 and a Rosetta oid answering 0. Aid `i08-rosetta`. Watched red (measured 2026-10-11) for a default-arm plant and a Rosetta plant on three interpreters. | MUST |
| SH-INST-11 | Prompt only from `/dev/tty`, only after `(: </dev/tty) 2>/dev/null`. Give every prompt a non-interactive default and a flag (`--yes`, `NONINTERACTIVE`). Never `read` from stdin. Write the prompt with `printf`, not `echo -n`. | Under `curl \| sh` stdin is the script. A bare `read` swallows the next script line on 8 shells in a linear script and gets EOF (a silent auto-answer) in a main-last script. An unguarded `</dev/tty` exits 2 with no controlling terminal. **Scope:** INST delivered by pipe. | `k-matrix` check `tty` (a controlling terminal and none). Aids `i11-read` (false positives on `while read` loops) and `i11-tty` (`/dev/tty` with no probe). Watched red (measured 2026-10-11) for a stdin-read plant and an unguarded-tty plant on three interpreters. | MUST |
| SH-INST-12 | Install into a user-writable directory with no `sudo`. Edit PATH only with consent and an opt-out (`--no-modify-path`). The rc edit is a marker block replaced in place, symlink-safe, in the one file for the detected shell (also SH-EMIT-16). Run twice against a throwaway `HOME`, the rc diff is empty. | Blind appends duplicate lines and break symlinked dotfiles, and a setup that writes every shell's rc file edits files the user never uses. **Scope:** user-space INST. Escalation is SH-INST-23. | `k-matrix` checks `rc` (second-run diff, opt-out, symlink) and `root`. Aids `i12-sudo` and `i12-rcappend`. Watched red (measured 2026-10-11) for an append-only rc plant on three interpreters. `root` is red only from an unprivileged uid (SH-INST-23). | MUST |
| SH-INST-22 | Pass the same wget options on and off a tty, and only options in the BusyBox v1.37.0 set: `-q`, `-S`, `-O`, `--no-verbose`, `--tries`, `--timeout`, `--header`, `--user-agent`. Probe anything else with `wget --help` (also SH-USER-10). Never `--show-progress`, `--https-only`, `--secure-protocol`, `--retry-connrefused` or `--netrc` unprobed. | An option chosen by `[ -t 1 ]` is never exercised by a harness that has no tty, and BusyBox v1.37.0 rejects those five, so the tty branch fails while the piped run installs. **Scope:** INST with a wget path. GNU wget 1.14 also rejects `--https-only`. | `k-matrix` check `flags`, scenario 2t (the BusyBox option set under a pty). Aid `i22-wget` (a hit inside a `wget --help` probe is a reading step). Watched red (measured 2026-10-11) for a tty-branch plant on three interpreters, aid red on the plant and silent on the twin. Confirmed 2026-10-11: `flags` scenario 2t red on a `--show-progress` tty-branch plant and green on the twin on dash 0.5.12-2, busybox ash 1.37.0 and bash `--posix`; the aid prints the plant and is silent on 25 fresh trees. | MUST |
| SH-INST-23 | An installer that can need root defaults to a user prefix. It escalates only for an explicitly chosen unwritable prefix and only the final copy, never the download, verify or extract. It announces the exact command first, offers `--no-sudo`, and never interpolates arguments into `su -c '...'`. A root-by-design installer (package-manager dispatch) says so in its header and is checked for the narrowest escalation. | Installers escalate four ways: blanket `$SUDO`, probe then escalate, only the final copy, none. Three of the four fail to install as a normal user, and one needs `sudo` for `/usr/local/bin` without saying so. **Scope:** INST, pinned decision (adopter may override). | `k-matrix` check `root` from an unprivileged uid (`uid=` non-zero, `installed=True`), then read where the first `sudo` falls with `i12-sudo` and `i23-su`. Watched red (measured 2026-10-11) for a hard-coded `sudo mkdir` plant at uid 1000 on dash, twin 0. `i23-su` red on a plant, silent on the twin. Confirmed 2026-10-11 at uid 1000: `root` green on the twin and red on the `sudo mkdir` plant (`sudo-calls=1`, rc 6) on dash, busybox ash and bash `--posix`; `i23-su` lists a planted `su -c` and is silent on 25 fresh trees; `i12-sudo` lists real `sudo` calls on 9 trees. | MUST |

```sh
# wrong: no hash tool means the check is skipped and the archive is extracted
if ! command -v sha256sum >/dev/null 2>&1; then warn "skipping checksum verification"; return 0; fi
# right: no tool, no digest and a mismatch all stop the install
command -v sha256sum >/dev/null 2>&1 || err "no sha256 tool (sha256sum, shasum or openssl)" 4
```

## Group 2: A Grep Decides or Lists

Run from the repository root. Any `PREFLIGHT:` line means the host cannot judge. `installers()` is the shell files that read `uname` and call curl or wget. A cell passes with empty stdout and stderr, and a cell marked listing prints sites to read. `files()` lists what `shfmt -f` lists, so it misses a shebang file that carries another extension (`.aliases`, `.in`, `.tmpl`) and a dash, ash or ksh shebang; cell `i00-gap` lists them, and the cells below do not read them until they are renamed or checked by hand. A bare `# inst-ok:` waiver is a hit. An installer you name differently still matches, since the helper keys on content.

```sh
for t in git xargs grep awk tr shfmt; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
files() { git ls-files -z --cached --others --exclude-standard | xargs -0 -r sh -c 'for f; do if [ -f "$f" ]; then printf "%s\0" "$f"; fi; done' _ | xargs -0 -r shfmt -f -- | tr '\n' '\0'; }
installers() { files | xargs -0 -r grep -l -e 'uname' -- | tr '\n' '\0' | xargs -0 -r grep -l -e 'curl' -e 'wget' -- | tr '\n' '\0'; }
# i00-gap (listing): a file with a shell shebang that files() does not list, because shfmt -f reads a shebang only on a sh, bash, mksh or zsh script that has no other extension (.aliases, .in, .tmpl, dash, ash and ksh files are not listed): read each hit, a template is not shell
t=$(mktemp) && files | tr '\0' '\n' >"$t" && git ls-files -z --cached --others --exclude-standard | xargs -0 -r sh -c 'for f; do if [ -f "$f" ]; then printf "%s\0" "$f"; fi; done' _ | xargs -0 -r awk -v listed="$t" 'BEGIN { while ((getline l < listed) > 0) seen[l] = 1 } FNR == 1 && !(FILENAME in seen) && /^#![ ]*\/([^ ]*\/)?(env +(-S +)?)?(sh|bash|dash|ash|ksh|ksh93|mksh|yash|zsh)([ \t]|$)/ { print FILENAME }'; rm -f "$t"
# i01-last (listing): the last statement of each installer, which must be one call to the entry function or the closing brace of a wrapper group
installers | xargs -0 -r awk 'FNR == 1 && last != "" { print pf ": " last } FNR == 1 { last = ""; pf = FILENAME } NF && $1 !~ /^#/ { last = $0 } END { print pf ": " last }'
# i04-claim: files that claim a check and call no hash tool
installers | xargs -0 -r grep -l -i -E -e 'sha-?256' -e 'checksum' -- | tr '\n' '\0' | xargs -0 -r grep -L -e 'sha256sum' -e 'shasum' -e 'openssl' --
# i05-tls: --tlsv1.2 with no help-all probe
installers | xargs -0 -r grep -l -e '--tlsv1.2' -- | tr '\n' '\0' | xargs -0 -r grep -L -e 'help all' --
# i05-proto: an installer that never passes --proto (a wget-only one needs i06-assert instead)
installers | xargs -0 -r grep -L -e '--proto' --
# i06-literal
files | xargs -0 -r grep -H -n -E -e '(BASE_URL|URL)=.*http://' -- | grep -v -E -e ':[0-9]+:[[:space:]]*#'
# i06-assert: an overridable URL and no https case
installers | xargs -0 -r grep -l -E -e '[$][{][A-Za-z_]*URL[:]?[-=]' -- | tr '\n' '\0' | xargs -0 -r grep -L -F -e 'https://*' --
# i07 (listing): a wget path that never reads the certificate-validation note
installers | xargs -0 -r grep -l -e 'wget' -- | tr '\n' '\0' | xargs -0 -r grep -L -e 'certificate validation' --
# i08-rosetta
files | xargs -0 -r grep -H -n -E -e 'sysctl[^|]*[|][|][^|)]*[|]' --
# i09-header: an installer with no exit table line
installers | xargs -0 -r grep -L -e '^# Exit:' --
# i11-read (listing)
installers | xargs -0 -r grep -H -n -E -e '(^|[;&|(]|[[:space:]])read[[:space:]]+(-[a-z]+[[:space:]]+)*[A-Za-z_]+' -- | grep -v -E -e '/dev/tty' -e ':[0-9]+:[[:space:]]*#'
# i11-tty: /dev/tty used with no probe
installers | xargs -0 -r grep -l -e '/dev/tty' -- | tr '\n' '\0' | xargs -0 -r grep -L -F -e ': </dev/tty' --
# i12-sudo (listing)
files | xargs -0 -r grep -H -n -E -e '(^|[;&|(]|[[:space:]])sudo[[:space:]]' -- | grep -v -E -e ':[0-9]+:[[:space:]]*#'
# i12-rcappend
files | xargs -0 -r grep -H -n -E -e '>>[[:space:]]*"?[$][{]?HOME[}]?/[.](bashrc|bash_profile|profile|zshrc|zprofile)' --
# i16-retry: --retry-all-errors with no help probe
files | xargs -0 -r grep -l -E -e '^[^#]*--retry-all-errors' -- | tr '\n' '\0' | xargs -0 -r grep -L -e '--help' --
# i17-latest
files | xargs -0 -r grep -H -n -E -e 'api[.]github[.]com' -e 'curl[^|;&]* -s[sS]?[[:space:]]' -- | grep -v -E -e 'curl[^|;&]*(-f|--fail)' -e ':[0-9]+:[[:space:]]*#'
# i19-hatch: an installer with no print hatch
installers | xargs -0 -r grep -L -e 'SH_PRINT' --
# i22-wget (listing)
files | xargs -0 -r grep -n -H -e '--show-progress' -e '--https-only' -e '--secure-protocol' -e '--retry-connrefused' --
# i23-su
files | xargs -0 -r grep -H -n -E -e '(^|[;&|(]|[[:space:]])su([[:space:]]+-[A-Za-z]*)*[[:space:]]+-c[[:space:]]' -- | grep -v -E -e ':[0-9]+:[[:space:]]*#'
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-INST-04 | Name the integrity class in the header: pinned digest, manifest from a different origin, sidecar from the same origin (worded "detects corruption, not a hostile origin"), GPG, or none. Make no verification claim the code does not back: a file that mentions `sha256` or `checksum` calls a hash tool or names the delegate that does and states what the delegate leaves unverified. | cargo-dist's own book says same-origin checksums over https "have minimal security benefit". A header that promises a check the code lacks is hallucinated safety, and a delegating front door is true only while its delegate's hash tool exists (cargo-dist 0.33.0, SH-INST-03). **Scope:** INST and sh front doors. A delegating front door gets a legitimate hit and resolves it by naming its delegate. | `i04-claim`: any file listed claims a check and calls no tool. The class is a reading step, the digest provenance trace: find where the expected value is fetched and compare that host with the archive host. Watched red (measured 2026-10-11) on a claim-only plant, silent on the twin. | MUST |
| SH-INST-09 | Document the exit table in a `# Exit:` header line: 0 ok, 1 generic, 2 usage or missing tool, 3 network, 4 checksum, 5 extract, 6 setup, 7 platform, 129 130 143 HUP INT TERM through traps. Call `err` only with documented codes, and never forward a child status in 123 to 125, 128 or 200 to 255 (also SH-ERR-18). | Callers script around installer failures, and 16 of 17 audited installers document none. **Scope:** INST and front-door bootstrap scripts, MUST for this kind and SHOULD elsewhere (also SH-ERR-25). Pinned decision (adopter may override): the 0 to 7 table. | `i09-header`: any file listed has no table. That each `err ... N` uses a documented code is a reading step, as no lexical pattern maps call sites to the table. Watched red (measured 2026-10-11) on a headerless plant, silent on the twin. | MUST |
| SH-INST-16 | Retry with `curl --retry 3 --retry-delay 2`. Add `--retry-connrefused` only behind a `curl --help all` probe, and never `--retry-all-errors` without one (curl 7.71.0). | On curl 8.22.0 a 404 is not retried by `--retry` (1 request, 4 with `--retry-all-errors`) and a refused connection is retried only with `--retry-connrefused` (0 s against 2 s). curl 7.61.1 rejects `--retry-all-errors` with rc 2. **Scope:** curl 7.61.1 to 8.22.0. | `i16-retry`: any file listed uses it unprobed (comment lines excluded). Watched red (measured 2026-10-11) on an unprobed plant, silent on the twin. | SHOULD |
| SH-INST-17 | Resolve "latest" from the `releases/latest/download/ASSET` redirect or the `releases/latest` 302 `Location`, not the REST API. When a text endpoint supplies the version, use `curl -f` and validate its shape before use. | The unauthenticated REST limit is 60 requests per hour, so a CI fleet behind one NAT hits it. `curl -s` without `-f` turns "Not Found" into the version. **Scope:** GitHub-hosted releases. | `i17-latest`: any hit is a violation. Watched red (measured 2026-10-11) on a REST-API plant, silent on a redirect twin. | SHOULD |
| SH-INST-19 | Expose a test hatch: `*_INSTALL_SH_PRINT=target` prints the detected target and exits 0, plus an https-only `*_INSTALL_BASE_URL` override. | The hatch makes the platform and offline runs possible, and rustup ships one. **Scope:** INST. | `i19-hatch` lists installers without it, and `TOOLX_INSTALL_SH_PRINT=target sh install.sh` prints the triple with rc 0. A missing hatch is its own failure, so no plant adds information. | SHOULD |

## Group 3: A Container Run or an Audit Decides

SH-INST-07 needs a real BusyBox: start a local https server with a self-signed certificate, run the installer in `busybox:1.37` with `TOOLX_INSTALL_BASE_URL` pointing at it, once without and once with the pinned digest. Pass is rc 4 and nothing installed without the pin, rc 0 with it. The listing cell is `i07` in Group 2.

For a generated installer (cargo-dist 0.33.0), run the ten-row checklist on the rendered file, never on the template, and fix upstream or wrap. Do not hand-edit the output. Rows 3, 5 and 6 are kit runs. The `trap` check signals the installer at its Nth download, and N must fall after the temporary directory exists, so run N = 1 and 2: a line reading `download-started=False` means the installer makes fewer downloads and that N is void, while a pass at 1 alone can be a false green when a version lookup precedes `mktemp`.

```sh
for t in git python3 grep dash busybox; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree"
IKIT=${IKIT:-scripts/installer-kit.py}
INSTALLER=${INSTALLER:-dist/installer.sh}
[ -f "$INSTALLER" ] || echo "PREFLIGHT: $INSTALLER missing"
# g1: TLS flags on the archive hop (each line printed is a missing flag)
grep -L -e '--proto' -- "$INSTALLER"
grep -L -e '--tlsv1' -- "$INSTALLER"
# g3: fail-open when the hash tool is missing
python3 -I "$IKIT" tamper "$INSTALLER" --shell dash >/dev/null || echo "RED($?): g3"
# g4: Rosetta precedence
grep -n -E -e 'sysctl[^|]*[|][|][^|)]*[|]' -- "$INSTALLER"
# g5: signals leave no temp directory
for n in 1 2; do python3 -I "$IKIT" trap "$INSTALLER" --shell dash --nth "$n" >/dev/null || echo "RED($?): g5 at N=$n"; done
# g6: truncation guard
python3 -I "$IKIT" trunc "$INSTALLER" --shell dash >/dev/null || echo "RED($?): g6"
# g7: the rendered file parses (run it in the release job)
dash -n "$INSTALLER"
busybox ash -n "$INSTALLER"
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-INST-07 | When wget is the fallback downloader, warn if it is BusyBox. Set a flag when wget prints "certificate validation not implemented", and with that flag refuse a same-origin checksum (exit 4) unless the digest is pinned (`*_INSTALL_SHA256`). | The official `busybox:1.37` and `-musl` images use internal TLS and accept a self-signed certificate, alpine:3.22's build validates, and Debian busybox 1.35 prints the same note. The refusal keys on the runtime message, not the version. **Scope:** INST on hosts with wget and no curl, BusyBox v1.37.0. 5 of 10 sampled installers have a wget path and none mentions BusyBox. | The container run above, then `i07` (watched red on a plant, silent on the twin). Watched red (measured 2026-10-11) on `busybox:1.37`: the twin unpinned rc 4 with nothing installed, a plant without the refusal rc 0 with the binary installed, the twin pinned rc 0. Re-run 2026-10-11 against a self-signed https origin on `busybox:1.37` and `busybox:1.37-musl`: the twin unpinned rc 4 with nothing installed, the plant without the refusal rc 0 with the binary installed, the twin pinned rc 0. `alpine:3.22` validates and fails the download with rc 3, as the row says. | MUST |
| SH-INST-20 | Audit every generated installer with the ten-row checklist before shipping, fix upstream or wrap and never hand-edit the output, pin the generator version, and run `dash -n` and `busybox ash -n` on the rendered file in the release job. Rows: 1 TLS flags on the archive hop, 2 digests embedded, 3 fail-open hash, 4 Rosetta precedence, 5 signals, 6 truncation guard, 7 the rendered file parses, 8 the `.ps1` hash (also PS-SEC-01), 9 which hops carry `--proto`, 10 the generator is pinned. | cargo-dist 0.33.0 has no TLS flags on the archive hop, a fail-open hash check, a wrong Rosetta test and no trap, and issue 2339 shipped an installer missing about 108 lines. Result on 0.33.0 (shipped in grimoire v0.14.3): rows 1, 3, 4, 5 and 8 fail, row 9 is partial, rows 2, 6 and 10 pass, row 7 is not enforced upstream. **Scope:** INST (generated), cargo-dist 0.33.0, whose template was identical on its main branch on 2026-10-02. | Block `g1` to `g7` above and the `.ps1` hash row. Watched red (measured 2026-10-11) on the rendered 0.33.0 installer: row 3 tampered archive installed with rc 0, row 4 two hits, row 1 no `--proto`. | MUST |

## What Agents Get Wrong Here

Ranked by measured frequency across 17 audited installers, then by whether a linter sees it. ShellCheck 0.11.0 reports the defect on 1 of 26 installer plants, so lint-green is not evidence.

1. **No integrity check, or one that fails open** on a missing hash tool or a blank digest: 9 of 17 have none. `SH-INST-03`.
2. **A header or doc that claims a verification the code lacks.** `SH-INST-04`.
3. **TLS and wget flags copied unprobed** (`--https-only`, `--secure-protocol`, `--tlsv1.2`, `--show-progress` on a tty): 16 of 17 do not probe. `SH-INST-05`, `SH-INST-22`.
4. **No exit table:** 16 of 17. `SH-INST-09`.
5. **Install steps at top level with no `main` last:** 5 of 17. `SH-INST-01`.
6. **A silent platform fall-through or the wrong Rosetta precedence.** `SH-INST-08`.
7. **A bare `read` or an unguarded `</dev/tty` in a piped installer.** `SH-INST-11`.
8. **Escalating the whole install with `sudo`, or interpolating into `su -c`.** `SH-INST-12`, `SH-INST-23`.
9. **`curl -s` for the latest version, or the REST API.** `SH-INST-17`.
10. **Trusting a generated installer, or calling an installer tested after one run on the host shell.** `SH-INST-20`, `SH-INST-01`.
11. **An http URL from an environment variable fetched unchecked.** `SH-INST-06`.
12. **`--retry-all-errors` or `--retry-connrefused` with no probe.** `SH-INST-16`.
