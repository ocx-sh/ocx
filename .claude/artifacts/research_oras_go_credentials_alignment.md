# Research: oras-go Credentials API — Alignment Analysis for `ocx login` / `ocx logout`

**Date:** 2026-05-14
**oras-go version surveyed:** v2.6.0 / main branch (v3 in development on `v3` branch; API below = stable v2)
**Related:** `adr_ocx_login_credential_store.md`, `research_docker_credential_helper_protocol.md`, `research_cli_login_patterns.md`

---

## 1. `Store` Interface

**Source:** `registry/remote/credentials/store.go`

```go
type Store interface {
    Get(ctx context.Context, serverAddress string) (auth.Credential, error)
    Put(ctx context.Context, serverAddress string, cred auth.Credential) error
    Delete(ctx context.Context, serverAddress string) error
}
```

Three methods, all context-bearing. `serverAddress` is a plain `string` canonicalized via `ServerAddressFromRegistry()`. `docker.io` maps to `https://index.docker.io/v1/`. Not-found is NOT a typed error — `Get` returns the zero-value `EmptyCredential` rather than erroring.

---

## 2. `auth.Credential` Struct

**Source:** `registry/remote/auth/credential.go`

```go
type Credential struct {
    Username     string  // basic auth username
    Password     string  // basic auth password
    RefreshToken string  // OAuth2 identity / refresh token
    AccessToken  string  // short-lived registry bearer token
}

var EmptyCredential Credential // zero value
```

**Flat struct, NOT tagged union.** All fields optional. Population pattern determines mode at call site:
- `Username + Password` → HTTP Basic
- `RefreshToken` alone → OAuth2 identity token (stored via `"<token>"` sentinel username in helpers)
- `AccessToken` alone → direct bearer (cannot survive helper round-trip)

---

## 3. `DynamicStore` — Three-Tier Resolution

**Source:** `registry/remote/credentials/store.go`

```go
type DynamicStore struct { /* ... */ }
type StoreOptions struct {
    AllowPlaintextPut        bool  // default false
    DetectDefaultNativeStore bool  // auto-detect platform helper on clean config
}

func NewStore(configPath string, opts StoreOptions) (*DynamicStore, error)
func NewStoreFromDocker(opt StoreOptions) (*DynamicStore, error)
```

Resolution order in `getStore(serverAddress)`:
1. `config.credHelpers[serverAddress]` — server-specific helper
2. `config.credsStore` — global default helper
3. `detectedCredsStore` — auto-detected, only if `DetectDefaultNativeStore` and config empty at load
4. `FileStore` — plaintext `auths` fallback

`Put()` persists `detectedCredsStore` to `credsStore` on first successful put (`setCredsStoreOnce.Do()`). Auto-detection becomes sticky.

`Delete()` calls `getStore(addr).Delete()` for the resolved tier only — does NOT walk all tiers. Known limitation: cannot distinguish "removed" from "not found"; both return `nil`.

`DetectDefaultNativeStore`: platform defaults `wincred` (Windows), `pass` then `secretservice` (Linux), `osxkeychain` (macOS) via `exec.LookPath("docker-credential-" + suffix)`.

---

## 4. `FileStore` — Plaintext Backend

```go
type FileStore struct {
    DisablePut bool  // if true, Put returns ErrPlaintextPutDisabled
}

var ErrPlaintextPutDisabled = errors.New("putting plaintext credentials is disabled")
var ErrBadCredentialFormat  = errors.New("bad credential format")
```

`Put()` rejects usernames containing `:` (would corrupt `base64(user:pass)`) via `ErrBadCredentialFormat`. `DynamicStore` sets `DisablePut = !opts.AllowPlaintextPut`.

---

## 5. `NativeStore` — Helper Subprocess Backend

**Source:** `registry/remote/credentials/native_store.go`

```go
const (
    remoteCredentialsPrefix       = "docker-credential-"
    emptyUsername                 = "<token>"
    errCredentialsNotFoundMessage = "credentials not found in native keychain"
)

type dockerCredentials struct {
    ServerURL string `json:"ServerURL"`
    Username  string `json:"Username"`
    Secret    string `json:"Secret"`
}
```

