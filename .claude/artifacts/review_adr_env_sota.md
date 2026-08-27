# SOTA / Known-Pitfall Gap Check — `adr_shell_env_overhaul.md`

Phase 5 review, focus: what the design misses that the field already knows.
Gaps only, most consequential first. Prior art already covered by
`research_shell_env_reconciler_and_launcher_farm.md` and the four Phase-2
artifacts is deliberately not repeated.

## G-1 — mise shipped this ADR's apply rule and reverted it five days ago

mise 2026.8.0 added continuous per-prompt enforcement (reapply desired env
unconditionally); **2026.8.9 (2026-08-19, jdx/mise#12094) reversed it**:
"Runtime environment overrides now persist between refreshes... no longer
reverted on every prompt, reversing the continuous enforcement introduced in
2026.8.0."

Decision 3's fingerprint fast-path dodges the *common* case (a no-op skips apply
entirely when nothing watched changed). But on **any** fingerprint change,
"Apply/update: set to D unconditionally" reapplies **every** constant key in D,
including keys unrelated to what changed — silently clobbering a manual
mid-session `export`. That is exactly the failure mode mise just walked back.

Note the internal asymmetry this creates: exit restores the prior only if
`C == L` (explicitly to avoid clobbering a user's override), while apply has no
equivalent guard. The same protection exists on one side only.

**Fix**: scope "set unconditionally" to keys whose composed value actually
changed, or accept the narrower blast radius explicitly and say why.
https://github.com/jdx/mise/releases/tag/v2026.8.9

## G-2 — Coexistence is direnv-only; mise is unaddressed

Validation lists only "direnv coexistence (detect `DIRENV_DIR`, yield)". mise's
own docs say "you should not use direnv with mise... incompatibilities are not
considered bugs", and the failure axis named is PATH ordering between two
per-prompt PATH-prepending hooks — precisely OCX's mechanism versus mise's. No
`MISE_SHELL` / `__MISE_ORIG_PATH` sentinel check is proposed.

**Fix**: add a symmetric yield, or document mise coexistence as an explicit
non-goal. https://mise.jdx.dev/direnv.html

## G-3 — Hook append-safety is committed for PowerShell only

Decision 5 commits PowerShell's `prompt` to "wrapped, never clobbered" but says
nothing equivalent for bash/zsh/fish — the same points starship, oh-my-zsh,
powerlevel10k and direnv hook.

- Bash 5.1+'s optional `PROMPT_COMMAND` **array** has bitten Warp
  (warpdotdev/Warp#5219 — syntax errors on semicolon-terminated elements) and
  VS Code (microsoft/vscode#158090 — exit-code `$?` clobbered, array vs string).
- zsh's safe idiom is `precmd_functions+=(...)` / `add-zsh-hook precmd`, never
  overwriting `precmd()`. That is how starship avoids clobbering.

**Fix**: state the same append-only guarantee, per shell, that the ADR already
states for PowerShell.

## G-4 — `remove_path_element`: two citable footguns beyond the two already named

- **PowerShell**: naive `-notlike` / substring removal over-matches — stripping
  `C:\WINDOWS` also strips `C:\WINDOWS\system32` unless the match is
  segment-exact. Separately, pwsh env-var **names** are case-sensitive on
  Linux/macOS but not on Windows, so `$env:PATH` and `$env:Path` are different
  variables cross-platform (PowerShell/PowerShell#3571). Needs segment-exact
  matching plus platform-conditional casing, not scalar ops.
- **fish**: no built-in remove primitive; the field workaround is index-based
  `set -e fish_user_paths[N]`, and removing 2+ elements in one call shifts every
  later index — must go highest-index-first or re-resolve. Still an open feature
  request for exactly this reason (fish-shell#8604, #9147).
- **zsh/bash**: no public bug report found for the glob-over-match / escaping
  hazard the ADR names. The caution is correct but externally unverified — do
  not over-cite it as "known".

## G-5 — Nushell spike should probe a specific known failure

nushell/nushell#14944 — `env_change.PWD` silently did not fire when PWD was
lowercase (case-sensitivity regression). Exactly the failure class the mandated
red+green nushell spike exists to catch ("hook silently doesn't fire"); name it
as a spike target.

## G-6 — tmux / SSH: settled, no action needed

direnv/direnv#106 (stale PATH after tmux reattach) is a shell-init timing bug,
not a per-prompt one. This design recomputes D from truth every prompt
regardless of L's staleness, so the classic tmux/direnv pain does not transfer.
A one-line note would preempt a reviewer raising it; no design change needed.

IDE-terminal staleness (JetBrains snapshots process env once at launch) reduces
to the already-explicit [#189](https://github.com/ocx-sh/ocx/issues/189)
"processes that never re-read env" scoping — real, but outside this ADR's
boundary.

## Sources

https://github.com/jdx/mise/releases/tag/v2026.8.9 ·
https://mise.jdx.dev/direnv.html ·
https://github.com/warpdotdev/Warp/issues/5219 ·
https://github.com/microsoft/vscode/issues/158090 ·
https://github.com/PowerShell/PowerShell/issues/3571 ·
https://github.com/fish-shell/fish-shell/issues/8604 ·
https://github.com/nushell/nushell/issues/14944 ·
https://github.com/direnv/direnv/issues/106 ·
https://intellij-support.jetbrains.com/hc/en-us/articles/15268184143890-Shell-Environment-Loading
