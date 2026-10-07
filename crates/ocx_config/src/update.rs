// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `[update]`: the personal posture of the background checks for ocx itself and the toolchain.
//!
//! Read from the local `config.toml` tiers only: the loader drops it from a managed payload and
//! `ocx.toml` refuses it, so neither a fleet publisher nor a cloned repository can switch on
//! binary replacement.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Deserializer};

use crate::refresh::{DEFAULT_INTERVAL, RefreshPolicy, parse_interval};

/// A `[update]` value this ocx cannot use, kept as written so the resolver can warn about it.
///
/// Rejection never happens at parse time: only a command that runs a check resolves, and so warns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejected {
    /// The value in TOML notation.
    pub value: String,
    /// The `config.toml` it came from, when the loader read it from a file.
    pub origin: Option<PathBuf>,
}

impl Rejected {
    fn new(value: &toml::Value) -> Self {
        Self {
            value: value.to_string(),
            origin: None,
        }
    }
}

/// A `[update]` key: the value this ocx understands, or the rejected input.
pub type Setting<T> = Result<T, Rejected>;

// No `deny_unknown_fields`, for the fleet forward-compat reason stated on `crate::Config`.
// Deserializes from any TOML value and never fails: a wrong shape becomes a `Rejected` the
// resolver warns about. A strict parse would make a malformed `[update]` exit 78 on every command.
/// The `[update]` section of `config.toml`.
#[derive(Debug, Default, Clone, PartialEq, Eq, schemars::JsonSchema)]
pub struct UpdateConfig {
    /// What ocx does when a newer ocx is published: `apply` installs it after the command
    /// finishes, `notify` (default) prints a notice, `manual` never checks.
    #[serde(rename = "self", default)]
    // `with` drops the derived `"default": null` the published schema carries.
    #[schemars(with = "Option<RefreshPolicy>", extend("default" = null))]
    pub self_policy: Option<Setting<RefreshPolicy>>,

    /// What ocx does when a locked tool's tag has moved: `notify` (default) prints a notice,
    /// `manual` never checks. `apply` is treated as `notify`: only `ocx update` moves a pin.
    #[serde(default)]
    // `with` drops the derived `"default": null` the published schema carries.
    #[schemars(with = "Option<RefreshPolicy>", extend("default" = null))]
    pub toolchain: Option<Setting<RefreshPolicy>>,

    /// Minimum time between two checks, `\d+[smhd]?` (bare digits = seconds); `"0"` checks on
    /// every command. Defaults to `"1d"`.
    #[schemars(with = "Option<String>")]
    pub interval: Option<Setting<Duration>>,

    /// Set when `update` was written as something other than a table.
    #[schemars(skip)]
    pub section: Option<Rejected>,
}

impl<'de> Deserialize<'de> for UpdateConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // `toml::Value` accepts every TOML value, so nothing below `[update]` can fail the file.
        Ok(Self::from_value(&toml::Value::deserialize(deserializer)?))
    }
}

impl UpdateConfig {
    fn from_value(value: &toml::Value) -> Self {
        let Some(table) = value.as_table() else {
            return Self {
                section: Some(Rejected::new(value)),
                ..Self::default()
            };
        };
        Self {
            self_policy: table.get("self").map(policy_setting),
            toolchain: table.get("toolchain").map(policy_setting),
            interval: table.get("interval").map(interval_setting),
            section: None,
        }
    }

    /// Records `path` as the origin of every rejected value, so the warning names the file.
    pub fn set_origin(&mut self, path: &Path) {
        let rejected = [
            self.self_policy.as_mut().and_then(|setting| setting.as_mut().err()),
            self.toolchain.as_mut().and_then(|setting| setting.as_mut().err()),
            self.interval.as_mut().and_then(|setting| setting.as_mut().err()),
            self.section.as_mut(),
        ];
        for rejected in rejected.into_iter().flatten() {
            rejected.origin = Some(path.to_path_buf());
        }
    }

    /// Merge higher-precedence `other` into `self`, field by field.
    pub fn merge(&mut self, other: UpdateConfig) {
        if other.self_policy.is_some() {
            self.self_policy = other.self_policy;
        }
        if other.toolchain.is_some() {
            self.toolchain = other.toolchain;
        }
        if other.interval.is_some() {
            self.interval = other.interval;
        }
        if other.section.is_some() {
            self.section = other.section;
        }
    }
}

fn policy_setting(value: &toml::Value) -> Setting<RefreshPolicy> {
    value
        .as_str()
        .and_then(RefreshPolicy::parse)
        .ok_or_else(|| Rejected::new(value))
}

