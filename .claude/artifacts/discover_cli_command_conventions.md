# CLI Command Conventions: Phase 1 Discovery

Scope: `crates/ocx_cli/src/**`. Documents how new subcommands are added so `ocx verify` / `ocx sbom` can slot in idiomatically.

## 1. Command Registration

**Location:** `crates/ocx_cli/src/command.rs:40–75` — single clap `Subcommand` enum.

Each subcommand gets one file at `crates/ocx_cli/src/command/{name}.rs`. Nested subcommands (e.g., `ci`, `shell`, `package`, `index`) use `#[command(subcommand)]`; flat commands are direct struct variants.

Module declarations (`pub mod verify;`, `pub mod sbom;`) go in `command.rs` alongside existing `pub mod info;` etc.

## 2. Command Body Anatomy

Canonical pattern (adapted from existing commands like `package_info`):

```rust
pub struct Verify {
    // flags FIRST (see §7)
    #[clap(short, long)]
    pub insecure: bool,
    // positionals LAST
    pub reference: options::Identifier,
}

impl Verify {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let identifier = self.reference.transform(context.default_registry());
        let client = context.remote_client()?;          // err on --offline
        let pinned = context.default_index().select(&identifier, ...).await?;
        let referrers = client.list_referrers(&pinned, Some(SIG_ARTIFACT_TYPE)).await?;
        let report = VerifyReport::from_referrers(referrers);
        context.api().report(&report)?;
        Ok(ExitCode::SUCCESS)
    }
}
```

Invariant: report data derives from task *results*, not from CLI arguments — preserves ordering and prevents drift.

## 3. Typed Exit Codes

**Location:** `crates/ocx_lib/src/cli/exit_code.rs:11–55`

```rust
#[repr(u8)]
#[non_exhaustive]
pub enum ExitCode {
    Success = 0,
    Failure = 1,
    UsageError = 64,
    DataError = 65,
    Unavailable = 69,
    IoError = 74,
    TempFail = 75,
    PermissionDenied = 77,
    ConfigError = 78,
    NotFound = 79,
    AuthError = 80,
    OfflineBlocked = 81,
}
```

BSD sysexits.h (64–78) + OCX-specific (79–81). Error → code mapping lives in `crates/ocx_lib/src/cli/classify.rs` (trait-based `ClassifyExitCode` dispatch). Every library error type implements `ClassifyExitCode`; new errors must add an impl + `try_downcast!` entry at `classify.rs:76`.

**Candidates for `verify` / `sbom`:**

| Condition | Code |
|-----------|------|
| Reference not resolvable (unknown repo/tag) | `NotFound` (79) |
| Referrers API unsupported (404/405) | `Unavailable` (69) — *or* new `ReferrersUnsupported` mapped to `Unavailable` |
| Referrer manifest malformed | `DataError` (65) |
| No referrers found for requested artifact type | `Success` (0) with empty report, OR `NotFound` (79) if `--require-signature` flag — architect decides |
| Offline + `--offline` flag blocks remote | `OfflineBlocked` (81) |
| Invalid reference input | `UsageError` (64) |
| Signature verification failed (crypto/policy) | new variant? Or `Failure` (1) with structured error — architect decides |
| Registry auth failure | `AuthError` (80) |

## 4. JSON Output

**Mechanism:** Global `--format json|plain` flag (default: plain), declared on `ContextOptions` (`crates/ocx_cli/src/app/context_options.rs:29`). Dispatch lives in `Api::report`:

- `Format` enum: `crates/ocx_cli/src/options/format.rs:7`
- `Api::report(&item)` → `item.print_json()` or `item.print_plain()` via `Printable` trait (`crates/ocx_cli/src/api.rs:19`/`:45`)

**Report data types:** New files under `crates/ocx_cli/src/api/data/`. Each implements:
- `Printable` (plain + JSON printers)
- `serde::Serialize` with explicit field shapes (custom `Serialize` impl common when the plain format diverges from the JSON structure)

