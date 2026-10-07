// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The public environment manifest.

/// One variable `ocx` documents as public.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EnvVar {
    pub name: &'static str,
    /// The kind of value it takes, as `cli.json` spells it.
    pub value: &'static str,
    pub summary: &'static str,
    /// Its value stays out of `Debug` and logs.
    pub secret: bool,
}

/// Every public variable, sorted by name. A child never inherits one: the SDK sets what the caller asks for.
pub const PUBLIC: &[EnvVar] = &[
    EnvVar {
        name: "OCX_ALLOW_YANKED",
        value: "bool",
        summary: "Boolean — allow a tag resolving to a yanked index entry. Forwarded to child ocx.",
        secret: false,
    },
    EnvVar {
        name: "OCX_ANNOUNCE_GIT_TOKEN",
        value: "string",
        summary: "The push-only forge credential (the secret half of `git push`'s Basic pair).",
        secret: true,
    },
    EnvVar {
        name: "OCX_ANNOUNCE_GIT_USERNAME",
        value: "string",
        summary: "The user half of the git write transport's Basic pair, default `gitlab-ci-token`;",
        secret: false,
    },
    EnvVar {
        name: "OCX_ANNOUNCE_TOKEN",
        value: "string",
        summary: "The API half of the forge credential pair (a forge access token).",
        secret: true,
    },
    EnvVar {
        name: "OCX_AUTH_{REGISTRY}_TOKEN",
        value: "string",
        summary: "Registry token for one registry; `{REGISTRY}` is the registry slug.",
        secret: true,
    },
    EnvVar {
        name: "OCX_AUTH_{REGISTRY}_TYPE",
        value: "choice",
        summary: "Authentication type for one registry; `{REGISTRY}` is the registry slug.",
        secret: false,
    },
    EnvVar {
        name: "OCX_AUTH_{REGISTRY}_USER",
        value: "string",
        summary: "User name for one registry; `{REGISTRY}` is the registry slug.",
        secret: false,
    },
    EnvVar {
        name: "OCX_BINARY_PIN",
        value: "path",
        summary: "Absolute path to the running `ocx`, set on every spawn so a child ocx runs the",
        secret: false,
    },
    EnvVar {
        name: "OCX_CEILING_PATH",
        value: "path",
        summary: "Path at which the CWD walk for a project `ocx.toml` stops; relative values join onto the",
        secret: false,
    },
    EnvVar {
        name: "OCX_CONFIG",
        value: "path",
        summary: "Path to an explicit configuration file. Mirrors `--config`.",
        secret: false,
    },
    EnvVar {
        name: "OCX_CONSENT_NAMESPACES",
        value: "string",
        summary: "Comma-separated OCI namespaces; a project whose whole lock names sources inside them",
        secret: false,
    },
    EnvVar {
        name: "OCX_CONSENT_PATHS",
        value: "path_list",
        summary: "Project directories (separated like `PATH`) the per-prompt shell hook may activate in",
        secret: false,
    },
    EnvVar {
        name: "OCX_DEFAULT_REGISTRY",
        value: "string",
        summary: "The registry a bare identifier resolves under; outranks `[registry] default`, else",
        secret: false,
    },
    EnvVar {
        name: "OCX_ENV",
        value: "json",
        summary: "JSON envelope of the resolved project `[env]` entries plus `ocx exec --env` overrides,",
        secret: false,
    },
    EnvVar {
        name: "OCX_EXTRA_CA_CERTS",
        value: "path_or_pem",
        summary: "Extra CA roots — a path or inline PEM (contains `-----BEGIN`); empty is unset.",
        secret: false,
    },
    EnvVar {
        name: "OCX_FROZEN",
        value: "bool",
        summary: "Boolean — a tag-only reference missing from the local index errors instead of",
        secret: false,
    },
    EnvVar {
        name: "OCX_GLOBAL",
        value: "bool",
        summary: "Boolean — select the global toolchain (`$OCX_HOME/ocx.toml`). Mirrors `--global`.",
        secret: false,
    },
    EnvVar {
        name: "OCX_HOME",
        value: "path",
        summary: "The OCX data root — `$OCX_HOME`, else `~/.ocx`.",
        secret: false,
    },
    EnvVar {
        name: "OCX_IDENTITY_TOKEN",
        value: "string",
        summary: "OIDC bearer token for keyless signing, below `--identity-token-file`/`-stdin`.",
        secret: true,
    },
    EnvVar {
        name: "OCX_INDEX",
        value: "path",
        summary: "Path to the local index directory. Mirrors `--index`.",
        secret: false,
    },
    EnvVar {
        name: "OCX_INSECURE_REGISTRIES",
        value: "host_list",
        summary: "Comma-separated `host[:port]` authorities that may be dialled over plain HTTP.",
        secret: false,
    },
    EnvVar {
        name: "OCX_JOBS",
        value: "integer",
        summary: "Maximum number of root packages pulled in parallel.",
        secret: false,
    },
    EnvVar {
        name: "OCX_KEY_PASSWORD",
        value: "string",
        summary: "Password for an encrypted signing key; never a flag, since `argv` is visible host-wide.",
        secret: true,
    },
    EnvVar {
        name: "OCX_LAUNCH_IDENTITIES",
        value: "json",
        summary: "JSON map from a content digest to the `registry/repository[:tag]` names one composition",
        secret: false,
    },
    EnvVar {
        name: "OCX_LAZY_MODE",
        value: "choice",
        summary: "Lazy materialisation mode (`never` / `always`), the weakest tier of the `lazy-mode` ladder.",
        secret: false,
    },
    EnvVar {
        name: "OCX_LAZY_REPORT",
        value: "choice",
        summary: "Lazy materialisation report (`silent` / `progress`); not forwarded, like `OCX_LAZY_MODE`.",
        secret: false,
    },
    EnvVar {
        name: "OCX_LOG_CONSOLE",
        value: "string",
        summary: "Log level for console messages only; outranks `OCX_LOG_LEVEL` there.",
        secret: false,
    },
    EnvVar {
        name: "OCX_LOG_LEVEL",
        value: "string",
        summary: "Log level, as `--log-level` accepts it; the flag wins.",
        secret: false,
    },
    EnvVar {
        name: "OCX_MANAGED_CONFIG",
        value: "string",
        summary: "OCI reference overriding `[managed].source` for this invocation only; empty is unset,",
        secret: false,
    },
    EnvVar {
        name: "OCX_MIRRORS",
        value: "json",
        summary: "JSON object mapping an upstream host to a mirror string or `{\"registry\"?, \"index\"?}`,",
        secret: false,
    },
    EnvVar {
        name: "OCX_NO_CODESIGN",
        value: "bool",
        summary: "Boolean — skip ad-hoc code signing of macOS binaries after installation.",
        secret: false,
    },
    EnvVar {
        name: "OCX_NO_COMPLETION",
        value: "bool",
        summary: "Boolean — `ocx self activate` skips the shell-completion block.",
        secret: false,
    },
    EnvVar {
        name: "OCX_NO_CONFIG",
        value: "bool",
        summary: "Boolean — skip the discovered config-tier chain; explicit `--config` / `OCX_CONFIG`",
        secret: false,
    },
    EnvVar {
        name: "OCX_NO_CONFIG_REFRESH",
        value: "bool",
        summary: "Boolean — kill switch for the managed-config background refresh; `ocx config update` still works.",
        secret: false,
    },
    EnvVar {
        name: "OCX_NO_CONSENT",
        value: "bool",
        summary: "Boolean — commands that stamp shell-activation consent as a side effect write none;",
        secret: false,
    },
    EnvVar {
        name: "OCX_NO_HOOK",
        value: "bool",
        summary: "Boolean — disables the per-prompt shell reconciler; `ocx self activate` still runs at shell start.",
        secret: false,
    },
    EnvVar {
        name: "OCX_NO_MODIFY_PATH",
        value: "bool",
        summary: "Boolean — `ocx self setup` modifies no shell profile. Mirrors `--no-modify-path`.",
        secret: false,
    },
    EnvVar {
        name: "OCX_NO_PROJECT",
        value: "bool",
        summary: "Boolean — skip the CWD walk and `OCX_PROJECT`; explicit `--project` paths still load.",
        secret: false,
    },
    EnvVar {
        name: "OCX_NO_UPDATE_CHECK",
        value: "bool",
        summary: "Boolean — kill switch for the background self-update and toolchain drift checks; forwarded to child ocx.",
        secret: false,
    },
    EnvVar {
        name: "OCX_NO_VERIFY",
        value: "bool",
        summary: "Boolean — skip the policy-gated auto-verify on install/pull; `--no-verify` wins. Forwarded to child ocx.",
        secret: false,
    },
    EnvVar {
        name: "OCX_OFFLINE",
        value: "bool",
        summary: "Boolean — disables network access when truthy. Mirrors `--offline`.",
        secret: false,
    },
    EnvVar {
        name: "OCX_PATCHES",
        value: "json",
        summary: "JSON object encoding the resolved `[patches]` config; forwarded only when configured.",
        secret: false,
    },
    EnvVar {
        name: "OCX_PATCH_SNAPSHOT",
        value: "path",
        summary: "Path to the active patch snapshot file, whose pinned digests win over live tag lookups.",
        secret: false,
    },
    EnvVar {
        name: "OCX_PROJECT",
        value: "path",
        summary: "Path to an explicit project `ocx.toml`. Mirrors `--project`.",
        secret: false,
    },
    EnvVar {
        name: "OCX_QUIET",
        value: "bool",
        summary: "Boolean — suppresses the stdout report and the transfer progress bars.",
        secret: false,
    },
    EnvVar {
        name: "OCX_RECORDS_DIR",
        value: "path",
        summary: "Directory execution records are written to; absent or empty turns recording off.",
        secret: false,
    },
    EnvVar {
        name: "OCX_RECORDS_NAME",
        value: "string",
        summary: "Filename template for each execution record; forwarded alongside `OCX_RECORDS_DIR`.",
        secret: false,
    },
    EnvVar {
        name: "OCX_REMOTE",
        value: "bool",
        summary: "Boolean — uses the remote index by default when truthy. Mirrors `--remote`.",
        secret: false,
    },
    EnvVar {
        name: "OCX_SELF_UPDATE",
        value: "choice",
        summary: "`RefreshPolicy` wire value for ocx's own update check; beats `[update] self`. Not forwarded.",
        secret: false,
    },
    EnvVar {
        name: "OCX_SIGNING_KEY",
        value: "string",
        summary: "The signing key PEM itself, for `--key env://OCX_SIGNING_KEY`; any other `env://` name",
        secret: true,
    },
    EnvVar {
        name: "OCX_SIGSTORE_TRUSTED_ROOT",
        value: "path",
        summary: "Path (bare or `file://`) to a Sigstore trusted-root JSON document, or a directory holding",
        secret: false,
    },
    EnvVar {
        name: "OCX_TOOLCHAIN_ACTIVATE",
        value: "choice",
        summary: "Toolchain activation mode (`env` / `bin` / `none`), the weakest tier of the `activate`",
        secret: false,
    },
    EnvVar {
        name: "OCX_TOOLCHAIN_DIR",
        value: "path",
        summary: "Root under which project toolchain homes render — the env tier of `toolchain_dir`.",
        secret: false,
    },
    EnvVar {
        name: "OCX_TOOLCHAIN_PINNED",
        value: "tri_bool",
        summary: "Boolean — composed paths pin to digest roots; the weakest tier of the `pinned` ladder.",
        secret: false,
    },
    EnvVar {
        name: "OCX_TOOLCHAIN_UPDATE",
        value: "choice",
        summary: "`RefreshPolicy` wire value for the toolchain drift check; beats `[update] toolchain`. Not forwarded.",
        secret: false,
    },
    EnvVar {
        name: "OCX_UPDATE_CHECK_INTERVAL",
        value: "string",
        summary: "Throttle interval of the background update checks, `\\d+[smhd]?` (bare digits = seconds).",
        secret: false,
    },
];

/// Whether `name` is a public variable whose value must stay out of logs and `Debug`. A `{...}` in a declared
/// name stands for one or more characters, and names compare ignoring ASCII case, as the environment does on Windows.
pub fn is_secret(name: &str) -> bool {
    PUBLIC.iter().any(|var| var.secret && matches_name(var.name, name))
}

fn matches_name(pattern: &str, name: &str) -> bool {
    let Some((literal, rest)) = pattern.split_once('{') else {
        return pattern.eq_ignore_ascii_case(name);
    };
    let Some((_, tail)) = rest.split_once('}') else {
        return false;
    };
    let Some(head) = name.get(..literal.len()) else {
        return false;
    };
    let after = &name[literal.len()..];
    if !head.eq_ignore_ascii_case(literal) || after.is_empty() {
        return false;
    }
    after
        .char_indices()
        .skip(1)
        .map(|(index, _)| index)
        .chain(std::iter::once(after.len()))
        .any(|start| matches_name(tail, &after[start..]))
}
