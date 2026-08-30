# WP-2 execution report — operator-typed file reads

Branch `hex/issue-sweep--wp2`, rebased onto `goat` at `037ad948` (WP-6 and WP-1
merged). Issues #370, #371.
Contracts C-010, C-010a, C-011, C-012, C-013, C-014; scenarios S-005, S-006, S-021.

## Contract outcomes

- **C-010 — DONE.** Exactly three reads in `resolve_trust_root` (`:71`, `:91`, `:101` at
  base), verified by hand before editing: `:106`/`:107` are the `match` arms of the read at
  `:101`, not reads. All three now go through
  `utility::fs::read_bounded` on the blocking pool, via one local
  `read_trusted_root(path)` helper. `trust_resolve.rs`.
- **C-010a — DONE.** Cap named explicitly: `MAX_TRUSTED_ROOT_BYTES = 1024 * 1024`, sited in
  `trust_resolve.rs` with the sizing rationale (~50x the public-good document; the same
  ceiling `MAX_SIGSTORE_RESPONSE_BYTES` puts on the same document over the network, so the
  transport does not change how large a trust root may be). Rung 4's fall-through is
  `Err(BoundedReadError::Io { source, .. }) if source.kind() == NotFound => {}` and nothing
  else; `TooLarge` and `NotRegularFile` fall to `Err(error) => return Err(read_err(error))`.
  No wildcard arm. A `JoinError` is mapped to `Io` carrying `ErrorKind::Other`, never
  `NotFound`, so a panicking pool task cannot be mistaken for an absent file.
- **C-011 — DONE, one mechanism deviation, recorded below.**
  - `--tags-file`: `package_announce.rs:130`'s second, unbounded copy of the read is gone.
    Both it and `TagsOpt::resolve` now call one `options::tags::read_tags_file`, which is
    `TagsOpt`'s existing bounded read (`MAX_TAGS_FILE_BYTES`, 128 KiB) lifted out of the
    method. `options/tags.rs` is one file beyond the declared scope; no other wave-1 WP
    declares it. Its existing bound tests exercise the extracted function unchanged.
  - `--identity-token-file`: **both of `read_bounded`'s guards applied, but not by calling
    `read_bounded`.** A literal call would regress three security properties, all of them
    load-bearing at that site and all documented in the code it replaces: it takes a
    **path**, so it re-`open`s a name the `O_NOFOLLOW` open and the uid/mode gate already
    validated (CWE-367 — the comment at `:141-147` says the handle is threaded through
    `spawn_blocking` precisely so the checks bind to what is read), it carries no
    `O_NOFOLLOW`, and it returns a plain `Vec<u8>`, landing the token cleartext outside the
    `Zeroizing` the comment at `:210-211` exists for. Instead: `!meta.is_file()` refusal on
    the already-validated handle (the regular-file guard) and
    `take(MAX_IDENTITY_TOKEN_BYTES + 1)` on it (the ceiling, 64 KiB, matching
    `MAX_KEY_PEM_BYTES` — the other credential file an operator names). Both failures map
    through `error::file_error` to exit 74, which is what the contract asks for.
- **C-011, third caller — DONE (scope extended by the lead after the first report).**
  `package_push.rs:601` `append_to_tags_file` read the existing tags file with a bare
  `tokio::fs::read` on an operator-typed path: the same defect class, on `push`'s write
  path where the file is read back before merging. Confirmed as genuinely the same defect
  and not a C-011-style exception — nothing has validated a handle first, there is no
  credential to zeroize, and the path is not reopened, so the shared reader is simply
  correct here.

  One difference, and it is the whole design: absence is **not** an error, because push
  creates the file. So the reader grew a second door rather than a flag —
  `read_tags_file_if_present`, sharing `read_tags_bytes` and `tags_file_error` with
  `read_tags_file`, with the absence arm written the same way rung 4's is: `Io` whose kind
  is `NotFound`, and nothing else. A wildcard there would let an oversized or non-regular
  file read as "no tags yet" and push would overwrite the operator's tag list with just
  this run's tags.

  Side effect worth naming: the pre-existing `push--tags-file` row (an unwritable
  destination, parent is a regular file) now fails at the *read* — `ENOTDIR`, not
  `NotFound` — rather than at the write. Same exit 74, same needle, and the row still
  passes.

- **C-012 — DONE.** `TrustRootLoadReason::TrustRootUnreadable`, classified `ExitCode::IoError`
  (74) ahead of the `TrustRootLoad(_) => ConfigError` arm. Covers all three operator doors
  *and* the convention path when present-but-unusable — missing, permission denied, not a
  regular file, and past the cap alike. The single-variant choice is deliberate: S-006 pins
  an oversized file at 74, so splitting missing from over-cap would mint a second variant
  answering the same code with the same slug.
