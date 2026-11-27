# R1B evidence

1. Per-prompt hashing. Test `a_render_directory_whose_stat_pair_matches_is_accepted_without_hashing` red before the fix
   (`render_toolchain.rs:3261` "equal size and identity: the recorded hash is never read"), green after. Control in
   `a_render_directory_with_new_inodes_and_the_same_bytes_still_matches` now changes the hash AND the file identity (false);
   restored-tree half unchanged and green. Render-time caller `stamp_directory_is_current` shares the function; tasks::render_toolchain green.
2. Unreadable `active`. Test `clean_skips_a_home_whose_active_cannot_be_read` red before (left 5, right 8: shells retired),
   green after. `read_active` maps NotFound to "no active render", any other error skips the home with a warning. Empty-path
   `InternalFile` now names the state `projects` root.
3. Help. The contradiction was in `crates/ocx_cli/src/command.rs` (variant long_about, outside the named set, edited); `command/clean.rs` doc restored
   the two dropped sentences and names the render-directory retirement (10-line doc cap kept).
4. `LauncherGeneration::host()` caches in a OnceLock. `random_hex16` duplicate of `random_suffix` (publish.rs) left, reported.
5. Plan-step labels removed from comments in render_toolchain.rs and clean.rs.

Counts: `cargo test -p ocx_package_manager --lib -- tasks::clean tasks::render_toolchain activation` 284 passed; `ocx_store launcher_generation` 5 passed; `cargo check -p ocx` ok.
