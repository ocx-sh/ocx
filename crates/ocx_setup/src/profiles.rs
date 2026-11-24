// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Profile-target detection for `ocx self setup`.

use std::path::{Component, Path, PathBuf};

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

/// An injectable snapshot of the environment variables that drive profile detection.
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

    /// `$ZDOTDIR`, or `$HOME` when unset or unsafe.
    fn zsh_dir(&self) -> PathBuf {
        match &self.zdotdir {
            Some(dir) if Self::is_safe_zdotdir(dir) => dir.clone(),
            Some(dir) => {
                tracing::warn!(
                    "ZDOTDIR {dir:?} is unsafe (non-absolute, contains '..', or is '/'); \
                     refusing to write under it and falling back to $HOME"
                );
                self.home.clone()
            }
            None => self.home.clone(),
        }
    }

    /// A relative, root or `..`-bearing `$ZDOTDIR` would redirect the auto-detected zsh RC writes (CWE-22).
    fn is_safe_zdotdir(dir: &Path) -> bool {
        dir.is_absolute()
            && dir != Path::new("/")
            && !dir.components().any(|component| component == Component::ParentDir)
    }

    /// Lowercased basename of `$SHELL` without a trailing `.exe` (`/usr/bin/fish` → `fish`); `sh` when unset.
    fn shell_name(&self) -> String {
        let name = self
            .shell
            .as_deref()
            .and_then(|shell| shell.rsplit(['/', '\\']).next())
            .filter(|name| !name.is_empty())
            .unwrap_or("sh")
            .to_ascii_lowercase();
        name.strip_suffix(".exe").map(str::to_owned).unwrap_or(name)
    }
}

/// The kind of machine `ocx self setup` runs on, which decides the profile targets and the shim text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    /// Linux, macOS and every other non-Windows host.
    Unix,
    /// A Windows host: native shells and any MSYS2, Cygwin or Git Bash runtime on it.
    Windows,
}

impl Host {
    /// The host this process runs on.
    pub fn current() -> Self {
        if cfg!(windows) { Self::Windows } else { Self::Unix }
    }
}

/// An injectable snapshot of what a Windows host adds to profile detection.
#[derive(Debug, Clone, Default)]
pub struct WindowsEnv {
    /// `%USERPROFILE%` — the profile directory, which is where the store lives.
    pub userprofile: Option<PathBuf>,
    /// `%APPDATA%` — where native nushell and elvish read their configuration.
    pub appdata: Option<PathBuf>,
    /// `nu.exe` is on `PATH`.
    pub nushell: bool,
    /// `elvish.exe` is on `PATH`.
    pub elvish: bool,
    /// `fish.exe` is on `PATH`.
    pub fish: bool,
    /// MSYS2 and Cygwin home directories that exist at their default install locations.
    pub posix_homes: Vec<PathBuf>,
}

/// The absolute entries of a `PATH` value. A relative or empty entry means the current directory,
/// where a checked-out `nu.exe` would otherwise count as an installed shell.
fn absolute_path_entries(path: &std::ffi::OsStr) -> Vec<PathBuf> {
    std::env::split_paths(path).filter(|dir| dir.is_absolute()).collect()
}

/// `dir` when it is an absolute path; anything else is ignored with a warning, as an unsafe `ZDOTDIR` is.
fn absolute_dir(variable: &str, dir: Option<&PathBuf>) -> Option<PathBuf> {
    let dir = dir?;
    if dir.is_absolute() {
        return Some(dir.clone());
    }
    tracing::warn!("{variable} {dir:?} is not an absolute path; ignoring it for profile detection");
    None
}

/// Whether `file` sits directly in one of `dirs`, as answered by `exists`.
fn on_path(dirs: &[PathBuf], file: &str, exists: impl Fn(&Path) -> bool) -> bool {
    dirs.iter().any(|dir| exists(&dir.join(file)))
}

