# LF — launcher fixes

Scope: `crates/ocx_shim/**`, `crates/ocx_env_apply/**`. Windows code was not run (CI only); `cargo check` and `cargo clippy -D warnings` for `x86_64-pc-windows-msvc` pass.

## 1. Windows past-MAX_PATH
- Root cause: `GetMappedFileNameW` with a buffer shorter than the name returns 0 and sets `ERROR_MORE_DATA` (234) or `ERROR_INSUFFICIENT_BUFFER` (122); `mapped_image_path` treated 0 as fatal and grew only on a full buffer. Red (CI at an older head): `GetMappedFileNameW failed (error 234)` in the two `starts_..._past_max_path` tests; the batch-target test failed at its stderr assertion for the same reason (the launcher died before it reached the batch target).
- Commit c987269e: one `fill_wide` helper grows on the zero return with 122/234, on a return equal to the capacity, and on a return above it (`GetFinalPathNameByHandleW` required size), up to 64K units. Both the mapped-name and final-path calls use it. `GetModuleFileNameW` in `main.rs` already handled both of its forms and is unchanged. No `QueryDosDeviceW` or environment read with a caller buffer exists in the two crates.
- Green: unit tests `a_wide_name_of_any_length_up_to_the_nt_limit_fills_in_every_short_buffer_form` (1..32767 units, four API forms) and `a_wide_fill_fails_on_another_error_and_stops_at_its_limit`; `cargo test -p ocx_shim` 161 passed + 18 passed, 1 ignored. The three Windows tests: not run, CI only.
- Relative pin test: the launcher (rung 1 of the record, `adr_unified_launcher.md` § `ocx` resolution, V-L12) refuses a relative pin with 78 and "must be an absolute path"; the test expected a spawn-time length-limit clause and was stale. Commit fd1173ad aligns the test (not run, CI only). Whether the four pass on `main` was not checked (`gh` read-only run list not queried).

## 2. No `ocx` on PATH
Launcher exits 78 `no ocx found on PATH`, which is the record's text and code (rung 3). `test_windows_shim.py` lines 363 and 493 already expect 78; no test at this head expects 69 or "ocx not found". No change.

## 3. Control characters in diagnostic lines
Sink `main()` already escaped per variant via `escape_controls`. Unescaped paths found: `ContainmentViolation.path`, `DirectExecFailed.reason`, and the direct `eprintln!` of the canonicalize notice (record-supplied package root). Commit b077f521 escapes all three; test `a_containment_path_and_a_canonicalization_notice_reach_stderr_with_control_characters_escaped`. `escape_controls` escapes `char::is_control` (C0, DEL, C1) and has no bidi/format-control handling; that was not added.

## 4. `is_record_name`
Not in the two crates; it lives in `ocx_package_manager/src/tasks/toolchain_names.rs`, carries no `expect(dead_code)` and has callers. No change.

## 5. Sweep
TODO/FIXME/todo!/unimplemented!: none. One `shortcut:` (non-Unicode argument, direct.rs) is a recorded limit. `entrypoints` occurs only in `derive_stem` test path fixtures. No `.direct`/`shims/` names. `allow`/`expect` attributes: all still apply (clippy `-D warnings` clean).

## 6. Gates
`cargo test -p ocx_shim --locked`, `cargo test -p ocx_env_apply --locked`, `cargo clippy -p ocx_shim -p ocx_env_apply --all-targets --locked -- -D warnings`, `cargo fmt --check`: green. `scripts/shim_inputs.py --print` before and after: 29a64192…c192, unchanged (the digest excludes shim source). Launcher sources changed: `crates/ocx_shim/src/direct.rs`, `crates/ocx_shim/src/main.rs`.

## Round 2 (review fixes)
1. `fill_wide` also grows when `written + 1 >= capacity` (a name cut to `capacity - 1` plus NUL). Red: new fake form `truncate-minus-one` failed `a_wide_name_of_any_length_up_to_the_nt_limit_fills_in_every_short_buffer_form` with `assertion left == right failed: truncate-minus-one 512` (511 of 600 units returned). Green: same test plus the cap test (`TooLong` at 1024) pass; exact fit and 32767 units still return the full name.
2. The `a_nul_in_an_env_value_exits_78` doc line is back on its test; `fake_call` documents its five forms.
3. `length_limit_hint`: every program reaching it is absolute (direct records expand `@VENDOR_DIR@` against the absolute image directory; a home launch refuses a relative pin or baked `ocx` and skips relative PATH entries), and `spawn_child` prefixes an over-long absolute `.exe` with `\\?\`. No input reaches the "relative program path" branch (it also misfired for an already verbatim path); branch and its test lines deleted. The batch clause stays and is tested.
4. `sanitize_for_terminal` in `ocx_cli` strips `is_control`, bidi controls (U+061C, U+200E/F, U+202A-E, U+2066-9) and zero-width (U+200B-D, U+FEFF); it does not treat U+2028/2029. The launcher now escapes those classes by code point. Red: `a_diagnostic_escapes_bidi_controls_and_zero_width_characters` showed the raw characters; green after the change.
Gates: ocx_shim 162 + 18 passed (1 ignored), ocx_env_apply 78 + 1 passed, clippy clean on host and `x86_64-pc-windows-msvc`, `cargo check --tests` for msvc, fmt clean. Launcher inputs changed: `crates/ocx_shim/src/direct.rs`, `crates/ocx_shim/src/main.rs`.
