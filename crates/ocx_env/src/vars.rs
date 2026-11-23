// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Every environment variable OCX reads, declared once.

crate::env_vars! {
    // ── Core configuration ──

    /// Absolute path to the running `ocx`, set on every spawn so a child OCX runs the
    /// same binary, not whatever `$PATH` resolves.
    ///
    /// Set by OCX on every subprocess it spawns, so the inner `ocx launcher exec` of a generated
    /// launcher runs the binary that installed the package. Unset, launchers fall back to
    /// `$PATH`-resolved `ocx`; set it by hand only to pin a specific binary.
    ///
    /// The Windows `.exe` shim treats a defined but empty value as defined, the Unix launcher's
    /// `${OCX_BINARY_PIN:-ocx}` as unset. The value is the installed `ocx` path when the running
    /// binary is that one and the path is safely owned, else the running binary; never empty.
    pub OCX_BINARY_PIN: Path, Public, propagation = BinaryPin;
    /// The OCX data root — `$OCX_HOME`, else `~/.ocx`.
    ///
    /// Forwarded set-always as the resolved root, or an `ocx exec --clean` child (no `HOME`)
    /// resolves another root and misses the package the parent just materialized. The files
    /// `ocx self setup` writes may export it.
    ///
    /// The project walk skips an `ocx.toml` sitting exactly here. That file is the global
    /// toolchain manifest, reached only through `--global`, never by upward discovery.
    pub OCX_HOME: Path, Public, propagation = SetupWritten;
    /// Boolean — disables network access when truthy. Mirrors `--offline`.
    ///
    /// Tag-to-digest resolution must come from the local index or a digest pin; an unpinned tag
    /// missing locally errors immediately. `--offline` takes precedence.
    ///
    /// With `OCX_REMOTE` it is pinned-only mode. For `ocx package verify` it scopes to the Sigstore
    /// trust services (Rekor key, TUF), not the registry. It needs a pinned Rekor key from a
    /// trust-root rung or the cache. Without one it exits 81 and never skips verification.
    pub OCX_OFFLINE: Bool, Public, on_invalid = Error, propagation = UserInput;
    /// Boolean — a tag-only reference missing from the local index errors instead of
    /// being fetched; digest-pinned content still fetches. Mirrors `--frozen`.
    ///
    /// A tag in the local index resolves from cache; a digest or `ocx.lock` pin still fetches. An
    /// unpinned tag missing from it exits 81, as does a frozen `ocx index update`.
    ///
    /// Unlike `OCX_OFFLINE` it is not a network ban; patch companions and the managed-config tier
    /// are unaffected. The flag takes precedence; combining with `OCX_REMOTE` is a usage error
    /// (exit 64), with `OCX_OFFLINE` offline wins.
    pub OCX_FROZEN: Bool, Public, on_invalid = Error, propagation = UserInput;
    /// Boolean — uses the remote index by default when truthy. Mirrors `--remote`.
    ///
    /// Routes tag list, catalog and tag resolution to the registry. Pure-query commands (
    /// `ocx index list`, `ocx index catalog`, `ocx package description pull`) persist nothing; run
    /// `ocx index update` to refresh the snapshot.
    ///
    /// Digest-addressed reads still consult the local index first. `--remote` takes precedence.
    pub OCX_REMOTE: Bool, Public, propagation = UserInput;
    /// Paths to explicit configuration files, separated like `PATH` (`:` on Unix, `;` on Windows).
    /// Mirrors `--config`, which takes one path.
    ///
    /// Each file layers on the discovered chain and the managed tier in list order, a later file
    /// winning on conflicting scalars; `--config` loads after the whole list. At most 32 files.
    ///
    /// A missing file is an error naming it; an empty value is unset and empty entries are skipped.
    /// A path containing the separator cannot be listed. With `OCX_NO_CONFIG` the listed files
    /// still load.
    pub OCX_CONFIG: PathList, Public, propagation = UserInput;
    /// Boolean — skip the discovered config-tier chain; every file `--config` and `OCX_CONFIG` name
    /// still loads and a system-tier lock survives. Forwarded to child OCX.
    ///
    /// Also drops the `[managed]` snapshot and the `OCX_MANAGED_CONFIG` read. A system-tier policy
    /// in `/etc/ocx/config.toml` still loads, filtered to its locked sections (`[records]` and
    /// `[telemetry]` among them), and an unreadable system file still aborts (exit 78).
    ///
    /// Env-only: there is no `--no-config` flag. Hermetic pattern:
    /// `OCX_NO_CONFIG=1 ocx --config /ci/ocx.toml ...`.
    pub OCX_NO_CONFIG: Bool, Public, propagation = UserInput;
    /// Path to an explicit project `ocx.toml`. Mirrors `--project`.
    ///
    /// A file loads directly and a directory resolves to `<dir>/ocx.toml`. Precedence: `--project`,
    /// this variable, the CWD walk; `OCX_NO_PROJECT` prunes the last two.
    ///
    /// A path that does not exist exits 79, a directory with no `ocx.toml` exits 64, an unreadable
    /// one exits 74. Empty is unset.
    ///
    /// Explicit paths follow symlinks; the CWD walk rejects a symlinked `ocx.toml`.
    pub OCX_PROJECT: Path, Public, propagation = UserInput;
    /// Boolean — skip the CWD walk and `OCX_PROJECT`; explicit `--project` paths still load.
    ///
    /// Skips the CWD walk and ignores `OCX_PROJECT`; explicit `--project` still loads. Env-only: no
    /// `--no-project` flag.
    ///
    /// Forwarded so children also skip discovery. Hermetic pattern:
    /// `OCX_NO_PROJECT=1 ocx --project /ci/ocx.toml exec -- ...`.
    pub OCX_NO_PROJECT: Bool, Public, propagation = UserInput;
    /// Path at which the CWD walk for a project `ocx.toml` stops; relative values join onto the
    /// working directory. An explicit `--project` or `OCX_PROJECT` is not bounded by it.
    ///
    /// The walk probes the ceiling directory and never ascends above it, so a project file there is
    /// found and one in a parent is not. It already stops at the first `.git` entry; this bounds it
    /// where no repository marks the edge, such as a CI workspace under a shared parent.
    pub OCX_CEILING_PATH: Path, Public, propagation = UserInput;
    /// Boolean — commands that stamp shell-activation consent as a side effect write none;
    /// `--consent` / `--no-consent` outrank it and `ocx shell allow` ignores it.
    ///
    /// Forwarded, else a script run by `ocx exec` that calls `ocx pull` stamps after all.
    ///
    /// Covers `add`, `remove`, `lock`, `update`, `upgrade`, `pull`, `exec` and `init`.
    /// `ocx exec --no-consent` sets it on the child too; `--consent` does not clear it.
    ///
    /// Set it in a pipeline or image, not an interactive profile: exported globally, growing a lock
    /// past the stamped sources leaves the project inert.
    pub OCX_NO_CONSENT: Bool, Public, on_invalid = Error, propagation = UserInput;
    /// Withdrawn — no longer read; pass `--global` instead.
    ///
    /// A set value is never honoured. Each process prints one notice naming `--global` and carries
    /// on, so a leaked environment keeps working. The name stays declared until 0.7 only as the
    /// target of that notice.
    pub OCX_GLOBAL: Bool, Public, propagation = UserInput;
    /// Path to the local index directory. Mirrors `--index`.
    ///
    /// Swaps the whole local index collection (default `$OCX_HOME/index/`) for a shipped one, never
    /// a partial overlay. It is meant for a bundled copy in a GitHub Action, Bazel rule or
    /// devcontainer. `--index` takes precedence.
    ///
    /// Under `--remote` or `OCX_REMOTE`, tag- and catalog-addressed lookups bypass it, while
    /// digest-addressed lookups still consult it first.
    pub OCX_INDEX: Path, Public, propagation = UserInput;
    /// Directories searched, read-only, for the blobs a command needs before it contacts a registry.
    /// Mirrors `--blob-dir`, which is repeatable.
    ///
    /// A list in the platform path-list syntax: `:` on Unix, `;` on Windows. Empty entries are
    /// skipped, an empty value is unset, and a list of more than 32 entries is refused (exit 78). A directory that is missing
    /// here is skipped with one warning on first use. Every blob is checked against its digest, so a
    /// directory cannot change what a digest means.
    ///
    /// A `--blob-dir` flag replaces the whole list, and the list replaces `[blobs] dirs`. Forwarded,
    /// so a launcher re-entry reads the same directories.
    pub OCX_BLOB_DIR: PathList, Public, propagation = UserInput;
    /// Directory where blobs fetched from a registry are kept for later runs. Mirrors
    /// `--blob-cache-dir`.
    ///
    /// Writable, unlike `OCX_BLOB_DIR`; OCX never writes into a blob directory. An empty value is
    /// unset. `--blob-cache-dir` takes precedence, and this variable outranks `[blobs] cache_dir`.
    /// Forwarded, so a launcher re-entry fills the same cache.
    pub OCX_BLOB_CACHE_DIR: Path, Public, propagation = UserInput;
    /// The registry a bare identifier resolves under; outranks `[registry] default`, else
    /// `ocx.sh`. Empty is unset.
    ///
    /// Forwarded so every frame of a launch chain agrees.
    ///
    /// Environment-only: no CLI flag, so the forwarded value is the one this invocation read. An
    /// empty value clears an inherited setting rather than travelling onward.
    pub OCX_DEFAULT_REGISTRY: String, Public, propagation = UserInput;
    /// Comma-separated `host[:port]` authorities that may be dialled over plain HTTP.
    ///
    /// Forwarded, or a child with a narrower set cannot reach a registry the parent just pulled from.
    ///
    /// Union with `[registries.<name>] insecure`; only a system-tier `insecure = false` subtracts.
    /// Names match exactly as `host[:port]`.
    ///
    /// Also gates plain `http://` `[mirrors]` targets. Credentials travel in the clear; an HTTPS
    /// registry naming an `http` token realm, or redirected to `http://`, is still refused.
    pub OCX_INSECURE_REGISTRIES: HostList, Public, propagation = UserInput;
    /// Boolean — allow a tag resolving to a yanked index entry. Forwarded to child OCX.
    ///
    /// Without it a tag resolve against a yanked entry of the public index exits 65. This holds
    /// online and against a local copy with no network. `ocx index update` does not bypass it. A
    /// digest-pinned reference never needs it, and a project resolved from `ocx.lock` is
    /// unaffected by a yank.
    pub OCX_ALLOW_YANKED: Bool, Public, propagation = UserInput;

    // ── Resolution and launch state ──

    /// Path to the patch snapshot file `ocx patch freeze` writes, whose pinned digests win over live tag lookups.
    /// Forwarded to child OCX.
    ///
    /// While set, every command but `ocx patch sync` (refused, exit 78) and `ocx patch freeze` uses
    /// only the snapshot's pinned digests and records no pin. A companion the snapshot omits is
    /// absent, and a required one fails with exit 79, as does a missing pinned blob under
    /// `--offline`.
    ///
    /// A format version this OCX cannot read exits 65. Independent of `--frozen`; `ocx clean` roots
    /// the snapshot's companions only while this is set.
    pub OCX_PATCH_SNAPSHOT: Path, Public, propagation = UserInput;
    /// JSON object mapping an upstream host to a mirror string or `{"registry"?, "index"?}`,
    /// the same union `[mirrors."<host>"]` accepts. Forwarded to child OCX.
    ///
    /// A string value redirects both the `registry` and `index` roles; the object form splits them.
    /// A role value is `scheme://host[/repo-key-prefix]`.
    ///
    /// Replaces the `[mirrors]` entry per host and per role; unlike a config-file `registry` entry
    /// it cannot drop a namespace off the compiled-in index. A malformed value (bad JSON, wrong
    /// shape, `http://` for a host not plaintext-eligible) aborts at startup.
    pub OCX_MIRRORS: Json, Public, propagation = UserInput;
    /// JSON object encoding the resolved `[patches]` config; forwarded only when configured.
    pub OCX_PATCHES: Json, Public, propagation = UserInput;
    /// Boolean — `ocx self setup` modifies no shell profile. Mirrors `--no-modify-path`.
    ///
    /// Suppresses both PATH surfaces on every platform, the managed profile blocks and the
    /// session-level registration; each location still shows in the summary as `skipped_opt_out`.
    /// Second rung of the ladder: `--no-modify-path`, this variable, `[shell] modify_path`, then
    /// allowed.
    ///
    /// The opt-out persists as `modify_path = false`; there is no positive flag. Only truthy values
    /// enable it.
    pub OCX_NO_MODIFY_PATH: Bool, Public, propagation = UserInput;
    /// OCI reference overriding `[managed].source` for this invocation only; empty is unset,
    /// `OCX_NO_CONFIG` suppresses it. Forwarded to child OCX.
    ///
    /// Never written back to the seed; only `ocx config setup` and
    /// `ocx self setup --managed-config` write it. Resolving the tier never fetches: pair the variable with an
    /// explicit `ocx config update` in CI.
    ///
    /// Forwarded to children. Suppressed entirely by `OCX_NO_CONFIG`.
    pub OCX_MANAGED_CONFIG: String, Public, propagation = UserInput;
    /// Boolean — kill switch for the managed-config background refresh; `ocx config update` still works.
    ///
    /// Forwarded, or a child's refresh replaces the managed tier with config the parent never saw.
    ///
    /// Silences both the `apply` and `notify` postures of the background tick. `ocx config update`
    /// still runs; `ocx self setup` and `ocx config setup` still re-sync an adopted seed regardless
    /// (use `--offline` to skip that).
    ///
    /// The tick is also off when `CI` is truthy, `OCX_OFFLINE` is set or stderr is not a terminal.
    /// Distinct from `OCX_NO_UPDATE_CHECK`, which covers the OCX self-update notice.
    pub OCX_NO_CONFIG_REFRESH: Bool, Public, propagation = UserInput;
    /// Directory execution records are written to; absent or empty turns recording off.
    /// Forwarded so every frame of a launch chain records into the outermost sink.
    ///
    /// Equivalent to `--records-dir` on `ocx exec` and `ocx package exec` and to the
    /// `[records] dir` key. Unset at every tier means no record and no added I/O.
    ///
    /// Forwarded, so a launcher re-entry (`ocx launcher exec`, which takes no flags) writes to the
    /// same sink. The flag overrides it; a system-tier `[records]` declaration locks the section
    /// and this variable then has no effect.
    pub OCX_RECORDS_DIR: Path, Public, propagation = UserInput;
    /// Filename template for each execution record; forwarded alongside `OCX_RECORDS_DIR`.
    ///
    /// No `OCX_RECORDS_REQUIRED` exists: fail-closed recording is config-file-only operator policy.
    ///
    /// Equivalent to `--records-name` and the `[records] name` key; the flag overrides it. The
    /// placeholders are `{time}`, `{pid}`, `{rand}` and `{host}`; an unrecognized one is a
    /// configuration error (exit 78), never a literal.
    ///
    /// Needs a sink from `OCX_RECORDS_DIR`, `--records-dir` or `[records] dir`. Locked with the
    /// rest of `[records]` under a system-tier policy.
    pub OCX_RECORDS_NAME: String, Public, propagation = UserInput;

    // ── Telemetry ──

    /// Switch for exporting usage telemetry; unset or empty defers to `[telemetry]` in config.
    ///
    /// `1`, `y`, `yes`, `on` and `true` (ASCII case-insensitive) turn it on; `0`, `n`, `no`, `off`
    /// and `false` are an explicit off. Any other value, a non-UTF-8 one included, is an explicit
    /// off plus one warning per run naming this variable and never the value, so a typo never
    /// arms export and never defers to a file that could.
    ///
    /// Forwarded to child OCX, so a nested run agrees with its parent.
    pub OCX_TELEMETRY: TriBool, Public, propagation = UserInput;

    // ── Toolchain ──

    /// Lazy materialization mode (`never` / `always`), the weakest tier of the `lazy-mode` ladder.
    ///
    /// Not forwarded: it changes when content materializes, never which digest resolves.
    ///
    /// `never` (default) composes eagerly, `always` composes a shim that defers the download to
    /// first invocation. Case-insensitive, unlike the `--lazy-mode` flag and the `ocx.toml` key.
    ///
    /// An unrecognized value is ignored with a warning. Overridden by the `--lazy-mode` flag,
    /// `[package."<id>"]`, `[group.<name>]` and the top-level `lazy-mode` key.
    pub OCX_LAZY_MODE: Choice["never", "always"], Public, propagation = UserInput;
    /// Lazy materialization report (`silent` / `progress`); not forwarded, like `OCX_LAZY_MODE`.
    ///
    /// `silent` (default) or `progress`, case-insensitive. `progress` opens a channel on the
    /// controlling terminal only when the first-invocation download actually happens. It falls
    /// back to `silent` where no terminal is reachable (Docker build, CI, `setsid`).
    ///
    /// Read inside `ocx launcher shim`, never by the composing command. Overridden by
    /// `--lazy-report`, `[package."<id>"]` and the top-level key; there is no group tier.
    pub OCX_LAZY_REPORT: Choice["silent", "progress"], Public, propagation = UserInput;
    /// Toolchain activation mode (`env` / `bin` / `none`), the weakest tier of the `activate`
    /// ladder. Not forwarded, like `OCX_LAZY_MODE`.
    ///
    /// `env` composes the toolchain environment every prompt (default), `bin` puts the home's
    /// `active/bin` on `PATH` only, `none` withdraws what OCX owns. Case-insensitive, not trimmed.
    ///
    /// An unrecognized value warns and falls through to the next tier; the `ocx.toml` key is a
    /// parse error (exit 78). Sits below the `ocx.toml` `activate` key.
    pub OCX_TOOLCHAIN_ACTIVATE: Choice["env", "bin", "none"], Public, propagation = UserInput;
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
    pub OCX_TOOLCHAIN_PINNED: TriBool, Public, propagation = UserInput;
    /// Root under which project toolchain homes render — the env tier of `toolchain_dir`.
    ///
    /// Weakest tier: `config.toml` wins. A refused root exits 78.
    pub OCX_TOOLCHAIN_DIR: Path, Public, propagation = UserInput;
    /// Boolean — skip the policy-gated auto-verify on install/pull; `--no-verify` wins. Forwarded to child OCX.
    ///
    /// Skips the policy-gated auto-verify on `install`, `pull` and every command that auto-installs
    /// on demand (`exec`, `env`, `package exec`, `package env`, patch discovery). Only `install`
    /// and `pull` carry the `--verify` / `--no-verify` flag, which outranks this variable. A
    /// skipped package that publishers or a policy cover prints one `warning:` line per invocation.
    ///
    /// With no publishers listed in the index and no `[[trust.policy]]` covering a package, verification does not run regardless.
    pub OCX_NO_VERIFY: Bool, Public, on_invalid = Error, propagation = UserInput;

    // ── Shell activation ──

    /// Project directories (separated like `PATH`) the per-prompt shell hook may activate in
    /// without a consent stamp, subtrees included.
    ///
    /// An entry is an exact canonicalized directory, or with a trailing `/*` that directory and
    /// everything beneath it.
    pub OCX_CONSENT_PATHS: PathList, Public, propagation = UserInput;
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
    pub OCX_CONSENT_NAMESPACES: String, Public, propagation = UserInput;
    /// Boolean — `ocx self activate` skips the shell-completion block.
    ///
    /// `PATH` prepend and the global env eval still run. Use it when a framework such as oh-my-zsh
    /// or a Nix completion store manages completions.
    ///
    /// `ocx shell completion` without `--if-enabled` generates the script regardless; with it, this
    /// variable is the first rung of the policy that decides whether anything prints.
    pub OCX_NO_COMPLETION: Bool, Public, propagation = UserInput;
    /// Boolean — disables the per-prompt shell hook; `ocx self activate` still runs at shell start.
    ///
    /// `ocx self activate` still prepends `PATH`, loads completions and evals the global env, but
    /// registers no per-prompt hook. Read once at shell start, so exporting it mid-session takes
    /// effect at the next shell.
    ///
    /// The only variable that makes a shell wholly inert: `OCX_NO_CONFIG` leaves explicit
    /// `--config` files and the consent grants active. Boolean, not tri-state; `--hook` is the
    /// positive channel.
    pub OCX_NO_HOOK: Bool, Public, propagation = UserInput;

    // ── Output, logging and runtime ──

    /// Log level, as `--log-level` accepts it; the flag wins. Defaults to `info`.
    pub OCX_LOG_LEVEL: String, Public, propagation = UserInput;
    /// Log level for console messages only; outranks `OCX_LOG_LEVEL` there.
    pub OCX_LOG_CONSOLE: String, Public, propagation = UserInput;
    /// Boolean — suppresses the stdout report and the transfer progress bars.
    pub OCX_QUIET: Bool, Public, propagation = UserInput;
    /// Maximum number of root packages pulled in parallel.
    ///
    /// Applies to every command that fans out through the package manager (`install`, `pull`,
    /// `package pull`, `exec`, `env`). `0` uses all logical cores, unlike Cargo's `--jobs 0` which
    /// errors.
    ///
    /// Negative or non-numeric values are ignored with a warning; unset is unbounded. `--jobs`
    /// takes precedence.
    pub OCX_JOBS: Integer, Public, propagation = UserInput;
    /// Boolean — kill switch for the background self-update and toolchain drift checks; forwarded to child OCX.
    ///
    /// The checks are also off when `CI` is truthy, `OCX_OFFLINE` is set, stderr is not a terminal,
    /// or the command is `version`, `about` or `shell completion`. The drift notice additionally
    /// skips `update`, `lock`, `add`, `remove` and `init`. This variable also suppresses the
    /// throttle state write.
    ///
    /// Distinct from `OCX_NO_CONFIG_REFRESH`, which silences only the managed-config tick.
    pub OCX_NO_UPDATE_CHECK: Bool, Public, propagation = UserInput;
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
    pub OCX_UPDATE_CHECK_INTERVAL: String, Public, propagation = UserInput;
    /// `RefreshPolicy` wire value for OCX's own update check; beats `[update] self`. Not forwarded.
    ///
    /// `notify` (default) prints a notice naming `ocx self update`, `apply` installs the newer OCX
    /// after the command finishes without changing its exit code, `manual` never checks. Lowercase
    /// only; an unrecognized value is ignored at debug level and falls through.
    ///
    /// Beats the config file. `apply` needs a terminal on stderr, `CI` unset, online, and
    /// `OCX_NO_UPDATE_CHECK` unset, and does nothing for an OCX that `ocx self setup` did not
    /// install.
    pub OCX_SELF_UPDATE: Choice["apply", "notify", "manual"], Public, propagation = UserInput;
    /// `RefreshPolicy` wire value for the toolchain drift check; beats `[update] toolchain`. Not forwarded.
    ///
    /// `notify` (default) prints one stderr line per toolchain file with drift, naming
    /// `ocx update`; `apply` is read as `notify`, since only `ocx update` moves a pin; `manual` never
    /// checks. Lowercase only; other values are ignored at debug level.
    ///
    /// The check compares the lock's platform digests with the registry without writing the local
    /// index. It probes a project only when it is consented. It stays silent on a missing or
    /// stale lock, a registry error or a 5-second timeout.
    pub OCX_TOOLCHAIN_UPDATE: Choice["apply", "notify", "manual"], Public, propagation = UserInput;
    /// Boolean — skip ad-hoc code signing of macOS binaries after installation.
    pub OCX_NO_CODESIGN: Bool, Public, propagation = UserInput;

    // ── Credentials and certificates ──

    /// Extra CA roots — a path or inline PEM (contains `-----BEGIN`); empty is unset.
    ///
    /// Adds trust for registry, index, forge and Sigstore clients on top of the host store and
    /// compiled-in roots. Wins over every `config.toml` tier unless the `extra_ca_certs` pair is
    /// system-locked, when it is ignored with a warning.
    ///
    /// A failure still reports `UnknownIssuer` (exit 69). On Windows an inline PEM can hit
    /// cmd.exe's 8,191-character limit; use a path.
    pub OCX_EXTRA_CA_CERTS: PathOrPem, Public, propagation = UserInput;
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
    pub OCX_SIGSTORE_TRUSTED_ROOT: Path, Public, propagation = UserInput;
    /// OIDC bearer token for keyless signing, below `--identity-token-file`/`-stdin`.
    ///
    /// Used by `ocx package sign` and `ocx package attest` only when neither
    /// `--identity-token-file` nor `--identity-token-stdin` is given; ambient CI detection and
    /// browser OAuth rank below it. Never forwarded or logged, and stripped from plugin-dispatched
    /// children and from any environment OCX writes.
    ///
    /// A short-lived JWT (typically under 10 minutes): fetch a fresh one just before signing.
    pub OCX_IDENTITY_TOKEN: String, Public, secret, propagation = Credential;
    /// Password for an encrypted signing key; never a flag, since `argv` is visible host-wide.
    ///
    /// Pair it with `--key <path>` or `OCX_SIGNING_KEY`; it opens a key and never names one. An
    /// unset variable reads as the empty password. That is the shape of a pair made by pressing
    /// enter twice in `cosign generate-key-pair`, so that case is supported rather than an error.
    ///
    /// Stripped from plugin-dispatched children and from any environment OCX writes. OCX does not
    /// generate key pairs.
    pub OCX_KEY_PASSWORD: String, Public, secret, propagation = Credential;
    /// The signing key PEM itself, never a path, for `--key env://OCX_SIGNING_KEY`.
    ///
    /// A cosign `ENCRYPTED SIGSTORE PRIVATE KEY` envelope when signing, an SPKI public key for a
    /// `[[trust.policy]]` signer. For runners with no writable disk. Read literally as an `env://`
    /// target; a file key is `--key file://<path>`. Unset or empty exits 74 naming the variable;
    /// over 64 KiB exits 65. Stripped from plugin-dispatched children and written environments;
    /// a key in any other `env://` name is inherited.
    pub OCX_SIGNING_KEY: String, Public, secret, propagation = Credential;
    /// Headers sent with every telemetry export, `name=value[,name=value...]`; values are
    /// percent-decoded and names trimmed.
    ///
    /// Merges over `[telemetry] headers` per name, case-insensitively, except that a name a
    /// system-locked section sets wins. Sent only to a destination `[telemetry]` names, never to
    /// one the `OTEL_EXPORTER_OTLP_*` variables name. A malformed value disables the export for the
    /// run with one warning that names this variable and never the value.
    ///
    /// Stripped from plugin-dispatched children and from any environment OCX writes, so a nested
    /// `ocx` under `ocx exec` does not see it; put a long-lived token in a config file instead.
    pub OCX_TELEMETRY_HEADERS: String, Public, secret, propagation = Credential;
    /// The API half of the forge credential pair (a forge access token).
    ///
    /// Read by `ocx package announce` and `ocx package claim` to open or update a pull or merge
    /// request; `--output` needs none. GitHub needs `repo` scope, GitLab `api`.
    ///
    /// Under `--transport git` it authenticates only the REST calls and `OCX_ANNOUNCE_GIT_TOKEN`
    /// carries the push. In a GitLab job an unset token falls through to `CI_JOB_TOKEN`, which cannot
    /// write through the API.
    pub OCX_ANNOUNCE_TOKEN: String, Public, secret, propagation = SecretInput;
    /// The push-only forge credential (the secret half of `git push`'s Basic pair).
    ///
    /// Used only under `--transport git`, for every `git` call to the index remote; REST reads keep
    /// using `OCX_ANNOUNCE_TOKEN`. Unset or empty reuses the resolved API credential.
    ///
    /// Sent as the Basic password via git's config environment, never in a URL, `argv` or
    /// `.git/config`; the user half is `OCX_ANNOUNCE_GIT_USERNAME`. Stripped from plugin-dispatched
    /// children and from any environment OCX writes, unlike the API half.
    pub OCX_ANNOUNCE_GIT_TOKEN: String, Public, secret, propagation = Credential;
    /// The user half of the git write transport's Basic pair, default `gitlab-ci-token`;
    /// a value containing `:`, and an empty one, fall back to the default.
    ///
    /// Not a credential, so plugins inherit it from the ambient environment.
    pub OCX_ANNOUNCE_GIT_USERNAME: String, Public, reference, propagation = UserInput;
    /// Authentication type for one registry.
    ///
    /// `<REGISTRY>` is the registry name with every non-alphanumeric character replaced by `_`, case
    /// kept, so `docker.io` reads `OCX_AUTH_docker_io_TYPE`.
    ///
    /// Any `OCX_AUTH_*` value set for a registry means OCX never invokes a Docker credential helper
    /// for it; `anonymous` opts a registry out of helper lookup entirely.
    pub OCX_AUTH_TYPE = "OCX_AUTH_{REGISTRY}_TYPE": Choice["anonymous", "basic", "token", "bearer"], Public, propagation = UserInput;
    /// User name for one registry; `<REGISTRY>` is the registry slug.
    ///
    /// Read for the `basic` type only; ignored for any other type.
    pub OCX_AUTH_USER = "OCX_AUTH_{REGISTRY}_USER": String, Public, reference, propagation = UserInput;
    /// Registry token for one registry; `<REGISTRY>` is the registry slug.
    ///
    /// The bearer token, or the password for `basic`; ignored unless the type is `bearer` or `basic`.
    pub OCX_AUTH_TOKEN = "OCX_AUTH_{REGISTRY}_TOKEN": String, Public, secret, propagation = SecretInput;

    // ── Plumbing ──

    /// The execution state: what OCX last applied to the shell, and the policy a parent OCX passed down.
    ///
    /// The transport state: it tells a value OCX wrote from one you typed, and a child OCX reads it to see what
    /// the shell applied. Crossing into the interactive shell makes it a user-facing contract despite the
    /// private name. `unset __OCX_ENV_STATE` (`unset-env` in elvish) means forget: the next prompt starts over
    /// and leaves the applied values in place.
    ///
    /// A reader refuses any applied record on a credential key or an `OCX_AUTH_*` key, because reverting a
    /// forged record would drop your credential. Reserved: a package cannot declare it and `--env` cannot set it.
    pub __OCX_ENV_STATE: String, Plumbing, propagation = State;
    /// Elvish only: the recording shell's pid and `$pwd`, so the hook re-runs on a directory change.
    ///
    /// Holds the pid, `$pwd`, the direnv/mise yield sentinels and the two consent-grant variables,
    /// space-joined; never set it by hand.
    pub __OCX_ENV_PWD: String, Plumbing, propagation = HookLocal;
    /// A hook-local copy of a stood-down `__OCX_ENV_STATE`, never transported.
    ///
    /// Kept by a shell hook that cannot hold an unexported variable across prompts (elvish); removed at
    /// every projected spawn, so it never reaches a child. Never set it by hand.
    pub __OCX_ENV_STOOD_DOWN: String, Plumbing, propagation = HookLocal;
    /// Set by OCX on every credential helper it starts until 0.7; an OCX running under it never downloads.
    ///
    /// A helper installed through OCX is a trampoline that starts `ocx exec`; without the marker that
    /// OCX would install the package the outer OCX is still pulling and wait on its lock. Under the
    /// marker OCX behaves as if `--offline` were given.
    pub __OCX_CREDENTIAL_HELPER: Bool, Plumbing, propagation = HookLocal;

    // ── Test seams ──

    /// Fault stage injected into a project mutation.
    pub __OCX_TESTING_FAULT: String, Testing, propagation = UserInput;
    /// File whose appearance releases a held `__OCX_TESTING_FAULT` stage.
    pub __OCX_TESTING_FAULT_RELEASE_FILE: Path, Testing, propagation = UserInput;
    /// Fixed clock for the announce pipeline.
    pub __OCX_TESTING_ANNOUNCE_CLOCK: String, Testing, propagation = UserInput;
    /// Milliseconds a config edit holds its lock.
    pub __OCX_TESTING_CONFIG_EDIT_HOLD_MS: Integer, Testing, propagation = UserInput;
    /// Base URL replacing the forge API host.
    pub __OCX_TESTING_FORGE_BASE_URL: String, Testing, propagation = UserInput;
    /// Base URL replacing the GitHub REST API host for the packages deleter.
    pub __OCX_TESTING_GITHUB_API_URL: String, Testing, propagation = UserInput;
    /// Milliseconds the forge waits to confirm a write.
    pub __OCX_TESTING_FORGE_CONFIRM_MS: Integer, Testing, propagation = UserInput;
    /// Milliseconds before a credential helper times out.
    pub __OCX_TESTING_HELPER_TIMEOUT_MS: Integer, Testing, propagation = UserInput;
    /// Index transport timeouts, in milliseconds.
    pub __OCX_TESTING_INDEX_TIMEOUTS_MS: String, Testing, propagation = UserInput;
    /// Milliseconds of latency injected into registry calls.
    pub __OCX_TESTING_LATENCY_INJECT_MS: Integer, Testing, propagation = UserInput;
    /// libc family the host detection reports.
    pub __OCX_TESTING_LIBC: String, Testing, propagation = UserInput;
    /// Makes the execution-record sink's probes fail.
    pub __OCX_TESTING_RECORDS_FAIL_PROBES: String, Testing, propagation = UserInput;
    /// Fault injected into the toolchain render.
    pub __OCX_TESTING_RENDER_FAULT: String, Testing, propagation = UserInput;
    /// Shells the live-shell tests must find (`1` / `all`, or a comma-separated list).
    pub __OCX_TESTING_REQUIRE_LIVE_SHELLS: String, Testing, propagation = UserInput;
    /// OCI reference `ocx self` treats as its own image.
    pub __OCX_TESTING_SELF_IMAGE: String, Testing, propagation = UserInput;
    /// Store root whose shim publish loses its race.
    pub __OCX_TESTING_SHIM_LOST_PUBLISH_RACE: Path, Testing, propagation = UserInput;
    /// Path replacing the system-tier config file.
    pub __OCX_TESTING_SYSTEM_CONFIG: Path, Testing, propagation = UserInput;
    /// Marks the child re-run of a telemetry unit test that must see the proxy variables its parent set.
    pub __OCX_TESTING_TELEMETRY_PROXY_CHILD: String, Testing, propagation = UserInput;
    /// Milliseconds before the toolchain lock times out.
    pub __OCX_TESTING_TOOLCHAIN_LOCK_TIMEOUT_MS: Integer, Testing, propagation = UserInput;

    // ── Foreign: CI and terminal ──

    /// Set by most CI providers. When truthy, the background update check and the managed-config
    /// refresh are skipped; see `OCX_NO_UPDATE_CHECK` and `OCX_NO_CONFIG_REFRESH`.
    pub CI: Bool, Foreign, propagation = UserInput;
    /// Disables colour output when set to any non-empty value.
    pub NO_COLOR: String, Foreign, propagation = UserInput;
    /// `0` disables colour output.
    pub CLICOLOR: String, Foreign, propagation = UserInput;
    /// Non-`0` forces colour output.
    pub CLICOLOR_FORCE: String, Foreign, propagation = UserInput;
    /// The terminal type.
    ///
    /// `TERM=dumb` turns colour off. `NO_COLOR` and `CLICOLOR_FORCE` rank above it.
    pub TERM: String, Foreign, propagation = UserInput;
    /// Log filter of the tracing subscriber.
    ///
    /// Fallback filter, read only when neither `OCX_LOG_CONSOLE` nor `OCX_LOG_LEVEL` is set.
    pub RUST_LOG: String, Foreign, reader = Dependency("tracing-subscriber"), propagation = UserInput;

    // ── Foreign: GitHub Actions ──

    /// `true` inside GitHub Actions.
    pub GITHUB_ACTIONS: Bool, Foreign, propagation = UserInput;
    /// File GitHub Actions reads exported variables from.
    pub GITHUB_ENV: Path, Foreign, propagation = UserInput;
    /// File GitHub Actions reads `PATH` additions from.
    pub GITHUB_PATH: Path, Foreign, propagation = UserInput;
    /// Commit the workflow runs on.
    pub GITHUB_SHA: String, Foreign, propagation = UserInput;
    /// GitHub server URL.
    pub GITHUB_SERVER_URL: String, Foreign, propagation = UserInput;
    /// `owner/repo` of the workflow.
    pub GITHUB_REPOSITORY: String, Foreign, propagation = UserInput;
    /// Workflow run id.
    pub GITHUB_RUN_ID: String, Foreign, propagation = UserInput;
    /// Login of the user that triggered the workflow.
    pub GITHUB_ACTOR: String, Foreign, propagation = UserInput;
    /// Numeric id of the user that triggered the workflow.
    pub GITHUB_ACTOR_ID: String, Foreign, propagation = UserInput;
    /// URL the runner serves OIDC tokens from.
    pub ACTIONS_ID_TOKEN_REQUEST_URL: String, Foreign, propagation = UserInput;
    /// Bearer token for `ACTIONS_ID_TOKEN_REQUEST_URL`.
    // Inherited: a tool under `ocx exec` signing keylessly needs it.
    pub ACTIONS_ID_TOKEN_REQUEST_TOKEN: String, Foreign, secret, propagation = SecretInput;

    // ── Foreign: GitLab CI and other providers ──

    /// `true` inside GitLab CI.
    pub GITLAB_CI: Bool, Foreign, propagation = UserInput;
    /// GitLab CI job token.
    pub CI_JOB_TOKEN: String, Foreign, secret, propagation = SecretInput;
    /// URL of the GitLab CI job.
    pub CI_JOB_URL: String, Foreign, propagation = UserInput;
    /// `group/project` path of the GitLab project.
    pub CI_PROJECT_PATH: String, Foreign, propagation = UserInput;
    /// URL of the GitLab project.
    pub CI_PROJECT_URL: String, Foreign, propagation = UserInput;
    /// Commit the GitLab pipeline runs on.
    pub CI_COMMIT_SHA: String, Foreign, propagation = UserInput;
    /// Creation time of the GitLab pipeline.
    pub CI_PIPELINE_CREATED_AT: String, Foreign, propagation = UserInput;
    /// Login of the GitLab user that started the job.
    pub GITLAB_USER_LOGIN: String, Foreign, propagation = UserInput;
    /// Numeric id of the GitLab user that started the job.
    pub GITLAB_USER_ID: String, Foreign, propagation = UserInput;
    /// OIDC token for keyless signing, from any provider.
    pub SIGSTORE_ID_TOKEN: String, Foreign, secret, propagation = SecretInput;
    /// CircleCI OIDC token.
    pub CIRCLE_OIDC_TOKEN_V2: String, Foreign, secret, propagation = SecretInput;

    // ── Foreign: telemetry ──

    /// Console-do-not-track convention; a truthy value is honoured as an opt-out and turns
    /// telemetry export off.
    pub DO_NOT_TRACK: Bool, Foreign, propagation = UserInput;
    /// OpenTelemetry SDK kill switch; a truthy value is honoured as an opt-out and turns
    /// telemetry export off.
    pub OTEL_SDK_DISABLED: Bool, Foreign, propagation = UserInput;
    /// OpenTelemetry logs exporter selector; honoured only as an opt-out, where `none` turns
    /// telemetry export off. Every other value is not honoured.
    pub OTEL_LOGS_EXPORTER: String, Foreign, propagation = UserInput;
    /// OpenTelemetry service name; honoured as the exported `service.name`, below `[telemetry] service_name`.
    pub OTEL_SERVICE_NAME: String, Foreign, propagation = UserInput;
    /// OpenTelemetry resource attributes, percent-decoded `key=value` pairs; honoured, below
    /// `[telemetry] resource_attributes`. A malformed pair is skipped.
    pub OTEL_RESOURCE_ATTRIBUTES: String, Foreign, propagation = UserInput;
    /// OTLP endpoint for every signal; honoured as the export destination when `OTEL_EXPORTER_OTLP_LOGS_ENDPOINT`
    /// is unset.
    pub OTEL_EXPORTER_OTLP_ENDPOINT: String, Foreign, propagation = UserInput;
    /// OTLP endpoint for logs; honoured as the export destination, used verbatim.
    pub OTEL_EXPORTER_OTLP_LOGS_ENDPOINT: String, Foreign, propagation = UserInput;
    /// OTLP export timeout for every signal, in milliseconds; honoured when
    /// `OTEL_EXPORTER_OTLP_LOGS_TIMEOUT` is unset.
    pub OTEL_EXPORTER_OTLP_TIMEOUT: Integer, Foreign, propagation = UserInput;
    /// OTLP export timeout for logs, in milliseconds; honoured.
    pub OTEL_EXPORTER_OTLP_LOGS_TIMEOUT: Integer, Foreign, propagation = UserInput;
    /// OTLP transport protocol for every signal; honoured when `OTEL_EXPORTER_OTLP_LOGS_PROTOCOL`
    /// is unset, default `http/protobuf`.
    pub OTEL_EXPORTER_OTLP_PROTOCOL: String, Foreign, propagation = UserInput;
    /// OTLP transport protocol for logs; honoured, default `http/protobuf`.
    pub OTEL_EXPORTER_OTLP_LOGS_PROTOCOL: String, Foreign, propagation = UserInput;
    /// OTLP headers for every signal; honoured only when `OTEL_EXPORTER_OTLP_ENDPOINT` names the
    /// destination and `OTEL_EXPORTER_OTLP_LOGS_HEADERS` is unset. Never sent to a `[telemetry]` endpoint.
    // Inherited: a tool under `ocx exec` exporting its own telemetry needs it.
    pub OTEL_EXPORTER_OTLP_HEADERS: String, Foreign, secret, propagation = SecretInput;
    /// OTLP headers for logs; honoured when an `OTEL_EXPORTER_OTLP_*` endpoint names the destination.
    /// Never sent to a `[telemetry]` endpoint.
    // Inherited: a tool under `ocx exec` exporting its own telemetry needs it.
    pub OTEL_EXPORTER_OTLP_LOGS_HEADERS: String, Foreign, secret, propagation = SecretInput;
    /// OTLP compression for every signal; not honoured, the request is never compressed.
    pub OTEL_EXPORTER_OTLP_COMPRESSION: String, Foreign, reader = Dependency("opentelemetry-otlp"), propagation = UserInput;
    /// OTLP compression for logs; not honoured, the request is never compressed.
    pub OTEL_EXPORTER_OTLP_LOGS_COMPRESSION: String, Foreign, reader = Dependency("opentelemetry-otlp"), propagation = UserInput;
    /// Name of the GitHub Actions job; read as a telemetry attribute.
    pub GITHUB_JOB: String, Foreign, propagation = UserInput;
    /// Id of the GitLab pipeline; read as a telemetry attribute.
    pub CI_PIPELINE_ID: String, Foreign, propagation = UserInput;
    /// Id of the GitLab job; read as a telemetry attribute.
    pub CI_JOB_ID: String, Foreign, propagation = UserInput;
    /// Name of the GitLab job; read as a telemetry attribute.
    pub CI_JOB_NAME: String, Foreign, propagation = UserInput;

    // ── Foreign: home, user and system ──

    /// The user's home directory.
    pub HOME: Path, Foreign, propagation = UserInput;
    /// The user's home directory on Windows.
    pub USERPROFILE: Path, Foreign, propagation = UserInput;
    /// Drive of the Windows home directory.
    /// Passed only to the git process `ocx package announce` runs; never used to derive paths.
    pub HOMEDRIVE: String, Foreign, propagation = UserInput;
    /// Path of the Windows home directory below `HOMEDRIVE`.
    /// Passed only to the git process `ocx package announce` runs; never used to derive paths.
    pub HOMEPATH: String, Foreign, propagation = UserInput;
    /// Windows system root, upper-case spelling.
    pub SYSTEMROOT: Path, Foreign, propagation = UserInput;
    /// Windows system root.
    pub SYSTEM_ROOT = "SystemRoot": Path, Foreign, propagation = UserInput;
    /// Windows program files directory.
    pub PROGRAM_FILES = "ProgramFiles": Path, Foreign, propagation = UserInput;
    /// Windows 32-bit program files directory.
    pub PROGRAM_FILES_X86 = "ProgramFiles(x86)": Path, Foreign, propagation = UserInput;
    /// Windows program data directory.
    pub PROGRAM_DATA = "ProgramData": Path, Foreign, propagation = UserInput;
    /// Windows roaming application data directory.
    pub APPDATA: Path, Foreign, propagation = UserInput;
    /// Directory zsh reads its startup files from.
    ///
    /// `ocx self setup` writes the zsh activation blocks into `$ZDOTDIR` instead of `$HOME`.
    pub ZDOTDIR: Path, Foreign, propagation = UserInput;
    /// XDG configuration directory.
    pub XDG_CONFIG_HOME: Path, Foreign, propagation = UserInput;
    /// XDG data directory.
    ///
    /// `ocx self setup` writes the nushell autoload file under `$XDG_DATA_HOME/nushell/vendor/autoload/`,
    /// else under `~/.local/share/nushell/vendor/autoload/`.
    pub XDG_DATA_HOME: Path, Foreign, propagation = UserInput;
    /// The user's login shell.
    pub SHELL: Path, Foreign, propagation = UserInput;
    /// Windows command interpreter; bare `ocx shell` runs it when the shell to open is `cmd`.
    pub COMSPEC: Path, Foreign, propagation = UserInput;
    /// Executable search path.
    pub PATH: PathList, Foreign, propagation = UserInput;
    /// Windows user name.
    pub USERNAME: String, Foreign, propagation = UserInput;
    /// POSIX user name.
    pub USER: String, Foreign, propagation = UserInput;
    /// POSIX login name.
    pub LOGNAME: String, Foreign, propagation = UserInput;
    /// Fixed timestamp for reproducible builds, in seconds since the epoch.
    pub SOURCE_DATE_EPOCH: Integer, Foreign, propagation = UserInput;

    // ── Foreign: coexisting tools ──

    /// Docker credential-store directory, read by the credential helper.
    pub DOCKER_CONFIG: Path, Foreign, reader = Dependency("docker_credential"), propagation = UserInput;
    /// Set by direnv inside an allowed directory.
    pub DIRENV_DIR: String, Foreign, propagation = UserInput;
    /// Set by mise's shell activation.
    pub MISE_SHELL: String, Foreign, propagation = UserInput;
    /// `PATH` before mise's activation changed it.
    pub MISE_ORIG_PATH = "__MISE_ORIG_PATH": PathList, Foreign, propagation = UserInput;

    // ── Foreign: network and TLS ──

    /// HTTP proxy URL.
    pub HTTP_PROXY: String, Foreign, reader = Dependency("reqwest"), propagation = UserInput;
    /// HTTP proxy URL, lower-case spelling.
    pub HTTP_PROXY_LOWER = "http_proxy": String, Foreign, reader = Dependency("reqwest"), propagation = UserInput;
    /// HTTPS proxy URL.
    pub HTTPS_PROXY: String, Foreign, reader = Dependency("reqwest"), propagation = UserInput;
    /// HTTPS proxy URL, lower-case spelling.
    pub HTTPS_PROXY_LOWER = "https_proxy": String, Foreign, reader = Dependency("reqwest"), propagation = UserInput;
    /// Proxy URL for every scheme.
    pub ALL_PROXY: String, Foreign, reader = Dependency("reqwest"), propagation = UserInput;
    /// Proxy URL for every scheme, lower-case spelling.
    pub ALL_PROXY_LOWER = "all_proxy": String, Foreign, reader = Dependency("reqwest"), propagation = UserInput;
    /// Hosts reached without a proxy.
    pub NO_PROXY: String, Foreign, reader = Dependency("reqwest"), propagation = UserInput;
    /// Hosts reached without a proxy, lower-case spelling.
    pub NO_PROXY_LOWER = "no_proxy": String, Foreign, reader = Dependency("reqwest"), propagation = UserInput;
    /// CA bundle git uses.
    pub GIT_SSL_CAINFO: Path, Foreign, propagation = UserInput;
    /// CA directory git uses.
    pub GIT_SSL_CAPATH: Path, Foreign, propagation = UserInput;
    /// CA bundle OpenSSL-style clients use.
    pub SSL_CERT_FILE: Path, Foreign, propagation = UserInput;
    /// CA directory OpenSSL-style clients use.
    pub SSL_CERT_DIR: Path, Foreign, propagation = UserInput;
    /// Temporary directory.
    pub TMPDIR: Path, Foreign, propagation = UserInput;
    /// Temporary directory on Windows.
    pub TEMP: Path, Foreign, propagation = UserInput;
    /// Temporary directory on Windows, alternate spelling.
    pub TMP: Path, Foreign, propagation = UserInput;
}

#[cfg(test)]
mod tests;
