# Research: SOTA + Known-Pitfall Gap Check on `adr_shell_env_overhaul.md`

**Date:** 2026-08-25
**Axis:** domain knowledge / falsification pass
**Consumer:** the implementation plan. This is a **deltas-only** artifact — it does not restate the ADR.

Two real contradictions found. Both are wrong *citations*, not wrong claims — and in both cases a
**better** citation exists that makes the ADR's own argument stronger.

## 1. Cited external claims — verdicts

| Claim | Verdict | Evidence |
|---|---|---|
| mise `EnvDiffOperation::{Add,Change,Remove}`, `__MISE_ORIG_PATH` heuristic | **confirmed** | `src/env_diff.rs`, typed diff keyed by `__MISE_DIFF`; `__MISE_ORIG_PATH` = pre-mise PATH snapshot |
| ~4 ms no-op / ~14 ms reload | **confirmed** | two independent sources converge ("4 ms if no changes, 14 ms full reload"; a second cites ~5 ms cache hit) |
| mise 2026.8.0 shipped unconditional apply, reversed in 2026.8.9 ([jdx/mise#12094](https://github.com/jdx/mise/issues/12094)) | **confirmed, strongly** | [v2026.8.9 release notes](https://github.com/jdx/mise/releases/tag/v2026.8.9), verbatim: *"Runtime environment overrides now persist between refreshes, with changes made via export, shell aliases, sourced scripts, or direct PATH edits no longer reverted on every prompt."* Shipped **2026-08-19**, four days before the ADR — known history, not coincidence. [PR #12095](https://github.com/jdx/mise/pull/12095) |
| direnv hard-fails on a corrupt `DIRENV_DIFF` ([#519](https://github.com/direnv/direnv/issues/519)) | **confirmed (historic)** | `direnv: error unmarshal() base64 decoding: illegal base64 data`, 2019, closed. Current-version behaviour not re-verified |
| `DIRENV_WATCHES` E2BIG ([doom-emacs#2335](https://github.com/hlissner/doom-emacs/issues/2335)) | **confirmed** | `Argument list too long` traced to `DIRENV_WATCHES` growth |
| direnv whole-env-capture class ([#82](https://github.com/direnv/direnv/issues/82), [#1249](https://github.com/direnv/direnv/issues/1249)) | **confirmed** | #82: unrelated `COMP_WORDBREAKS`/tmux vars deleted by a PATH-only `.envrc`. #1249: an *empty* `.envrc` still rewrites PATH — **open** |
| direnv tmux/SSH reattach ([#106](https://github.com/direnv/direnv/issues/106)) | **confirmed (tmux only)** | open since 2014; the SSH-reattach case is the same timing class but not literally in the thread |
| mise **GHSA-436v-8fw5-4mj8** | **confirmed** | "Local settings bypass config trust checks" — an untrusted `.mise.toml` sets trust-control values read *before* the trust check, letting `[env] _.source` execute. Affects 2026.2.18–2026.6.4, high, published 2026-04-03. Maps to **CVE-2026-35533** |
| **CVE-2026-33646** | **confirmed, genuinely distinct** | `.tool-versions` is parsed through Tera with `exec()` registered and is **not** trust-gated the way `.mise.toml` is (fixed ≥2026.3.10). Exactly "a project file format with no trust gate at all" |
| git `safe.directory` exact-path-only, "only respected in protected configuration" | **confirmed, literal quote match** | git docs: no wildcard/recursive form |
| [PowerShell#3571](https://github.com/PowerShell/PowerShell/issues/3571) casing | **confirmed, and permanently unresolved** | closed `Committee-Reviewed` / `Resolution-No Activity` — discussed, not fixed |
| [fish-shell#8604](https://github.com/fish-shell/fish-shell/issues/8604) / [#9147](https://github.com/fish-shell/fish-shell/issues/9147) for the index-shift hazard | **weak citation** | both support "fish has no remove primitive" (feature requests for `fish_remove_path`); **neither discusses multi-element index shift**. The mechanic is real fish behaviour but is not documented in either thread — see the better citation in §5 |
| [nushell#14944](https://github.com/nushell/nushell/issues/14944) | **confirmed, exact match** | lowercase `pwd` fails to fire an `env_change.PWD` hook |
| [Warp#5219](https://github.com/warpdotdev/Warp/issues/5219) | **confirmed** | joining array elements with `;` breaks on `cmd &` elements |
| [vscode#158090](https://github.com/microsoft/vscode/issues/158090) | **confirmed** | `__vsc_status="$?"` reassigned inside the array loop, capturing the penultimate exit code |
| conda **#9597** "silent wrong restore" | **CONTRADICTED — wrong issue** | #9597 is about `conda activate --stack` and `site-packages`, unrelated. See §5 item 2 |
| pyenv **#2829** "stale-lock wedge" | **partially confirmed** | real, but it is a rehash race leaving a stuck `.pyenv-shim` marker, not a lock file. Fixed by [PR #3450](https://github.com/pyenv/pyenv/pull/3450) |
| [vscode#313681](https://github.com/microsoft/vscode/issues/313681) symlink state fork | **confirmed, open** | `workspaceStorage` hashes the raw opened path; symlink vs canonical = two buckets, no merge |
| pnpm 10.x `projects/` symlink registry | **confirmed, with a fresh caveat** | real, enables mark-and-sweep `pnpm store prune`; but circular symlinks in that registry already broke `pnpm deploy` in 10.27.0 ([pnpm#10411](https://github.com/pnpm/pnpm/issues/10411), open) |
| Nix `gcroots/auto/` never cleans stale entries | **CONTRADICTED — wrong directory** | see §5 item 1 |

## 2. Changed since 2026-08-25

- mise v2026.8.10 → v2026.8.12 shipped 2026-08-20…24 — no further env-diff or trust changes.
- nushell 0.115.0 (2026-08-15) and 0.115.1 (2026-08-23) are current; no hooks/env fix relevant to the spike.
- No new CVE or advisory in the shell-activation / project-trust class for direnv, mise or asdf.
- **No tool has shipped this combination** (typed provenance ledger + OCI-namespace consent whitelist +
  per-prompt reconciler). mise's 2026.8.9 fix is the closest adjacent move and **independently
  corroborates Decision 3's `D ≠ L` apply gate** — cite it as external validation, not only as a lesson.

## 3. The nushell gating question

**(a) Removing one PATH element from inside `env_change.PWD`:** likely works, not independently
reproduced. Nushell's [hooks docs](https://www.nushell.sh/book/hooks.html) state hook blocks preserve
environment "in a similar way as `def --env`", so an ordinary list reassignment
(`$env.PATH = ($env.PATH | where $it != $target)`) should propagate. No report of it failing in a hook.

**(b) `hide-env` to unset, visible after the hook returns: still no, as of 0.115.1 (2026-08-23).**
[#6593](https://github.com/nushell/nushell/issues/6593) reports `hide`/`hide-env` failing specifically
*inside* `env_change` hook blocks while working at the REPL. [#11818](https://github.com/nushell/nushell/issues/11818)
and [#15872](https://github.com/nushell/nushell/issues/15872) are open, still tracking a real `unset`
primitive. The *adjacent* leak — a hidden var reaching child processes — was fixed by
[PR #12901](https://github.com/nushell/nushell/pull/12901) via `env_clear()`, but that answers a
different question.

**Consequence for the plan:** the gating decision is correct and the evidence strengthens it. There is
no workaround. Track #6593 / #11818 / #15872 as live upstream issues rather than treating the spike as
a formality — there is a real chance nushell constant-revert stays permanently unimplementable, which
Decision 6(b)'s "until that spike lands" wording should accommodate.

## 4. Adversarial gap check

| Angle | Verdict |
|---|---|
| Env block limits (Windows 32767 whole-block, Linux `MAX_ARG_STRLEN` 128 KiB **per string**, macOS `ARG_MAX`) | **figures confirmed, already covered.** Decision 1 states the Windows factor-of-two honestly and notes the shared budget. No gap |
| `SHLVL`, `exec`-replacement | **covered** — Invariant L-2 and Option B's rationale |
| `su` / `sudo -E`, ssh `AcceptEnv`/`SendEnv` | **real, structurally absorbed, never named.** `sudo` without `-E` strips the env (ordinary absent-ledger path). `sudo -E` or a permissive `SendEnv __OCX_ENV_STATE` could carry a **stale carrier across a host boundary** where `$OCX_HOME` and the project dir both differ. Decision 1 rule (a) — *a `dir` that does not match the walk's result invalidates the whole project scope* — catches it by construction. **Add one sentence to the Security NFR** so a reviewer need not re-derive it |
| tmux/screen `update-environment`, systemd user session | **covered** — Decision 9's tmux bullet subsumes both; nothing but the hook reads the carrier |
| IDE terminal + prompt framework + ocx, all wrapping `PROMPT_COMMAND` | **partially covered.** The append-only design names starship/oh-my-zsh/p10k/direnv and cites the Warp/VS Code array bugs, but not a **three-way** stack. Low severity — each wrapper's contract is append-preserving — but worth one line |
| `set -u` / `set -e` / pipefail, POSIX-mode bash | **mixed.** `set -u` explicitly handled. `set -e` is *likely* safe if the freshness test is `[[ file -nt stamp ]] && ocx …` (errexit does not fire inside an `&&` list) but is **not asserted as a contract anywhere** — needs a regression test, not a redesign |
| **Restricted shells (`rbash`/`rksh`)** | **REAL, UNADDRESSED GAP.** They forbid setting/unsetting `PATH` and forbid invoking any command containing `/` — both of which the emitted hook does unconditionally. D3's "the hook must never break a prompt" has **no stated behaviour** for this class |
| Non-interactive `bash -c`, login vs non-login | **covered** — interactive-only default with per-shell detection excludes `bash -c` |
| Base64url blob visible in `ps` / `/proc/*/environ` / crash reporters / CI env dumps | **not a new risk.** The ledger only mirrors values already present as ordinary env vars or path strings; no marginal exposure |

## 5. Ranked gaps for the plan

1. **The Nix citation names the wrong directory.** `gcroots/auto/` **does** self-heal — indirect roots are
   collected when the intermediate symlink is gone. The real never-cleaned bug is **`gcroots/per-user/`**:
   [NixOS/nix#7166](https://github.com/NixOS/nix/issues/7166) states the inconsistency verbatim, caused by
   nix-direnv creating per-user roots that go stale
   ([nix-direnv#242](https://github.com/nix-community/nix-direnv/issues/242)). The comparative point
   survives; the citation must move.
2. **The conda citation is wrong and a strictly better one exists.** Swap #9597 for
   [conda#12769](https://github.com/conda/conda/issues/12769): when the user's pre-activation value
   *coincidentally equals* the value conda is about to set, conda skips creating the backup var, so
   `deactivate` **unsets** the variable instead of restoring it (regression introduced in 23.1.0). That is
   precisely the scenario Decision 3's "Coincidence" clause gets right — the swap makes the design
   argument stronger, not merely correct.
3. **Restricted shells can break the prompt.** No stated behaviour for `rbash`/`rksh`; D3's invariant is
   untested against a shell that forbids `PATH` mutation and slash-containing commands. Needs an explicit
   detect-and-silently-no-op path.
4. **The nushell `hide-env` question is three live upstream issues, not one spike.**
   [#6593](https://github.com/nushell/nushell/issues/6593), [#11818](https://github.com/nushell/nushell/issues/11818),
   [#15872](https://github.com/nushell/nushell/issues/15872) — all open at v0.115.1. Carry the risk that it
   never lands.
5. **The fish citations do not document the hazard they are cited for.** Replace #8604/#9147 with
   [fish-shell#7776](https://github.com/fish-shell/fish-shell/issues/7776) (`fish_add_path`:
   `set --erase` incorrectly handles indices), or mark the hazard explicitly as codebase-derived — the
   same honesty bar the ADR already applies to its own two internally-discovered hazards.
6. **SSH env-forwarding cross-host carrier leakage is handled but never named.** One sentence in the
   Security NFR citing Decision 1 rule (a).
