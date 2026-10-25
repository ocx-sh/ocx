// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Lays a [`DocsModel`] out as reference pages: one entry per command, flag, variable, key or exit code,
//! each a heading with an explicit anchor followed by a short description and facts.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use super::{
    CommandEntry, DocsError, DocsModel, EnvVarEntry, ExitCodeEntry, FlagEntry, KeyEntry, MAX_ENTRIES_PER_PAGE, Page,
};

const COMMAND_DIR: &str = "reference/command-line";
/// The group that spans pages, and the pages it spans: `(page suffix, subcommands)`.
///
/// A subcommand is placed by its first word below `package`, so `cascade check` follows `cascade`. The bare
/// `ocx package` entry opens the first page. A subcommand missing here is an error; any other group over the
/// entry cap is too.
const SPLIT_GROUP: &str = "package";
const SPLIT_PAGES: &[(&str, &[&str])] = &[
    (
        "publishing",
        &[
            "announce",
            "attest",
            "cascade",
            "claim",
            "copy",
            "create",
            "description",
            "push",
            "sign",
            "test",
        ],
    ),
    (
        "consuming",
        &[
            "deps",
            "deselect",
            "env",
            "exec",
            "inspect",
            "install",
            "prune",
            "pull",
            "receipt",
            "sbom",
            "select",
            "uninstall",
            "verify",
            "which",
        ],
    ),
];
/// Page for the top-level commands that are not a group.
const LEAF_GROUP: &str = "commands";

/// Ids that keep a lowercase underscore, as the pages they replace spelled them.
const CONFIG_ID_OVERRIDES: &[(&str, &str)] = &[
    ("toolchain_dir", "keys-toolchain_dir"),
    ("extra_ca_certs", "keys-extra_ca_certs"),
];
const PROJECT_ID_OVERRIDES: &[(&str, &str)] = &[
    ("group.<name>", "project-config-groups"),
    ("package.<name>", "project-config-package"),
    ("env.<name>", "project-config-env"),
    ("lazy-mode", "project-config-toolchain-lazy"),
    ("activate", "project-config-activate"),
    ("pinned", "project-config-pinned"),
];

/// `(key prefix, link text, target)`: the guide or explanation that uses a key, one See line per entry.
const CONFIG_SEE: &[(&str, &str, &str)] = &[
    ("extra_ca_certs", "Proxy and CA", "../integration/proxy-and-ca.md"),
    ("extra_ca_certs_pem", "Proxy and CA", "../integration/proxy-and-ca.md"),
    (
        "managed",
        "Manage configuration centrally",
        "../guides/manage-config-centrally.mdx",
    ),
    ("mirrors", "Corporate mirror", "../guides/corporate-mirror.mdx"),
    ("patches", "Patch packages", "../guides/patch-packages.mdx"),
    ("records", "Execution records", "../explanation/execution-records.md"),
    (
        "registries.<name>.index",
        "Public index",
        "../explanation/public-index.md",
    ),
    (
        "registries.<name>.index",
        "Serve an index",
        "../guides/serve-an-index.md",
    ),
    (
        "registries.<name>.insecure",
        "Corporate mirror",
        "../guides/corporate-mirror.mdx",
    ),
    (
        "registries.<name>.trusted_hosts",
        "Corporate mirror",
        "../guides/corporate-mirror.mdx",
    ),
    ("registry", "Configure ocx", "../guides/configure-ocx.mdx"),
    (
        "shell.completions",
        "Shell integration",
        "../explanation/shell-integration.mdx",
    ),
    ("shell.consent", "Shell consent", "../explanation/shell-consent.mdx"),
    (
        "shell.hook",
        "Shell integration",
        "../explanation/shell-integration.mdx",
    ),
    (
        "shell.modify_path",
        "Shell integration",
        "../explanation/shell-integration.mdx",
    ),
    (
        "shell.profiles",
        "Shell integration",
        "../explanation/shell-integration.mdx",
    ),
    // After the `shell.*` rows: the first covering row wins, so the section itself gets the general page.
    ("shell", "Shell integration", "../explanation/shell-integration.mdx"),
    ("toolchain_dir", "Project toolchain", "../guides/project-toolchain.mdx"),
    (
        "trust.policy",
        "Pin a signer identity",
        "../guides/pin-signer-identity.mdx",
    ),
    (
        "trust.sigstore",
        "Self-hosted Sigstore",
        "../guides/self-hosted-sigstore.md",
    ),
    ("trust", "Pin a signer identity", "../guides/pin-signer-identity.mdx"),
    ("update", "Update ocx", "../guides/update-ocx.mdx"),
];

