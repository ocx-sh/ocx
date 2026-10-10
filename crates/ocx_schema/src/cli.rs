// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `cli.json`: the `ocx` command grammar as a document, with the checks that hold it to the
//! per-command output table.
//!
//! clap keeps `requires`, `required_unless*` and `overrides_with` private, so those relations are
//! read off the parser itself: each is a parse of a minimal valid argument vector with one argument
//! added or removed.

use std::collections::{BTreeMap, BTreeSet};

use clap::error::{ContextKind, ContextValue, ErrorKind};
use clap::parser::ValueSource;
use clap::{Arg, ArgAction, Command};
use ocx::command::contract::{self, ValueType};
use ocx::command::deprecated::{self, RenamedCommand, RenamedFlag};
use ocx_env::{Change, EnvValue, EnvVar, OnInvalid, Release, Retired, Status, Visibility};
use schemars::JsonSchema;
use serde::Serialize;

pub use ocx::command::OutputMode;

/// In-band version of the `cli.json` document.
pub const CLI_SCHEMA_VERSION: u32 = cli_version!();

/// `$id` of the JSON Schema that describes `cli.json`.
pub const CLI_SCHEMA_ID: &str = schema_id!("cli", cli_version!());

/// The whole command-line surface: the command tree, the environment it reads, and retired spellings.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Cli {
    pub schema_version: u32,
    pub root: CommandSpec,
    pub env: Vec<EnvSpec>,
    pub retired: Vec<RetiredSpec>,
}

/// One command or command group.
#[derive(Debug, Serialize, JsonSchema)]
pub struct CommandSpec {
    /// Words below `ocx`; empty for the root.
    pub path: Vec<String>,
    /// Version of this command's output contract; 0 for a group.
    pub version: u32,
    pub summary: String,
    pub hidden: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<Deprecation>,
    pub external_subcommands: bool,
    /// What the command can write to stdout; empty only for a group.
    pub output: Vec<OutputMode>,
    pub args: Vec<ArgSpec>,
    pub groups: Vec<GroupSpec>,
    pub commands: Vec<CommandSpec>,
}

/// One flag, option or positional argument, listed on the command that declares it.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ArgSpec {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub long: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub short: Option<char>,
    /// 1-based index of a positional argument.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<usize>,
    pub value: ValueSpec,
    pub required: bool,
    /// Arguments or groups that waive this argument's requirement; without one of them it is required.
    pub required_unless: Vec<String>,
    pub num_args: Arity,
    /// Each occurrence adds its values to the earlier ones, so the argument may be given more than once.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub repeatable: bool,
    /// Accepted on every command below the declaring one.
    pub global: bool,
    pub hidden: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<Deprecation>,
    pub default: Vec<String>,
    /// Arguments or groups that must accompany this one.
    pub requires: Vec<String>,
    /// Arguments that may not accompany this one.
    pub conflicts: Vec<String>,
    /// Arguments this one replaces when both are given; the later one wins.
    pub overrides: Vec<String>,
    pub require_equals: bool,
    pub allow_hyphen_values: bool,
    /// Taken only after `--`.
    pub last: bool,
    pub trailing_var_arg: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_terminator: Option<String>,
    /// The secret value is read from stdin, never from the argument list.
    pub stdin_secret: bool,
    pub help: String,
    /// The environment variable that stands in for this flag.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
}

/// How many values an argument takes per occurrence; `max` absent means unbounded.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Arity {
    pub min: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<usize>,
}

/// A named set of arguments with a shared rule.
#[derive(Debug, Serialize, JsonSchema)]
pub struct GroupSpec {
    pub id: String,
    pub args: Vec<String>,
    /// One member must be given.
    pub required: bool,
    /// More than one member may be given.
    pub multiple: bool,
}

/// The grammar of an argument's value.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ValueSpec {
    /// A flag that takes no value.
    Switch,
    /// A flag that counts its occurrences.
    Count,
    String,
    Integer,
    Path,
    Choice {
        choices: Vec<Choice>,
    },
    /// A package identifier.
    Identifier,
    Platform,
    Digest,
}

/// One accepted value of a choice argument.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Choice {
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    pub hidden: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<Deprecation>,
}

/// One environment variable OCX reads.
#[derive(Debug, Serialize, JsonSchema)]
pub struct EnvSpec {
    pub name: String,
    /// The value grammar: `bool`, `tri_bool`, `string`, `integer`, `path`, `path_list`, `path_or_pem`,
    /// `host_list`, `choice` or `json`.
    pub value: String,
    /// The accepted values of a `choice` variable.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    /// `public`, `foreign` or `plumbing`.
    pub visibility: String,
    pub summary: String,
    /// The flag this variable stands in for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flag: Option<String>,
    pub secret: bool,
    /// What a value outside the grammar does: `default` falls back, `error` refuses.
    pub on_invalid: String,
}

/// An environment spelling OCX no longer reads under its own name.
#[derive(Debug, Serialize, JsonSchema)]
pub struct RetiredSpec {
    pub name: String,
    pub replacement: String,
    /// `rename`, `value_rename` or `polarity`.
    pub change: String,
    /// `window` while the old spelling still works, `removed` once it is refused.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removal: Option<String>,
}

/// A spelling that still works and is removed in a named release.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Deprecation {
    pub replacement: String,
    pub removal: String,
}

/// The tables the export joins onto the clap tree.
pub struct Sources<'a> {
    pub contract: &'a [(&'a [&'a str], u32, &'a [OutputMode])],
    pub env: Vec<&'a EnvVar>,
    pub retired: &'a [Retired],
    pub env_flags: &'a [(&'a EnvVar, &'a str)],
    pub renamed_commands: &'a [RenamedCommand],
    pub renamed_flags: &'a [RenamedFlag],
    pub removal: Release,
}

impl Sources<'static> {
    /// The tables of this build of `ocx`.
    pub fn ocx() -> Self {
        Self {
            contract: contract::CONTRACT,
            env: ocx_env::all().collect(),
            retired: ocx_env::RETIRED,
            env_flags: ocx::app::ENV_FLAGS,
            renamed_commands: deprecated::COMMANDS,
            renamed_flags: deprecated::FLAGS,
            removal: ocx_env::REMOVAL_RELEASE,
        }
    }
}

/// The `ocx` command tree.
pub fn ocx_command() -> Command {
    <ocx::app::Cli as clap::CommandFactory>::command()
}

/// `cli.json` for this build, pretty-printed.
pub fn cli_json() -> String {
    let cli = export(ocx_command(), &Sources::ocx());
    serde_json::to_string_pretty(&cli).expect("the export holds only strings, numbers and booleans")
}

/// The JSON Schema of `cli.json`, pretty-printed.
pub fn cli_schema() -> String {
    let mut settings = schemars::generate::SchemaSettings::draft2020_12().for_serialize();
    settings.meta_schema = Some("https://json-schema.org/draft/2020-12/schema".into());
    let root = settings.into_generator().into_root_schema_for::<Cli>();
    let mut document = serde_json::to_value(&root).expect("a schemars Schema is always serializable");
    if let Some(object) = document.as_object_mut() {
        object.insert("$id".to_owned(), serde_json::Value::String(CLI_SCHEMA_ID.to_owned()));
    }
    serde_json::to_string_pretty(&document).expect("a serde_json::Value is always serializable")
}

/// Export `command` as a [`Cli`].
///
/// # Panics
///
/// On an argument whose parser no [`ValueType`] row names, an `env_flags` flag the root does not
/// declare, and an argument the parser refuses every sample value for. Each names the argument.
pub fn export(mut command: Command, sources: &Sources) -> Cli {
    command.build();
    for (var, flag) in sources.env_flags {
        let declared = command
            .get_arguments()
            .any(|arg| arg.get_long().is_some_and(|long| format!("--{long}") == *flag));
        assert!(
            declared,
            "{} names {flag}, which the root command does not declare",
            var.name
        );
    }
    let root = command_spec(&command, &[], &BTreeSet::new(), sources);
    let env = sources
        .env
        .iter()
        .filter(|var| var.visibility != Visibility::Testing)
        .map(|var| env_spec(var, sources))
        .collect();
    let retired = sources.retired.iter().map(retired_spec).collect();
    Cli {
        schema_version: CLI_SCHEMA_VERSION,
        root,
        env,
        retired,
    }
}