impl WindowsEnv {
    /// Read `USERPROFILE`, `APPDATA` and the shells found on `PATH` from the process environment.
    pub async fn from_environment() -> Self {
        const SHELLS: [&str; 3] = ["nu.exe", "elvish.exe", "fish.exe"];
        let dirs = ocx_env::PATH
            .get_raw()
            .map(|path| absolute_path_entries(&path))
            .unwrap_or_default();
        let mut found = std::collections::HashSet::new();
        for dir in &dirs {
            for shell in SHELLS {
                let candidate = dir.join(shell);
                if ocx_util::fs::path_exists_lossy(&candidate).await {
                    found.insert(candidate);
                }
            }
        }
        let userprofile = ocx_env::USERPROFILE.get().map(PathBuf::from);
        let mut posix_homes = Vec::new();
        for candidate in userprofile.as_deref().map(posix_home_candidates).unwrap_or_default() {
            if ocx_util::fs::path_exists_lossy(&candidate).await {
                posix_homes.push(candidate);
            }
        }
        Self {
            posix_homes,
            ..Self::from_parts(userprofile, ocx_env::APPDATA.get().map(PathBuf::from), &dirs, |path| {
                found.contains(path)
            })
        }
    }

    /// The snapshot for the given variables and `PATH` directories, probing shells through `exists`.
    fn from_parts(
        userprofile: Option<PathBuf>,
        appdata: Option<PathBuf>,
        dirs: &[PathBuf],
        exists: impl Fn(&Path) -> bool,
    ) -> Self {
        Self {
            userprofile: userprofile.filter(|path| !path.as_os_str().is_empty()),
            appdata: appdata.filter(|path| !path.as_os_str().is_empty()),
            nushell: on_path(dirs, "nu.exe", &exists),
            elvish: on_path(dirs, "elvish.exe", &exists),
            fish: on_path(dirs, "fish.exe", &exists),
            posix_homes: Vec::new(),
        }
    }
}

/// Default MSYS2 and Cygwin home directories for the user that owns `userprofile`, on its drive.
///
/// shortcut: assumes the default install roots and a home named after the profile directory;
/// a custom install or a renamed account gets no hint.
fn posix_home_candidates(userprofile: &Path) -> Vec<PathBuf> {
    let (Some(user), Some(root)) = (userprofile.file_name(), userprofile.ancestors().last()) else {
        return Vec::new();
    };
    ["msys64", "cygwin64", "cygwin"]
        .iter()
        .map(|install| root.join(install).join("home").join(user))
        .collect()
}

/// The MSYS2 or Cygwin homes in `windows` that setup writes no rc file under: neither `HOME` nor
/// `%USERPROFILE%`. A bash or zsh using one as `HOME` never reads what this run writes.
pub fn unreached_posix_homes<'a>(home_env: &HomeEnv, windows: &'a WindowsEnv) -> Vec<&'a Path> {
    let written = [Some(home_env.home.as_path()), windows.userprofile.as_deref()];
    windows
        .posix_homes
        .iter()
        .filter(|home| {
            !written
                .iter()
                .flatten()
                .any(|dir| ocx_util::path::path_element_eq(home.as_os_str(), dir.as_os_str(), true))
        })
        .map(PathBuf::as_path)
        .collect()
}

/// The POSIX rc files of one home, in write order.
///
/// `.profile` stays wired even beside `.bash_profile`, or dash/ksh/sh login shells lose activation.
/// `.bash_profile` shadows `.profile` for bash, so it is wired too when present.
fn posix_family(home: &Path, zsh_dir: &Path) -> Vec<ProfileTarget> {
    let mut targets = Vec::with_capacity(5);
    let posix = |path: PathBuf| ProfileTarget {
        path,
        kind: ProfileKind::PosixFence,
    };
    targets.push(posix(home.join(".profile")));
    if home.join(".bash_profile").is_file() {
        targets.push(posix(home.join(".bash_profile")));
    }
    targets.push(posix(home.join(".bashrc")));
    targets.push(posix(zsh_dir.join(".zprofile")));
    targets.push(posix(zsh_dir.join(".zshrc")));
    targets
}

