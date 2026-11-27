# FX6 — product name in the clean help, suite census pin

## 1. website:test product name
- Commit 1afe21e1. Red: `reference/command-line/commands.md:83: It also removes the launcher directories other ocx builds wrote`.
- Source was the long help in `crates/ocx_cli/src/command.rs` (not the clean.rs doc comment alone); `clean.rs` line 10 had the same spelling. Both fixed. Other branch help strings: no further lowercase prose `ocx` (grep over the branch diff).
- Regenerated with `cargo run -p ocx_schema -- docs --out website/src/content/docs`: only `commands.md` changed; goldens carry no such text.
- Green: `node --test website/tests/product-name.test.mjs` 2 pass 0 fail; `ocx_schema docs --check` exit 0; `cargo test -p ocx_schema` all ok. `ocx_sdkgen` golden unchanged, not run. Whole `task website:test` not run: `@ocx-sh/theme` missing (no node_modules), `glossary.test.mjs` fails to import; the other 12 test files pass.

## 2. suite census
Commit 20448293. Red: `{'skipped': (147, 155)}`. Green: `suite census: ... skipped 155 ... as pinned`. tests/parametrized floors left (live 4036 / 287 exceed pins).

| file | test | skip condition | verdict |
|---|---|---|---|
| test_entrypoints.py | 6 tests (bare-name start x2, previous-release install, concurrent first use, private dep, bare name via PATH) | win32 (Unix scripts / PATH lookup) | platform condition |
| test_entrypoints_crossplat.py | linux_install_writes_only_launcher_and_record, linux_every_launcher..._same_bytes | not linux | platform condition |
| test_entrypoints_crossplat.py | windows_native_launcher_exists_no_script_no_cmd | not win32 | platform condition (renamed; 2 old skips removed) |
| test_exec_env_compat.py | old_toolchain_trampoline_with_exec_sidecar | not win32 (.exec trampolines are Windows launchers) | platform condition |
| test_lazy_loading.py | deferred_launcher_builds_the_frozen_argv | win32 (stub ocx is /bin/sh) | host condition |
| test_trampoline_exec.py, test_windows_shim.py | reason reworded / renamed, same skip | unchanged | not an addition |

Count: 11 added, 4 removed/renamed counterparts: platform or host condition 11, silenced red 0.

## 3. ci:lint after suite-census
- dead-path-sweep: exit 0 (6 tolerated by name).
- clippy:check, doc:ratchet: Bazel actions, not run (would need a Bazel build root; no change touches Rust code beyond a string literal and a doc comment).
