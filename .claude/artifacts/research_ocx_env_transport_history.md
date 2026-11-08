# Research: history of OCX's transported environment state

## Metadata

**Date:** 2026-11-06
**Domain:** cli
**Triggered by:** discussion `.agents/discussions/exec-env-model.md` (repo archaeology lane)
**Expires:** 2027-05-06

Grades: [O] observed (sha, file, issue, retro entry), [I] inferred, [A] assumed.

## Direct Answer

Every addition to the forwarding set came from one failure shape: `--clean` or a launcher re-entry lost a resolution-affecting setting. The selector clears (451c54a8, ed5f17f8) and the exec-scope marker (4b8b94d2) came from the opposite shape: state crossing into a child that should not have it. No design discussion exists for `--stack`, bare `ocx shell` or `__OCX_EXEC_SCOPE`: #668, #672 and #649 have zero comments.

| Date | sha / issue | Mechanism | Reason (commit body) | Test |
|---|---|---|---|---|
| 2026-05-02 | 795920cb | `OCX_BINARY_PIN`, `apply_ocx_config` seed | pin child ocx to the running binary; launcher `${OCX_BINARY_PIN:-ocx}` | unit |
| 2026-05-18 | 45f2d0fa | Windows shim honours the pin, defined-but-empty counts | parity with `.cmd` `IF DEFINED` | ADR U9/E5 |
| 2026-07-02 | e78e1948 | `OCX_PATCHES` | the patch overlay must reach child ocx | — |
| 2026-07-26 | 7d20b40c, 9bfd0e8f | `OCX_ENV`, `apply_child_env` | `launcher exec` has no ProjectConfig; `--env` overrides were lost at re-entry | — |
| 2026-08-27 | 9309125f | `__OCX_ENV_STATE` ledger, `__OCX_ENV_PWD` | exec/run/package test "no longer inherit the environment this reconciler applied" (released isolation) | ADR Decision 1, ledger tests |
| 2026-09-05/07 | bdf1cc7f | `OCX_TOOLCHAIN_DIR`; trampoline `unset OCX_GLOBAL OCX_PROJECT`; shim strip | the baked selector must be the only selector | ADR item 38 |
| 2026-09-21 | e4ce1186 (#488) | `OCX_HOME` set-always, absolutized | `--clean` lost `HOME`; launcher re-entry exited 78 | unit + acceptance |
| 2026-09-21 | 191b9324 | `OCX_NO_PROJECT`, `OCX_DEFAULT_REGISTRY`, `OCX_INSECURE_REGISTRIES`, `OCX_NO_CONFIG_REFRESH` | a `--clean` child re-adopted the project the parent pruned | unit + `test_exec_forwarding.py` |
| 2026-10-01 | 6bd0d727 (#544) | `OCX_TOOLCHAIN_DIR` into the view | env-only root stripped from every child | acceptance |
| 2026-10-19 | a963f220 (#623) | `__OCX_CREDENTIAL_HELPER` | trampoline helper deadlocked on the outer pull's staging lock | — |
| 2026-10-25 | a4c8ef1c | `OCX_LAUNCH_IDENTITIES` → `__OCX_LAUNCH_IDENTITIES` | written only by ocx | goldens |
| 2026-10-25 | 451c54a8 (#649), ed5f17f8 | `OCX_GLOBAL`, `OCX_PROJECT` always cleared | nested ocx saw two selectors, exit 64 | unit + acceptance item 38 |
| 2026-11-01 | 4b8b94d2 (#668) | `__OCX_EXEC_SCOPE` | `ocx exec -g … -- bash` rc re-applied global and default sets | unit + `test_shell_reconcile.py` |
| 2026-11-01 | 3f85e2e0 (#668) | `--stack` | #668 "ocx exec merge other (or parent) projects into env, this includes an opt out of the isolation feature" | acceptance |
| 2026-11-01 | 8d1bc0f1 (#668) | bare `ocx shell` | #668 "ocx shell command to launch a shell in the env" | acceptance |
| 2026-11-03 | 833e8cb5 | `--clean --stack`; hook stays registered in exec/shell child | `--clean` silently discarded `--stack` | — |

## Key Findings

1. **#649's reporter and the fix.** The reporter, an agent running acceptance tests under `ocx exec zot --`, expected the trampoline not to propagate the selector. The fix scrubbed in `apply_ocx_config` instead, covering every spawn site. [O #649 body]
2. **Owner question already on record.** ocx ledger `project-zot-hint-global-trampoline`, report 2026-10-24 P-10: "should a global-toolchain trampoline forward `OCX_GLOBAL=1` into the launched tool's own environment at all." [O]
3. **grimoire friction is a different leak from #649.** `OCX_GLOBAL` was exported into the outer shell, and the caller's own `ocx` read it. 451c54a8 does not touch this. [I] Four consumed entries plus unfolded ones, including:
   - `ocx exec -- task shell:verify` resolved the global toolchain, printed "not installed - skipping" for shellcheck/shfmt and exited 0. That is a silent false green. [O grimoire inbox 20261105T203807Z-0b8d85be]
4. **#668 framed isolation as a feature to opt out of.** "includes an opt out of the isolation feature" [O]. Exec dropping the hook's contribution shipped in 9309125f (2026-08). The ADR amendment of 2026-11-03 (`adr_shell_env_overhaul.md:732-737`) says exec composes hermetically without `--stack`.
5. **ADR history.**
   - `adr_shell_env_overhaul.md` Decision 1 (line 102) originally kept `__OCX_ENV_STATE` in exec children; the 2026-11-03 amendment strips it. 9309125f's note already disagreed. [I]
   - `adr_live_env_reload.md` OD-4 proposed renaming `OCX_ENV` → `__OCX_ENV`, with a mixed-version caveat tied to `OCX_BINARY_PIN`. The overhaul ADR dropped it.
   - `adr_exec_resolution_record.md` F10 counts "13+ internal channels" forwarded by `apply_ocx_config`.
   - No ADR declares the `OCX_GLOBAL`/`OCX_PROJECT` clearing; the rationale lives in commit bodies only.

## negative

- 0 comments on #668, #672, #649.
- ocx retro inbox empty (ledger only).
- No ADR for `__OCX_CREDENTIAL_HELPER`.
- No test that `ocx package exec` leaves `__OCX_EXEC_SCOPE` unset.
- `--clean` original semantics not read.

## leads

- Read `adr_toolchain_activation.md` D-9 and amendments A-2/A-5/20 in full.
- The outer-shell `OCX_GLOBAL` leak has no ocx-side issue yet.
- Mutation-check the item-38 tests (451c54a8 says "written but run by the orchestrator").

## Recommendation

None. This is a findings-only lane.

## Sources

| Source | Type | Date | Relevance |
|---|---|---|---|
| git log on main (shas above) | Repo | 2026-11-06 | Introductions and reasons |
| ocx-sh/ocx #488 #544 #623 #649 #668 #672 | Issues | 2026 | Problem statements |
| `.agents/retro/` (ocx), `/home/mherwig/dev/grimoire-rs/grimoire/.agents/retro/` | Retro | 2026-10/11 | Agent friction |
| `.claude/artifacts/adr_shell_env_overhaul.md`, `adr_toolchain_activation.md`, `adr_live_env_reload.md`, `adr_exec_resolution_record.md`, `adr_windows_exe_shim.md` | ADRs | 2026 | Written rules |
