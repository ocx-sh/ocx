# Research: CLI `login` / `auth login` Patterns Across Rust + Go OCI Tools

**Date:** 2026-05-14
**Scope:** Survey login UX across 7 reference CLIs, recommend `ocx login` / `ocx logout` shape.

## Direct Answer

Industry-converged pattern for OCI/registry CLI login:
- Positional `REGISTRY` argument (not a flag)
- `--username/-u` optional flag
- `--password-stdin` flag for piped credentials (no `--password`)
- Masked interactive prompts when credentials absent and TTY present
- `--insecure` / `--plain-http` for HTTP registries
- One-line success confirmation
- Exit 80 (`AuthError`, OCX's existing enum value) on rejected credentials

---

## Per-Tool Survey

### 1. `oras login` (Go, oras-project)

Sources: https://oras.land/docs/commands/oras_login/, https://github.com/oras-project/oras/blob/main/cmd/oras/root/login.go, https://pkg.go.dev/oras.land/oras-go/v2/registry/remote/credentials

| Dimension | Detail |
|---|---|
| Flags | `-u/--username`, `-p/--password`, `--password-stdin`, `--identity-token`, `--identity-token-stdin`, `--insecure`, `--plain-http`, `--ca-file`, `--cert-file`, `--key-file`, `--registry-config` |
| Prompt sequence | Username first; if username empty → prompt `"Token:"` (masked); if username given → prompt `"Password:"` (masked) |
| `--password` on CLI | Accepted **without warning**. This is an oras gap vs docker's practice. |
| Credential store | `credentials.NewStore(configs...)` → `credentials.Login(ctx, store, reg, cred)`. Three-tier DynamicStore: (1) per-server credential helper, (2) platform native keychain (`wincred`/`osxkeychain`/`pass`), (3) plain-text Docker-compat `~/.docker/config.json` |
| Success message | `"Login Succeeded"` |
| Error UX | Returns Go error; no sysexits alignment |

### 2. `docker login` (Go, docker/cli)

Sources: https://github.com/docker/cli/blob/master/cli/command/registry/login.go, https://docs.docker.com/engine/reference/commandline/login/

| Dimension | Detail |
|---|---|
| Flags | `-u/--username`, `-p/--password`, `--password-stdin` |
| Prompt sequence | Tries stored credentials first; if absent → `PromptUserForCredentials()`. Docker Hub also tries device-code flow before CLI prompts. |
| `--password` on CLI | **Emits warning to stderr**: `"WARNING! Using --password via the CLI is insecure. Use --password-stdin."` Login still proceeds. |
| Credential store | `storeCredentials()` → `creds.Store(AuthConfig{...})`. Docker Desktop: OS keychain. Linux without helper: base64 in `~/.docker/config.json` (warns "less secure"). |
| Success message | `response.Auth.Status` — typically `"Login Succeeded"` |

### 3. `gh auth login` (Go, cli/cli)

Source: https://cli.github.com/manual/gh_auth_login

| Dimension | Detail |
|---|---|
| Flags | `--with-token` (PAT from stdin), `-h/--hostname`, `-w/--web` (browser OAuth), `--insecure-storage` |
| Prompt sequence | Multi-step: HTTPS vs SSH → web vs token → masked token paste or browser open |
| `--password` flag | **Does not exist**. PAT only via `--with-token` (stdin only). Web OAuth is default. |
| Credential store | System credential store (keychain/keyring/netrc). `--insecure-storage` forces plaintext fallback. |

`gh` most UX-polished — no `--password` at all makes shell-history leakage impossible by design.

### 4. `cargo login` (Rust)

Sources: https://doc.rust-lang.org/cargo/commands/cargo-login.html, https://doc.rust-lang.org/cargo/reference/credential-provider-protocol.html

| Dimension | Detail |
|---|---|
| Flags | `--registry <name>` (named registry from `.cargo/config.toml`, NOT a positional URL) |
| Token input | Positional argument or read from stdin. No `--password` concept. |
| Credential store | Default `cargo:token` provider: `$CARGO_HOME/credentials.toml`. **Plugin model** (cargo 1.74+, stable): `credential-provider` key per registry, falls back to `registry.global-credential-providers`. Plugins: `cargo-credential-wincred`, `cargo-credential-macos-keychain`. |

**Trend signal**: cargo's credential-provider plugin model is the emerging pattern. Plugin protocol uses JSON stdin/stdout — same direction as Docker credential helpers + oras DynamicStore.

### 5. `npm login`

Source: https://docs.npmjs.com/cli/v10/commands/npm-login

| Dimension | Detail |
|---|---|
| Flags | `--registry=<url>`, `--scope=@scopename`, `--auth-type=web|legacy` |
| Default flow | **Browser OAuth** (`--auth-type=web` is default). Legacy: username + password + email prompts. |

**Trend signal**: browser OAuth as default. Not relevant for OCX backend-first but confirms industry direction.

### 6. `helm registry login`

Source: https://helm.sh/docs/helm/helm_registry_login/

| Dimension | Detail |
|---|---|
| Flags | `-u/--username`, `-p/--password`, `--password-stdin`, `--ca-file`, `--cert-file`, `--key-file`, `--insecure`, `--plain-http`, `--registry-config` |
| CI example | `echo "$GITHUB_TOKEN" | helm registry login ghcr.io -u $GITHUB_USER --password-stdin` |

Nearly identical to oras — both thin wrappers over Docker-compat credential logic.

### 7. `crane auth login` (go-containerregistry)

Source: https://github.com/google/go-containerregistry/blob/main/cmd/crane/cmd/auth.go

| Dimension | Detail |
|---|---|
| Flags | `-u/--username`, `-p/--password`, `--password-stdin` |
| Prompt sequence | **No interactive prompts**. Hard error `"username and password required"` if both absent. |
| Credential store | Docker config: `config.Load(os.Getenv("DOCKER_CONFIG"))` + `creds.Store(AuthConfig{...})` + `cf.Save()`. |

crane = minimal baseline. Pure Docker-config-compat, no keychain.

---

## Synthesis

### The Convention (consensus across 7 tools)

1. Positional `REGISTRY` argument
2. `--username/-u` optional flag
3. `--password-stdin`, NOT `--password` on CLI
4. Interactive prompt when absent and TTY: username first, then masked password/token
5. `--insecure` / `--plain-http` for HTTP-only registries
6. One-line success message
7. Overwrite silently when already logged in — no-op-or-update, not error

### Anti-Patterns to Avoid

- **`--password VALUE` flag** — docker warns, gh removed entirely. Omit from `ocx login`.
- **Printing credentials to stdout/logs** — never echo back.
- **Non-zero exit when not logged in on logout** — docker + oras return 0 (CI-friendly). Only `gh` errors. Majority is right for automation.

### JSON Output Mode

None of the 7 surveyed tools support `--format json` on login. OCX backend-first + CI-first — this is a genuine differentiator worth adding.

Proposed `--format json` success output:
```json
{"registry": "ghcr.io", "username": "ocx-bot", "store": "docker-credential-secretservice", "action": "stored"}
```

`ocx logout --format json`:
```json
{"registry": "ghcr.io", "action": "removed"}
```
or on not-logged-in no-op:
```json
{"registry": "ghcr.io", "action": "noop"}
```

### Logout Behavior

| Tool | Exit when not logged in |
|---|---|
| docker | 0 (silent no-op) |
| oras | 0 (silent no-op) |
| gh | Non-zero |
| helm | 0 |

**Recommendation**: OCX exits 0 on logout-when-not-logged-in. Backend cleanup scripts must not fail because a previous step already cleaned up.

---

## Recommended `ocx login` UX Spec

### Synopsis

```
ocx login REGISTRY [--username USER] [--password-stdin] [--insecure] [--format plain|json]
ocx logout REGISTRY [--format plain|json]
```

### Flag Table

| Flag | Short | Default | Description |
|---|---|---|---|
| `REGISTRY` | — | required | Registry hostname (e.g., `ghcr.io`, `registry.myco.com`) |
| `--username` | `-u` | prompted | Registry username. Interactive prompt if absent and TTY. |
| `--password-stdin` | — | off | Read password/token from stdin. Required in non-TTY context. |
| `--insecure` | — | off | Allow plain HTTP. Consistent with `OCX_INSECURE_REGISTRIES`. |
| `--format` | — | `plain` | `plain` or `json` |

No `--password` flag. CI pipes via stdin (`echo "$TOKEN" | ocx login ghcr.io -u $USER --password-stdin`); humans get interactive prompt.

### Interactive Prompt Sequence

When stdin is a TTY and credentials not supplied:
1. `Username: ` — plain input (or pre-filled from `--username`)
2. If username empty: `Token: ` (masked) → stored as `Auth::Bearer`
3. If username given: `Password: ` (masked) → stored as `Auth::Basic`

When stdin is not a TTY and `--password-stdin` absent:
→ exit 64 `UsageError`: `"non-interactive login requires --password-stdin"`

### Exit Code Table

| Variant | Value | Condition |
|---|---|---|
| `Success` | 0 | Credentials stored; registry confirmed them |
| `UsageError` | 64 | Missing `REGISTRY` arg; `--password-stdin` absent in non-TTY context |
| `Unavailable` | 69 | Registry unreachable during credential verification |
| `ConfigError` | 78 | Failed to write to credential store |
| `AuthError` | 80 | Registry rejected credentials (HTTP 401/403) |

### Success Output

Plain: `Login succeeded` (one line to stdout)
JSON: `{"registry":"ghcr.io","username":"ocx-bot","store":"docker-credential-secretservice","action":"stored"}`

`store` field names the credential helper binary used (or `"config-file"` when falling back to `~/.docker/config.json`).

### `ocx logout`

```
ocx logout REGISTRY [--format plain|json]
```

- Removes entry from Docker config / credential helper
- Exit 0 even if not logged in (silent no-op — CI cleanup-friendly)
- Plain: `Logged out of ghcr.io` (or `Not logged in to ghcr.io` — still exit 0)
- JSON: `{"registry":"ghcr.io","action":"removed"}` or `{"registry":"ghcr.io","action":"noop"}`

### Comparative Table

| Dimension | oras | docker | gh | crane | **OCX** |
|---|---|---|---|---|---|
| Positional REGISTRY | Yes | Yes | `--hostname` | Yes | **Yes** |
| `--username/-u` | Yes | Yes | `--with-token` only | Yes | **Yes** |
| `--password-stdin` | Yes | Yes | `--with-token` | Yes | **Yes** |
| `--password` flag | Yes (no warn) | Yes (warns) | No | Yes | **No** |
| Interactive prompt | Yes | Yes | Yes (multi-step) | **No** | **Yes (2-step)** |
| `--insecure` | Yes | No | N/A | No | **Yes** |
| Credential store | DynamicStore 3-tier | Docker helpers | OS keychain | Docker config | **Docker-compat via in-house helper subprocess** |
| sysexits exit codes | No | No | No | No | **Yes** |
| `--format json` | No | No | No | No | **Yes** |
| Logout no-op exit 0 | Yes | Yes | **No** | Yes | **Yes** |

---

## Sources

- https://oras.land/docs/commands/oras_login/
- https://github.com/oras-project/oras/blob/main/cmd/oras/root/login.go
- https://pkg.go.dev/oras.land/oras-go/v2/registry/remote/credentials
- https://github.com/docker/cli/blob/master/cli/command/registry/login.go
- https://docs.docker.com/engine/reference/commandline/login/
- https://cli.github.com/manual/gh_auth_login
- https://doc.rust-lang.org/cargo/commands/cargo-login.html
- https://doc.rust-lang.org/cargo/reference/credential-provider-protocol.html
- https://docs.npmjs.com/cli/v10/commands/npm-login
- https://helm.sh/docs/helm/helm_registry_login/
- https://github.com/google/go-containerregistry/blob/main/cmd/crane/cmd/auth.go
- https://orca.security/resources/blog/password-in-shell-history/ — shell history risk rationale