No automated schema validation today — JSON shape stability is a review-enforced contract with doc comments. The architect should consider whether `ocx verify` warrants a schemas/ directory entry for consumer contract stability.

## 5. API Layer Boundary

- `crates/ocx_cli/src/` = parse flags + format output
- `ocx_lib::package_manager::*` = business logic

`Api` in `api.rs` is a pure dispatcher (format + printer). There is **no** cross-cutting API facade; each command calls the domain directly.

For `verify` / `sbom`: new methods land either on `oci::Client` (if purely registry-discovery) or on `PackageManager` (if integrated with install flow). Current discovery-only feature fits `oci::Client` best — the ADR in Design should justify this choice explicitly.

Report types go in `crates/ocx_cli/src/api/data/verify.rs` and `api/data/sbom.rs`. Module declarations in `crates/ocx_cli/src/api/data.rs`.

## 6. Help Text Conventions

- Short description: struct doc comment (one line)
- Long description: multi-line doc comment with usage examples
- Flags: `#[clap(short, long)]` with doc comments (first sentence = short help, remainder = long help)
- Patterns: verb-first first word ("Verify OCI referrers attached to a package"); "Use `--flag` for..." explanations; limitations noted inline
- Examples embedded in long description (e.g., `find` shows `jq` usage for JSON output)

## 7. Flag-Before-Positional Ordering

All 30+ existing commands place `#[clap]` flags before positional arguments in the struct definition. This is a documented user preference (`MEMORY.md` → `feedback_flags_before_args.md`) and improves CLI help readability.

Apply to `Verify` / `Sbom`:

```rust
pub struct Sbom {
    // flags first
    #[clap(long, value_enum)]
    pub kind: Option<SbomKind>,  // spdx / cyclonedx
    #[clap(long)]
    pub show_raw: bool,
    // positionals last
    pub reference: options::Identifier,
}
```

## 8. Implementation Checklist for `ocx verify` / `ocx sbom`

1. Create `crates/ocx_cli/src/command/verify.rs` struct with `Parser` derive, flags before positionals, doc comments
2. Create `crates/ocx_cli/src/command/sbom.rs` (same pattern)
3. Add `pub mod verify;` and `pub mod sbom;` in `command.rs` (ln 8–38)
4. Add `Verify(verify::Verify)` / `Sbom(sbom::Sbom)` variants to `Command` enum with doc comments
5. Add dispatch arms in `execute()` match (ln 78–96)
6. Implement `async fn execute(&self, context: Context) -> anyhow::Result<ExitCode>` in each
7. Create report types in `crates/ocx_cli/src/api/data/verify.rs` and `sbom.rs` implementing `Printable + Serialize`
8. Add module declarations to `crates/ocx_cli/src/api/data.rs`
9. Add `ocx_lib` domain functions (`oci::Client::list_referrers` at minimum; possibly `PackageManager::verify`)
10. Implement `ClassifyExitCode` on any new error types + add `try_downcast!` entries in `classify.rs:76`
11. Run `task rust:verify`; then `task verify` before commit

## Citations

| Fact | File:line |
|------|-----------|
| `Command` enum | `crates/ocx_cli/src/command.rs:40–75` |
| Dispatch `execute` match | `crates/ocx_cli/src/command.rs:78–96` |
| Typed exit codes | `crates/ocx_lib/src/cli/exit_code.rs:11–55` |
| Error classification dispatch | `crates/ocx_lib/src/cli/classify.rs:58`, `:76` |
| Format enum | `crates/ocx_cli/src/options/format.rs:7` |
| Global `--format` flag | `crates/ocx_cli/src/app/context_options.rs:29` |
| `Api::report` | `crates/ocx_cli/src/api.rs:45` |
| `Printable` trait | `crates/ocx_cli/src/api.rs:19` |
| `Context::remote_client` | `crates/ocx_cli/src/app/context.rs:126` |
| Reference command: `package_info` | `crates/ocx_cli/src/command/package_info.rs:35` (remote_client usage) |
| Flag-ordering preference | `MEMORY.md` → `feedback_flags_before_args.md` |