- **C-013 — DONE.** `AssetReadFailed` keeps `ConfigError` (78) and its `trust_root_load`
  slug. Its two remaining sites are `trust_root.rs:141` (`load_embedded`, TUF fetch) and
  `pipeline.rs:1529` (`Verifier::new`) — read while confirming, neither opens a file.
  Pinned by name in `an_unreadable_trust_root_file_maps_to_io_error_while_the_tuf_sites_keep_config_error`.
- **C-014 — DONE.** New slug `trust_root_unreadable`, following the `key_unreadable`
  precedent (Open Question 2, resolved as directed). The other six `TrustRootLoadReason`
  variants still answer `trust_root_load` through the untouched wildcard. The frozen table
  at `verify/error.rs` gains a row and its count moves 45 → 46, which is the reviewable line
  of diff that pin exists to force.

Beyond the contracts, two corrections the work surfaced:

- `TrustRootUnreadable` interpolates its `#[source]`. `VerifyErrorKind::TrustRootLoad`
  renders its reason's `Display` and stops, so the first shipped form reached stderr as
  "trust-root file could not be read" — exit 74 and no path. Found by the acceptance row's
  needle assertion, which is what that leg is for. `AssetReadFailed` has the same shape and
  is left alone (see Deferrals).
- `trust_root_load_maps_to_config_error`'s comment claimed "every variant" over a list
  missing two (`NoCtLogKey`, `AmbiguousTrustRootConfig`). Both added; the claim is now true.

## Red/green proofs

Every mutation was confirmed present before the run and confirmed gone after, by grep on the
mutated token — and, for the four-file restore, by asserting the *original* text back.

### Unit (`cargo test -p ocx_lib --lib`)

| # | Mutation | Result |
|---|---|---|
| — | none (green baseline) | `trust_resolve`: exit **0**, 10 passed |
| 1 | rung 4's two arms → `Err(_mutant_wildcard) => {}` | exit **101**, 1 failed: `a convention path that is not a regular file must fail, not fall through, got Err(TrustRootLoad(OfflineTrustMaterialUnavailable))` — the masquerade C-010a names, reproduced |
| 2 | `read_trusted_root` → `tokio::fs::read` | exit **101**, 2 failed, both `got Err(TrustRootLoad(PemParseFailed …))` — the unbounded read succeeded and the parser refused |
| 3 | `TrustRootUnreadable => ExitCode::ConfigError` | exit **101**, 1 failed |
| 4 | slug → `"trust_root_load"` | exit **101**, 2 failed (the new test *and* the frozen table) |
| — | restored | exit **0**, 244 passed (`oci::verify`) |

Mutation 1's first spelling used a `SCREAMING` binding name and killed the build on
`-D non-snake-case` instead of the test; renamed and re-run, per the known trap.

### Acceptance (`test/tests/test_exit_codes.py`, rebuilt `test/bin/ocx` each time)

| # | Mutation | Result |
|---|---|---|
| — | none (green baseline) | exit **0**, 24 passed, 1 xfailed |
| A2 | `TrustRootUnreadable => ExitCode::ConfigError`, message untouched | exit **1**, 3 failed / 2 passed. All three fail on the **code** assertion — `expected exactly 74, got 78` — with needles intact, so the 74 pin is what went red, not the needle |
| B | all three reads reverted to unbounded, classification kept | exit **1**, 4 failed / 1 passed: `verify--trusted-root-big` (parse error instead of the cap), `sign--identity-token-file-big` and `announce--tags-file-big` (needle absent — the oversized file was accepted), `character_device` (empty read → parse error at 78). `verify--trusted-root` stays green, correctly: a missing file is missing either way |
| — | restored | exit **0**, 24 passed, 1 xfailed |
| — | push fix, green baseline | exit **0**, 28 passed, 1 xfailed (`test_exit_codes` + `test_announce_push_file`) |
| P1 | `read_tags_file_if_present` → unbounded `tokio::fs::read` | exit **1**, 1 failed: `push--tags-file-big` only, on the needle — the oversized file was accepted and the push succeeded |
| P2 | absence arm → `Err(_mutant) => Ok(Vec::new())` | exit **1**, 1 failed: `push--tags-file-big` only. The oversized file read as "no tags yet", which is the silent overwrite the arm exists to stop |
| P3 | `tags_file_error` → untyped `anyhow!` | exit **1**, **6** failed — every tags-file row, on `assert 1 != 1` with needles intact. This is the mutation that pins 74 for this family |
| — | restored | exit **0**, 43 passed, 1 xfailed (`test_exit_codes` + `test_announce_push_file` + `test_push`) |