/// `(key, default)`: the default a key has when its schema states none, as the page prints it.
const CONFIG_EFFECTIVE_DEFAULTS: &[(&str, &str)] = &[
    ("shell.completions", "unset (follows session interactivity)"),
    ("shell.hook", "unset (follows session interactivity)"),
    ("shell.modify_path", "`true`"),
    ("shell.profiles", "unset (detected on every run)"),
    (
        "toolchain_dir",
        "unset (each project renders at `<project>/.ocx/toolchain`)",
    ),
];
/// `(key, relation, variable, note)`: an environment variable that overrides or joins a key, printed
/// as one fact line. The environment page maps only the variable to the key, so this is the way back.
const CONFIG_VARIABLES: &[(&str, &str, &str, &str)] = &[
    (
        "mirrors.<name>",
        "Overridden by",
        "OCX_MIRRORS",
        "per host and role; a role it omits still comes from `[mirrors]`",
    ),
    ("patches.registry", "Overridden by", "OCX_PATCHES", ""),
    (
        "shell.consent.namespaces",
        "Joined by",
        "OCX_CONSENT_NAMESPACES",
        "the two are unioned across tiers",
    ),
    (
        "shell.consent.paths",
        "Joined by",
        "OCX_CONSENT_PATHS",
        "the two are unioned across tiers",
    ),
    (
        "shell.modify_path",
        "Overridden by",
        "OCX_NO_MODIFY_PATH",
        "outranks this key; `--no-modify-path` outranks both and writes `false`",
    ),
    (
        "toolchain_dir",
        "Environment fallback",
        "OCX_TOOLCHAIN_DIR",
        "read only when no config file sets the key",
    ),
];
const PROJECT_SEE: &[(&str, &str, &str)] = &[
    ("env", "Environments", "../explanation/environments.mdx"),
    ("tools", "Project toolchain", "../guides/project-toolchain.mdx"),
    (
        "activate",
        "Toolchain activation",
        "../explanation/toolchain-activation.md",
    ),
    ("group", "Project groups", "../explanation/project-groups.md"),
    ("lazy-mode", "Deferred packages", "../explanation/deferred-packages.mdx"),
    (
        "lazy-report",
        "Deferred packages",
        "../explanation/deferred-packages.mdx",
    ),
    ("package", "Patches", "../explanation/patches.mdx"),
    (
        "pinned",
        "Toolchain activation",
        "../explanation/toolchain-activation.md",
    ),
    ("trust", "Pin a signer identity", "../guides/pin-signer-identity.mdx"),
];

/// Where a `config.toml` is read from, and the size cap every file obeys.
fn config_locations() -> String {
    entry(vec![
        "## Config file locations {#config-file-locations}".to_owned(),
        format!(
            "OCX reads each file that exists, lowest precedence first. A file over {} KiB is refused with exit 78.",
            ocx_config::loader::MAX_CONFIG_SIZE / 1024
        ),
        "| Tier | Path |\n| --- | --- |\n| System | `/etc/ocx/config.toml` |\n| User on Linux | `$XDG_CONFIG_HOME/ocx/config.toml`, else `~/.config/ocx/config.toml` |\n| User on macOS | `~/Library/Application Support/ocx/config.toml` |\n| User on Windows | `%APPDATA%\\ocx\\config.toml` |\n| OCX home | `$OCX_HOME/config.toml`, default `~/.ocx/config.toml` |\n| Explicit | [`OCX_CONFIG`](./environment.md#ocx-config) and [`--config`](./command-line/index.md#arg-config) |".to_owned(),
    ])
}

pub(super) fn pages(model: &DocsModel) -> Result<Vec<Page>, DocsError> {
    let mut ids = Ids::default();
    let mut out = Vec::new();

    let mut global = model.global_flags.iter().collect::<Vec<_>>();
    global.sort_by(|a, b| a.long.cmp(&b.long));
    let mut index_entries: Vec<String> = global
        .into_iter()
        .map(|flag| global_flag_entry(flag, &ids.claim(&global_flag_base(&flag.long), "global")))
        .collect();

    let mut groups: BTreeMap<String, Vec<&CommandEntry>> = BTreeMap::new();
    for command in &model.commands {
        let Some(first) = command.path.first() else {
            continue;
        };
        let nested = command.path.len() > 1
            || model
                .commands
                .iter()
                .any(|other| other.path.len() > command.path.len() && other.path.starts_with(&command.path));
        let group = if nested { first.clone() } else { LEAF_GROUP.to_owned() };
        groups.entry(group).or_default().push(command);
    }

    let mut listing: Vec<(String, String, String)> = Vec::new();
    let mut moved: Vec<(String, String)> = Vec::new();
    for (name, mut commands) in groups {
        commands.sort_by(|a, b| a.path.cmp(&b.path));
        // Claimed in path order before the split, so an anchor never depends on which page its command lands on.
        let claimed: Vec<(&CommandEntry, String)> = commands
            .into_iter()
            .map(|command| {
                let id = ids.claim(&command_id(&command.path), &command.path.join("-"));
                (command, id)
            })
            .collect();
        for (suffix, file, chunk) in split(&name, claimed)? {
            let title = match (name.as_str(), suffix) {
                (LEAF_GROUP, _) => "Top-level commands".to_owned(),
                (_, "") => format!("ocx {name}"),
                (_, suffix) => format!("ocx {name}: {suffix}"),
            };
            let mut entries = Vec::new();
            for (command, id) in &chunk {
                moved.push((id.clone(), format!("./{file}#{id}")));
                entries.push(command_entry(command, id));
            }
            let names = chunk
                .iter()
                .map(|(command, _)| format!("`ocx {}`", command.path.join(" ")))
                .collect::<Vec<_>>()
                .join(", ");
            listing.push((title.clone(), file.clone(), names));
            out.push(page(&format!("{COMMAND_DIR}/{file}"), &title, "", entries));
        }
    }

    if !listing.is_empty() {
        let rows = listing
            .iter()
            .map(|(title, file, names)| format!("| [{title}](./{file}) | {names} |"))
            .collect::<Vec<_>>()
            .join("\n");
        index_entries.push(entry(vec![
            "## Command pages {#command-pages}".to_owned(),
            "Every command is documented on the page of its group.".to_owned(),
            format!("| Page | Commands |\n| --- | --- |\n{rows}"),
        ]));
    }
    moved.push(("exit-codes".to_owned(), "../exit-codes.md".to_owned()));
    moved.sort();
    let rows = moved
        .iter()
        .map(|(id, target)| format!("| `{id}` | [{}]({target}) |", target.trim_start_matches("./")))
        .collect::<Vec<_>>()
        .join("\n");
    index_entries.push(entry(vec![
        "## Anchors on other pages {#moved-anchors}".to_owned(),
        "These anchors resolve on the page listed.".to_owned(),
        format!("| Anchor | Page |\n| --- | --- |\n{rows}"),
    ]));
    out.push(page(
        &format!("{COMMAND_DIR}/index.md"),
        "Command line",
        "Options before the command name apply to every command. Each command group has its own page.",
        index_entries,
    ));

    out.push(env_page(&model.env_vars));
    out.push(keys_page(
        &KeysPage {
            path: "reference/configuration.md",
            title: "Configuration",
            intro: "Keys of `config.toml`, from the published config schema. Tiers, precedence and the kill switch are in [Configuration](../explanation/configuration.mdx).",
            prefix: "keys",
            overrides: CONFIG_ID_OVERRIDES,
            see: CONFIG_SEE,
            defaults: CONFIG_EFFECTIVE_DEFAULTS,
            variables: CONFIG_VARIABLES,
        },
        &model.config_keys,
        vec![config_locations()],
    ));
    out.push(keys_page(
        &KeysPage {
            path: "reference/project-file.md",
            title: "Project file",
            intro: "Keys of `ocx.toml`, from the published project schema. An unknown key is a parse error (exit 78), so `config.toml`-only keys such as `[shell]`, `[update]` and `toolchain_dir` are refused here.",
            prefix: "project",
            overrides: PROJECT_ID_OVERRIDES,
            see: PROJECT_SEE,
            defaults: &[],
            variables: &[],
        },
        &model.project_keys,
        Vec::new(),
    ));
    out.push(exit_page(&model.exit_codes));
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// A command with its claimed anchor.
type Claimed<'a> = (&'a CommandEntry, String);

