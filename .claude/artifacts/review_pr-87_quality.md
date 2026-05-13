# PR #87 Quality + CLI-UX Review

Scope: `feat(cli,oci): OCI referrers sign + verify (slice 1)` — diff `1fa82446..HEAD`.
Focus: Quality (SOLID/DRY/KISS/YAGNI, rust idiom, error/exit-code shape) + CLI-UX lens (flags, help, JSON envelope, exit-code surface).

---

## Verdict

**Needs Work (Warn-tier).** No Block-tier issues found — code compiles, tests are green per CI, sigstore stubs use `unimplemented!()` only on Phase 5c-blocked paths (documented). Exit-code taxonomy, error-message style, three-layer error model, and JSON envelope schema all comply with `quality-rust*.md` and the CLI-API rules. Several Warn-tier issues are worth fixing before Phase 5c lands.

Counts: **Block 0 / Warn 6 / Suggest 4 / Deferred 3**.

---

## Findings — Block

*(none)*

---

## Findings — Warn

### W1. [actionable] `unimplemented!()` reachable in shipped paths via `SignErrorKind::Internal` blocker
- File: `crates/ocx_cli/src/command/package_sign.rs:127-132`
- The CLI builds a string `"SignPipeline::run is Phase 5c blocked …"`, boxes it as a `Box<dyn Error>`, and surfaces `SignErrorKind::Internal(blocker)` → exit code 1 (Failure).
- `quality-rust.md` Block-tier rule: "`todo!()` / `unimplemented!()` in production paths — OK in stub phases, block-tier if reachable in released build." Today the CLI is not actually `unimplemented!()` (it returns a typed error), but `Internal` with an opaque string violates Spec/ADR error taxonomy: a user who hits this gets exit 1 with no actionable remediation, no distinct kind. Two cheaper-to-evolve options:
  1. Add a dedicated `SignErrorKind::NotImplemented` (exit 78 ConfigError or 75 TempFail) so the JSON envelope's `error.detail` is machine-actionable.
  2. Wire the pipeline before the slice merges to main.
- Remediation: introduce `SignErrorKind::NotImplemented { phase: &'static str }` mapped to `ExitCode::ConfigError` (78), document in ADR as Slice 1 sentinel, swap the blocker site to use it. Cheaper than option (2) and recovers the "stable contract" promise in the slice doc.

### W2. [actionable] Plain output uses a degenerate 2-column "Field/Value" layout — violates Printable single-table intent
- Files: `crates/ocx_cli/src/api/data/signature.rs:79-97`, `crates/ocx_cli/src/api/data/verification.rs:68-86`
- Rows are built as `[Vec<String>; 2]` where each column is a *parallel array per field*, i.e., row N's "column 0" holds the Nth label and row N's "column 1" holds the Nth value. This is not how `print_table` is intended (per `subsystem-cli-api.md` `paths.rs` / `env.rs` reference impls: one row per record, columns = fields). The output is N-wide table, not N-tall. Worst case: terminals wider than $WIDTH overflow horizontally; copy-paste loses field association. Compare `api/data/info.rs` and `paths.rs` for the canonical layout.
- Remediation: emit one row per `(label, value)` pair. Replace `rows[0].push(label); rows[1].push(value)` with row-major `Vec<Vec<String>>` and header `["Field", "Value"]`. Concrete change is ~6 lines per file.

### W3. [actionable] `report_signature` / `report_verification` are dead convenience wrappers
- File: `crates/ocx_cli/src/api.rs:67-75`
- Both methods only call `self.report(item)`. The doc comment claims "mirroring the per-type convention used elsewhere in this API layer" — but a quick grep shows no current callers, and the only callers in Phase 5c are still `unimplemented!()`. `#[allow(dead_code)]` papers over it now.
- This adds API surface without buying anything: every other command goes through `context.api().report(&data)?` directly. Per `quality-core.md` KISS / YAGNI: "no premature abstraction." If a future signing/verify type *needs* a special render path, add the wrapper then; today it's dead surface that drifts.
- Remediation: delete `report_signature` and `report_verification`; let Phase 5c callers do `context.api().report(&signature_report)?` like every other command.

