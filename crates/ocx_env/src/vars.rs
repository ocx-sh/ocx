// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Every environment variable OCX reads, declared once.

crate::env_vars! {
    // ── Core configuration ──

    /// Absolute path to the running `ocx`, set on every spawn so a child ocx runs the
    /// same binary, not whatever `$PATH` resolves.
    ///
    /// Set by ocx on every subprocess it spawns, so the inner `ocx launcher exec` of a generated
    /// launcher runs the binary that installed the package. Unset, launchers fall back to
    /// `$PATH`-resolved `ocx`; set it by hand only to pin a specific binary.
    ///
    /// The Windows `.exe` shim treats a defined but empty value as defined, the Unix launcher's
    /// `${OCX_BINARY_PIN:-ocx}` as unset.
    pub OCX_BINARY_PIN: Path, Public, child = Forward;
    /// The OCX data root — `$OCX_HOME`, else `~/.ocx`.
    ///
    /// Forwarded set-always as the resolved root, or an `ocx exec --clean` child (no `HOME`)
    /// resolves another root and misses the package the parent just materialized.
    ///
    /// The project walk skips an `ocx.toml` sitting exactly here. That file is the global
    /// toolchain manifest, reached only through `--global` or `OCX_GLOBAL`, never by upward discovery.
    pub OCX_HOME: Path, Public, child = Forward;
    /// Boolean — disables network access when truthy. Mirrors `--offline`.
    ///
    /// Tag-to-digest resolution must come from the local index or a digest pin; an unpinned tag
    /// missing locally errors immediately. `--offline` takes precedence.
    ///
    /// With `OCX_REMOTE` it is pinned-only mode. For `ocx package verify` it scopes to the Sigstore
    /// trust services (Rekor key, TUF), not the registry. It needs a pinned Rekor key from a
    /// trust-root rung or the cache. Without one it exits 81 and never skips verification.
    pub OCX_OFFLINE: Bool, Public, on_invalid = Error, child = Forward;
    /// Boolean — a tag-only reference missing from the local index errors instead of
    /// being fetched; digest-pinned content still fetches. Mirrors `--frozen`.
    ///
    /// A tag in the local index resolves from cache; a digest or `ocx.lock` pin still fetches. An
    /// unpinned tag missing from it exits 81, as does a frozen `ocx index update`.
    ///
    /// Unlike `OCX_OFFLINE` it is not a network ban; patch companions and the managed-config tier
    /// are unaffected. The flag takes precedence; combining with `OCX_REMOTE` is a usage error
    /// (exit 64), with `OCX_OFFLINE` offline wins.
    pub OCX_FROZEN: Bool, Public, on_invalid = Error, child = Forward;
    /// Boolean — uses the remote index by default when truthy. Mirrors `--remote`.
    ///
    /// Routes tag list, catalog and tag resolution to the registry. Pure-query commands (`ocx index
    /// list`, `ocx index catalog`, `ocx package description pull`) persist nothing; run `ocx index
    /// update` to refresh the snapshot.
    ///
    /// Digest-addressed reads still consult the local index first. `--remote` takes precedence.
    pub OCX_REMOTE: Bool, Public, child = Forward;
    /// Path to an explicit configuration file. Mirrors `--config`.
    ///
    /// Layers on top of the discovered chain (system, user, `$OCX_HOME/config.toml`) at the highest
    /// file-tier precedence. With `--config` both load and the flag wins on conflicting scalars.
    ///
    /// A missing file is an error naming the path; empty is unset. Combine with `OCX_NO_CONFIG` for
    /// a hermetic load.
    pub OCX_CONFIG: Path, Public, child = Forward;
    /// Boolean — skip the discovered config-tier chain; explicit `--config` / `OCX_CONFIG`
    /// paths still load and a system-scope lock survives. Forwarded to child ocx.
    ///
    /// Also drops the `[managed]` snapshot and the `OCX_MANAGED_CONFIG` read. A system-scope policy
    /// in `/etc/ocx/config.toml` still loads, filtered to its locked sections, and an unreadable
    /// system file still aborts (exit 78).
    ///
    /// Env-only: there is no `--no-config` flag. Hermetic pattern: `OCX_NO_CONFIG=1 ocx --config
    /// /ci/ocx.toml ...`.
    pub OCX_NO_CONFIG: Bool, Public, child = Forward;
    /// Path to an explicit project `ocx.toml`. Mirrors `--project`.
    ///
    /// A file loads directly and a directory resolves to `<dir>/ocx.toml`. Precedence: `--project`,
    /// this variable, the CWD walk; `OCX_NO_PROJECT` prunes the last two.
    ///
    /// A path that does not exist exits 79, a directory with no `ocx.toml` exits 64, an unreadable
    /// one exits 74. Empty is unset.
    ///
    /// Explicit paths follow symlinks; the CWD walk rejects a symlinked `ocx.toml`. A child ocx, a
    /// launcher re-entry included, does not inherit it and discovers its own project.
    pub OCX_PROJECT: Path, Public, child = Forward;
    /// Boolean — skip the CWD walk and `OCX_PROJECT`; explicit `--project` paths still load.
    ///
    /// Skips the CWD walk and ignores `OCX_PROJECT`; explicit `--project` still loads. Env-only: no
    /// `--no-project` flag.
    ///
    /// Forwarded so children also skip discovery. Hermetic pattern: `OCX_NO_PROJECT=1 ocx
    /// --project /ci/ocx.toml exec -- ...`.
    pub OCX_NO_PROJECT: Bool, Public, child = Forward;
    /// Path at which the CWD walk for a project `ocx.toml` stops; relative values join onto the
    /// working directory. An explicit `--project` or `OCX_PROJECT` is not bounded by it.
    ///
    /// The walk probes the ceiling directory and never ascends above it, so a project file there is
    /// found and one in a parent is not. It already stops at the first `.git` entry; this bounds it
    /// where no repository marks the edge, such as a CI workspace under a shared parent.
    pub OCX_CEILING_PATH: Path, Public;
    /// Boolean — commands that stamp shell-activation consent as a side effect write none;
    /// `--consent` / `--no-consent` outrank it and `ocx shell allow` ignores it.
    ///
    /// Forwarded, else a script run by `ocx exec` that calls `ocx pull` stamps after all.
    ///
    /// Covers `add`, `remove`, `lock`, `update`, `upgrade`, `pull`, `exec` and `init`. `ocx exec
    /// --no-consent` sets it on the child too; `--consent` does not clear it.
    ///
    /// Set it in a pipeline or image, not an interactive profile: exported globally, growing a lock
    /// past the stamped sources leaves the project inert.
    pub OCX_NO_CONSENT: Bool, Public, on_invalid = Error, child = Forward;
    /// Boolean — select the global toolchain (`$OCX_HOME/ocx.toml`). Mirrors `--global`.
    ///
    /// Toolchain-tier commands (`add`, `remove`, `lock`, `update`, `pull`, `exec`, `env`) target
    /// `$OCX_HOME/ocx.toml` instead of a discovered project, with no implicit home-tier fallback
    /// otherwise. Setting it together with `OCX_PROJECT` is a usage error (exit 64).
    ///
    /// The global toolchain never composes into project-tier resolution, and `ocx exec` never sees
    /// global packages. A child ocx, a launcher re-entry included, does not inherit it and can take
    /// `--project`.
    pub OCX_GLOBAL: Bool, Public, child = Forward;
    /// Path to the local index directory. Mirrors `--index`.
    ///
    /// Swaps the whole local index collection (default `$OCX_HOME/index/`) for a shipped one, never
    /// a partial overlay. It is meant for a bundled copy in a GitHub Action, Bazel rule or
    /// devcontainer. `--index` takes precedence.
    ///
    /// Under `--remote` or `OCX_REMOTE`, tag- and catalog-addressed lookups bypass it, while
    /// digest-addressed lookups still consult it first.
    pub OCX_INDEX: Path, Public, child = Forward;
    /// The registry a bare identifier resolves under; outranks `[registry] default`, else
    /// `ocx.sh`. Empty is unset.
    ///
    /// Forwarded so every frame of a launch chain agrees.
    ///
    /// Environment-only: no CLI flag, so the forwarded value is the one this invocation read. An
    /// empty value clears an inherited setting rather than travelling onward.
    pub OCX_DEFAULT_REGISTRY: String, Public, child = Forward;
    /// Comma-separated `host[:port]` authorities that may be dialled over plain HTTP.
    ///
    /// Forwarded, or a child with a narrower set cannot reach a registry the parent just pulled from.
    ///
    /// Union with `[registries.<name>] insecure`; only a system-scope `insecure = false` subtracts.
    /// Names match exactly as `host[:port]`.
    ///
    /// Also gates plain `http://` `[mirrors]` targets. Credentials travel in the clear; an HTTPS
    /// registry naming an `http` token realm, or redirected to `http://`, is still refused.
    pub OCX_INSECURE_REGISTRIES: HostList, Public, child = Forward;
    /// Boolean — allow a tag resolving to a yanked index entry. Forwarded to child ocx.
    ///
    /// Without it a tag resolve against a yanked entry of the public index exits 65. This holds
    /// online and against a local copy with no network. `ocx index update` does not bypass it. A
    /// digest-pinned reference never needs it, and a project resolved from `ocx.lock` is
    /// unaffected by a yank.
    pub OCX_ALLOW_YANKED: Bool, Public, child = Forward;

    // ── Resolution and launch state ──

    /// Path to the patch snapshot file `ocx patch freeze` writes, whose pinned digests win over live tag lookups.
    /// Forwarded to child ocx.
    ///
    /// While set, every command but `ocx patch sync` (refused, exit 78) and `ocx patch freeze` uses
    /// only the snapshot's pinned digests and records no pin. A companion the snapshot omits is
    /// absent, and a required one fails with exit 79, as does a missing pinned blob under
    /// `--offline`.
    ///
    /// A format version this ocx cannot read exits 65. Independent of `--frozen`; `ocx clean` roots
    /// the snapshot's companions only while this is set.
    pub OCX_PATCH_SNAPSHOT: Path, Public, child = Forward;
    /// JSON object mapping an upstream host to a mirror string or `{"registry"?, "index"?}`,
    /// the same union `[mirrors."<host>"]` accepts. Forwarded to child ocx.
    ///
    /// A string value redirects both the `registry` and `index` roles; the object form splits them.
    /// A role value is `scheme://host[/repo-key-prefix]`.
    ///
    /// Replaces the `[mirrors]` entry per host and per role; unlike a config-file `registry` entry
    /// it cannot drop a namespace off the compiled-in index. A malformed value (bad JSON, wrong
    /// shape, `http://` for a host not plaintext-eligible) aborts at startup.
    pub OCX_MIRRORS: Json, Public, child = Forward;
    /// JSON object encoding the resolved `[patches]` config; forwarded only when configured.
    pub OCX_PATCHES: Json, Public, child = Forward;
    /// JSON envelope of the resolved project `[env]` entries plus `ocx exec --env` overrides,
    /// or a launcher re-entry silently reverts them.
    ///
    /// Decode fails closed on the whole envelope (reserved key, unknown `kind`): a forged entry
    /// can set a value, so the leniency of `OCX_PATCHES` must not be copied.
    ///
    /// Keys starting `OCX_` or `__OCX_` are refused in `ocx.toml` `[env]` (exit 78) and in `--env` (exit 64).
    pub OCX_ENV: Json, Public, child = Forward;
    /// Boolean — `ocx self setup` modifies no shell profile. Mirrors `--no-modify-path`.
    ///
    /// Suppresses both PATH surfaces on every platform, the managed profile blocks and the
    /// session-level registration; each location still shows in the summary as `skipped_opt_out`.
    /// Second rung of the ladder: `--no-modify-path`, this variable, `[shell] modify_path`, then
    /// allowed.
    ///
    /// The opt-out persists as `modify_path = false`; there is no positive flag. Only truthy values
    /// enable it.
    pub OCX_NO_MODIFY_PATH: Bool, Public;
    /// OCI reference overriding `[managed].source` for this invocation only; empty is unset,
    /// `OCX_NO_CONFIG` suppresses it. Forwarded to child ocx.
    ///
    /// Never written back to the seed; only `ocx config setup` and `ocx self setup
    /// --managed-config` write it. Resolving the tier never fetches: pair the variable with an
    /// explicit `ocx config update` in CI.
    ///
    /// Forwarded to children. Suppressed entirely by `OCX_NO_CONFIG`.
    pub OCX_MANAGED_CONFIG: String, Public, child = Forward;
    /// Boolean — kill switch for the managed-config background refresh; `ocx config update` still works.
    ///
    /// Forwarded, or a child's refresh replaces the managed tier with config the parent never saw.
    ///
    /// Silences both the `apply` and `notify` postures of the background tick. `ocx config update`
    /// still runs; `ocx self setup` and `ocx config setup` still re-sync an adopted seed regardless
    /// (use `--offline` to skip that).
    ///
    /// The tick is also off when `CI` is truthy, `OCX_OFFLINE` is set or stderr is not a terminal.
    /// Distinct from `OCX_NO_UPDATE_CHECK`, which covers the ocx self-update notice.
    pub OCX_NO_CONFIG_REFRESH: Bool, Public, child = Forward;
    /// Directory execution records are written to; absent or empty turns recording off.
    /// Forwarded so every frame of a launch chain records into the outermost sink.
    ///
    /// Equivalent to `--records-dir` on `ocx exec` and `ocx package exec` and to the `[records]
    /// dir` key. Unset at every tier means no record and no added I/O.
    ///
    /// Forwarded, so a launcher re-entry (`ocx launcher exec`, which takes no flags) writes to the
    /// same sink. The flag overrides it; a system-scope `[records]` declaration locks the section
    /// and this variable then has no effect.
    pub OCX_RECORDS_DIR: Path, Public, child = Forward;
    /// Filename template for each execution record; forwarded alongside `OCX_RECORDS_DIR`.
    ///
    /// No `OCX_RECORDS_REQUIRED` exists: fail-closed recording is config-file-only operator policy.
    ///
    /// Equivalent to `--records-name` and the `[records] name` key; the flag overrides it. The
    /// placeholders are `{time}`, `{pid}`, `{rand}` and `{host}`; an unrecognized one is a
    /// configuration error (exit 78), never a literal.
    ///
    /// Needs a sink from `OCX_RECORDS_DIR`, `--records-dir` or `[records] dir`. Locked with the
    /// rest of `[records]` under a system-scope policy.
    pub OCX_RECORDS_NAME: String, Public, child = Forward;

    // ── Toolchain ──

    /// Lazy materialisation mode (`never` / `always`), the weakest tier of the `lazy-mode` ladder.
    ///
    /// Not forwarded: it changes when content materializes, never which digest resolves.
    ///
    /// `never` (default) composes eagerly, `always` composes a shim that defers the download to
    /// first invocation. Case-insensitive, unlike the `--lazy-mode` flag and the `ocx.toml` key.
    ///
    /// An unrecognized value is ignored with a warning. Overridden by the `--lazy-mode` flag,
    /// `[package."<id>"]`, `[group.<name>]` and the toolchain-level `lazy-mode` key.
    pub OCX_LAZY_MODE: Choice["never", "always"], Public;
    /// Lazy materialisation report (`silent` / `progress`); not forwarded, like `OCX_LAZY_MODE`.
    ///
    /// `silent` (default) or `progress`, case-insensitive. `progress` opens a channel on the
    /// controlling terminal only when the first-invocation download actually happens. It falls
    /// back to `silent` where no terminal is reachable (Docker build, CI, `setsid`).
    ///
    /// Read inside `ocx launcher shim`, never by the composing command. Overridden by
    /// `--lazy-report`, `[package."<id>"]` and the toolchain-level key; there is no group tier.
    pub OCX_LAZY_REPORT: Choice["silent", "progress"], Public;
    /// Toolchain activation mode (`env` / `bin` / `none`), the weakest tier of the `activate`
    /// ladder. Not forwarded, like `OCX_LAZY_MODE`.
    ///
    /// `env` composes the toolchain environment every prompt (default), `bin` puts the home's
    /// `active/bin` on `PATH` only, `none` withdraws what ocx owns. Case-insensitive, not trimmed.
    ///
    /// An unrecognized value warns and falls through to the next tier; the `ocx.toml` key is a
    /// parse error (exit 78). Sits below the `ocx.toml` `activate` key.
    pub OCX_TOOLCHAIN_ACTIVATE: Choice["env", "bin", "none"], Public;
    /// Boolean — composed paths pin to digest roots; the weakest tier of the `pinned` ladder.
    ///
    /// Unset and an explicit `false` are different answers. Below `--pinned` / `--no-pinned` and
    /// the `ocx.toml` `pinned` key.
    ///
    /// True names the digest roots `ocx.lock` pins, so a later `ocx update` does not reach that
    /// environment; false follows the rendered `links/<group>/<entry>` links, the default.
    ///
    /// A root entry whose link is absent or stale composes on its digest path; dependency paths
    /// always do. An unparseable value warns and falls through.
    pub OCX_TOOLCHAIN_PINNED: TriBool, Public;
    /// Root under which project toolchain homes render — the env tier of `toolchain_dir`.
    ///
    /// Weakest tier: `config.toml` wins. A refused root exits 78.
    pub OCX_TOOLCHAIN_DIR: Path, Public, child = Forward;
    /// Boolean — skip the policy-gated auto-verify on install/pull; `--no-verify` wins. Forwarded to child ocx.
    ///
    /// Skips the policy-gated auto-verify on `install`, `pull` and every command that auto-installs
    /// on demand (`exec`, `env`, `package exec`, `package env`, patch discovery). Only `install`
    /// and `pull` carry the `--verify` / `--no-verify` flag, which outranks this variable. A
    /// skipped policy-covered package prints one `warning:` line per invocation.
    ///
    /// With no `[[trust.policy]]` covering a package, verification does not run regardless.
    pub OCX_NO_VERIFY: Bool, Public, on_invalid = Error, child = Forward;

    // ── Shell activation ──

    /// Project directories (separated like `PATH`) the per-prompt shell hook may activate in
    /// without a consent stamp, subtrees included.
    ///
    /// An entry is an exact canonicalized directory, or with a trailing `/*` that directory and
    /// everything beneath it.
    pub OCX_CONSENT_PATHS: PathList, Public;
    /// Comma-separated OCI namespaces; a project whose whole lock names sources inside them
    /// activates without a directory grant or a consent stamp.
    ///
    /// Each token is `<registry>/<first-path-segment>`, with or without a trailing `/*`; there is
    /// no whole-registry token, and one malformed token discards the whole variable with a warning.
    /// Additive with `[shell.consent] namespaces`.
    ///
    /// The match is against the store's record of the coordinate each digest was pulled under on
    /// this machine. It is not against the lock text, and it never covers a project's own `[env]`
    /// table.
    pub OCX_CONSENT_NAMESPACES: String, Public;
    /// Boolean — `ocx self activate` skips the shell-completion block.
    ///
    /// `PATH` prepend and the global env eval still run. Use it when a framework such as oh-my-zsh
    /// or a Nix completion store manages completions.
    ///
    /// `ocx shell completion` without `--if-enabled` generates the script regardless; with it, this
    /// variable is the first rung of the policy that decides whether anything prints.
    pub OCX_NO_COMPLETION: Bool, Public;
    /// Boolean — disables the per-prompt shell reconciler; `ocx self activate` still runs at shell start.
    ///
    /// `ocx self activate` still prepends `PATH`, loads completions and evals the global env, but
    /// registers no per-prompt hook. Read once at shell start, so exporting it mid-session takes
    /// effect at the next shell.
    ///
    /// The only variable that makes a shell wholly inert: `OCX_NO_CONFIG` leaves explicit
    /// `--config` files and the consent grants active. Boolean, not tri-state; `--hook` is the
    /// positive channel.
    pub OCX_NO_HOOK: Bool, Public;

    // ── Output, logging and runtime ──

    /// Log level, as `--log-level` accepts it; the flag wins. Defaults to `info`.
    pub OCX_LOG_LEVEL: String, Public;
    /// Log level for console messages only; outranks `OCX_LOG_LEVEL` there.
    pub OCX_LOG_CONSOLE: String, Public;
    /// Boolean — suppresses the stdout report and the transfer progress bars.
    pub OCX_QUIET: Bool, Public;
    /// Maximum number of root packages pulled in parallel.
    ///
    /// Applies to every command that fans out through the package manager (`install`, `pull`,
    /// `package pull`, `exec`, `env`). `0` uses all logical cores, unlike Cargo's `--jobs 0` which
    /// errors.
    ///
    /// Negative or non-numeric values are ignored with a warning; unset is unbounded. `--jobs`
    /// takes precedence.
    pub OCX_JOBS: Integer, Public;
    /// Boolean — kill switch for the background self-update and toolchain drift checks; forwarded to child ocx.
    ///
    /// The checks are also off when `CI` is truthy, `OCX_OFFLINE` is set, stderr is not a terminal,
    /// or the command is `version`, `about` or `shell completion`. The drift notice additionally
    /// skips `update`, `lock`, `add`, `remove` and `init`. This variable also suppresses the
    /// throttle state write.
    ///
    /// Distinct from `OCX_NO_CONFIG_REFRESH`, which silences only the managed-config tick.
    pub OCX_NO_UPDATE_CHECK: Bool, Public, child = Forward;
    /// Throttle interval of the background update checks, `\d+[smhd]?` (bare digits = seconds).
    ///
    /// Beats `[update] interval`. Not forwarded: a personal preference, like `OCX_LAZY_MODE`.
    ///
    /// Unset means `1d`; `0` checks on every eligible invocation; units are `s`, `m`, `h`, `d`.
    /// Surrounding whitespace is trimmed. An invalid value is logged at debug level and falls through to `[update] interval`.
    ///
    /// The check waits for a registry round trip and never fails the command. It is also suppressed
    /// when stderr is not a terminal or `CI` is set, whatever the interval; `ocx self update`
    /// always bypasses it.
    pub OCX_UPDATE_CHECK_INTERVAL: String, Public;
    /// `RefreshPolicy` wire value for ocx's own update check; beats `[update] self`. Not forwarded.
    ///
    /// `notify` (default) prints a notice naming `ocx self update`, `apply` installs the newer ocx
    /// after the command finishes without changing its exit code, `manual` never checks. Lowercase
    /// only; an unrecognized value is ignored at debug level and falls through.
    ///
    /// Beats the config file. `apply` needs a terminal on stderr, `CI` unset, online, and
    /// `OCX_NO_UPDATE_CHECK` unset, and does nothing for an ocx that `ocx self setup` did not
    /// install.
    pub OCX_SELF_UPDATE: Choice["apply", "notify", "manual"], Public;
    /// `RefreshPolicy` wire value for the toolchain drift check; beats `[update] toolchain`. Not forwarded.
    ///
    /// `notify` (default) prints one stderr line per toolchain file with drift, naming `ocx
    /// update`; `apply` is read as `notify`, since only `ocx update` moves a pin; `manual` never
    /// checks. Lowercase only; other values are ignored at debug level.
    ///
    /// The check compares the lock's platform digests with the registry without writing the local
    /// index. It probes a project only when it is consented. It stays silent on a missing or
    /// stale lock, a registry error or a 5-second timeout.
    pub OCX_TOOLCHAIN_UPDATE: Choice["apply", "notify", "manual"], Public;
    /// Boolean — skip ad-hoc code signing of macOS binaries after installation.
    pub OCX_NO_CODESIGN: Bool, Public;

    // ── Credentials and certificates ──

    /// Extra CA roots — a path or inline PEM (contains `-----BEGIN`); empty is unset.
    ///
    /// Adds trust for registry, index, forge and Sigstore clients on top of the host store and
    /// compiled-in roots. Wins over every `config.toml` tier unless the `extra_ca_certs` pair is
    /// system-locked, when it is ignored with a warning.
    ///
    /// A failure still reports `UnknownIssuer` (exit 69). On Windows an inline PEM can hit
    /// cmd.exe's 8,191-character limit; use a path.
    pub OCX_EXTRA_CA_CERTS: PathOrPem, Public;
    /// Path (bare or `file://`) to a Sigstore trusted-root JSON document, or a directory holding
    /// `trusted_root.json`, that `ocx package verify` loads its trust material from.
    ///
    /// The `--sigstore-trusted-root` flag takes precedence. The root must carry the Fulcio CAs, the CT log keys and the
    /// pinned Rekor key together; CA anchors alone are refused with exit 78.
    ///
    /// Bypasses the TUF fetch, so keeping it current is the operator's job.
    ///
    /// With `OCX_OFFLINE` it is the fully offline path. Ladder after the flag: this variable,
    /// `[trust.sigstore]`, `$OCX_HOME/sigstore/trusted-root.json`, the cache, then TUF.
    pub OCX_SIGSTORE_TRUSTED_ROOT: Path, Public;
    /// OIDC bearer token for keyless signing, below `--identity-token-file`/`-stdin`.
    ///
    /// Used by `ocx package sign` and `ocx package attest` only when neither
    /// `--identity-token-file` nor `--identity-token-stdin` is given; ambient CI detection and
    /// browser OAuth rank below it. Never forwarded or logged, and stripped from plugin-dispatched
    /// children and from any environment OCX writes.
    ///
    /// A short-lived JWT (typically under 10 minutes): fetch a fresh one just before signing.
    pub OCX_IDENTITY_TOKEN: String, Public, secret, child = Scrub;
    /// Password for an encrypted signing key; never a flag, since `argv` is visible host-wide.
    ///
    /// Pair it with `--key <path>` or `OCX_SIGNING_KEY`; it opens a key and never names one. An
    /// unset variable reads as the empty password. That is the shape of a pair made by pressing
    /// enter twice in `cosign generate-key-pair`, so that case is supported rather than an error.
    ///
    /// Stripped from plugin-dispatched children and from any environment OCX writes. OCX does not
    /// generate key pairs.
    pub OCX_KEY_PASSWORD: String, Public, secret, child = Scrub;
    /// The signing key PEM itself, for `--key env://OCX_SIGNING_KEY`; any other `env://` name
    /// works but is not scrubbed from plugins.
    ///
    /// Holds the PEM, never a path: a cosign `ENCRYPTED SIGSTORE PRIVATE KEY` envelope when
    /// signing, an SPKI public key when a `[[trust.policy]]` signer names it. For runners with no
    /// writable disk.
    ///
    /// Unset or empty exits 74 naming the variable; over 64 KiB exits 65. Stripped from plugin-dispatched
    /// children and from any environment OCX writes; a key held in any other `env://` name is inherited by plugins and launchers.
    pub OCX_SIGNING_KEY: String, Public, secret, child = Scrub;
    /// The API half of the forge credential pair (a forge access token).
    ///
    /// Read by `ocx package announce` and `ocx package claim` to open or update a pull or merge
    /// request; `--output` needs none. GitHub needs `repo` scope, GitLab `api`.
    ///
    /// Under `--transport git` it authenticates only the REST calls and `OCX_ANNOUNCE_GIT_TOKEN`
    /// carries the push. In a GitLab job an unset token falls through to `CI_JOB_TOKEN`, which cannot
    /// write through the API.
    pub OCX_ANNOUNCE_TOKEN: String, Public, secret;
    /// The push-only forge credential (the secret half of `git push`'s Basic pair).
    ///
    /// Used only under `--transport git`, for every `git` call to the index remote; REST reads keep
    /// using `OCX_ANNOUNCE_TOKEN`. Unset or empty reuses the resolved API credential.
    ///
    /// Sent as the Basic password via git's config environment, never in a URL, `argv` or
    /// `.git/config`; the user half is `OCX_ANNOUNCE_GIT_USERNAME`. Stripped from plugin-dispatched
    /// children and from any environment OCX writes, unlike the API half.
    pub OCX_ANNOUNCE_GIT_TOKEN: String, Public, secret, child = Scrub;
    /// The user half of the git write transport's Basic pair, default `gitlab-ci-token`;
    /// a value containing `:`, and an empty one, fall back to the default.
    ///
    /// Not a credential, so plugins inherit it from the ambient environment.
    pub OCX_ANNOUNCE_GIT_USERNAME: String, Public;
    /// Authentication type for one registry.
    ///
    /// `<REGISTRY>` is the registry name with every non-alphanumeric character replaced by `_`, case
    /// kept, so `docker.io` reads `OCX_AUTH_docker_io_TYPE`.
    ///
    /// Any `OCX_AUTH_*` value set for a registry means ocx never invokes a Docker credential helper
    /// for it; `anonymous` opts a registry out of helper lookup entirely.
    pub OCX_AUTH_TYPE = "OCX_AUTH_{REGISTRY}_TYPE": Choice["anonymous", "basic", "token", "bearer"], Public;
    /// User name for one registry; `<REGISTRY>` is the registry slug.
    ///
    /// Read for the `basic` type only; ignored for any other type.
    pub OCX_AUTH_USER = "OCX_AUTH_{REGISTRY}_USER": String, Public;
    /// Registry token for one registry; `<REGISTRY>` is the registry slug.
    ///
    /// The bearer token, or the password for `basic`; ignored unless the type is `bearer` or `basic`.
    pub OCX_AUTH_TOKEN = "OCX_AUTH_{REGISTRY}_TOKEN": String, Public, secret;

    // ── Plumbing ──

    /// The per-prompt reconciler's private ledger of what it last applied to the shell.
    ///
    /// The ledger that tells a value ocx wrote from one you typed. Crossing into the interactive
    /// shell makes it a user-facing contract despite the private name. `unset __OCX_ENV_STATE`
    /// (`unset-env` in elvish) repairs a stale or corrupted shell, at the cost of the reconciler's
    /// memory of what it may restore.
    ///
    /// Reserved: a package cannot declare it and `--env` cannot set it.
    pub __OCX_ENV_STATE: String, Plumbing;
    /// Elvish only: the recording shell's pid and `$pwd`, so the hook re-runs on a directory change.
    ///
    /// Holds the pid, `$pwd`, the direnv/mise yield sentinels and the two consent-grant variables,
    /// space-joined; never set it by hand.
    pub __OCX_ENV_PWD: String, Plumbing;
    /// JSON map from a content digest to the `registry/repository[:tag]` names one composition
    /// resolved it under; read by `ocx launcher exec` to match targeted patch rules.
    ///
    /// Written by ocx only while a `[patches]` tier is in effect; reserved, so `[env]` and
    /// `--env` cannot set it. Never forwarded explicitly, or a launcher nested under another
    /// launcher loses the outer composition's names.
    ///
    /// The per-prompt `ocx:` summary line never lists it, nor any other `__OCX_*` key.
    pub __OCX_LAUNCH_IDENTITIES: Json, Plumbing;
    /// Set by ocx on every credential helper it starts; an ocx running under it never downloads.
    ///
    /// A helper installed through ocx is a trampoline that starts `ocx exec`; without the marker that
    /// ocx would install the package the outer ocx is still pulling and wait on its lock. Under the
    /// marker ocx behaves as if `--offline` were given.
    pub __OCX_CREDENTIAL_HELPER: Bool, Plumbing;

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

    /// Set by most CI providers. When truthy, the background update check and the managed-config
    /// refresh are skipped; see `OCX_NO_UPDATE_CHECK` and `OCX_NO_CONFIG_REFRESH`.
    pub CI: Bool, Foreign;
    /// Disables colour output when set to any non-empty value.
    pub NO_COLOR: String, Foreign;
    /// `0` disables colour output.
    pub CLICOLOR: String, Foreign;
    /// Non-`0` forces colour output.
    pub CLICOLOR_FORCE: String, Foreign;
    /// The terminal type.
    ///
    /// `TERM=dumb` turns colour off. `NO_COLOR` and `CLICOLOR_FORCE` rank above it.
    pub TERM: String, Foreign;
    /// Log filter of the tracing subscriber.
    ///
    /// Fallback filter, read only when neither `OCX_LOG_CONSOLE` nor `OCX_LOG_LEVEL` is set.
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
    /// Passed only to the git process `ocx package announce` runs; never used to derive paths.
    pub HOMEDRIVE: String, Foreign;
    /// Path of the Windows home directory below `HOMEDRIVE`.
    /// Passed only to the git process `ocx package announce` runs; never used to derive paths.
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
    ///
    /// `ocx self setup` writes the zsh activation blocks into `$ZDOTDIR` instead of `$HOME`.
    pub ZDOTDIR: Path, Foreign;
    /// XDG configuration directory.
    pub XDG_CONFIG_HOME: Path, Foreign;
    /// XDG data directory.
    ///
    /// `ocx self setup` writes the nushell autoload file under `$XDG_DATA_HOME/nushell/vendor/autoload/`,
    /// else under `~/.local/share/nushell/vendor/autoload/`.
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