/// The string grammar, or a non-negative integer read as seconds like bare digits are.
fn interval_setting(value: &toml::Value) -> Setting<Duration> {
    let parsed = match value {
        toml::Value::String(text) => parse_interval(text).ok(),
        toml::Value::Integer(seconds) => u64::try_from(*seconds).ok().map(Duration::from_secs),
        _ => None,
    };
    parsed.ok_or_else(|| Rejected::new(value))
}

/// The effective `[update]` posture after env, config and defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedUpdatePolicy {
    pub self_policy: RefreshPolicy,
    /// Never [`RefreshPolicy::Apply`].
    pub toolchain: RefreshPolicy,
    pub interval: Duration,
}

impl ResolvedUpdatePolicy {
    pub const DEFAULT_POLICY: RefreshPolicy = RefreshPolicy::Notify;

    /// Resolves each key as env ▸ `config` ▸ default; never fails.
    ///
    /// An invalid value is skipped for the next source: a `config.toml` value with one warning,
    /// an env value at debug. A no-update setting must never break a command on any host.
    #[must_use]
    pub fn resolve(config: Option<&UpdateConfig>) -> Self {
        let file = config.cloned().unwrap_or_default();
        if let Some(rejected) = &file.section {
            warn_rejected("update", rejected, "a table");
        }

        let self_policy = env_policy(&ocx_env::OCX_SELF_UPDATE)
            .or_else(|| file_value("[update] self", file.self_policy, POLICY_VALUES))
            .unwrap_or(Self::DEFAULT_POLICY);

        let toolchain = match env_policy(&ocx_env::OCX_TOOLCHAIN_UPDATE) {
            Some(RefreshPolicy::Apply) => {
                log::debug!(
                    "{}=apply is not supported; using notify",
                    ocx_env::OCX_TOOLCHAIN_UPDATE.name
                );
                RefreshPolicy::Notify
            }
            Some(policy) => policy,
            None => match file_value("[update] toolchain", file.toolchain, POLICY_VALUES) {
                Some(RefreshPolicy::Apply) => {
                    log::warn!(
                        "[update] toolchain = \"apply\" in config.toml is not supported; using notify (only `ocx update` moves a pin)"
                    );
                    RefreshPolicy::Notify
                }
                Some(policy) => policy,
                None => Self::DEFAULT_POLICY,
            },
        };

        let interval = env_interval()
            .or_else(|| file_value("[update] interval", file.interval, "a duration such as \"6h\""))
            .unwrap_or_else(default_interval);

        Self {
            self_policy,
            toolchain,
            interval,
        }
    }
}

impl Default for ResolvedUpdatePolicy {
    fn default() -> Self {
        Self {
            self_policy: Self::DEFAULT_POLICY,
            toolchain: Self::DEFAULT_POLICY,
            interval: default_interval(),
        }
    }
}

fn default_interval() -> Duration {
    // `DEFAULT_INTERVAL` is a literal `parse_interval` accepts (pinned by its own test); the
    // fallback only keeps this path panic-free.
    parse_interval(DEFAULT_INTERVAL).unwrap_or(Duration::from_secs(86_400))
}

fn env_policy(var: &'static ocx_env::EnvVar) -> Option<RefreshPolicy> {
    let value = var.get()?;
    let parsed = RefreshPolicy::parse(&value);
    if parsed.is_none() {
        log::debug!("ignoring {}={value:?}: expected apply, notify or manual", var.name);
    }
    parsed
}

fn env_interval() -> Option<Duration> {
    let value = ocx_env::OCX_UPDATE_CHECK_INTERVAL.get()?;
    match parse_interval(value.trim()) {
        Ok(interval) => Some(interval),
        Err(error) => {
            log::debug!("ignoring {}: {error}", ocx_env::OCX_UPDATE_CHECK_INTERVAL.name);
            None
        }
    }
}

const POLICY_VALUES: &str = "one of apply, notify or manual";

/// The usable value of a `config.toml` key; a rejected one warns once and yields `None`.
fn file_value<T>(key: &str, setting: Option<Setting<T>>, expected: &str) -> Option<T> {
    match setting? {
        Ok(value) => Some(value),
        Err(rejected) => {
            warn_rejected(key, &rejected, expected);
            None
        }
    }
}

