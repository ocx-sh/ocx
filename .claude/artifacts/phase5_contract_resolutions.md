# Phase 5 Contract Resolutions (Slice 1: `ocx package sign` + `ocx verify`)

Phase 5 bridges the stubbed contracts from Phase 3 and the specified test
contracts from Phase 4 with executable behavior. Before touching code we
resolve every under-specified contract point surfaced by the Phase 4 test
suite so downstream implementation has a single source of truth.

**Branch**: `evelynn` · **Issue**: #24 · **Author**: builder worker (Phase 5a).

---

## Resolution Index

| ID | Contract point | Decision |
|----|----------------|----------|
| R1 | Envelope `command` field values | `"package sign"` and `"verify"` (literal space-separated subcommand path) |
| R2 | OIDC identity-token env var | `OCX_IDENTITY_TOKEN` — matches `test_sign.py:48, 187` |
| R3 | `--offline` + `package sign` exit code | `77` (PermissionDenied) via `SignErrorKind::OfflineSignRefused` |
| R4 | Bundle + referrer artifactType | Referrer manifest `artifactType: application/vnd.dev.sigstore.bundle.v0.3+json`; bundle layer mediaType identical |
| R5 | Fake-server scaffolding (Phase 5b) | Python stdlib `http.server` + `ssl` + `cryptography` (ECDSA P-256 leaf certs, RSA JWT) |
| R6 | `SignatureReport` field naming | Expose BOTH `bundle_digest` **and** `referrer_digest` as distinct typed digests |
| R7 | Envelope error `kind` taxonomy | 12 snake_case `ErrorCategory` variants; unknown chain → `internal` |
| R8 | `render_error_envelope` emission | stdout (not stderr) — parseable via `json.loads(result.stdout)` per `test_verify.py:250` |

---

## R1 — Envelope `command` values

**Contract**: `.claude/artifacts/adr_oci_referrers_signing_v1.md` §"Envelope v1 (C-S1-1)".

**Observation**:

- `test_sign.py:67`  — `assert sign_envelope["command"] == "package sign"`
- `test_verify.py:93, 252, 317` — `assert envelope["command"] == "verify"`

**Decision**: the `command` field is a human-readable invocation path, not a
subcommand id. The leading `ocx` is implicit — the binary name is already
known to every reader of the envelope. Use space-separated segments matching
the clap tree:

- Top-level: `"verify"`, `"install"`, `"find"`, etc.
- Nested: `"package sign"`, `"package pull"`, `"index update"`, etc.

**Implementation**: The caller of `render_error_envelope()` / the success
renderer supplies the literal string. For sign and verify specifically this is
wired into the command bodies in `package_sign.rs` and `verify.rs`.

---

## R2 — OIDC identity-token env var name

**Contract**: C-S1-4 (token precedence) forbids raw `--identity-token <VALUE>`.

**Observation**: `test_sign.py:48, 187` uses `OCX_IDENTITY_TOKEN`.

**Decision**: The env var name is **`OCX_IDENTITY_TOKEN`**. Precedence
(highest wins):

1. `--identity-token-file <PATH>` (file contents, trimmed)
2. `--identity-token-stdin` (stdin contents, trimmed — single-use)
3. `$OCX_IDENTITY_TOKEN` (env var, full value)
4. Ambient OIDC provider (GitHub Actions `ACTIONS_ID_TOKEN_REQUEST_*`, etc.)

`--identity-token-file` and `--identity-token-stdin` are mutually exclusive
(clap `conflicts_with`). Empty/whitespace-only overrides are treated as
unset (log a debug message, fall through to next source).

---

## R3 — `--offline` + `package sign` policy

**Contract**: ADR §"Risks" — offline signing unsupported in v1.

**Observation**: `test_sign.py:269` expects exit **77**, not 81. The xfail
reason names `PermissionDenied`, not `OfflineBlocked`.

**Decision**: Rejecting sign under `--offline` is a **policy decision**, not a
network fault. This maps to `SignErrorKind::OfflineSignRefused` → `ExitCode::PermissionDenied` (77).

Rationale:
- `ExitCode::OfflineBlocked` (81) is for *accidental* network attempts while
  offline (a bug-class condition).
- `ExitCode::PermissionDenied` (77) is for *intentional* policy refusals
  ("you asked for something we refuse to do"). Sign under `--offline` is the
  latter — we ship no crypto, we cannot mint a cert, period.

