mod profile_builder;
pub use profile_builder::ProfileBuilder;

use crate::{Error, env, log};

/// List of supported shells for OCX to generate scripts for, ie. profiles or auto-completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    /// Almquist `SHell` (ash)
    Ash,
    /// Korn `SHell` (ksh)
    Ksh,
    /// `Dash` shell, a POSIX-compliant shell often used in Debian-based systems
    Dash,
    /// Bourne Again `SHell` (bash)
    Bash,
    /// Elvish shell
    Elvish,
    /// Friendly Interactive `SHell` (fish)
    Fish,
    /// Windows `Batch` shell
    Batch,
    /// `PowerShell`
    PowerShell,
    /// Z `SHell` (zsh)
    Zsh,
}

impl Shell {
    /// Tries to resolve the current shell by checking the `SHELL` environment variable and then the parent processes.
    pub fn detect() -> Option<Self> {
        Self::from_process().or_else(Self::from_env)
    }

    /// Tries to resolve the shell from a given path, which can be a full path or just a filename.
    pub fn from_path(path: impl AsRef<std::path::Path>) -> Option<Self> {
        let path = path.as_ref();
        log::trace!("Detecting shell from path: {}", path.display());

        // If the path is a symlink, we try to resolve it to get the actual shell, before falling back to filename matching.
        if path.is_symlink() {
            log::trace!("Shell is a symlink, attempting to resolve it...");
            if let Ok(canonical_path) = std::fs::read_link(path)
                && let Some(shell) = Self::from_path(canonical_path) {
                    return Some(shell);
                }
        }

        // Extracts the filename from the path and matches it against known shell names.
        let filename = path.file_stem()?.to_str()?;
        match filename {
            "ash" | "busybox" => Some(Self::Ash),
            "ksh" | "ksh86" | "ksh88" | "ksh93" => Some(Self::Ksh),
            "dash" => Some(Self::Dash),
            "bash" => Some(Self::Bash),
            "elvish" => Some(Self::Elvish),
            "fish" => Some(Self::Fish),
            "cmd" => Some(Self::Batch),
            "powershell" | "powershell_ise" | "pwsh" => Some(Self::PowerShell),
            "zsh" => Some(Self::Zsh),
            _ => None,
        }
    }

    /// Tries to resolve the shell from the `SHELL` environment variable.
    pub fn from_env() -> Option<Self> {
        std::env::var("SHELL").ok().and_then(Self::from_path)
    }

    /// Tries to resolve the shell by inspecting the current and parent process information.
    pub fn from_process() -> Option<Self> {
        fn try_process_id(pid: sysinfo::Pid, system : &sysinfo::System) -> Option<Shell> {
            log::trace!("Checking process with PID {} for shell information...", pid);
            if let Some(process) = system.process(pid)
                && let Some(shell) = Shell::from_path(process.name()) {
                    return Some(shell);
                }
            #[cfg(unix)]
            if let Some(shell) = Shell::from_path(format!("/proc/{}/exe", pid)) {
                return Some(shell);
            }
            None
        }

        let system = sysinfo::System::new_with_specifics(
            sysinfo::RefreshKind::default().with_processes(sysinfo::ProcessRefreshKind::default()),
        );
        let mut current_pid = sysinfo::get_current_pid().ok()?;
        if let Some(shell) = try_process_id(current_pid, &system) {
            return Some(shell);
        }
        while let Some(parent_pid) = system.process(current_pid)?.parent() {
            if let Some(shell) = try_process_id(parent_pid, &system) {
                return Some(shell);
            }
            current_pid = parent_pid;
        }
        None
    }

    pub fn profile_builder(&self, content: impl Into<std::path::PathBuf>) -> ProfileBuilder {
        ProfileBuilder::new(content.into(), *self)
    }

    pub fn export_path(self, key: impl AsRef<str>, value: impl AsRef<str>) -> String {
        let (key, value) = (key.as_ref(), self.escape_value(value));
        let separator = env::PATH_SEPARATOR;
        match self {
            Self::Ash | Self::Ksh | Self::Dash | Self::Bash | Self::Zsh => {
                format!("export {key}=\"{value}{separator}${{{key}}}\"")
            }
            Self::Fish => format!("set -x {key} \"{value}{separator}\" ${key}"),
            Self::PowerShell => format!("$env:{key} = \"{value}{separator}$env:{key}\""),
            Self::Batch => format!("SET \"{key}={value}{separator}%{key}%\""),
            Self::Elvish => format!("set E:{key} = \"{value}{separator}\"$E:{key}"),
        }
    }

    pub fn export_constant(self, key: impl AsRef<str>, value: impl AsRef<str>) -> String {
        let (key, value) = (key.as_ref(), self.escape_value(value.as_ref()));
        match self {
            Self::Ash | Self::Ksh | Self::Dash | Self::Bash | Self::Zsh => format!("export {key}=\"{value}\""),
            Self::Fish => format!("set -x {key} \"{value}\""),
            Self::PowerShell => format!("$env:{key} = \"{value}\""),
            Self::Batch => format!("SET \"{key}={value}\""),
            Self::Elvish => format!("set E:{key} = \"{value}\""),
        }
    }

