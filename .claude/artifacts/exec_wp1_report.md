# WP-1 (quick-five) execution report — issue sweep 2026-08-30

Branch `hex/issue-sweep--wp1`, worktree `.agents/worktrees/isw-wp1`, base `52eecc15`.
Gate: `task rust:verify` (not `task verify` — eight worktrees share one registry).
**Result: exit 0**, `Summary [113.182s] 6429 tests run: 6429 passed, 8 skipped`;
fmt, `clippy -D warnings`, license and both Windows cross-checks green.

> A first gate run was backgrounded and reported exit 0 while its log held five
> lines — the run had been cut short. A second, foreground run hit the 10-minute
> tool cap and SIGTERM'd its last test, which nextest reported as a failure.
> Neither is the result above; the quoted summary is from a third, complete run.

## Contract results

| ID | Status | Evidence |
|---|---|---|
| C-001 | DONE | `config/loader.rs` ceiling joined onto `start` + `lexical_normalize`. Commit `2e14b699`. Tests `project_path_walk_stops_at_a_relative_ceiling`, `an_empty_ceiling_does_not_bound_the_walk`. |
| C-002 | DONE | `file_structure::home_directory` + `default_ocx_root` are the single definition; `ConfigLoader::home_dir` deleted, its three uses rerouted; `auth/store.rs:163` on the shared resolver. Commit `16e84e21`. Test `the_ocx_home_default_has_one_definition`. |
| C-003 | DONE | `ocx_index.rs` check 1a (`file_colon_tail` / `file_colon_origin`). Commit `e41b8743`. Tests `resolve_base_url_refuses_a_single_slash_file_base`, `a_schemeless_base_still_resolves_as_https`. |
| C-004 | DONE | `announce/error.rs` `OutputWrite` → `ExitCode::IoError`. Commit `450e9f8c`. Test `output_write_classifies_as_io_error`. |
| C-005 | DONE | `.claude/taskfile.yml` lychee gains `--include-fragments`. Commit `bc92878f`. |

## Red/green proofs

Every mutation was gated on the mutated token actually being present before the
run; every restore was verified after it.

### C-005 (S-004) — lychee anchor gate

Fixture: `.claude/rules.md`'s `(./rules/quality-core.md)` → `(...#no-such-heading)`.

| State | Command | Exit |
|---|---|---|
| flag on, clean tree | `task lint:links --force` | **0** |
| flag on, broken anchor | `task lint:links --force` | **201** (lychee exit 2) |
| flag **off**, same broken anchor (control) | `task lint:links --force` | **0** |

The control is what makes the green meaningful: without `--include-fragments`
the identical broken anchor passes, so the flag is the discriminator.

### C-004 (S-003) — announce `--out` exit code

| State | Mutation | Exit |
|---|---|---|
| green | — | **0** |
| red | deleted `Self::OutputWrite { .. } => Some(crate::cli::ExitCode::IoError),` (grep count 1 → 0 before the run) | **101** — `assertion left == right failed: an --out write failure of kind StorageFull must exit 74` |

`StorageFull` is deliberately in the loop: `cli/classify.rs`'s bare-`io::Error`
walker special-cases only `PermissionDenied`, so a test that used only
`PermissionDenied` would stay green with the arm deleted.

### C-003 (S-002) — single-slash `file:` index base

| State | Mutation | Exit |
|---|---|---|
| green | — | **0** (71 tests) |
| red | check-1a arm made unreachable (`if let Some(tail) = … && false`) | **101** — `resolve_base_url_refuses_a_single_slash_file_base` FAILED |

Mutated the **consumer** arm rather than deleting `file_colon_tail` /
`file_colon_origin`: deleting the producers would trip `dead_code` and kill the
build instead of the test.

### S-022 — the negative C-003 must not break

| State | Mutation | Exit |
|---|---|---|
| green | — | **0** |
| red | `file_colon_tail`'s prefix test replaced by `true` (the realistic over-broad guard) | **101** — `a_schemeless_base_still_resolves_as_https` FAILED, with 6 sibling tests |

### C-001 (S-001) — relative `OCX_CEILING_PATH`

| State | Mutation | Exit |
|---|---|---|
| green | — | **0** (109 tests) |
| red | ceiling expression reverted verbatim to the pre-fix `crate::env::var("OCX_CEILING_PATH").map(PathBuf::from)` | **101** — `project_path_walk_stops_at_a_relative_ceiling` FAILED |
| red (empty-value guard) | dropped `.filter(\|value\| !value.is_empty())` | **101** — `an_empty_ceiling_does_not_bound_the_walk` FAILED |