The existing `SignErrorKind::OfflineSignRefused` variant already maps to
exit 77 in `sign/error.rs`. No contract change — we just invoke it from
`PackageSign::execute`.

---

## R4 — artifactType stability

**Contract**: ADR §"Target architecture" — referrer manifest format.

**Decision**:

- Referrer manifest `artifactType`: `"application/vnd.dev.sigstore.bundle.v0.3+json"`
- Bundle layer `mediaType`: `"application/vnd.dev.sigstore.bundle.v0.3+json"`
  (single layer, contents = protobuf-encoded Sigstore Bundle v0.3)
- Referrer manifest has 1 layer (the bundle), no config layer (empty config
  blob `sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a`,
  size 2 = `{}`)
- Subject = the multi-platform image manifest digest for `--platform <plat>`

Expose these as associated constants on the referrers module so the verify
path's mediaType filter stays DRY with the sign path.

---

## R5 — Python fake-server scaffolding (Phase 5b)

**Contract**: `.claude/state/plans/plan_slice1_sign_and_verify.md` §"Phase 5b".

**Decision**: three co-located fakes in `test/tests/fixtures/fake_sigstore.py`:

| Fake | Surface | Implementation |
|------|---------|----------------|
| `FakeFulcio` | HTTPS server on ephemeral port; accepts `POST /api/v2/signingCert`, returns `application/pem-certificate-chain` | `http.server.HTTPServer` + `ssl.SSLContext.wrap_socket`; ECDSA P-256 CA cert + leaf via `cryptography.hazmat` |
| `FakeRekor` | HTTPS server; accepts `POST /api/v1/log/entries`, returns signed entry with SET + inclusion proof | Same stack; ed25519 keypair for SET signing; in-memory log |
| `FakeOidcIssuer` | Test-only, no HTTP — returns a pre-signed RSA JWT with configurable SAN+issuer | `cryptography.hazmat.primitives.asymmetric.rsa` + `jwt.encode` (PyJWT); or hand-roll with base64+sign |

**Dependencies to pin in `test/pyproject.toml`**:

- `cryptography>=43.0` (MIT) — X.509, key generation, TLS cert synthesis
- `pyjwt>=2.9` (MIT) — JWT encoding for `fake_oidc_token`

**CA trust**: The CA cert is written to a tmp file. Both `FakeFulcio` and
`FakeRekor` wrap their sockets with certs chained to this CA. The TrustRoot
override path (`--trust-root <PEM>`) is the injection seam for the binary to
accept the fake CA.

**Determinism**: All keypairs are generated once per `pytest.fixture(scope="session")`
and cached. Clock skew issues are avoided by issuing certs with `not_before =
now - 1m` and `not_after = now + 1h`.

**Defer to 5b**: Full implementation. Phase 5a only needs the xfail markers
to stay `strict=True` and the skeletal dataclasses to remain importable.

---

## R6 — `SignatureReport` field naming (NEW)

**Problem**: `test_sign.py:71, 204, 234` assert `data["bundle_digest"]`, but
the current `SignatureReport` only exposes `referrer_digest`.

**Analysis**: These are two different hashes:

- `bundle_digest` — SHA-256 of the Sigstore Bundle v0.3 protobuf blob (the
  layer's content; what Rekor's inclusion proof covers)
- `referrer_digest` — SHA-256 of the OCI referrer manifest JSON (the top-level
  artifact digest; what `Referrers API` discovery returns)

They are **not** interchangeable. A script that wants to fetch the bundle
contents uses `bundle_digest`. A script that wants to fetch the full referrer
(manifest + bundle layer) uses `referrer_digest`.

**Decision**: Expose both. Final `SignatureReport` shape:

```rust
pub struct SignatureReport {
    pub identifier: String,
    pub subject_digest: oci::Digest,   // the signed image manifest digest
    pub bundle_digest: oci::Digest,    // the bundle blob content digest
    pub referrer_digest: oci::Digest,  // the referrer manifest digest
    pub certificate_identity: String,
    pub certificate_oidc_issuer: String,
}
```

The `Printable` impl lists all three in the single-table render.

---

## R7 — Envelope `error.kind` taxonomy

**Contract**: C-S1-1 `ErrorCategory` enum in `error_envelope.rs`.

**Decision**: The 12 snake_case variants are final:

