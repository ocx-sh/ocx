# R1T — Windows and macOS acceptance fixes (deep run at 04152fdc)

Linux run, same module set, same cargo-built `ocx` (`-p ocx --features __testing`), before and after:
`test_vendor`, `test_package_vendor`, `test_vendor_parity`, `test_vendor_launcher`, `test_toolchain_render`,
`test_toolchain_activate`, `test_entrypoints`, `test_trampoline_exec`, `test_windows_shim`:
242 passed, 30 skipped before; 242 passed, 30 skipped after. Skip delta 0 on every platform.
`bench/shell_latency.py --self-check` green. T0 `lint/` tier: 807 passed; the one red, the code-docs ratchet,
names only `crates/ocx_env/src/vars.rs` (not this pipeline's file).

| Group | Cause | Change | Proof |
|---|---|---|---|
| G1 | confirmed by reading: `HOST` has no leaf on Windows/macOS | `-p linux/amd64` for the two available-platforms tests; `foreign_platform_package` gives a linux/amd64 leaf on a windows/amd64 host (was `pytest.fail`) | Linux green; the Windows-host branch run on Linux by patching `current_platform` (publish, vendor `-p`, manifest) green; macOS/Windows blind |
| G2 | confirmed: `_publish` rewrites `tool` to `tool.exe`, a wasm claim is `tool` | `make_package` with literal bins in both wasm tests | Linux green; blind |
| G3 | confirmed: `_tool` ships no `.exe`, so no `l/` | control plant creates `l/` first | blind |
| G4 | reasoned: a junction is no symlink to `is_symlink` | `os.path.isjunction` joins the link branch; Windows-only `PermissionError` on a read records `<unreadable>` | blind; the fallback covers a dangling reparse point if the junction reading is wrong |
| G5 | confirmed by log: concurrent `rustc` proxy toolchain installs | `build_recorder()` builds once per machine under `tempdir/ocx-recorder-<src hash>/`, exclusive-create lock (stale after 1200 s, wait 1500 s), private staged name, one `os.replace` | 6 concurrent Linux processes: one compile, one path, no leftovers. Red (the install collision) needs rustup on Windows |
| G6a | confirmed: JSON body doubles backslashes | `_as_json_text` (`json.dumps(...)[1:-1]`) at both asserts | blind; function checked on a Windows-spelled path |
| G6b | confirmed: report spells the store path natively | `_store_to_token` matches both separators | run on Linux with Windows-spelled and POSIX input: both give `@VENDOR_DIR@/p/<h>/c/bin` |
| G6c | confirmed: Windows launcher is `hello.exe` | name chosen by platform | Linux green; blind |
| G8 | confirmed from the log: the older prompt writes native `C:\` entries joined by `:` | `older_release=True` skips the POSIX-form check for the two older-prompt reads; the new binary keeps it | blind. A Windows run must print the raw PATH of step 3 if `_real(shell_bin(home))` is then missing |
| G9 | confirmed by code: `make_package_with_entrypoints` writes `bin/probe.bat` on Windows, PATHEXT order takes `.bat` before `.cmd` (`vendor.rs::resolve_program`), so `probe.cmd` never ran | the probe overwrites `bin/probe.bat`; assertion message now carries stdout | blind. The sibling `gamma` hostile-argument test has the same shape and passes without ever running its `.cmd` (weak, not red) |
| G10 | hypothesis consistent with the log (extra key only on the `ocx exec` side) | `__CF_USER_TEXT_ENCODING` joins `_SHELL_NOISE` | blind |
| shell-latency | confirmed: Linux red reproduced before (`RuntimeError ... is not a rendered trampoline`) | `count_trampolines` reads `NAME.launch` kind `toolchain` beside a launcher | Linux live run: gate "renders trampolines" 8 vs floor 2 PASS, past the former raise (timing gates red on a debug build, expected) |

G7 untouched (R1G). Outside the file set: the Windows `SKIP_CEILING` (1873 vs 1925 measured) and macOS (535 vs 542)
are the owner's hub files; these changes add and remove no skip.
