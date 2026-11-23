# Research: Registry credential input, codebase recon for ocx-sh/ocx#756

## Metadata

**Date:** 2026-11-22
**Domain:** security / cli / packaging
**Triggered by:** ocx-sh/ocx#756, a request for private-registry credentials supplied from a file (scripts, batch HPC jobs, secrets locked by filesystem ACLs, Windows UNC paths). Candidates: (A) `OCX_AUTH_<REGISTRY>_TOKEN_FILE`, (B) env-file loading (`--env-file`), (C) age-encrypted secrets with `OCX_AGE_KEY_FILE`, (D) existing behaviour already covers it.
**Expires:** 2027-05-22 (re-verify; code paths move fast)

Scope: recon only, read-only on source. Everything cited was read in this tree (branch `tri`). Items marked (inferred) were not read directly. The GitHub issue text itself could not be fetched (`gh issue view 756` printed nothing, no auth), so the candidate list is taken from the task brief.

## Direct Answer

Works today without a code change: partly, and only for the Docker-config path. Setting `DOCKER_CONFIG=<dir>` to any directory (ACL-locked, UNC included, inferred for UNC) that holds a `config.json` with `auths[<registry>].auth` (base64 `user:pass`) or `identitytoken` makes OCX read credentials from there (`external/docker_credential/src/lib.rs:120-124,216-223`; `crates/ocx_oci/src/auth.rs:108`). What does not work: pointing at an arbitrary file name (the name is fixed to `config.json`, directory only), `REGISTRY_AUTH_FILE` (never read by OCX), and a token-only file consumed as `OCX_AUTH_*_TOKEN`. A shell can already do `OCX_AUTH_<slug>_TOKEN="$(cat file)"` per invocation, which works but puts the secret in the process environment (see Q1, Q3).

## Q1. Credential resolution order today, and what works without code change

Order, per registry, per process (one lookup per registry, outcome cached, `crates/ocx_oci/src/auth.rs:35-61` doc, `lookup` at `auth.rs:77-92`):

1. `OCX_AUTH_<slug>_{TYPE,USER,TOKEN}` env vars (`auth.rs:78`, `get_env_auth` at `auth.rs:134-174`).
2. Docker config via `docker_credential::get_credential` (`auth.rs:108`, re-export at `crates/ocx_oci/src/lib.rs:135`), on a blocking thread (`auth.rs:83`). Inside the dependency, in order: `credHelpers[server]`, then `identitytoken`, then `auths[server].auth`, then `credsStore` (`external/docker_credential/src/lib.rs:148-173`). Note helper entries win over a plaintext `auths` entry for the same registry.
3. Anonymous fallback, with a `log::warn!` on error (`auth.rs:63-75`).

Where `ocx login` writes: `$DOCKER_CONFIG/config.json`, else `<home>/.docker/config.json` (`crates/ocx_oci/src/auth/store.rs:155-171`), with `<home>` from `ocx_env::home_dir()` (USERPROFILE on Windows, `crates/ocx_env/src/lib.rs:420-430`). Tier choice on write: `credHelpers[reg]`, then `credsStore`, then plaintext `auths` only with `--allow-insecure-store` (`crates/ocx_cli/src/command/login.rs:42-43`; `store.rs:246-263`; docs `website/src/content/docs/guides/authenticate.mdx:97-107`). New file mode is `0600` via `NewFileMode::Private` (`store.rs:447`), existing mode and symlink target preserved (`authenticate.mdx:99`). No native-store detection on login (`login.rs:119-124`). Password input is `--password-stdin` only, no argv flag (`login.rs:21,33`).

Overridability of the location:
- `DOCKER_CONFIG` only. Declared `Path, Foreign, reader = Dependency("docker_credential"), propagation = UserInput` (`crates/ocx_env/src/vars.rs:742`). Honored on write (`store.rs:160`, test `store.rs:900-917`) and on read (`external/docker_credential/src/lib.rs:122`).
- `REGISTRY_AUTH_FILE` is read only by `docker_credential::get_podman_credential` (`lib.rs:232-259`). Nothing in `crates/` calls it (only `get_docker_credential` is re-exported, `ocx_oci/src/lib.rs:135`). So it has no effect. No `OCX_*` variable moves the Docker config location.

