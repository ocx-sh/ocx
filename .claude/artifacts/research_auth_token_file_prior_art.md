# Research: Non-interactive credentials from files in CLIs (registry/API tokens)

## Metadata

**Date:** 2026-11-22
**Domain:** cli, security, packaging
**Triggered by:** feature request to supply private-registry credentials to OCX from a file, so filesystem ACLs can lock the secret to a user (batch HPC, Windows UNC shares). Directions: (A) `OCX_AUTH_<REGISTRY>_TOKEN_FILE`; (B) `--env-file`; (C) age-encrypted secrets; (D) no new mechanism, use Docker `config.json` / containers `auth.json`.
**Expires:** 2027-05-22 (re-verify). Pages fetched via a summarising fetcher, not read verbatim; items marked UNVERIFIED were not confirmed.

## Direct Answer

Findings only, no pick. Balance per direction:

- A (`*_FILE`): the dominant convention for container entrypoints and several first-party CLIs (kubectl, AWS, GCP, libpq, restic); semantics differ per tool (conflict rule, trimming, perms, rotation). No mainstream OCI client has it.
- B (`--env-file`): widely shipped (docker, compose, node, uv, mise) but the dotenv dialect is unspecified and the file is a channel for ALL variables; no tool documents it as a credential mechanism with perms checks.
- C (age): real adoption (sops, mise, fnox, chezmoi, flux) but every adopter adds key management, and the key file just moves the "file locked by ACL" problem one step.
- D (existing format): every OCI client reads Docker `config.json` and most accept an arbitrary path; the gap is that file-per-user and path override are not uniform, and Windows/UNC is untested here.

## Technology Landscape

### Trending (gaining momentum)