fn command_spec(command: &Command, path: &[String], inherited: &BTreeSet<String>, sources: &Sources) -> CommandSpec {
    let own: Vec<&Arg> = command
        .get_arguments()
        .filter(|arg| !is_builtin(arg) && !inherited.contains(arg.get_id().as_str()))
        .collect();
    let mut globals = inherited.clone();
    globals.extend(
        own.iter()
            .filter(|arg| arg.is_global_set())
            .map(|arg| arg.get_id().to_string()),
    );

    let joined = path.join(" ");
    let relations = Relations::probe(command, &own, &joined);
    let groups = groups(command, &own, &relations);
    let args = own
        .iter()
        .map(|arg| arg_spec(command, arg, &relations, path.is_empty(), sources, &joined))
        .collect();

    let subcommands: Vec<&Command> = subcommands(command).collect();
    let (version, output) = match sources.contract.iter().find(|(entry, _, _)| *entry == path) {
        Some((_, version, output)) if subcommands.is_empty() => (*version, output.to_vec()),
        _ => (0, Vec::new()),
    };
    let deprecated = sources
        .renamed_commands
        .iter()
        .find(|row| row.old.path() == path)
        .map(|row| Deprecation {
            replacement: row.new.path().join(" "),
            removal: sources.removal.minor_form(),
        });

    CommandSpec {
        path: path.to_vec(),
        version,
        summary: command.get_about().map(ToString::to_string).unwrap_or_default(),
        hidden: command.is_hide_set(),
        deprecated,
        external_subcommands: command.is_allow_external_subcommands_set(),
        output,
        args,
        groups,
        commands: subcommands
            .into_iter()
            .map(|sub| {
                let mut child = path.to_vec();
                child.push(sub.get_name().to_owned());
                command_spec(sub, &child, &globals, sources)
            })
            .collect(),
    }
}

/// Subcommands, without the `help` subcommand clap adds on its own.
fn subcommands(command: &Command) -> impl Iterator<Item = &Command> {
    let generated_help = !command.is_disable_help_subcommand_set();
    command
        .get_subcommands()
        .filter(move |sub| !(generated_help && sub.get_name() == "help"))
}

fn is_builtin(arg: &Arg) -> bool {
    matches!(
        arg.get_action(),
        ArgAction::Help | ArgAction::HelpShort | ArgAction::HelpLong | ArgAction::Version
    )
}

fn arg_spec(command: &Command, arg: &Arg, relations: &Relations, root: bool, sources: &Sources, path: &str) -> ArgSpec {
    let id = arg.get_id().to_string();
    let long = arg.get_long().map(str::to_owned);
    let range = arg.get_num_args().unwrap_or_default();
    // ponytail: the stdin-secret flags are the `--*-stdin` spellings; a flag-level marker replaces
    // this when one is not a secret.
    let stdin_secret = long.as_deref().is_some_and(|long| long.ends_with("-stdin"));
    let env = root
        .then(|| {
            sources
                .env_flags
                .iter()
                .find(|(_, flag)| long.as_deref().is_some_and(|long| format!("--{long}") == *flag))
                .map(|(var, _)| var.name.to_owned())
        })
        .flatten();
    ArgSpec {
        long,
        short: arg.get_short(),
        position: arg.get_index(),
        value: value_spec(arg, path),
        required: arg.is_required_set(),
        required_unless: relations.required_unless.get(&id).cloned().unwrap_or_default(),
        num_args: Arity {
            min: range.min_values(),
            max: (range.max_values() != usize::MAX).then_some(range.max_values()),
        },
        repeatable: matches!(arg.get_action(), ArgAction::Append),
        global: arg.is_global_set(),
        hidden: arg.is_hide_set(),
        deprecated: deprecation(arg, path, sources),
        default: arg
            .get_default_values()
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect(),
        requires: relations.requires.get(&id).cloned().unwrap_or_default(),
        conflicts: conflicts(command, arg),
        overrides: relations.overrides.get(&id).cloned().unwrap_or_default(),
        require_equals: arg.is_require_equals_set(),
        allow_hyphen_values: arg.is_allow_hyphen_values_set(),
        last: arg.is_last_set(),
        trailing_var_arg: arg.is_trailing_var_arg_set(),
        value_terminator: arg.get_value_terminator().map(ToString::to_string),
        stdin_secret,
        help: arg
            .get_help()
            .or_else(|| arg.get_long_help())
            .map(ToString::to_string)
            .unwrap_or_default(),
        env,
        id,
    }
}

/// The window `arg`'s spelling is in, from the flag rows of `sources.renamed_flags`.
fn deprecation(arg: &Arg, path: &str, sources: &Sources) -> Option<Deprecation> {
    // A long name wins over a short one, as it does when the argument is typed.
    let (long, short) = (arg.get_long(), arg.get_short().filter(|_| arg.get_long().is_none()));
    sources
        .renamed_flags
        .iter()
        .find(|row| row.path.path().join(" ") == path && row.old.is_spelled(long, short))
        .map(|row| Deprecation {
            replacement: row.new_text(),
            removal: sources.removal.minor_form(),
        })
}

fn value_spec(arg: &Arg, path: &str) -> ValueSpec {
    match arg.get_action() {
        ArgAction::SetTrue | ArgAction::SetFalse => return ValueSpec::Switch,
        ArgAction::Count => return ValueSpec::Count,
        _ => {}
    }
    let choices = arg.get_possible_values();
    if !choices.is_empty() {
        return ValueSpec::Choice {
            choices: choices
                .iter()
                .map(|choice| Choice {
                    value: choice.get_name().to_owned(),
                    help: choice.get_help().map(ToString::to_string),
                    hidden: choice.is_hide_set(),
                    deprecated: None,
                })
                .collect(),
        };
    }
    let parser = arg.get_value_parser();
    match contract::value_type(parser) {
        Some(ValueType::String) => ValueSpec::String,
        Some(ValueType::Integer) => ValueSpec::Integer,
        Some(ValueType::Path) => ValueSpec::Path,
        Some(ValueType::Identifier) => ValueSpec::Identifier,
        Some(ValueType::Platform) => ValueSpec::Platform,
        Some(ValueType::Digest) => ValueSpec::Digest,
        None => panic!(
            "`ocx {path}` argument `{}`: no value type for parser {:?}; add its type to the table in `ocx::command::contract`",
            arg.get_id(),
            parser.type_id()
        ),
    }
}

/// Every argument `arg` may not appear with, in both directions, including group exclusivity.
fn conflicts(command: &Command, arg: &Arg) -> Vec<String> {
    let id = arg.get_id().as_str();
    let mut out = BTreeSet::new();
    for other in command.get_arguments().filter(|other| !is_builtin(other)) {
        let other_id = other.get_id().as_str();
        if other_id == id {
            continue;
        }
        let forward = command
            .get_arg_conflicts_with(arg)
            .iter()
            .any(|conflict| conflict.get_id() == other_id);
        let backward = command
            .get_arg_conflicts_with(other)
            .iter()
            .any(|conflict| conflict.get_id() == id);
        if forward || backward {
            out.insert(other_id.to_owned());
        }
    }
    for group in command.get_groups() {
        if group.clone().is_multiple() {
            continue;
        }
        let members: Vec<&str> = group.get_args().map(|member| member.as_str()).collect();
        if members.contains(&id) {
            out.extend(members.into_iter().filter(|member| *member != id).map(str::to_owned));
        }
    }
    out.into_iter().collect()
}

