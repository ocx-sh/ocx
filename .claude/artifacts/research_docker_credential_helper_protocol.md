# Research: Docker Credential Helper Protocol — Wire-Level Implementation Spec

**Date:** 2026-05-14
**Scope:** Authoritative spec for `store` / `get` / `erase` / `list` subprocess protocol; implementation guidance for `ocx login` write path.

## Direct Answer

Complete wire-level specification for implementing `store`, `get`, `erase`, `list` in Rust by shelling out to `docker-credential-{helper}` binaries. All protocol details verified against the canonical Go source at `github.com/docker/docker-credential-helpers`.

**Implementation recommendation: do NOT fork `keirlawson/docker_credential`.** The crate has no extractable subprocess primitive — the spawn logic is locked inside one private `response_from_helper` function. A 25-line in-house primitive in `crates/ocx_lib/src/oci/credential_helper.rs` is cleaner than maintaining a fork + upstreaming PR. Keep using the upstream crate for the read path (`get_credential`); implement `store` / `erase` / `list` natively in OCX.

---

## 1. Protocol Specification

**Binary invocation**: `docker-credential-{suffix} {action}` — one positional argument. Supported actions: `store`, `get`, `erase`, `list`, `version`.

**Error output channel**: Errors go to **stdout**, not stderr. `Serve()` calls `fmt.Fprintln(os.Stdout, err)` then `os.Exit(1)`. Always capture both stdout + stderr; check exit code first, then stdout content to classify the error.

### `store`

```
stdin:  {"ServerURL":"https://registry.example.com","Username":"myuser","Secret":"mypassword"}
stdout: (empty on success)
stderr: (empty — errors go to stdout)
exit:   0 success / 1 failure
```

Validation enforced before backend helper called:
- `ServerURL` empty → stdout `"no credentials server URL"`, exit 1
- `Username` empty → stdout `"no credentials username"`, exit 1

### `get`

```
stdin:  https://registry.example.com   (raw URL bytes, helper trims whitespace)
stdout: {"ServerURL":"https://registry.example.com","Username":"myuser","Secret":"mypassword"}
exit:   0 success
```

**"No credentials" sentinel** — when credentials do not exist, helper exits 1 and writes this exact string to stdout:

```
credentials not found in native keychain
```

Go constant `errCredentialsNotFoundMessage` in `credentials/error.go`. Detection: `stdout.trim() == "credentials not found in native keychain"`. Docker/oras ecosystem uses `IsErrCredentialsNotFoundMessage` which does `strings.TrimSpace(err) == errCredentialsNotFoundMessage`. Exit code 1 alone insufficient — any error exits 1.

**Identity token**: if `Username == "<token>"` in the JSON response, `Secret` is a bearer token → map to `IdentityToken`, not `UsernamePassword`.

### `erase`

```
stdin:  https://registry.example.com   (raw URL bytes)
stdout: (empty on success)
exit:   0 success / 1 failure
```

### `list`

```
stdin:  (nothing — do not write to stdin)
stdout: {"https://registry.example.com":"myuser","https://ghcr.io":"anotheruser"}\n
exit:   0 success / 1 failure
```

stdout is `map[string]string` (server URL → username) encoded via `json.NewEncoder(writer).Encode(accts)` which appends a trailing newline.

---

## 2. Helper Binaries Per Platform

**Linux**:
- `docker-credential-secretservice` — GNOME Keyring / KWallet via libsecret; `apt install golang-docker-credential-helpers`; or GitHub releases to `/usr/local/bin/`
- `docker-credential-pass` — GPG-encrypted `pass` store; requires `pass` initialized with GPG key
- CI / headless: neither may be available — always probe before assuming

**macOS**:
- `docker-credential-osxkeychain` — macOS Keychain; ships with Docker Desktop; `brew install docker-credential-helper`

**Windows**:
- `docker-credential-wincred` — Windows Credential Manager; ships with Docker Desktop; detect with `where.exe docker-credential-wincred`

**Detection probe** (from oras-credentials-go `NewDefaultNativeStore`): `exec.LookPath("docker-credential-{suffix}")` returning `(Store, bool)` where bool = false if not on PATH. Replicate in Rust: `which::which(format!("docker-credential-{suffix}"))`. Platform defaults: Linux → `pass` then `secretservice`; macOS → `osxkeychain`; Windows → `wincred`.

---

## 3. docker `config.json` Schema

```json
{
  "auths": {
    "https://index.docker.io/v1/": {
      "auth": "dXNlcjpwYXNz",
      "identitytoken": ""
    }
  },
  "credsStore": "secretservice",
  "credHelpers": {
    "ghcr.io": "secretservice",
    "123456789.dkr.ecr.us-east-1.amazonaws.com": "ecr-login"
  }
}
```

| Field | Meaning |
|---|---|
| `auths.{reg}.auth` | base64(`username:password`) — plaintext fallback |
| `auths.{reg}.identitytoken` | bearer token alternative |
| `credsStore` | global default helper suffix; binary = `docker-credential-{value}` |
| `credHelpers` | per-registry map: hostname → suffix |

**Resolution order** (first match wins): `credHelpers[reg]` → `credsStore` → `auths[reg]`

**Missing helper on PATH**: Docker does NOT fall back to `auths`. If `credsStore` or `credHelpers` configured and binary absent from PATH, Docker errors hard — no transparent fallback.

---

## 4. `keirlawson/docker_credential` Crate Analysis

**Version**: v1.3.3 (2026-05-01). **Read-only** — public API: `get_credential`, `get_credential_from_reader`, `get_podman_credential`. No `store`, `erase`, `list`.

Complete `helper.rs` source (the only subprocess logic in the crate):

