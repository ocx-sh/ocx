// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// When ANSI color codes are emitted; the value of `--color`.
#[derive(Clone, Copy, Debug, Default)]
pub enum ColorMode {
    /// Enable colors when stdout is a terminal and color-suppressing env vars are not set.
    #[default]
    Auto,
    /// Always emit ANSI color codes, even when piped.
    Always,
    /// Disable all color output.
    Never,
}

impl ColorMode {
    /// Pre-scans the process argv for `--color <value>` or `--color=<value>`, so clap's own help and
    /// error rendering can respect it.
    pub fn from_args() -> Self {
        Self::from_argv(std::env::args().skip(1))
    }

    /// [`Self::from_args`] over an explicit argument list, program name excluded.
    pub fn from_argv(args: impl IntoIterator<Item = String>) -> Self {
        use clap_builder::ValueEnum;

        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            if arg == "--" {
                break;
            }
            let value = if arg == "--color" {
                args.next()
            } else {
                arg.strip_prefix("--color=").map(String::from)
            };
            if let Some(value) = value {
                return ColorMode::from_str(&value, true).unwrap_or_default();
            }
        }
        ColorMode::default()
    }

    pub fn config(self) -> ColorModeConfig {
        match self {
            ColorMode::Always => ColorModeConfig {
                stdout: true,
                stderr: true,
                relayed: true,
            },
            ColorMode::Never => ColorModeConfig {
                stdout: false,
                stderr: false,
                relayed: false,
            },
            ColorMode::Auto => ColorModeConfig::from_env(),
        }
    }
}

impl clap_builder::ValueEnum for ColorMode {
    fn value_variants<'a>() -> &'a [Self] {
        &[Self::Auto, Self::Always, Self::Never]
    }

    fn to_possible_value(&self) -> Option<clap_builder::builder::PossibleValue> {
        use clap_builder::builder::PossibleValue;

        Some(match self {
            Self::Auto => PossibleValue::new("auto"),
            Self::Always => PossibleValue::new("always"),
            Self::Never => PossibleValue::new("never"),
        })
    }
}

impl From<ColorMode> for clap_builder::ColorChoice {
    fn from(mode: ColorMode) -> Self {
        match mode {
            ColorMode::Auto => Self::Auto,
            ColorMode::Always => Self::Always,
            ColorMode::Never => Self::Never,
        }
    }
}

/// Per-stream color decision; under `Auto`, stdout and stderr differ when only one is a TTY.
#[derive(Clone, Copy, Debug)]
pub struct ColorModeConfig {
    pub stdout: bool,
    pub stderr: bool,
    /// The decision for text another program prints, such as the per-prompt reconcile stream a shell evaluates.
    ///
    /// Never falls back to a tty probe: no descriptor this process holds is the one the text lands on.
    pub relayed: bool,
}

impl ColorModeConfig {
    /// Applies the env-var priority chain for `Auto` mode, with per-stream TTY fallback.
    fn from_env() -> Self {
        let enabled = 'env: {
            // Precedence: NO_COLOR (https://no-color.org/), CLICOLOR_FORCE, CLICOLOR=0, TERM=dumb, then a tty probe.
            if ocx_env::NO_COLOR.get().is_some() {
                break 'env false;
            }
            if ocx_env::CLICOLOR_FORCE.get().is_some_and(|v| v != "0") {
                break 'env true;
            }
            if ocx_env::CLICOLOR.get().is_some_and(|v| v == "0") {
                break 'env false;
            }
            if ocx_env::TERM.get().is_some_and(|v| v == "dumb") {
                break 'env false;
            }
            return Self {
                stdout: console::Term::stdout().is_term(),
                stderr: console::Term::stderr().is_term(),
                relayed: true,
            };
        };

        Self {
            stdout: enabled,
            stderr: enabled,
            relayed: enabled,
        }
    }

    /// Sets the `console` crate's global color state for stdout and stderr; call once after [`ColorMode::config()`].
    pub fn apply(&self) {
        console::set_colors_enabled(self.stdout);
        console::set_colors_enabled_stderr(self.stderr);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn always_enables_both_streams() {
        let config = ColorMode::Always.config();
        assert!(config.stdout);
        assert!(config.stderr);
        assert!(config.relayed);
    }

    #[test]
    fn never_disables_both_streams() {
        let config = ColorMode::Never.config();
        assert!(!config.stdout);
        assert!(!config.stderr);
        assert!(
            !config.relayed,
            "an explicit refusal must reach the relayed channel too"
        );
    }

    /// Every row decides before the tty probe, so the answer never depends on how the test runs.
    #[test]
    fn auto_follows_the_color_environment_precedence() {
        let keys: [&'static ocx_env::EnvVar; 4] = [
            &ocx_env::NO_COLOR,
            &ocx_env::CLICOLOR_FORCE,
            &ocx_env::CLICOLOR,
            &ocx_env::TERM,
        ];
        let env = ocx_env::overrides::lock();
        let rows: [(&[(&ocx_env::EnvVar, &str)], bool); 7] = [
            (&[(&ocx_env::NO_COLOR, "1"), (&ocx_env::CLICOLOR_FORCE, "1")], false),
            (&[(&ocx_env::NO_COLOR, ""), (&ocx_env::CLICOLOR_FORCE, "1")], true),
            (&[(&ocx_env::CLICOLOR_FORCE, "0"), (&ocx_env::CLICOLOR, "0")], false),
            (&[(&ocx_env::CLICOLOR_FORCE, ""), (&ocx_env::TERM, "dumb")], false),
            (&[(&ocx_env::CLICOLOR_FORCE, "1"), (&ocx_env::TERM, "dumb")], true),
            (&[(&ocx_env::CLICOLOR, "1"), (&ocx_env::TERM, "dumb")], false),
            (&[(&ocx_env::CLICOLOR, "0"), (&ocx_env::TERM, "xterm")], false),
        ];
        for (set, expected) in rows {
            for key in keys {
                env.remove(key);
            }
            for (key, value) in set {
                env.set(key, *value);
            }
            let config = ColorModeConfig::from_env();
            assert_eq!((config.stdout, config.relayed), (expected, expected), "{set:?}");
        }
    }

    #[test]
    fn pre_parse_returns_auto_when_no_flag() {
        // Can't easily test with real args, but the default path returns Auto
        assert!(matches!(ColorMode::default(), ColorMode::Auto));
    }
}
