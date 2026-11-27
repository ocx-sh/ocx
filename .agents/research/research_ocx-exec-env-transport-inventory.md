# Research: OCX process-to-process state inventory

## Metadata

**Date:** 2026-11-06
**Domain:** cli
**Triggered by:** discussion `.agents/discussions/exec-env-model.md` (codebase recon lane); leaked `OCX_GLOBAL` behind grimoire#297 and ocx#678
**Expires:** 2027-02-06 (re-verify against main; `--stack`, bare `ocx shell` and `__OCX_EXEC_SCOPE` are unreleased and moving)

Grades: [O] observed at file:line, [I] inferred, [A] assumed. Read on branch `uno` = `main` at 833e8cb5. No scenario was executed; nesting traces are [I].

## Direct Answer

| Variable | Declared (crates/ocx_env/src/vars.rs) | Writers | Readers outside ocx Rust | Declaration matches code? |
|---|---|---|---|---|
| `OCX_BINARY_PIN` | Public, Forward (:18) | `apply_ocx_config` (crates/ocx_config/src/env.rs:211); SDK spawner | sh launcher/trampoline `${OCX_BINARY_PIN:-…}` (crates/ocx_package_manager/src/launcher/body.rs:20,35,96-98); Windows shim raw `var_os` (crates/ocx_shim/src/main.rs:252) | Partly — doc says "set on every subprocess", only `apply_ocx_config` sets it [I] |
| `OCX_HOME` | Public, Forward (:26) | `apply_ocx_config`, absolutized | Windows shim (main.rs:217) | Yes [O] |
| `OCX_GLOBAL`, `OCX_PROJECT` | Public, Forward (:118, :82) | always cleared by `apply_ocx_config` (env.rs:223-231); `unset` in trampoline (body.rs:96); deleted by shim for `.exec` sidecars (main.rs:238-248) | sh trampoline, Windows shim | No — "Forward" means force-cleared [O] |
| `OCX_ENV` | Public, Forward (:186) | `apply_child_env` → `set_forwarded_env` (crates/ocx_package/src/metadata/env/apply.rs:76); always cleared first (env.rs:263-264) | none | Yes |
| `OCX_PATCHES`, `OCX_TOOLCHAIN_DIR`, `OCX_RECORDS_*`, `OCX_MIRRORS`, `OCX_PATCH_SNAPSHOT`, `OCX_MANAGED_CONFIG`, `OCX_NO_VERIFY`, `OCX_NO_CONFIG`, `OCX_NO_CONSENT` | Forward | `apply_ocx_config`, set-or-remove from the parsed view | none | Yes (`forward!` asserts `child = Forward`; env.rs:883 pins the set). Stale comment env.rs:34-35 ("every producer writes None") — context.rs:450 sets it |
| `OCX_ALLOW_YANKED`, `OCX_NO_PROJECT`, `OCX_NO_CONFIG_REFRESH`, `OCX_NO_UPDATE_CHECK`, `OCX_DEFAULT_REGISTRY`, `OCX_INSECURE_REGISTRIES` | Forward, ambient-only | copied from ambient by `apply_ocx_config` | none | Yes |
| credentials (`OCX_IDENTITY_TOKEN`, `OCX_KEY_PASSWORD`, `OCX_SIGNING_KEY`, `OCX_ANNOUNCE_GIT_TOKEN`) | secret, Scrub | stripped (env.rs:208-210; plugin_dispatch.rs:114-117) | none | Yes |
| `__OCX_ENV_STATE` (hook ledger) | Plumbing, Inherit (:492) | hook export (crates/ocx_cli/src/command/self_group/activate.rs:441, 743-745) | generated hook `${__OCX_ENV_STATE-}` (crates/ocx_shell hook.rs:286-293) | No — dropped by `inherited_env`/`stacked_env`/`Env::clean` (reconcile.rs:104-133); kept by plugin dispatch and `launcher exec` |
| `__OCX_ENV_PWD` | Plumbing, Inherit (:497) | generated Elvish hook only (hook.rs:537, 930-988) | Elvish | No Rust reader or writer at all [O] |
| `__OCX_EXEC_SCOPE` | Plumbing, Inherit (:512) | `ocx exec` sets `ctx.config_path.parent()` (crates/ocx_cli/src/command/toolchain_exec.rs:240-243) | none | Declared Inherit but explicitly written; `ocx package exec` neither writes nor clears it |
| `__OCX_LAUNCH_IDENTITIES` | Plumbing, Inherit (:506) | `apply_child_env` under a patches tier (apply.rs:78-85); emitted into shell code by `ocx env`, `direnv export` | none | Mostly |
| `__OCX_CREDENTIAL_HELPER` | Plumbing, Inherit (:518) | `docker_credential::set_helper_env`, a process global outside `ocx_env::Env` (context.rs:113-116) | none | Written outside the registry; `Env::clean()` drops it |