```rust
fn response_from_helper(address: &str, helper: &str) -> Result<HelperResponse> {
    let full_helper_name = format!("docker-credential-{}", helper);
    let mut process = Command::new(&full_helper_name)
        .arg("get")
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|_| CredentialRetrievalError::HelperCommunicationError)?;
    // ... writes address to stdin, reads stdout, parses JSON
}
```

**No `Helper::new(name)` abstraction exists** — suffix passed as `&str`. No internal reusable spawn primitive. Write-path functions belong in OCX module, not as a crate patch.

### The ~25-line in-house addition for `crates/ocx_lib/src/oci/credential_helper.rs`

```rust
const NOT_FOUND: &str = "credentials not found in native keychain";

fn run_helper(helper: &str, action: &str, stdin_payload: &[u8]) -> Result<Vec<u8>, HelperError> {
    let name = format!("docker-credential-{helper}");
    let mut child = Command::new(&name)
        .arg(action)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| HelperError::SpawnFailed { helper: name.clone(), source: e })?;

    if !stdin_payload.is_empty() {
        child.stdin.as_mut().unwrap().write_all(stdin_payload)?;
    }
    let out = child.wait_with_output()?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        let stdout = String::from_utf8_lossy(&out.stdout);
        if stdout.trim() == NOT_FOUND {
            Err(HelperError::NotFound)
        } else {
            Err(HelperError::Failed {
                stdout: stdout.into_owned(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            })
        }
    }
}

pub fn store_credential(helper: &str, server: &str, username: &str, secret: &str) -> Result<(), HelperError> {
    #[derive(serde::Serialize)]
    #[serde(rename_all = "PascalCase")]
    struct Payload<'a> { server_url: &'a str, username: &'a str, secret: &'a str }
    let json = serde_json::to_vec(&Payload { server_url: server, username, secret })?;
    run_helper(helper, "store", &json).map(|_| ())
}

pub fn erase_credential(helper: &str, server: &str) -> Result<(), HelperError> {
    run_helper(helper, "erase", server.as_bytes()).map(|_| ())
}

pub fn list_credentials(helper: &str) -> Result<HashMap<String, String>, HelperError> {
    let raw = run_helper(helper, "list", &[])?;
    Ok(serde_json::from_slice(&raw)?)
}
```

---

## 5. `keyring-core` Crate — Current State

- **Stars**: 29; **Version**: 1.0.0 (2026-04-21); bundled stores explicitly "not secure or robust, not for production"
- **Role**: Native keychain access from Rust process memory — different problem from shelling out to Docker helper binaries
- **Verdict**: Defer. No value for OCX's Docker-protocol subprocess path.

---

## 6. Wire-Level Edge Cases

**Helper exits non-zero with empty stdout**: treat as `HelperCrashed`. Surface binary name + exit code.

**`credsStore` set but binary absent from PATH**: error immediately with `HelperNotOnPath` naming the binary, install hint. Do not fall through to `auths`.

**Error discrimination**:

| stdout (trimmed) | exit | Map to |
|---|---|---|
| `"credentials not found in native keychain"` | 1 | `NotFound` |
| `"no credentials server URL"` | 1 | caller bug |
| any other string | 1 | `HelperFailed` |
| (spawn fails) | n/a | `NotOnPath` |
| `""` | 1 | `HelperCrashed` |

No exit-code-2 convention. Protocol: exit 0 = success, exit 1 = any failure. Discriminate via stdout content only.

---

## Sources

- https://raw.githubusercontent.com/docker/docker-credential-helpers/master/credentials/credentials.go — canonical Store/Get/Erase/List/Serve protocol
- https://raw.githubusercontent.com/docker/docker-credential-helpers/master/credentials/error.go — sentinel strings (`errCredentialsNotFoundMessage`)
- https://github.com/docker/docker-credential-helpers — README, four-subcommand overview
- https://raw.githubusercontent.com/keirlawson/docker_credential/master/src/helper.rs — verbatim Rust subprocess impl for `get`
- https://raw.githubusercontent.com/keirlawson/docker_credential/master/src/lib.rs — public API surface (read-only confirmed)
- https://docs.docker.com/engine/reference/commandline/cli/ — auths/credsStore/credHelpers schema + resolution order
- https://pkg.go.dev/github.com/oras-project/oras-credentials-go — NewDefaultNativeStore detection pattern
- https://docs.rs/keyring-core/latest/keyring_core/ — crate API + stability
- https://oscarchou.com/posts/explanation/docker-credential-deep-dive/ — resolution order + no-fallback behavior

---

## Recommended Implementation Path

1. **Write `run_helper(helper, action, stdin_payload)` as the single subprocess primitive** in `crates/ocx_lib/src/oci/credential_helper.rs`. All four operations share this.

2. **Build thin wrappers**: `store` serializes `{ServerURL, Username, Secret}` JSON (PascalCase keys); `get` deserializes stdout JSON and maps `Username == "<token>"` to `IdentityToken`; `erase` writes raw URL bytes to stdin; `list` deserializes `HashMap<String, String>` from stdout.

3. **Implement config.json write path** following resolution order: `credHelpers[reg]` → `credsStore` → `auths[reg]`. Use `docker_credential` crate's existing `get_credential` for reads. Write path in-house via the subprocess wrappers. Do not patch the crate.

4. **Detect binary presence** with `which::which(format!("docker-credential-{suffix}"))` before any spawn. Return `HelperNotOnPath { name, install_hint }` — do not silently fall through to `auths` when helper configured but missing.

5. **Cover sentinel string in acceptance tests** with fake `docker-credential-test` shell script that exits 1 with stdout `"credentials not found in native keychain\n"`. Assert code maps to `NotFound`, not `HelperFailed`. Highest-risk parsing edge case.