    pub fn escape_value(self, value: impl AsRef<str>) -> String {
        let value = value.as_ref();
        match self {
            Self::Ash | Self::Ksh | Self::Dash | Self::Bash | Self::Zsh => value.replace('"', "\\\""),
            Self::Fish => value.replace('"', "\\\""),
            Self::PowerShell => value.replace('"', "`\""),
            Self::Batch => value
                .replace('%', "%%")
                .replace('^', "^^")
                .replace('&', "^&")
                .replace('<', "^<")
                .replace('>', "^>")
                .replace('|', "^|"),
            Self::Elvish => value.replace('"', "\\\""),
        }
    }
}

impl std::fmt::Display for Shell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Ash => "Ash",
            Self::Ksh => "Ksh",
            Self::Dash => "Dash",
            Self::Bash => "Bash",
            Self::Elvish => "Elvish",
            Self::Fish => "Fish",
            Self::Batch => "Batch",
            Self::PowerShell => "PowerShell",
            Self::Zsh => "Zsh",
        };
        write!(f, "{}", name)
    }
}

impl clap_builder::ValueEnum for Shell {
    fn value_variants<'a>() -> &'a [Self] {
        &[
            Self::Ash,
            Self::Ksh,
            Self::Dash,
            Self::Bash,
            Self::Elvish,
            Self::Fish,
            Self::Batch,
            Self::PowerShell,
            Self::Zsh,
        ]
    }

    fn to_possible_value(&self) -> Option<clap_builder::builder::PossibleValue> {
        use clap_builder::builder::PossibleValue;

        Some(match self {
            Self::Ash => PossibleValue::new("ash"),
            Self::Ksh => PossibleValue::new("ksh"),
            Self::Dash => PossibleValue::new("dash"),
            Self::Bash => PossibleValue::new("bash"),
            Self::Elvish => PossibleValue::new("elvish"),
            Self::Fish => PossibleValue::new("fish"),
            Self::Batch => PossibleValue::new("batch"),
            Self::PowerShell => PossibleValue::new("powershell"),
            Self::Zsh => PossibleValue::new("zsh"),
        })
    }
}

impl TryInto<clap_complete::Shell> for Shell {
    type Error = Error;

    fn try_into(self) -> Result<clap_complete::Shell, Self::Error> {
        match self {
            Self::Bash => Ok(clap_complete::Shell::Bash),
            Self::Elvish => Ok(clap_complete::Shell::Elvish),
            Self::Fish => Ok(clap_complete::Shell::Fish),
            Self::PowerShell => Ok(clap_complete::Shell::PowerShell),
            Self::Zsh => Ok(clap_complete::Shell::Zsh),
            _ => Err(Error::UnsupportedClapShell(self)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test;

    #[test]
    fn test_from_path() {
        assert_eq!(Shell::from_path("/bin/ash"), Some(Shell::Ash));
        assert_eq!(Shell::from_path("/bin/busybox"), Some(Shell::Ash));
        assert_eq!(Shell::from_path("/bin/ksh"), Some(Shell::Ksh));
        assert_eq!(Shell::from_path("/usr/bin/dash"), Some(Shell::Dash));
        assert_eq!(Shell::from_path("/bin/bash"), Some(Shell::Bash));
        assert_eq!(Shell::from_path("/usr/bin/fish"), Some(Shell::Fish));
        assert_eq!(Shell::from_path("C:/Windows/System32/cmd.exe"), Some(Shell::Batch));
        assert_eq!(
            Shell::from_path("C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe"),
            Some(Shell::PowerShell)
        );
        assert_eq!(
            Shell::from_path("C:/Windows/System32/WindowsPowerShell/v1.0/pwsh.exe"),
            Some(Shell::PowerShell)
        );
        assert_eq!(Shell::from_path("/bin/zsh"), Some(Shell::Zsh));
        assert_eq!(Shell::from_path("/bin/unknown"), None);
    }

    #[test]
    fn test_from_env() {
        test::env::lock!();
        unsafe {
            std::env::set_var("SHELL", "/bin/bash");
        }
        assert_eq!(Shell::from_env(), Some(Shell::Bash));
        unsafe {
            std::env::set_var("SHELL", "/usr/bin/fish");
        }
        assert_eq!(Shell::from_env(), Some(Shell::Fish));
        unsafe {
            std::env::remove_var("SHELL");
        }
        assert_eq!(Shell::from_env(), None);
    }

    #[test]
    fn test_from_parent_process() {
        let shell = Shell::from_process();
        println!("Detected shell from parent process: {:?}", shell);
    }
}