## Key Findings

1. **Scrub sites, each patched for its own leak** [O]: credential strip (env.rs:208-210, plugin_dispatch.rs:114-117); trampoline `unset OCX_GLOBAL OCX_PROJECT` (body.rs:96, rationale :64-65); shim `.exec` delete (main.rs:238-248); `OCX_TOOLCHAIN_DIR` remove arm (env.rs:237); `OCX_ENV` clear-then-rewrite (env.rs:263-264); carrier dropped in three of four exec start modes (reconcile.rs:104-133); generated SDK drops every `OCX_*`/`__OCX_*` then `env_clear()` (ocx_sdkgen templates spawn.rs:46-66).
2. **Nesting `ocx --project a exec -- ocx --project b exec -- cmd`** [I from toolchain_exec.rs:216-243, reconcile.rs:104-180, env.rs:207-294]:
   - default: inner inherits a's merged env; a's entries stay as plain ambient values (no ledger records them); b's are prepended. Effectively layered. `OCX_ENV` replaced by b's; `__OCX_EXEC_SCOPE` becomes b; `__OCX_LAUNCH_IDENTITIES` overwritten only under a patches tier, else a's persists.
   - inner `--clean`: a's contributions dropped. Outer `--clean`: command resolution uses the composed PATH (toolchain_exec.rs:257-259), so `ocx` must be a composed tool or absolute.
   - `--stack`: no observable difference in an a→b exec chain. `stacked_env` acts on a hook ledger carrier, which the outer exec already removed. `--stack` differs only when the parent is a hook-applied shell.
3. **Global trampoline → long-lived process → child shells** (`ocx --global exec -- herdr`):
   - Steps 1–2 [O]: the trampoline unsets the selectors; exec sets `__OCX_EXEC_SCOPE`.
   - Step 3 [A]: the value is `$OCX_HOME` for a global exec. Unverified.
   - Login stream in panes [O activate.rs:153, 170-172, 728-750]: skips PATH and global env, still registers the hook.
   - First prompt [I]: the ledger is empty, so `--reconcile` runs. The yield branch (activate.rs:204-217) fires only when the walked project equals the marker dir. A walk never yields `$OCX_HOME` (vars.rs:24-25), so reconcile proceeds and applies global entries.
   - Net: panes get the global env at the first prompt; only the login-time application is skipped.
4. **Non-ocx consumers** [O]:
   - sh launcher and trampoline: `${OCX_BINARY_PIN:-…}`, `unset OCX_GLOBAL OCX_PROJECT`.
   - Windows shim: raw `OCX_HOME` and `OCX_BINARY_PIN`. Size-budget reason for skipping `ocx_env`. A defined-but-empty pin counts as a pin (adr_windows_exe_shim.md:362).
   - Generated hooks read `__OCX_ENV_STATE`, `__OCX_ENV_PWD`, `DIRENV_DIR`, `MISE_SHELL`, `__MISE_ORIG_PATH`, `OCX_CONSENT_PATHS`, `OCX_CONSENT_NAMESPACES`.
   - Hook bodies must not read `OCX_BINARY_PIN` (activate.rs:1663-1672).