/// `(page suffix, file, commands)`: one page of a command group.
type GroupPage<'a> = (&'static str, String, Vec<Claimed<'a>>);

/// The pages of one group; only [`SPLIT_GROUP`] spans more than one.
fn split<'a>(name: &str, commands: Vec<Claimed<'a>>) -> Result<Vec<GroupPage<'a>>, DocsError> {
    // `index.md` is the landing page, so the `index` group cannot take its own name.
    let stem = if name == "index" { "index-commands" } else { name };
    let pages = if name == SPLIT_GROUP {
        let mut pages: Vec<GroupPage<'a>> = SPLIT_PAGES
            .iter()
            .map(|(suffix, _)| (*suffix, format!("{stem}-{suffix}.md"), Vec::new()))
            .collect();
        for claimed in commands {
            let word = claimed.0.path.get(1).map(String::as_str);
            let index = match word {
                None => 0,
                Some(word) => SPLIT_PAGES
                    .iter()
                    .position(|(_, members)| members.contains(&word))
                    .ok_or_else(|| DocsError::UngroupedCommand {
                        command: claimed.0.path.join(" "),
                    })?,
            };
            pages[index].2.push(claimed);
        }
        pages
    } else {
        vec![("", format!("{stem}.md"), commands)]
    };
    for (_, file, chunk) in &pages {
        if chunk.len() > MAX_ENTRIES_PER_PAGE {
            return Err(DocsError::PageTooLarge {
                page: PathBuf::from(format!("{COMMAND_DIR}/{file}")),
                entries: chunk.len(),
            });
        }
    }
    Ok(pages)
}

/// Hands out anchors; a taken one is retried behind the owner's prefix, then numbered.
#[derive(Default)]
struct Ids(BTreeSet<String>);

impl Ids {
    fn claim(&mut self, base: &str, prefix: &str) -> String {
        let mut id = base.to_owned();
        if self.0.contains(&id) {
            id = format!("{prefix}-{base}");
        }
        let mut number = 2;
        while self.0.contains(&id) {
            id = format!("{prefix}-{base}-{number}");
            number += 1;
        }
        self.0.insert(id.clone());
        id
    }
}

/// The id a global flag heading claims first; `--global` would collide with the `global-` prefix.
fn global_flag_base(long: &str) -> String {
    if long == "global" {
        "global-flag".to_owned()
    } else {
        format!("arg-{long}")
    }
}

/// The anchor the command had on the single command page.
fn command_id(path: &[String]) -> String {
    match path.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["env"] => "env-root".to_owned(),
        ["package", "deps"] => "deps".to_owned(),
        ["package", "which"] => "which".to_owned(),
        _ => path.join("-"),
    }
}

/// The anchor of a variable declared by OCX; foreign ones take `external-` on top.
fn env_slug(name: &str) -> String {
    if name == "OCX_TOOLCHAIN_DIR" {
        return "ocx-toolchain_dir".to_owned();
    }
    kebab(&name.replace(['{', '}'], ""))
}

fn env_id(var: &EnvVarEntry) -> String {
    let slug = env_slug(&var.name);
    if var.visibility == "foreign" {
        format!("external-{slug}")
    } else {
        slug
    }
}

fn key_id(key: &str, prefix: &str, overrides: &[(&str, &str)]) -> String {
    if let Some((_, id)) = overrides.iter().find(|(name, _)| *name == key) {
        return (*id).to_owned();
    }
    let segments: Vec<String> = key
        .split('.')
        .filter(|segment| *segment != "<name>")
        .map(kebab)
        .collect();
    format!("{prefix}-{}", segments.join("-"))
}

