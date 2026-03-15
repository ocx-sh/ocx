// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Controls when ANSI color codes are emitted.
///
/// Implements `clap_builder::ValueEnum` for use as a CLI flag (`--color`).
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
    /// Pre-scans `std::env::args()` for `--color <value>` or `--color=<value>` and
    /// returns the corresponding [`ColorMode`].
    ///
    /// This allows setting color state *before* clap parses, so that clap's own
    /// help/error rendering respects `--color never`/`--color always`.
    pub fn from_args() -> Self {
        use clap_builder::ValueEnum;

        let mut args = std::env::args().skip(1);
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
            },
            ColorMode::Never => ColorModeConfig {
                stdout: false,
                stderr: false,
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

/// Per-stream color resolution result.
///
/// Each stream (stdout, stderr) may have a different color setting in `Auto` mode
/// because one may be a TTY while the other is piped.
#[derive(Clone, Copy, Debug)]
pub struct ColorModeConfig {
    pub stdout: bool,
    pub stderr: bool,
}

impl ColorModeConfig {
    /// Applies the env-var priority chain for `Auto` mode, with per-stream TTY fallback.
    fn from_env() -> Self {
        let enabled = 'env: {
            // NO_COLOR: any non-empty value disables color (https://no-color.org/)
            if std::env::var("NO_COLOR").is_ok_and(|v| !v.is_empty()) {
                break 'env false;
            }
            // CLICOLOR_FORCE: non-zero value forces color even without TTY
            if std::env::var("CLICOLOR_FORCE").is_ok_and(|v| v != "0" && !v.is_empty()) {
                break 'env true;
            }
            // CLICOLOR=0 disables color
            if std::env::var("CLICOLOR").is_ok_and(|v| v == "0") {
                break 'env false;
            }
            // TERM=dumb: terminal does not support escape sequences
            if std::env::var("TERM").is_ok_and(|v| v == "dumb") {
                break 'env false;
            }
            // Fall back to per-stream TTY detection
            return Self {
                stdout: console::Term::stdout().is_term(),
                stderr: console::Term::stderr().is_term(),
            };
        };

        Self {
            stdout: enabled,
            stderr: enabled,
        }
    }

    /// Sets the global `console` crate color state for both stdout and stderr.
    ///
    /// Call this once after [`ColorMode::config()`] to ensure the `console`
    /// crate's styling functions respect the resolved color setting.
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
    }

    #[test]
    fn never_disables_both_streams() {
        let config = ColorMode::Never.config();
        assert!(!config.stdout);
        assert!(!config.stderr);
    }

    #[test]
    fn pre_parse_returns_auto_when_no_flag() {
        // Can't easily test with real args, but the default path returns Auto
        assert!(matches!(ColorMode::default(), ColorMode::Auto));
    }
}