5. **Written contracts contradict each other** [O]:
   - ADR says exec is hermetic: `.claude/artifacts/adr_shell_env_overhaul.md:732-737` ("Without `--stack`, exec composes hermetically and replaces the parent's tools").
   - Docs say exec inherits: `website/src/content/docs/explanation/environments.mdx:197` ("By default `ocx exec` inherits… not hermetic; pass `--clean`").
   - `reference/env-composition.md:75-78` says a nested project replaces the enclosing one.
   - `explanation/shell-integration.mdx:74-82` says "Nested projects switch, they do not stack".
   - `reference/environment.md:826-835` says the `--reconcile` arm "leaves the environment alone" under the marker. Code gates that on the marker matching the project (Finding 3).
6. **Cross-version promises** [O]:
   - The hook ledger is the only versioned carrier. `ledger.rs` `LEDGER_VERSION=1`; "a shape break must ship a `v-1` revert-read arm in the same release, or live shells lose their revert set". A mismatched version decodes as absent (:225-241).
   - `patch.rs:183` calls `OCX_PATCHES` "the wire format a child ocx of another version reads".
   - `OCX_ENV` and `__OCX_LAUNCH_IDENTITIES` have no version and fail closed.
   - No statement that a launcher or trampoline baked by version N works under version M.
7. **Plugin dispatch scrubs more than documented** [I]: its view lacks mirrors, patches, toolchain_dir and records. So `apply_ocx_config` strips ambient `OCX_TOOLCHAIN_DIR`, `OCX_PATCHES`, `OCX_RECORDS_*` and others from plugins, against the module doc "inherit the parent env like cargo/git plugins, minus bearer credentials".

## negative

- `__OCX_EXEC_SCOPE`: one Rust writer (toolchain_exec.rs:240); `ocx package exec` has none.
- `__OCX_ENV_PWD`: no Rust reader or writer, 12 hits total.
- ADR grep `--stack|__OCX_EXEC_SCOPE|inherited_env|stacked_env`: 3 hits, all in `adr_shell_env_overhaul.md`.
- Not read: `crates/ocx_config/src/records.rs`, `tasks/update_check.rs`, Windows `.exec`/`.shim` sidecar grammars.

## leads

- Verify `ctx.config_path.parent()` for a global exec (decides whether the global-exec yield ever fires).
- Run the three nesting variants against a built binary and diff `env` at the leaf.
- Read `adr_toolchain_activation.md` and `adr_windows_exe_shim.md` in full for the trampoline and shim variable contracts.
- Check whether an undecodable `__OCX_ENV_STATE` surviving `inherited_env` (reconcile.rs:187-192) leaks into children.
- The `ocx_sdkgen` spawn and `OCX_BINARY_PIN` contract is a separate consumer.

## Recommendation

None. This is a findings-only lane.

## Sources

| Source | Type | Date | Relevance |
|---|---|---|---|
| crates/ocx_env/src/vars.rs | Repo | 2026-11-06 | Declarations |
| crates/ocx_config/src/env.rs | Repo | 2026-11-06 | `apply_ocx_config` |
| crates/ocx_package/src/metadata/env/apply.rs | Repo | 2026-11-06 | `apply_child_env` |
| crates/ocx_cli/src/command/toolchain_exec.rs, shell.rs, self_group/activate.rs | Repo | 2026-11-06 | exec, `ocx shell`, hook |
| crates/ocx_package_manager/src/launcher/body.rs, crates/ocx_shim | Repo | 2026-11-06 | non-Rust readers |
| .claude/artifacts/adr_shell_env_overhaul.md and website docs listed above | Docs | 2026-11-06 | Written contracts |
