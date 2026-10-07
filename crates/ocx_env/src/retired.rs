// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Renamed and removed environment spellings: never silently ignored.

use std::ffi::OsString;

use crate::EnvVar;

/// A release version, const-constructible so one value backs every removal notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Release {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Release {
    /// `MAJOR.MINOR`, the spelling command rows carry (`"0.7"`); [`Display`](std::fmt::Display) gives the full one.
    pub fn minor_form(&self) -> String {
        format!("{}.{}", self.major, self.minor)
    }
}

impl std::fmt::Display for Release {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The release that removes every spelling deprecated in 0.6.
pub const REMOVAL_RELEASE: Release = Release {
    major: 0,
    minor: 7,
    patch: 0,
};

/// A spelling OCX no longer reads under its own name.
#[derive(Debug)]
pub struct Retired {
    /// The old variable name; for a [`Change::ValueRename`], the name of its replacement.
    pub name: &'static str,
    pub replacement: &'static EnvVar,
    pub change: Change,
    pub status: Status,
}

/// What changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Same meaning under a new name.
    Rename,
    /// Same variable, the value spelling `old` became `new`.
    ValueRename { old: &'static str, new: &'static str },
    /// The meaning inverted (`OCX_X` → `OCX_NO_X`); never honoured, or an old setting would flip a hardening switch.
    Polarity,
}

/// Whether the old spelling still works.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Honoured with a warning until the release `removal`, which must be above the crate version.
    Window { removal: Release },
    /// Refused: naming it is a configuration error that names the replacement.
    Removed,
}

/// Every retired spelling of this build.
pub static RETIRED: &[Retired] = &[
    Retired {
        name: "OCX_NO_COMPLETIONS",
        replacement: &crate::OCX_NO_COMPLETION,
        change: Change::Rename,
        status: Status::Window {
            removal: REMOVAL_RELEASE,
        },
    },
    Retired {
        name: "OCX_LOG",
        replacement: &crate::OCX_LOG_LEVEL,
        change: Change::Rename,
        status: Status::Window {
            removal: REMOVAL_RELEASE,
        },
    },
];

/// The [`RETIRED`] entries `env` uses: a retired name that is set, or a retired value spelling that is set.
pub fn retired_hits(env: &[(OsString, OsString)]) -> Vec<&'static Retired> {
    hits(table(), env)
}

/// [`RETIRED`], or the table a test injected through the override seam.
pub(crate) fn table() -> &'static [Retired] {
    #[cfg(any(test, feature = "__testing"))]
    if let Some(table) = crate::overrides::retired() {
        return table;
    }
    RETIRED
}