/// Lowercase, every run of non-alphanumerics a single hyphen, none at either end.
fn kebab(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_owned()
}

fn page(path: &str, title: &str, intro: &str, entries: Vec<String>) -> Page {
    let mut body = format!("---\ntitle: \"{title}\"\n---\n<!-- doc_type: reference -->\n");
    if !intro.is_empty() {
        body.push('\n');
        body.push_str(intro);
        body.push('\n');
    }
    for entry in entries {
        body.push('\n');
        body.push_str(&entry);
    }
    Page {
        path: PathBuf::from(path),
        body,
    }
}

/// Blocks joined by blank lines, ending in one newline.
fn entry(blocks: Vec<String>) -> String {
    let mut text = blocks
        .into_iter()
        .filter(|block| !block.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    text.push('\n');
    text
}

fn command_entry(command: &CommandEntry, id: &str) -> String {
    let mut blocks = vec![
        format!("## `ocx {}` {{#{id}}}", command.path.join(" ")),
        prose(&sentence(&command.summary)),
        prose(&command.description),
    ];
    if !command.aliases.is_empty() {
        let aliases = command
            .aliases
            .iter()
            .map(|alias| format!("`{alias}`"))
            .collect::<Vec<_>>();
        blocks.push(format!("- Alias: {}", aliases.join(", ")));
    }
    if !command.usage.is_empty() {
        blocks.push(format!("```text\n{}\n```", command.usage));
    }
    if !command.flags.is_empty() {
        let mut flags = command.flags.iter().collect::<Vec<_>>();
        flags.sort_by(|a, b| a.long.cmp(&b.long));
        let rows = flags
            .into_iter()
            .map(|flag| {
                format!(
                    "| {} | {} | {} |",
                    flag_name(flag),
                    pipe_safe(
                        &flag
                            .value
                            .as_ref()
                            .map(|value| format!("`{value}`"))
                            .unwrap_or_default()
                    ),
                    cell(&flag.help)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        blocks.push(format!("| Flag | Value | Description |\n| --- | --- | --- |\n{rows}"));
    }
    entry(blocks)
}

fn global_flag_entry(flag: &FlagEntry, id: &str) -> String {
    let mut facts = Vec::new();
    if let Some(short) = flag.short {
        facts.push(format!("- Short: `-{short}`"));
    }
    if let Some(value) = &flag.value {
        facts.push(format!("- Value: `{value}`"));
    }
    if let Some(var) = &flag.env {
        facts.push(format!("- Environment: [`{var}`](../environment.md#{})", env_slug(var)));
    }
    entry(vec![
        format!("## `--{}` {{#{id}}}", flag.long),
        prose(&sentence(&flag.help)),
        prose(&flag.description),
        facts.join("\n"),
    ])
}

fn flag_name(flag: &FlagEntry) -> String {
    match flag.short {
        Some(short) => format!("`--{}`, `-{short}`", flag.long),
        None => format!("`--{}`", flag.long),
    }
}

/// A heading that gathers the variables below it; `members` is empty for a visibility group.
struct EnvGroup {
    id: &'static str,
    title: &'static str,
    intro: &'static str,
    members: &'static [&'static str],
}

const INTERNAL_GROUP: EnvGroup = EnvGroup {
    id: "internal",
    title: "Internal",
    intro: "Variables OCX writes and reads for its own plumbing. Do not set them by hand unless an entry says so.",
    members: &[],
};
const EXTERNAL_GROUP: EnvGroup = EnvGroup {
    id: "external",
    title: "External",
    intro: "Variables set by the system or by other tools, which OCX reads.",
    members: &[],
};
/// Foreign variables that share one heading, in page order.
const EXTERNAL_THEMES: &[EnvGroup] = &[
    EnvGroup {
        id: "external-shell-terminal",
        title: "Shell and terminal",
        intro: "The login shell, the terminal type and the directory zsh reads its startup files from.",
        members: &["SHELL", "TERM", "ZDOTDIR"],
    },
    EnvGroup {
        id: "external-user-name",
        title: "User name",
        intro: "The account name, which execution records store as a readable label.",
        members: &["LOGNAME", "USER", "USERNAME"],
    },
    EnvGroup {
        id: "external-windows-directories",
        title: "Windows system directories",
        intro: "Directories Windows sets, which OCX reads to refuse a system location for `toolchain_dir`.",
        members: &[
            "APPDATA",
            "ProgramData",
            "ProgramFiles",
            "ProgramFiles(x86)",
            "SYSTEMROOT",
            "SystemRoot",
        ],
    },
];

/// `(variable, "--long", target)`: a variable that mirrors a flag of one command rather than a root flag.
const COMMAND_FLAGS: &[(&str, &str, &str)] = &[(
    "OCX_NO_MODIFY_PATH",
    "--no-modify-path",
    "./command-line/self.md#self-setup",
)];

/// The boolean vocabulary every `Bool` variable shares, and the four that refuse a value outside it.
const TRUTHY_VALUES: &str = "A boolean variable reads these spellings in any case.\n\n| Reads as | Spellings |\n| --- | --- |\n| On | `1`, `y`, `yes`, `on`, `true` |\n| Off | `0`, `n`, `no`, `off`, `false` |\n\n- Strict: [`OCX_OFFLINE`](#ocx-offline), [`OCX_FROZEN`](#ocx-frozen), [`OCX_NO_CONSENT`](#ocx-no-consent) and [`OCX_NO_VERIFY`](#ocx-no-verify) exit 78 naming the variable on any other value.\n- Lenient: every other boolean variable logs a warning and uses its default.";

/// `(variable, link text, target)`: the command reference or guide that uses a variable that names no flag.
const ENV_SEE: &[(&str, &str, &str)] = &[
    ("ALL_PROXY", "Proxy and CA", "../integration/proxy-and-ca.md"),
    ("CI", "Unattended CI", "../integration/unattended-ci.md"),
    ("DOCKER_CONFIG", "Authenticate", "../guides/authenticate.mdx"),
    ("HTTPS_PROXY", "Proxy and CA", "../integration/proxy-and-ca.md"),
    ("HTTP_PROXY", "Proxy and CA", "../integration/proxy-and-ca.md"),
    ("NO_PROXY", "Proxy and CA", "../integration/proxy-and-ca.md"),
    ("OCX_ALLOW_YANKED", "Public index", "../explanation/public-index.md"),
    (
        "OCX_ANNOUNCE_GIT_TOKEN",
        "Announce from CI",
        "../publishing/announce-from-ci.md",
    ),
    (
        "OCX_ANNOUNCE_GIT_USERNAME",
        "Announce from CI",
        "../publishing/announce-from-ci.md",
    ),
    (
        "OCX_ANNOUNCE_TOKEN",
        "Announce from CI",
        "../publishing/announce-from-ci.md",
    ),
    (
        "OCX_AUTH_{REGISTRY}_TOKEN",
        "Authenticate",
        "../guides/authenticate.mdx",
    ),
    ("OCX_AUTH_{REGISTRY}_TYPE", "Authenticate", "../guides/authenticate.mdx"),
    ("OCX_AUTH_{REGISTRY}_USER", "Authenticate", "../guides/authenticate.mdx"),
    (
        "OCX_BINARY_PIN",
        "Launcher forwarding",
        "../explanation/launcher-forwarding.md",
    ),
    (
        "OCX_CEILING_PATH",
        "Project toolchain",
        "../guides/project-toolchain.mdx",
    ),
    (
        "OCX_CONSENT_NAMESPACES",
        "Shell consent",
        "../explanation/shell-consent.mdx",
    ),
    ("OCX_CONSENT_PATHS", "Shell consent", "../explanation/shell-consent.mdx"),
    ("OCX_DEFAULT_REGISTRY", "Configure ocx", "../guides/configure-ocx.mdx"),
    ("OCX_ENV", "Environments", "../explanation/environments.mdx"),
    ("OCX_EXTRA_CA_CERTS", "Proxy and CA", "../integration/proxy-and-ca.md"),
    ("OCX_HOME", "Storage", "../explanation/storage.mdx"),
    (
        "OCX_IDENTITY_TOKEN",
        "Sign a release",
        "../publishing/sign-a-release.md",
    ),
    (
        "OCX_INSECURE_REGISTRIES",
        "Corporate mirror",
        "../guides/corporate-mirror.mdx",
    ),
    ("OCX_KEY_PASSWORD", "Sign a release", "../publishing/sign-a-release.md"),
    (
        "__OCX_LAUNCH_IDENTITIES",
        "Execution records",
        "../explanation/execution-records.md",
    ),
    (
        "OCX_LAZY_MODE",
        "Deferred packages",
        "../explanation/deferred-packages.mdx",
    ),
    (
        "OCX_LAZY_REPORT",
        "Deferred packages",
        "../explanation/deferred-packages.mdx",
    ),
    (
        "OCX_LOG_CONSOLE",
        "`--log-level`",
        "./command-line/index.md#arg-log-level",
    ),
    (
        "OCX_MANAGED_CONFIG",
        "Manage configuration centrally",
        "../guides/manage-config-centrally.mdx",
    ),
    ("OCX_MIRRORS", "Corporate mirror", "../guides/corporate-mirror.mdx"),
    ("OCX_NO_CODESIGN", "Platforms", "../troubleshooting/platforms.mdx"),
    (
        "OCX_NO_COMPLETION",
        "Shell integration",
        "../explanation/shell-integration.mdx",
    ),
    ("OCX_NO_CONFIG", "Configuration", "../explanation/configuration.mdx"),
    (
        "OCX_NO_CONFIG_REFRESH",
        "Managed configuration",
        "../explanation/managed-configuration.md",
    ),
    ("OCX_NO_CONSENT", "Shell consent", "../explanation/shell-consent.mdx"),
    (
        "OCX_NO_HOOK",
        "Shell integration",
        "../explanation/shell-integration.mdx",
    ),
    ("OCX_NO_PROJECT", "Project toolchain", "../guides/project-toolchain.mdx"),
    ("OCX_NO_UPDATE_CHECK", "Update ocx", "../guides/update-ocx.mdx"),
    ("OCX_NO_VERIFY", "Verify installs", "../guides/verify-installs.mdx"),
    ("OCX_PATCHES", "Patch packages", "../guides/patch-packages.mdx"),
    ("OCX_PATCH_SNAPSHOT", "Patch packages", "../guides/patch-packages.mdx"),
    (
        "OCX_RECORDS_DIR",
        "Execution records",
        "../explanation/execution-records.md",
    ),
    (
        "OCX_RECORDS_NAME",
        "Execution records",
        "../explanation/execution-records.md",
    ),
    ("OCX_SELF_UPDATE", "Update ocx", "../guides/update-ocx.mdx"),
    ("OCX_SIGNING_KEY", "Sign a release", "../publishing/sign-a-release.md"),
    (
        "OCX_SIGSTORE_TRUSTED_ROOT",
        "Self-hosted Sigstore",
        "../guides/self-hosted-sigstore.md",
    ),
    (
        "OCX_TOOLCHAIN_ACTIVATE",
        "Toolchain activation",
        "../explanation/toolchain-activation.md",
    ),
    (
        "OCX_TOOLCHAIN_DIR",
        "Project toolchain",
        "../guides/project-toolchain.mdx",
    ),
    (
        "OCX_TOOLCHAIN_PINNED",
        "Toolchain activation",
        "../explanation/toolchain-activation.md",
    ),
    ("OCX_TOOLCHAIN_UPDATE", "Update ocx", "../guides/update-ocx.mdx"),
    ("OCX_UPDATE_CHECK_INTERVAL", "Update ocx", "../guides/update-ocx.mdx"),
    ("SSL_CERT_DIR", "Proxy and CA", "../integration/proxy-and-ca.md"),
    ("SSL_CERT_FILE", "Proxy and CA", "../integration/proxy-and-ca.md"),
    ("all_proxy", "Proxy and CA", "../integration/proxy-and-ca.md"),
    ("http_proxy", "Proxy and CA", "../integration/proxy-and-ca.md"),
    ("https_proxy", "Proxy and CA", "../integration/proxy-and-ca.md"),
    ("no_proxy", "Proxy and CA", "../integration/proxy-and-ca.md"),
];

fn env_page(vars: &[EnvVarEntry]) -> Page {
    let named = |visibility: &str, keep: &dyn Fn(&EnvVarEntry) -> bool| {
        let mut found = vars
            .iter()
            .filter(|var| var.visibility == visibility && keep(var))
            .collect::<Vec<_>>();
        found.sort_by(|a, b| a.name.cmp(&b.name));
        found
    };
    let themed = |var: &EnvVarEntry| {
        EXTERNAL_THEMES
            .iter()
            .any(|group| group.members.contains(&var.name.as_str()))
    };
    let mut sections = vec![
        (None, named("public", &|_| true)),
        (Some(&INTERNAL_GROUP), named("plumbing", &|_| true)),
        (Some(&EXTERNAL_GROUP), named("foreign", &|var| !themed(var))),
    ];
    for group in EXTERNAL_THEMES {
        let members = named("foreign", &|var| group.members.contains(&var.name.as_str()));
        sections.push((Some(group), members));
    }
    let mut ids = Ids::default();
    let mut entries = vec![entry(vec![
        format!("## Truthy values {{#{}}}", ids.claim("truthy-values", "group")),
        TRUTHY_VALUES.to_owned(),
    ])];
    for (group, members) in sections {
        if members.is_empty() {
            continue;
        }
        if let Some(group) = group {
            let id = ids.claim(group.id, "group");
            entries.push(entry(vec![
                format!("## {} {{#{id}}}", group.title),
                group.intro.to_owned(),
            ]));
        }
        for var in members {
            let id = ids.claim(&env_id(var), &kebab(&var.name));
            let mut facts = vec![
                format!("- Visibility: {}", var.visibility),
                format!("- Value: `{}`", var.value),
            ];
            if let Some(flag) = &var.flag {
                facts.push(format!(
                    "- Flag: [`{flag}`](./command-line/index.md#{})",
                    global_flag_base(flag.trim_start_matches('-'))
                ));
            }
            if let Some((_, flag, target)) = COMMAND_FLAGS.iter().find(|(name, _, _)| *name == var.name) {
                facts.push(format!("- Flag: [`{flag}`]({target})"));
            }
            if let Some((_, label, target)) = ENV_SEE.iter().find(|(name, _, _)| *name == var.name) {
                facts.push(format!("- See: [{label}]({target})"));
            }
            entries.push(entry(vec![
                format!("## `{}` {{#{id}}}", var.name.replace('{', "<").replace('}', ">")),
                prose(&var.summary),
                prose(&var.description),
                facts.join("\n"),
            ]));
        }
    }
    page(
        "reference/environment.md",
        "Environment",
        "Every variable OCX reads, sets or lists as external, public ones first, linking the root flag a variable stands in for; test seams are excluded. [ocx-mirror](https://github.com/ocx-sh/ocx-mirror/blob/main/docs/reference/environment.md) documents its own variables.",
        entries,
    )
}

/// The fixed text and lookup tables of one keys page; the keys themselves come from the model.
#[derive(Clone, Copy)]
struct KeysPage<'a> {
    path: &'a str,
    title: &'a str,
    intro: &'a str,
    prefix: &'a str,
    overrides: &'a [(&'a str, &'a str)],
    see: &'a [(&'a str, &'a str, &'a str)],
    defaults: &'a [(&'a str, &'a str)],
    variables: &'a [(&'a str, &'a str, &'a str, &'a str)],
}