/// Groups a reader needs: required, exclusive, or named by a `requires` relation.
fn groups(command: &Command, own: &[&Arg], relations: &Relations) -> Vec<GroupSpec> {
    let referenced: BTreeSet<&str> = relations.requires.values().flatten().map(String::as_str).collect();
    command
        .get_groups()
        .filter(|group| {
            group
                .get_args()
                .any(|member| own.iter().any(|arg| arg.get_id() == member))
        })
        .filter_map(|group| {
            let multiple = group.clone().is_multiple();
            let id = group.get_id().as_str();
            (group.is_required_set() || !multiple || referenced.contains(id)).then(|| GroupSpec {
                id: id.to_owned(),
                args: group.get_args().map(ToString::to_string).collect(),
                required: group.is_required_set(),
                multiple,
            })
        })
        .collect()
}

fn env_spec(var: &EnvVar, sources: &Sources) -> EnvSpec {
    let value = match var.value {
        EnvValue::Bool => "bool",
        EnvValue::TriBool => "tri_bool",
        EnvValue::String => "string",
        EnvValue::Integer => "integer",
        EnvValue::Path => "path",
        EnvValue::PathList => "path_list",
        EnvValue::PathOrPem => "path_or_pem",
        EnvValue::HostList => "host_list",
        EnvValue::Choice(_) => "choice",
        EnvValue::Json => "json",
    };
    let visibility = match var.visibility {
        Visibility::Public => "public",
        Visibility::Foreign => "foreign",
        Visibility::Plumbing => "plumbing",
        Visibility::Testing => "testing",
    };
    EnvSpec {
        name: var.name.to_owned(),
        value: value.to_owned(),
        choices: match var.value {
            EnvValue::Choice(choices) => choices.iter().map(|choice| (*choice).to_owned()).collect(),
            _ => Vec::new(),
        },
        visibility: visibility.to_owned(),
        summary: var.doc.lines().next().unwrap_or_default().to_owned(),
        flag: sources
            .env_flags
            .iter()
            .find(|(flagged, _)| std::ptr::eq(*flagged, var))
            .map(|(_, flag)| (*flag).to_owned()),
        secret: var.secret,
        on_invalid: match var.on_invalid {
            OnInvalid::Default => "default",
            OnInvalid::Error => "error",
        }
        .to_owned(),
    }
}

fn retired_spec(retired: &Retired) -> RetiredSpec {
    let (status, removal) = match retired.status {
        Status::Window { removal } => ("window", Some(removal.to_string())),
        Status::Removed => ("removed", None),
    };
    RetiredSpec {
        name: retired.name.to_owned(),
        replacement: retired.replacement.name.to_owned(),
        change: match retired.change {
            Change::Rename => "rename",
            Change::ValueRename { .. } => "value_rename",
            Change::Polarity => "polarity",
        }
        .to_owned(),
        status: status.to_owned(),
        removal,
    }
}

// ── Probing ─────────────────────────────────────────────────────────────────

/// The relations clap does not expose, keyed by argument id, each list sorted.
#[derive(Default)]
struct Relations {
    requires: BTreeMap<String, Vec<String>>,
    required_unless: BTreeMap<String, Vec<String>>,
    overrides: BTreeMap<String, Vec<String>>,
}

/// What clap made of one argument vector.
enum Outcome {
    Parsed,
    /// Refused for this many missing required arguments or groups.
    Missing(usize),
    Refused,
}

impl Relations {
    fn probe(command: &Command, own: &[&Arg], path: &str) -> Self {
        let mut prober = Prober::new(command, own, path);
        let Some(base) = prober.base() else {
            return Self::default();
        };
        let mut relations = Self::default();
        for (index, arg) in own.iter().enumerate() {
            if !arg.is_required_set() {
                let waivers = prober.waivers(&base, index, command);
                if !waivers.is_empty() {
                    relations.required_unless.insert(arg.get_id().to_string(), waivers);
                }
            }
            if base.contains(&index) {
                continue;
            }
            let mut with = base.clone();
            with.insert(index);
            match prober.parse(&with) {
                Outcome::Missing(missing) => {
                    let requires = prober.satisfying(&with, missing, command);
                    if !requires.is_empty() {
                        relations.requires.insert(arg.get_id().to_string(), requires);
                    }
                }
                Outcome::Parsed => {
                    for (other_index, other) in own.iter().enumerate() {
                        if other_index == index || base.contains(&other_index) {
                            continue;
                        }
                        if prober.overrides(&base, other_index, index) {
                            insert_sorted(&mut relations.overrides, arg, other);
                            insert_sorted(&mut relations.overrides, other, arg);
                        }
                    }
                }
                Outcome::Refused => {}
            }
        }
        relations
    }
}

/// `found`, sorted, with every group whose members all appear in it replaced by the group's id.
fn fold_groups(mut found: BTreeSet<String>, command: &Command) -> Vec<String> {
    for group in command.get_groups() {
        let members: BTreeSet<String> = group.get_args().map(ToString::to_string).collect();
        if members.len() > 1 && members.is_subset(&found) {
            found.retain(|id| !members.contains(id));
            found.insert(group.get_id().to_string());
        }
    }
    found.into_iter().collect()
}

fn insert_sorted(map: &mut BTreeMap<String, Vec<String>>, arg: &Arg, other: &Arg) {
    let list = map.entry(arg.get_id().to_string()).or_default();
    let id = other.get_id().to_string();
    if let Err(at) = list.binary_search(&id) {
        list.insert(at, id);
    }
}

/// Builds and parses argument vectors over one command's own arguments, addressed by index.
struct Prober<'a> {
    command: Command,
    own: &'a [&'a Arg],
    samples: Vec<Vec<String>>,
}

/// Values tried in order until the argument's parser accepts one.
const SAMPLE_VALUES: &[&str] = &[
    "x",
    "example.com/pkg:1.0",
    "linux/amd64",
    "1",
    "1.0.0",
    "1s",
    "a=b",
    "ns/project",
    "cyclonedx",
    "sha256:0000000000000000000000000000000000000000000000000000000000000000",
    "1.0.0-canary",
    "example.com/pkg:1.0@sha256:0000000000000000000000000000000000000000000000000000000000000000",
];

