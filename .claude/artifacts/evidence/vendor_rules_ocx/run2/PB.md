# PB evidence (run 2: publish primitives and the generation key)

Base `333a5264`. Branch `hex/bazel-rbe-rules-ocx--PB`. `CARGO_TARGET_DIR=$SCRATCH/run2/target-PB`, 2 jobs, no debug info.
`task verify:scoped` was not run because it routes a Bazel lane (rc-file rule, DP-39). Each step ran these by hand:
`cargo fmt --check`, `cargo clippy -p <crate> --all-targets --locked -- -D warnings` and the crate tests.

| Step | Commit | Red (against the stubs) | Green |
|---|---|---|---|
| 1 generation key (C-124) | `596d00f6` | `not implemented: LauncherGeneration::of`, `not implemented: render_fingerprint` (3 red) | `cargo test -p ocx_store launcher_generation`: 5 passed. Both goldens of § 4.1 match. |
| 2 `publish_dir`, `retire_dir` (C-125) | `89df2484` | `publish.rs:39 not implemented: publish_dir`, `publish.rs:61 … retire_dir` | `fs::publish`: 11 passed. The 4 red were step 3's stubs. Added: V-L22 two concurrent publishes converge (one `New`, one `AlreadyThere`); V-L21 a failed publish leaves no target, then a retry succeeds; V-L30 `Removed` and `LeftForLater` (read-only parent); a `.gc-*` symlink is swept as a link. |
| 3 `retarget_link`, `active_target_name` (C-125, C-138) | `30170312` | `publish.rs:103 not implemented: retarget_link` (4 `contract_retarget_*`); `toolchain_store.rs:205 not implemented: ToolchainHome::active_target_name` | `fs::publish`: 16 passed. `file_structure::toolchain_store`: 32 passed. |
| review fix | `934ef5ab` | Mutations, each grepped as landed and then reverted: the early return in `sweep_retarget_debris` fails `retarget_sweeps_a_stale_staged_link_and_leaves_bystanders` (`publish.rs:665`); trailing-slash and `./` folding in `active_target_name` fails `active_target_name_refuses_every_non_canonical_spelling` (`toolchain_store.rs:479`). | `fs::publish`: 17 passed. `toolchain_store`: 33 passed. `launcher_generation`: 5 passed. |

Review: one adversarial reviewer (sonnet, read-only) over `333a5264..934ef5ab^`.

- Fixed:
  - the junction-proof reader floor (at least 1000 reads per phase, both markers seen);
  - the clippy `#[expect]` on `Command`;
  - `active_target_name` raw-byte compare;
  - the `.tmp-retarget-*` sweep;
  - the Windows absent-link create, now a staged junction plus a no-replace `MoveFileExW`;
  - the EXDEV partial-directory exception, now documented;
  - the blocking I/O and drive-letter-only docs;
  - the `.gc-` pick, which now treats only `NotFound` as free.
- Void: "the junction-proof job does not exist". CI2 added it to the feature branch.
- Not fixed: no transient retry on the junction open in `retarget_link`. No sync retry helper exists in `fs.rs`.

Windows:
- `cargo check -p ocx_util -p ocx_store --lib --tests --target x86_64-pc-windows-gnullvm` is clean.
- Clippy for that target reports nothing in PB's files. Its pre-existing hits in `symlink.rs`, `user_file.rs` and `fs.rs` were suppressed for the run.
- Both checks ran with a zig 0.16.0 C compiler from PyPI `ziglang` in scratch.
- Not linked and not run on this host. V-L31 and V-L32 are proven only by `basic-repo.yml › junction-proof`, which runs `fs::publish::tests::junction_proof_retarget_under_lookup` and `fs::publish::tests::junction_proof_retire_running_exe` from `//crates/ocx_util:ocx_util_test`.
- When the standard-user leg lacks symlink privilege (os error 1314), it skips only the directory-symlink heal case, and says so.

Not run: Bazel lanes, acceptance, macOS. `OCX_UTIL_WITHOUT_CONSUMER` is untouched (I2): PB adds no caller of the three helpers.