**Sentinel-string not-found detection** in `Get()`: exact equality `err.Error() == errCredentialsNotFoundMessage`. On match → `EmptyCredential, nil`. All other errors propagate.

**Bearer round-trip**: `Put()` with `RefreshToken` set encodes as `{Username: "<token>", Secret: refreshToken}`. `Get()` detects `Username == "<token>"` and maps back. `AccessToken` is NOT helper-storable.

**No timeout, no stdout cap** in oras-go. OCX fork adds 30s + 64 KiB defaults.

---

## 6. `Login()` and `Logout()` — Package-Level Functions

**Source:** `registry/remote/credentials/registry.go` (v2.6.0)

```go
var ErrClientTypeUnsupported = errors.New("client type not supported")

func Login(ctx context.Context, store Store, reg *remote.Registry, cred auth.Credential) error {
    regClone := *reg
    var authClient auth.Client
    if reg.Client == nil {
        authClient = *auth.DefaultClient
        authClient.Cache = nil
    } else if client, ok := reg.Client.(*auth.Client); ok {
        authClient = *client
    } else {
        return ErrClientTypeUnsupported
    }
    regClone.Client = &authClient
    authClient.Credential = auth.StaticCredential(reg.Reference.Registry, cred)
    if err := regClone.Ping(ctx); err != nil {
        return fmt.Errorf("failed to validate the credentials for %s: %w", regClone.Reference.Registry, err)
    }
    hostname := ServerAddressFromRegistry(regClone.Reference.Registry)
    if err := store.Put(ctx, hostname, cred); err != nil {
        return fmt.Errorf("failed to store the credentials for %s: %w", hostname, err)
    }
    return nil
}

func Logout(ctx context.Context, store Store, registryName string) error {
    registryName = ServerAddressFromRegistry(registryName)
    if err := store.Delete(ctx, registryName); err != nil {
        return fmt.Errorf("failed to delete the credential for %s: %w", registryName, err)
    }
    return nil
}
```

**`Login()` verifies credentials BEFORE storing**: `Ping(ctx)` → `GET /v2/` with credential applied. Bad credentials never reach the store. Non-negotiable invariant.

**`Logout()`**: resolves name, `store.Delete`. Returns `nil` for both "removed" and "not found". No distinction.

**Both return `error` only** — no location, no enum.

---

## 7. Error Types

```go
var ErrPlaintextPutDisabled  = errors.New("putting plaintext credentials is disabled")
var ErrBadCredentialFormat   = errors.New("bad credential format")
var ErrClientTypeUnsupported = errors.New("client type not supported")
const errCredentialsNotFoundMessage = "credentials not found in native keychain"  // internal
```

**Not-found strategy**: `Get()` returns `(EmptyCredential, nil)` — caller compares to `EmptyCredential`. No exported `ErrNotFound`. `Delete()` returns `nil` for both remove and noop.

---

## 8. config.json Schema Handling

**Source:** `registry/remote/internal/configuration/config.go`

Top-level uses `map[string]json.RawMessage` → unknown fields survive round-trip. Only `auths`, `credsStore`, `credHelpers` are typed.

**Atomic write**:
1. Marshal to bytes
2. `os.MkdirAll(dir, 0700)`
3. Write temp file in same dir (same-FS guarantee for rename)
4. `os.Rename(tempPath, cfg.path)`

No file locking. Relies on POSIX rename atomicity (Windows has same limitation as Docker CLI).

---

## 9. `oras login` CLI

**Source:** `cmd/oras/root/login.go`

```
oras login [flags] <registry>

  -u, --username string            Registry username
  -p, --password string            Password / identity token
      --password-stdin             Read password from stdin
      --identity-token string      OAuth2 refresh token
      --identity-token-stdin       Read identity token from stdin
      --insecure                   Allow HTTP
      --plain-http                 Allow connections without SSL check
      --ca-file string             Server CA cert file
      --registry-config path       Auth config file path
```

Prompt sequence in `runLogin()`: `opts.Secret` empty → prompt username; username empty after prompt → prompt "Token:"; else prompt "Password:". Masked via `term.ReadPassword()` on TTY. Calls `credentials.Login(ctx, store, remote, opts.Credential())`. Prints `"Login Succeeded"`.

