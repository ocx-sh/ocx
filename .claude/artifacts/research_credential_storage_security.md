# Research: OCI Credential Storage Security & Compliance

**Date:** 2026-05-14
**Scope:** Threat model, secure-by-default requirements, audit trail for `ocx login REGISTRY`.

---

## 1. Threat Model

### Assets

- Bearer tokens (long-lived identity tokens from OAuth flows)
- Basic-auth passwords (username + password)
- `~/.docker/config.json` (confidentiality + integrity)

### Attacker classes × storage tier

| Attacker | Plaintext config.json (base64) | secretservice (Linux) | macOS Keychain | Windows DPAPI | docker-credential-pass (GPG) |
|---|---|---|---|---|---|
| Different uid, same machine | **Mitigated** by 0600 (advisory) | **Mitigated** (D-Bus session per uid) | **Mitigated** (ACL gated) | **Mitigated** (per-user key) | **Mitigated** (GPG encrypted) |
| Root | **Exposed** | **Exposed** (can read keyring backing file) | **Partially** (SIP unless disabled) | **Partially** (Mimikatz-class) | **Exposed** (can read agent socket) |
| Shoulder-surfer | **Exposed** at login | **Partial** (unlock dialog) | **Partial** (unlock prompt) | None | **Partial** (GPG passphrase prompt) |
| Same-uid malicious software | **Fully exposed** | **Exposed when unlocked** (documented GNOME Keyring risk) | **Partial** (path/sig prompt) | **Exposed** (CryptUnprotectData same session) | **Exposed when agent unlocked** |

**Conclusion**: No tier protects against same-uid malware in active desktop session. Tiers differ on: (a) different-uid protection (OS-native all mitigate), (b) at-rest when logged out (Keychain / DPAPI / pass mitigate; plaintext does not), (c) audit trail (none provide one). Plaintext base64 offers no protection beyond filesystem permissions.

---

## 2. Secret Lifecycle Hazards

### Logs (CWE-532)

Current `auth.rs` log sites do not log secret values — only registry name + error class. Rule: no `String` holding a credential may appear as `{}` / `{:?}` arg in any `log::` or `tracing::` call. Enforce via type discipline (see secrecy below) not redaction filters.

### Process memory (CWE-316)

`oci::native::Auth` holds credentials as plain `String` in `Arc<RwLock<HashMap>>`. Rust `String` does NOT zero on drop. Core dump captures live credentials. Recommendation: wrap `Bearer(token)` and `Basic(_, pwd)` value fields in `secrecy::SecretString` — `zeroize`-on-drop + `Debug` redaction at compile time. `ExposeSecret::expose_secret()` makes every access site explicit + auditable.

### Shell history (CWE-214)

`ocx login --password VALUE` literal → bash/zsh `HISTFILE`, ps aux, `/proc/PID/cmdline`. CWE-214 classifies argv-visible secrets as vulnerability. Docker docs explicit: "Using STDIN prevents the password from ending up in the shell's history". **`--password VALUE` must be refused outright**.

### Core dumps

Linux writes `/proc/sys/kernel/core_pattern` dumps on SIGABRT/SIGSEGV. `secrecy::SecretString` zeroing on drop doesn't help if dump written before drop. Mitigation: `prctl(PR_SET_DUMPABLE, 0)` early in `main()`. Suggest-tier for v1.

### Backtraces

`RUST_BACKTRACE=1` does not print local variable values. Risk only via `{:?}` format on credential-bearing struct. `secrecy::SecretString` `Debug` impl prints `"[REDACTED]"` — primary motivation for type-level discipline.

---

## 3. Docker Credential Helper Protocol Security

`docker_credential` crate (v1.3.3) shells out to helper binaries per protocol:

- Registry URL passed via stdin (not argv). Spec compliant.
- Helper responds with JSON on stdout. Secrets never on argv.

### OCX-specific risks

**Helper path resolution**: crate uses bare `Command::new("docker-credential-{name}")` → PATH lookup. PATH-injection risk if attacker has `docker-credential-myhelper` early in PATH (tampered shell rc). Mitigation: canonicalize via `which::which`, validate path not under user-writable directory.

**Subprocess timeout**: crate does not bound helper execution. Rogue / hung helper blocks OCX indefinitely. **Block-tier**: wrap subprocess in `tokio::time::timeout` (30s default). `FileLock::lock_exclusive_with_timeout` pattern in codebase confirms approach works.

**Helper stdout validation**: crate parses with `serde_json` unbounded. Oversized stdout from rogue helper = allocator-level DoS. Mitigation: cap read at 64 KiB; reject larger.

**Concurrent config.json access**: `ocx login` writes `~/.docker/config.json`. Concurrent `ocx pull` reading same file → torn writes. **Block-tier**: read-modify-write must hold `FileLock::lock_exclusive` for full duration. `crates/ocx_lib/src/file_lock.rs` already provides the primitive; `acquire_project_lock` in `project/mutate.rs` is the pattern.

---

## 4. Cryptographic Primitives by Tier