P3's first two spellings did **not** land — a stale needle after `cargo fmt` joined two
lines — and the run that followed used the previous mutation's binary. The
`grep -c 'MUTANT P3'` gate is what caught it; the third spelling landed and was re-run.
The absence half of the push arm is held by an existing test I did not write,
`test_push_tags_file_feeds_announce_tags_file_union`, which pushes to a tags file that does
not exist yet and reads its contents back.

**Which mutation pins 74, said plainly.** A first attempt (A) reverted the *classification
site* in `trust_resolve.rs`, which also removed the path from the message; all three rows
then red on the **needle** — "the run failed before it ever reached the file" — before ever
evaluating the exit code. That is a red proving something other than the property under
test, and on its own it would have shipped the 74 pin unproved. **A2 is the mutation that
pins 74 for the trust-root family**: it changes only `exit_code()`, leaves the message
alone, and reds all three on `expected exactly 74, got 78`. **P3 is the mutation that pins
74 for the tags-file family** — it untypes `tags_file_error`, and all six tags-file rows red
on `assert 1 != 1`, the exit-1-`internal` leg, needles intact. P1 and P2 red only on the
needle, because under them the read *succeeds*; they pin the bound and the absence-only
arm, not the code.

This is the failure this repo keeps re-learning, and it is recorded here rather than in a
commit message so the next reader of this artifact meets it: a red is evidence only for the
assertion that actually fired.

`/dev/null`, not the reported `/dev/zero`, in the character-device test: same class, same
guard, but `/dev/zero`'s pre-fix behaviour is unbounded allocation, which is not a state a
suite may enter to prove a point. Stated in the test's own docstring.

### Gates

- `task rust:verify --force` (from the worktree, redirected to a log), after the rebase and
  the push fix: exit **0** — 6432 tests run, 6432 passed, 8 skipped.
- `cargo clippy --workspace --all-targets --all-features`: exit **0**, no issues.
- Affected acceptance suites (`test_verify`, `test_sign`, `test_offline_verify`,
  `test_trust_root_distribution`, `test_announce`, `test_trust_policy`): exit **0**,
  140 passed, 1 xfailed. No existing test asserted 78 for an unreadable trusted-root file.
- `uv run ruff check .` under WP-6's landed config (`test/pyproject.toml`, ruff's own
  default rule set, floor derived from `requires-python = ">=3.13"`): exit **0**, all checks
  passed. The pre-rebase estimate was made against a guessed rule set; this is the real gate.

## Deferrals

1. ~~`package_push.rs:601`~~ — **no longer deferred.** Scope extended by the lead; fixed,
   tested and committed here (`fix(push): bound the tags-file read …`). No instance of this
   defect class is left open in the sweep.
2. **`AssetReadFailed` names nothing either.** `#[error("trust-root asset read failed")]`
   swallows a source that says *why* the TUF fetch failed, so exit 78 arrives with no
   reason. One-line fix — `#[error("trust-root asset read failed: {source}")]` — deliberately
   **not** applied: it is outside every contract here, and its two sites are a network TUF
   failure and an unusable assembled root, neither of which I can drive red without a
   fixture the suite does not have (`trust_root.rs` records the TUF fetch as untestable by
   design, TEST-07). Shipping an untested message change is the unchecked green the rules
   name.
3. **`options/tags.rs` is one file beyond the declared scope**, taken deliberately: the
   alternative was a second copy of a bounded read on an operator-typed path, which is the
   history `bounded_read.rs`'s own module doc records as the reason the helper exists. No
   other wave-1 WP declares that file.
4. **`website/src/docs/reference/command-line.md` was not in WP-2's expected-files list.**
   Two of its tables stated 78 for every trust-root failure and became stale the moment
   C-012 landed — Block-tier under `quality-cli-help.md`. Corrected in its own `docs:`
   commit. WP-4 owns `environment.md`; these are different files.
5. **`pipeline.rs` untouched**, as instructed. No contract here needed it — C-013 only
   required reading `:1529` to confirm it is not a file read, which it is not.

## Commits

Rebased onto `goat` at `037ad948`, clean, no conflicts.

```
62c8fdee fix(push): bound the tags-file read `--tags-file` does before merging
f7d81b6f chore(claude): record the WP-2 operator-typed-reads execution report
9c97a89a docs(reference): document exit 74 for an unreadable trusted-root path
100764d7 test(verify): pin exit 74 for every unreadable operator-typed path
dcd4e81c fix(verify): name the unreadable trusted-root file in the refusal
d9dc444d fix(verify): an unreadable trusted-root path exits 74, not 78
d9b08f7c fix(verify): bound every operator-typed trust-root, tags and token file read
```