---

## 10. OCX Alignment — Side-by-Side

| Concern | oras-go | OCX planned (ADR draft) | **Recommended OCX shape** | Rationale |
|---|---|---|---|---|
| Credential type | Flat struct `{Username, Password, RefreshToken, AccessToken}` | `Credential::Basic / Bearer` enum | **Flat struct** with secrecy-wrapped fields | Enum loses `AccessToken`, diverges from wire format |
| Store trait methods | `Get`, `Put`, `Delete` | `store()`, `erase()`, `get()` | **`get`, `put`, `delete`** | Match Docker helper protocol verbs |
| `put()` return | `error` only | `StoreLocation` | **`Result<(), AuthError>`** — drop StoreLocation | oras-go deliberately omits location; tier is implementation detail |
| `delete()` return | `error` only | `EraseResult::Removed / Noop` | **`Result<(), AuthError>`** — drop EraseResult | Matches oras-go; if UI needs distinction, `get` before `delete` |
| Not-found from `get()` | `(EmptyCredential, nil)` | not yet defined | **`Result<Option<Credential>, AuthError>`** | More idiomatic Rust than zero-value sentinel |
| `login` placement | Package-level fn | Method on store | **Module-level `pub async fn login`** | Keeps Store trait minimal; login = network+store orchestration |
| `logout` placement | Package-level fn | Method on store | **Module-level `pub async fn logout`** | Same rationale |
| Credential verification | `Ping(ctx)` before `Put` | not yet defined | **MUST Ping before Put** | Single most load-bearing invariant — bad creds never reach store |
| Plaintext gate | `AllowPlaintextPut: false` default | not yet defined | **`allow_plaintext_put: false`** default; `--allow-insecure-store` flag | Same safe default |
| Registry canonical | `ServerAddressFromRegistry()`; `docker.io` → `https://index.docker.io/v1/` | `auth/registry_url::canonicalize_registry` planned | **Mirror exactly** | Required for Docker config.json interop |
| Atomic write | Same-dir temp + `os.Rename` + dir `0700` | already in plan | **Same-dir temp + rename + dir `0700` + file `0600`** | Same approach |
| Unknown fields | `map[string]json.RawMessage` | `#[serde(flatten)] other` | **Already correct** | Tools like Docker Desktop write fields OCX must not destroy |

---

## Concrete Recommendations

1. **Adopt flat `Credential` struct, NOT enum.** Resolved-auth `oci::native::Auth` enum stays for OCI HTTP requests. Stored credential is flat 4-field struct mapping 1:1 to `dockerCredentials{ServerURL, Username, Secret}`.

2. **`get()` returns `Result<Option<Credential>, AuthError>`.** No zero-value sentinel.

3. **Drop `StoreLocation` from `put()`.** oras-go intentionally omits it (see oras-credentials-go discussion #18).

4. **Flatten `EraseResult`.** `Ok(())` for both remove and noop. If UI distinction needed, call `get()` first.

5. **Ping-then-Put in `login()` is non-negotiable.** `GET /v2/` with credential before `put()`. Wrap error as `"failed to validate credentials for {registry}: {source}"`.

6. **Top-level `login()` / `logout()` functions, not methods on Store trait.** Trait stays at exactly three methods matching protocol verbs.

7. **`AllowPlaintextPut: false` default.** Expose `--allow-insecure-store` flag for headless CI.

8. **Preserve unknown JSON fields.** `#[serde(flatten)] extra: serde_json::Map<String, Value>`.

---

## Sources

- https://pkg.go.dev/oras.land/oras-go/v2/registry/remote/credentials
- https://pkg.go.dev/oras.land/oras-go/v2/registry/remote/auth#Credential
- https://github.com/oras-project/oras-go/blob/v2.6.0/registry/remote/credentials/registry.go
- https://github.com/oras-project/oras/blob/main/cmd/oras/root/login.go
- https://oras.land/docs/commands/oras_login/
- https://github.com/oras-project/oras-credentials-go/discussions/18 — design rationale for flat-error API shape