Works today, no code change:
- `DOCKER_CONFIG=<locked dir>` plus a hand-written or `ocx login`-written `config.json` there. Read path is a plain `File::open(<dir>/config.json)` (`lib.rs:216-221`), so any path the process can open works, a UNC path included (inferred: `PathBuf::from(var_os)`, no canonicalisation, no Windows-specific code read).
- The value must be a directory; a file named other than `config.json` cannot be targeted.
- Helpers named in that config run as usual (PATH lookup, `which`, world-writable-ancestor refusal, `external/docker_credential/src/helper.rs:316-335`).

Does not work / gaps:
- Windows without `DOCKER_CONFIG`: the read path falls back to `HOME` only (`external/docker_credential/src/lib.rs:121`), the write path to `USERPROFILE` (`store.rs:166-170`, `ocx_env/src/lib.rs:426`). On a Windows host with `HOME` unset, `ocx login` writes where the lookup does not read (inferred from the two reads; not run). Setting `DOCKER_CONFIG` avoids it.
- A failed read of `config.json` (missing, permission denied, ACL refusal) maps to `ConfigReadError`/`ConfigNotFound`, which `get_docker_auth` swallows at `debug` level and falls to anonymous (`auth.rs:113-118`). A user pointing at a file they cannot open sees an anonymous 401 from the registry, not a credential error (exit code is the registry's, see Q4).
- No plain "token in a file" form: the Docker config needs base64 `user:pass` or an `identitytoken`; a bearer-only secret is representable only as `identitytoken` (`lib.rs:160-162`, becomes `Auth::Bearer`, `auth.rs:109`).
- `DOCKER_CONFIG` is `UserInput`: inherited by children (`ocx exec`, nested ocx) untouched (`ocx_env/src/lib.rs:82-83`), but it names a path, not a secret.

Shell-side workaround today: `OCX_AUTH_<slug>_TOKEN="$(cat file)"` per command. The value then sits in the OCX process environment and, being `SecretInput`, is inherited by every child (see Q3). No file-read happens inside OCX.

## Q2. Registry slug and the "any OCX_AUTH_* value disables helpers" rule

Slug: `registry.to_slug()` (`auth.rs:135`), where `to_slug` replaces every non-`[A-Za-z0-9]` char with `_`, case kept (`crates/ocx_util/src/string_ext.rs:16,22-24`). `docker.io` gives `docker_io`, `localhost:5000` gives `localhost_5000` (test `string_ext.rs:49-51` is the relaxed variant; the strict one is the stated behaviour in `vars.rs:480-483`). The `registry` string is `image.registry()` at dial time (`crates/ocx_oci/src/client/native_transport.rs:53`), so for index-routed names it is the registry actually dialled (inferred from the call site, not traced through routing). The slot is substituted into the pattern name by `EnvVar::slot_value` (`ocx_env/src/lib.rs:193-197`); the read is not case-folded in code, on Windows the OS env is case-insensitive (inferred).

The helper rule, as code actually behaves (`auth.rs:134-174`):
- `TYPE` set: always returns `Some` (anonymous, basic needs USER+TOKEN, token needs TOKEN); a missing half is `AuthError::MissingEnv`, exit 78 slug `auth_env_missing` (`auth/error.rs:20-26`), but `lookup_or_anonymous` turns any error into a warning plus anonymous (`auth.rs:63-75`), so a half-set basic type ends as anonymous, not as a Docker-config lookup.
- `TYPE` unset: `USER`+`TOKEN` gives Basic, `TOKEN` alone gives Bearer, anything else (including `USER` alone) returns `None` and falls through to the Docker config and helpers (`auth.rs:166-170`).
- So "any `OCX_AUTH_*` value set disables helpers" (`vars.rs:484-485`, `environment.md:89`) is exact for TYPE and TOKEN, not for a lone USER. The guide says `USER` alone is ignored (`authenticate.mdx:67`), which contradicts the broader env-reference wording. Unit test covers TYPE and TOKEN only (`auth.rs` test `an_ocx_auth_override_never_asks_the_helper`), acceptance test the same two (`test/tests/test_credential_helper_reentry.py:328-374`).
- Empty values read as unset (`ocx_env/src/lib.rs:196`; test `auth.rs` `test_get_env_auth_empty_value_reads_as_unset`).

What a fourth slot variable (`OCX_AUTH_{REGISTRY}_TOKEN_FILE`) would touch: a fourth `get_slot` read and a new precedence between TOKEN and file inside `get_env_auth` (`auth.rs:140-148`); it would have to join the "any value suppresses helpers" rule and the TYPE-less inference at `auth.rs:166-170`. Name-parsing note (inferred): pattern slots are matched by prefix/suffix only; `OCX_AUTH_a_TOKEN_FILE` cannot be read as TYPE/USER/TOKEN of some registry (suffix differs). The sdkgen secret matcher treats `OCX_AUTH_ghcr_io_TOKEN_EXTRA` as not secret (`crates/ocx_sdkgen/tests/sdk_rust.rs:343-351`), which is correct for a path-valued variable.

Prefix-based defences already cover any `OCX_AUTH_*` name: Starlark env masking (`crates/ocx_script/src/ocx_module.rs:47,380`) and "never recorded in applied state" (`crates/ocx_package_manager/src/exec_env.rs:316-323`, `activation.rs:1394`, doc `vars.rs:505`). Explicit lists that name the three suffixes and would need the fourth: the seam poison list (`crates/ocx_cli/src/app/seam.rs:513`) and the docs link table (`crates/ocx_schema/src/docs/render.rs:698-711`).

## Q3. How the env registry models a variable

Declaration (`crates/ocx_env/src/lib.rs:20-37`): `name` (literal or `{SLOT}` pattern), value kind, `on_invalid`, `visibility`, `secret`, `propagation`, `reader`.
- Value kinds `EnvValue` (`lib.rs:41-55`): `Bool`, `TriBool`, `String`, `Integer`, `Path`, `PathList`, `PathOrPem`, `HostList`, `Choice[..]`, `Json`.
- `secret`: the plain readers answer `None`; value reads only through `SecretVar` as `Sensitive` (no `Display`, redacted `Debug`) (`lib.rs:128-152,200-251`).
- Visibility: `Public` (`OCX_*`, in docs, SDK manifest and compat gate), `Foreign`, `Plumbing`, `Testing` (`lib.rs:66-77`).
- `on_invalid`: `Default` or `Error` (exit 78 slug `invalid_env`, names the key only) (`lib.rs:57-64,106-125`).
- Propagation (`lib.rs:79-96`): `UserInput` (inherited untouched), `SecretInput` (UserInput that is never recorded in state, priors `Withheld`), `SetupWritten`, `State`, `BinaryPin`, `HookLocal` (removed at every projected spawn), `Credential` (a `secret` removed at every spawn, `credential_keys()` at `lib.rs:388-402`). A structural test requires every `secret` to be `Credential` or `SecretInput` (`crates/ocx_env/src/vars/tests.rs:270-283`).

Current OCX_AUTH row: TYPE `Choice`, USER `String`, both `Public, UserInput`; TOKEN `String, Public, secret, SecretInput` (`vars.rs:486-494`). SecretInput means children (`ocx exec` tools, nested ocx, plugins) inherit the token. `.claude/rules/subsystem-cli.md:434` records this as a known-open mechanism gap: the credential scrub list is a fixed `&[&str]`, so a name pattern cannot be on it.

A path-valued variable pointing at secret material in this model: `Path` kind, `Public`, not `secret` (the path is not the secret; `secret` would make `get()` return `None` for it), probably `UserInput`. Existing analogues:
- `OCX_EXTRA_CA_CERTS: PathOrPem, Public, UserInput` (`vars.rs:405`), certificates not secrets.
- `OCX_SIGNING_KEY: String, secret, Credential` (`vars.rs:444`) holds the PEM itself, "never a path", for runners with no writable disk; the file form is the `--key <path>` flag instead (`crates/ocx_sign/src/sign/key_backend.rs:105-110,133-142`).
- No existing `Path`-kind variable that names a secret file was found (grep of `vars.rs` for `_FILE`: only `__OCX_TESTING_FAULT_RELEASE_FILE` and `SSL_CERT_FILE`, neither a secret).
Contract rule for new Credential-class members: add to the credential table in `subsystem-cli.md`, state in `environment.md` it is never forwarded (`lib.rs:394-397`); the doc comment there calls this a four-edit checklist.

## Q4. Precedent for reading a secret from a file or stdin

- `ocx login --password-stdin`: reads stdin to end, strips exactly one trailing `\n`, preserves `\r`, empty input exits 64 `UsageError`; no argv password flag by design (`login.rs:21-34,81-92`; tests `test/tests/test_login.py:236-364`; ADR requirement table `.claude/artifacts/adr_ocx_login_credential_store.md:186-187`).
- Signing key file: `--key <path>` read by `read_key_pem` through `read_bounded(path, 64 KiB)` (`key_backend.rs:133-142`; `ocx_util/src/fs/bounded_read.rs:14-72`). Refuses non-regular files (`/dev/zero`), over-cap gives `MalformedKey` (exit 65), I/O error carries raw `ErrorKind` (exit 74, `ocx_trust/src/key_ref.rs:12-17,29-39`). No trim (PEM), no permission-bit check, no owner check on the key file.
- Companion secret via env: `OCX_KEY_PASSWORD` read verbatim, empty means empty password (`key_backend.rs:73-79`); `OCX_SIGNING_KEY` via `env://` (`key_ref.rs:44-57`) is the value-not-path counterpart.
- No `*_FILE` env variable for a secret exists. No forge-token file: `OCX_ANNOUNCE_TOKEN` and `OCX_ANNOUNCE_GIT_TOKEN` are env only (`crates/ocx_announce/src/forge/credentials.rs:112-121`).
- Permission checking precedent exists only for directories, not secret files: `toolchain_dir` must be owner-owned and not group/world writable (Unix, `crates/ocx_config/src/lib.rs:1105-1137`; error slug `toolchain_dir_group_or_world_writable`, `lib.rs:597-613`). Written credential files get `0600` (`store.rs:447`); the vendored security rule SEC-12 requires `0600` at creation and SEC-24 names "stdin, an fd, or a `0600` file" as the safe secret channels (`.claude/rules/rust-quality/security.md:62,101`).
- Exit code/slug when a credential cannot be obtained: `AuthError::MissingEnv` is 78 `auth_env_missing`; `InvalidType` 78 `invalid_auth_type`; `DockerCredentialRetrieval` and `Helper` map per slug (`crates/ocx_oci/src/auth/error.rs:12-59`); a file-read failure would map by the quality rule's `IoError` 74 / `DataError` 65 split (`quality-rust-exit_codes.md`, `ocx_exit/src/exit_code.rs:50,55,62`). Note the current auth path never exits on these: `lookup_or_anonymous` converts every `AuthError` into a warning plus anonymous (`auth.rs:63-75`), so a new file-read error would silently degrade unless the path is changed.
- Secrets never named in errors: `InvalidEnv` names the key only (`lib.rs:106-125`); `BoundedReadError::into_io_error` exists to avoid naming a path that is itself secret (`bounded_read.rs:36-38`).

## Q5. Env loading from a file; credentials in config

- No dotenv, no `--env-file`, no `*.env` reader in `crates/` (grep for `dotenv|env-file|env_file` found only GitHub/GitLab CI export sinks, `ocx_shell/src/ci/github_flavor.rs`, which write files, not read them).
- `--env KEY=VAL` exists on `ocx exec` and related commands and `[env]` in `ocx.toml` (`crates/ocx_cli/src/command.rs:397-398,430`). Both feed the child's environment, never OCX's own resolution. They also cannot set `OCX_*` at all: `is_reserved_ocx_key` is "the one gate stopping every env surface from setting `OCX_*`, or a checked-in file could reconfigure resolution" (`ocx_env/src/lib.rs:445-452`). A general env-file loader would have to either bypass that gate for `OCX_AUTH_*` or stay a process-local feature (design question, not evidence either way).
- Config tiers (`crates/ocx_config`): `[registries."<ns>"]` has `index`, `trusted_hosts`, `insecure`, `system_locked`, `signatures` (`registry.rs:26-67`); no credential field. Credentials are kept out of the config tree by design in the auth docs and ADR: `ocx login` and the env vars are the two inputs, and `config.toml` is fleet-read, managed-tier-distributed state (`arch-principles.md` fleet forward-compat row). I did not find a sentence ruling out credentials in `config.toml` explicitly (negative).
- Precedent against that: `[telemetry] headers` in `config.toml` may carry an `Authorization` token, and `OCX_TELEMETRY_HEADERS` is documented as "put a long-lived token in a config file instead" (`vars.rs:454`; `crates/ocx_config/src/loader.rs:1836`, parse errors scrub the source line because it "can hold a credential", `ocx_config/src/error.rs:37`). So a config file holding one credential class is accepted in this repo.

## Q6. Crypto dependencies that could read age files

- No `age`, `rage`, `chacha20poly1305`, `x25519-dalek` or HKDF-with-ChaCha package in `Cargo.lock` (grep by exact `name =`; absent). Direct workspace crypto deps: `sha2`, `secrecy`, `zeroize`, `p256` (`ecdsa`, `pem`, `pkcs8`) (`Cargo.toml:175,178,181,201`). Transitive, via the `sigstore` fork used to decrypt cosign `ENCRYPTED SIGSTORE PRIVATE KEY`: `scrypt` (0.11.0 and 0.12.0), `pbkdf2`, `aes` 0.8.4, `hkdf` 0.12.4, `curve25519-dalek` 4.1.3, `ring` 0.17.14, `aws-lc-rs` 1.18.2, `rustls` 0.23.46 (`Cargo.lock:32,268,1028,2914,5071,5808,5922,6160`). Whether any of these assemble into age's scheme (X25519 + ChaCha20-Poly1305 + scrypt recipients) without a new crate: age needs ChaCha20-Poly1305 and the age format code, neither present (inferred).
- Rules for adding: `.claude/rules/subsystem-deps.md:56-64` (license allowlist, advisories, "existing crate or stdlib solution exist? Minimize deps", maintenance check, workspace dep, `task license:deps`); license allowlist `deny.toml:31-44` (Apache-2.0, MIT, BSD-3, ISC and a few more; `age`/`rage` licensing not checked here); SEC-28 requires reading any dep's `build.rs`/proc-macro (`rust-quality/security.md:119`); EVO-10/EVO-11 require validating a crypto-backend-touching dep on the full release matrix (`rust-quality/current-apis.md:75-76`). `product-tech-strategy.md` lists no crypto choice and says only "follow this file, no deviations unless told". It lists no dependency-count budget; `quality-core.md` "Choose Boring Technology" budgets "~3 innovation tokens" and "Don't Own Non-Domain Code" is Block-tier for hand-rolled wire formats.
- An age feature would also need a key-input story (`OCX_AGE_KEY_FILE`, a second file path) so the plaintext key still lives in a file; this is a design observation, not read from the repo.

## Q7. Where a new public `OCX_*` variable must be reflected

Per `.claude/rules/subsystem-interface-contract.md` (counterpart table) and what the code/tests show for the existing `OCX_AUTH_*` family:

1. Declaration in `crates/ocx_env/src/vars.rs` (visibility `Public`, `OCX_` prefix, docs non-empty, secret-ness, propagation). Held by `crates/ocx_env/src/vars/tests.rs`: unique names, prefix-of-visibility, `a_credential_shaped_name_is_secret`, `every_declaration_is_documented`, `a_pattern_carries_exactly_one_slot`, `every_secret_declaration_is_credential_or_secret_input`, `credential_class_is_exactly_the_scrubbed_secrets`, plus `crates/ocx_env/src/tests.rs:193` (`credential_keys_are_exactly_the_five_scrubbed_credentials`, a count test that a new Credential-class variable edits).
2. `reference/environment.md`, generated: `task website:reference:generate`; held by `task website:reference:check` and `crates/ocx_schema/tests/docs.rs`. The renderer keeps a hand table `ENV_SEE` linking a variable to a guide (`crates/ocx_schema/src/docs/render.rs:659-711`, current OCX_AUTH rows 698-711).
3. `cli.json` golden `crates/ocx_schema/tests/golden/cli.json` (generated from the declarations; the OCX_AUTH TOKEN row is at `crates/ocx_schema/contract/baseline/cli.json:14859-14866`, with `secret` and `propagation`), and the same checked-in baseline under `crates/ocx_schema/contract/baseline/`. An added variable is additive; a ledger entry in `crates/ocx_schema/contract/ledger.toml` is needed only for a break (header text `ledger.toml:1-23`). Choice enum lists in `crates/ocx_schema/src/cli.rs:1480` are by-hand for Choice values (a new `Choice` value would need it).
4. SDK generator: `crates/ocx_sdkgen/tests/golden/rust/env.rs` golden, and `sdk::env::is_secret` (`crates/ocx_sdkgen/tests/sdk_rust.rs:329-352`).
5. If a Credential-class secret: credential table row in `.claude/rules/subsystem-cli.md` (`:417-437`) and the "never forwarded" statement in `environment.md` (`ocx_env/src/lib.rs:394-397`).
6. The fixed explicit-suffix lists: `crates/ocx_cli/src/app/seam.rs:513` (poisoned keys), `render.rs` `ENV_SEE`; the prefix-based masks already cover any `OCX_AUTH_*`.
7. Docs: `website/src/content/docs/guides/authenticate.mdx` (describes the three variables, `:59-67`); the guide is the only prose that lists them. Env reads must go through `ocx_env` (`std::env::var` is banned by the root `clippy.toml`, `subsystem-interface-contract.md` "Environment reads"; dependency-side reads like `DOCKER_CONFIG` need a `Foreign`+`reader = Dependency(...)` declaration, `vars.rs:742`).
8. Acceptance tests for env-auth behaviour live in `test/tests/test_credential_helper_reentry.py:328-374` (override beats helper, with a control run) and `test/tests/test_error_document.py:259-260`; login/store side in `test/tests/test_login.py`. A new variable has no required acceptance file by rule; the closest existing guard is the parametrised override test (new variable would add a row, inferred).
9. Name rule IC-13/IC-14: `OCX_` + SCREAMING(long flag departing from default); positive `OCX_X`, negative `OCX_NO_X` (`subsystem-interface-contract.md` style guide). IC-20: defaults are literals, env applies in `ContextOptions` (`app/context_options.rs`, mentioned in the rule's counterpart scope), not in clap. If a flag (`--env-file`) is added, `reference/command-line/**` regenerate, `cli.json` command golden, C01-C09 lints, `CONTRACT` entry only if stdout changes.
10. `ocx_config/src/ecosystem_api_pin.rs:34-39` pins the `SecretVar`/`EnvVar` API of `OCX_AUTH_*` for the lockstep satellite (ocx-mirror); `task satellite:verify` applies if `ocx_env` public API shape changes (`AGENTS.md` ecosystem tier).

## Q8. Prior attempts and decisions

- `.claude/artifacts/adr_ocx_login_credential_store.md` is the only ADR on credential input. It fixes the auth chain as env vars highest priority, then Docker config/helpers (`:20,175`: "Env-var auth remains highest-priority. `ocx login` writes to the store; env vars still override at read time for CI scenarios"). Eight block-tier requirements (`:182-193`): no `--password VALUE` flag, `--password-stdin` for non-TTY, helper via stdin, timeouts, atomic locked write, `SecretString`, `0600`. No mention of file-based token input or an `OCX_*` Docker-config override. Rejected options in the ADR are in-house helper code (B) and keyring-core (C) (`:85,109`), unrelated to the candidates here.
- `.claude/artifacts/research_credential_storage_security.md:41,90,122` backs the no-argv-secret rule; no `*_FILE` discussion (grep of the file for `_FILE`, `token file`, `vault`, `sops`, `age` found none).
- `git log --oneline -i --grep=auth --grep=credential --grep=token -n 40`: relevant commits only `a963f220` (helper installed through ocx no longer deadlocks), `e97ec299` (helper stderr >64 KiB), `5b4f181f` (log lines carry the auth cause chain), `cf9fa347` (`ocx exec` layers onto caller env; only `__OCX_ENV_STATE` and `OCX_BINARY_PIN` reach children), `1a22c8e1` (config parse errors no longer quote the source line, because it can hold a credential), `1affd092` (telemetry export; OCX_CONFIG as a path list). No commit about a token file, env-file or age.
- No artifact in `.claude/artifacts/` ruled any of A/B/C in or out (listing of filenames for `auth|cred|secret|token|login` plus the greps above).

## negative:

- `gh issue view 756` returned nothing (no auth in this environment): issue body and comments were not read; the candidate list is the brief's.
- No `*_FILE` secret variable, no env-file loader, no age/rage crate, no file-mode check on a secret file exist anywhere in the repo (grep over `crates/`).
- `get_podman_credential`/`REGISTRY_AUTH_FILE` support exists in the vendored dependency but is not wired: OCX does not honour `REGISTRY_AUTH_FILE`.
- Could not confirm UNC-path behaviour or the Windows HOME-vs-USERPROFILE mismatch by running anything (read-only, no Windows run); both are inferred from code.
- No test covers `OCX_AUTH_<slug>_USER` alone preceding the helper; docs disagree on that case (`authenticate.mdx:67` vs `vars.rs:484-485`).
- `.claude/rules/subsystem-oci.md` and `quality-security.md:69-70` state the auth chain in one line each and say nothing about a file input.
- `get_or_fallback` swallows all `AuthError`s, so none of the existing exit codes for auth-config errors (78 `auth_env_missing`) is reachable from `ocx package pull`; relevant if a file-read error is meant to be fatal.

## leads:

- Windows credential lookup: confirm on a real Windows run whether `docker_credential` finds `%USERPROFILE%\.docker\config.json` when `HOME` is unset (affects every candidate and the "already works" answer).
- Error-surface lane: make a credential-file read failure an explicit exit (74/80/78) instead of the `auth.rs:63-75` warn-and-anonymous path.
- Credential scrub for patterns: teach `ocx_env::credential_keys()` prefix patterns so `OCX_AUTH_*_TOKEN` stops propagating to children (`subsystem-cli.md:434`), relevant before adding a second token source.
- Docker-config location override: whether `REGISTRY_AUTH_FILE` or an `OCX_*` equivalent (a file, not a directory) should be wired through `docker_credential` (it is the fork we own, `external/docker_credential`).
- Contract cost of one more slot variable: dry-run the contract tooling (`task contract:*`, golden regen, `ocx-sdkgen` golden) to size candidate A's change set.
- age crate vetting: licence, `build.rs`, release-matrix build (musl, Windows arm64) per SEC-28/EVO-11, only if candidate C stays on the table.
- Env-file loader semantics: how a loader would interact with `is_reserved_ocx_key` and the "checked-in file could reconfigure resolution" threat model (`ocx_env/src/lib.rs:445-452`).
- Upstream docs consistency: reconcile `environment.md:89` / `vars.rs:484-485` with `authenticate.mdx:67` on the lone-USER case.