/// Profile targets on a Windows host: every shell found, whatever `$SHELL` says.
///
/// The POSIX family goes under `HOME` and, when the profile is a different directory, under the
/// profile too: MSYS2 and Cygwin keep their own home, Git Bash uses the profile as `HOME`.
fn detect_windows_targets(home_env: &HomeEnv, windows: &WindowsEnv) -> Vec<ProfileTarget> {
    let mut targets = posix_family(&home_env.home, &home_env.zsh_dir());
    if let Some(profile) = &windows.userprofile
        && !ocx_util::path::path_element_eq(profile.as_os_str(), home_env.home.as_os_str(), true)
    {
        targets.extend(posix_family(profile, profile));
    }

    let appdata = || absolute_dir("APPDATA", windows.appdata.as_ref());
    if windows.nushell {
        let data = absolute_dir("XDG_DATA_HOME", home_env.xdg_data_home.as_ref());
        if let Some(dir) = data.or_else(appdata).map(|dir| dir.join("nushell")) {
            targets.push(ProfileTarget {
                path: dir.join("vendor").join("autoload").join("ocx.nu"),
                kind: ProfileKind::DedicatedFile(DedicatedShell::Nushell),
            });
        }
    }
    let xdg_config = (windows.elvish || windows.fish)
        .then(|| absolute_dir("XDG_CONFIG_HOME", home_env.xdg_config_home.as_ref()))
        .flatten();
    if windows.elvish
        && let Some(dir) = xdg_config.clone().or_else(appdata)
    {
        targets.push(ProfileTarget {
            path: dir.join("elvish").join("rc.elv"),
            kind: ProfileKind::ElvishFence,
        });
    }
    if windows.fish {
        let config = xdg_config.unwrap_or_else(|| home_env.home.join(".config"));
        targets.push(ProfileTarget {
            path: config.join("fish").join("conf.d").join("ocx.fish"),
            kind: ProfileKind::DedicatedFile(DedicatedShell::Fish),
        });
    }
    targets
}

/// Resolve the profile files a `ocx self setup` run should target on a unix host, in write order.
///
/// PowerShell `$PROFILE` is not included; see [`detect_powershell_profile`].
pub fn detect_targets(home_env: &HomeEnv) -> Vec<ProfileTarget> {
    detect_targets_on(Host::Unix, home_env, &WindowsEnv::default())
}