fn keys_page(spec: &KeysPage, keys: &[KeyEntry], leading: Vec<String>) -> Page {
    let KeysPage {
        path,
        title,
        intro,
        prefix,
        overrides,
        see,
        defaults,
        variables,
    } = *spec;
    let mut keys = keys.iter().collect::<Vec<_>>();
    keys.sort_by(|a, b| a.key.cmp(&b.key));
    let mut ids = Ids::default();
    let entries = leading
        .into_iter()
        .chain(keys.into_iter().map(|key| {
            let id = ids.claim(&key_id(&key.key, prefix, overrides), &kebab(&key.key));
            let mut facts = Vec::new();
            if !key.type_name.is_empty() {
                facts.push(format!("- Type: `{}`", key.type_name));
            }
            if !key.values.is_empty() {
                facts.push(values_fact(&key.values));
            }
            if let Some((_, effective)) = defaults.iter().find(|(name, _)| *name == key.key) {
                facts.push(format!("- Default: {effective}"));
            } else if let Some(default) = &key.default {
                facts.push(match default.as_str() {
                    "null" => "- Default: unset".to_owned(),
                    other => format!("- Default: `{other}`"),
                });
            }
            if let Some(var) = &key.env {
                facts.push(format!("- Environment: `{var}`"));
            }
            for (_, relation, var, note) in variables.iter().filter(|(name, ..)| *name == key.key) {
                let note = if note.is_empty() {
                    String::new()
                } else {
                    format!(" ({note})")
                };
                facts.push(format!(
                    "- {relation}: [`{var}`](./environment.md#{}){note}",
                    env_slug(var)
                ));
            }
            let exact = see.iter().filter(|(prefix, ..)| *prefix == key.key).collect::<Vec<_>>();
            let rows = if exact.is_empty() {
                see.iter()
                    .find(|(prefix, ..)| covers(prefix, &key.key))
                    .into_iter()
                    .collect()
            } else {
                exact
            };
            if !rows.is_empty() {
                let links = rows
                    .iter()
                    .map(|(_, label, target)| format!("[{label}]({target})"))
                    .collect::<Vec<_>>();
                facts.push(format!("- See: {}", links.join(", ")));
            }
            entry(vec![
                format!("## `{}` {{#{id}}}", key.key),
                prose(&key.summary),
                facts.join("\n"),
            ])
        }))
        .collect();
    page(path, title, intro, entries)
}