| Category | Exit code | Mapped from |
|----------|-----------|-------------|
| `usage_error` | 64 | UsageError |
| `config_error` | 78 | ConfigError |
| `data_error` | 65 | DataError |
| `auth_error` | 80 | AuthError |
| `permission_denied` | 77 | PermissionDenied |
| `not_found` | 79 | NotFound |
| `unavailable` | 69 | Unavailable |
| `temp_fail` | 75 | TempFail |
| `rekor_unavailable` | 82 | RekorUnavailable |
| `referrers_unsupported` | 83 | ReferrersUnsupported |
| `io_error` | 74 | IoError |
| `internal` | 1 | Failure + any unclassified chain |

**Classifier**: `ExitCode → ErrorCategory` is a total function, defined in
`error_envelope.rs::ErrorCategory::from_exit_code`.

**Tests**: `test_verify.py:256` asserts `error["kind"] == "not_found"` — this
locks in `NoSignaturesFound → 79 → not_found`.

---

## R8 — Envelope emission stream

**Observation**: `test_verify.py:250`:

```python
envelope = json.loads(result.stdout or result.stderr)
```

The `or result.stderr` is defensive, but `test_sign.py:65` and
`test_verify.py:315` read **only** `result.stdout`. The contract is:

**Decision**: `render_error_envelope` + the success envelope both write to
**stdout** when `--format json` is active. The plain-text error log (via
`tracing::error!`) still goes to stderr; the two streams carry different
payloads:

- stdout (JSON mode, error): one-line envelope JSON object
- stderr (JSON mode, error): structured tracing error (same content, human-readable)

This lets `jq` consume stdout while humans read stderr. In plain mode,
envelope emission is skipped — only the tracing line goes to stderr.

---

## Cross-cutting: Precedence tables for builder clarity

### Token override precedence (Sign)

```
file > stdin > env > ambient
```

Resolved in `package_sign.rs::resolve_identity_token()`:

```rust
if let Some(path) = self.identity_token_file {
    read_and_trim(&path)?
} else if self.identity_token_stdin {
    read_stdin_and_trim()?
} else if let Ok(tok) = std::env::var("OCX_IDENTITY_TOKEN") {
    trim_or_skip(tok)
} else {
    // None → pipeline uses ambient OIDC provider
    None
}
```

### Offline-mode gate (Sign)

`ocx --offline package sign …` → `SignErrorKind::OfflineSignRefused` before any
filesystem or network work, wrapped in `SignError { identifier, kind }` at the
top-level execute body.

### Error envelope JSON rendering (Sign/Verify)

```
main.rs (match result, --format json branch)
  → render_error_envelope("<command>", &anyhow_err)?
    → walk anyhow::Error::chain() via std::iter::successors
      → for each cause, try_classify → Option<ExitCode>
      → first Some wins
    → ExitCode::from_exit_code → ErrorCategory snake_case
    → collect context (identifier, registry, expected_identity, …)
    → serde_json::to_string(&ErrorEnvelope)
  → println!(stdout, "{envelope}")
  → return ExitCode::from(<classified>)
```

Order matters: the classifier walks the chain once, producing both the exit
code and the category. A second walk to collect context annotations happens
in `ErrorEnvelope::from_anyhow`.

---

## Non-Goals / Explicitly Deferred

- **Cert SAN encoding** — deferred to 5b. Phase 5a doesn't mint certs.
- **Rekor SET protobuf layout** — deferred to 5c.
- **TUF trust root refresh** — Phase 5a uses the embedded sigstore trust
  root; TUF updater wiring is Slice 2.
- **Capability cache eviction policy** — write-through TTL only; no
  persistence across Slice boundaries.

---

## Phase 5a Exit Criteria (re-confirmed)

After this doc lands + the Phase 5a commit:

- [x] Contract ambiguities resolved (this doc).
- [ ] `TrustRoot::load_embedded`, `load_from_pem` return a real, usable value.
- [ ] `ReferrersApiCapability::{probe, from_cache, is_fresh, write_cache_atomically}` all work against a live OCI client.
- [ ] `render_error_envelope` produces v1-shaped JSON for every classified error.
- [ ] `PackageSign::execute` / `Verify::execute` bodies compile and dispatch to the pipelines (pipelines themselves may still `unimplemented!()` — Phase 5c's scope).
- [ ] `main.rs` routes `--format json` errors through the envelope before exit.
- [ ] `task rust:verify` is green.
- [ ] Python tests unchanged (5b's scope).

Phase 5b and 5c remain gated on the builder's real-time assessment of
sigstore-rs API surface vs. session budget; if either blows up, the blocker
lands in `.claude/artifacts/phase5_blockers.md` and the commit is scoped to
what did work.
