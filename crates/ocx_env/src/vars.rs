// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Every environment variable OCX reads, declared once.

crate::env_vars! {
    // ── Core configuration ──

    /// Absolute path to the running `ocx`, set on every spawn so a child ocx runs the
    /// same binary, not whatever `$PATH` resolves.
    pub OCX_BINARY_PIN: Path, Public, child = Forward;
    /// The OCX data root — `$OCX_HOME`, else `~/.ocx`.
    ///
    /// Forwarded set-always as the resolved root, or an `ocx exec --clean` child (no `HOME`)
    /// resolves another root and misses the package the parent just materialized.
    pub OCX_HOME: Path, Public, child = Forward;
    /// Boolean — disables network access when truthy. Mirrors `--offline`.
    pub OCX_OFFLINE: Bool, Public, on_invalid = Error, child = Forward;
    /// Boolean — a tag-only reference missing from the local index errors instead of
    /// being fetched; digest-pinned content still fetches. Mirrors `--frozen`.
    pub OCX_FROZEN: Bool, Public, on_invalid = Error, child = Forward;
    /// Boolean — uses the remote index by default when truthy. Mirrors `--remote`.
    pub OCX_REMOTE: Bool, Public, child = Forward;
    /// Path to an explicit configuration file. Mirrors `--config`.
    pub OCX_CONFIG: Path, Public, child = Forward;
    /// Boolean — skip the discovered config-tier chain; explicit `--config` / `OCX_CONFIG`
    /// paths still load and a system-scope lock survives. Forwarded to child ocx.
    pub OCX_NO_CONFIG: Bool, Public, child = Forward;
    /// Path to an explicit project `ocx.toml`. Mirrors `--project`.
    pub OCX_PROJECT: Path, Public, child = Forward;
    /// Boolean — skip the CWD walk and `OCX_PROJECT`; explicit `--project` paths still load.
    /// Forwarded only when this invocation resolved no project.
    pub OCX_NO_PROJECT: Bool, Public, child = Forward;
    /// Path at which the CWD walk for a project `ocx.toml` stops; relative values join onto the
    /// working directory. An explicit `--project` or `OCX_PROJECT` is not bounded by it.
    pub OCX_CEILING_PATH: Path, Public;
    /// Boolean — commands that stamp shell-activation consent as a side effect write none;
    /// `--consent` / `--no-consent` outrank it and `ocx shell allow` ignores it.
    ///
    /// Forwarded, else a script run by `ocx exec` that calls `ocx pull` stamps after all.
    pub OCX_NO_CONSENT: Bool, Public, on_invalid = Error, child = Forward;
    /// Boolean — select the global toolchain (`$OCX_HOME/ocx.toml`). Mirrors `--global`.
    pub OCX_GLOBAL: Bool, Public, child = Forward;
    /// Path to the local index directory. Mirrors `--index`.
    pub OCX_INDEX: Path, Public, child = Forward;
    /// The registry a bare identifier resolves under; outranks `[registry] default`, else
    /// `ocx.sh`. Empty is unset. Forwarded so every frame of a launch chain agrees.
    pub OCX_DEFAULT_REGISTRY: String, Public, child = Forward;
    /// Comma-separated `host[:port]` authorities that may be dialled over plain HTTP.
    ///
    /// Forwarded, or a child with a narrower set cannot reach a registry the parent just pulled from.
    pub OCX_INSECURE_REGISTRIES: HostList, Public, child = Forward;
    /// Boolean — allow a tag resolving to a yanked index entry. Forwarded to child ocx.
    pub OCX_ALLOW_YANKED: Bool, Public, child = Forward;

    // ── Resolution and launch state ──

    /// Path to the active patch snapshot file, whose pinned digests win over live tag lookups.
    /// Forwarded to child ocx.
    pub OCX_PATCH_SNAPSHOT: Path, Public, child = Forward;
    /// JSON object mapping an upstream host to a mirror string or `{"registry"?, "index"?}`,
    /// the same union `[mirrors."<host>"]` accepts. Forwarded to child ocx.
    pub OCX_MIRRORS: Json, Public, child = Forward;
    /// JSON object encoding the resolved `[patches]` config; forwarded only when configured.
    pub OCX_PATCHES: Json, Public, child = Forward;
    /// JSON envelope of the resolved project `[env]` entries plus `ocx exec --env` overrides,
    /// or a launcher re-entry silently reverts them.
    ///
    /// Decode fails closed on the whole envelope (reserved key, unknown `kind`): a forged entry
    /// can set a value, so the leniency of `OCX_PATCHES` must not be copied.
    pub OCX_ENV: Json, Public, child = Forward;
    /// JSON map from a content digest to the `registry/repository[:tag]` names one composition
    /// resolved it under; read by `ocx launcher exec` to match targeted patch rules.
    ///
    /// Written by ocx only while a `[patches]` tier is in effect; reserved, so `[env]` and
    /// `--env` cannot set it. Never forwarded explicitly, or a launcher nested under another
    /// launcher loses the outer composition's names.
    pub OCX_LAUNCH_IDENTITIES: Json, Public;
    /// Boolean — `ocx self setup` modifies no shell profile. Mirrors `--no-modify-path`.
    pub OCX_NO_MODIFY_PATH: Bool, Public;
    /// OCI reference overriding `[managed].source` for this invocation only; empty is unset,
    /// `OCX_NO_CONFIG` suppresses it. Forwarded to child ocx.
    pub OCX_MANAGED_CONFIG: String, Public, child = Forward;
    /// Boolean — kill switch for the managed-config background refresh; `ocx config update` still works.
    ///
    /// Forwarded, or a child's refresh replaces the managed tier with config the parent never saw.
    pub OCX_NO_CONFIG_REFRESH: Bool, Public, child = Forward;
    /// Directory execution records are written to; absent or empty turns recording off.
    /// Forwarded so every frame of a launch chain records into the outermost sink.
    pub OCX_RECORDS_DIR: Path, Public, child = Forward;
    /// Filename template for each execution record; forwarded alongside `OCX_RECORDS_DIR`.
    ///
    /// No `OCX_RECORDS_REQUIRED` exists: fail-closed recording is config-file-only operator policy.
    pub OCX_RECORDS_NAME: String, Public, child = Forward;

    // ── Toolchain ──

    /// Lazy materialisation mode (`never` / `always`), the weakest tier of the `lazy-mode` ladder.
    ///
    /// Not forwarded: it changes when content materializes, never which digest resolves.
    pub OCX_LAZY_MODE: Choice["never", "always"], Public;
    /// Lazy materialisation report (`silent` / `progress`); not forwarded, like `OCX_LAZY_MODE`.
    pub OCX_LAZY_REPORT: Choice["silent", "progress"], Public;
    /// Toolchain activation mode (`env` / `bin` / `none`), the weakest tier of the `activate`
    /// ladder. Not forwarded, like `OCX_LAZY_MODE`.
    pub OCX_TOOLCHAIN_ACTIVATE: Choice["env", "bin", "none"], Public;
    /// Boolean — composed paths pin to digest roots; the weakest tier of the `pinned` ladder.
    ///
    /// Unset and an explicit `false` are different answers.
    pub OCX_TOOLCHAIN_PINNED: TriBool, Public;
    /// Root under which project toolchain homes render — the env tier of `toolchain_dir`.
    ///
    /// Forwarded set-or-remove; the containment refusals apply to it like to every tier.
    pub OCX_TOOLCHAIN_DIR: Path, Public, child = Forward;
    /// Boolean — skip the policy-gated auto-verify on install/pull; `--no-verify` wins. Forwarded to child ocx.
    pub OCX_NO_VERIFY: Bool, Public, on_invalid = Error, child = Forward;

    // ── Shell activation ──

    /// Project directories (separated like `PATH`) the per-prompt shell hook may activate in
    /// without a consent stamp, subtrees included.
    pub OCX_CONSENT_PATHS: PathList, Public;
    /// Comma-separated OCI namespaces; a project whose whole lock names sources inside them
    /// activates without a directory grant or a consent stamp.
    pub OCX_CONSENT_NAMESPACES: String, Public;
    /// Boolean — `ocx self activate` skips the shell-completion block.
    pub OCX_NO_COMPLETION: Bool, Public;
    /// Boolean — disables the per-prompt shell reconciler; `ocx self activate` still runs at shell start.
    pub OCX_NO_HOOK: Bool, Public;

    // ── Output, logging and runtime ──

    /// Log level, as `--log-level` accepts it; the flag wins.
    pub OCX_LOG_LEVEL: String, Public;
    /// Log level for console messages only; outranks `OCX_LOG_LEVEL` there.
    pub OCX_LOG_CONSOLE: String, Public;
    /// Boolean — suppresses the stdout report and the transfer progress bars.
    pub OCX_QUIET: Bool, Public;
    /// Maximum number of root packages pulled in parallel.
    pub OCX_JOBS: Integer, Public;
    /// Boolean — kill switch for the background self-update and toolchain drift checks; forwarded to child ocx.
    pub OCX_NO_UPDATE_CHECK: Bool, Public, child = Forward;
    /// Throttle interval of the background update checks, `\d+[smhd]?` (bare digits = seconds).
    ///
    /// Beats `[update] interval`. Not forwarded: a personal preference, like `OCX_LAZY_MODE`.
    pub OCX_UPDATE_CHECK_INTERVAL: String, Public;
    /// `RefreshPolicy` wire value for ocx's own update check; beats `[update] self`. Not forwarded.
    pub OCX_SELF_UPDATE: Choice["apply", "notify", "manual"], Public;
    /// `RefreshPolicy` wire value for the toolchain drift check; beats `[update] toolchain`. Not forwarded.
    pub OCX_TOOLCHAIN_UPDATE: Choice["apply", "notify", "manual"], Public;
    /// Boolean — skip ad-hoc code signing of macOS binaries after installation.
    pub OCX_NO_CODESIGN: Bool, Public;

    // ── Credentials and certificates ──

    /// Extra CA roots — a path or inline PEM (contains `-----BEGIN`); empty is unset.
    ///
    /// Not forwarded: an inline PEM can exceed Windows' 32,767-character per-variable limit.
    pub OCX_EXTRA_CA_CERTS: PathOrPem, Public;
    /// Path (bare or `file://`) to a Sigstore trusted-root JSON document, or a directory holding
    /// `trusted_root.json`, that `ocx package verify` loads its trust material from.
    pub OCX_SIGSTORE_TRUSTED_ROOT: Path, Public;
    /// OIDC bearer token for keyless signing, below `--identity-token-file`/`-stdin`.
    pub OCX_IDENTITY_TOKEN: String, Public, secret, child = Scrub;
    /// Password for an encrypted signing key; never a flag, since `argv` is visible host-wide.
    pub OCX_KEY_PASSWORD: String, Public, secret, child = Scrub;
    /// The signing key PEM itself, for `--key env://OCX_SIGNING_KEY`; any other `env://` name
    /// works but is not scrubbed from plugins.
    pub OCX_SIGNING_KEY: String, Public, secret, child = Scrub;
    /// The API half of the forge credential pair (a forge access token).
    ///
    /// Not scrubbed, so a plugin-dispatched `ocx-mirror` inherits it to announce.
    pub OCX_ANNOUNCE_TOKEN: String, Public, secret;
    /// The push-only forge credential (the secret half of `git push`'s Basic pair).
    pub OCX_ANNOUNCE_GIT_TOKEN: String, Public, secret, child = Scrub;
    /// The user half of the git write transport's Basic pair, default `gitlab-ci-token`;
    /// a value containing `:`, and an empty one, fall back to the default.
    pub OCX_ANNOUNCE_GIT_USERNAME: String, Public;
    /// Authentication type for one registry; `{REGISTRY}` is the registry slug.
    pub OCX_AUTH_TYPE = "OCX_AUTH_{REGISTRY}_TYPE": Choice["anonymous", "basic", "token", "bearer"], Public;
    /// User name for one registry; `{REGISTRY}` is the registry slug.
    pub OCX_AUTH_USER = "OCX_AUTH_{REGISTRY}_USER": String, Public;
    /// Registry token for one registry; `{REGISTRY}` is the registry slug.
    pub OCX_AUTH_TOKEN = "OCX_AUTH_{REGISTRY}_TOKEN": String, Public, secret;

    // ── Plumbing ──

    /// The per-prompt reconciler's private ledger of what it last applied to the shell.
    pub __OCX_ENV_STATE: String, Plumbing;
    /// Elvish only: the recording shell's pid and `$pwd`, so the hook re-runs on a directory change.
    pub __OCX_ENV_PWD: String, Plumbing;

    // ── Test seams ──

    /// Fault stage injected into a project mutation.
    pub __OCX_TESTING_FAULT: String, Testing;
    /// File whose appearance releases a held `__OCX_TESTING_FAULT` stage.
    pub __OCX_TESTING_FAULT_RELEASE_FILE: Path, Testing;
    /// Fixed clock for the announce pipeline.
    pub __OCX_TESTING_ANNOUNCE_CLOCK: String, Testing;
    /// Milliseconds a config edit holds its lock.
    pub __OCX_TESTING_CONFIG_EDIT_HOLD_MS: Integer, Testing;
    /// Base URL replacing the forge API host.
    pub __OCX_TESTING_FORGE_BASE_URL: String, Testing;
    /// Milliseconds the forge waits to confirm a write.
    pub __OCX_TESTING_FORGE_CONFIRM_MS: Integer, Testing;
    /// Milliseconds before a credential helper times out.
    pub __OCX_TESTING_HELPER_TIMEOUT_MS: Integer, Testing;
    /// Index transport timeouts, in milliseconds.
    pub __OCX_TESTING_INDEX_TIMEOUTS_MS: String, Testing;
    /// Milliseconds of latency injected into registry calls.
    pub __OCX_TESTING_LATENCY_INJECT_MS: Integer, Testing;
    /// libc family the host detection reports.
    pub __OCX_TESTING_LIBC: String, Testing;
    /// Makes the execution-record sink's probes fail.
    pub __OCX_TESTING_RECORDS_FAIL_PROBES: String, Testing;
    /// Fault injected into the toolchain render.
    pub __OCX_TESTING_RENDER_FAULT: String, Testing;
    /// Shells the live-shell tests must find (`1` / `all`, or a comma-separated list).
    pub __OCX_TESTING_REQUIRE_LIVE_SHELLS: String, Testing;
    /// OCI reference `ocx self` treats as its own image.
    pub __OCX_TESTING_SELF_IMAGE: String, Testing;
    /// Store root whose shim publish loses its race.
    pub __OCX_TESTING_SHIM_LOST_PUBLISH_RACE: Path, Testing;
    /// Path replacing the system-scope config file.
    pub __OCX_TESTING_SYSTEM_CONFIG: Path, Testing;
    /// Milliseconds before the toolchain lock times out.
    pub __OCX_TESTING_TOOLCHAIN_LOCK_TIMEOUT_MS: Integer, Testing;

    // ── Foreign: CI and terminal ──

    /// Set by most CI providers; enables CI behaviour.
    pub CI: Bool, Foreign;
    /// Disables colour output when set to any non-empty value.
    pub NO_COLOR: String, Foreign;
    /// `0` disables colour output.
    pub CLICOLOR: String, Foreign;
    /// Non-`0` forces colour output.
    pub CLICOLOR_FORCE: String, Foreign;
    /// The terminal type.
    pub TERM: String, Foreign;
    /// Log filter of the tracing subscriber.
    pub RUST_LOG: String, Foreign, reader = Dependency("tracing-subscriber");

    // ── Foreign: GitHub Actions ──

    /// `true` inside GitHub Actions.
    pub GITHUB_ACTIONS: Bool, Foreign;
    /// File GitHub Actions reads exported variables from.
    pub GITHUB_ENV: Path, Foreign;
    /// File GitHub Actions reads `PATH` additions from.
    pub GITHUB_PATH: Path, Foreign;
    /// Commit the workflow runs on.
    pub GITHUB_SHA: String, Foreign;
    /// GitHub server URL.
    pub GITHUB_SERVER_URL: String, Foreign;
    /// `owner/repo` of the workflow.
    pub GITHUB_REPOSITORY: String, Foreign;
    /// Workflow run id.
    pub GITHUB_RUN_ID: String, Foreign;
    /// Login of the user that triggered the workflow.
    pub GITHUB_ACTOR: String, Foreign;
    /// Numeric id of the user that triggered the workflow.
    pub GITHUB_ACTOR_ID: String, Foreign;
    /// URL the runner serves OIDC tokens from.
    pub ACTIONS_ID_TOKEN_REQUEST_URL: String, Foreign;
    /// Bearer token for `ACTIONS_ID_TOKEN_REQUEST_URL`.
    // Inherited: a tool under `ocx exec` signing keylessly needs it.
    pub ACTIONS_ID_TOKEN_REQUEST_TOKEN: String, Foreign, secret;

    // ── Foreign: GitLab CI and other providers ──

    /// `true` inside GitLab CI.
    pub GITLAB_CI: Bool, Foreign;
    /// GitLab CI job token.
    pub CI_JOB_TOKEN: String, Foreign, secret;
    /// URL of the GitLab CI job.
    pub CI_JOB_URL: String, Foreign;
    /// `group/project` path of the GitLab project.
    pub CI_PROJECT_PATH: String, Foreign;
    /// URL of the GitLab project.
    pub CI_PROJECT_URL: String, Foreign;
    /// Commit the GitLab pipeline runs on.
    pub CI_COMMIT_SHA: String, Foreign;
    /// Creation time of the GitLab pipeline.
    pub CI_PIPELINE_CREATED_AT: String, Foreign;
    /// Login of the GitLab user that started the job.
    pub GITLAB_USER_LOGIN: String, Foreign;
    /// Numeric id of the GitLab user that started the job.
    pub GITLAB_USER_ID: String, Foreign;
    /// OIDC token for keyless signing, from any provider.
    pub SIGSTORE_ID_TOKEN: String, Foreign, secret;
    /// CircleCI OIDC token.
    pub CIRCLE_OIDC_TOKEN_V2: String, Foreign, secret;

    // ── Foreign: home, user and system ──

    /// The user's home directory.
    pub HOME: Path, Foreign;
    /// The user's home directory on Windows.
    pub USERPROFILE: Path, Foreign;
    /// Drive of the Windows home directory.
    pub HOMEDRIVE: String, Foreign;
    /// Path of the Windows home directory below `HOMEDRIVE`.
    pub HOMEPATH: String, Foreign;
    /// Windows system root, upper-case spelling.
    pub SYSTEMROOT: Path, Foreign;
    /// Windows system root.
    pub SYSTEM_ROOT = "SystemRoot": Path, Foreign;
    /// Windows program files directory.
    pub PROGRAM_FILES = "ProgramFiles": Path, Foreign;
    /// Windows 32-bit program files directory.
    pub PROGRAM_FILES_X86 = "ProgramFiles(x86)": Path, Foreign;
    /// Windows program data directory.
    pub PROGRAM_DATA = "ProgramData": Path, Foreign;
    /// Windows roaming application data directory.
    pub APPDATA: Path, Foreign;
    /// Directory zsh reads its startup files from.
    pub ZDOTDIR: Path, Foreign;
    /// XDG configuration directory.
    pub XDG_CONFIG_HOME: Path, Foreign;
    /// XDG data directory.
    pub XDG_DATA_HOME: Path, Foreign;
    /// The user's login shell.
    pub SHELL: Path, Foreign;
    /// Executable search path.
    pub PATH: PathList, Foreign;
    /// Windows user name.
    pub USERNAME: String, Foreign;
    /// POSIX user name.
    pub USER: String, Foreign;
    /// POSIX login name.
    pub LOGNAME: String, Foreign;
    /// Fixed timestamp for reproducible builds, in seconds since the epoch.
    pub SOURCE_DATE_EPOCH: Integer, Foreign;

    // ── Foreign: coexisting tools ──

    /// Docker credential-store directory, read by the credential helper.
    pub DOCKER_CONFIG: Path, Foreign, reader = Dependency("docker_credential");
    /// Set by direnv inside an allowed directory.
    pub DIRENV_DIR: String, Foreign;
    /// Set by mise's shell activation.
    pub MISE_SHELL: String, Foreign;
    /// `PATH` before mise's activation changed it.
    pub MISE_ORIG_PATH = "__MISE_ORIG_PATH": PathList, Foreign;

    // ── Foreign: network and TLS ──

    /// HTTP proxy URL.
    pub HTTP_PROXY: String, Foreign, reader = Dependency("reqwest");
    /// HTTP proxy URL, lower-case spelling.
    pub HTTP_PROXY_LOWER = "http_proxy": String, Foreign, reader = Dependency("reqwest");
    /// HTTPS proxy URL.
    pub HTTPS_PROXY: String, Foreign, reader = Dependency("reqwest");
    /// HTTPS proxy URL, lower-case spelling.
    pub HTTPS_PROXY_LOWER = "https_proxy": String, Foreign, reader = Dependency("reqwest");
    /// Proxy URL for every scheme.
    pub ALL_PROXY: String, Foreign, reader = Dependency("reqwest");
    /// Proxy URL for every scheme, lower-case spelling.
    pub ALL_PROXY_LOWER = "all_proxy": String, Foreign, reader = Dependency("reqwest");
    /// Hosts reached without a proxy.
    pub NO_PROXY: String, Foreign, reader = Dependency("reqwest");
    /// Hosts reached without a proxy, lower-case spelling.
    pub NO_PROXY_LOWER = "no_proxy": String, Foreign, reader = Dependency("reqwest");
    /// CA bundle git uses.
    pub GIT_SSL_CAINFO: Path, Foreign;
    /// CA directory git uses.
    pub GIT_SSL_CAPATH: Path, Foreign;
    /// CA bundle OpenSSL-style clients use.
    pub SSL_CERT_FILE: Path, Foreign;
    /// CA directory OpenSSL-style clients use.
    pub SSL_CERT_DIR: Path, Foreign;
    /// Temporary directory.
    pub TMPDIR: Path, Foreign;
    /// Temporary directory on Windows.
    pub TEMP: Path, Foreign;
    /// Temporary directory on Windows, alternate spelling.
    pub TMP: Path, Foreign;
}

#[cfg(test)]
mod tests;