/// Whether `key` is `prefix` or nested under it.
fn covers(prefix: &str, key: &str) -> bool {
    key == prefix || key.strip_prefix(prefix).is_some_and(|rest| rest.starts_with('.'))
}

/// The accepted values of a key, one line when none has a meaning and a nested list when any does.
fn values_fact(values: &[(String, String)]) -> String {
    if values.iter().all(|(_, meaning)| meaning.is_empty()) {
        let names = values.iter().map(|(value, _)| format!("`{value}`")).collect::<Vec<_>>();
        return format!("- Values: {}", names.join(", "));
    }
    let rows = values
        .iter()
        .map(|(value, meaning)| match meaning.is_empty() {
            true => format!("  - `{value}`"),
            false => format!("  - `{value}`: {}", escape(meaning)),
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("- Values:\n{rows}")
}

fn exit_page(codes: &[ExitCodeEntry]) -> Page {
    let retired = ocx_exit::RETIRED
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let mut codes = codes.iter().collect::<Vec<_>>();
    codes.sort_by_key(|code| code.code);
    let specific = codes
        .iter()
        .filter(|code| code.code > SYSEXITS_MAX)
        .map(|code| code.code);
    let range = match (specific.clone().min(), specific.max()) {
        (Some(first), Some(last)) => format!(" Codes {first} to {last} are specific to OCX."),
        _ => String::new(),
    };
    let entries = codes
        .into_iter()
        .map(|code| {
            let facts = if code.kind.is_empty() {
                String::new()
            } else {
                format!("- Error kind: `{}`", code.kind)
            };
            let slugs = if code.slugs.is_empty() {
                String::new()
            } else {
                let rows = code
                    .slugs
                    .iter()
                    .map(|slug| format!("| `{}` | {} |", slug.slug, cell(&slug.summary)))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("| Slug | Meaning |\n| --- | --- |\n{rows}")
            };
            entry(vec![
                format!("## `{}` {} {{#exit-{}}}", code.code, code.name, code.code),
                sentence(&code.summary),
                facts,
                slugs,
            ])
        })
        .collect();
    page(
        "reference/exit-codes.md",
        "Exit codes",
        &format!(
            "Codes follow BSD sysexits.h where one fits.{range}\n\nValues are stable across releases, so branching on `$?` is supported. Exit numbers {retired} are retired and never reused.\n\nUnder `--format json` a failure names its code's kind and its slug in the [error document](./machine-interface.md#machine-interface-error-document)."
        ),
        entries,
    )
}

/// The highest number BSD `sysexits.h` assigns.
const SYSEXITS_MAX: u8 = 78;

/// Ends the text with a full stop unless it already ends a sentence.
fn sentence(text: &str) -> String {
    let text = text.trim();
    match text.chars().last() {
        None => String::new(),
        Some('.' | '!' | '?' | ':') => text.to_owned(),
        Some(_) => format!("{text}."),
    }
}

/// Markdown-safe prose: a `<` outside a code span or a leading `#` would be read as markup.
fn prose(text: &str) -> String {
    let mut fenced = false;
    let mut lines = Vec::new();
    for line in text.trim().lines() {
        let line = line.trim_end();
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            lines.push(line.to_owned());
        } else if fenced || line.starts_with("    ") {
            lines.push(line.to_owned());
        } else {
            lines.push(escape(line));
        }
    }
    split_paragraphs(&lines.join("\n"))
}

/// Sentences a paragraph may hold on a reference page, as `test/lint/test_reference_shape.py` counts them.
const MAX_SENTENCES_PER_PARAGRAPH: usize = 2;

/// Starts a new paragraph after every second sentence of a run of prose lines; a shorter run is left as written.
fn split_paragraphs(text: &str) -> String {
    let mut out = Vec::new();
    let mut run: Vec<&str> = Vec::new();
    let mut fenced = false;
    for line in text.lines() {
        let opens_fence = line.trim_start().starts_with("```");
        if opens_fence {
            fenced = !fenced;
        }
        if opens_fence || fenced || is_not_prose(line) {
            flush_run(&mut run, &mut out);
            out.push(line.to_owned());
        } else {
            run.push(line);
        }
    }
    flush_run(&mut run, &mut out);
    out.join("\n")
}

fn flush_run(run: &mut Vec<&str>, out: &mut Vec<String>) {
    if run.is_empty() {
        return;
    }
    let joined = run.iter().map(|line| line.trim()).collect::<Vec<_>>().join(" ");
    let sentences = sentences(&joined);
    if sentences.len() <= MAX_SENTENCES_PER_PARAGRAPH {
        out.extend(run.iter().map(|line| (*line).to_owned()));
    } else {
        let paragraphs = sentences
            .chunks(MAX_SENTENCES_PER_PARAGRAPH)
            .map(|group| group.join(" "));
        out.push(paragraphs.collect::<Vec<_>>().join("\n\n"));
    }
    run.clear();
}

/// A blank line, table row, list item, heading or markup line: not part of a paragraph.
fn is_not_prose(line: &str) -> bool {
    let line = line.trim_start();
    let list_marker = line
        .strip_prefix(['-', '*', '+'])
        .is_some_and(|rest| rest.starts_with(char::is_whitespace));
    let digits = line.trim_start_matches(|ch: char| ch.is_ascii_digit());
    let numbered = digits.len() < line.len()
        && digits
            .strip_prefix(['.', ')'])
            .is_some_and(|rest| rest.starts_with(char::is_whitespace));
    line.is_empty() || line.starts_with(['|', '<', '#']) || line.starts_with(":::") || list_marker || numbered
}

/// The sentences of one paragraph. A terminator inside a code span or a link target, or ending `e.g.`, `i.e.`,
/// `etc.` or `vs.`, does not end one.
fn sentences(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let (mut found, mut start, mut index, mut code) = (Vec::new(), 0, 0, false);
    while index < bytes.len() {
        match bytes[index] {
            b'`' => code = !code,
            b']' if !code && text[index..].starts_with("](") => {
                index += text[index..].find(')').unwrap_or(0);
            }
            b'.' | b'!' | b'?' if !code => {
                while index + 1 < bytes.len() && matches!(bytes[index + 1], b'.' | b'!' | b'?') {
                    index += 1;
                }
                let ends_word = index + 1 == bytes.len() || bytes[index + 1].is_ascii_whitespace();
                if ends_word && !ends_abbreviation(&text[..=index]) {
                    found.push(text[start..=index].trim());
                    start = index + 1;
                }
            }
            _ => {}
        }
        index += 1;
    }
    let tail = text[start..].trim();
    if tail.chars().any(|ch| ch.is_alphanumeric() || ch == '_') {
        found.push(tail);
    }
    found
}

fn ends_abbreviation(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    ["e.g.", "i.e.", "etc.", "vs."].iter().any(|abbreviation| {
        lower
            .strip_suffix(abbreviation)
            .is_some_and(|before| !before.ends_with(|ch: char| ch.is_alphanumeric() || ch == '_'))
    })
}

fn escape(line: &str) -> String {
    let mut out = String::new();
    if line.starts_with('#') {
        out.push('\\');
    }
    let mut code = false;
    let mut rest = line;
    while let Some(ch) = rest.chars().next() {
        let width = ch.len_utf8();
        match ch {
            '`' => {
                code = !code;
                out.push(ch);
            }
            '<' if !code => match autolink(rest) {
                Some(link) => {
                    out.push_str(link);
                    rest = &rest[link.len()..];
                    continue;
                }
                None => out.push_str("\\<"),
            },
            _ => out.push(ch),
        }
        rest = &rest[width..];
    }
    out
}

/// The `<https://…>` at the start of `text`, which Markdown renders as a link and must not be escaped.
fn autolink(text: &str) -> Option<&str> {
    let end = text.find('>')?;
    let target = &text[1..end];
    let is_url = target.starts_with("https://") || target.starts_with("http://");
    (is_url && !target.contains(char::is_whitespace)).then(|| &text[..=end])
}

/// One table cell: whitespace collapsed to one line, markup and pipes escaped.
fn cell(text: &str) -> String {
    pipe_safe(&escape(&text.split_whitespace().collect::<Vec<_>>().join(" ")))
}

fn pipe_safe(text: &str) -> String {
    text.replace('|', "\\|")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_of_more_than_two_sentences_becomes_paragraphs_of_two() {
        let text = "One. Two. Three.\nFour. Five.";
        assert_eq!(split_paragraphs(text), "One. Two.\n\nThree. Four.\n\nFive.");
    }

    #[test]
    fn two_sentences_keep_their_line_breaks() {
        assert_eq!(split_paragraphs("One\nand two. Three."), "One\nand two. Three.");
    }

    #[test]
    fn code_spans_and_abbreviations_do_not_end_a_sentence() {
        let text = "Reads `a. B` (e.g. here) and more, i.e. now. Two. Three.";
        assert_eq!(
            split_paragraphs(text),
            "Reads `a. B` (e.g. here) and more, i.e. now. Two.\n\nThree."
        );
    }

    #[test]
    fn lists_fences_and_blank_lines_are_left_alone() {
        let text = "- One. Two. Three.\n\n```\nA. B. C.\n```";
        assert_eq!(split_paragraphs(text), text);
    }
}
