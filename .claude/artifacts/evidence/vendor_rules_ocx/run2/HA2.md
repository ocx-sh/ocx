# HA2 evidence (run 2, acceptance tests for render, clean and refresh, red first)

Base `333a5264`. `ocx` built with `cargo build -p ocx --locked` (debug, 2 jobs) at the base and run through `OCX_COMMAND`,
scratch `HOME` and `OCX_HOME`, the suite's own zot registries (no Docker). The tests stay red by design: the code that makes
them green lands in TR, GC, EP, LA and act B; PF shows them green.

`verify:scoped` not run in any step: `python3 scripts/scoped_gate.py --plan` fails under a scratch `OCX_HOME` (it reads
`$OCX_HOME/ocx.toml`), and the acceptance lane is Bazel (rc-file rule). By hand per step: `ruff check` clean, `ruff format`
clean on new code (modules unformatted at base were not reformatted whole), `pytest --collect-only` imports every touched module.

## Step 1 — `5948998d` `test(toolchain): each render with new content publishes a new directory and switches active`

| Test | Row | Failing line (against the base build) |
|---|---|---|
| `test_toolchain_render.py::test_a_render_with_new_content_publishes_a_new_directory_and_switches_active` | V-L28, C-131 | `:1613` `shells/` holds only `default` |
| `test_toolchain_render.py::test_a_render_writes_the_v2_stamp_naming_the_active_directory_and_no_v1_stamp` | C-131 | `:1653` no `render_stamp.v2.json` |
| `test_toolchain_render.py::test_a_repeated_render_writes_nothing` | V-L28 | `:1677` no v2 stamp |
| `test_toolchain_render.py::test_a_render_with_changed_content_publishes_another_directory_and_keeps_the_old_one` | V-L28, V-L30 | `:1719` `active` names `default` |
| `test_toolchain_render.py::test_a_render_never_touches_shells_default_or_the_older_stamp_an_older_ocx_wrote` | C-132 | `:1770` `shells/default` rewritten in place |
| `test_toolchain_activate.py::test_two_ocx_versions_alternate_on_one_home_and_every_launcher_starts` | V-L34, C-134, S-102 | `:1556` `active` names `default` (later phases unexercised until TR) |

Old-layout tests of both modules and the `toolchain_fixtures.py` helpers rewritten to the new contract (`shell_bin` follows
`active`, `NAME` + `NAME.launch`, v2 stamp; new `active_shell`, `generated_shells`, `run_previous_in`); 38 of 113 collected red.

## Step 2 — `5f266e08` `test(clean): ocx clean retires superseded launcher directories and setup refreshes the global toolchain`

| Test group | Row | Failing line |
|---|---|---|
| `test_clean.py`: package `launchers/<G'>` retired, `deferred/<G'>` retired, `--dry-run` reports only (3) | V-L41, C-137, S-105 | `:244` no `launchers/` directory (precondition) |
| `test_clean.py`: toolchain home retires planted `default-<hex>`, `.staging-*`, `.gc-*`, keeps `shell_dir`/`previous_shell_dir` and a symlink child; `--dry-run` (3) | V-L30, C-137, C-132 | `:347` two renders give one directory (precondition) |
| `test_self_update.py::test_self_update_renders_the_global_toolchain_into_a_new_directory` | V-L42, C-136, S-103 | `:904` exit 75 `skipped_reason: bootstrap` — environment: the module's existing `test_self_update_installs_newer_version` fails the same way here; the `active` assertion was not reached |

Unit-only (not written): S-105's Windows running-`.exe` half; C-136's "render failure warns, setup succeeds" (no CLI seam).

## Step 3 — this commit `test(toolchain): render directories, clean and refresh after upgrade`

| Test | Failing line |
|---|---|
| `test_toolchain_cli.py::test_exec_refuses_a_foreign_trampoline_answer` | `:889` no `wp8tool.launch` in the active shell |
| `test_dependencies.py` public-dependency PATH case | `:1524` PATH names `entrypoints` |
| `test_exec_modes.py` entry-point mirror | `:386` PATH names `entrypoints` |
| `test_execution_records.py` two records joined | `:755` outer launcher under `entrypoints/` |
| `test_patches.py::test_a_launcher_run_by_absolute_path_matches_only_catch_all_rules` | `FileNotFoundError` on `launchers/` |
| `test_patches.py::test_companion_launchers_stay_off_path_and_its_dependency_launchers_are_claimed` | `FileNotFoundError` on `launchers/` |

Untouched tests: 200 passed, 2 skipped (toolchain and project modules); 277 passed, 3 skipped, 7 failed in the four package
modules, of which 5 are the reds above and 2 (`test_record_written_when_every_best_effort_probe_fails`,
`test_a_symlinked_system_config_refuses_the_launch`) fail on host conditions, not compared against base.

False hits (no changed surface): `test_toolchain_env.py` 1106-1541 (`"default"` group), 1726, 1764 (`entrypoints` report field);
`test_execution_records.py:775` (`sh.ocx.entrypoints` annotation); `test_package_inspect_closure.py` 124, 354, 357, 485, 487,
494, 495, 583, 612 (report field); `test_patches.py` 3359, 6623 (metadata field); `test_project_pull.py` 371, 1707 (group);
`toolchain_fixtures.py` 106, 115, 119 (older-stamp constants); `test_toolchain_offline_after_pull.py`, `test_self_setup.py`: none.

## Open for later pipelines

- `test/src/helpers.py` (outside every acceptance pipeline's file set) `shim_bin_dirs`, `assert_shim_dir_exists`,
  `assert_shim_dir_absent` walk `$OCX_HOME/shims/…/bin`; the new deferred layout is `deferred/<G>/…/l`. Callers
  `test_patches.py:3218`, `test_execution_records.py:636`, `test_execution_record_standards.py:441`, `test_progress_channel.py:110`.
- `test_dependencies.py` sealed-dependency case asserts `launchers` absent and passes today without a positive control.
- The tests assume stamp v2 keeps `home`, `scope`, `names`, `bin_fingerprint` (`content_hash`, `size`), `link_fingerprint`.
- GC must walk project homes (`state/projects/<key>/`) as well as the global one; the `deferred/<G'>` report path is the
  `$OCX_HOME/deferred/<G'>` directory itself.
- `test_self_setup.py` full run: 12 reds from no network to `index.ocx.sh` (environment, unedited).
