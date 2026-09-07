# Research: Session PATH visibility for GUI apps + POSIX trampoline self-location

<!--
Technology Landscape Research
Filename: artifacts/research_toolchain_activation_shell_session.md
Owner: Researcher (worker-researcher)
Handoff to: Architect (/architect), Swarm Plan (/swarm-plan), hex-architect (toolchain activation ADR)
Related Skills: architect, swarm-plan

Purpose: Persist tech landscape findings to inform ADRs, plans, design decisions.
Artifacts decay — check dates before trusting findings.
-->

## Metadata

**Date:** 2026-09-04
**Domain:** cli / packaging (toolchain activation, shell/session integration)
**Triggered by:** `ocx self setup` registering `$OCX_HOME/bin` and `$OCX_HOME/toolchain/bin` at session level (decision brief D5) and POSIX trampoline scripts deriving their home from `$0` (decision brief D2), per `/home/mherwig/.cache/hex/toolchain_activation_decision.md`
**Expires:** 2027-03-04 (6 months — platform PATH APIs move slowly, but rustup/mise/aqua internals and macOS launchd behavior are worth re-checking)

## Direct Answer

D5 and D2 both hold up against precedent. Three sharpenings, in order of how much they should change the plan:

1. **Linux scope correction (D5).** `~/.config/environment.d/ocx.conf` does **not** reach every GUI-launched app — only processes started by (or under) the `systemd --user` manager. GNOME and KDE-Plasma-Wayland sessions are the confirmed first-class consumers; LightDM (by default), SDDM, and non-systemd-session desktops never read it and instead depend on profile-sourcing. D5's `~/.profile` fallback is not covering an edge case — it is covering the *majority* of real GUI-launch paths on non-GNOME/KDE Linux. Reframe it as co-primary in the plan, not a fallback of last resort.
2. **Windows write correctness (D5).** Write PATH via a direct registry write, never `setx` (silent 1024-char truncation, has caused real data loss in other tools' installers). Always write `REG_EXPAND_SZ`, unconditionally — this is exactly the bug rustup shipped and later fixed (`REG_SZ` broke `%SystemRoot%`-style expansion for every other PATH entry). Broadcast `WM_SETTINGCHANGE` via `SendMessageTimeoutW` (not blocking `SendMessage`), and document plainly that it only reaches new processes.
3. **D2's `$0` derivation is sound but should resolve through `readlink -f`, not bare `dirname "$0"`.** The common case (bare PATH-found invocation) is reliable because a script's `$0` comes from the shell's *resolved* execve pathname, not the caller-controlled ELF `argv[0]` convention that trips up compiled binaries. The one real, precedent-documented failure mode is a symlink placed at some *other* path pointing at the trampoline — rustup hit exactly this class of bug twice (hardlink/symlink fragility, and again in 1.28.0, issue #4224). Resolve symlinks explicitly before deriving `<home>`.

## Technology Landscape

### Established (proven, widely accepted)

| Mechanism | Status | Notes |
|---|---|---|
| `HKCU\Environment` + `REG_EXPAND_SZ` + `WM_SETTINGCHANGE` broadcast | Standard | Used by rustup, scoop, volta, uv, winget-installed MSIs. Universal disclaimer: new processes only. |
| systemd `environment.d` | Standard for systemd-session desktops | Recommended successor to deprecated `~/.pam_environment`; scope is narrower than "every GUI app" (see Direct Answer). |
| `launchctl setenv` + LaunchAgent (`RunAtLoad`) | Standard, community-driven | The accepted fix for macOS GUI-app PATH invisibility; no major tool ships it by default (JetBrains Toolbox, Homebrew do not). |
| Idempotent hash-marked fenced RC block (conda-style) | Standard | OCX's own `rc_block.rs` three-hash state machine is a stricter superset — conda's own block is documented non-idempotent on re-run. |
| Compiled-binary self-location via `/proc/self/exe` / `GetModuleFileNameW` / `KERN_PROC_PATHNAME` | Standard | Universal answer to "where am I" for compiled proxies (rustup, mise, aqua-proxy, volta, scoop's `shim.exe`). |

### Declining / cautionary

| Mechanism | Signal | Avoid because |
|---|---|---|
| `setx` for PATH writes | Long-documented bug, still bites installers today | Silent 1024-character truncation corrupts/drops existing PATH entries (GitHub Desktop hit this in production). |
| `REG_SZ` for a PATH value that must expand `%VAR%` references | rustup shipped this, then fixed it | Breaks expansion of every *other* entry already in PATH, not just the new one. |
| Absolute-path hardcoding in a relocatable launcher (Python venv's `pyvenv.cfg`) | CPython issue #136051 still open | Years-old, still-unfixed precedent for exactly the class of bug D2's `$0`-derivation is designed to avoid. |
| `~/.pam_environment` | Deprecated since pam_env 1.5.0; Arch stopped reading it 2022-10-20 | Superseded by `environment.d`. |

## Design Patterns Worth Considering

- **argv0-identity dispatch, syscall-backed self-location** — rustup, `mise --shims`, aqua-proxy, and volta all resolve *which tool to run* from the invocation name (symlink name or `$0`), but resolve *where am I installed* through a syscall channel, never through argv[0]/PATH-search artifacts. Two different questions, two different reliable channels — worth stating explicitly if OCX ever grows a compiled Unix dispatcher (out of scope for D2, which stays a script).
- **Sidecar file keyed by same-basename, located via the OS self-path API** — Scoop's `shim.exe` reads a same-basename `.shim` file from `GetModuleFileNameW`'s own directory. This is exactly the shape of OCX's existing `.shim`/`.shimref` grammar in `crates/ocx_shim/src/core.rs` — the precedent validates the existing design, no import needed.
- **Fenced, hash-marked idempotent RC block** — conda popularized the pattern; OCX's `rc_block.rs` (canonical/marker/actual three-hash state machine) is already a stricter version of it.
- **LaunchAgent `RunAtLoad` + `launchctl setenv`** — the only mechanism that reaches macOS GUI apps launched outside any shell; not to be confused with `/etc/paths.d`, which only ever reaches login shells.

## Key Findings

1. rustup shipped `REG_SZ` instead of `REG_EXPAND_SZ` for its PATH write (issue #261, 2016), breaking expansion of every other `%VAR%`-style entry already in PATH. Current `windows.rs` writes via `set_expand_hstring("PATH", &new_path)` — unconditionally `REG_EXPAND_SZ`, never conditional on the pre-existing type. [rust-lang/rustup#261](https://github.com/rust-lang/rustup/issues/261), [`windows.rs`](https://github.com/rust-lang/rustup/blob/master/src/cli/self_update/windows.rs)
2. `setx` silently truncates to 1024 characters and can corrupt or drop existing PATH entries; GitHub Desktop's installer hit this in production. The safe path is a direct registry write, never shelling out to `setx`. [2called-chaos/mcl#13](https://github.com/2called-chaos/mcl/issues/13), [desktop/desktop#18176](https://github.com/desktop/desktop/issues/18176)
3. `WM_SETTINGCHANGE` should be broadcast via `SendMessageTimeout(HWND_BROADCAST, WM_SETTINGCHANGE, 0, "Environment", SMTO_ABORTIFHUNG, 5000, ...)`, not a blocking `SendMessage` — this notifies Explorer and new consoles without requiring reboot, but reaches only newly-created processes; already-running ones never see it. [MS Learn: WM_SETTINGCHANGE](https://learn.microsoft.com/en-us/windows/win32/winmsg/wm-settingchange), [ofek/userpath#40](https://github.com/ofek/userpath/issues/40)
4. Windows always resolves System PATH before User PATH in the effective combined PATH — order is fixed at the OS level; OCX cannot make its user-PATH contribution win a name collision against a system-PATH entry regardless of write order. [Baeldung: User vs System Variables](https://www.baeldung.com/cs/user-vs-system-variables)
5. rustup, scoop, volta, uv, and winget-installed MSIs all write to `HKCU\Environment` for per-user installs and universally disclaim "restart your terminal" — none guarantees already-running shells see the update. [docs.volta.sh/advanced/installers](https://docs.volta.sh/advanced/installers), [docs.astral.sh/uv/reference/installer](https://docs.astral.sh/uv/reference/installer/)
6. `~/.config/environment.d/*.conf` is real and PATH-capable — the systemd man page's own worked example is a `PATH=/opt/foo/bin:$PATH` prepend — read from `~/.config/`, `/etc/`, `/run/`, `/usr/lib/` in that override order, lexicographic within each, with `${FOO:-default}`/`${FOO:+alt}` expansion (no other shell syntax). [man7.org: environment.d(5)](https://man7.org/linux/man-pages/man5/environment.d.5.html)
7. `~/.pam_environment` reading is deprecated since pam_env 1.5.0 and flagged for eventual removal; Arch Linux stopped reading it on 2022-10-20. `environment.d` is the acknowledged successor — but see finding 6's scope caveat. [man7.org: pam_env(8)](https://man7.org/linux/man-pages/man8/pam_env.8.html), [Kisaragi Hiu: Migrating away from .pam_environment](https://kisaragi-hiu.com/migrating-away-from-pam-environment/)
8. Desktop-manager profile-sourcing is inconsistent and independent of `environment.d`: LightDM's default `/etc/lightdm/Xsession` sources `/etc/profile`, `~/.profile`, `/etc/xprofile`, `~/.xprofile` (Debian's package variant strips this); SDDM sources `.profile`/`.bash_profile`/`.zprofile` (unusual for a graphical session); GDM3 sources profile/xprofile scripts before handing off to `/etc/X11/Xsession.d`. [goral.net.pl: Xsession in Debian](https://goral.net.pl/post/xsession/), [ArchWiki: LightDM](https://wiki.archlinux.org/title/LightDM)
9. GNOME and KDE Plasma's Wayland sessions launch under `systemd --user` as session leader — the confirmed carve-out that makes `environment.d` actually reach app-launcher-started GUI apps on those two desktops specifically. [ArchWiki: Environment variables](https://wiki.archlinux.org/title/Environment_variables), [systemd.io: Desktop Environment Integration](https://systemd.io/DESKTOP_ENVIRONMENTS/)
10. `launchctl setenv VAR value` inside a `RunAtLoad=true` LaunchAgent plist under `~/Library/LaunchAgents/` remains the current, unbroken mechanism for macOS GUI-app PATH visibility — no Sonoma/Sequoia-specific regression found; multiple still-cited community write-ups confirm the pattern is unchanged. [gist: apply shell PATH to macOS GUI apps via launchd](https://gist.github.com/riaf/cf662d965ebd1b8b47453dd79cdd5578), [bounga.org: Set system-wide PATH for macOS GUI apps](https://www.bounga.org/tips/2020/04/07/instructs-mac-os-gui-apps-about-path-environment-variable/)
11. `/etc/paths.d/*` + `/usr/libexec/path_helper` is login-shell-only, invoked from `/etc/profile` (bash) / `/etc/zprofile` (zsh); requires root to write; has zero effect on GUI-launched apps. Not a substitute for `launchctl setenv`. Source dated 2017 — the mechanism itself is unchanged, but treat shell-specific claims (bash-default era) with caution. [scriptingosx.com: Where PATHs come from](https://scriptingosx.com/2017/05/where-paths-come-from/) *(dated — mechanism cross-checked, not shell-default framing)*
12. Real-world GUI-PATH-visibility fixes (JetBrains Toolbox, generic "why can't my Mac app see brew" guides) are community scripts wrapping the exact LaunchAgent+`launchctl setenv` pattern — no major tool ships it by default, so OCX doing so proactively is ahead of the field. [jetbrains.com/help/toolbox-app/installation](https://www.jetbrains.com/help/toolbox-app/installation.html)
13. Compiled-dispatcher trampolines (rustup, `mise --shims`, aqua-proxy, volta) resolve "which tool to run" from invocation identity (symlink name / `$0`) but resolve "where am I installed" through a syscall-backed self-path API — argv[0] is caller-controlled and unreliable for self-location, but is (in practice) reliable for invocation identity. [jdx.dev: Shims — how they work in mise-en-place](https://jdx.dev/posts/2024-04-13-shims-how-they-work-in-mise-en-place/), [aquaproj.github.io/docs/products/aqua-proxy](https://aquaproj.github.io/docs/products/aqua-proxy/)
14. rustup's own proxy strategy needed two corrective iterations: pure hardlinks broke macOS backup tools that mishandle hardlinks-to-symlinks (issues [#2858](https://github.com/rust-lang/rustup/issues/2858), [#3136](https://github.com/rust-lang/rustup/issues/3136)); the symlink-first-with-hardlink-fallback fix then regressed in 1.28.0 when proxies went through an *additional* layer of symlink resolution ([#4224](https://github.com/rust-lang/rustup/issues/4224)). Relocatable self-discovery through the filesystem is a repeated source of platform-specific bugs even for a mature tool.
15. Since aqua v2.5.0, `aqua-proxy` execve()s directly with no extra forked subprocess, using `$0` for tool identity — the cleanest documented precedent for OCX's "generated launcher, dispatch by identity, no wrapper process" shape. [aquaproj.github.io/docs/reference/execve-2](https://aquaproj.github.io/docs/reference/execve-2/)
16. Scoop's `shim.exe` locates its *own* directory via `GetModuleFileNameW` (Windows' `/proc/self/exe` equivalent), then reads a same-basename sidecar `.shim` file from that directory — directly precedent for OCX's existing `.shim`/`.shimref` sidecar grammar in `crates/ocx_shim/src/core.rs`. [ScoopInstaller/Shim README](https://github.com/ScoopInstaller/Shim/blob/main/README.md)
17. Python venv's `pyvenv.cfg` still hardcodes an absolute `home` path; moving a venv breaks it, and CPython's own tracker for a relative/relocatable `home` is still open years later. Validates D2's explicit "no baked project root" requirement as the fix venv still lacks. [python/cpython#136051](https://github.com/python/cpython/issues/136051)
18. Conda's `# >>> conda initialize >>>` fenced block popularized the idempotent-marker pattern but is documented non-idempotent on re-run; OCX's `rc_block.rs` three-hash (canonical/marker/actual) state machine is a stricter, already-shipped superset — no further import needed. [conda/conda#8703](https://github.com/conda/conda/issues/8703)
19. Bare `vfork()+exec()` is the cheapest external-program launch; `system()` (forks a full shell to parse the command) is roughly 12x slower; `posix_spawn()+exec()` sits close behind vfork at roughly 27% slower. Older but still-cited numbers put fork+exit alone near 700 microseconds. A generated `/bin/sh` trampoline pays a `system()`-shaped cost (shell startup + parse + its own further exec), categorically more than a compiled proxy's single execve — noise for interactive use, compounding only under heavy programmatic fan-out. [blog.famzah.net: fork gets slower](https://blog.famzah.net/2009/11/20/fork-gets-slower-as-parent-process-use-more-memory/)
20. `UV_NO_MODIFY_PATH` (uv) and rustup's own PATH-modification opt-out precedent ([rust-lang/rustup#170](https://github.com/rust-lang/rustup/issues/170)) confirm OCX's existing `OCX_NO_MODIFY_PATH` naming already matches ecosystem convention — no change needed. [docs.astral.sh/uv/reference/installer](https://docs.astral.sh/uv/reference/installer/)
21. Homebrew-on-Linux's own documented advice is `eval "$(brew shellenv)"` in `~/.profile` (Debian/Ubuntu) or `~/.bash_profile` (RHEL-family) — i.e. even a widely-deployed tool falls back to classic profile-sourcing on Linux rather than leaning on `environment.d` as primary, reinforcing finding 6/8's scope correction. [docs.brew.sh/Homebrew-on-Linux](https://docs.brew.sh/Homebrew-on-Linux)
22. A script's `$0`, when invoked via a bare `$PATH`-found command name, is populated from the *resolved* pathname the calling shell passed to `execve()` (threaded through by the kernel's `#!` script loader as the interpreter's positional argument) — not from the separate, caller-controlled `argv[0]` "process name" string that trips up naive compiled-binary self-location schemes. This is why `dirname "$0"` reliably works for the dominant invocation pattern, but a robust implementation still resolves via `readlink -f "$0"` (GNU/BusyBox extension, not POSIX — needs a manual symlink-walk fallback on strict `/bin/sh`) to cover finding 14's symlink-indirection failure mode. [golinuxcloud.com: get script directory reliably](https://www.golinuxcloud.com/get-script-name-get-script-path-shell-script/), [ostricher.com: The Right Way to Get the Directory of a bash Script](https://www.ostricher.com/2014/10/the-right-way-to-get-the-directory-of-a-bash-script/)

## Recommendation

### D5 — Session PATH registration, per platform

**Windows.**
- Write `%OCX_HOME%\bin` and `%OCX_HOME%\toolchain\bin` into `HKCU\Environment\Path` via a direct registry write (`RegSetValueExW`-equivalent, e.g. the `winreg` crate), never `setx` (finding 2).
- Always write the merged value as `REG_EXPAND_SZ`, unconditionally — read the existing value's type only to decide *how to merge*, never to decide what type to write back (finding 1).
- Broadcast `WM_SETTINGCHANGE` via `SendMessageTimeoutW(HWND_BROADCAST, WM_SETTINGCHANGE, 0, "Environment", SMTO_ABORTIFHUNG, 5000, &result)` after the write (finding 3).
- Document, in the same terms every surveyed installer uses, that this reaches new processes only (finding 5) — **cannot promise**: an already-open terminal or IDE seeing the change without restart, or winning a name collision against an entry already on the System PATH (finding 4).

**Linux.**
- Write `~/.config/environment.d/ocx.conf` with `PATH=$OCX_HOME/bin:$OCX_HOME/toolchain/bin:$PATH` (finding 6) — but present it as **co-primary**, not a fallback: it reaches only `systemd --user`-session GUI apps, confirmed for GNOME/KDE-Wayland (finding 9), while LightDM/SDDM/GDM's own profile-sourcing (finding 8) is what most other desktops actually rely on.
- Keep the existing `~/.profile` block; it is doing real, majority-case work, not covering a rare edge.
- No work needed for `~/.pam_environment` — correctly excluded, already deprecated (finding 7).
- **Cannot promise**: PATH visibility on a desktop that is neither GNOME/KDE-Wayland nor profile-sourcing (e.g. a bare i3/sway session started outside any login-manager Xsession wrapper). State this as a known, structural limitation in the plan rather than a bug to chase.

**macOS.**
- Ship a `RunAtLoad=true` LaunchAgent at `~/Library/LaunchAgents/sh.ocx.path.plist` running `/bin/launchctl setenv PATH "$OCX_HOME/bin:$OCX_HOME/toolchain/bin:$PATH"` (finding 10) — no major tool ships this proactively (finding 12), so this is a genuine differentiator worth naming in user-facing framing, not just an implementation detail.
- Do not use `/etc/paths.d` for GUI visibility — it is login-shell-only and irrelevant to the problem D5 solves (finding 11).
- **Cannot promise**: retroactive visibility for GUI apps already running before the LaunchAgent first loads.

### D2 — POSIX trampoline home derivation

- Keep `$0`-derivation as the mechanism — it is reliable for the dominant case because a script's `$0` rides the shell's resolved execve pathname, not the unreliable ELF `argv[0]` convention (finding 22).
- **Sharpen**: resolve via `readlink -f "$0"` (with a manual symlink-walk loop as the POSIX-strict/non-GNU fallback), not bare `dirname "$0"`, to close the one real precedent-documented gap — a symlink placed at some path *other* than the generated trampoline pointing at it. rustup hit this exact class of bug twice (finding 14). Since D1 makes the trampoline a real file (not itself a symlink), this only bites when something external to OCX symlinks to it — low probability, but cheap to close and cheap to regression-test: generate a trampoline, symlink it elsewhere, invoke through the symlink, assert it still resolves the real `<home>`.
- **Cannot promise**: correctness under a caller that constructs its own `execve()` with a non-canonical (e.g. relative or otherwise non-resolved) pathname, bypassing normal shell PATH-search semantics — an inherent limit of any `$0`-based scheme, and the exact reason every compiled proxy surveyed (rustup, mise, aqua, volta) uses a syscall-backed self-path API once it can afford to be a compiled binary (finding 13). This is the option D2 explicitly declines in favor of script simplicity/auditability, consistent with OCX's existing `env.*` shim precedent — note it as a stated trade-off, not an oversight.
- **Performance**: a generated `sh` trampoline pays a `system()`-shaped cost, categorically more than a compiled proxy's single `execve()` (finding 19) — sub-millisecond-to-low-single-digit-millisecond in practice, not a reason to change D2, but worth recording for a future fan-out-heavy escalation (e.g. `ocx exec` invoked through many trampolines inside a build system).

## Sources

| Source | Type | Date | Relevance |
|---|---|---|---|
| [rust-lang/rustup#261](https://github.com/rust-lang/rustup/issues/261) | GitHub issue | 2016-04 (reported) | REG_SZ vs REG_EXPAND_SZ bug origin |
| [`rustup/src/cli/self_update/windows.rs`](https://github.com/rust-lang/rustup/blob/master/src/cli/self_update/windows.rs) | Source | current | Confirms unconditional `set_expand_hstring` fix |
| [2called-chaos/mcl#13](https://github.com/2called-chaos/mcl/issues/13) | GitHub issue | — | `setx` 1024-char limit |
| [desktop/desktop#18176](https://github.com/desktop/desktop/issues/18176) | GitHub issue | — | Real-world PATH truncation damage |
| [MS Learn: WM_SETTINGCHANGE](https://learn.microsoft.com/en-us/windows/win32/winmsg/wm-settingchange) | Official docs | current | Broadcast mechanism, semantics |
| [ofek/userpath#40](https://github.com/ofek/userpath/issues/40) | GitHub issue | — | SendMessageTimeout vs SendMessage practice |
| [Baeldung: User vs System Variables](https://www.baeldung.com/cs/user-vs-system-variables) | Blog/explainer | recent | Windows combined PATH ordering |
| [docs.volta.sh/advanced/installers](https://docs.volta.sh/advanced/installers) | Official docs | current | Volta Windows PATH behavior |
| [docs.astral.sh/uv/reference/installer](https://docs.astral.sh/uv/reference/installer/) | Official docs | current | `UV_NO_MODIFY_PATH` precedent |
| [man7.org: environment.d(5)](https://man7.org/linux/man-pages/man5/environment.d.5.html) | Man page | current | Canonical environment.d spec |
| [man7.org: pam_env(8)](https://man7.org/linux/man-pages/man8/pam_env.8.html) | Man page | current | `.pam_environment` deprecation |
| [Kisaragi Hiu: Migrating away from .pam_environment](https://kisaragi-hiu.com/migrating-away-from-pam-environment/) | Blog | recent | Arch Linux deprecation timeline (2022-10-20) |
| [goral.net.pl: Xsession in Debian](https://goral.net.pl/post/xsession/) | Blog | recent | LightDM/SDDM/GDM profile-sourcing comparison |
| [ArchWiki: LightDM](https://wiki.archlinux.org/title/LightDM) | Wiki | maintained | LightDM Xsession default behavior |
| [ArchWiki: Environment variables](https://wiki.archlinux.org/title/Environment_variables) | Wiki | maintained | GNOME/KDE systemd-session environment.d carve-out |
| [systemd.io: Desktop Environment Integration](https://systemd.io/DESKTOP_ENVIRONMENTS/) | Official docs | current | systemd --user as session leader |
| [gist: apply shell PATH to macOS GUI apps via launchd](https://gist.github.com/riaf/cf662d965ebd1b8b47453dd79cdd5578) | Community script | recent | launchctl setenv + LaunchAgent pattern |
| [bounga.org: Set system-wide PATH for macOS GUI apps](https://www.bounga.org/tips/2020/04/07/instructs-mac-os-gui-apps-about-path-environment-variable/) | Blog | 2020 (still cited) | Same pattern, independent confirmation |
| [scriptingosx.com: Where PATHs come from](https://scriptingosx.com/2017/05/where-paths-come-from/) | Blog | 2017 — dated | `/etc/paths.d` + `path_helper` mechanism (cross-checked) |
| [jetbrains.com/help/toolbox-app/installation](https://www.jetbrains.com/help/toolbox-app/installation.html) | Official docs | current | Confirms no major tool ships GUI-PATH fix by default |
| [jdx.dev: Shims — how they work in mise-en-place](https://jdx.dev/posts/2024-04-13-shims-how-they-work-in-mise-en-place/) | Blog (maintainer) | 2024-04 | argv0-identity dispatch pattern |
| [mise.jdx.dev/dev-tools/shims.html](https://mise.jdx.dev/dev-tools/shims.html) | Official docs | current | Shims vs activate trade-offs |
| [aquaproj.github.io/docs/products/aqua-proxy](https://aquaproj.github.io/docs/products/aqua-proxy/) | Official docs | current | aqua-proxy dispatch mechanism |
| [aquaproj.github.io/docs/reference/execve-2](https://aquaproj.github.io/docs/reference/execve-2/) | Official docs | current | Direct execve since aqua v2.5.0 |
| [rust-lang/rustup#2858](https://github.com/rust-lang/rustup/issues/2858) | GitHub issue | — | Hardlink-to-symlink breakage (macOS backup tools) |
| [rust-lang/rustup#3136](https://github.com/rust-lang/rustup/issues/3136) | GitHub issue | — | Hardlink portability follow-up |
| [rust-lang/rustup#4224](https://github.com/rust-lang/rustup/issues/4224) | GitHub issue | — | 1.28.0 symlink-resolution regression |
| [ScoopInstaller/Shim README](https://github.com/ScoopInstaller/Shim/blob/main/README.md) | Official docs | current | Sidecar-file + self-location precedent for `.shim`/`.shimref` |
| [python/cpython#136051](https://github.com/python/cpython/issues/136051) | GitHub issue | open | `pyvenv.cfg` relocatability still unresolved |
| [conda/conda#8703](https://github.com/conda/conda/issues/8703) | GitHub issue | — | conda init non-idempotency |
| [blog.famzah.net: fork gets slower](https://blog.famzah.net/2009/11/20/fork-gets-slower-as-parent-process-use-more-memory/) | Blog/benchmark | dated, still cited | vfork/exec/system/posix_spawn relative costs |
| [rust-lang/rustup#170](https://github.com/rust-lang/rustup/issues/170) | GitHub issue | — | PATH-modification opt-out precedent |
| [docs.brew.sh/Homebrew-on-Linux](https://docs.brew.sh/Homebrew-on-Linux) | Official docs | current | Homebrew-on-Linux profile-sourcing advice |
| [golinuxcloud.com: get script directory reliably](https://www.golinuxcloud.com/get-script-name-get-script-path-shell-script/) | Blog/explainer | recent | `$0` resolution semantics for PATH-found scripts |
| [ostricher.com: The Right Way to Get the Directory of a bash Script](https://www.ostricher.com/2014/10/the-right-way-to-get-the-directory-of-a-bash-script/) | Blog | dated, still standard reference | `readlink -f` + portable symlink-walk fallback |
