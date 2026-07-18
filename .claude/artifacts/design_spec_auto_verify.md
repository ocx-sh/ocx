# Design Spec — Auto-verify on install/pull (#99)

> Policy-gated automatic Sigstore verification at the metadata-first pull seam.
> Composes #98 (`resolve_tiered`), #194 (`VerifyPipeline::run`), #196 (trust-root
> cache / offline seam). Branch `wip/99-auto-verify`.

## Seam

`PackageManager::setup_impl` (`tasks/pull.rs`) is the single choke point every
package (root + transitive dep) passes through. The hook fires **immediately
after** `let resolved = mgr.resolve(...)` (manifest digest known) and **before**
the singleflight acquire + layer download. At that point resolve has only done
content-addressed blob-cache write-through (inert per issue); no layers, no
package assembly, no symlinks — so a fail-closed abort leaves no partial state.

## `PackageManager::verify_one` — single lib verify facade

```rust
pub struct VerifyOptions<'a> {
    pub policies: &'a [trust::CompiledPolicy], // effective ANY-of set (already resolved)
    pub transport: &'a dyn OciTransport,       // registry transport (present even --offline)
    pub trust_root: &'a TrustRoot,             // #196 material (Fulcio CA + opt Rekor key)
    pub rekor_url: &'a Url,
    pub offline: bool,                         // Sigstore-trust-services offline flag
    pub cache_root: &'a Path,                  // $OCX_HOME (capability + trust-root cache)
    pub no_cache: bool,
}

pub async fn verify_one(&self, package: &oci::Identifier, platform: &oci::Platform,
                        opts: VerifyOptions<'_>) -> Result<VerifyReport, PackageError>
```

Body: build `VerifyContext { identifier: package, platform, policies, no_cache,
transport: opts.transport, index: self.index(), trust_root, rekor_url, cache_root,
offline }` → `VerifyPipeline::run` → wrap `VerifyResult` in `VerifyReport`; map
`VerifyError` → `PackageError(package, Internal(crate::Error::Verify(box)))` so
the exit code (IdentityMismatch→77, RekorSetInvalid→65, TrustRootLoad→78, …)
survives the `InstallFailed` batch classifier (verified via
`PackageErrorKind::Internal → classify_error → crate::Error::Verify → VerifyError::classify`).

Uses `opts.transport` (not `self.client`) because verify reads the artifact +
signature referrer from the registry **even offline** (mirrors CLI
`verify_context`); the manager's own `client` is `None` under `--offline`.

## Policy-gating hook — `maybe_auto_verify`

Injected config on `PackageManager` (`with_auto_verify(Option<AutoVerify>)`,
peer of `with_patches`/`with_progress`; carried through `offline_view`):

```rust
struct AutoVerify {
    operator_policies: Vec<TrustPolicy>,  // config.toml (authoritative)
    project_policies:  Vec<TrustPolicy>,  // ocx.toml (empty for OCI-tier install/pull)
    registry_client:   oci::Client,       // always-available registry transport
    rekor_url:         Url,               // default public
    offline:           bool,
    cache_root:        PathBuf,
    tuf_root_env:      Option<PathBuf>,   // OCX_SIGSTORE_TUF_ROOT captured
    pem_root_env:      Option<PathBuf>,   // OCX_SIGSTORE_TRUST_ROOT captured
    user_opted_out:    bool,              // resolved --no-verify/OCX_NO_VERIFY (flag>env)
    trust_root:        Arc<OnceCell<TrustRoot>>, // memoized success (get_or_try_init)
    warned:            Arc<AtomicBool>,   // WARN-once per invocation (batch-shared)
}

async fn maybe_auto_verify(&self, resolved: &Identifier)
    -> Result<(), PackageErrorKind>
```

`resolved` is the platform-selected leaf digest (`ResolvedChain.pinned`), so
verification runs against `Platform::any()` — re-selecting the flat leaf with the
concrete platform strict-equality-fails against its advertised `any()`.

