# R1C evidence

1. retire_dir NotFound — 1c9e5336. Red: `retire_real_directory_gone_after_the_lstat_is_removed` panicked on unwrap (rename NotFound returned Err). Green: ocx_util publish:: 22 passed.
2. Junction verbatim prefix — 1c9e5336. Pure `mount_point_buffer(&[u16])`. Red: `mount_point_buffer_drops_a_verbatim_prefix` substitute `\??\\\?\C:\ddd…`. Green with the strip; UNC maps to `\??\UNC\server\share`. Windows runtime: not run, CI only.
3. First use re-check — generate.rs 6f6dbbb8, prepare_lazy.rs 594bb207. Red: `publish_after_retiring_keeps_a_directory_completed_in_the_meantime` ("the complete directory was retired"); `retire_incomplete_keeps_a_tree_completed_in_the_meantime` red with the check removed. Green: ocx_package_manager launcher::/tasks::prepare_lazy/tasks::vendor 133 passed.
4. LeftForLater — 594bb207. Red: `retire_incomplete_names_a_refused_retire` (`unwrap_err` on Ok(false)) with the arm swallowed. Now PermissionDenied "in use or protected, not retired" with the path (exit 77 by the existing taxonomy). Unix-only test (symlink): skipped on Windows, 1 test.
5. vendor host_paths — 7a744b8d. Red: both new tests failed (non-UTF-8 accepted; `/tmp/opt/x` refused by `/tmp/o`). Green: tasks::vendor 54 passed. Non-UTF-8 test is cfg(unix): skipped on Windows, 1 test. Segment match: a value is refused unless the next char after the path continues a name (alphanumeric, `_`, `-`, `.`); a trailing-separator path matches any continuation.
6. sync_data / writer unification: untouched, deferred.
7. retarget_symlink vs replace_atomic: left. Not byte-for-byte equal: replace_atomic creates the parent directory, stages as `.tmp-<pid>-…` (so sweep_retarget_debris and test `retarget_sweeps_a_stale_staged_link…` no longer cover it) and returns FileError, not io::Error.
