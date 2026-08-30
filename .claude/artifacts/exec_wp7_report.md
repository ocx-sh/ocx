# WP-7 execution report — fork dead code ([#368](https://github.com/ocx-sh/ocx/issues/368))

Plan: [`plan_issue_sweep_2026-08-30.md`](./plan_issue_sweep_2026-08-30.md), contracts **C-060**, **C-061**.
Worktree: `/home/mherwig/dev/ocx/.agents/worktrees/isw-wp7`, branch `hex/issue-sweep--wp7`, base `52eecc15`.

| Contract | Verdict |
|---|---|
| C-060 — delete `pull_referrers` + `pull_referrers_via_tag_schema` | **DONE** |
| C-061 — three stale artifact claims | **DONE** |

---

## C-060 — DONE

### Resolved dependency graph inside the fork

`external/rust-oci-client/Cargo.lock` is gitignored (`.gitignore:2:Cargo.lock`), so nothing
about the fork's own resolution ships. Re-resolved before any measurement:

```
$ cargo update
    Locking 429 packages to latest compatible versions
exit=0
```

Resolved versions asserted from the regenerated lock:

| crate | version |
|---|---|
| oci-client (this crate) | 0.17.0 |
| reqwest | 0.13.4 |
| tokio | 1.53.1 |
| hyper | 1.11.1 |
| http | 1.5.0 |
| serde_json | 1.0.151 |
| testcontainers | 0.27.3 |

### Zero non-test call sites — the actual commands

Word-precise needle: `pull_referrers` **not** followed by `_` or an alphanumeric, so
`pull_referrers_native` (which is live) cannot be mistaken for a hit. Run from the outer
worktree root, so it covers `crates/**` **and** the submodule, tracked and untracked alike
(plain `grep -rn`, not `git grep`):

```
$ cd /home/mherwig/dev/ocx/.agents/worktrees/isw-wp7
$ grep -rn --binary-files=without-match "pull_referrers[^_a-zA-Z0-9]" . \
      --exclude-dir=.git --exclude-dir=target --exclude=client.rs
exit=1        # no match anywhere outside the defining file
```

The only hits in the whole tree — including `.claude/artifacts/**` — were documentation
prose, listed and dispositioned below. There was **no** hit in `crates/**`, none in
`external/rust-oci-client/tests/**`, none in `examples/`, `benches/` or any untracked file.

The broad (non-word-precise) grep, for completeness, showed what *does* live in `crates/**`:

```
$ grep -rn "pull_referrers" . --exclude-dir=.git --exclude-dir=target --exclude=client.rs
./external/rust-oci-client/tests/referrers_bounds.rs:113,127,141   → pull_referrers_native
./crates/ocx_lib/src/oci/client/native_transport.rs:695,698        → pull_referrers_native
(remainder: .claude/artifacts/*.md prose)
```

Inside `client.rs` itself the only callers of `pull_referrers` were the fallback dispatch at
`:2198`, the two `#[cfg(feature = "test-registry")]` tests, and the two intra-doc links in
`pull_referrers_native`'s doc comment.

### Why the code is genuinely superseded, not merely uncalled

`NativeTransport::list_referrers` (`crates/ocx_lib/src/oci/client/native_transport.rs:680`, call at `:698`) uses `pull_referrers_native`
**deliberately** — its `Ok(None)` on a `404` is the `ReferrersUnsupported` capability verdict
that keeps exit 84 apart from exit 79. The OCI referrers tag-schema fallback the deleted
function provided is owned by OCX at `crates/ocx_lib/src/oci/client/transport.rs:578`
(`list_referrers_with_fallback`) and `:627` (`pull_referrer_fallback_index`).