fn hits(table: &'static [Retired], env: &[(OsString, OsString)]) -> Vec<&'static Retired> {
    table
        .iter()
        .filter(|entry| {
            env.iter().any(|(key, value)| {
                *key == entry.name
                    && match entry.change {
                        Change::ValueRename { old, .. } => *value == old,
                        Change::Rename | Change::Polarity => !value.is_empty(),
                    }
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;
    use crate::{OCX_LAZY_MODE, OCX_LOG_LEVEL, OCX_QUIET, overrides};

    fn table(entries: Vec<Retired>) -> &'static [Retired] {
        Box::leak(entries.into_boxed_slice())
    }

    const fn release(major: u32, minor: u32, patch: u32) -> Release {
        Release { major, minor, patch }
    }

    fn version(text: &str) -> Option<Release> {
        let mut parts = text.split('.').map(|part| part.parse::<u32>().ok());
        let parsed = release(parts.next()??, parts.next()??, parts.next()??);
        parts.next().is_none().then_some(parsed)
    }

    /// Windows whose removal release is not above `crate_version`: each must flip to `Removed`.
    fn expired_windows(table: &[Retired], crate_version: &str) -> Vec<String> {
        let Some(current) = version(crate_version) else {
            return vec![format!("crate version {crate_version} is not MAJOR.MINOR.PATCH")];
        };
        table
            .iter()
            .filter_map(|entry| match entry.status {
                Status::Window { removal } if removal <= current => Some(format!("{} (removal {removal})", entry.name)),
                _ => None,
            })
            .collect()
    }

    /// Polarity entries still honoured: an inverted old setting would flip a hardening switch.
    fn honoured_polarity(table: &[Retired]) -> Vec<&'static str> {
        table
            .iter()
            .filter(|entry| entry.change == Change::Polarity && entry.status != Status::Removed)
            .map(|entry| entry.name)
            .collect()
    }

    fn rename(name: &'static str, status: Status) -> Retired {
        Retired {
            name,
            replacement: &OCX_LOG_LEVEL,
            change: Change::Rename,
            status,
        }
    }

    #[test]
    fn a_release_renders_its_minor_form_and_its_full_form() {
        assert_eq!(REMOVAL_RELEASE.minor_form(), "0.7");
        assert_eq!(REMOVAL_RELEASE.to_string(), "0.7.0");
        let release = Release {
            major: 1,
            minor: 12,
            patch: 3,
        };
        assert_eq!(
            (release.minor_form(), release.to_string()),
            ("1.12".into(), "1.12.3".into())
        );
    }

    #[test]
    fn a_window_at_or_below_the_crate_version_reds() {
        assert_eq!(
            expired_windows(RETIRED, env!("CARGO_PKG_VERSION")),
            Vec::<String>::new()
        );
        let synthetic = [
            rename(
                "OCX_OLD_EQUAL",
                Status::Window {
                    removal: release(0, 6, 4),
                },
            ),
            rename(
                "OCX_OLD_BELOW",
                Status::Window {
                    removal: release(0, 5, 9),
                },
            ),
            rename(
                "OCX_OLD_ABOVE",
                Status::Window {
                    removal: release(0, 7, 0),
                },
            ),
            rename("OCX_OLD_REMOVED", Status::Removed),
        ];
        assert_eq!(
            expired_windows(&synthetic, "0.6.4"),
            ["OCX_OLD_EQUAL (removal 0.6.4)", "OCX_OLD_BELOW (removal 0.5.9)"]
        );
        assert_eq!(expired_windows(&synthetic[2..], "0.6.4"), Vec::<String>::new());
    }

    #[test]
    fn a_polarity_change_is_always_removed() {
        assert_eq!(honoured_polarity(RETIRED), Vec::<&str>::new());
        let polarity = |name, status| Retired {
            name,
            replacement: &OCX_LOG_LEVEL,
            change: Change::Polarity,
            status,
        };
        let synthetic = [
            polarity(
                "OCX_OLD_VERIFY",
                Status::Window {
                    removal: release(9, 0, 0),
                },
            ),
            polarity("OCX_OLD_CONSENT", Status::Removed),
            rename(
                "OCX_OLD_NAME",
                Status::Window {
                    removal: release(9, 0, 0),
                },
            ),
        ];
        assert_eq!(honoured_polarity(&synthetic), ["OCX_OLD_VERIFY"]);
    }

    #[test]
    fn hits_names_a_set_retired_name_or_value() {
        let retired = table(vec![
            rename(
                "OCX_OLD_LOG",
                Status::Window {
                    removal: release(9, 0, 0),
                },
            ),
            Retired {
                name: OCX_LAZY_MODE.name,
                replacement: &OCX_LAZY_MODE,
                change: Change::ValueRename {
                    old: "eager",
                    new: "always",
                },
                status: Status::Window {
                    removal: release(9, 0, 0),
                },
            },
        ]);
        let env = |pairs: &[(&str, &str)]| -> Vec<(OsString, OsString)> {
            pairs
                .iter()
                .map(|(key, value)| ((*key).into(), (*value).into()))
                .collect()
        };
        assert!(hits(retired, &env(&[("OCX_LOG_LEVEL", "debug"), ("OCX_LAZY_MODE", "never")])).is_empty());
        let found = hits(retired, &env(&[("OCX_OLD_LOG", "debug"), ("OCX_LAZY_MODE", "eager")]));
        assert_eq!(
            found.iter().map(|entry| entry.name).collect::<Vec<_>>(),
            ["OCX_OLD_LOG", "OCX_LAZY_MODE"]
        );
    }

    #[test]
    fn an_in_window_rename_is_honoured_only_while_the_replacement_is_unset() {
        let retired = table(vec![
            rename(
                "OCX_OLD_LOG",
                Status::Window {
                    removal: release(9, 0, 0),
                },
            ),
            Retired {
                name: "OCX_GONE_QUIET",
                replacement: &OCX_QUIET,
                change: Change::Rename,
                status: Status::Removed,
            },
        ]);
        let env = overrides::lock();
        env.remove(&OCX_LOG_LEVEL);
        env.set_raw("OCX_OLD_LOG", "trace");
        assert_eq!(crate::lookup(&OCX_LOG_LEVEL, retired), Some("trace".into()));
        env.set(&OCX_LOG_LEVEL, "debug");
        assert_eq!(crate::lookup(&OCX_LOG_LEVEL, retired), Some("debug".into()));
        env.remove(&OCX_QUIET);
        env.set_raw("OCX_GONE_QUIET", "1");
        assert_eq!(crate::lookup(&OCX_QUIET, retired), None);
    }

    #[test]
    fn an_injected_table_replaces_retired_until_the_lock_drops() {
        let retired = table(vec![rename(
            "OCX_OLD_LOG",
            Status::Window {
                removal: release(9, 0, 0),
            },
        )]);
        let set = [(OsString::from("OCX_OLD_LOG"), OsString::from("trace"))];
        {
            let env = overrides::lock();
            env.retire(retired);
            env.remove(&OCX_LOG_LEVEL);
            env.set_raw("OCX_OLD_LOG", "trace");
            assert_eq!(retired_hits(&set).len(), 1);
            assert_eq!(OCX_LOG_LEVEL.get().as_deref(), Some("trace"));
        }
        let _env = overrides::lock();
        assert!(retired_hits(&set).is_empty());
    }

    #[test]
    fn an_in_window_value_rename_maps_the_old_spelling() {
        let retired = table(vec![Retired {
            name: OCX_LAZY_MODE.name,
            replacement: &OCX_LAZY_MODE,
            change: Change::ValueRename {
                old: "eager",
                new: "always",
            },
            status: Status::Window {
                removal: release(9, 0, 0),
            },
        }]);
        let env = overrides::lock();
        env.set(&OCX_LAZY_MODE, "eager");
        assert_eq!(crate::lookup(&OCX_LAZY_MODE, retired), Some("always".into()));
        env.set(&OCX_LAZY_MODE, "never");
        assert_eq!(crate::lookup(&OCX_LAZY_MODE, retired), Some("never".into()));
    }
}