| Tool/Pattern | Adoption Signal | Key Benefit | Relevance |
|---|---|---|---|
| Credential providers/helpers (cargo, bazel, docker) | cargo `cargo:token-from-stdout`, external plugins ([cargo](https://doc.rust-lang.org/cargo/reference/registry-authentication.html)); Bazel `--credential_helper` ([proposal, 2022, re-verify](https://github.com/bazelbuild/proposals/blob/main/designs/2022-06-07-bazel-credential-helpers.md)) | no plaintext file at all | alternative to A-D; subprocess seam |
| systemd credentials | [systemd.io](https://systemd.io/CREDENTIALS/) | file per credential, not inherited by children, kernel-enforced access | the OS-native form of "file not env" |
| age/sops-style encrypted-in-repo secrets | sops ~23.8k stars, CNCF sandbox ([repo](https://github.com/mozilla/sops)); fnox ([site](https://fnox.jdx.dev/)) | git-committable secrets | direction C |

### Established (proven, widely accepted)

| Tool/Pattern | Status | Notes |
|---|---|---|
| `--password-stdin` | standard | docker, helm, oras, regctl ([docker](https://docs.docker.com/reference/cli/docker/login/), [helm](https://helm.sh/docs/helm/helm_registry_login/), [oras](https://oras.land/docs/commands/oras_login/), [regctl](https://regclient.org/usage/regctl/)) |
| Docker `config.json` as shared format | standard | read by docker, podman/skopeo/buildah, go-containerregistry, apptainer, regctl |
| `*_FILE` env var convention | standard for images; real in some CLIs | see Key findings |
| Path-override env var for a credentials file | standard | `AWS_SHARED_CREDENTIALS_FILE`, `PGPASSFILE`, `GOOGLE_APPLICATION_CREDENTIALS`, `REGISTRY_AUTH_FILE`, `RATTLER_AUTH_FILE`, `NETRC` |

### Emerging

| Tool/Pattern | Signal | Worth Watching Because |
|---|---|---|
| mise age values (experimental) | [mise docs](https://mise.jdx.dev/environments/secrets/) | package-manager-adjacent tool embedding age |
| fnox | v1.50.0 per [site](https://fnox.jdx.dev/) | secrets-to-env broker beside mise |

### Declining

| Tool/Pattern | Signal | Avoid Because |
|---|---|---|
| Plaintext tokens in env as sole channel | OWASP: "not recommended unless the other methods are not possible" ([OWASP](https://cheatsheetseries.owasp.org/cheatsheets/Secrets_Management_Cheat_Sheet.html)) | inheritance, dumps |
| Service-account key files (GCP) | "not recommended" ([GCP ADC](https://docs.cloud.google.com/docs/authentication/application-default-credentials)) | long-lived file credential; still the documented path via env |

## Key findings

### Axis 1 - Conventions

1. `_FILE` in images: Docker secrets docs say Docker does not set env vars from secrets (leak via `--link` etc.), and images offer `_FILE` variants, e.g. `WORDPRESS_DB_PASSWORD_FILE`; secrets mount at `/run/secrets/<name>` ([Docker secrets](https://docs.docker.com/engine/swarm/secrets/); page age not shown, re-verify). The convention is implemented by entrypoint shell, e.g. mysql `file_env` ([source](https://github.com/docker-library/mysql/blob/master/docker-entrypoint.sh)). Origin date not verified.
2. `_FILE`/path in real CLIs: `AWS_WEB_IDENTITY_TOKEN_FILE` (file contents sent as the token) ([AWS CLI v1 page, v1 in maintenance mode](https://docs.aws.amazon.com/cli/v1/userguide/cli-configure-envvars.html)); `GOOGLE_APPLICATION_CREDENTIALS` path to JSON ([GCP](https://docs.cloud.google.com/docs/authentication/application-default-credentials)); `PGPASSFILE` ([libpq](https://www.postgresql.org/docs/current/libpq-pgpass.html)); `RESTIC_PASSWORD_FILE` ([restic](https://restic.readthedocs.io/en/stable/075_scripting.html)); kubeconfig `tokenFile` ([k8s](https://kubernetes.io/docs/reference/config-api/kubeconfig.v1/)). A general `_FILE` suffix on a token var in a CLI (as opposed to path-of-config-file) is rarer; I found only restic and AWS as close matches. Not proven exhaustive.
3. Counter-examples with no file variable: cargo takes `CARGO_REGISTRIES_<NAME>_TOKEN` plus providers (incl. `token-from-stdout`), not a token-file var ([cargo](https://doc.rust-lang.org/cargo/reference/registry-authentication.html)); gh documents `GH_TOKEN`/`GITHUB_TOKEN` only, plus `GH_CONFIG_DIR` ([gh](https://cli.github.com/manual/gh_help_environment)); Homebrew documents token vars and `brew.env`, no `_FILE` ([brew](https://docs.brew.sh/Manpage), second pass; an earlier fetch summary claimed `_FILE` variants and was contradicted, treat as absent); Vault: `VAULT_TOKEN` + `~/.vault-token` helper, no `VAULT_TOKEN_FILE` documented ([vault](https://developer.hashicorp.com/vault/docs/commands#environment-variables)).
4. systemd: `LoadCredential=` places one file per credential under `$CREDENTIALS_DIRECTORY`; access restricted to the service user, not passed down the process tree, fixed for service lifetime ([systemd](https://systemd.io/CREDENTIALS/)). It states env vars are inherited down the tree and files are weak unless access-controlled.
5. Per-tool file-in-env summaries: npm `.npmrc` interpolates `${VAR}` ([npm](https://docs.npmjs.com/cli/v10/configuring-npm/npmrc)), the inverse of A.

### Axis 2 - OCI registry clients

| Client | File credential | Arbitrary path | Source |
|---|---|---|---|
| docker | `config.json`, `--password-stdin`, credential stores; base64 plaintext if no store | `DOCKER_CONFIG` (not on the login page; documented by go-containerregistry) | [docker login](https://docs.docker.com/reference/cli/docker/login/) |
| podman/skopeo/buildah | `auth.json`; falls back to `~/.docker/config.json` | `--authfile`, `REGISTRY_AUTH_FILE` | [podman-login](https://docs.podman.io/en/latest/markdown/podman-login.1.html), [containers-auth.json](https://github.com/containers/image/blob/main/docs/containers-auth.json.5.md) |
| go-containerregistry (crane) | Docker config, then podman `auth.json`; helpers | `DOCKER_CONFIG`; `REGISTRY_AUTH_FILE` not confirmed | [authn README](https://github.com/google/go-containerregistry/blob/main/pkg/authn/README.md) |
| oras | `--password-stdin`, `--identity-token-stdin` | `--registry-config path` | [oras login](https://oras.land/docs/commands/oras_login/) |
| helm | `--password-stdin` | `--registry-config` (default `~/.config/helm/registry/config.json`); env var not confirmed | [helm](https://helm.sh/docs/helm/helm_registry_login/) |
| regctl | reads Docker creds by default; `--pass-stdin`; clear-text own config | `REGCTL_CONFIG` | [regctl](https://regclient.org/usage/regctl/) |
| apptainer | `~/.docker/config.json`, `remote.yaml`; `APPTAINER_DOCKER_USERNAME/PASSWORD` override stored creds and `--authfile`; credential helpers NOT supported | `--authfile` (docs name batch processes as a use case) | [docker_and_oci](https://apptainer.org/docs/user/main/docker_and_oci.html), [registry 1.4](https://apptainer.org/docs/user/1.4/registry.html) |

Pattern: no OCI client found offers `*_TOKEN_FILE`; the shared answer is a Docker-format JSON at a movable path. Apptainer's HPC-facing docs steer to env vars or `--authfile`, and warn authfiles are unencrypted so "location, ownership, and permissions need care" ([docker_and_oci](https://apptainer.org/docs/user/main/docker_and_oci.html)).

### Axis 3 - Package/tool managers

- cargo: env `CARGO_REGISTRIES_<NAME>_TOKEN` works only if `cargo:token` provider listed; `credentials.toml` plaintext; subprocess provider output has newlines trimmed ([registry-auth](https://doc.rust-lang.org/cargo/reference/registry-authentication.html), [config](https://doc.rust-lang.org/cargo/reference/config.html#credentials)).
- uv: URL creds, then netrc (`NETRC` var or `~/.netrc`), uv credentials store (plaintext TOML), keyring (off by default) ([uv](https://docs.astral.sh/uv/concepts/authentication/http/)); `UV_ENV_FILE` for `uv run` ([env ref](https://docs.astral.sh/uv/reference/environment/)). Index `UV_INDEX_*` page not retrieved (404): UNVERIFIED.
- pixi/rattler: keychain, else `~/.rattler/credentials.json`; `RATTLER_AUTH_FILE` replaces all other sources when set; `--token` for CI ([pixi](https://pixi.prefix.dev/latest/deployment/authentication/)).
- Nix: `netrc-file` (absolute path, `~` not expanded), `access-tokens` host=token; docs give no perms requirement ([nix.conf](https://nix.dev/manual/nix/stable/command-ref/conf-file.html)).
- Bazel: `--credential_helper`; rationale lists `.netrc` as plaintext and flags as unergonomic ([proposal, 2022](https://github.com/bazelbuild/proposals/blob/main/designs/2022-06-07-bazel-credential-helpers.md)).
- Homebrew: token env vars; `brew.env` files with no shell expansion ([brew](https://docs.brew.sh/Manpage)).
- mise, aqua, cargo-binstall: mise secrets documented ([mise](https://mise.jdx.dev/environments/secrets/)); aqua page 404, cargo-binstall not fetched: UNVERIFIED.

### Axis 4 - Semantics where a token-file mechanism exists

- Both direct and file variable set: error: mysql image `file_env` aborts "Both $var and $fileVar are set (but are exclusive)" ([source](https://github.com/docker-library/mysql/blob/master/docker-entrypoint.sh)). Precedence: kubeconfig `tokenFile` overrides `token` ([k8s](https://kubernetes.io/docs/reference/config-api/kubeconfig.v1/)); AWS: web-identity env var overrides the profile setting ([AWS](https://docs.aws.amazon.com/cli/v1/userguide/cli-configure-envvars.html)); restic: CLI flag beats its env var, but ranking among the three password vars is unstated ([restic](https://restic.readthedocs.io/en/stable/075_scripting.html)); pixi: file var replaces all other sources ([pixi](https://pixi.prefix.dev/latest/deployment/authentication/)).
- Trailing newline: mysql `$(< file)` strips trailing newlines implicitly (inference from bash semantics, the page text does not say so); cargo trims newlines on provider stdout ([cargo](https://doc.rust-lang.org/cargo/reference/registry-authentication.html)); restic: unstated (UNVERIFIED); curl `--variable name@file` offers explicit `trim` function ([curl](https://curl.se/docs/manpage.html)).
- Permission checks: ssh ignores a private key file accessible by others ([ssh](https://man.openbsd.org/ssh#FILES)); libpq ignores a group/world-accessible `.pgpass` on Unix and on Windows does no check, "assumed to store the file in a secure directory" ([libpq](https://www.postgresql.org/docs/current/libpq-pgpass.html)). Podman, containers-auth, apptainer, nix, cargo docs state no permission rule (cargo's 0600 write is UNVERIFIED). Windows uses ACLs, not mode bits ([Microsoft](https://learn.microsoft.com/en-us/windows/win32/fileio/file-security-and-access-rights)); I found no CLI doc that verifies ACLs on credential files (ssh-on-Windows ACL behaviour: not verified).
- When read: kubeconfig `tokenFile` periodically re-read, last good value kept on read failure ([k8s](https://kubernetes.io/docs/reference/config-api/kubeconfig.v1/)); systemd credential fixed for service lifetime ([systemd](https://systemd.io/CREDENTIALS/)); uv credentials not cached across invocations ([uv](https://docs.astral.sh/uv/concepts/authentication/http/)); OWASP says rotate regularly, sidecar refresh ([OWASP](https://cheatsheetseries.owasp.org/cheatsheets/Secrets_Management_Cheat_Sheet.html)).
- Missing/unreadable/empty: mysql `file_env` does not check existence ([source](https://github.com/docker-library/mysql/blob/master/docker-entrypoint.sh)); Node `--env-file` errors on a missing file, `--env-file-if-exists` does not ([node](https://nodejs.org/api/cli.html#--env-fileconfig)); Nix `include` errors on missing, `!include` tolerates ([nix](https://nix.dev/manual/nix/stable/command-ref/conf-file.html)).
- Passed to children: a path var is inherited like any env var (harmless itself); the content is not exposed that way. systemd explicitly does not inherit credential data ([systemd](https://systemd.io/CREDENTIALS/)). mysql `file_env` exports the resolved secret and unsets `VAR_FILE`, so the secret re-enters the env ([source](https://github.com/docker-library/mysql/blob/master/docker-entrypoint.sh)).

### Axis 5 - `--env-file` / dotenv

- Shipped by: docker run (`#` only at line start; no quoting/export/interpolation docs on page) ([docker run](https://docs.docker.com/reference/cli/docker/container/run/#env)); Compose (quotes, `${}` interpolation, multiline single-quoted, precedence shell > `--env-file` > project `.env`) ([compose](https://docs.docker.com/compose/how-tos/environment-variables/variable-interpolation/)); Node (env beats file; multiline; `export` ignored; backticks) ([node](https://nodejs.org/api/cli.html#--env-fileconfig)); uv `UV_ENV_FILE` scoped to `uv run` ([uv](https://docs.astral.sh/uv/reference/environment/)); mise `_.file` ([mise](https://mise.jdx.dev/environments/secrets/)). just/task/bun/deno: not fetched, UNVERIFIED.
- Dialect: no spec; parsers differ on inline `#`, interpolation, escapes, backticks; Ruby dotenv treated as de facto reference (search snippets only, not opened: [dotenv-spec](https://github.com/php-xdg/dotenv-spec), [env.dev](https://env.dev/guides/env-file-syntax), [dotenvy PR](https://github.com/allan2/dotenvy/pull/3)). Homebrew's `brew.env` deliberately refuses expansion/command execution ([brew](https://docs.brew.sh/Manpage)).
- Secret-channel commentary: mise says redaction does not encrypt and `mise env` prints real values ([mise](https://mise.jdx.dev/environments/secrets/)); loaded values land in the process env anyway, with every env leak path (axis 7).

### Axis 6 - Encrypted secrets in CLIs

- age: identity files, passphrase, plugins; README says passphrase-protected identity is "not necessary for most use cases, where access to the encrypted identity file implies access to the whole system" ([age](https://github.com/FiloSottile/age/blob/main/README.md)). Rust impl rage: `age` crate is the library, dual MIT/Apache; no audit mentioned on page; `unstable` feature flag exists ([rage](https://github.com/str4d/rage)).
- mise: sops/age values decrypted whenever config loads, needs an identity or `sops` CLI; experimental ([mise](https://mise.jdx.dev/environments/secrets/)). fnox: age secrets in `fnox.toml`, recipients managed per teammate ([fnox](https://fnox.jdx.dev/)). chezmoi: external `age` if on PATH else builtin without passphrase/SSH-key support, since prompting each decrypt is tiresome ([chezmoi](https://www.chezmoi.io/user-guide/encryption/age/)). Flux: private key in a cluster Secret, back-up burden, per-cluster key ([flux](https://fluxcd.io/flux/guides/mozilla-sops/)). helm-secrets: not fetched, UNVERIFIED.
- Cost: key file or passphrase must itself be supplied non-interactively, so a batch job needs an unlocked identity file with the same ACL need as a token file.

### Axis 7 - HPC/batch

- Slurm: `sbatch --export` default `ALL` propagates the submission environment; `NONE`/list restrict it ([sbatch](https://slurm.schedmd.com/sbatch.html)). `AccountingStoreFlags=job_env` can store a batch job's submission environment in the accounting DB ([slurm.conf](https://slurm.schedmd.com/slurm.conf.html)); who can read it was not confirmed. `scontrol write batch_script` is limited to owner/privileged ([scontrol](https://slurm.schedmd.com/scontrol.html)). Whether `scontrol show job` prints env: the field list read showed no env fields, not confirmed either way (UNVERIFIED).
- `/proc/<pid>/environ` is gated by a ptrace read check, reflects the env at exec ([proc(5)](https://man7.org/linux/man-pages/man5/proc_pid_environ.5.html)); so same-user and root only, not other users by default (my reading; mode details not on page).
- Env-vs-file critique: environment is "generally accessible to all processes and may be included in logs or system dumps" ([OWASP](https://cheatsheetseries.owasp.org/cheatsheets/Secrets_Management_Cheat_Sheet.html)); inherited by children ([systemd](https://systemd.io/CREDENTIALS/)). Counter: a file in the home directory is still readable by any code running as the user, so the gain is fewer leak paths, not isolation ([search summary, secondary: Socket.dev/Doppler](https://socket.dev/glossary/environment-variables.md)).
- Site practice: apptainer HPC docs and site guides (Stanford Sherlock, EPFL) show env vars or `remote login`; I found no site recommending a chmod-600 token file for registries ([search](https://scitas-doc.epfl.ch/advanced-guide/singularity-docker/), [Sherlock](https://www.sherlock.stanford.edu/docs/software/containers/apptainer)); search-level evidence only.
- Windows UNC shares: not researched beyond ACL model; no CLI evidence found on credential files on UNC paths (UNVERIFIED).

## Evidence per direction

| Dir | For | Against |
|---|---|---|
| A | kubectl, AWS, restic, libpq, systemd all do file-not-env; maps to Docker/systemd secrets; error-on-both is a precedent; no new format | no OCI client or cargo/gh/brew/vault has it; per-registry var count grows (`OCX_AUTH_<REG>_*`); mysql re-exports secret; perms/trim/rotation semantics must be invented; apptainer env overrides file |
| B | shipped in docker, compose, node, uv, mise; one file for token/user/type; Node precedence env>file is a clean model | dialect unspecified; carries every OCX_* var, not just secrets; plaintext values end up in env anyway; no tool documents perms checks; docker `--env-file` page lacks quoting rules |
| C | sops/mise/fnox/chezmoi/flux adoption; encrypted at rest in repos | identity file needs the same ACL; extra crate (`age`/rage: unstable flag, no audit noted); key management burden; mise decrypts to plaintext in env; no registry client does it |
| D | all OCI clients read Docker format; `--authfile`/`REGISTRY_AUTH_FILE`/`--registry-config` give arbitrary path; zero new surface | docker `config.json` plaintext base64 without store; apptainer reads only direct entries, no helpers; one blob per path, not per-registry file; Windows/UNC not evidenced; OCX must confirm it honours `DOCKER_CONFIG` (not checked here) |

## Sources (URL - what it covers)

- https://docs.docker.com/reference/cli/docker/login/ - `--password-stdin`, config.json, stores
- https://docs.docker.com/engine/swarm/secrets/ - `/run/secrets`, `_FILE` rationale
- https://github.com/docker-library/mysql/blob/master/docker-entrypoint.sh - `file_env` exclusivity
- https://github.com/containers/image/blob/main/docs/containers-auth.json.5.md - auth.json lookup
- https://docs.podman.io/en/latest/markdown/podman-login.1.html - `--authfile`, `REGISTRY_AUTH_FILE`
- https://github.com/google/go-containerregistry/blob/main/pkg/authn/README.md - keychain
- https://oras.land/docs/commands/oras_login/ - oras flags
- https://helm.sh/docs/helm/helm_registry_login/ - helm flags
- https://regclient.org/usage/regctl/ - regctl creds
- https://apptainer.org/docs/user/main/docker_and_oci.html , https://apptainer.org/docs/user/1.4/registry.html , https://apptainer.org/docs/user/main/endpoint.html - apptainer auth
- https://doc.rust-lang.org/cargo/reference/registry-authentication.html , https://doc.rust-lang.org/cargo/reference/config.html#credentials - cargo
- https://docs.astral.sh/uv/concepts/authentication/http/ , https://docs.astral.sh/uv/reference/environment/ - uv
- https://pixi.prefix.dev/latest/deployment/authentication/ - pixi/rattler
- https://nix.dev/manual/nix/stable/command-ref/conf-file.html - nix
- https://github.com/bazelbuild/proposals/blob/main/designs/2022-06-07-bazel-credential-helpers.md - bazel helper (2022, old)
- https://docs.brew.sh/Manpage - Homebrew vars, brew.env
- https://cli.github.com/manual/gh_help_environment - gh
- https://developer.hashicorp.com/vault/docs/commands#environment-variables - vault
- https://kubernetes.io/docs/reference/config-api/kubeconfig.v1/ - `tokenFile`
- https://docs.aws.amazon.com/cli/v1/userguide/cli-configure-envvars.html - AWS (v1 doc, old)
- https://docs.cloud.google.com/docs/authentication/application-default-credentials - GCP
- https://www.postgresql.org/docs/current/libpq-pgpass.html - pgpass perms, Windows
- https://restic.readthedocs.io/en/stable/075_scripting.html - restic vars
- https://man.openbsd.org/ssh#FILES - ssh key perms
- https://learn.microsoft.com/en-us/windows/win32/fileio/file-security-and-access-rights - Windows ACLs
- https://systemd.io/CREDENTIALS/ - systemd credentials
- https://cheatsheetseries.owasp.org/cheatsheets/Secrets_Management_Cheat_Sheet.html - env vs files
- https://man7.org/linux/man-pages/man5/proc_pid_environ.5.html - environ access
- https://slurm.schedmd.com/sbatch.html , https://slurm.schedmd.com/slurm.conf.html , https://slurm.schedmd.com/scontrol.html - Slurm env
- https://nodejs.org/api/cli.html#--env-fileconfig - Node env-file
- https://docs.docker.com/reference/cli/docker/container/run/#env , https://docs.docker.com/compose/how-tos/environment-variables/variable-interpolation/ - docker/compose env files
- https://github.com/php-xdg/dotenv-spec , https://env.dev/guides/env-file-syntax - dotenv dialects (search snippets only)
- https://mise.jdx.dev/environments/secrets/ , https://fnox.jdx.dev/ , https://www.chezmoi.io/user-guide/encryption/age/ , https://fluxcd.io/flux/guides/mozilla-sops/ - age/sops adopters
- https://github.com/FiloSottile/age/blob/main/README.md , https://github.com/str4d/rage , https://github.com/mozilla/sops - age, rage, sops
- https://scitas-doc.epfl.ch/advanced-guide/singularity-docker/ , https://www.sherlock.stanford.edu/docs/software/containers/apptainer - HPC site docs (search results)

negative:
- No OCI registry client found (docker, podman, oras, crane, helm, regctl, apptainer) offering a `*_TOKEN_FILE`-style variable; they converge on a Docker-format JSON at a configurable path.
- Common assumption "`_FILE` is a CLI convention" is only partly true: it originated in image entrypoints (mysql `file_env`); cargo, gh, brew, vault document none. restic, AWS, kubeconfig are the real CLI matches.
- A first Homebrew fetch claimed `_FILE` variants; second pass found none. Treated as false.
- Compose docs and cargo docs state no permission requirement; only ssh and libpq verify mode bits, and libpq explicitly skips the check on Windows. No tool found that checks ACLs.
- No HPC site doc found recommending a permission-locked registry token file; env vars are the documented norm there (search evidence).
- Slurm: no confirmed evidence that `scontrol show job` prints env; `job_env` accounting storage is the confirmed exposure path, reader access unverified.
- Gaps: aqua page 404, uv index-auth page 404, restic source 404, cargo-binstall/just/task/bun/deno/helm-secrets/gh hosts.yml not checked, UNC-path behaviour not checked, whether OCX honours `DOCKER_CONFIG` not checked in code.

leads:
- OCX auth code audit: confirm which Docker-config paths/vars OCX already honours (`DOCKER_CONFIG`, `REGISTRY_AUTH_FILE`) before judging direction D.
- Credential-helper seam: cargo `token-from-stdout`/Bazel helper as an option beyond A-D; check docker-credential-helper protocol reuse.
- Windows ACL verification: how a Rust CLI can check/enforce user-only ACLs on a file and on UNC shares (and whether to at all).
- Slurm job_env exposure: confirm PrivateData/`sacct` reader rules and whether `scontrol show job` prints env, for the env-weakness claim.
- Per-registry var naming: how `OCX_AUTH_<REGISTRY>_*` normalises hostnames (ports, dots), since A multiplies this surface.
- restic/AWS source reading: precedence and trimming behaviour in code to settle axis-4 gaps.
