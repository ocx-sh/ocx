# VF evidence (run 2, the vendored format, ocx)

Base `333a5264`. Built with `cargo` in a VF-only target dir. `task verify:scoped` was not run: `scripts/scoped_gate.py
--plan` routes a Bazel lane on this branch (rc-file rule, DP-39), so each step ran `cargo fmt --all --check`,
`cargo clippy -p <crate> --all-targets --locked -- -D warnings` and `cargo test -p <crate>` per touched crate by hand.
The driver widened the file set by `tasks/pull.rs` (`VendorTarget` and its tests only; DP-37 precedent, no wave-1
editor, EP after VF).

## Step 1 — `18807f29` `feat(vendor): short vendored layout names`

| Test | Failing line |
|---|---|
| `ocx_store` `vendor_layout::tests::contract_package_keys_refuses_a_hex16_clash_naming_both` | `vendor_layout.rs:135` (`package_keys` unimplemented) |
| `contract_package_keys_keys_by_first_16_hex` | `vendor_layout.rs:135` |
| `the_same_digest_twice_is_not_a_clash` | `vendor_layout.rs:135` |
| `contract_check_relative_path_refuses_reserved_segment` | assertion at `:384` |
| `contract_check_relative_path_agrees_with_every_path_rule` (C-123 table) | `:442` |
| `short_layout_names` | stub at `:114` |

Green: `ocx_store --lib vendor_layout` 29 passed.

## Step 2 — `7383d5f0` `refactor(vendor): the composer takes vendored paths from VendorLayout`

| Test | Failing line |
|---|---|
| the rewritten vendor-lane units of `ocx_package_manager` | did not compile: `composer.rs:8151` E0061, `ComposePaths::vendor` took 1 argument |

Green: composer 160 passed (1 more is step 3's test), resolve 156, vendor_lane 4. Deviation: the home-lane helpers
`emit_dep_path_block`, `emit_root_path_block` and `synth_entrypoints_path_for` (`composer.rs:759-767`) now take an
entry-points `&Path`; home output is byte-identical (the existing home-lane tests stay green).

## Merge `039b7ff1`

The feature branch with IX and DOC2 merged in, as the coordinator directed.

## Step 3 — `2d4f024e` `feat(vendor)!: vendored trees use the short layout`

| Test | Failing line |
|---|---|
| `a_selection_launcher_starts_the_tools_target_in_the_selection_environment` | `vendor.rs:2788` |
| `an_entrypoint_launcher_starts_the_command_its_environment_finds_first` | `vendor.rs:2684` |
| `the_selection_environment_is_the_composers_with_the_install_root_swapped` | `vendor.rs:2301` (left `p/cab8…/c/bin`, right `packages/…/content/bin`) |

Green: vendor 49, pull 63, composer 161, `ocx_package_manager --lib` 1632 + 1 ignored, `ocx_store --lib` 406 passed with
the 4 PB contract reds (not VF's). New C-115 unit
`two_packages_sharing_a_directory_name_are_unrepresentable_naming_both_digests`. The `legacy_*` functions are deleted.
Latent, not fixed: `setup_vendored` builds `PackageDir::with_root(package_dir)`, whose `.content()` joins `content`,
not the vendored `c`. Nothing reads it (`place_package` uses only `info.metadata()`).

## Step 4 — `b2dfcc63` `feat(vendor): the manifest lists record paths and the selecting index`

| Test | Failing line |
|---|---|
| `the_manifest_lists_record_paths_and_the_selecting_index` | `vendor.rs:2494` (`index_digest` was `None`) |
| `a_name_ending_in_the_record_suffix_is_refused` | `Ok` returned |
| `a_record_path_is_the_file_the_writers_place` | "… lists no record" |
| `names_equal_after_folding_collapse_to_one_record_on_a_folding_target` | count 1 against 2 |

Green: vendor 54, `ocx_package --lib vendor` 4, `ocx_package_manager --lib` 1637 + 1 ignored. Contract-wave drift: the
`expect(dead_code)` attribute on `is_record_name` was removed from `tasks/toolchain_names.rs` (a CW2 file outside VF's
set), forced by the first caller. An entry-point `.launch` refusal is unreachable: the metadata grammar forbids `.`.

## Step 5 — `fix(vendor): a vendor run writes nothing into the index directory`

All three tests are in `crates/ocx_package_manager/src/tasks/vendor.rs`.

| Test (`tasks::vendor::tests::`) | Failing line (before) | Green after |
|---|---|---|
| `placing_a_package_writes_nothing_into_the_index_directory` (C-119, R-U10) | `:3159`: the index directory gained `tool/p/example.com/o/sha256/<index digest>.json` | passes |
| `a_declared_binary_the_content_lacks_names_the_package_and_the_claim` (C-121, R-U1, V-L52) | `:3196`: the message lacked `example.com/tool:1.0@sha256:…` | passes |
| `the_manifest_lists_index_lookups_only_with_a_recorder` (C-118) | `:3227`: `index_lookups` was `None` with a recorder | passes |

What the first test proves: `place_package`, handed the writable manager, resolves through its read-only view. It
resolves a digest that names an image index over a seeded index directory, which a writable chain persists as a
dispatch object, and the directory is byte-identical afterwards. It does not prove the whole run: the fake registry
serves no layers, so the pull ends in an error after the resolve. And `verify_selection` has no assertion of its own: the
trust round it runs writes nothing without a configured policy, so its view is by construction only. VA's 13 probes found
no `--index` write by a run today (`select` reads through an inspect view already), so the acceptance test was green
before this step and is not this step's red.

The message test asserts the identifier with its tag, `tool:1.0`, `` `binaries` ``, `` `cmake` ``, `does not hold` and
`publisher`; the VA fragments are the same set. The refusal stays `PathUnrepresentable` (65). The lookups test asserts the
field equals `PackageManager::index_lookups()`, sorted and each once, and is absent without a recorder.

Green: `ocx_package_manager --lib vendor` 57, `--lib pull` 63, `--lib` 1640 passed + 1 ignored; clippy
`-D warnings` clean for `ocx_package_manager`; `cargo fmt --all --check` clean.

## V-rows VF proves at unit level

V-L50 (paths, unit half), V-L51, V-L54 (unit half), V-L56 (`ocx_store` half), V-L27 (vendor half), V-L52 (message unit),
V-L60 (unit half). The acceptance halves belong to VA; CI and Windows rows are not proven here.

## Open items

- The record files are pretty-printed JSON, not the compact `encode` form (`launcher/direct.rs`).
- `direct.rs` has a redundant non-Windows `.launch` check.
- `write_selection_launchers` assumes one selection (one `l/`).
- The latent `setup_vendored` `PackageDir` content path above.
