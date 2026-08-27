# Brief: Shell Environment Overhaul (direnv replacement)

**Date:** 2026-08-24
**Status:** Scope confirmed by owner; input to `/hex-architect`
**Supersedes as framing:** `adr_live_env_reload.md` (phase-2-of-links framing dropped)
**Non-goal:** toolchain links / `ocx select` ([#189](https://github.com/ocx-sh/ocx/issues/189)) — separate track, no sequencing claim either way

## Why re-framed

`adr_live_env_reload.md` was written as phase 2 of `adr_project_toolchain_links.md`.
That dependency is not real: the reconciler drives current env C toward desired D
regardless of whether D's strings are digest paths or link paths, and element
ownership is a prefix test that covers both shapes. Links buy a per-prompt no-op in
the common case — an optimization, not a precondition.

The two initiatives solve disjoint classes:

| | Reaches | Solves |
|---|---|---|
| Env overhaul (this) | shells, at next prompt | direnv replacement: enter/leave, project switch, post-update staleness, Windows/pwsh project story |
| Toolchain links ([#189](https://github.com/ocx-sh/ocx/issues/189)) | everything incl. processes that never re-read env | frozen envs — running IDE holding `JAVA_HOME`/PATH, no restart |

## Confirmed scope

1. **Private state carrier.** Session ledger under `__OCX_*`. Distinct from `OCX_ENV`
   (see downstream findings — that is a live forwarding contract, not pollution to hide).
   One naming rule, one visibility rule, one size/degradation rule.
2. **Reconciler.** C/D/L per prompt, per scope stack. Enter, leave, project switch,
   `ocx update` freshness in a live shell. PowerShell + nushell first-class.
3. **Enablement, symmetric with completions.** `--hook`/`--no-hook` on **both**
   `ocx self setup` and `ocx self activate`, plus `OCX_NO_HOOK`, neither-flag = auto.
   Mirrors the shipped `--completion`/`--no-completion` + `OCX_NO_COMPLETIONS=1` +
   stderr-TTY probe (`activate.rs:53-61`).
   `self setup --[no-]hook` writes `[shell] hook = true|false`; **flag absent → nothing
   written**, default applies.
4. **Regeneration + lag.** `self setup` writes per-shell env shims into `$OCX_HOME`;
   `self update` rewrites them; `env.sh` sets `OCX_HOME` then calls `ocx self activate`.
   **Corrected 2026-08-24 by `research_shell_integration_rollout.md`** — the earlier framing
   ("hook logic lands one `self update` later") is FALSE for this codebase. The shims are thin
   dispatchers that `eval` the output of `ocx self activate` at every shell start
   (`shims.rs:63`, `:103`, `:140`, `:239`; a unit test asserts `invokes_binary` at `:461-464`),
   so hook *logic* changes reach every NEW shell immediately with no shim rewrite. What actually
   lags is only (a) the shim's own wrapper shape — rare, and diff-gated via `needs_write` /
   `refresh_shims` — and (b) an already-running shell, which no surveyed tool solves. The ADR
   must state the thin-dispatcher invariant, and scope the lag risk to those two cases only.
   **Precise mechanism for (a)**, confirmed at `update.rs:109-111`: `refresh_shell_integration_after_swap`
   runs in the OLD binary still in memory, so a hop that swaps in a binary carrying a new shim/RC
   **body** does not heal on that hop — the heal lands on the next `self update` or a `self setup`
   re-run. Best-effort throughout: a failure warns and advises re-running setup, never fails the
   update. So the one-hop lag is a property of body regeneration, not of activation behavior.
5. **Per-project state layout.** Currently three keying schemes: `state/` flat feature
   dirs (`update-check/`, `referrers/`, `trust_root/`), the `$OCX_HOME/projects/` symlink
   ledger keyed by `ReferenceManager::name_for_path`, and phase 2's proposed
   `state/activation-consent/<project-key>` would be a third. Define one project-key
   derivation, one per-project state dir, GC'd off the ledger that already tracks project
   liveness. In scope here — consent stamps and the private carrier are both state-shape
   questions.
6. **Consent model.** Fresh clone inert. Stamp binds path AND project identity;
   source-set drift re-confirms. Residual: publisher compromise inside a consented
   namespace gets PATH-front code at next prompt, silently (OD-3).

## Owner decisions (this session)

- **Whitelist lives in `config.toml`, never `ocx.toml`.** It is user configuration, not
  toolchain. Structural consequence: the project cannot write its own trust entry, which
  removes the mise CVE class (GHSA-436v-8fw5-4mj8: project settings loaded before the
  trust check) by construction rather than by sequencing discipline.
- **Managed config may ship the whitelist** — fleet pre-trusts internal namespaces.
  Contrast with `[shell] hook` (OD-2), still open.
- **Env var for the whitelist too** — devcontainer pre-whitelists the repo checkout.
- Shim/PATH shadowing is settled and untouched: shim slot pushed first so it resolves
  last, `entrypoints/` > `bin/` > `shims/` (`composer.rs:1072-1102`, C-012/S-004).
  Materialization makes compose-time-empty dirs real and the same exported env stops
  routing through the shim — no mutation, no repoint.

## Downstream coupling (verified 2026-08-24)

- **`ocx-sdk-python` — real.** `OCX_ENV` documented as opaque pass-through with
  `OCX_PATCHES`/`OCX_BINARY_PIN` (`.claude/rules/architecture.md:27`); malformed = hard
  startup abort; `_env.py:306` rejects `--env` keys in `OCX_*`/`__OCX_*` at exit 64.
  **Seven golden help fixtures** (`tests/fixtures/cli/*.help.txt`) snapshot ocx's help
  sentence verbatim — rewording it breaks them mechanically.
- **`rules_ocx` — behavioral only.** The `OCX_ENV` in generated `env.bzl`
  (`repo_utils.bzl:1216`) is a Starlark constant rules_ocx names itself, unrelated to
  ocx's env var. Actual dependency at `repo_utils.bzl:40`: `OCX_ENV` is deliberately
  absent from `_OCX_NEUTRALIZED_ENV` because "ocx strips an inherited one itself on every
  compose, and its decoder refuses `OCX_*` keys outright". Changing that stripping
  silently invalidates their neutralization set, with no test to catch it.
- **`find_ocx` — no coupling.** `__OCX_PIN_VERSION`, `__OCX_DIST_JSON`,
  `__OCX_MODULE_VERSION`, `__OCX_PASSTHROUGH_VARS` are CMake variables in `ocx.cmake`,
  not environment variables. Spelling collision only — do not "harmonize".

**Conclusion:** `__OCX_*` is already a reserved namespace with a downstream-enforced
contract, so a private `__OCX_ENV_STATE` needs no new reservation and breaks nothing.
Repurposing `OCX_ENV` is not free — it is a live forwarding contract with a real
consumer, distinct from the session-state carrier.

## Reuse, do not discard

`adr_live_env_reload.md` has been through an adversarial round already (ledger spoof
channel closed, reserved-key gate, `set -u` discipline, pwsh ordering, nushell spike
gate, 10-shell mechanics matrix). `research_shell_env_reconciler_and_launcher_farm.md`
is the prior-art survey (mise, direnv, volta, proto, scoop, pyenv/rbenv/asdf).
Both are inputs. Supersede the ADR, do not amend it.

## Open decisions to resolve

- **OD-2** — `[shell]` in the managed tier: local-only vs fleet-controllable hook toggle.
  (Note the whitelist decision above went the other way — managed tier ships it.)
- **OD-3** — consent residual: accept silent digest-swap within a consented namespace
  (documented) vs digest-set re-confirm (fires on every legitimate lock bump).
- Whitelist grammar: direnv's `prefix`/`exact` shape, and precedence between
  config tiers + env var.
