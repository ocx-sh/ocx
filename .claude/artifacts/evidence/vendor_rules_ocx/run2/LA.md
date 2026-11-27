# LA evidence (run 2, the launcher program and the record decoder)

Base `333a5264`, branch `hex/bazel-rbe-rules-ocx--LA`. Each step was built with cargo in an LA-only target
directory, with 3 jobs and no debug info. `task verify:scoped` was not run at any step: `scripts/scoped_gate.py --plan`
routes a Bazel lane on this branch (rc-file rule, DP-39). Each step instead ran `cargo fmt`, `cargo clippy -p <crate>
--all-targets --locked -- -D warnings` and `cargo test -p <crate> --locked` by hand.
Clippy ran on the host and on four cross targets: `x86_64-pc-windows-gnullvm`, `aarch64-pc-windows-gnullvm`,
`aarch64-apple-darwin` and `x86_64-apple-darwin`. The cross targets were checked only; nothing was executed on Windows
or macOS.

Final suites at the LA head:

- `ocx_env_apply`: 78 unit tests and 1 doctest.
- `ocx_shim`: 158 unit tests and 18 `direct_launch` tests, with 1 timing test ignored. `batch_spawn` is `cfg(windows)`.
- The host `shim`-profile launcher is 471592 bytes against the 524288-byte budget (V-L57, local half).

## Steps

| Step | Commit | Red (failing line) | Green |
|---|---|---|---|
| 1 record decode (C-101, C-103, C-105) | `21117206` | 14 `launch::tests::contract_*` panics at `launch.rs:180/228/255/278` (CW2 stubs) | `ocx_env_apply` 77 unit tests |
| 2 dispatch on the record (C-106) | `a379b7a3` | `vl08_a_launcher_beside_only_an_old_sidecar_exits_78_naming_the_record`, `direct.rs:852` `Legacy("shim")` | `ocx_shim` 100 unit, 10 integration |
| 3 home kinds (C-107…C-109) | `bf11f492` | `vl06_an_empty_pin_resolves_to_the_next_rung` `core.rs:461` (pin kept); `vl07_*`, `vl10_*`, `vl12_*`, `vl14_*` red per mutation; 7 `direct_launch` cases red against the old source | 112 unit, 17 integration |
| 4 image path, Linux (C-110) | `01cc0222` | 8 red, e.g. `vl15_…reads_the_record_beside_its_image` "none of /links/elsewhere/alias.launch", `vl18` (` (deleted)` accepted) | 120 unit, 17 integration |
| 5 image path, macOS and Windows (C-110) | `ed46210a` | `a_home_launcher_started_by_a_bare_argv0_off_path_runs_its_record` (exit 74, `SelfPathFailure`). V-L19 harness on Linux: an `image_path` that returns argv0 → "…/active/probe is not …/render/probe" | 124 unit; V-L19 Windows and macOS bodies are CI claims |
| 6 runfiles manifest (C-111) | `ded328cf` | 22 red, e.g. `vl43_a_plain_manifest_line_names_the_root` "no vendored root found" | 150 unit, 6 runs green |
| 7 batch failure line, budget (C-112, C-113) | `f5408ef7` | "failed to start" → "failed to launch" at `direct.rs:1322` → `…names_the_program_the_exit_and_the_length_limit` fails at `direct.rs:2379` | 152 unit, 5 runs green; size 471592 B |
| 8 security audit (opus) | `46d8c41a`, `a6aa58d7`, `c0049cc9`, `fb429893`, `bc0ac1bb` | see `LA-security.md` | 158 unit, 18 integration |
| conformance gap check | `c6524a27`, `b9e52ba1` | `a_windows_record_refuses_a_path_on_the_current_drive` `launch.rs:1083`; E0308 when `None` is restored at the `spawn_child` call (Windows target) | `ocx_env_apply` 78 unit tests, `ocx_shim` unchanged |

## Mutation controls (scratch edits, each confirmed present by grep, then reverted)

| Row | Mutation | Red |
|---|---|---|
| V-L01 | `deny_unknown_fields` removed from `PackageRecord`, then from `EnvRecord` | `contract_decode_refuses_extra_key`, `…_a_key_of_a_sibling_kind`, `…_an_unknown_key_in_an_env_entry` |
| V-L06, V-L07, V-L10, V-L12, V-L14 | Five mutations: empty pin kept, selector order changed, strip dropped, depth not raised, each skip/batch/absolute/guard check removed | the matching `vl06_`/`vl07_`/`vl10_`/`vl12_`/`vl14_` test |
| V-L43 | split at the last space; dedupe removed | `vl43_an_escaped_line_unescapes_…`; `vl43_duplicate_roots_are_deduplicated_before_the_cap_of_4096` |
| V-L44 | "lists `P`" rule removed | 2 `vl44_` tests |
| V-L45 | root hygiene removed | 3 `vl45_` tests |

V-L02, V-L05 and the struct-kind check were green against CW2's real code. Their mutations belong to the red-proof
protocol for PF/P10 rows; they were not applied here.

## Conformance against the launcher record

The gap check covered `adr_unified_launcher.md` § 3, § 5, § 9, § 13 and § 17. It also covered system design C-012,
C-013…C-020 and C-025…C-032, and the plan's C-101…C-113. Result: 40 launcher-source rows, all met. Five of them (V-L19
on Windows and macOS, the junction and blob rows) are met as CI claims. No launcher-source behaviour is known to be
missing.

One decision was recorded: when the image path cannot be read (Linux without `/proc`), every kind fails, `direct`
included. This was kept because C-012 step 1 requires `I` for the Unix device and inode check of kind `direct`.

## Interface drift

Additive only:

- `ocx_env_apply::launch::{is_absolute_any_host, is_batch_target, is_fully_qualified}` are `pub`. The shim's copies are
  deleted.
- `LookupFs::image_path` returns `Result<String, ShimError>`, because a deleted image is a 78.
- `ReadError` has two new variants for the manifest bounds.

No pub signature or type that CW2 declared was changed.

## Outside LA's files (reported, not edited)

- `scripts/launcher_blob_check.sh:121-127` writes a Windows `tool.shim` and expects `ocx` to start. After act B it
  needs a `tool.launch` package record.
- `test/tests/test_windows_shim.py:459` expects exit 69 and `ocx not found`. The Unix and Windows launchers now exit
  78 with `no ocx found on PATH` when resolution is exhausted.
- The `.shimref` golden test of `prepare_lazy`'s bytes was deleted with the reader. `ocx_package_manager` needs a
  record-based golden for its writer.
- Writers should refuse a Windows `package_root`, `project_root` or `ocx` with no drive letter. The decoder now refuses
  them with 78.
- Check that `__OCX_LAUNCHER_DEPTH` reaches the child under `ocx exec --clean` (`crates/ocx_env/src/vars.rs`).
- Act B must rebuild the six blobs from the LA head.
