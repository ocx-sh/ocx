# Research: lock-time verification and libc variants

## Metadata

**Date:** 2026-11-18
**Lane:** codebase recon (`/hex-discuss`, `.agents/discussions/bazel-hermetic-package-bundles.md`)
**Basis:** ocx worktree `uno` at 5f1f5962
**Expires:** 2027-05-18
**Caveat:** findings only, no recommendation.

### Findings

**1. Signature verification at lock time**
- **No signature verification happens when `ocx.lock` is written.** `ocx lock` runs `resolve_lock_reconcile` (`crates/ocx_cli/src/command/lock.rs:63`). That calls `resolve_one` → `retry_fetch` + `fan_out_candidates` (`crates/ocx_project/src/resolve.rs:487-554`). Both only fetch manifests through the index. `ocx_project` has no call into `ocx_sign` or `AutoVerify` (grep of `crates/ocx_project/src`: verify hits are consent-related only).
- Auto-verify has exactly one caller on the install path: `setup_impl` → `maybe_auto_verify_requested` (`crates/ocx_package_manager/src/tasks/pull.rs:255`). It runs after `resolve`, as singleflight leader, **before any layer download**. Its own doc says an install path that bypasses `setup_impl` installs unverified (`auto_verify.rs:7`).
- `AutoVerify` is attached once on the shared manager in `Context::try_init` (`crates/ocx_cli/src/app/context.rs:471-485`). Every command that reaches `setup_impl` therefore inherits it: install, pull, exec, env, patch.
- `ocx lock` and `ocx add` **do download** by default. After the commit they call `materialize_lock` (`lock.rs:109`; `crates/ocx_cli/src/app/project_context.rs:346-367`) → `pull_or_prepare`, which pulls the entries whose pin changed. That path hits auto-verify, but only after `ocx.lock` is already committed (`lock.rs:108` comment: a failed download leaves the lock committed). It does not gate the write.
- `--no-pull` skips the download, so lock then runs no verification at all (`lock.rs:76`, `project_context.rs:354`). A deferred (lazy) tool is prepared, not pulled, so it is also unverified at that point.
- Uncovered package: when no policy or publisher declaration covers it, the install proceeds with only a `log::info!` (`auto_verify.rs:315-322`, returns `Ok(false)`).
- Opt-out: `--no-verify`/`--verify` come from `SignatureVerify` (`options/verify.rs:44-59`). The flag outranks `OCX_NO_VERIFY` (`conventions.rs:424-434`). On a covered package an opt-out only logs a warn-once (`auto_verify.rs:325-327`).
- A system-config `signatures = "required"` registry refuses the opt-out (`refuse_locked_opt_out`, `auto_verify.rs:162`).
- A request naming a digest, which includes lock pins, reads only the committed index root for publisher declarations (`committed_only`, `auto_verify.rs:240-253`). `lock-and-reproduce.mdx:46` says a fresh home with no committed root treats the package as undeclared and installs without the publisher check.
- Hook point if lock-time verification were wanted: between `fan_out_candidates` (`resolve.rs:534`) and `LockedTool` assembly (`resolve.rs:510`). Alternatively in `lock.rs` before `commit_and_render` (`lock.rs:86`). Neither exists today.

**2. What `ocx.lock` pins**
- Each `[tool.platforms]` value is a **leaf platform-manifest digest**, not an image-index digest. `LockedTool` says "one leaf digest per shipped platform" (`crates/ocx_project/src/lock.rs:128`, `:146-149`).
- Resolution from the tag: the tag resolves to an image index. `fetch_candidates` fans out to its children, each pinned to its leaf digest (`resolve.rs:539-589`), keyed by `Platform::to_string()`. The index digest is discarded: "the next push orphans it; only leaves are durable" (`resolve.rs:527`). A flat image yields key `"any"` (`resolve.rs:486`).
- The dependency closure is **not pinned by `ocx.lock`**. A lock entry holds only `name`, `group`, `repository`, `declared` (V4 only) and `platforms` (`lock.rs:133-150`). Dependencies are pinned inside each package's own metadata as manifest digests (`crates/ocx_package/src/dependency_pinning.rs:6`). That is authoring-time pinning, so the closure is covered transitively through the leaf manifest/config blob digest.

