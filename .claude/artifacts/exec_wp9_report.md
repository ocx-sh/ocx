# WP-9 — `FileReference` unification (issue #379)

Branch `hex/issue-sweep--wp9`, rebased onto `goat @ f847d487` (carries WP-5 and WP-10) —
**zero conflicts**, including `command-line.md`, which WP-10 also touched on a different row.
Gate: `task rust:verify` → **exit 0**, 6473 tests run, 6473 passed, 8 skipped.
Design record: `adr_key_reference_grammar.md` (two spellings, not three) — not re-derived.

## Contracts

| | Verdict | Evidence |
|---|---|---|
| **C-080** | **DONE** | `FileReference<'a>` + `Spelling { Bare, FileUrl }` in `crates/ocx_lib/src/utility/fs/path.rs`, immediately after `RelativePath`. Borrowed and **total** — parsing cannot fail, so each door keeps its own error vocabulary and exit code (`InvalidIndexUrl` 78 vs `KeyRefError` 64 say different things about an empty value). Exactly three exits — `anchored_at`, `absolute`, `as_written` — **no `as_path()`**. Proven by M4 / M5 / M6 / M7 below. |
| **C-081** | **DONE** | `resolve_base_url` (`oci/index/ocx_index.rs`) routes on `FileReference::parse(base).spelling() == Spelling::FileUrl`; the `Some("file")` arm is gone from the `scheme_of` match. `resolve_file_base` now gets its payload from `FileReference::absolute()`, which replaced the hand-rolled authority split. New test `resolve_base_url_refuses_the_bare_file_reference_spelling`. Proven by M1. |
| **C-082** | **DONE** | `KeyRef::as_file() -> Option<FileReference<'_>>` gated on `Scheme::File`; `as_path()` is now `self.as_file().map(|f| f.as_written())`. `trust.rs::anchor_relative_keys` consumes `as_file()` + `anchored_at`, deleting its own copy of the `!has_root()` rule. `Scheme::Env` never reaches the delegation — proven by M8. |
| **C-083** | **DONE** — all four readers | Door 1: `SigstoreTrust::anchor_relative_root` parses through `FileReference` (unit test S-015 + **acceptance** test). Door 2: `package_sign_common::explicit_trust_root_path` for `--sigstore-trusted-root` / `OCX_SIGSTORE_TRUSTED_ROOT`. Doors 3 and 4 landed as D1/D2 below — auto-verify's own env read (`app/context.rs`) and `ocx config push`'s own payload resolution (`managed_config/publish.rs`). **Nothing narrows** — every bare path round-trips byte for byte (M3's table pins it). |

## Scenarios

- **S-015** — `[trust.sigstore] trusted_root = "file:///abs/path"` accepted. Covered twice: unit
  (`trust::tests::anchor_relative_root_takes_the_file_url_spelling`) and **acceptance**
  (`test/tests/test_trust_root_distribution.py::test_config_tier_trusted_root_takes_the_file_url_spelling`),
  the latter with a bare-path control leg in the same test so a green cannot come from a warm
  cache or a TUF fetch.
- **S-022** — the negative contract. `index = "index.corp.example"` still resolves as
  `https://index.corp.example`, never as a path. New test
  `resolve_base_url_refuses_the_bare_file_reference_spelling`; red proof M1.

## Red/green proofs

Harness `~/.cache/wp9-proof/prove.py`. Every case gates that (a) the mutation landed on disk,
(b) the red run was a real **test** failure and not a build break, (c) the restore is
**byte-identical** to the original file, (d) the green run reports `1 passed; 0 failed`.
Re-run in full **after** `cargo fmt`, so the proven text is the shipped text.

| Mutation | Target test | red | green |
|---|---|---|---|
| M1 invert the `Spelling::FileUrl` route in `resolve_base_url` | `…::resolve_base_url_refuses_the_bare_file_reference_spelling` | 101 (FAILED) | 0 |
| M2 `parse` → `bare` in `anchor_relative_root` | `trust::tests::anchor_relative_root_takes_the_file_url_spelling` | 101 | 0 |
| M3 `parse` → `bare` in `explicit_trust_root_path` | `command::package_sign_common::tests::explicit_trust_root_takes_a_bare_path_or_a_file_url` | 101 | 0 |
| M4 `eq_ignore_ascii_case` → `==` in `FileReference::parse` | `…::file_reference_reads_two_spellings_and_nothing_else` | 101 | 0 |
| M5 drop the trailing-slash trim in `absolute()` | `…::absolute_answers_only_for_an_empty_authority_naming_a_directory` | 101 | 0 |
| M6 invert the `has_root()` branch in `anchored_at` | `…::anchored_at_joins_only_a_rootless_reference` | 101 | 0 |
| M7 `bare()` delegates to `parse()` | `…::bare_never_re_splits_an_already_extracted_payload` | 101 | 0 |
| M8 drop the `Scheme::File` gate in `KeyRef::as_file` | `trust::tests::a_key_reference_naming_an_unimplemented_backend_never_becomes_a_key_ref` | 101 | 0 |

Acceptance red proof (`~/.cache/wp9-proof/prove_acc.sh`), gated on the mutation landing, the
release binary's **sha256 changing**, the restore landing, and the restored binary hashing back to
the green one (`64f470cb891c` → `517973c66385` → `64f470cb891c`):

```
GATE mutation-landed hits=1
GATE binary-changed before=64f470cb891c after=517973c66385
RED pytest exit=1   # assert 74 == 0, message: I/O error reading
                    #   '…/ocx-home/file:///…/test/sigstore/trusted_root.json'
GATE restore-landed original=1 mutated=0
GATE binary-restored final=64f470cb891c equals_green=yes
```

The red message is the pre-widening behaviour verbatim — the `file://` value anchored against
`$OCX_HOME` as if it were a filename. The **control leg passed under the mutation**, so the test
discriminates the spelling, not the rung. Green: `5 passed` for the whole file.
`uv run ruff check .` from `test/` → exit 0.

### One guard whose red state is not reachable on Linux — recorded, not papered over

The **rooted** rows of `anchored_at_joins_only_a_rootless_reference` (`/srv/keys/acme.pub`,
`file:///srv/keys/acme.pub`) cannot be shown red on this host. Replacing the whole branch with
`dir.join(path)` left the test green, because Unix `Path::join` already **replaces** the base when
the argument is absolute. The `has_root()` branch is a **Windows-only** guard — there `join` keeps
only the base's prefix and silently moves the reference to the config dir's drive. M6 therefore
mutates the *branch condition* instead, which reds on the rootless rows. Per the "mutation that
fails to red means you have not found every guard" corollary: the rooted rows document a Windows
invariant and are load-bearing only there.

## Where the plan was stale against the tree

1. **Collisions table, `oci/index/ocx_index.rs` row** — predicted WP-9 would touch `file_root` and
   `has_drive_prefix`. It touched neither. The real edit is in `resolve_base_url` and
   `resolve_file_base` — i.e. **exactly the functions the row assigned to WP-1**. WP-1 is merged, so
   there was no live collision, but the file-disjointness claim for that row was wrong.
2. **`command/package_sign_common.rs` row** — `resolve_trust_root` is at `:547`, not `:502-523`.
   WP-2's landed edits moved it. Symbol correct, line numbers stale.
3. **`trust.rs` row** — accurate. `compile_key_reference` (WP-4's `:1063-1067`) is now `~:1056-1090`
   and was not touched; my edits are `anchor_relative_root` (`~:238-268`) and
   `anchor_relative_keys` (`~:875-895`).
4. **Handover claim about WP-4** — "renamed `FileKeyBackend` → `PemKeyBackend`" is **correct**;
   `PemKeyBackend` is live in `oci/sign/key_backend.rs`, `env.rs` and `package_manager/tasks/sign.rs`.
   `Scheme::Env` and `KeyRefError::FileColonPrefix` are both in the tree as described.
5. **C-080's "no `as_path()`"** scopes to `FileReference`, not to `KeyRef`. `KeyRef::as_path()` is
   **kept**: its production caller `package_manager/tasks/sign.rs:322` is **WP-5's file**, and
   removing it would have forced an edit there. It is now one line of delegation.

## Deliberate design calls worth a reviewer's eye

- **`absolute()` yields `&str`, not `&Path`.** Its one caller composes `file://{path}` back out of
  the answer and applies a byte-level drive-designator rule to it. It is also **byte-tested**
  (`starts_with('/')`) rather than `Path::has_root()`: on Windows `Path::new("C:/srv/x")` reports a
  root, and that payload is the *authority* of `file://C:/srv/x`. Using `has_root()` would have
  **widened the index door on Windows only** — a base has to be valid or not independently of the
  host reading it. M5 and the `file://C:/srv/x` row pin this.
- **`FileReference::bare()` exists so `KeyRef` never re-splits.** `KeyRef::parse` consumes
  `<scheme>://` generically; handing the remainder to `parse` would make `--key file://file://x` name
  `x` instead of the file literally called `file://x`, breaking the escape the ADR relies on for
  `file://file:x`. M7 is the guard.
- **`FileReference::parse` matches `file://` case-insensitively**, which is what the index door
  already did (`scheme_of` lowercases). The key door is unaffected because `KeyRef` keeps its own
  case-sensitive `Scheme::parse` token match and only delegates *after* it — so `FILE://x` is still
  `UnknownScheme` on `--key`, unchanged.
- **`anchor_relative_keys` now rewrites unconditionally** instead of only when rootless. For every
  input any existing test covers the stored string is byte-identical. The one shape that changes is
  an *absolute* `file://`-spelled policy key, which is now normalized to its path — every downstream
  reader already parsed it to exactly that, and the error messages already named the stripped path.
- **`anchor_relative_root` goes through `to_string_lossy`.** Exact here: the value is deserialized
  from a TOML string, so it is UTF-8 by construction. The CLI door uses `to_str()` with a verbatim
  fallthrough instead, because an `OsString` from a shell genuinely can be non-UTF-8 and a `file://`
  prefix is ASCII — nothing is lost at that door.

## Scope extension, declared

C-083 is a user-visible widening, so it is not shipped until its surfaces say so. Files edited
outside the WP-9 list, all unclaimed by any wave-2 package (WP-2 and WP-4, the only declared owners
of `command-line.md`, are merged):

- `crates/ocx_cli/src/command/package_verify.rs`, `package_sbom.rs` — one help line each
  (`…named by a bare path or a file:// one`). `value_name = "PATH"` deliberately **unchanged**: one
  extra spelling does not earn a CLI-surface rename to `<REF>`.
- `website/src/docs/reference/configuration.md` (the `trusted_root` field row),
  `reference/command-line.md` (the `--sigstore-trusted-root` row), `reference/environment.md`
  (`OCX_SIGSTORE_TRUSTED_ROOT`), `in-depth/signing.md` (ladder rungs 1 and 3). Wording matches the
  established `key` phrasing — "a bare path, or a `file://` one". No migration prose.
- `test/tests/test_trust_root_distribution.py` — the S-015 acceptance test (ruff clean).

## D1 / D2 / D3 — granted and landed

A widening true at two doors and false at two others is a worse contract than the narrow one.
All three are now closed, each with the acceptance test its own command surface needs.

| | What it was | Now |
|---|---|---|
| **D1** | `app/context.rs` read `OCX_SIGSTORE_TRUSTED_ROOT` at its own call site, so **auto-verify** on install/pull stayed narrow | `.map(explicit_trust_root_path)` — one env value cannot mean a bare path on install and a `file://` one on verify. `explicit_trust_root_path` is `pub(crate)`; `context.rs` already imported from that module. |
| **D2** | `ocx config push` resolves a payload's `trusted_root` **without ever reaching the config loader**, so `file:///x` sent the operator's own publish run looking for a file named `file:///x` | One `FileReference::parse(...).anchored_at(dir)` call at the publish site. `declared_trusted_root` is untouched — its name promises what the payload *declared*, and its existing test passes unmodified. |
| **D3** | The flag door was unit-covered while the config door was acceptance-covered — the asymmetry at the door an operator reaches for first | `test_offline_verify.py::test_trusted_root_flag_and_env_take_the_file_url_spelling`, covering `--sigstore-trusted-root` and the env var, with a bare-path control leg. |

### D2 turned up a third copy of the rule

`publish_managed_config` carried its own `!has_root()` anchoring, under a comment stating that it
and `SigstoreTrust::anchor_relative_root` "must not drift". They had already drifted — not on
Windows, which the comment guarded, but on the **spelling**, which nobody was watching. That is the
same shape as the finding that opened #379: a rule documented honestly at one site and violated at
a sibling. Three copies are now one call.

### Acceptance proofs for D1 / D2 / D3

Harness `~/.cache/wp9-proof/prove_acc2.py`. Per case: mutate, gate the mutation landed, **rebuild
the release binary and gate its sha256 changed**, run the target test, restore, gate the source is
byte-identical, rebuild, gate the restored binary differs from the mutated one, re-run green.

| Mutation | Target test | mutated sha | red | restored sha | green |
|---|---|---|---|---|---|
| D1 drop `.map(explicit_trust_root_path)` in `context.rs` | `test_auto_verify.py::test_offline_auto_verify_reads_the_file_url_spelling` | `950a96d97600` | 1 (FAILED) | `fded17fee1c8` | 0 |
| D2 `parse` → `bare` at the publish site | `test_trust_root_distribution.py::test_config_push_reads_the_file_url_spelling_of_the_declared_root` | `9fa0508fadca` | 1 | `fded17fee1c8` | 0 |
| D3 `parse` → `bare` in `explicit_trust_root_path` | `test_offline_verify.py::test_trusted_root_flag_and_env_take_the_file_url_spelling` | `ccc2d0abb4d8` | 1 | `fded17fee1c8` | 0 |

Re-run **after** `cargo fmt`, so the proven text is the shipped text: the harness aborts on
`anchor hits=0`, which is precisely what a reflowed mutation site would produce.

D3's red message is the pre-widening behaviour verbatim —
`trust-root file could not be read: I/O error reading 'file:///…/trusted_root.json'` — and its
**control leg passed under the mutation**, so the test discriminates the spelling and not the rung.
D1's is offline by construction: with the spelling unconsumed there is no pinned Rekor key, install
carries no `--rekor-url`, and `OCX_OFFLINE=1` forbids the default-endpoint fetch, so it fails closed
rather than passing for an unrelated reason.

`uv run ruff check .` from `test/` → exit 0. All three files green together: `27 passed`.
All 8 unit mutations re-run post-rebase: **8/8 OK**.

### The binary gate needed correcting — recorded, because the first version of it was luck

The first acceptance proof asserted the restored binary hashed **back** to the green baseline
(`64f470cb` → `517973c6` → `64f470cb`). That held, but it is not a sound gate: `vergen-gix` stamps
build metadata into `ocx`, so a rebuild is not guaranteed byte-identical, and a run of this harness
against a stale baseline duly reported all three cases BROKEN on that assertion alone while every
red and green half was correct.

The sound properties, and what the harness now gates:

1. the mutated binary differs from the baseline — proves a rebuild happened and the red run did not
   measure the green binary;
2. the restored binary differs from the **mutated** one — proves the green run did not measure the
   mutated binary;
3. the source file is restored byte-for-byte;
4. the green run passes on that binary.

Equality back to the baseline is a nice-to-have that this crate cannot promise. Worth carrying into
any other package that copies this harness.

### One harness bug worth naming

The first version aborted inside `build()` on a mutation that broke the compile (an unused import
under `-D warnings`) and left the mutation on disk — the exact "prove the restore landed" trap. The
next run's anchor gate caught it (`anchor hits=0`) rather than silently measuring a mutated tree.
The harness now restores on abort, and the D1 mutation is written to keep the import used so it
reverts *behaviour*, never the build.