fn warn_rejected(key: &str, rejected: &Rejected, expected: &str) {
    let file = rejected
        .origin
        .as_deref()
        .map_or_else(|| "config.toml".to_string(), |path| path.display().to_string());
    log::warn!(
        "{key} = {} in {file} is not {expected}; ignored, the default applies",
        rejected.value
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: Duration = Duration::from_secs(86_400);

    /// Holds the env table with every update key removed, so the process env cannot leak in.
    fn clean_env() -> ocx_env::overrides::EnvLock {
        let guard = ocx_env::overrides::lock();
        for key in [
            &ocx_env::OCX_SELF_UPDATE,
            &ocx_env::OCX_TOOLCHAIN_UPDATE,
            &ocx_env::OCX_UPDATE_CHECK_INTERVAL,
        ] {
            guard.remove(key);
        }
        guard
    }

    fn file(toml_text: &str) -> UpdateConfig {
        let config: crate::Config = toml::from_str(toml_text).expect("config must parse");
        config.update.expect("[update] must be present")
    }

    #[test]
    fn defaults_without_env_or_config() {
        let _guard = clean_env();
        assert_eq!(
            ResolvedUpdatePolicy::resolve(None),
            ResolvedUpdatePolicy {
                self_policy: RefreshPolicy::Notify,
                toolchain: RefreshPolicy::Notify,
                interval: DAY,
            }
        );
    }

    #[test]
    fn config_beats_the_defaults() {
        let _guard = clean_env();
        let config = file("[update]\nself = \"manual\"\ntoolchain = \"manual\"\ninterval = \"6h\"\n");
        assert_eq!(
            ResolvedUpdatePolicy::resolve(Some(&config)),
            ResolvedUpdatePolicy {
                self_policy: RefreshPolicy::Manual,
                toolchain: RefreshPolicy::Manual,
                interval: Duration::from_secs(6 * 3600),
            }
        );
    }

    #[test]
    fn env_beats_config() {
        let guard = clean_env();
        guard.set(&ocx_env::OCX_SELF_UPDATE, "notify");
        guard.set(&ocx_env::OCX_TOOLCHAIN_UPDATE, "notify");
        guard.set(&ocx_env::OCX_UPDATE_CHECK_INTERVAL, "60");
        let config = file("[update]\nself = \"manual\"\ntoolchain = \"manual\"\ninterval = \"6h\"\n");
        assert_eq!(
            ResolvedUpdatePolicy::resolve(Some(&config)),
            ResolvedUpdatePolicy {
                self_policy: RefreshPolicy::Notify,
                toolchain: RefreshPolicy::Notify,
                interval: Duration::from_secs(60),
            }
        );
    }

    #[test]
    fn invalid_env_values_fall_through_to_config() {
        let guard = clean_env();
        guard.set(&ocx_env::OCX_SELF_UPDATE, "someday");
        guard.set(&ocx_env::OCX_TOOLCHAIN_UPDATE, "someday");
        guard.set(&ocx_env::OCX_UPDATE_CHECK_INTERVAL, "bogus");
        let config = file("[update]\nself = \"apply\"\ntoolchain = \"manual\"\ninterval = \"6h\"\n");
        assert_eq!(
            ResolvedUpdatePolicy::resolve(Some(&config)),
            ResolvedUpdatePolicy {
                self_policy: RefreshPolicy::Apply,
                toolchain: RefreshPolicy::Manual,
                interval: Duration::from_secs(6 * 3600),
            }
        );
    }

    #[test]
    fn invalid_config_values_fall_through_to_the_defaults() {
        let _guard = clean_env();
        let config = file("[update]\nself = \"someday\"\ntoolchain = 7\ninterval = \"bogus\"\n");
        assert_eq!(
            ResolvedUpdatePolicy::resolve(Some(&config)),
            ResolvedUpdatePolicy::default()
        );
    }

    #[test]
    fn toolchain_apply_is_never_resolved() {
        let guard = clean_env();
        let config = file("[update]\ntoolchain = \"apply\"\n");
        assert_eq!(
            ResolvedUpdatePolicy::resolve(Some(&config)).toolchain,
            RefreshPolicy::Notify
        );
        guard.set(&ocx_env::OCX_TOOLCHAIN_UPDATE, "apply");
        assert_eq!(ResolvedUpdatePolicy::resolve(None).toolchain, RefreshPolicy::Notify);
    }

    #[test]
    fn self_apply_is_kept() {
        let guard = clean_env();
        guard.set(&ocx_env::OCX_SELF_UPDATE, "apply");
        assert_eq!(ResolvedUpdatePolicy::resolve(None).self_policy, RefreshPolicy::Apply);
    }

    #[test]
    fn env_interval_accepts_the_suffix_grammar() {
        let guard = clean_env();
        for (value, expected) in [
            ("1d", DAY),
            ("3600", Duration::from_secs(3600)),
            ("0", Duration::ZERO),
            ("bogus", DAY),
        ] {
            guard.set(&ocx_env::OCX_UPDATE_CHECK_INTERVAL, value);
            assert_eq!(ResolvedUpdatePolicy::resolve(None).interval, expected, "{value}");
        }
    }

    #[test]
    fn empty_env_values_are_unset() {
        let guard = clean_env();
        guard.set(&ocx_env::OCX_SELF_UPDATE, "");
        guard.set(&ocx_env::OCX_UPDATE_CHECK_INTERVAL, "");
        let config = file("[update]\nself = \"manual\"\ninterval = \"1h\"\n");
        let resolved = ResolvedUpdatePolicy::resolve(Some(&config));
        assert_eq!(resolved.self_policy, RefreshPolicy::Manual);
        assert_eq!(resolved.interval, Duration::from_secs(3600));
    }

    #[test]
    fn merge_is_field_wise_nearest_wins() {
        let mut lower = file("[update]\nself = \"manual\"\ntoolchain = \"manual\"\ninterval = \"6h\"\n");
        lower.merge(file("[update]\nself = \"apply\"\n"));
        assert_eq!(lower.self_policy, Some(Ok(RefreshPolicy::Apply)));
        assert_eq!(lower.toolchain, Some(Ok(RefreshPolicy::Manual)));
        assert_eq!(lower.interval, Some(Ok(Duration::from_secs(6 * 3600))));
    }

    /// The owner ruling: no `[update]` value shape may fail the config parse (exit 78 on every command).
    #[test]
    fn a_wrongly_typed_value_never_fails_the_file() {
        let _guard = clean_env();
        for text in [
            "update = 1\n",
            "update = \"apply\"\n",
            "[update]\nself = 5\n",
            "[update]\ntoolchain = []\n",
            "[update]\ninterval = []\n",
            "[update]\ninterval = -5\n",
            "[update]\ninterval = 1.5\n",
        ] {
            let config: crate::Config = toml::from_str(text).unwrap_or_else(|error| panic!("{text:?}: {error}"));
            assert_eq!(
                ResolvedUpdatePolicy::resolve(config.update.as_ref()),
                ResolvedUpdatePolicy::default(),
                "{text:?}"
            );
        }
    }

    /// Bare digits are seconds in the string grammar, so an integer reads the same way.
    #[test]
    fn an_integer_interval_is_seconds() {
        let _guard = clean_env();
        let config: crate::Config = toml::from_str("[update]\ninterval = 3600\n").expect("must parse");
        assert_eq!(
            ResolvedUpdatePolicy::resolve(config.update.as_ref()).interval,
            Duration::from_secs(3600)
        );
    }

    #[test]
    fn unknown_keys_and_values_do_not_fail_the_file() {
        let config = file("[update]\nself = \"someday\"\nfuture_key = 1\ninterval = \"2h\"\n");
        assert_eq!(
            config.self_policy,
            Some(Err(Rejected {
                value: "\"someday\"".to_string(),
                origin: None
            }))
        );
        assert_eq!(config.interval, Some(Ok(Duration::from_secs(2 * 3600))));
    }

    /// The loader stamps the file onto every rejected value, so the warning can name it.
    #[test]
    fn set_origin_reaches_every_rejected_value() {
        let mut config = file("[update]\nself = 5\ntoolchain = \"manual\"\ninterval = []\n");
        config.set_origin(Path::new("/etc/ocx/config.toml"));
        let origin = Some(PathBuf::from("/etc/ocx/config.toml"));
        assert_eq!(
            config.self_policy.and_then(Result::err).map(|r| r.origin),
            Some(origin.clone())
        );
        assert_eq!(config.interval.and_then(Result::err).map(|r| r.origin), Some(origin));
        assert_eq!(config.toolchain, Some(Ok(RefreshPolicy::Manual)));

        let mut section: crate::Config = toml::from_str("update = 1\n").expect("must parse");
        let update = section.update.as_mut().expect("a non-table update is kept as rejected");
        update.set_origin(Path::new("/etc/ocx/config.toml"));
        assert_eq!(
            update.section.as_ref().and_then(|r| r.origin.as_deref()),
            Some(Path::new("/etc/ocx/config.toml"))
        );
    }
}