**3. libc / ABI variants**
- Selection happens **at use time on the host**, not at lock time. Lock time records every leaf, including all libc variants (`resolve.rs:577-589`). Reading a lock calls `lookup_host_leaf` → `select_best(host, candidates)` (`resolve.rs:604-617`, `lock.rs:160-161`). `Platform::current()` fills `os_features` from `HostCapabilities` (`crates/ocx_oci/src/platform.rs:182-190`).
- Lock representation: one key per variant, with features in the key. Examples from the repo root `ocx.lock:103-104`:
  - `"linux/amd64+libc.glibc" = "sha256:00d9…"`
  - `"linux/amd64+libc.musl" = "sha256:922e…"`
- Grammar (`platform.rs:366-417`, `options/platform.rs:10`): `os/arch[/variant][+feature[,feature...]]` or `any`. Features are sorted and deduped on normalize (`platform.rs:421`). A raw `+` always introduces the feature list. Lock keys must be the canonical spelling or the lock is rejected (`lock.rs:527`, `validate_canonical_platform_keys`). Valid os/arch pairs come from `SUPPORTED_PAIRS`.
- `-p`/`--platform` takes the same string: `linux/amd64+libc.musl` or `linux/amd64+libc.glibc` (`options/platform.rs:10-14`). The flag is single-valued.
- Matching: `is_compatible(required, offered)` requires equal os and arch, and the offer's variant, if declared, to match. It also requires `offered.os_features ⊆ required.os_features` (`platform.rs:241-276`). `compatibility_score` = `(is_specific, variant_matched, os_features.len())`, higher wins (`:282`). A tie yields `Selection::Ambiguous` (`:327`). Lock-read maps that to `AmbiguousHostLeaf`, distinct from `NoHostLeaf` (`lock.rs:157-159`, `error.rs:237`).
- A host advertising both libcs has `os_features = [libc.glibc, libc.musl]` (`subsystem-oci.md` "Host detection"). Both offers are then compatible with equal feature counts, so the result is `AmbiguousHostLeaf`. I did not run it. This is derived from the code. Check `crates/ocx_project/src/resolve.rs` tests, or run it, to confirm before relying on it.
- An offer with empty `os_features` matches any host. The `+libc.*` suffix only restricts an offer.
- `-p` on `ocx lock` only selects which platform's leaf is pulled eagerly and rendered (`lock.rs:83`). The lock still records all platforms.

**4. Repo locks**
- Root `ocx.lock` has libc variants. `pnpm` (`:101-107`) and `python` (`:127-134`) carry both `+libc.glibc` and `+libc.musl` for `linux/amd64`. `python` also carries both for `linux/arm64`. `pnpm` has only `linux/arm64+libc.glibc`. Other tools carry glibc only, for example lines 30-31, 42-43, 91-92 and 130-133.
- `test/ocx.lock` has no `libc` entries (grep count 0).
- Parse fixture `crates/ocx_project/src/testdata/repo.ocx.lock:30-54` also carries `+libc.glibc` and `+libc.musl` keys.

### negative:
- No lock-time signature verification exists, and no flag or env var adds it.
- `ocx.lock` does not record signer identity, signature digests or trust-policy state. A lock-pinned digest verified later depends on the machine's policy at pull time.
- `ocx lock --check` is explicitly offline and verifies nothing (`lock.rs:26-35`).
- No lock key means "any libc". A host without libc detection (empty `os_features`) matches only offers with empty features, so musl/glibc-only leaves are `NoHostLeaf`.

### leads:
- `crates/ocx_package_manager/src/tasks/signer_rounds.rs` — `signer_rounds_for_pinned`, the digest-request policy lookup a lock-time hook would reuse.
- `.claude/artifacts/adr_platform_model_unification.md` — D1 relation and D3 lock doctrine.
- `.claude/artifacts/adr_platform_libc_os_features.md` — host-detection details.
- `website/src/content/docs/guides/verify-installs.mdx` — user-facing auto-verify contract.
- `crates/ocx_project/src/lock.rs:1380-1430` — lock tests for key canonicalization.
- Not checked: the exact `ocx add` / `ocx update` / `ocx pull` command files. They share `materialize_lock` via `app/project_context.rs` per the CLI rules, but I did not read each one.