**Attached once, not per-command.** `with_auto_verify` is called in
`Context::try_init` (peer of `with_patches`), so the hook fires on EVERY install
surface — `install`/`pull` and every `find_or_install` path (`package exec`,
`package env`, `run`, patch discovery), not just the two commands carrying the
flag. install/pull refine the opt-out from `--verify`/`--no-verify` via
`conventions::manager_with_verify_flag`.

Logic per package:
1. `auto_verify` is `None` (no policies configured) → `Ok(())` (zero overhead/noise).
2. `resolve_tiered(operator, project, "registry/repo")`; malformed matched policy → exit 78.
3. Empty (no policy covers) → `INFO` "installing without signature verification" → `Ok(())`.
4. Covered + `user_opted_out` → `WARN` once (AtomicBool swap) → `Ok(())`.
5. Covered + verify on → resolve trust root (memoized, lazy — only when a policy
   actually matches, so a non-covered package never trips the offline gate) →
   `verify_one`. Failure → abort (fail-closed). Success → `Ok(())`.

Trust root lazily resolved via `get_or_try_init` so offline+no-material only
fails when a policy actually covers a package being installed — not at Context
build, not for uncovered packages.

## Shared trust-root resolver (DRY, security gate)

Extract the flag-less env→cache→embedded ladder + offline-rekor-key gate from the
CLI `verify.rs::resolve_trust_root` into
`oci::verify::resolve_trust_root(tuf_override, pem_override, cache_root,
rekor_cache_key, offline) -> Result<TrustRoot, VerifyErrorKind>` (async, tokio::fs).
Both the CLI verify command (flags → override) and auto-verify (env only) call it
— one copy of the offline gate. Safety net: existing `test_verify.py`.

## `--no-verify` / `OCX_NO_VERIFY` precedence (flag > env)

Reuse `options::Verify` (`--verify`/`--no-verify`, last-wins). New method:
```rust
fn resolve(&self, env_opt_out: bool) -> bool { // verification enabled?
    if self.no_verify { false } else if self.verify { true } else { !env_opt_out }
}
```
Command: `env_opt_out = env::flag(keys::OCX_NO_VERIFY, false)`;
`user_opted_out = !verify.resolve(env_opt_out)`. `env::keys::OCX_NO_VERIFY` +
`OcxConfigView.no_verify` (env-passthrough) forwarded by `apply_ocx_config`.

## Surfaces

Auto-verify is attached on the shared manager, so it gates **every** install
surface, gated by operator `config.toml` `[[trust.policy]]`:

- `ocx package install` / `ocx package pull` (carry the `--verify`/`--no-verify` flag).
- Every `find_or_install` path: `ocx package exec`, `ocx package env`, `ocx run`,
  `ocx env`, patch discovery. No flag — `OCX_NO_VERIFY` is the opt-out.

Project pool stays empty (no new OCI-tier `ocx.toml` carve-out).

## Limitations

- **Transitive deps** of a covered root are verified only if a policy also covers
  *their* scope (per-scope opt-in model). The hook fires per unique digest, so a
  covered dep IS verified; an uncovered dep of a covered root is not. Broadening
  to "cover the root ⇒ cover the closure" is a tracked follow-up.
- **Project-tier `ocx.toml` policies** are not read on OCI-tier surfaces (project
  pool empty). Wiring `project_policies` from the project prologue is a follow-up.

## Docs

`environment.md` (`OCX_NO_VERIFY`), user-guide (verify-by-default / auto-verify),
`command-line.md` (install/pull `--verify`/`--no-verify` + policy-gating + WARN).

## Acceptance (`test/tests/test_auto_verify.py`, fake stack)

policy-covered + valid sig → installs & auto-verifies; policy-covered + bad/absent
sig → aborts fail-closed before download, no store/symlink state, correct exit
code; no-policy → installs (no verify); `--no-verify`/`OCX_NO_VERIFY` on covered →
skip + single WARN, flag>env; offline + cached/pinned material (TUF root) → works.