impl<'a> Prober<'a> {
    fn new(command: &Command, own: &'a [&'a Arg], path: &str) -> Self {
        let mut prober = Self {
            command: command.clone(),
            own,
            samples: vec![Vec::new(); own.len()],
        };
        for index in 0..own.len() {
            prober.samples[index] = prober.sample(index, path);
        }
        prober
    }

    /// The values `own[index]` carries in a probe: the fewest it takes, each one its parser accepts.
    fn sample(&mut self, index: usize, path: &str) -> Vec<String> {
        let arg = self.own[index];
        let count = arg.get_num_args().unwrap_or_default().min_values();
        if count == 0 {
            return Vec::new();
        }
        let candidates: Vec<String> = match arg.get_possible_values().iter().find(|choice| !choice.is_hide_set()) {
            Some(choice) => vec![choice.get_name().to_owned()],
            None => SAMPLE_VALUES.iter().map(|value| (*value).to_owned()).collect(),
        };
        for candidate in candidates {
            self.samples[index] = vec![candidate.clone(); count];
            let argv = self.argv(&BTreeSet::from([index]));
            let refused = matches!(
                self.command
                    .try_get_matches_from_mut(argv)
                    .map_err(|error| error.kind()),
                Err(ErrorKind::ValueValidation | ErrorKind::InvalidValue)
            );
            if !refused {
                return vec![candidate; count];
            }
        }
        panic!(
            "`ocx {path}` argument `{}` refuses every sample value; add one it accepts to SAMPLE_VALUES",
            arg.get_id()
        )
    }

    /// `set` plus every positional below its highest one, which [`Self::argv`] must fill in.
    fn filled(&self, set: &BTreeSet<usize>) -> BTreeSet<usize> {
        let mut set = set.clone();
        let highest = set.iter().filter_map(|index| self.own[*index].get_index()).max();
        if let Some(highest) = highest {
            for (index, arg) in self.own.iter().enumerate() {
                if arg.get_index().is_some_and(|position| position < highest) && !arg.is_last_set() {
                    set.insert(index);
                }
            }
        }
        set
    }

    /// The argument vector for a set of own arguments, lower positionals filled in.
    fn argv(&self, set: &BTreeSet<usize>) -> Vec<String> {
        let set = self.filled(set);
        let mut positionals: Vec<usize> = set
            .iter()
            .copied()
            .filter(|i| self.own[*i].get_index().is_some())
            .collect();
        positionals.sort_by_key(|i| self.own[*i].get_index());

        let mut flags = Vec::new();
        for index in set.iter().filter(|i| self.own[**i].get_index().is_none()) {
            let arg = self.own[*index];
            let flag = match (arg.get_long(), arg.get_short()) {
                (Some(long), _) => format!("--{long}"),
                (None, Some(short)) => format!("-{short}"),
                (None, None) => continue,
            };
            match self.samples[*index].as_slice() {
                [] => flags.push(flag),
                [one] => flags.push(format!("{flag}={one}")),
                many => {
                    flags.push(flag);
                    flags.extend(many.iter().cloned());
                }
            }
        }
        let leading: Vec<usize> = positionals.iter().copied().filter(|i| !self.trails(*i)).collect();
        // A `--` terminator ends option parsing, so a flag after it would be a value of the next positional.
        let flags_first = leading.iter().any(|i| {
            self.own[*i]
                .get_value_terminator()
                .is_some_and(|terminator| terminator == "--")
        });

        let mut argv = vec![self.command.get_name().to_owned()];
        if flags_first {
            argv.append(&mut flags);
        }
        for index in &leading {
            argv.extend(self.samples[*index].iter().cloned());
            argv.extend(self.own[*index].get_value_terminator().map(ToString::to_string));
        }
        argv.append(&mut flags);
        for index in positionals.iter().filter(|i| self.trails(**i)) {
            if self.own[*index].is_last_set() {
                argv.push("--".to_owned());
            }
            argv.extend(self.samples[*index].iter().cloned());
        }
        argv
    }

    /// Whether a positional goes after every option: a `last`, trailing or hyphen-accepting
    /// variadic one swallows what follows it.
    fn trails(&self, index: usize) -> bool {
        let arg = self.own[index];
        let greedy = arg.is_allow_hyphen_values_set()
            && arg.get_num_args().is_some_and(|range| range.max_values() == usize::MAX);
        arg.is_last_set() || arg.is_trailing_var_arg_set() || greedy
    }

    fn parse(&mut self, set: &BTreeSet<usize>) -> Outcome {
        let argv = self.argv(set);
        match self.command.try_get_matches_from_mut(argv) {
            Ok(_) => Outcome::Parsed,
            Err(error) if error.kind() == ErrorKind::MissingRequiredArgument => {
                let missing = match error.get(ContextKind::InvalidArg) {
                    Some(ContextValue::Strings(missing)) => missing.len(),
                    _ => 1,
                };
                Outcome::Missing(missing)
            }
            Err(_) => Outcome::Refused,
        }
    }

    /// The smallest argument set the command parses: required arguments, a member of each required
    /// group, then whatever else lowers the number of missing requirements. `None` when the command
    /// cannot run with its own arguments alone, such as a group that needs a subcommand.
    fn base(&mut self) -> Option<BTreeSet<usize>> {
        let mut set: BTreeSet<usize> = (0..self.own.len()).filter(|i| self.own[*i].is_required_set()).collect();
        let required_groups: Vec<Vec<String>> = self
            .command
            .get_groups()
            .filter(|group| group.is_required_set())
            .map(|group| group.get_args().map(ToString::to_string).collect())
            .collect();
        for members in required_groups {
            if let Some(index) = self
                .own
                .iter()
                .position(|arg| members.iter().any(|m| arg.get_id() == m.as_str()))
            {
                set.insert(index);
            }
        }
        loop {
            let missing = match self.parse(&set) {
                Outcome::Parsed => return Some(set),
                Outcome::Missing(missing) => missing,
                Outcome::Refused => return None,
            };
            let next = (0..self.own.len()).filter(|i| !set.contains(i)).find(|i| {
                let mut with = set.clone();
                with.insert(*i);
                match self.parse(&with) {
                    Outcome::Parsed => true,
                    Outcome::Missing(now) => now < missing,
                    Outcome::Refused => false,
                }
            })?;
            set.insert(next);
        }
    }

    /// Own arguments whose addition to `with` lowers its `missing` count, folded into a group id when
    /// every member of one group qualifies.
    fn satisfying(&mut self, with: &BTreeSet<usize>, missing: usize, command: &Command) -> Vec<String> {
        let mut found = BTreeSet::new();
        for index in 0..self.own.len() {
            if with.contains(&index) {
                continue;
            }
            let mut more = with.clone();
            more.insert(index);
            let satisfies = match self.parse(&more) {
                Outcome::Parsed => true,
                Outcome::Missing(now) => now < missing,
                Outcome::Refused => false,
            };
            if satisfies {
                found.insert(self.own[index].get_id().to_string());
            }
        }
        fold_groups(found, command)
    }

    /// The own arguments any one of which waives `own[index]`'s conditional requirement, folded into a
    /// group id like [`Self::satisfying`]; empty when nothing ever demands it.
    ///
    /// clap reports a missing argument only as rendered usage text, so the probe renames the absent
    /// argument to a marker and reads the marker back out of the error.
    fn waivers(&mut self, base: &BTreeSet<usize>, index: usize, command: &Command) -> Vec<String> {
        const MARKER: &str = "ocx-required-probe";
        let id = self.own[index].get_id().clone();
        // In place, never `mut_arg`: that reorders the arguments under the built key map.
        let mut marked = self.command.clone().mut_args(|arg| match arg.get_id() == &id {
            false => arg,
            true if arg.get_index().is_some() => arg.value_names([MARKER]),
            true => arg.long(MARKER),
        });
        let mut demands = |set: &BTreeSet<usize>| -> Option<bool> {
            if self.filled(set).contains(&index) {
                return None;
            }
            match marked.try_get_matches_from_mut(self.argv(set)) {
                Ok(_) => Some(false),
                Err(error) if error.kind() == ErrorKind::MissingRequiredArgument => {
                    let named = match error.get(ContextKind::InvalidArg) {
                        Some(ContextValue::Strings(missing)) => missing.iter().any(|text| text.contains(MARKER)),
                        _ => false,
                    };
                    Some(named)
                }
                Err(_) => None,
            }
        };
        let candidates: Vec<BTreeSet<usize>> = if base.contains(&index) {
            vec![base.iter().copied().filter(|i| *i != index).collect()]
        } else {
            base.iter()
                .filter(|removed| !self.own[**removed].is_required_set())
                .map(|removed| base.iter().copied().filter(|i| i != removed).collect())
                .collect()
        };
        let Some(without) = candidates.into_iter().find(|set| demands(set) == Some(true)) else {
            return Vec::new();
        };
        // A positional sampled with no values puts no token of its own on the line, only its terminator.
        let silent = |i: usize| self.own[i].get_index().is_some() && self.samples[i].is_empty();
        let candidates: Vec<usize> = (0..self.own.len())
            .filter(|i| *i != index && !without.contains(i) && !silent(*i))
            .collect();
        let mut found = BTreeSet::new();
        for other in candidates {
            let mut with = without.clone();
            with.insert(other);
            if demands(&with) == Some(false) {
                found.insert(self.own[other].get_id().to_string());
            }
        }
        fold_groups(found, command)
    }

    /// Whether `later`, given after `earlier`, drops `earlier` from the parse.
    fn overrides(&mut self, base: &BTreeSet<usize>, earlier: usize, later: usize) -> bool {
        let earlier_id = self.own[earlier].get_id().to_string();
        let later_id = self.own[later].get_id().to_string();
        let mut set = base.clone();
        set.insert(earlier);
        set.insert(later);
        let mut argv = self.argv(&set);
        // Both are options here; put `earlier` first by moving `later`'s tokens to the end.
        let Some(later_token) = self.option_token(later) else {
            return false;
        };
        if self.option_token(earlier).is_none() {
            return false;
        }
        argv.retain(|token| token != &later_token);
        argv.push(later_token);
        match self.command.try_get_matches_from_mut(argv) {
            Ok(matches) => {
                matches.value_source(&earlier_id) != Some(ValueSource::CommandLine)
                    && matches.value_source(&later_id) == Some(ValueSource::CommandLine)
            }
            Err(_) => false,
        }
    }

    /// The single token an option occupies in [`Self::argv`], or `None` for a positional or a
    /// multi-value option.
    fn option_token(&self, index: usize) -> Option<String> {
        let arg = self.own[index];
        if arg.get_index().is_some() {
            return None;
        }
        let flag = match (arg.get_long(), arg.get_short()) {
            (Some(long), _) => format!("--{long}"),
            (None, Some(short)) => format!("-{short}"),
            (None, None) => return None,
        };
        match self.samples[index].as_slice() {
            [] => Some(flag),
            [one] => Some(format!("{flag}={one}")),
            _ => None,
        }
    }
}

