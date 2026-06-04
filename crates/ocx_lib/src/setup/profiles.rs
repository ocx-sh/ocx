// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Profile-target detection for `ocx self setup` (plan contract 5).
//!
//! [`detect_targets`] ports the POSIX multi-profile decision tree from
//! `install.sh:744-784`: for any POSIX login shell it wires BOTH the login and
//! interactive RC files for BOTH bash and zsh, so activation fires regardless of
//! how the terminal is launched. fish, nushell, and elvish own dedicated files
//! instead of a fenced block.
//!
//! Detection reads no real environment: it operates over an injectable
//! [`HomeEnv`] so the decision tree is unit-testable without touching the
//! process environment.
//!
//! PowerShell `$PROFILE` cannot be hardcoded (OneDrive / GPO redirect the path),
//! so [`detect_powershell_profile`] asks the host itself via a subprocess. The
//! execution-policy probe [`execution_policy_is_restricted`] surfaces a
//! non-fatal advisory when a `$PROFILE` fence would be inert.

use std::path::PathBuf;

/// How a profile target carries its OCX activation payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileKind {
    /// POSIX shells (bash/zsh) — a `# >>> ocx v1 … >>>` fence around the
    /// `. "$OCX_HOME/env.sh"` source line.
    PosixFence,
    /// Elvish — a `#`-comment fence around the `eval (slurp …)` source line.
    ElvishFence,
    /// fish / nushell — a dedicated file, full-rewrite, no inline fence.
    DedicatedFile(DedicatedShell),
    /// PowerShell `$PROFILE` — a `#`-comment fence around the `. env.ps1` line.
    PowerShellFence,
}

/// Shells whose activation lives in a dedicated, fully-rewritten file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DedicatedShell {
    /// fish — `${XDG_CONFIG_HOME:-$HOME/.config}/fish/conf.d/ocx.fish`.
    Fish,
    /// nushell — `${XDG_DATA_HOME:-$HOME/.local/share}/nushell/vendor/autoload/ocx.nu`.
    Nushell,
}

/// A single profile file the orchestrator should write, with the payload kind
/// that file expects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileTarget {
    /// Absolute path of the profile file.
    pub path: PathBuf,
    /// How the activation payload is carried in this file.
    pub kind: ProfileKind,
}

/// An injectable snapshot of the environment variables that drive profile
/// detection, so [`detect_targets`] is unit-testable without reading the real
/// process environment.
#[derive(Debug, Clone)]
pub struct HomeEnv {
    /// `$HOME` — the user's home directory.
    pub home: PathBuf,
    /// `$ZDOTDIR` — zsh's RC-file root; falls back to `home` when unset.
    pub zdotdir: Option<PathBuf>,
    /// `$XDG_CONFIG_HOME` — falls back to `home/.config` when unset.
    pub xdg_config_home: Option<PathBuf>,
    /// `$XDG_DATA_HOME` — falls back to `home/.local/share` when unset.
    pub xdg_data_home: Option<PathBuf>,
    /// `$OCX_HOME` — the ocx store root (the directory the shims write into).
    pub ocx_home: PathBuf,
    /// `$SHELL` — used to pick the dedicated-file shells (fish/nu/elvish).
    pub shell: Option<String>,
}

impl HomeEnv {
    /// `$XDG_CONFIG_HOME`, or `$HOME/.config` when unset.
    fn config_home(&self) -> PathBuf {
        self.xdg_config_home
            .clone()
            .unwrap_or_else(|| self.home.join(".config"))
    }

    /// `$XDG_DATA_HOME`, or `$HOME/.local/share` when unset.
    fn data_home(&self) -> PathBuf {
        self.xdg_data_home
            .clone()
            .unwrap_or_else(|| self.home.join(".local/share"))
    }

    /// `$ZDOTDIR`, or `$HOME` when unset. Rejects `ZDOTDIR="/"` to avoid writing
    /// `/.zprofile` — a filesystem-root escape (CWE-22), ported from
    /// `install.sh:776-781`.
    fn zsh_dir(&self) -> PathBuf {
        match &self.zdotdir {
            Some(dir) if dir != std::path::Path::new("/") => dir.clone(),
            Some(_) => {
                tracing::warn!("ZDOTDIR is '/' — refusing to write under /; falling back to $HOME");
                self.home.clone()
            }
            None => self.home.clone(),
        }
    }

