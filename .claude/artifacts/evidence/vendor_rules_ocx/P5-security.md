# P5 security audit: the native `.direct` launcher

Scope: `git diff 36e5cc57..HEAD -- crates/ocx_shim` (root lookup, sidecar read and decode, Unix `exec`, Windows
`CreateProcessW` and batch spawn), the decoder it calls (`crates/ocx_env_apply/src/direct.rs`,
`DirectSidecar::validate`) and the writer (`crates/ocx_package_manager/src/launcher/direct.rs`). Method: the
security-auditor procedure (surface, STRIDE per entry point, data trace) against ADR § NFRs Security and design
§ 4.2 and C-012…C-020. Tiers per `quality-core.md`.

Threat model used: the launcher is never setuid (C-008 clears the bit), so it runs with its caller's authority.
An attacker of interest is a party who can write somewhere the launcher *reads* but cannot write the vendored tree
itself, or who can shape a sidecar's content through a package (file names, env declarations).

## Summary

| Tier | Count | Fixed here | Reported (outside `crates/ocx_shim`) |
|---|---|---|---|
| Block | 2 | 1 | 1 |
| Warn | 2 | 1 | 1 |
| Suggest | 6 | 0 | 0 |

## Block

**B-1 (fixed). NUL inside a sidecar argument or environment value.** `crates/ocx_shim/src/direct.rs`,
`decode_sidecar`; consumer `environment_block` (Windows). CWE-158. The decoder refused NUL in keys only. The
Windows environment block is written as raw UTF-16, so a NUL inside a value ends that entry early and the rest
becomes a variable of its own: it can re-add a key the sidecar lists under `unset`, or a reserved `__OCX_*` key,
past the decoder's reserved-key rule. In the `CreateProcessW` command line a NUL truncates the arguments. Fix: the
launcher refuses a NUL in any baked argument, env value or separator with 78. Regression test
`direct::tests::a_nul_in_an_env_value_or_baked_argument_exits_78`; red before the fix:
`called Result::unwrap_err() on an Ok value`; green after.

**B-2 (reported, not edited). Panic from a multi-byte device-like path segment.**
`crates/ocx_env_apply/src/direct.rs:145`, `is_device_name`. CWE-135. `stem[..3]` slices a 4-byte stem by byte
index; a segment such as `CO` followed by a two-byte character panics with
`end byte index 3 is not a char boundary`. Reachable from untrusted input whenever `validate(true)` runs: in the
launcher it aborts (shim profile `panic = "abort"`) instead of exiting 78; in the writer it would abort `ocx vendor`
on a package whose file name has that shape. Confirmed with a probe test run through `decode_sidecar` (removed, it
sits outside this pipeline). Fix belongs in the decoder: compare bytes
(`stem.as_bytes()[..3].eq_ignore_ascii_case(b"COM")`) or slice with `get(..3)`.

## Warn

**W-1 (fixed). `%` in a batch target's path.** `crates/ocx_shim/src/direct.rs`, `run_windows` → `spawn_batch`.
CWE-78. std's `make_bat_command_line` (toolchain 1.95.0) escapes arguments but inserts the script path raw inside
quotes; `cmd.exe` expands `%NAME%` there, so a root or package file name holding `%…%` can make `cmd.exe` run a
different script, chosen by the child environment. Fix: a batch target whose path holds `%` is refused with 78
(`batch_program_refusal`). Regression test
`direct::tests::a_batch_target_whose_path_holds_a_percent_sign_is_refused`; red before the fix: `refused` (no
refusal returned); green after. Parity note: `ocx exec` reaches batch targets through the same std path and has
the same gap; whoever owns `ocx exec` should decide whether it refuses too.

**W-2 (reported). Reserved `__OCX_` prefix compared case-sensitively on Windows.**
`crates/ocx_env_apply/src/direct.rs:93`, `DirectSidecar::validate`. CWE-178. `OCX_BINARY_PIN` and the `unset`
collision fold ASCII case under `platform_is_windows`, but `key.starts_with("__OCX_")` does not, so an env record
keyed `__ocx_env_state` passes and, on Windows, sets the same variable as `__OCX_ENV_STATE`. Same file, related:
`validate` does not refuse NUL in values or arguments (C-010 places that rule on the writer); B-1 closes it in the
launcher, the shared rule set still lacks it.

## Suggest

- **S-1.** `crates/ocx_shim/src/direct.rs`, `build_env`: the Windows fold is ASCII-only, while Windows and std's
  `EnvKey` compare names with the OS upper-case table. An inherited variable spelled with a non-ASCII letter the OS
  folds onto a listed key would survive `unset`. Needs the caller to spell its own variable that way. CWE-178.