// ── Output contract ─────────────────────────────────────────────────────────

/// Every way `contract` disagrees with `command` and the published report `roots`.
///
/// A visible leaf without an entry, an entry naming no leaf, a report root the registry lacks, and
/// a registry root no entry produces.
pub fn contract_findings(
    command: &Command,
    contract: &[(&[&str], u32, &[OutputMode])],
    roots: &[String],
) -> Vec<String> {
    let mut leaves = Vec::new();
    collect_leaves(command, &mut Vec::new(), false, &mut leaves);
    let mut findings = Vec::new();
    for (path, hidden) in &leaves {
        if !hidden && !contract.iter().any(|(entry, _, _)| entry == path) {
            findings.push(format!("`ocx {}` has no CONTRACT entry", path.join(" ")));
        }
    }
    let mut produced = BTreeSet::new();
    for (entry, _, modes) in contract {
        let path: Vec<String> = entry.iter().map(|word| (*word).to_owned()).collect();
        if !leaves.iter().any(|(leaf, _)| *leaf == path) {
            findings.push(format!("CONTRACT entry `ocx {}` names no command", entry.join(" ")));
        }
        for mode in *modes {
            if let OutputMode::Report { root } | OutputMode::ReportThenFail { root } = mode {
                let root = root.as_str();
                produced.insert(root);
                if !roots.iter().any(|published| published == root) {
                    findings.push(format!(
                        "`ocx {}` reports `{root}`, which the reports registry does not publish",
                        entry.join(" ")
                    ));
                }
            }
        }
    }
    for root in roots {
        if !produced.contains(root.as_str()) {
            findings.push(format!("registered root `{root}` is reported by no CONTRACT entry"));
        }
    }
    findings
}