    /// Lowercased basename of `$SHELL` (`/usr/bin/fish` → `fish`); `sh` when unset.
    fn shell_name(&self) -> String {
        self.shell
            .as_deref()
            .and_then(|shell| shell.rsplit(['/', '\\']).next())
            .filter(|name| !name.is_empty())
            .unwrap_or("sh")
            .to_ascii_lowercase()
    }
}

/// Resolve the profile files a `ocx self setup` run should target, in write
/// order, from an injectable [`HomeEnv`].
///
/// fish, nushell, and elvish each own a dedicated activation file. Every other
/// POSIX login shell wires BOTH login and interactive RC files for BOTH bash and
/// zsh (`install.sh:744-784`): the shared `env.sh` detects the running shell, so
/// a copy that fires in an unused shell is a harmless no-op. PowerShell `$PROFILE`
/// is intentionally absent here — it requires a subprocess probe
/// ([`detect_powershell_profile`]).
pub fn detect_targets(home_env: &HomeEnv) -> Vec<ProfileTarget> {
    match home_env.shell_name().as_str() {
        // Dedicated-file shells opt out of the shared bash+zsh wiring.
        "fish" => {
            let path = home_env.config_home().join("fish").join("conf.d").join("ocx.fish");
            return vec![ProfileTarget {
                path,
                kind: ProfileKind::DedicatedFile(DedicatedShell::Fish),
            }];
        }
        "nu" => {
            let path = home_env
                .data_home()
                .join("nushell")
                .join("vendor")
                .join("autoload")
                .join("ocx.nu");
            return vec![ProfileTarget {
                path,
                kind: ProfileKind::DedicatedFile(DedicatedShell::Nushell),
            }];
        }
        "elvish" => {
            let path = home_env.config_home().join("elvish").join("rc.elv");
            return vec![ProfileTarget {
                path,
                kind: ProfileKind::ElvishFence,
            }];
        }
        _ => {}
    }

    // Shared bash+zsh wiring for any POSIX login shell. Both source the same
    // env.sh; the runtime shell detection inside it picks the completion backend.
    let mut targets = Vec::with_capacity(4);

    // bash: login (.bash_profile, or .profile when absent — also the generic
    // POSIX login file for sh/dash/ksh) + interactive (.bashrc).
    let bash_login = if home_env.home.join(".bash_profile").is_file() {
        home_env.home.join(".bash_profile")
    } else {
        home_env.home.join(".profile")
    };
    targets.push(ProfileTarget {
        path: bash_login,
        kind: ProfileKind::PosixFence,
    });
    targets.push(ProfileTarget {
        path: home_env.home.join(".bashrc"),
        kind: ProfileKind::PosixFence,
    });

    // zsh: login (.zprofile) + interactive (.zshrc), under $ZDOTDIR.
    let zsh_dir = home_env.zsh_dir();
    targets.push(ProfileTarget {
        path: zsh_dir.join(".zprofile"),
        kind: ProfileKind::PosixFence,
    });
    targets.push(ProfileTarget {
        path: zsh_dir.join(".zshrc"),
        kind: ProfileKind::PosixFence,
    });

    targets
}

/// Ask a PowerShell host for the current-user, all-hosts `$PROFILE` path.
///
/// The path is never hardcoded: OneDrive and Group Policy redirect the profile
/// directory, so only the host knows the real location. Spawns `pwsh` first,
/// then falls back to `powershell`; returns `None` when neither host is present.
pub async fn detect_powershell_profile() -> Option<PathBuf> {
    for host in ["pwsh", "powershell"] {
        if let Some(path) = query_powershell_profile(host).await {
            return Some(path);
        }
    }
    None
}

/// Run one PowerShell host and parse `$PROFILE.CurrentUserAllHosts` from stdout.
///
/// Returns `None` on any failure (host absent, exec error, non-zero exit, empty
/// output) so detection degrades to "no PowerShell profile" rather than failing
/// setup.
async fn query_powershell_profile(host: &str) -> Option<PathBuf> {
    let output = tokio::process::Command::new(host)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$PROFILE.CurrentUserAllHosts",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        tracing::debug!("{host} exited non-zero while resolving $PROFILE");
        return None;
    }

    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        return None;
    }
    Some(PathBuf::from(path))
}