Restore verified byte-identical (`cmp` against the pre-mutation backup).

**A first attempt at C-001 failed and is recorded because it changes the fix.**
A bare `start.join(value)` — which is what "absolutized against `start`" reads
as — left `project_path_walk_stops_at_a_relative_ceiling` red: the ceiling only
ever fires at `start` or an ancestor, so every *useful* relative spelling is
`..`-prefixed, and `Path` equality keeps `..` as a component rather than folding
it (`<cwd>/..` ≠ `<cwd>`'s parent). The shipped fix therefore also runs
`utility::fs::path::lexical_normalize` (existing helper, no filesystem access —
which is what keeps it comparable against `current`, itself never canonicalized).

### C-002 (#381) — one `$OCX_HOME` definition

| State | Mutation | Exit |
|---|---|---|
| green | — | **0** |
| red 1 | `ConfigLoader::home_path` restored to the pre-fix duplicate (`crate::env::var` + `dirs::home_dir`) | **101** — `left: Some("config.toml")` vs `right: Some("/home/…/.ocx/config.toml")` |
| red 2 | `default_ocx_root` restored to `std::env::var("OCX_HOME")` | **101** — `left: Some("/home/…/.ocx")` vs `right: Some("/tmp/.tmpYdhixZ")` |

Both restores verified byte-identical with `cmp`.

Red 1 is worth reading: the pre-fix loader turned an **empty** `OCX_HOME` into
the *relative* path `config.toml`, because it had no non-empty filter. That was
a second, unreported defect inside #381's blast radius, and it is fixed by the
same collapse.

## Blast radius mapped before C-002 (as the plan required)

`default_ocx_root` — 4 call sites, all wanting the OCX data root, none depending
on `std::env::var` specifically:
`crates/ocx_cli/src/command/about.rs:62`,
`crates/ocx_lib/src/project/consent.rs:712`,
`crates/ocx_lib/src/file_structure.rs:101` (`FileStructure::new`),
`crates/ocx_lib/src/oci/host_capabilities.rs:1234`.

`ConfigLoader::home_dir` — 3 call sites, all inside `config/loader.rs`
(`:271` `managed_snapshot_candidate`, `home_path`, `home_sigstore_trusted_root_path`).
All three now call `default_ocx_root()`; the function is deleted, not aliased.

`setup::home_env_from_environment()` (`setup.rs:840-846`) untouched, per C-002.

## Deferred / refused

Nothing refused — all five contracts landed inside the declared file scope.

- **C-003's correction rides `origin`, not a new error field.** The natural shape
  would be a `hint` on `Error::InvalidIndexUrl`, but that variant lives in
  `crates/ocx_lib/src/oci/index/error.rs`, outside WP-1's file scope. `origin` is
  the only free-form field `invalid_index_url()` lets a caller in `ocx_index.rs`
  compose, and it is what an operator reads to learn which setting to change, so
  the correction is appended there:
  `[registries."<ns>"] index; a file base needs two more slashes, as "file:///srv/x"`.
  `source: None` and the existing helper are both as C-003 specifies. If a later
  WP touches that file, a dedicated `hint: Option<String>` field would read
  better — noted, not needed.
- **`auth/store.rs:163` has no locally reachable red state.** The swap to
  `home_directory()` is compiler-verified, but `dirs::home_dir()` and
  `std::env::home_dir()` return the same value on this Linux host — they diverge
  only on Windows (`%USERPROFILE%` overridden) and over an empty `pw_dir`. A test
  asserting the path would be green in both states, which is not a check. Left
  untested deliberately rather than shipped as an unchecked green.
- **`OCX_CEILING_PATH` is folded lexically, never canonicalized.** Symlinks are
  not resolved, because `current` is not canonicalized either and canonicalizing
  one side would stop the two from ever matching under a symlinked cwd. A
  consequence of `lexical_normalize`'s backslash pre-pass: on Unix a ceiling
  naming a directory whose name literally contains `\` would be split into
  components. Pre-existing behaviour of that helper, exotic, not fixed here.
- **C-003 also refuses `file:8080/x` and any other `file:`-prefixed schemeless
  base.** Before the change those resolved to `https://file:8080/x` — a host
  literally named `file`. Refusing them is the point of #382, but it is a
  behaviour change beyond the single-slash example, so it is named here.