- **S-2.** `read_regular_bounded`: the open precedes the regular-file check, so a device node behind a symlink at
  the sidecar path (symlinks must be followed for runfiles) is opened, with any open side effect; `O_NOCTTY` is not
  set. Needs write access to the launcher's own directory. CWE-367.
- **S-3.** `accepts`: an unreadable candidate sidecar (not regular, permission denied) aborts the whole lookup
  instead of rejecting that one candidate, so a planted entry sorting first in a runfiles tree denies service.
  Fails closed. CWE-400.
- **S-4.** `RealFs::list_dir`: every entry is read before the sort and the 4096 cap, so time and memory grow with
  the directory. CWE-400.
- **S-5.** `spawn_batch`: std spawns with `bInheritHandles` and no handle list, so a batch child inherits every
  inheritable handle, where the `CreateProcessW` path passes only the three std handles; the spawn-to-job window is
  documented at the call. Same as `ocx exec`.
- **S-6.** `started_path_unix`: the `PATH` search tests mode bits `0o111`, not executability for this user; a
  mismatch with the shell's pick exits 78 (fails closed) instead of running.

## Accepted by design (no finding)

- Root binding is by byte identity of `<C>/<self>.direct`, not by content of the tree: a party able to write into
  `<P>.runfiles`, into `$RUNFILES_DIR` while `P` is under it, or into an ancestor of `P` can supply a tree beside a
  copied sidecar. That access already equals write access to the launcher's own directory; no boundary is crossed.
- The started-path check (`stat(P)` against the running image) can race a directory swap between check, sidecar
  read and `exec`; the swap needs write access to a directory on `P`, the same trust as above.
- `argv[0]`, `PATH` and `RUNFILES_DIR` (including a lexical `..` that satisfies the under-`$RUNFILES_DIR` prefix
  test) are the caller's own; the caller already controls the child.

## Checked, no finding

- Unix: `program` is absolute after expansion (no `PATH` search); `env_clear` plus the built list; duplicate
  inherited keys collapse before `unset`, so every copy leaves; sidecar handles are close-on-exec and dropped
  before `exec`; nothing is written (C-018).
- Windows `CreateProcessW`: explicit application name (CWE-428), arguments quoted per `CommandLineToArgvW`, job via
  `PROC_THREAD_ATTRIBUTE_JOB_LIST`, std-handle whitelist, exit-code passthrough.
- Windows batch: std escapes arguments and refuses `\r`, `\n` and unquotable ones (`InvalidInput` → 64), decides
  batch-ness after `GetFullPathNameW` (trailing dots and spaces seen), runs `cmd.exe` from the system directory;
  the decoder refuses Windows segments ending in `.` or a space, holding `:` (alternate streams) or `\`, and
  device names.
- Reads: `O_NONBLOCK` open, `fstat` on the handle, at most 1 MiB with a one-byte overrun check (V-60 FIFO test).
- Environment: an `env` key equal to an `unset` key (folded on Windows), `OCX_BINARY_PIN` or `__OCX_*` is refused
  by the decoder (Windows prefix case: W-2); the sidecar carries keys to remove, no secret values.
- Exit codes: refusals 78, batch argument 64, exec failure 74 or 77, child status passed through.
- Panics: launcher-side slicing goes through `get`; the one reachable panic is B-2 in the decoder.
- Messages: stderr names paths and keys, never environment values; `serde_json` errors carry a position only.

## Not audited

The writer `write_direct_launcher` is an `unimplemented!` stub in this pipeline (its body lands in P6), so the
writer-side guarantees (C-009 staged publish, C-010 value refusals, byte copy, no secret values) are audited only
as contracts here. No GitHub issue was filed from this step; B-2 and W-2 are for the owner of `ocx_env_apply`.

## Checks after the fixes

- `cargo test -p ocx_shim --locked`: 152 + 9 passed, 1 ignored (the timing row, ignored before this step).
- `cargo clippy -p ocx_shim --all-targets --locked -- -D warnings`: no issues.
- `cargo check -p ocx_shim --all-targets --locked --target x86_64-pc-windows-gnullvm`: finished, no warnings.

Fixed in 5fc5f62f: B-2 (device-name check is byte-wise, no panic on multi-byte segments); W-2 (`__OCX_` prefix folds ASCII case on Windows, exact on Unix); NUL byte in env value, args, program or self refused by `validate` (`SidecarError::NulByte`, 65 writer / 78 decoder).