/// Whether the current-user PowerShell execution policy is `Restricted`.
///
/// A `Restricted` policy makes a `$PROFILE` fence inert, so the orchestrator
/// emits a non-fatal advisory to relax it. Returns `false` on non-Windows hosts,
/// when no PowerShell host is present, or on any subprocess failure — the probe
/// never fails setup, and it never auto-changes the policy (a user security
/// decision).
pub async fn execution_policy_is_restricted() -> bool {
    for host in ["pwsh", "powershell"] {
        if let Some(policy) = query_execution_policy(host).await {
            return policy == "Restricted";
        }
    }
    false
}

/// Run one PowerShell host's `Get-ExecutionPolicy -Scope CurrentUser` and return
/// the trimmed stdout, or `None` when the host is absent or the probe fails.
async fn query_execution_policy(host: &str) -> Option<String> {
    let output = tokio::process::Command::new(host)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Get-ExecutionPolicy -Scope CurrentUser",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        tracing::debug!("{host} exited non-zero while resolving execution policy");
        return None;
    }

    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `HomeEnv` with the given shell and no XDG / ZDOTDIR overrides.
    fn home_env(home: &str, shell: &str) -> HomeEnv {
        HomeEnv {
            home: PathBuf::from(home),
            zdotdir: None,
            xdg_config_home: None,
            xdg_data_home: None,
            ocx_home: PathBuf::from(home).join(".ocx"),
            shell: Some(shell.to_string()),
        }
    }

    /// Collect just the paths from a target list, for order-sensitive assertions.
    fn paths(targets: &[ProfileTarget]) -> Vec<PathBuf> {
        targets.iter().map(|target| target.path.clone()).collect()
    }

    // ── POSIX multi-profile ─────────────────────────────────────────

    #[test]
    fn posix_shell_wires_both_bash_and_zsh_login_and_interactive() {
        let env = home_env("/home/dev", "/usr/bin/bash");
        let targets = detect_targets(&env);

        // .bash_profile is absent on disk → .profile is the bash login target.
        assert_eq!(
            paths(&targets),
            vec![
                PathBuf::from("/home/dev/.profile"),
                PathBuf::from("/home/dev/.bashrc"),
                PathBuf::from("/home/dev/.zprofile"),
                PathBuf::from("/home/dev/.zshrc"),
            ]
        );
        assert!(targets.iter().all(|t| t.kind == ProfileKind::PosixFence));
    }

    #[test]
    fn unknown_posix_shell_falls_back_to_bash_zsh_wiring() {
        // dash/ksh/sh and friends all take the shared POSIX tree.
        let env = home_env("/home/dev", "/bin/dash");
        assert_eq!(
            paths(&detect_targets(&env)),
            vec![
                PathBuf::from("/home/dev/.profile"),
                PathBuf::from("/home/dev/.bashrc"),
                PathBuf::from("/home/dev/.zprofile"),
                PathBuf::from("/home/dev/.zshrc"),
            ]
        );
    }

    #[test]
    fn missing_shell_defaults_to_posix_wiring() {
        let env = HomeEnv {
            home: PathBuf::from("/home/dev"),
            zdotdir: None,
            xdg_config_home: None,
            xdg_data_home: None,
            ocx_home: PathBuf::from("/home/dev/.ocx"),
            shell: None,
        };
        assert_eq!(
            paths(&detect_targets(&env)),
            vec![
                PathBuf::from("/home/dev/.profile"),
                PathBuf::from("/home/dev/.bashrc"),
                PathBuf::from("/home/dev/.zprofile"),
                PathBuf::from("/home/dev/.zshrc"),
            ]
        );
    }

    // ── ZDOTDIR handling ────────────────────────────────────────────

    #[test]
    fn zdotdir_override_relocates_zsh_targets_only() {
        let mut env = home_env("/home/dev", "/usr/bin/zsh");
        env.zdotdir = Some(PathBuf::from("/home/dev/.config/zsh"));
        let detected = paths(&detect_targets(&env));

        assert!(detected.contains(&PathBuf::from("/home/dev/.config/zsh/.zprofile")));
        assert!(detected.contains(&PathBuf::from("/home/dev/.config/zsh/.zshrc")));
        // bash targets stay anchored to $HOME.
        assert!(detected.contains(&PathBuf::from("/home/dev/.profile")));
        assert!(detected.contains(&PathBuf::from("/home/dev/.bashrc")));
    }

    #[test]
    fn zdotdir_root_is_rejected_and_falls_back_to_home() {
        let mut env = home_env("/home/dev", "/usr/bin/zsh");
        env.zdotdir = Some(PathBuf::from("/"));
        let detected = paths(&detect_targets(&env));

        // No target may live directly under the filesystem root (CWE-22 guard).
        assert!(
            !detected.contains(&PathBuf::from("/.zprofile")),
            "ZDOTDIR=/ must not write /.zprofile"
        );
        assert!(!detected.contains(&PathBuf::from("/.zshrc")));
        assert!(detected.contains(&PathBuf::from("/home/dev/.zprofile")));
        assert!(detected.contains(&PathBuf::from("/home/dev/.zshrc")));
    }

    // ── dedicated-file shells ───────────────────────────────────────

    #[test]
    fn fish_targets_conf_d_with_default_xdg_config_home() {
        let env = home_env("/home/dev", "/usr/bin/fish");
        let targets = detect_targets(&env);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].path, PathBuf::from("/home/dev/.config/fish/conf.d/ocx.fish"));
        assert_eq!(targets[0].kind, ProfileKind::DedicatedFile(DedicatedShell::Fish));
    }

    #[test]
    fn fish_honors_xdg_config_home_override() {
        let mut env = home_env("/home/dev", "/usr/bin/fish");
        env.xdg_config_home = Some(PathBuf::from("/cfg"));
        let targets = detect_targets(&env);
        assert_eq!(targets[0].path, PathBuf::from("/cfg/fish/conf.d/ocx.fish"));
    }

    #[test]
    fn nushell_targets_vendor_autoload_with_default_xdg_data_home() {
        let env = home_env("/home/dev", "/usr/bin/nu");
        let targets = detect_targets(&env);
        assert_eq!(targets.len(), 1);
        assert_eq!(
            targets[0].path,
            PathBuf::from("/home/dev/.local/share/nushell/vendor/autoload/ocx.nu")
        );
        assert_eq!(targets[0].kind, ProfileKind::DedicatedFile(DedicatedShell::Nushell));
    }

    #[test]
    fn nushell_honors_xdg_data_home_override() {
        let mut env = home_env("/home/dev", "/usr/bin/nu");
        env.xdg_data_home = Some(PathBuf::from("/data"));
        let targets = detect_targets(&env);
        assert_eq!(targets[0].path, PathBuf::from("/data/nushell/vendor/autoload/ocx.nu"));
    }

    #[test]
    fn elvish_targets_rc_elv_with_elvish_fence_kind() {
        let env = home_env("/home/dev", "/usr/bin/elvish");
        let targets = detect_targets(&env);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].path, PathBuf::from("/home/dev/.config/elvish/rc.elv"));
        assert_eq!(targets[0].kind, ProfileKind::ElvishFence);
    }

    // ── HOME with spaces ────────────────────────────────────────────

    #[test]
    fn home_with_spaces_is_joined_safely() {
        let env = home_env("/home/dev user", "/usr/bin/bash");
        let detected = paths(&detect_targets(&env));
        assert!(detected.contains(&PathBuf::from("/home/dev user/.profile")));
        assert!(detected.contains(&PathBuf::from("/home/dev user/.bashrc")));
        // The space lives in one path component, never split across two.
        let bashrc = detected
            .iter()
            .find(|p| p.file_name() == Some(std::ffi::OsStr::new(".bashrc")))
            .expect("bashrc target present");
        assert_eq!(bashrc.parent(), Some(std::path::Path::new("/home/dev user")));
    }

    #[test]
    fn fish_path_with_spaces_in_home_stays_single_component() {
        let env = home_env("/home/dev user", "/usr/bin/fish");
        let targets = detect_targets(&env);
        assert_eq!(
            targets[0].path,
            PathBuf::from("/home/dev user/.config/fish/conf.d/ocx.fish")
        );
    }

    // ── shell-name parsing ──────────────────────────────────────────

    #[test]
    fn shell_name_is_basename_lowercased() {
        let env = home_env("/home/dev", "/opt/homebrew/bin/FISH");
        // Uppercase basename normalizes to the fish dedicated file.
        let targets = detect_targets(&env);
        assert_eq!(targets[0].kind, ProfileKind::DedicatedFile(DedicatedShell::Fish));
    }
}