/// [`detect_targets`] for `host`; `windows` is read only on a Windows host.
pub fn detect_targets_on(host: Host, home_env: &HomeEnv, windows: &WindowsEnv) -> Vec<ProfileTarget> {
    if host == Host::Windows {
        return detect_windows_targets(home_env, windows);
    }
    match home_env.shell_name().as_str() {
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
    posix_family(&home_env.home, &home_env.zsh_dir())
}

/// Ask a PowerShell host for the current-user, all-hosts `$PROFILE` path.
///
/// Never hardcoded: OneDrive and Group Policy redirect the profile directory.
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
/// Returns `None` on any failure, so detection never fails setup.
#[expect(
    clippy::disallowed_types,
    reason = "shell detection asks a candidate shell what it is"
)]
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
/// `false` when no host answers; the probe never fails setup and never changes
/// the policy, which is the user's security decision.
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
#[expect(
    clippy::disallowed_types,
    reason = "shell detection asks a candidate shell what it is"
)]
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
    fn bash_profile_present_also_wires_dot_profile_for_non_bash_login() {
        // Regression: a distro skel that ships ~/.bash_profile (e.g. Fedora) must
        // not strand dash/ksh/sh login shells. Those read ~/.profile and NEVER
        // ~/.bash_profile, so the login fence has to land in ~/.profile too — not
        // only in ~/.bash_profile because it happens to exist. A real temp HOME is
        // used because the `.bash_profile` probe (`is_file`) reads the filesystem.
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().to_path_buf();
        std::fs::write(home.join(".bash_profile"), "# pre-existing skel\n").expect("seed .bash_profile");

        let env = HomeEnv {
            home: home.clone(),
            zdotdir: None,
            xdg_config_home: None,
            xdg_data_home: None,
            ocx_home: home.join(".ocx"),
            shell: Some("/bin/dash".to_string()),
        };
        let detected = paths(&detect_targets(&env));

        assert!(
            detected.contains(&home.join(".profile")),
            "dash/ksh/sh login reads ~/.profile — it must be wired even when ~/.bash_profile exists: {detected:?}"
        );
        assert!(
            detected.contains(&home.join(".bash_profile")),
            "bash login still gets ~/.bash_profile additionally: {detected:?}"
        );
        // Generic ~/.profile must precede the bash-specific ~/.bash_profile.
        let profile_index = detected
            .iter()
            .position(|path| path == &home.join(".profile"))
            .expect("profile present");
        let bash_profile_index = detected
            .iter()
            .position(|path| path == &home.join(".bash_profile"))
            .expect("bash_profile present");
        assert!(
            profile_index < bash_profile_index,
            "generic ~/.profile must come before ~/.bash_profile: {detected:?}"
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

    // ZDOTDIR is a zsh-only (Unix) concept and the assertions pin a POSIX
    // `/home/dev/...` target layout; on Windows the path joins normalize
    // differently, so this is gated to unix (zsh profile relocation is not a
    // Windows scenario).
    #[cfg(unix)]
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

    #[test]
    fn zdotdir_relative_is_rejected_and_falls_back_to_home() {
        let mut env = home_env("/home/dev", "/usr/bin/zsh");
        env.zdotdir = Some(PathBuf::from("relative/zsh"));
        let detected = paths(&detect_targets(&env));

        // A relative ZDOTDIR must not be trusted as a write root (CWE-22).
        assert!(
            !detected.iter().any(|path| path.starts_with("relative")),
            "relative ZDOTDIR must not be trusted: {detected:?}"
        );
        assert!(detected.contains(&PathBuf::from("/home/dev/.zprofile")));
        assert!(detected.contains(&PathBuf::from("/home/dev/.zshrc")));
    }

    #[test]
    fn zdotdir_with_parent_escape_is_rejected_and_falls_back_to_home() {
        let mut env = home_env("/home/dev", "/usr/bin/zsh");
        env.zdotdir = Some(PathBuf::from("/home/dev/../../etc/zsh"));
        let detected = paths(&detect_targets(&env));

        // A `..`-escaping ZDOTDIR must not redirect the auto-detect write path.
        assert!(
            !detected.iter().any(|path| path.to_string_lossy().contains("..")),
            "'..'-escaping ZDOTDIR must not be trusted: {detected:?}"
        );
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

    #[test]
    fn shell_name_drops_a_trailing_exe_on_every_host() {
        for shell in ["fish.exe", r"C:\msys64\usr\bin\FISH.EXE"] {
            let env = home_env("/home/dev", shell);
            let targets = detect_targets(&env);
            assert_eq!(
                targets[0].kind,
                ProfileKind::DedicatedFile(DedicatedShell::Fish),
                "{shell}"
            );
        }
    }

    // ── Windows host ────────────────────────────────────────────────

    /// An absolute path on whichever host runs the test.
    fn abs(name: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!(r"C:\{name}"))
        } else {
            PathBuf::from(format!("/{name}"))
        }
    }

    fn windows_home(home: &str) -> HomeEnv {
        HomeEnv {
            home: PathBuf::from(home),
            zdotdir: None,
            xdg_config_home: None,
            xdg_data_home: None,
            ocx_home: PathBuf::from(home).join(".ocx"),
            shell: None,
        }
    }

    fn windows_env(userprofile: &str) -> WindowsEnv {
        WindowsEnv {
            userprofile: Some(PathBuf::from(userprofile)),
            appdata: Some(abs("appdata")),
            ..WindowsEnv::default()
        }
    }

    fn posix_names(home: &Path) -> Vec<PathBuf> {
        [".profile", ".bashrc", ".zprofile", ".zshrc"]
            .iter()
            .map(|name| home.join(name))
            .collect()
    }

    #[test]
    fn windows_without_extra_shells_wires_only_the_posix_family() {
        let env = windows_home(r"C:\Users\u");
        let targets = detect_targets_on(Host::Windows, &env, &windows_env(r"C:\Users\u"));
        assert_eq!(paths(&targets), posix_names(Path::new(r"C:\Users\u")));
        assert!(targets.iter().all(|target| target.kind == ProfileKind::PosixFence));
    }

    #[test]
    fn git_bash_home_spelled_differently_is_one_posix_set() {
        let env = windows_home(r"C:\Users\u");
        let detected = paths(&detect_targets_on(Host::Windows, &env, &windows_env("c:/users/U")));
        assert_eq!(detected, posix_names(Path::new(r"C:\Users\u")));
    }

    #[test]
    fn msys_and_cygwin_homes_get_a_second_posix_set_under_the_profile() {
        for home in [r"C:\msys64\home\u", r"C:\cygwin64\home\u"] {
            let env = windows_home(home);
            let detected = paths(&detect_targets_on(Host::Windows, &env, &windows_env(r"C:\Users\u")));
            let mut expected = posix_names(Path::new(home));
            expected.extend(posix_names(Path::new(r"C:\Users\u")));
            assert_eq!(detected, expected, "{home}");
        }
    }

    #[test]
    fn a_posix_home_other_than_home_and_the_profile_is_reported_unreached() {
        let mut windows = windows_env(r"C:\Users\u");
        windows.posix_homes = vec![PathBuf::from(r"C:\msys64\home\u"), PathBuf::from(r"C:\cygwin64\home\u")];
        let from_powershell = windows_home(r"C:\Users\u");
        assert_eq!(
            unreached_posix_homes(&from_powershell, &windows),
            [Path::new(r"C:\msys64\home\u"), Path::new(r"C:\cygwin64\home\u")]
        );
        // Run inside MSYS2, its home is `HOME` and gets rc files; only Cygwin's stays unreached.
        let from_msys = windows_home(r"c:\MSYS64\home\u");
        assert_eq!(
            unreached_posix_homes(&from_msys, &windows),
            [Path::new(r"C:\cygwin64\home\u")]
        );
        assert!(unreached_posix_homes(&from_powershell, &windows_env(r"C:\Users\u")).is_empty());
    }

    #[test]
    fn posix_home_candidates_sit_under_the_profile_drive_root() {
        let profile = abs("Users").join("u");
        let root = profile.ancestors().last().unwrap().to_path_buf();
        assert_eq!(
            posix_home_candidates(&profile),
            [
                root.join("msys64").join("home").join("u"),
                root.join("cygwin64").join("home").join("u"),
                root.join("cygwin").join("home").join("u"),
            ]
        );
    }

    #[test]
    fn zdotdir_moves_only_the_home_set() {
        let mut env = windows_home(r"C:\msys64\home\u");
        env.zdotdir = Some(abs("zdot"));
        let detected = paths(&detect_targets_on(Host::Windows, &env, &windows_env(r"C:\Users\u")));
        assert!(detected.contains(&abs("zdot").join(".zshrc")), "{detected:?}");
        assert!(
            detected.contains(&Path::new(r"C:\Users\u").join(".zshrc")),
            "the profile set keeps its own zsh files: {detected:?}"
        );
    }

    #[test]
    fn nushell_goes_under_appdata_when_found_and_nowhere_when_absent() {
        let env = windows_home(r"C:\Users\u");
        let mut windows = windows_env(r"C:\Users\u");
        let absent = detect_targets_on(Host::Windows, &env, &windows);
        assert!(
            absent
                .iter()
                .all(|target| !matches!(target.kind, ProfileKind::DedicatedFile(_)))
        );

        windows.nushell = true;
        let targets = detect_targets_on(Host::Windows, &env, &windows);
        let nushell = targets.last().expect("nushell target");
        assert_eq!(
            nushell.path,
            abs("appdata")
                .as_path()
                .join("nushell")
                .join("vendor")
                .join("autoload")
                .join("ocx.nu")
        );
        assert_eq!(nushell.kind, ProfileKind::DedicatedFile(DedicatedShell::Nushell));
    }

    #[test]
    fn nushell_honours_an_absolute_xdg_data_home_and_ignores_a_relative_one() {
        let mut env = windows_home(r"C:\Users\u");
        let mut windows = windows_env(r"C:\Users\u");
        windows.nushell = true;

        env.xdg_data_home = Some(abs("xdg-data"));
        let absolute = detect_targets_on(Host::Windows, &env, &windows);
        assert_eq!(
            absolute.last().expect("nushell target").path,
            abs("xdg-data")
                .join("nushell")
                .join("vendor")
                .join("autoload")
                .join("ocx.nu")
        );

        env.xdg_data_home = Some(PathBuf::from("relative/data"));
        let relative = detect_targets_on(Host::Windows, &env, &windows);
        assert!(
            relative
                .last()
                .expect("nushell target")
                .path
                .starts_with(abs("appdata")),
            "{relative:?}"
        );
    }

    #[test]
    fn elvish_prefers_xdg_config_home_then_appdata() {
        let mut env = windows_home(r"C:\Users\u");
        let mut windows = windows_env(r"C:\Users\u");
        windows.elvish = true;

        let fallback = detect_targets_on(Host::Windows, &env, &windows);
        let rc = fallback.last().expect("elvish target");
        assert_eq!(rc.path, abs("appdata").as_path().join("elvish").join("rc.elv"));
        assert_eq!(rc.kind, ProfileKind::ElvishFence);

        env.xdg_config_home = Some(abs("xdg-config"));
        let xdg = detect_targets_on(Host::Windows, &env, &windows);
        assert_eq!(
            xdg.last().expect("elvish target").path,
            abs("xdg-config").join("elvish").join("rc.elv")
        );
    }

    #[test]
    fn fish_is_wired_only_when_found_and_ignores_shell() {
        let mut env = windows_home(r"C:\msys64\home\u");
        env.shell = Some("fish.exe".to_string());
        let mut windows = windows_env(r"C:\Users\u");

        let absent = detect_targets_on(Host::Windows, &env, &windows);
        assert!(
            absent.iter().all(|target| target.kind == ProfileKind::PosixFence),
            "SHELL names fish, but no fish.exe was found: {absent:?}"
        );

        windows.fish = true;
        let targets = detect_targets_on(Host::Windows, &env, &windows);
        let fish = targets.last().expect("fish target");
        assert_eq!(
            fish.path,
            Path::new(r"C:\msys64\home\u")
                .join(".config")
                .join("fish")
                .join("conf.d")
                .join("ocx.fish")
        );
        assert_eq!(fish.kind, ProfileKind::DedicatedFile(DedicatedShell::Fish));
        assert!(
            targets
                .iter()
                .filter(|target| target.kind == ProfileKind::PosixFence)
                .count()
                >= 8,
            "the POSIX families stay wired beside fish: {targets:?}"
        );
    }

    #[test]
    fn a_unix_host_ignores_the_windows_snapshot() {
        let env = home_env("/home/dev", "/bin/dash");
        let mut windows = windows_env(r"C:\Users\u");
        windows.nushell = true;
        windows.elvish = true;
        windows.fish = true;
        assert_eq!(detect_targets_on(Host::Unix, &env, &windows), detect_targets(&env));
    }

    // ── PATH lookup ─────────────────────────────────────────────────

    #[test]
    fn shells_are_found_through_the_existence_predicate() {
        let dirs = vec![PathBuf::from("/a"), PathBuf::from("/b")];
        let present = [PathBuf::from("/b/nu.exe"), PathBuf::from("/a/fish.exe")];
        let found = WindowsEnv::from_parts(None, None, &dirs, |path| present.iter().any(|p| p == path));
        assert!(found.nushell && found.fish && !found.elvish, "{found:?}");

        let none = WindowsEnv::from_parts(None, None, &dirs, |_| false);
        assert!(!none.nushell && !none.fish && !none.elvish, "{none:?}");
        let empty = WindowsEnv::from_parts(None, None, &[], |_| true);
        assert!(!empty.nushell, "no directory, nothing found");
    }

    #[test]
    fn an_empty_variable_is_not_a_profile_directory() {
        let found = WindowsEnv::from_parts(Some(PathBuf::new()), Some(PathBuf::new()), &[], |_| false);
        assert!(found.userprofile.is_none() && found.appdata.is_none());
    }

    #[test]
    fn relative_xdg_and_appdata_values_are_ignored_on_a_windows_host() {
        let mut env = windows_home(r"C:\Users\u");
        env.xdg_config_home = Some(PathBuf::from("rel/config"));
        env.xdg_data_home = Some(PathBuf::from("rel/data"));
        let mut windows = windows_env(r"C:\Users\u");
        windows.appdata = Some(abs("appdata"));
        windows.nushell = true;
        windows.elvish = true;
        windows.fish = true;

        let targets = detect_targets_on(Host::Windows, &env, &windows);
        for target in &targets {
            assert!(
                !target.path.starts_with("rel"),
                "a relative directory reached a target: {target:?}"
            );
        }
        let by_kind = |kind| {
            targets
                .iter()
                .find(|target| target.kind == kind)
                .map(|t| t.path.clone())
        };
        assert_eq!(
            by_kind(ProfileKind::ElvishFence),
            Some(abs("appdata").join("elvish").join("rc.elv"))
        );
        assert_eq!(
            by_kind(ProfileKind::DedicatedFile(DedicatedShell::Fish)),
            Some(
                Path::new(r"C:\Users\u")
                    .join(".config")
                    .join("fish")
                    .join("conf.d")
                    .join("ocx.fish")
            )
        );

        windows.appdata = Some(PathBuf::from("rel/appdata"));
        let without_base = detect_targets_on(Host::Windows, &windows_home(r"C:\Users\u"), &windows);
        assert!(
            without_base.iter().all(|target| !matches!(
                target.kind,
                ProfileKind::ElvishFence | ProfileKind::DedicatedFile(DedicatedShell::Nushell)
            )),
            "no absolute base, so no nushell or elvish target: {without_base:?}"
        );
    }

    #[test]
    fn relative_and_empty_path_entries_do_not_count_as_directories() {
        let absolute = abs("tools");
        let joined = std::env::join_paths([PathBuf::from("rel"), PathBuf::new(), absolute.clone()]).expect("joinable");
        assert_eq!(absolute_path_entries(&joined), vec![absolute]);
    }
}
