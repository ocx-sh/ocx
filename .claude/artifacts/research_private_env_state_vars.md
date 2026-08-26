# Research: Private env-var conventions for session state

**Date:** 2026-08-24
**Axis:** 4 of 4 — env overhaul ADR (`brief_env_overhaul.md` scope item 1 + downstream coupling)
**Consumed by:** `adr_shell_env_overhaul.md`

## Per-tool findings

**direnv** — `DIRENV_DIFF`, `DIRENV_WATCHES`, `DIRENV_DIR`. Encoding is "gzenv":
`json.Marshal` → zlib → base64 URL-encode
([gzenv](https://pkg.go.dev/github.com/direnv/direnv/v2/gzenv)). Hidden from the printed diff by a
literal prefix check — `direnvKey(key) { return strings.HasPrefix(key, "DIRENV_") }`, skipped in
`diffStatus()` ([env_diff.go](https://github.com/direnv/direnv/blob/master/internal/cmd/env_diff.go)).
`LoadEnvDiff` returns `(diff, err)` from `gzenv.Unmarshal` — a hard error propagated to the caller,
so a corrupt `DIRENV_DIFF` surfaces as a visible
`direnv: error unmarshal() base64 decoding: illegal base64 data...`
([#519](https://github.com/direnv/direnv/issues/519)). Known real-world failure: `DIRENV_WATCHES` /
`DIRENV_DIFF` growing unbounded in long-lived shells until `execve` fails with E2BIG
([doomemacs#2335](https://github.com/hlissner/doom-emacs/issues/2335)). No signature or HMAC —
`Unmarshal` only checks structural decode, so direnv trusts whatever the shell hands it as prior
state; the real security boundary is the separate `.envrc` content-hash allow-list.

**mise** — `__MISE_DIFF`, `__MISE_SESSION`, `__MISE_ORIG_PATH`, plus per-shell coordination vars
(`__MISE_ZSH_PRECMD_RUN`). Encoding is MessagePack → zlib → base64. `hook_env.rs` (`PREV_SESSION`
static) does
`env::var("__MISE_SESSION").ok().and_then(|s| deserialize(s).map_err(|err| warn!("error deserializing __MISE_SESSION: {err}")).ok()).unwrap_or_default()`
— **warn, ignore, rebuild from empty default**, never a hard failure
([hook_env.rs](https://github.com/jdx/mise/blob/main/src/hook_env.rs)). The double underscore is
purely a social convention marking "internal, not user-facing"; no shell and no mise-internal code
treats `__`-prefixed vars specially beyond mise's own reserved-key list.

**venv / conda** — architecturally different: no encoded blob, N flat stash vars. venv:
`VIRTUAL_ENV`, `_OLD_VIRTUAL_PATH` (single-underscore stash of pre-activation PATH), restored
verbatim by `deactivate()`. conda: `CONDA_SHLVL` (activation depth), `CONDA_PREFIX_1`,
`CONDA_PREFIX_2`, … — one var per stack level, no diff representation at all. The known breakages
follow directly from that design: stacking (`--stack`) leaves `CONDA_PREFIX_N` stale across nested
activations ([conda#9597](https://github.com/conda/conda/issues/9597)), and Windows nested
activation picks the wrong interpreter because PATH-restoration ordering is fragile
([conda#9578](https://github.com/conda/conda/issues/9578)). Neither validates nor authenticates
these vars — `deactivate` blindly trusts `_OLD_VIRTUAL_PATH` exists and is well-formed.

## Comparison

| Tool | Naming | Encoding | Typical size | Corruption handling | Forgery defense |
|---|---|---|---|---|---|
| direnv | `DIRENV_*` (single underscore, prefix-hidden from diff) | JSON → zlib → base64 | KB-scale; unbounded growth is a known bug class | hard error, propagated | none — structural decode only; boundary is the `.envrc` allow-list |
| mise | `__MISE_*` (double underscore, social convention) | MessagePack → zlib → base64 | small (msgpack denser than JSON) | warn + ignore + rebuild from default | none — same posture |
| venv/conda | flat per-purpose vars (`_OLD_VIRTUAL_PATH`, `CONDA_PREFIX_N`) | none (plain strings) | trivial | none — blind trust, causes documented stacking bugs | none |
| Windows env block | — | — | hard cap 32767 chars (XP/2003); effectively 2 GiB (Vista+), still shared across all vars | — | — |
| Linux env block | — | — | `ARG_MAX`, kernel ≥2.6.23: ¼ of `RLIMIT_STACK`; single-string cap `MAX_ARG_STRLEN` = 32 pages (128 KiB) historically, up to 6 MiB patched | — | — |

## Recommendation

- **Naming** — `__OCX_ENV_STATE`, one blob, not N flat vars. The double-underscore prefix is
  already an enforced reservation downstream (`ocx-sdk-python` rejects `OCX_*`/`__OCX_*` at exit
  64) — reuse it rather than inventing a third convention. Reject the venv/conda flat-var pattern:
  OCX targets PowerShell and nushell as first-class, and one opaque blob is far cheaper to thread
  through 3+ shell grammars than N reserved scalar names per shell.
- **Encoding** — base64(JSON), no compression. direnv and mise compress because their payloads
  (watched-file mtime lists, full env diffs across deep tool chains) grow large; OCX's ledger is a
  handful of package-scoped PATH entries and env scalars — hundreds of bytes to low KB. zlib or
  msgpack is complexity with no evidenced payload to justify it (Choose Boring Technology / YAGNI).
  Revisit only if measurement shows payloads approaching the cap.
- **Size cap** — hard cap at 16 KiB, comfortably under every platform floor (old-kernel Linux
  single-string cap 128 KiB; Windows XP/2003 whole-block cap 32767 chars shared with everything
  else). Cap exceeded degrades the same way corruption does.
- **Degradation rule** — corrupt, truncated, or foreign-written `__OCX_ENV_STATE` → treat the shell
  as `C = ∅` and rebuild `D` fully from truth (project config). Same as mise's warn-ignore-rebuild.
  Never direnv's hard refuse (an env reconciler that can brick a prompt on a bit-flip is worse than
  a stale prompt) and never a silent no-op. Log at **debug**, not warn — an absent or foreign
  ledger is the normal first-shell case, not an anomaly, per OCX's "no WARN on common benign
  states" doctrine.
- **Forgery defense** — none, matching all three tools; attempting it would move the trust question
  rather than answer it. The var must never be a trust or authorization input, only a hint for
  computing the diff to apply. The structural defense is that `D` is always recomputed
  independently from project config plus the separate consent model — never derived from what the
  ledger claims was previously applied. A forged ledger's worst case is then a wrong *diff* (ocx
  fails to unset something it believes was not previously set), not code execution or a bypassed
  consent check — the same blast-radius ceiling direnv and mise accept.

Sources: [gzenv](https://github.com/direnv/direnv/blob/master/gzenv/gzenv.go) ·
[env_diff.go](https://github.com/direnv/direnv/blob/master/internal/cmd/env_diff.go) ·
[direnv#519](https://github.com/direnv/direnv/issues/519) ·
[doomemacs#2335](https://github.com/hlissner/doom-emacs/issues/2335) ·
[mise hook_env.rs](https://github.com/jdx/mise/blob/main/src/hook_env.rs) ·
[conda#9597](https://github.com/conda/conda/issues/9597) ·
[conda#9578](https://github.com/conda/conda/issues/9578) ·
[MS Learn CreateProcess](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessa) ·
[ARG_MAX survey](https://www.in-ulm.de/~mascheck/various/argmax/)