### W4. [actionable] `SignatureReport`/`VerificationReport` fields excessive `.clone()` in plain printer
- Files: `crates/ocx_cli/src/api/data/signature.rs:81-95`, `verification.rs:70-83`
- `self.identifier.clone()`, `self.platform.clone()`, etc., for each printed row. Per `quality-rust.md` Warn-tier: "Unnecessary `.clone()` — clone to silence borrow checker masks design problem. Restructure ownership, pass refs."
- The `fields` array could hold `&str` references to the struct's owned strings; `oci::Digest::to_string` is fine. With the row-fix in W2 the cloning issue evaporates because each row is `vec![label.to_string(), value]` once.
- Remediation: combined with W2 — replace cloning with single allocation per row.

### W5. [actionable] `OfflineSignRefused` flag-handling lives in CLI before token resolution but documented as part of `SignErrorKind` — bypassable
- Files: `crates/ocx_cli/src/command/package_sign.rs:104-109` vs `crates/ocx_lib/src/oci/sign/error.rs:97-99`
- The CLI short-circuits when `context.is_offline()`, raising `SignErrorKind::OfflineSignRefused` BEFORE the pipeline is consulted. Good: the policy is enforced. Bad: the only place this check lives is the CLI binary. A library caller (mirror tool, future SDK, Bazel rule) that constructs `SignContext` directly will silently miss the check and trigger network calls inside `SignPipeline::run`. Per `quality-core.md` DIP / arch-principles "library owns invariant": the offline-refuse policy is a contract of the sign pipeline, not the CLI.
- Remediation: have `SignPipeline::run` (or `PackageManager::sign_one`) refuse offline mode at the lib boundary. CLI may still short-circuit for a faster path, but lib enforcement is the security/contract surface. Phase 5c blocker.

### W6. [actionable] Stub modules forward-declare `pub` types whose all consumers are unimplemented — extends API surface prematurely
- Files: `crates/ocx_lib/src/oci/sign.rs:42-46`, `verify.rs:24-27`
- `pub use bundle::SignedBundle`, `pub use pipeline::{SignContext, SignPipeline, SignResult}`, `pub use signer::{KeylessSigner, Signer}`, `pub use trust_root::TrustRoot`, etc. — all re-exported as stable lib API. Several have `unimplemented!()` bodies; some (`BundleBuilder`) are `pub(crate)` already. Re-exporting `SignContext`/`VerifyContext` couples Slice 2 to today's struct layout (a `pub struct` with `pub` fields).
- Per `quality-rust.md` Warn: "`pub(crate)` / `pub(super)` as design smell — control visibility through module nesting." But here the reverse problem applies: things that ARE crate-internal until Phase 5c are leaking through `pub use`. Concretely: rust API consumers can `use ocx_lib::oci::sign::SignPipeline` today and have it call `unimplemented!()` — bad shape contract.
- Remediation: keep `unimplemented!()`-bodied stub types `pub(crate)` until Phase 5c wires real impls. Only `SignError`/`SignErrorKind`/`VerifyError`/`VerifyErrorKind` and the option structs (`SignOptions`/`VerifyOptions`) need to be `pub` today — they are the surface the CLI binds against.

---

## Findings — Suggest

### S1. [actionable] CLI-UX: identifier positional + required `-p/--platform` flag mismatch with rest of `package` family
- File: `crates/ocx_cli/src/command/package_sign.rs:38`, `verify.rs:40`
- `--platform` is `required = true` and short `-p` (good — matches `package pull`, `package push`, `package test`, `find`, etc.). However `package describe` and `package info` take only the identifier with no `-p` because they operate on repository-level metadata. The sign+verify pair correctly mirrors `package push`/`test` — confirmed by `subsystem-cli-commands.md`. Suggestion: add a help-text snippet specifically calling out that omitting `--platform` exits 64 with a clap usage message (the same as `package push`).
- Remediation: noop for now; the contract is already correct. Mention in `website/src/docs/reference/command-line.md#package-sign` examples.

