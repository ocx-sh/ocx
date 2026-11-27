# FX7 — three reds of verify-basic run 45965981873 (head d9009ff6)

## 1 rust:format:check
- Red: `cargo fmt --check` diff at `crates/ocx_package_manager/src/tasks/vendor.rs:319` (match arm in `offered_platforms`).
- Fix: `cargo fmt` (workspace); only that file changed. Commit 29e493e8.
- Green: `cargo fmt --check` exits 0.

## 2 website:test glossary
- Red: `explanation/toolchain-activation.mdx:117: "toolchain tier" is retired, use "toolchain"`; `reference/glossary.md is stale`.
- Fix: line 117 now says "which toolchain it serves, project or global, with its project root" (commit 6f6c7da5);
  `node scripts/glossary.mjs` regenerated `reference/glossary.md` (3 launcher terms, commit ad554d18; a generated page, committed because the CI red names it).
- Green: `pnpm test` in `website/` (frozen-lockfile install, store in scratch): 37 tests, 37 pass, 0 fail. Covers plugins/scripts tests and
  tests/glossary, product-name, shell-session; the retired-form scan reads every hand-written page.

## 3 test:accept:floor
- Red, reproduced: `accept_floor.py --junit <run 45965981873 shards> --expect-shards 4 --limits-dir test` -> `282 skipped exceeds test/SKIP_CEILING (279)`.
- Baseline: main run 45806079252 (head 03adc12b, newest green verify-basic on main): 274 skips, 4535 cases; branch 4667 cases, 282 skips.
- Diff: 26 skips added, 18 gone. Every one is gated on `sys.platform` (module `skipif` or `windows_only`/`darwin_only`); none silences a red.
  | group | count | verdict |
  | test_windows_shim `test_shim_*` -> `test_launcher_*` (14 gone, 14 added) | 14 | renamed |
  | test_trampoline_exec v9/v20 (2 gone, 2 added) | 2 | renamed |
  | test_entrypoints_crossplat native shim -> native launcher | 1 | renamed |
  | test_vendor_launcher Mach-O signature x3 (macOS only) | 3 | platform condition, part of the work (already in ceiling 279 via a5328e24) |
  | test_vendor_launcher cmd/env case folding x2 (Windows only) | 2 | platform condition, part of the work (same) |
  | test_windows_shim never_starts_a_planted_ocx_exe, refuses_a_batch_file_pin_with_78 | 2 | platform condition, new Windows-only cases |
  | test_exec_env_compat `.exec` sidecar via this ocx | 1 | platform condition, new Windows-only case |
  Net vs ceiling 279: +3 (the last two rows) = 282.
- Fix: `test/SKIP_CEILING` 279 -> 282, body of commit c2a5d514 names run, count and cases (a5328e24 convention).
- Green: same command, same input: `4 report(s); 4667 cases (floor 4600), 282 skipped (ceiling 282), 3 xfailed (ceiling 5)`, rc 0. Red was shown first with 279.
- `test/SUITE_FLOOR` 4600 (4667 measured, untouched); `test/XFAIL_CEILING` 5 (3 measured, within slack 2, untouched).