fn collect_leaves(command: &Command, path: &mut Vec<String>, hidden: bool, out: &mut Vec<(Vec<String>, bool)>) {
    let hidden = hidden || command.is_hide_set();
    let mut any = false;
    for sub in subcommands(command) {
        any = true;
        path.push(sub.get_name().to_owned());
        collect_leaves(sub, path, hidden, out);
        path.pop();
    }
    if !any && !path.is_empty() {
        out.push((path.clone(), hidden));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Arg;
    use ocx_env::{Child, Reader};

    fn no_sources() -> Sources<'static> {
        Sources {
            contract: &[],
            env: Vec::new(),
            retired: &[],
            env_flags: &[],
            renamed_commands: &[],
            renamed_flags: &[],
            removal: ocx_env::REMOVAL_RELEASE,
        }
    }

    fn json(command: Command, sources: &Sources) -> String {
        serde_json::to_string_pretty(&export(command, sources)).expect("serializable")
    }

    fn arg_of<'a>(cli: &'a Cli, id: &str) -> &'a ArgSpec {
        cli.root
            .args
            .iter()
            .find(|arg| arg.id == id)
            .unwrap_or_else(|| panic!("no argument {id}"))
    }

    fn two_options() -> Command {
        Command::new("t")
            .arg(Arg::new("alpha").long("alpha"))
            .arg(Arg::new("beta").long("beta"))
    }

    fn parse(command: &Command, argv: &[&str]) -> Result<clap::ArgMatches, ErrorKind> {
        command.clone().try_get_matches_from(argv).map_err(|error| error.kind())
    }

    // ── Arg coverage ──────────────────────────────────────────────────────────

    fn declared_args(command: &Command, path: &str, out: &mut BTreeSet<String>) {
        for arg in command.get_arguments() {
            out.insert(format!("{path}|{}", arg.get_id()));
        }
        for sub in command.get_subcommands() {
            declared_args(sub, &format!("{path} {}", sub.get_name()), out);
        }
    }

    fn exported_args(spec: &CommandSpec, out: &mut BTreeSet<String>) {
        let path = std::iter::once("ocx".to_owned())
            .chain(spec.path.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ");
        for arg in &spec.args {
            out.insert(format!("{path}|{}", arg.id));
        }
        for sub in &spec.commands {
            exported_args(sub, out);
        }
    }

    /// The export lists exactly the arguments the help-text walk sees: each once, on the command
    /// that declares it, without the help and version flags clap adds.
    #[test]
    fn export_lists_every_declared_argument_once() {
        let mut declared = BTreeSet::new();
        declared_args(&ocx_command(), "ocx", &mut declared);
        let cli = export(ocx_command(), &Sources::ocx());
        let mut exported = BTreeSet::new();
        exported_args(&cli.root, &mut exported);
        assert!(declared.len() > 100, "the walk read only {} arguments", declared.len());
        assert_eq!(
            exported.symmetric_difference(&declared).collect::<Vec<_>>(),
            Vec::<&String>::new(),
            "exported and declared argument sets differ"
        );
    }

    // ── Probe argv ──────────────────────────────────────────────────────────

    fn probe_argv(command: &Command) -> Vec<String> {
        let own: Vec<&Arg> = command.get_arguments().collect();
        let all = (0..own.len()).collect();
        Prober::new(command, &own, "t").argv(&all)
    }

    /// A `--`-terminated positional ends option parsing, so the flag must come before it; without a terminator the
    /// flag follows the positional.
    #[test]
    fn a_terminated_positional_is_probed_after_the_flags() {
        let flag = || Arg::new("flag").long("flag").num_args(0);
        let terminated = Command::new("t")
            .arg(Arg::new("words").index(1).num_args(1..).value_terminator("--"))
            .arg(flag());
        assert_eq!(probe_argv(&terminated), ["t", "--flag", "x", "--"]);

        let plain = Command::new("t")
            .arg(Arg::new("words").index(1).num_args(1..))
            .arg(flag());
        assert_eq!(probe_argv(&plain), ["t", "x", "--flag"]);
    }

    // ── Relation mutations ──────────────────────────────────────────────────

    #[test]
    fn a_flag_rename_changes_the_export() {
        let before = json(two_options(), &no_sources());
        let renamed = Command::new("t")
            .arg(Arg::new("alpha").long("alpha-renamed"))
            .arg(Arg::new("beta").long("beta"));
        assert_ne!(before, json(renamed, &no_sources()));
    }

    #[test]
    fn requires_is_exported_and_clap_agrees() {
        let command = Command::new("t")
            .arg(Arg::new("alpha").long("alpha").requires("beta"))
            .arg(Arg::new("beta").long("beta"));
        let cli = export(command.clone(), &no_sources());
        assert_eq!(arg_of(&cli, "alpha").requires, vec!["beta"]);
        assert!(arg_of(&cli, "beta").requires.is_empty());
        assert_ne!(json(two_options(), &no_sources()), json(command.clone(), &no_sources()));
        assert_eq!(
            parse(&command, &["t", "--alpha=x"]).err(),
            Some(ErrorKind::MissingRequiredArgument)
        );
        assert!(parse(&command, &["t", "--alpha=x", "--beta=x"]).is_ok());
    }

    #[test]
    fn requiring_a_group_names_the_group() {
        let command = Command::new("t")
            .arg(Arg::new("alpha").long("alpha").requires("source"))
            .arg(Arg::new("beta").long("beta"))
            .arg(Arg::new("gamma").long("gamma"))
            .group(clap::ArgGroup::new("source").args(["beta", "gamma"]));
        let cli = export(command, &no_sources());
        assert_eq!(arg_of(&cli, "alpha").requires, vec!["source"]);
        assert!(
            cli.root
                .groups
                .iter()
                .any(|group| group.id == "source" && !group.multiple)
        );
        assert_eq!(
            arg_of(&cli, "beta").conflicts,
            vec!["gamma"],
            "members of a non-multiple group exclude each other"
        );
    }

    #[test]
    fn conflicts_are_exported_both_ways_and_clap_agrees() {
        let command = Command::new("t")
            .arg(Arg::new("alpha").long("alpha").conflicts_with("beta"))
            .arg(Arg::new("beta").long("beta"));
        let cli = export(command.clone(), &no_sources());
        assert_eq!(arg_of(&cli, "alpha").conflicts, vec!["beta"]);
        assert_eq!(arg_of(&cli, "beta").conflicts, vec!["alpha"]);
        assert_eq!(
            parse(&command, &["t", "--alpha=x", "--beta=x"]).err(),
            Some(ErrorKind::ArgumentConflict)
        );
        assert!(parse(&command, &["t", "--beta=x"]).is_ok());
    }

    #[test]
    fn overrides_are_exported_both_ways_and_clap_agrees() {
        let command = Command::new("t")
            .arg(
                Arg::new("alpha")
                    .long("alpha")
                    .action(ArgAction::SetTrue)
                    .overrides_with("beta"),
            )
            .arg(Arg::new("beta").long("beta").action(ArgAction::SetTrue));
        let cli = export(command.clone(), &no_sources());
        assert_eq!(arg_of(&cli, "alpha").overrides, vec!["beta"]);
        assert_eq!(arg_of(&cli, "beta").overrides, vec!["alpha"]);
        assert!(arg_of(&cli, "alpha").conflicts.is_empty());
        let matches = parse(&command, &["t", "--alpha", "--beta"]).expect("an override is not a conflict");
        assert!(!matches.get_flag("alpha"));
    }

    #[test]
    fn required_unless_is_exported_and_clap_agrees() {
        let command = Command::new("t")
            .arg(Arg::new("alpha").long("alpha").required_unless_present("beta"))
            .arg(Arg::new("beta").long("beta"));
        let cli = export(command.clone(), &no_sources());
        assert!(!arg_of(&cli, "alpha").required);
        assert_eq!(arg_of(&cli, "alpha").required_unless, vec!["beta"]);
        assert!(arg_of(&cli, "beta").required_unless.is_empty());
        assert_ne!(json(two_options(), &no_sources()), json(command.clone(), &no_sources()));
        assert_eq!(parse(&command, &["t"]).err(), Some(ErrorKind::MissingRequiredArgument));
        assert!(parse(&command, &["t", "--beta=x"]).is_ok());
    }

    /// A positional waived by any of several flags lists every waiver, and only on itself.
    #[test]
    fn required_unless_any_on_a_positional_lists_every_waiver() {
        let command = Command::new("t")
            .arg(
                Arg::new("target")
                    .required_unless_present_any(["gamma", "alpha"])
                    .value_name("TARGET"),
            )
            .arg(Arg::new("alpha").long("alpha").action(ArgAction::SetTrue))
            .arg(Arg::new("beta").long("beta"))
            .arg(Arg::new("gamma").long("gamma").action(ArgAction::SetTrue));
        let cli = export(command.clone(), &no_sources());
        assert_eq!(arg_of(&cli, "target").required_unless, vec!["alpha", "gamma"]);
        for waiver in ["alpha", "beta", "gamma"] {
            assert!(arg_of(&cli, waiver).required_unless.is_empty(), "{waiver}");
        }
        assert_eq!(
            parse(&command, &["t", "--beta=x"]).err(),
            Some(ErrorKind::MissingRequiredArgument)
        );
        assert!(parse(&command, &["t", "--gamma"]).is_ok());
    }

    #[test]
    fn num_args_is_exported_and_clap_agrees() {
        let command = Command::new("t").arg(Arg::new("alpha").long("alpha").num_args(2));
        let cli = export(command.clone(), &no_sources());
        assert_eq!(
            (arg_of(&cli, "alpha").num_args.min, arg_of(&cli, "alpha").num_args.max),
            (2, Some(2))
        );
        assert_ne!(json(two_options(), &no_sources()), json(command.clone(), &no_sources()));
        assert!(parse(&command, &["t", "--alpha", "x", "y"]).is_ok());
        assert_eq!(
            parse(&command, &["t", "--alpha", "x"]).err(),
            Some(ErrorKind::WrongNumberOfValues)
        );
    }

    #[test]
    fn repeatable_is_exported_and_clap_agrees() {
        let command = Command::new("t").arg(Arg::new("alpha").long("alpha").action(ArgAction::Append));
        let cli = export(command.clone(), &no_sources());
        assert!(arg_of(&cli, "alpha").repeatable);
        assert!(!json(two_options(), &no_sources()).contains("repeatable"));
        let matches = parse(&command, &["t", "--alpha", "x", "--alpha", "y"]).expect("repeats parse");
        assert_eq!(matches.get_many::<String>("alpha").expect("values").count(), 2);
        assert_eq!(
            parse(&two_options(), &["t", "--alpha", "x", "--alpha", "y"]).err(),
            Some(ErrorKind::ArgumentConflict)
        );
    }

    #[test]
    fn require_equals_is_exported_and_clap_agrees() {
        let command = Command::new("t").arg(Arg::new("alpha").long("alpha").require_equals(true));
        let cli = export(command.clone(), &no_sources());
        assert!(arg_of(&cli, "alpha").require_equals);
        assert_ne!(
            json(Command::new("t").arg(Arg::new("alpha").long("alpha")), &no_sources()),
            json(command.clone(), &no_sources())
        );
        assert!(parse(&command, &["t", "--alpha=x"]).is_ok());
        assert!(parse(&command, &["t", "--alpha", "x"]).is_err());
    }

    #[test]
    fn last_is_exported_and_clap_agrees() {
        let command = Command::new("t").arg(Arg::new("rest").num_args(1..).last(true));
        let cli = export(command.clone(), &no_sources());
        assert!(arg_of(&cli, "rest").last);
        assert_ne!(
            json(Command::new("t").arg(Arg::new("rest").num_args(1..)), &no_sources()),
            json(command.clone(), &no_sources())
        );
        assert!(parse(&command, &["t", "--", "x"]).is_ok());
        assert!(parse(&command, &["t", "x"]).is_err());
    }

    #[test]
    fn allow_hyphen_values_is_exported_and_clap_agrees() {
        let command = Command::new("t").arg(Arg::new("alpha").long("alpha").allow_hyphen_values(true));
        let cli = export(command.clone(), &no_sources());
        assert!(arg_of(&cli, "alpha").allow_hyphen_values);
        let plain = Command::new("t").arg(Arg::new("alpha").long("alpha"));
        assert_ne!(json(plain.clone(), &no_sources()), json(command.clone(), &no_sources()));
        assert!(parse(&command, &["t", "--alpha", "-v"]).is_ok());
        assert!(parse(&plain, &["t", "--alpha", "-v"]).is_err());
    }

    // ── Fail-closed inputs ──────────────────────────────────────────────────

    #[test]
    #[should_panic(expected = "no value type for parser")]
    fn an_unmapped_value_parser_panics() {
        let command = Command::new("t").arg(Arg::new("ratio").long("ratio").value_parser(clap::value_parser!(f64)));
        export(command, &no_sources());
    }

    #[test]
    #[should_panic(expected = "which the root command does not declare")]
    fn an_env_flag_the_root_lacks_panics() {
        static FLAGS: &[(&EnvVar, &str)] = &[(&ocx_env::OCX_OFFLINE, "--no-such-flag")];
        let sources = Sources {
            env_flags: FLAGS,
            ..no_sources()
        };
        export(ocx_command(), &sources);
    }

    // ── Env and retired tables ──────────────────────────────────────────────

    static LENIENT: EnvVar = EnvVar {
        name: "OCX_EXAMPLE",
        doc: "An example switch.\nSecond line.",
        value: EnvValue::Bool,
        on_invalid: OnInvalid::Default,
        visibility: Visibility::Public,
        secret: false,
        child: Child::Inherit,
        reader: Reader::Ocx,
    };
    static STRICT: EnvVar = EnvVar {
        on_invalid: OnInvalid::Error,
        ..LENIENT
    };

    #[test]
    fn on_invalid_reaches_the_export() {
        let lenient = Sources {
            env: vec![&LENIENT],
            ..no_sources()
        };
        let strict = Sources {
            env: vec![&STRICT],
            ..no_sources()
        };
        let cli = export(two_options(), &strict);
        assert_eq!(cli.env[0].on_invalid, "error");
        assert_eq!(cli.env[0].summary, "An example switch.");
        assert_ne!(json(two_options(), &lenient), json(two_options(), &strict));
    }

    #[test]
    fn a_retired_entry_reaches_the_export() {
        static WINDOW: &[Retired] = &[Retired {
            name: "OCX_OLD_EXAMPLE",
            replacement: &LENIENT,
            change: Change::Rename,
            status: Status::Window {
                removal: ocx_env::Release {
                    major: 0,
                    minor: 9,
                    patch: 0,
                },
            },
        }];
        static REMOVED: &[Retired] = &[Retired {
            name: "OCX_OLD_EXAMPLE",
            replacement: &LENIENT,
            change: Change::Rename,
            status: Status::Removed,
        }];
        let window = Sources {
            retired: WINDOW,
            ..no_sources()
        };
        let removed = Sources {
            retired: REMOVED,
            ..no_sources()
        };
        let cli = export(two_options(), &window);
        assert_eq!(cli.retired[0].replacement, "OCX_EXAMPLE");
        assert_eq!(cli.retired[0].removal.as_deref(), Some("0.9.0"));
        assert_ne!(json(two_options(), &window), json(two_options(), &removed));
        assert_ne!(json(two_options(), &no_sources()), json(two_options(), &window));
    }

    #[test]
    fn every_env_flag_joins_its_root_flag() {
        let cli = export(ocx_command(), &Sources::ocx());
        for (var, flag) in ocx::app::ENV_FLAGS {
            let arg = cli
                .root
                .args
                .iter()
                .find(|arg| arg.long.as_deref().is_some_and(|long| format!("--{long}") == *flag))
                .unwrap_or_else(|| panic!("no root flag {flag}"));
            assert_eq!(arg.env.as_deref(), Some(var.name));
            let spec = cli.env.iter().find(|spec| spec.name == var.name).expect("declared");
            assert_eq!(spec.flag.as_deref(), Some(*flag));
        }
    }

    #[test]
    fn choice_values_reach_the_export() {
        static MODE: EnvVar = EnvVar {
            value: EnvValue::Choice(&["fast", "slow"]),
            ..LENIENT
        };
        let cli = export(
            two_options(),
            &Sources {
                env: vec![&MODE, &LENIENT],
                ..no_sources()
            },
        );
        assert_eq!(cli.env[0].choices, vec!["fast", "slow"]);
        assert!(cli.env[1].choices.is_empty());
    }

    #[test]
    fn the_ocx_choice_variables_export_their_values() {
        let cli = export(ocx_command(), &Sources::ocx());
        let choices = |name: &str| {
            cli.env
                .iter()
                .find(|spec| spec.name == name)
                .unwrap_or_else(|| panic!("no variable {name}"))
                .choices
                .clone()
        };
        assert_eq!(choices("OCX_LAZY_MODE"), vec!["never", "always"]);
        assert_eq!(choices("OCX_LAZY_REPORT"), vec!["silent", "progress"]);
        assert_eq!(choices("OCX_TOOLCHAIN_ACTIVATE"), vec!["env", "bin", "none"]);
        assert_eq!(
            choices("OCX_AUTH_{REGISTRY}_TYPE"),
            vec!["anonymous", "basic", "token", "bearer"]
        );
    }

    // ── Hermetic defaults ───────────────────────────────────────────────────

    // ── Probe agreement on the real tree ────────────────────────────────────

    fn visit(command: &Command, path: &mut Vec<String>, check: &mut dyn FnMut(&Command, &str)) {
        check(command, &path.join(" "));
        for sub in subcommands(command) {
            path.push(sub.get_name().to_owned());
            visit(sub, path, check);
            path.pop();
        }
    }

    /// Every leaf has a parsing base, and for each exported conflict clap refuses the pair while
    /// accepting each side alone.
    #[test]
    fn clap_agrees_with_every_exported_conflict() {
        let mut built = ocx_command();
        built.build();
        let mut checked = 0;
        visit(&built, &mut Vec::new(), &mut |command, path| {
            let own: Vec<&Arg> = command.get_arguments().filter(|arg| !is_builtin(arg)).collect();
            let mut prober = Prober::new(command, &own, path);
            let is_leaf = subcommands(command).next().is_none();
            let Some(base) = prober.base() else {
                assert!(!is_leaf, "`ocx {path}` has no argument set clap accepts");
                return;
            };
            for (a, arg) in own.iter().enumerate() {
                for (b, other) in own.iter().enumerate() {
                    if a >= b || base.contains(&a) || base.contains(&b) {
                        continue;
                    }
                    if !conflicts(command, arg).contains(&other.get_id().to_string()) {
                        continue;
                    }
                    let alone = |index| base.iter().copied().chain([index]).collect::<BTreeSet<usize>>();
                    if !matches!(prober.parse(&alone(a)), Outcome::Parsed)
                        || !matches!(prober.parse(&alone(b)), Outcome::Parsed)
                    {
                        continue;
                    }
                    let both = base.iter().copied().chain([a, b]).collect::<BTreeSet<usize>>();
                    let argv = prober.argv(&both);
                    let kind = prober
                        .command
                        .try_get_matches_from_mut(argv.clone())
                        .err()
                        .map(|e| e.kind());
                    assert_eq!(
                        kind,
                        Some(ErrorKind::ArgumentConflict),
                        "`ocx {path}` exports {} and {} as conflicting but clap parses {argv:?}",
                        arg.get_id(),
                        other.get_id()
                    );
                    checked += 1;
                }
            }
        });
        assert!(checked >= 10, "only {checked} conflicts checked");
    }

    fn command_at<'a>(spec: &'a CommandSpec, path: &[&str]) -> &'a CommandSpec {
        path.iter().fold(spec, |spec, word| {
            spec.commands
                .iter()
                .find(|sub| sub.path.last().is_some_and(|last| last == word))
                .unwrap_or_else(|| panic!("no command {path:?}"))
        })
    }

    /// The conditional requirements the `ocx` tree declares reach `cli.json`.
    #[test]
    fn the_ocx_conditional_requirements_are_exported() {
        let cli = export(ocx_command(), &Sources::ocx());
        let waivers = |path: &[&str], id: &str| {
            let spec = command_at(&cli.root, path);
            let arg = spec
                .args
                .iter()
                .find(|arg| arg.id == id)
                .unwrap_or_else(|| panic!("`ocx {}` has no argument {id}", path.join(" ")));
            arg.required_unless.clone()
        };
        assert_eq!(waivers(&["patch", "publish"], "base"), vec!["global"]);
        assert!(waivers(&["patch", "publish"], "global").is_empty());
        assert_eq!(waivers(&["package", "test"], "command"), vec!["junit", "script"]);
        assert!(waivers(&["package", "test"], "script").is_empty());
        assert_eq!(waivers(&["package", "announce"], "package"), vec!["package_flag"]);
    }

    /// `(path, flag, replacement flag)` for every flag row of the renamed table.
    fn flag_rows() -> Vec<(Vec<&'static str>, String, String)> {
        let rows: Vec<_> = ocx::command::deprecated::FLAGS
            .iter()
            .map(|row| (row.path.path().to_vec(), row.old.to_string(), row.new.to_string()))
            .collect();
        assert_eq!(
            rows.len(),
            8,
            "the renamed table has 8 flag rows; a dropped row reds here"
        );
        rows
    }

    fn spelled<'a>(spec: &'a CommandSpec, flag: &str) -> Vec<&'a ArgSpec> {
        spec.args
            .iter()
            .filter(|arg| match flag.strip_prefix("--") {
                Some(long) => arg.long.as_deref() == Some(long),
                None => flag.strip_prefix('-').and_then(|short| short.chars().next()) == arg.short,
            })
            .collect()
    }

    /// Each renamed flag spelling is exported hidden, with the window that removes it, beside a
    /// visible argument under its replacement spelling.
    #[test]
    fn every_renamed_flag_exports_its_window() {
        let cli = export(ocx_command(), &Sources::ocx());
        for (path, flag, replacement) in flag_rows() {
            let spec = command_at(&cli.root, &path);
            let at = format!("ocx {} {flag}", path.join(" "));
            let old = spelled(spec, &flag);
            assert_eq!(old.len(), 1, "`{at}` names one argument");
            assert!(old[0].hidden, "`{at}` is hidden");
            let window = old[0]
                .deprecated
                .as_ref()
                .unwrap_or_else(|| panic!("`{at}` exports no window"));
            assert_eq!(window.replacement, format!("{} {replacement}", path.join(" ")));
            assert_eq!(window.removal, "0.7");
            assert!(
                spelled(spec, &replacement)
                    .iter()
                    .any(|arg| !arg.hidden && arg.deprecated.is_none()),
                "`{at}` is replaced by a visible `{replacement}`"
            );
        }
    }

    /// A short letter freed this release is not rebound: only its deprecated spelling carries it.
    #[test]
    fn a_freed_short_letter_is_not_rebound() {
        let cli = export(ocx_command(), &Sources::ocx());
        let mut freed = 0;
        for (path, flag, _) in flag_rows().into_iter().filter(|(_, flag, _)| !flag.starts_with("--")) {
            freed += 1;
            for arg in spelled(command_at(&cli.root, &path), &flag) {
                assert!(
                    arg.deprecated.is_some(),
                    "`ocx {} {flag}` was freed this release and is bound to `{}`",
                    path.join(" "),
                    arg.id
                );
            }
        }
        assert_eq!(freed, 4, "four flag rows free a short letter; a dropped one reds here");
    }

    // ── Output contract ─────────────────────────────────────────────────────

    #[test]
    fn package_sbom_lists_a_raw_document() {
        let modes = contract::CONTRACT
            .iter()
            .find(|(path, _, _)| *path == ["package", "sbom"])
            .map(|(_, _, modes)| *modes)
            .expect("package sbom has a CONTRACT entry");
        assert!(modes.contains(&OutputMode::RawDocument));
        assert_eq!(
            serde_json::to_value(OutputMode::RawDocument).expect("serializable"),
            serde_json::json!({ "type": "raw_document" })
        );
    }

    fn descriptions<'a>(value: &'a serde_json::Value, out: &mut Vec<&'a str>) {
        match value {
            serde_json::Value::Object(map) => {
                if let Some(serde_json::Value::String(text)) = map.get("description") {
                    out.push(text);
                }
                map.values().for_each(|child| descriptions(child, out));
            }
            serde_json::Value::Array(items) => items.iter().for_each(|child| descriptions(child, out)),
            _ => {}
        }
    }

    /// Rustdoc link syntax copies raw into the published schema, where nothing resolves it.
    #[test]
    fn cli_schema_descriptions_carry_no_intra_doc_link() {
        let schema: serde_json::Value = serde_json::from_str(&cli_schema()).expect("valid JSON");
        let mut texts = Vec::new();
        descriptions(&schema, &mut texts);
        assert!(texts.len() > 20, "the walk read only {} descriptions", texts.len());
        let leaks: Vec<&&str> = texts.iter().filter(|text| text.contains("[`")).collect();
        assert!(leaks.is_empty(), "{leaks:?}");
    }

    fn tree() -> Command {
        Command::new("t")
            .subcommand(Command::new("shown"))
            .subcommand(Command::new("secret").hide(true))
            .subcommand(Command::new("group").subcommand(Command::new("inner")))
    }

    use ocx::api::data::{about::About, catalog::Catalog, clean::Clean};

    fn roots() -> Vec<String> {
        vec!["About".to_owned(), "Catalog".to_owned()]
    }

    const REPORT_ABOUT: &[OutputMode] = &[OutputMode::report::<About>()];
    const REPORT_CATALOG: &[OutputMode] = &[OutputMode::report_then_fail::<Catalog>()];

    #[test]
    fn a_consistent_contract_has_no_findings() {
        let contract: &[(&[&str], u32, &[OutputMode])] =
            &[(&["shown"], 1, REPORT_ABOUT), (&["group", "inner"], 1, REPORT_CATALOG)];
        assert_eq!(contract_findings(&tree(), contract, &roots()), Vec::<String>::new());
    }

    #[test]
    fn a_visible_leaf_without_an_entry_is_a_finding() {
        let contract: &[(&[&str], u32, &[OutputMode])] = &[(&["shown"], 1, REPORT_ABOUT)];
        let findings = contract_findings(&tree(), contract, &["About".to_owned()]);
        assert_eq!(findings, vec!["`ocx group inner` has no CONTRACT entry"]);
    }

    #[test]
    fn a_stale_entry_is_a_finding() {
        let contract: &[(&[&str], u32, &[OutputMode])] = &[
            (&["shown"], 1, REPORT_ABOUT),
            (&["group", "inner"], 1, REPORT_CATALOG),
            (&["gone"], 1, &[OutputMode::Empty]),
        ];
        let findings = contract_findings(&tree(), contract, &roots());
        assert_eq!(findings, vec!["CONTRACT entry `ocx gone` names no command"]);
    }

    #[test]
    fn an_unregistered_report_root_is_a_finding() {
        let contract: &[(&[&str], u32, &[OutputMode])] = &[
            (&["shown"], 1, REPORT_ABOUT),
            (&["group", "inner"], 1, &[OutputMode::report::<Clean>()]),
        ];
        let findings = contract_findings(&tree(), contract, &["About".to_owned()]);
        assert_eq!(
            findings,
            vec!["`ocx group inner` reports `Clean`, which the reports registry does not publish"]
        );
    }

    #[test]
    fn a_registered_root_nothing_reports_is_a_finding() {
        let contract: &[(&[&str], u32, &[OutputMode])] = &[
            (&["shown"], 1, REPORT_ABOUT),
            (&["group", "inner"], 1, &[OutputMode::Empty]),
        ];
        let findings = contract_findings(&tree(), contract, &roots());
        assert_eq!(
            findings,
            vec!["registered root `Catalog` is reported by no CONTRACT entry"]
        );
    }

    #[test]
    fn the_ocx_contract_has_no_findings() {
        let findings = contract_findings(&ocx_command(), contract::CONTRACT, &crate::reports::root_names());
        assert!(findings.is_empty(), "{}", findings.join("\n"));
    }
}