### S2. [actionable] Mismatch: `package sign` rejects `--offline` with `OfflineSignRefused` (77) but `package verify` rejects it with generic `OfflineMode` → `OfflineBlocked` (81)
- Files: `package_sign.rs:104-109`, `verify.rs:79`
- Sign: deliberate policy rejection (`PermissionDenied`, 77). Verify: passive network failure (`OfflineBlocked`, 81). Both are technically correct per their semantics — verify *could* be wired to support cache-hit offline in Slice 2. But the exit-code split surprises scripted consumers: a CI matrix that catches `case $? in 77|81)` works; one that conditions on `77` only will mis-handle verify.
- Remediation: document the asymmetry explicitly in `website/src/docs/in-depth/signing.md` and in the JSON envelope contract section. Phase 5c may add a `--require-online` flag to verify if the cache-hit path lands.

### S3. [actionable] `IdentityMatcher` / `IssuerMatcher` constructors take owned `String` — accept `impl Into<String>` instead
- File: `crates/ocx_lib/src/oci/verify/identity.rs:22-27, 42-47`
- Per `quality-rust.md` Suggest: "`impl Into<T>` parameters — accepts both `&str` and `String` without forcing alloc." Today CLI callers will pass owned strings from `clap` parsing, but tests and Phase 5c orchestrators will allocate fresh strings. Cheap ergonomic win.
- Remediation: change signature to `pub fn new(expected: impl Into<String>) -> Self` in both matchers.

### S4. [actionable] `Internal(Box<dyn Error + Send + Sync>)` in `SignErrorKind` swallows JSON envelope detail
- File: `crates/ocx_lib/src/oci/sign/error.rs:113-119`, render path `error_envelope.rs:215-234`
- The boxed inner error becomes opaque in the envelope: `context.detail` is unset, `message` is the outermost Display only. Per the ADR's frozen "fine-grained snake_case variant name for programmatic matching" goal, `Internal` users can't be distinguished by callers.
- Remediation: in `render_error_envelope` (when Phase 5c wires real Internal errors), set `detail = Some("internal")` consistently. Or: tag `Internal` variants with a `&'static str` discriminant (e.g., `Internal { kind: &'static str, source: Box<...> }`) so the envelope detail is meaningful.

---

## Findings — Deferred

### D1. [deferred] `pub` fields on `SignContext` / `VerifyContext`
- File: `crates/ocx_lib/src/oci/sign/pipeline.rs:34-57`, `verify/pipeline.rs:34-53`
- Every field is `pub` borrowed reference. Reason: human judgment needed on whether these structs are intended as a stable lib API or a temporary slice-1 wiring shape. If stable, fields should be private with constructor/builder; if temporary, mark `#[doc(hidden)]` or downgrade visibility per W6.

### D2. [deferred] `OCX_IDENTITY_TOKEN` credential exemption — token forwarded only via CLI process env, not via child env
- File: `crates/ocx_cli/src/command/package_sign.rs:208-213`; `subsystem-cli.md` lines 124-135
- Documented in subsystem rule as intentional. Reason: human judgment needed on whether the documented credential-exemption pattern scales when Slice 2 adds `--key-file` or HSM-based signers — same security model may apply, but each new credential type should be listed in the same table.

### D3. [deferred] Sigstore endpoint defaults hardcoded as string literals in CLI
- Files: `command/package_sign.rs:31-33`, `verify.rs:35`
- `DEFAULT_FULCIO_URL` / `DEFAULT_REKOR_URL` are `const &str` in two different files. Reason: human judgment needed on whether `OCX_FULCIO_URL` / `OCX_REKOR_URL` env-var support should land before Slice 2 (no current env-var wiring). Today the only way to override is the flag.

---

## Notes

- Error message style (lowercase, no trailing period, acronym retention) is compliant across `SignErrorKind` and `VerifyErrorKind`. Locked in by `sign_error_kind_display_rules` and `verify_error_kind_display_rules` tests.
- Exit-code mapping for both error families is exhaustive via `ClassifyErrorKind` trait (compile-time guarantee), and individual tests in `classify.rs` lock the public contract.
- JSON envelope shape passes byte-stable golden tests (`error_envelope.rs::tests`).
- No `.unwrap()` in library code; no `MutexGuard` across `.await`; no blocking I/O in async (verified by reading the diff).
- `sigstore_url.rs` SSRF guard is correctly placed at the CLI boundary, has comprehensive tests including IPv4-mapped-IPv6 / userinfo-bearing / scheme normalization edges.