| Tier | Crypto | OCX adds |
|---|---|---|
| `~/.docker/config.json` base64 | None (encoding, not encryption) | Nothing |
| secretservice (Linux) | AES-128 + SHA-256, session key from login pwd | Nothing |
| macOS Keychain | AES-256, ACL gated by code signature + path | Nothing |
| Windows DPAPI | AES, PBKDF2 from user pwd, per-user scope | Nothing |
| docker-credential-pass | GPG symmetric/asymmetric, strength = key choice | Nothing |

OCX adds no crypto of its own. Correct policy. Custom encryption above OS tiers = key management problem without security improvement.

---

## 5. Block-tier Security Requirements

The implementing developer must satisfy ALL of these before merge:

1. **`--password VALUE` literal refused** at argument-parse with `ExitCode::UsageError` + message directing to `--password-stdin`. CWE-214. Acceptance test verifies exit 64 + stderr.
2. **`--password-stdin` mandatory** for non-interactive credential supply. Strip exactly one trailing newline, do not echo.
3. **Credential helper subprocess via stdin only** — argv carries only action verb (`store` / `get` / `erase`). Registry URL on stdin. Already correct in `docker_credential`; new write path must follow.
4. **Helper subprocess wrapped in `tokio::time::timeout`** (30s default). Unit test with mock hanging helper verifies timeout.
5. **Atomic config write under exclusive flock** — `~/.docker/config.json` read-modify-write holds `FileLock::lock_exclusive` for full duration. Write-to-temp-then-rename. Unit test: concurrent reader sees old or new complete JSON, never torn state.
6. **No log site references credential-bearing variable** — structural via `secrecy::SecretString` makes `{:?}` / `{}` a compile error via `ExposeSecret`.
7. **Helper path resolution does not traverse user-writable directories** — validate resolved path under `/usr/bin/`, `/usr/local/bin/`, or `~/.docker/`. Unit test for path validation.
8. **Config file created with mode 0600** — `OpenOptions` + `unix::fs::OpenOptionsExt::mode(0o600)`. Acceptance test reads mode after `ocx login`.

---

## 6. Suggest-tier (improvement, not merge-blocking)

- Wrap `Auth::Bearer(token)` and `Auth::Basic(_, password)` value fields in `secrecy::SecretString`. Compile-time enforcement of secret discipline.
- `Display` impls on credential-bearing types print `"[REDACTED]"` or are not implemented.
- `prctl(PR_SET_DUMPABLE, 0)` early in `main()` on Linux to suppress core dumps.
- Cap helper stdout read at 64 KiB.

---

## 7. Compliance Touchpoints

**GDPR/PII**: username may be personal data. OCX stores locally only, never transmits. Document in privacy notice.

**SLSA/supply chain**: out of scope for v1 login. Sigstore/cosign signing = separate concern.

---

## Security Acceptance Criteria

Implementation must satisfy all before merge:

1. `ocx login REGISTRY --password VALUE` rejected at parse time with exit 64 + stderr message.
2. `ocx login REGISTRY --password-stdin` reads from stdin, strips one trailing newline, writes via configured store. Happy-path acceptance test.
3. `~/.docker/config.json` write path holds `FileLock::lock_exclusive` for full duration + atomic rename. Concurrent-reader unit test.
4. Helper subprocess wrapped in `tokio::time::timeout` (30s). Mock-hanging-helper unit test.
5. No `log::*` / `tracing::*` call site references credential-bearing variable. Code review enforces; `secrecy::SecretString` makes it compile error.
6. `~/.docker/config.json` created with mode 0600 when absent. Mode-check acceptance test.
7. Helper binary path resolution validated under safe prefix. Unit test for path validator.
8. `--password-stdin` impl does not echo credential to stdout/stderr at any log level. `--log-level debug` acceptance test asserts stderr free of test credential.
9. `ExitCode::AuthError` (80) returned when registry rejects credentials. CI distinguishes auth failure from network (69) or config (78).
10. `Display` impl on credential-bearing types prints `"[REDACTED]"`. `format!("{}", val)` unit test asserts output free of secret.

---

## Sources

- https://cwe.mitre.org/data/definitions/214.html — Invocation of Process Using Visible Sensitive Information
- https://cwe.mitre.org/data/definitions/256.html — Plaintext Storage of a Password
- https://cwe.mitre.org/data/definitions/312.html — Cleartext Storage of Sensitive Information
- https://cwe.mitre.org/data/definitions/316.html — Cleartext Storage of Sensitive Information in Memory
- https://cwe.mitre.org/data/definitions/532.html — Information Exposure Through Log Files
- https://cheatsheetseries.owasp.org/cheatsheets/Secrets_Management_Cheat_Sheet.html
- https://cheatsheetseries.owasp.org/cheatsheets/Cryptographic_Storage_Cheat_Sheet.html
- https://github.com/docker/docker-credential-helpers
- https://docs.docker.com/engine/reference/commandline/login/
- https://wiki.gnome.org/Projects/GnomeKeyring/SecurityFAQ
- https://learn.microsoft.com/en-us/windows/win32/api/dpapi/nf-dpapi-cryptprotectdata
- https://docs.rs/secrecy/latest/secrecy/
- https://docs.rs/zeroize/latest/zeroize/