One artifact appears to contradict this and does not: `design_spec_cosign_parity.md:162`
("Switch to the fallback-capable `pull_referrers`") and `meta-plan_cosign_parity.md:194` are
the *pre-implementation* design for what shipped as
[PR #369](https://github.com/ocx-sh/ocx/pull/369). What actually shipped put the fallback in
the OCX transport layer instead, which is why `native_transport.rs` still calls the native-only
form. No in-flight work package plans to re-adopt the fork's version.

### Deleted

In `external/rust-oci-client/src/client.rs` (the only file touched in the submodule):

| Item | Reason |
|---|---|
| `Client::pull_referrers` (was `:2154-2207`) | the contract's target; zero callers |
| `Client::pull_referrers_via_tag_schema` (was `:2256-2328`) | private; only caller was `pull_referrers` |
| `test_pull_referrers_with_tag_schema_fallback`, `test_pull_referrers_no_tag_schema` | the two `#[cfg(feature = "test-registry")]` tests |
| two intra-doc links at `:2212`, `:2218` | reworded, not just unlinked — the surrounding sentences explained the *contrast* with the deleted function |
| `empty_image_index` (was `:2852-2861`) | **consequence**: its only caller was the deleted fallback |
| test helper `push_minimal_manifest` (was `:5579-5623`) | **consequence**: its only callers were the two deleted tests |

`registry_image()`, `read_body_bounded`, `MAX_REFERRERS_INDEX_BYTES`,
`MAX_REFERRERS_DESCRIPTORS`, `to_v2_referrers_url` and `validate_registry_response` all keep
other callers and stay.

Diff: `1 file changed, 8 insertions(+), 410 deletions(-)`.

### Gates

| Gate | Command | Exit |
|---|---|---|
| format | `cargo fmt --check` (in fork) | 0 |
| build | `cargo build --features test-registry` | 0, **0 warnings** |
| compile test targets | `cargo check --features test-registry --all-targets` | 0, **0 warnings** |
| test | `cargo test` (unfiltered) | 0 — 137 passed, 0 failed, 1 ignored across 8 binaries (84/16/14/11/6/3/3/1) |
| outer tree | `task rust:verify --force` in the worktree | 0 — **6423 passed, 8 skipped** |

`cargo` was invoked as `/home/mherwig/.cargo/bin/cargo` for the warning-sensitive runs: the
`rtk` hook rewrites `cargo build`/`cargo check` and its summary line (`cargo build (0 crates
compiled)`) replaces the compiler output, so a warning captured through it is invisible.

### Red-proof for the zero-warning claim

A zero-warning build is only evidence that the two *consequential* deletions were required if
`dead_code` can actually fire here. Proven, at both levels:

```
$ # probes added: one private fn at lib module level, one inside `#[cfg(test)] mod test`
$ /home/mherwig/.cargo/bin/cargo check --features test-registry --all-targets --message-format=short
src/client.rs:2722:4: warning: function `wp7libdeadprobe` is never used
warning: `oci-client` (lib) generated 1 warning
src/client.rs:3455:14: warning: function `wp7testdeadprobe` is never used
warning: `oci-client` (lib test) generated 2 warnings (1 duplicate)
```

Probes then removed; `cmp src/client.rs <post-deletion backup>` → identical, and the re-run is
green with 0 warnings.

**Trap worth recording** (cost four wasted rounds): the first probes were named
`__wp7_dead_code_probe` / `__wp7_lib_dead_probe`. rustc suppresses `dead_code` for any
identifier starting with `_`, so both came back green — including under
`RUSTFLAGS="-D dead_code"` with a full 429-crate rebuild. **A leading-underscore probe is a
red-proof that can never go red.** Renaming to `wp7libdeadprobe` / `wp7testdeadprobe` reds
immediately.

Separately: `cargo build` alone does **not** compile `#[cfg(test)] mod test`, so it could not
have caught a leftover `push_minimal_manifest`. `--all-targets` does; proven by making the
probe body reference a nonexistent symbol, which failed with
`error: could not compile 'oci-client' (lib test)`.

---

## C-061 — DONE

Three claims corrected, all in the **outer** repo:

| File:line | Was | Now |
|---|---|---|
| `discover_referrers_architecture_map.md:205` | "`NativeTransport` delegates to `self.client.pull_referrers(…)`" | delegates to `pull_referrers_native`; `Ok(None)` on `404` is the `ReferrersUnsupported` verdict; tag-schema fallback owned by `OciTransport::list_referrers_with_fallback` |
| `discover_referrers_architecture_map.md:260` | flow arrow `→ NativeTransport: self.client.pull_referrers(…)` | `→ NativeTransport: self.client.pull_referrers_native(…)` |
| `discover_oci_client_extension_points.md:98` | "Native impl: `self.client.pull_referrers(…)` → map errors via `registry_error()`" | `pull_referrers_native`, with `Ok(None)` → `ClientError::ReferrersUnsupported`; fallback owned by OCX |

---

## Deliberately not changed

- **`discover_referrers_architecture_map.md:42,44,47,54,173,242`** still name `pull_referrers`
  and cite the long-stale `client.rs:1659`. I first left all six as a dated snapshot; that was
  wrong for `:44-52` (a heading plus the full signature in a code block) and `:242` (a
  reference-table row) — a lookup table naming a symbol that no longer exists at a line that no
  longer resolves reads as authoritative in a way a "we decided X in Phase 1" narrative does
  not. Fixed cheaply instead of six times: a dated **stale symbol notice** now sits under the
  document's H1, naming the deletion, the live `pull_referrers_native` path and the OCX-side
  fallback, and marking every line number below as a Phase-1 snapshot. The six lines are left
  in place as history.
- Every other `.claude/artifacts/*.md` mention (`design_spec_cosign_parity.md`,
  `meta-plan_cosign_parity.md`, `prd_oci_referrers_discovery.md`, `adr_*`, `review_r1_*`,
  `pr_faq_*`, `plan_issue194_*`) is likewise a dated design or discovery record.
- No `CHANGELOG.md` edit. Neither commit is user-observable, so both are `chore:`.

## Deferrals / to report, not filed

- **Nothing needs an ocx `fork — …` issue from this package.** No latent fork defect surfaced.
- One near-miss worth naming: the now-deleted `pull_referrers` read its response body with an
  unbounded `res.bytes()`, while the surviving `pull_referrers_native` uses
  `read_body_bounded(…, MAX_REFERRERS_INDEX_BYTES)`. The deletion removes that asymmetry;
  no separate fix is needed.

## Commits

| Repo | Commit | Subject |
|---|---|---|
| `external/rust-oci-client` (branch `ocx/drop-dead-referrers-fallback`, off `ocx/integration`) | `e5ed433a` | `refactor(client)!: drop pull_referrers and the tag-schema fallback` |
| outer (branch `hex/issue-sweep--wp7`) | `d9028adf` | `chore(deps): bump the oci-client fork past the dead pull_referrers removal` |
| outer | `a0055d6d` | `chore(claude): correct three artifact claims about the referrers call` (also carries the stale-symbol banner) |

The fork branch was created off `origin/ocx/integration` (`609d3f7`) as instructed, which is
**one merge commit ahead** of the previously pinned gitlink `21ded5e`. Verified that no other
fork change rides along on the bump: `git rev-parse 21ded5e^{tree}` and
`git rev-parse 609d3f7^{tree}` are both `253279661d64c52033e684f6e1a27de4a69757c5` — byte-identical
trees. (`git diff` output is swallowed by the local proxy, so tree-object comparison was used
instead of an empty diff.)

Neither repo was pushed. The gitlink points at a local-only fork commit until the fork branch
is pushed and its PR is opened against `ocx/integration`.
