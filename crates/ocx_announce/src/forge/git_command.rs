// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The `git` executable this crate drives, and the one seam that runs it.
//!
//! The only file on the git-transport path allowed to name a child-process builder; an
//! exported alias or wrapper of one lets a sibling spawn unseen by the process firewall's
//! source-text search.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use super::{ForgeError, GitPushCredential};
use ocx_config::env::Env;

/// A resolved `git` executable and the version it reported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitBinary {
    /// The executable as it was resolved on `PATH`.
    pub path: PathBuf,
    /// The version it reported.
    pub version: GitVersion,
}

/// A `git` version as the plain `(major, minor, patch)` triple.
//
// Not `ocx_package::version::Version`: its rolling-parent order ranks `2.31` above
// `2.31.0`, so a `>= MINIMUM` gate on it would refuse git 2.31.0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GitVersion {
    /// The major component.
    pub major: u32,
    /// The minor component.
    pub minor: u32,
    /// The patch component; `0` when `git` reported only two.
    pub patch: u32,
}

impl GitVersion {
    /// The oldest `git` the write transport runs on.
    pub const MINIMUM: Self = Self::new(2, 31, 0);

    /// The triple, as a `const` so a floor can be written as one.
    #[must_use]
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self { major, minor, patch }
    }

    /// The version `git --version` reported, or `None` when its output carries none.
    ///
    /// `2.31` yields `(2, 31, 0)` and git-for-Windows' `2.54.0.windows.1` `(2, 54, 0)`.
    /// A component that does not fit a `u32` yields `None`, never a panic or saturation.
    #[must_use]
    pub fn from_version_output(stdout: &str) -> Option<Self> {
        // Anchored on `git version `: unanchored, a `warning:` preamble or a `GIT_TRACE`
        // timestamp parses as the version and a good git is refused.
        let reported = stdout
            .lines()
            .find_map(|line| line.strip_prefix("git version "))?
            .split_whitespace()
            .next()?;

        let mut components = reported.split('.');
        let major: u32 = components.next()?.parse().ok()?;
        let minor: u32 = components.next()?.parse().ok()?;
        let patch: u32 = match components.next() {
            Some(patch) => patch.parse().ok()?,
            None => 0,
        };
        Some(Self::new(major, minor, patch))
    }
}

impl std::fmt::Display for GitVersion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Resolve `git` on `PATH` and read its version, refusing anything below
/// [`GitVersion::MINIMUM`].
///
/// # Errors
///
/// Returns [`ForgeError::GitUnavailable`] when `git` is absent, cannot be run,
/// reports a version that cannot be parsed, or reports one below the floor.
pub async fn probe_git_binary() -> Result<GitBinary, ForgeError> {
    // A failed `current_dir` only loses relative `PATH` entries; absolute ones resolve either way.
    let cwd = std::env::current_dir().unwrap_or_default();
    // Resolved against ocx's `PATH` and spawned absolute: `Command::new("git")` would search
    // the parent's `PATH`, so `GitBinary::path` could name a binary that never ran.
    let search_path = ocx_util::env::var("PATH");
    let resolved = which::which_in("git", search_path.as_deref(), cwd).map_err(|error| ForgeError::GitUnavailable {
        reason: format!("git could not be resolved on PATH: {error}"),
    })?;
    probe_git_binary_at(&resolved).await
}

/// [`probe_git_binary`] against an explicitly named executable.
///
/// # Errors
///
/// Returns [`ForgeError::GitUnavailable`] when `program` cannot be run, exits
/// non-zero, prints output no version can be parsed from, or reports a version
/// below [`GitVersion::MINIMUM`].
pub(super) async fn probe_git_binary_at(program: &Path) -> Result<GitBinary, ForgeError> {
    let unavailable = |reason: String| ForgeError::GitUnavailable { reason };

    let output = tokio::process::Command::new(program)
        .arg("--version")
        .env_clear()
        .envs(git_child_env(|name| std::env::var_os(name), None, LazyFetch::Refuse).iter())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|error| unavailable(format!("{} could not be run: {error}", program.display())))?;

    // Carry git's stderr: a malformed `$HOME/.gitconfig` (`HOME` passes through) fails
    // `--version` with exit 128 and empty stdout on a healthy git.
    if !output.status.success() {
        let stderr = redact(String::from_utf8_lossy(&output.stderr).trim(), &[]).capped();
        return Err(unavailable(format!(
            "{} failed ({}): {stderr}",
            program.display(),
            output.status
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let version = GitVersion::from_version_output(&stdout).ok_or_else(|| {
        let printed = redact(stdout.trim(), &[]).capped();
        unavailable(format!("{} printed no version: {printed}", program.display()))
    })?;
    if version < GitVersion::MINIMUM {
        return Err(unavailable(format!(
            "{} reports {version}, below the {} the git write transport needs",
            program.display(),
            GitVersion::MINIMUM
        )));
    }

    Ok(GitBinary {
        path: program.to_path_buf(),
        version,
    })
}

/// The child-environment allowlist, as data.
///
/// Only [`PASSTHROUGH`] is `#[cfg]`-selected: a `#[cfg(windows)]` platform table would
/// leave its assertions unrun on Linux CI.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "`PASSTHROUGH` and `SET` are read by the child-environment builder; `NEVER`, `INJECTED` and whichever platform table is not live are assertion data by design — the disjointness check is what makes them load-bearing, so they have no production reader and must not acquire one"
    )
)]
mod table {
    /// Names copied from the parent environment when the parent has them.
    ///
    /// Both proxy spellings stay: lowercase alone loses the proxy on Unix runners exporting `HTTPS_PROXY`.
    /// `PATH` stays although `git` is spawned absolute: `git` resolves
    /// `git-remote-https`, `ssh` and every hook off it.
    pub const UNIX_PASSTHROUGH: &[&str] = &[
        "PATH",
        "HOME",
        "http_proxy",
        "https_proxy",
        "no_proxy",
        "all_proxy",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
        "ALL_PROXY",
        "GIT_SSL_CAINFO",
        "GIT_SSL_CAPATH",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "TMPDIR",
        "TEMP",
        "TMP",
    ];

    /// [`UNIX_PASSTHROUGH`] plus Windows' home-directory triple and `SYSTEMROOT`, which
    /// the Windows CRT and the TLS stack both need.
    pub const WINDOWS_PASSTHROUGH: &[&str] = &[
        "PATH",
        "HOME",
        "USERPROFILE",
        "HOMEDRIVE",
        "HOMEPATH",
        "SYSTEMROOT",
        "http_proxy",
        "https_proxy",
        "no_proxy",
        "all_proxy",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
        "ALL_PROXY",
        "GIT_SSL_CAINFO",
        "GIT_SSL_CAPATH",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "TMPDIR",
        "TEMP",
        "TMP",
    ];

    /// The table this platform's child is actually built from.
    #[cfg(not(windows))]
    pub const PASSTHROUGH: &[&str] = UNIX_PASSTHROUGH;

    /// The table this platform's child is actually built from.
    #[cfg(windows)]
    pub const PASSTHROUGH: &[&str] = WINDOWS_PASSTHROUGH;

    /// Names ocx sets unconditionally, with the values it sets them to.
    ///
    /// All four identity names are set, or `$HOME/.gitconfig` supplies the missing one.
    pub const SET: &[(&str, &str)] = &[
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_AUTHOR_NAME", "ocx"),
        ("GIT_AUTHOR_EMAIL", "noreply@ocx.sh"),
        ("GIT_COMMITTER_NAME", "ocx"),
        ("GIT_COMMITTER_EMAIL", "noreply@ocx.sh"),
        ("LC_ALL", "C"),
        ("LANGUAGE", ""),
    ];

    /// The credential triple, present **only** on an invocation that injects an ocx credential.
    pub const INJECTED: &[&str] = &["GIT_CONFIG_COUNT", "GIT_CONFIG_KEY_0", "GIT_CONFIG_VALUE_0"];

    /// Set on a **local** invocation and on no other.
    ///
    /// Without it a local command lazily fetches a missing blob with no credential, and the
    /// 401 is reported as a rejected credential.
    /// Not in [`SET`]: `fetch` and `push` depend on the lazy fetch this disables.
    //
    // Inert below git 2.45.0 (above `MINIMUM`), so `git_workspace`'s `write-tree --missing-ok` must stay.
    pub const NO_LAZY_FETCH: &[(&str, &str)] = &[("GIT_NO_LAZY_FETCH", "1")];

    /// Name **prefixes** that must never reach the child, whatever the parent holds.
    pub const NEVER: &[&str] = &[
        "GIT_TRACE",
        "GIT_CURL_VERBOSE",
        "GIT_ASKPASS",
        "SSH_ASKPASS",
        "OCX_ANNOUNCE_",
        "CI_JOB_TOKEN",
    ];
}

/// The `http.<prefix>` scope a credential may be injected under.
///
/// It carries the full project path: a broader prefix silently sends the `Authorization`
/// header to every repository under it.
//
// Private field in a submodule-free module, or code outside `new` can build a too-wide scope.
pub(super) struct CredentialScope(String);

impl CredentialScope {
    /// The scope for `prefix`, refusing one that names no project.
    ///
    /// A coordinate is `NAMESPACE/PROJECT` with a possibly nested namespace, so two path
    /// segments is the floor; one covers a whole group.
    ///
    /// # Errors
    ///
    /// Returns [`ForgeError::GitUnavailable`] when `prefix` carries fewer than two path
    /// segments.
    pub(super) fn new(prefix: &str) -> Result<Self, ForgeError> {
        let authority_and_path = prefix.split_once("://").map_or(prefix, |(_, rest)| rest);
        let path = authority_and_path.split_once('/').map_or("", |(_, path)| path);
        if path.split('/').filter(|segment| !segment.is_empty()).count() < 2 {
            return Err(ForgeError::GitUnavailable {
                reason: format!(
                    "the credential scope {prefix} names no project, so the announce credential would reach every repository under it"
                ),
            });
        }
        Ok(Self(prefix.to_string()))
    }
}

impl std::fmt::Display for CredentialScope {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The credential injected, and the [`CredentialScope`] it is injected under.
pub(super) struct CredentialInjection<'a> {
    /// The `http.<prefix>.extraHeader` scope, e.g. `https://gitlab.example/acme/index`.
    pub url_prefix: &'a CredentialScope,
    /// The credential whose `Authorization` header is injected under it.
    pub credential: &'a GitPushCredential,
}

/// Build the child environment for a `git` invocation from [`Env::clean`] and the tables above.
///
/// A passthrough name present but empty is carried verbatim: dropping `http_proxy=` lets
/// `$HOME/.gitconfig`'s `http.proxy` win silently.
pub(super) fn git_child_env(
    lookup: impl Fn(&str) -> Option<OsString>,
    injection: Option<&CredentialInjection<'_>>,
    lazy_fetch: LazyFetch,
) -> Env {
    // `Env::clean`, never `Env::new`/`default`: those start from the whole parent environment.
    let mut env = Env::clean();

    for name in table::PASSTHROUGH {
        if let Some(value) = lookup(name) {
            env.set(*name, value);
        }
    }
    for (name, value) in table::SET {
        env.set(*name, *value);
    }

    if lazy_fetch == LazyFetch::Refuse {
        for (name, value) in table::NO_LAZY_FETCH {
            env.set(*name, *value);
        }
    }

    if let Some(injection) = injection {
        env.set("GIT_CONFIG_COUNT", "1");
        env.set("GIT_CONFIG_KEY_0", format!("http.{}.extraHeader", injection.url_prefix));
        env.set(
            "GIT_CONFIG_VALUE_0",
            format!("Authorization: {}", injection.credential.basic_authorization()),
        );
    }

    env
}

/// Whether an invocation may resolve a missing object over the network.
///
/// Not derivable from the injection: an invocation that injects nothing may still talk to
/// the remote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LazyFetch {
    /// A network invocation: promisor semantics stay on.
    Allow,
    /// A local plumbing invocation: a missing object is an error, never a fetch.
    Refuse,
}

/// Run one `git` invocation to completion and hand back what it printed.
///
/// The capture is raw and uncapped; masking it is the caller's job.
///
/// # Errors
///
/// Returns [`ForgeError::GitUnavailable`] only when the child cannot start; a `git` that ran
/// and failed comes back in the capture for the caller to classify.
//
// Nothing here may export or yield a `tokio::process::Command`: a holder could restore
// `GIT_TRACE` or put a credential-bearing URL on argv without naming a spawn token.
pub(super) async fn run_git(
    git: &GitBinary,
    workdir: &Path,
    injection: Option<&CredentialInjection<'_>>,
    lazy_fetch: LazyFetch,
    args: &[&OsStr],
) -> Result<std::process::Output, ForgeError> {
    let env = git_child_env(|name| std::env::var_os(name), injection, lazy_fetch);
    let mut command = git_child_command(git, workdir, &env);
    command.args(args);
    command.output().await.map_err(|error| ForgeError::GitUnavailable {
        reason: redact(&format!("{} could not be run: {error}", git.path.display()), &[])
            .capped()
            .to_string(),
    })
}

/// The child-process builder for one `git` invocation; private, so siblings spawn only
/// through [`run_git`].
fn git_child_command(git: &GitBinary, workdir: &Path, env: &Env) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(&git.path);
    command
        .current_dir(workdir)
        .env_clear()
        .envs(env.iter())
        // A cancelled announce must not leave a `git` holding the workspace open past the
        // guard that removes it.
        .kill_on_drop(true);
    command
}

/// Text that has been through [`redact`], and the only thing the two `stderr`-bearing
/// [`ForgeError`] variants accept.
//
// Declared in `forge.rs`, its private field would be writable from every forge submodule.
// `redact` stays its only constructor: a `From<String>` or `new` lets unmasked stderr into an error.
#[derive(Debug)]
pub struct Redacted(String);

impl Redacted {
    /// The cap, in **characters**, on redacted text placed in an error.
    //
    // Kept equal to `error.rs`'s `STATUS_DETAIL_CAP`, so the two caps on one disclosure agree.
    pub const CAP: usize = 300;

    /// The masked text, for a classifier matching phrases against it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The same text, shortened to [`Self::CAP`] characters.
    ///
    /// Apply to an error's payload, never to the classifier's input: the server's refusal
    /// phrases sit at the tail, and capping first turns every one into an unclassified exit 1.
    #[must_use]
    pub fn capped(self) -> Self {
        // Counted in chars: a byte slice panics when `CAP` lands inside a multi-byte character.
        match self.0.char_indices().nth(Self::CAP) {
            Some((boundary, _)) => Self(format!("{}... [truncated]", &self.0[..boundary])),
            None => self,
        }
    }
}

impl std::fmt::Display for Redacted {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Mask every secret in `text` before it reaches an error, a log or a report.
///
/// Pass every live form of the credential (API credential, push secret, the base64
/// `user:secret` blob): a form left out stays in the output.
#[must_use]
pub fn redact(text: &str, secrets: &[&str]) -> Redacted {
    // Skip empty secrets: `str::replace` matches an empty needle at every boundary, so a
    // blank push secret would shred the message into markers.
    let mut masked = text.to_string();
    for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
        masked = masked.replace(secret, "[redacted]");
    }
    Redacted(masked)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;

    use super::*;
    use crate::forge::ForgeToken;

    /// A parent environment as a closure, for [`git_child_env`]'s injectable
    /// reader. Owning, so the closure outlives the fixture literal.
    fn parent_env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let map: HashMap<String, OsString> = pairs
            .iter()
            .map(|(name, value)| ((*name).to_string(), OsString::from(*value)))
            .collect();
        move |name| map.get(name).cloned()
    }

    /// A scope naming a real project, built through the one constructor.
    ///
    /// Deliberately not a bare string: the injection fixtures cannot spell a
    /// host-only scope even by accident, which is the property
    /// [`CredentialScope`] exists to hold.
    fn project_scope() -> CredentialScope {
        CredentialScope::new("https://gitlab.example/acme/index").expect("a project scope must be accepted")
    }

    /// The child's value for `name`, as UTF-8.
    fn child_value(child: &Env, name: &str) -> Option<String> {
        child.get(name).map(|value| value.to_string_lossy().into_owned())
    }

    /// A `git` that is not `git`: an executable printing `script` on stdout.
    ///
    /// The version gate has no other way to observe a version it did not
    /// install — `probe_git_binary` takes no argument and can only ever see this
    /// host's real `git`, whose green says nothing about the floor (DX-95).
    ///
    /// Written by a child rather than in-process, and that is not ceremony:
    /// `execve` refuses a file any process holds open for writing (ETXTBSY), and
    /// a sibling test thread that forks while this thread holds the write handle
    /// leaks it into its child, which keeps the shim busy until that child execs.
    /// Writing through a shell keeps the handle out of this process's descriptor
    /// table, so no fork can inherit it and no later exec of the shim is refused.
    #[cfg(unix)]
    fn shim(directory: &Path, script: &str) -> PathBuf {
        let path = directory.join("git");
        let status = std::process::Command::new("/bin/sh")
            .args(["-c", r#"printf '%s' "$1" > "$2" && chmod 755 "$2""#, "sh"])
            .arg(format!("#!/bin/sh\n{script}\n"))
            .arg(&path)
            .status()
            .expect("write the git shim");
        assert!(status.success(), "the git shim must be written and made executable");
        path
    }

    /// A shim printing exactly one `git version …` line on stdout, exit 0.
    #[cfg(unix)]
    fn version_shim(directory: &Path, reported: &str) -> PathBuf {
        shim(directory, &format!("echo 'git version {reported}'"))
    }

    // ---- the version parser (C-020, as amended by DX-96) ----

    /// Every output shape a real `git` prints, plus the two the parser must
    /// refuse rather than crash on.
    ///
    /// The two-component row is honestly a *parser* contract, not observed
    /// behaviour: no released `git` prints two components — 2.54.0 prints three
    /// and git-for-Windows prints four.
    ///
    /// Reds on: making the third capture group mandatory (`2.31` stops parsing);
    /// splitting on `.` and requiring exactly three numeric parts
    /// (`2.54.0.windows.1` stops parsing); `parse::<u32>().unwrap()` on the
    /// components (the overflow row panics instead of returning `None`).
    #[test]
    fn git_version_parses_the_shapes_git_actually_prints() {
        for (output, expected) in [
            ("git version 2.54.0\n", Some(GitVersion::new(2, 54, 0))),
            ("git version 2.31.0\n", Some(GitVersion::new(2, 31, 0))),
            ("git version 2.31\n", Some(GitVersion::new(2, 31, 0))),
            ("git version 2.54.0.windows.1\n", Some(GitVersion::new(2, 54, 0))),
            ("git version 02.31.0\n", Some(GitVersion::new(2, 31, 0))),
            ("git version 999999999999.1.0\n", None),
            ("git version unknown\n", None),
            ("", None),
            ("fatal: bad config line 1 in file /home/o/.gitconfig\n", None),
        ] {
            assert_eq!(
                GitVersion::from_version_output(output),
                expected,
                "git --version printed {output:?}"
            );
        }
    }

    /// The parse is anchored on a line beginning `git version `, and reads
    /// **stdout only**.
    ///
    /// C-020's original rule — the first `\d+\.\d+(\.\d+)?` anywhere in the
    /// output — ships a defect: measured, a `git` whose stdout carries a
    /// preamble parses as `1.2` and a perfectly good 2.31.0 is refused. The
    /// `GIT_TRACE` line below is the same trap arriving from the other side: its
    /// timestamp `28.682964` matches the naive pattern and would be read as a
    /// version ≥ the floor.
    ///
    /// Reds on: dropping the `git version ` anchor (the preamble row parses
    /// `1.2`).
    #[test]
    fn git_version_parse_anchors_on_the_git_version_line() {
        assert_eq!(
            GitVersion::from_version_output("warning: something 1.2 happened\ngit version 2.31.0\n"),
            Some(GitVersion::new(2, 31, 0)),
            "a preamble carrying a version-shaped number must not be read as the version"
        );
        assert_eq!(
            GitVersion::from_version_output(
                "11:59:28.682964 git.c:502 trace: built-in: git version\ngit version 2.54.0\n"
            ),
            Some(GitVersion::new(2, 54, 0)),
            "a GIT_TRACE timestamp matches the naive pattern and must not be read as the version"
        );
        assert_eq!(
            GitVersion::from_version_output("2.31.0\n"),
            None,
            "a bare number on no `git version` line is not a version this parser recognises"
        );
    }

    /// The comparison is the plain triple, not a string and not
    /// [`ocx_package::version::Version`].
    ///
    /// Two distinct defects live here and only one of them is caught by a
    /// below-the-floor case. A **string** comparison refuses `2.30.9` correctly
    /// and also refuses git 10, because `"10.0.0" < "9.9.9"` lexicographically —
    /// so the accept side carries `10.0.0` and `9.9.9` or the check is a habit.
    /// `package::version::Version` orders a two-component `2.31` *above*
    /// `2.31.0` (rolling-parent semantics), so the last row is what tells the
    /// two apart.
    ///
    /// Reds on: comparing the rendered strings (`10.0.0` is refused); swapping
    /// the triple for `package::version::Version` (`2.31` and `2.31.0` stop
    /// comparing equal).
    #[test]
    fn git_version_compare_is_a_plain_tuple() {
        assert_eq!(GitVersion::MINIMUM, GitVersion::new(2, 31, 0), "the floor is 2.31.0");

        for accepted in [
            GitVersion::new(2, 31, 0),
            GitVersion::new(2, 54, 0),
            GitVersion::new(3, 0, 0),
            GitVersion::new(9, 9, 9),
            GitVersion::new(10, 0, 0),
        ] {
            assert!(accepted >= GitVersion::MINIMUM, "{accepted} is at or above the floor");
        }
        for refused in [
            GitVersion::new(2, 30, 9),
            GitVersion::new(1, 99, 99),
            GitVersion::new(0, 0, 0),
        ] {
            assert!(refused < GitVersion::MINIMUM, "{refused} is below the floor");
        }

        assert!(
            GitVersion::new(10, 0, 0) > GitVersion::new(9, 9, 9),
            "a string comparison would order git 10 below git 9"
        );
        assert_eq!(
            GitVersion::from_version_output("git version 2.31"),
            GitVersion::from_version_output("git version 2.31.0"),
            "a missing patch component means zero, not `the newest 2.31.x`"
        );
    }

    // ---- the version gate (C-020, C-075) ------------------------------------

    /// A `git` one patch release below the floor is refused, and the reason
    /// names what was found.
    ///
    /// Reds on: inverting the gate (`<=` for `>=`); dropping the gate entirely.
    #[cfg(unix)]
    #[tokio::test]
    async fn probe_git_binary_refuses_below_floor() {
        let directory = tempfile::TempDir::new().expect("temp dir");
        for below in ["2.30.9", "1.99.99", "2.30.0"] {
            let path = version_shim(directory.path(), below);
            let error = probe_git_binary_at(&path)
                .await
                .expect_err("a git below the floor must not be accepted");
            let ForgeError::GitUnavailable { reason } = &error else {
                panic!("expected GitUnavailable, got {error:?}");
            };
            assert!(
                reason.contains(below),
                "the reason must name the version that was found: {reason}"
            );
        }
    }

    /// Absent, a directory, and present-but-not-executable all reach
    /// `GitUnavailable` rather than a panic.
    ///
    /// Reds on: `.expect("git runs")` on the spawn — every row panics
    /// (`ENOENT`, `EISDIR`, `EACCES`) instead of returning the error the argv
    /// boundary is written against.
    #[cfg(unix)]
    #[tokio::test]
    async fn probe_git_binary_refuses_absent() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::TempDir::new().expect("temp dir");

        let missing = directory.path().join("no-such-git");
        let not_executable = directory.path().join("not-executable");
        std::fs::write(&not_executable, "#!/bin/sh\necho 'git version 2.54.0'\n").expect("write");
        std::fs::set_permissions(&not_executable, std::fs::Permissions::from_mode(0o644)).expect("chmod");

        for absent in [missing.as_path(), directory.path(), not_executable.as_path()] {
            let error = probe_git_binary_at(absent)
                .await
                .expect_err("an unusable git must not be accepted");
            assert!(
                matches!(error, ForgeError::GitUnavailable { .. }),
                "expected GitUnavailable for {absent:?}, got {error:?}"
            );
        }
    }

    /// The **accept** side, plural: the boundary release itself, both of its
    /// spellings, the four-component git-for-Windows form, and versions well
    /// above the floor. A gate that refuses everything passes every refusal
    /// test.
    ///
    /// Reds on: inverting the comparison; making the patch component mandatory;
    /// refusing a version string with a fourth component.
    #[cfg(unix)]
    #[tokio::test]
    async fn probe_git_binary_accepts_the_boundary_release() {
        for (reported, expected) in [
            ("2.31", GitVersion::new(2, 31, 0)),
            ("2.31.0", GitVersion::new(2, 31, 0)),
            ("2.54.0", GitVersion::new(2, 54, 0)),
            ("2.54.0.windows.1", GitVersion::new(2, 54, 0)),
            ("3.0.0", GitVersion::new(3, 0, 0)),
            ("10.0.0", GitVersion::new(10, 0, 0)),
        ] {
            let directory = tempfile::TempDir::new().expect("temp dir");
            let path = version_shim(directory.path(), reported);
            let binary = probe_git_binary_at(&path)
                .await
                .unwrap_or_else(|error| panic!("git version {reported} must be accepted, got {error:?}"));
            assert_eq!(binary.version, expected, "git version {reported}");
            assert_eq!(binary.path, path, "the resolved path travels with the version");
        }
    }

    /// A `git` that is present and healthy can still fail `--version`: a
    /// malformed `$HOME/.gitconfig` exits 128 with **empty stdout**, and
    /// `GIT_CONFIG_NOSYSTEM=1` does not protect against it — measured; it
    /// suppresses `/etc/gitconfig` only, while `HOME` is a deliberate C-033
    /// passthrough. An operator told "git is absent" then hunts for a binary
    /// they already have.
    ///
    /// Reds on: ignoring the exit status and reporting only "the version could
    /// not be parsed"; dropping git's stderr from the reason.
    #[cfg(unix)]
    #[tokio::test]
    async fn probe_git_binary_reports_gits_own_reason_when_it_exits_nonzero() {
        let directory = tempfile::TempDir::new().expect("temp dir");
        let path = shim(
            directory.path(),
            "echo 'fatal: bad config line 1 in file /home/o/.gitconfig' >&2\nexit 128",
        );

        let error = probe_git_binary_at(&path).await.expect_err("exit 128 is not a version");
        let ForgeError::GitUnavailable { reason } = &error else {
            panic!("expected GitUnavailable, got {error:?}");
        };
        assert!(
            reason.contains("bad config line 1"),
            "git's own diagnosis must reach the operator: {reason}"
        );
    }

    /// The version is read from **stdout**, never from stdout and stderr
    /// concatenated.
    ///
    /// The companion guard to the parser's anchor, and it must be mutated
    /// separately: `GIT_TRACE=1` writes `11:59:28.682964 …` to stderr, whose
    /// `28.682964` parses as a version far above the floor. Two guards defend
    /// this one property — the NEVER table keeps `GIT_TRACE` out of the child,
    /// and this keeps stderr out of the parse — so deleting either alone leaves
    /// the other green.
    ///
    /// Reds on: parsing `[stdout, stderr].concat()`.
    #[cfg(unix)]
    #[tokio::test]
    async fn probe_git_binary_reads_the_version_from_stdout_only() {
        let directory = tempfile::TempDir::new().expect("temp dir");
        let path = shim(
            directory.path(),
            "echo '11:59:28.682964 git.c:502 trace: built-in: git version' >&2\necho 'git version 2.31.0'",
        );

        let binary = probe_git_binary_at(&path)
            .await
            .expect("stderr chatter is not an error");
        assert_eq!(
            binary.version,
            GitVersion::new(2, 31, 0),
            "a trace timestamp on stderr must not be read as the version"
        );
    }

    /// An exit-0 `git` whose stdout carries no version is refused, and the
    /// reason says what it printed.
    ///
    /// Reds on: returning `GitVersion::new(0, 0, 0)` on a failed parse — the
    /// floor gate then refuses it too, so the *outer* behaviour looks right and
    /// only the reason tells the two apart.
    #[cfg(unix)]
    #[tokio::test]
    async fn probe_git_binary_refuses_output_carrying_no_version() {
        let directory = tempfile::TempDir::new().expect("temp dir");
        let path = shim(directory.path(), "echo 'git version unknown'");

        let error = probe_git_binary_at(&path)
            .await
            .expect_err("`unknown` is not a version");
        let ForgeError::GitUnavailable { reason } = &error else {
            panic!("expected GitUnavailable, got {error:?}");
        };
        assert!(
            reason.contains("unknown"),
            "the reason must quote what git printed, not just say the parse failed: {reason}"
        );
    }

    /// Output that is not valid UTF-8 is decoded lossily, never unwrapped and
    /// never dropped whole.
    ///
    /// The capturing helper hands back bytes, because C-020 has to parse stdout
    /// — and `git` writes a filename in the runner's own byte encoding, so a
    /// `fatal:` line naming one is an ordinary day on a runner whose paths are
    /// not UTF-8. Both halves matter and each has its own row: an invalid byte
    /// on **stdout** must reach `GitUnavailable` rather than a panic, and an
    /// invalid byte on **stderr** must not cost the operator the ASCII diagnosis
    /// sitting beside it.
    ///
    /// Reds on: `String::from_utf8(bytes).unwrap()` (both rows panic);
    /// `from_utf8(bytes).unwrap_or_default()` (the second row loses git's own
    /// diagnosis and the reason says nothing an operator can act on).
    #[cfg(unix)]
    #[tokio::test]
    async fn probe_git_binary_decodes_output_that_is_not_utf8_lossily() {
        let directory = tempfile::TempDir::new().expect("temp dir");

        let invalid_stdout = shim(directory.path(), "printf 'git version \\377\\376\\n'");
        let error = probe_git_binary_at(&invalid_stdout)
            .await
            .expect_err("bytes that are not a version are not a version");
        assert!(
            matches!(error, ForgeError::GitUnavailable { .. }),
            "invalid UTF-8 on stdout must be refused, not unwrapped: {error:?}"
        );

        let second = tempfile::TempDir::new().expect("temp dir");
        let invalid_stderr = shim(
            second.path(),
            "printf 'fatal: bad config line 1 in file /home/o/\\377\\376/.gitconfig\\n' >&2\nexit 128",
        );
        let error = probe_git_binary_at(&invalid_stderr)
            .await
            .expect_err("exit 128 is not a version");
        let ForgeError::GitUnavailable { reason } = &error else {
            panic!("expected GitUnavailable, got {error:?}");
        };
        assert!(
            reason.contains("bad config line 1"),
            "one undecodable byte must not cost the operator the rest of git's diagnosis: {reason}"
        );
    }

    /// `probe_git_binary` resolves `git` against the `PATH` **ocx** reads, and
    /// spawns the absolute result.
    ///
    /// `Command::new("git")` would search the parent process's `PATH` through
    /// whichever of `posix_spawnp` / fork-exec the standard library chose, so
    /// [`GitBinary::path`]'s promise would describe a search ocx never
    /// performed. The shim below reports a version this host's real `git` does
    /// not, which is what makes the two answers distinguishable.
    ///
    /// Reds on: spawning `"git"` by name — the real `git` on the ambient `PATH`
    /// answers and both assertions fail.
    #[cfg(unix)]
    #[tokio::test]
    async fn probe_git_binary_resolves_git_on_the_child_path() {
        let env = ocx_util::env::overrides::lock();
        let directory = tempfile::TempDir::new().expect("temp dir");
        let path = version_shim(directory.path(), "3.14.15");
        env.set("PATH", directory.path().to_str().expect("utf-8 temp path"));

        let binary = probe_git_binary().await.expect("the shim on PATH is a usable git");
        assert_eq!(
            binary.path, path,
            "the binary resolved must be the one on the child PATH"
        );
        assert_eq!(binary.version, GitVersion::new(3, 14, 15));
    }

    // ---- redaction and capping (C-019, C-022, DX-24) ------------------------

    /// Every live form of the credential is masked, and the diagnosis around it
    /// survives — redaction that eats the message leaves an operator with
    /// nothing to act on.
    ///
    /// Reds on: masking only `secrets[0]`, or dropping the surrounding text.
    #[test]
    fn redact_masks_every_form_and_keeps_the_message() {
        let masked = redact(
            "remote: rejected: PRIVATE-TOKEN glpat-notarealvalue, basic Z2l0bGFiLWNpOnNlY3JldA==",
            &["glpat-notarealvalue", "Z2l0bGFiLWNpOnNlY3JldA=="],
        )
        .to_string();
        assert!(
            !masked.contains("glpat-notarealvalue"),
            "the API form survived: {masked}"
        );
        assert!(
            !masked.contains("Z2l0bGFiLWNpOnNlY3JldA=="),
            "the base64 form survived: {masked}"
        );
        assert!(masked.contains("remote: rejected"), "the diagnosis was lost: {masked}");
    }

    /// The falsifying half: with nothing to match, the text is returned whole.
    /// Without it the assertion above would pass on a function that blanks every
    /// input. The empty secret is the live edge — `str::replace` matches an
    /// empty needle everywhere.
    #[test]
    fn redact_leaves_text_alone_when_no_secret_matches() {
        assert_eq!(
            redact(
                "fatal: could not read from the remote repository",
                &["", "glpat-absent"]
            )
            .to_string(),
            "fatal: could not read from the remote repository"
        );
    }

    /// **All three** forms in one call, which the sibling above does not deliver
    /// — it passes two secrets (the API credential and the base64 blob) and the
    /// distinct push token never appears.
    ///
    /// The three are genuinely different values in the shape this feature ships
    /// in: an `OCX_ANNOUNCE_TOKEN` for the REST half, an `OCX_ANNOUNCE_GIT_TOKEN`
    /// for the push half, and the base64 `user:secret` blob the second becomes on
    /// the wire.
    ///
    /// Reds on: masking only the first two secrets; masking only the value the
    /// injector happened to hold.
    #[test]
    fn redactor_masks_all_three_secret_forms() {
        let api = "glpat-notarealapivalue";
        let push = "glpat-notarealpushvalue";
        let wire = BASE64_STANDARD.encode(format!("gitlab-ci-token:{push}"));

        let masked = redact(
            &format!(
                "remote: HTTP Basic: Access denied\nremote: PRIVATE-TOKEN: {api}\nremote: sent Authorization: Basic {wire}\nfatal: Authentication failed (job token {push})"
            ),
            &[api, push, &wire],
        );

        for secret in [api, push, wire.as_str()] {
            assert!(
                !masked.as_str().contains(secret),
                "a live secret form survived redaction: {}",
                masked.as_str()
            );
        }
        assert!(
            masked.as_str().contains("HTTP Basic: Access denied"),
            "the diagnosis was lost: {}",
            masked.as_str()
        );
        assert_eq!(
            masked.as_str().matches("[redacted]").count(),
            3,
            "one marker per form, or a form was never reached: {}",
            masked.as_str()
        );
    }

    /// Masking is plain substring replacement: a secret embedded in a longer
    /// token is still a disclosure of the secret.
    ///
    /// Reds on: switching to a word-boundary or whitespace-delimited match.
    #[test]
    fn redact_masks_a_secret_embedded_in_a_longer_token() {
        let masked = redact("remote: rejected token=glpat-abcdef", &["glpat-abc"]);
        assert!(
            !masked.as_str().contains("glpat-abc"),
            "the embedded occurrence survived: {}",
            masked.as_str()
        );
    }

    /// A secret sitting on its own line is masked, and the lines around it
    /// survive.
    ///
    /// Reds on: redacting only the first line of the captured text.
    ///
    /// The neighbouring shape — a newline injected *inside* the secret, as a
    /// wrapped curl trace produces — is **not** covered and is not claimed: no
    /// substring redactor can mask it and no reachable red exists. Accepted
    /// residual.
    #[test]
    fn redact_masks_a_secret_on_a_later_line() {
        let masked = redact(
            "remote: line one\nglpat-notarealvalue\nremote: line three",
            &["glpat-notarealvalue"],
        );
        assert!(
            !masked.as_str().contains("glpat-notarealvalue"),
            "a secret past the first line survived: {}",
            masked.as_str()
        );
        assert!(
            masked.as_str().contains("line three"),
            "the lines after the secret were dropped: {}",
            masked.as_str()
        );
    }

    /// `redact` masks, and does **not** shorten. The stderr classifier matches
    /// phrases the server writes at the tail, behind its own `remote:` banner,
    /// so a cap applied before classification destroys the phrase table's inputs
    /// and turns every recognised refusal into an unclassified exit 1.
    ///
    /// Reds on: applying [`Redacted::CAP`] inside `redact`.
    #[test]
    fn redact_does_not_cap_so_the_classifier_reads_the_whole_message() {
        let filler = "y".repeat(Redacted::CAP * 2);
        let text = format!("remote: {filler}\nremote: pre-receive hook declined");
        let masked = redact(&text, &["glpat-absent"]);

        assert!(
            masked.as_str().ends_with("pre-receive hook declined"),
            "the classifier's phrase sits at the tail and must survive redaction"
        );
        assert_eq!(
            masked.as_str().chars().count(),
            text.chars().count(),
            "redaction changed the length of a message it had nothing to mask in"
        );
    }

    /// The cap counts **characters** and leaves anything under it untouched.
    ///
    /// A byte-counted slice panics the moment index `CAP` lands inside a
    /// multi-byte sequence, which a runner's non-ASCII path in a `fatal:` line
    /// reaches on an ordinary day. The `é` fixture puts the boundary there
    /// deliberately.
    ///
    /// Reds on: `&text[..CAP]` (the multi-byte row panics); returning `self`
    /// unchanged (the long row is not shortened); appending the marker
    /// unconditionally (the short row grows one).
    #[test]
    fn capping_counts_characters_and_leaves_short_text_alone() {
        let short = redact("fatal: could not read from the remote repository", &[]).capped();
        assert_eq!(
            short.as_str(),
            "fatal: could not read from the remote repository",
            "text under the cap must be returned unchanged, marker included"
        );

        let long = redact(&"é".repeat(Redacted::CAP + 50), &[]).capped();
        assert_eq!(
            long.as_str().chars().take_while(|character| *character == 'é').count(),
            Redacted::CAP,
            "the cap counts characters, not bytes"
        );
        assert!(
            long.as_str().ends_with("... [truncated]"),
            "a shortened message must say so: {}",
            long.as_str()
        );
    }

    // ---- the child environment (C-019, C-035) -------------------------------

    /// The child is built from [`Env::clean`], so nothing the parent holds
    /// reaches it unless a table names it.
    ///
    /// Asserted **positively**, as a subset of the tables, rather than as "some
    /// named variable is absent": `Env::new` is what this guards against and an
    /// absence assertion is green under `Env::new` on any runner that happens
    /// not to export that particular name. Under `Env::new` the test process's
    /// own `CARGO_*`, `RUSTUP_*` and `LD_LIBRARY_PATH` appear instead, on every
    /// machine, with no ambient manipulation and no `unsafe`.
    ///
    /// Reds on: constructing with `Env::new()` instead of `Env::clean()`.
    #[test]
    fn git_child_env_is_built_from_clean() {
        let child = git_child_env(
            parent_env(&[
                ("PATH", "/usr/bin"),
                ("HOME", "/home/publisher"),
                ("GIT_TRACE", "1"),
                ("CARGO_MANIFEST_DIR", "/w/ocx"),
                ("LD_LIBRARY_PATH", "/w/lib"),
                ("OCX_ANNOUNCE_TOKEN", "glpat-notarealvalue"),
            ]),
            None,
            LazyFetch::Allow,
        );

        let allowed: Vec<&str> = table::PASSTHROUGH
            .iter()
            .copied()
            .chain(table::SET.iter().map(|(name, _)| *name))
            .chain(table::INJECTED.iter().copied())
            .collect();
        let mut leaked: Vec<String> = child
            .iter()
            .map(|(name, _)| name.to_string_lossy().into_owned())
            .filter(|name| !allowed.iter().any(|allowed| allowed.eq_ignore_ascii_case(name)))
            .collect();
        leaked.sort();

        assert!(
            leaked.is_empty(),
            "the child inherited names no table admits, so it was not built from a clean env:\n{}",
            leaked.join("\n")
        );
        // The falsifying half: a child with zero keys satisfies the subset
        // assertion above in every state of the code.
        assert!(child.iter().next().is_some(), "the child environment is empty");
    }

    /// Every table row is asserted **by name**, and the platform tables are
    /// asserted on **both** platforms.
    ///
    /// The naive spelling of the Windows half — a `#[cfg(windows)]` test over a
    /// `#[cfg(windows)]` table — never runs on Linux CI, so it is green because
    /// it never ran. The tables are unconditional data; only which one is *live*
    /// is chosen by `#[cfg]`, and that choice is the one thing asserted under a
    /// `cfg`.
    ///
    /// Reds on: dropping `PATH` from a passthrough table; dropping
    /// `GIT_TERMINAL_PROMPT` from `SET`; setting `GIT_CONFIG_NOSYSTEM` to `0`;
    /// dropping `SYSTEMROOT` from `WINDOWS_PASSTHROUGH` (**on Linux**, which is
    /// what proves the Windows arm is pinned rather than merely written);
    /// listing only the lowercase proxy spellings.
    #[test]
    fn git_child_env_allowlist_tables_are_complete() {
        for required in [
            "PATH",
            "HOME",
            "http_proxy",
            "https_proxy",
            "no_proxy",
            "all_proxy",
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "NO_PROXY",
            "ALL_PROXY",
            "GIT_SSL_CAINFO",
            "GIT_SSL_CAPATH",
            "SSL_CERT_FILE",
            "SSL_CERT_DIR",
            "TMPDIR",
            "TEMP",
            "TMP",
        ] {
            assert!(
                table::UNIX_PASSTHROUGH.contains(&required),
                "{required} must pass through on Unix"
            );
            assert!(
                table::WINDOWS_PASSTHROUGH.contains(&required),
                "{required} must pass through on Windows"
            );
        }
        for windows_only in ["USERPROFILE", "HOMEDRIVE", "HOMEPATH", "SYSTEMROOT"] {
            assert!(
                table::WINDOWS_PASSTHROUGH.contains(&windows_only),
                "{windows_only} must pass through on Windows — pinned here because Linux CI cannot exercise it"
            );
        }

        for (name, value) in [
            ("GIT_TERMINAL_PROMPT", "0"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_AUTHOR_NAME", "ocx"),
            ("GIT_AUTHOR_EMAIL", "noreply@ocx.sh"),
            ("GIT_COMMITTER_NAME", "ocx"),
            ("GIT_COMMITTER_EMAIL", "noreply@ocx.sh"),
            ("LC_ALL", "C"),
            ("LANGUAGE", ""),
        ] {
            assert!(
                table::SET.contains(&(name, value)),
                "ocx must set {name}={value} in the child"
            );
        }
        assert_eq!(
            table::SET.len(),
            8,
            "a row was added to SET without a by-name assertion here"
        );

        assert_eq!(
            table::NO_LAZY_FETCH,
            &[("GIT_NO_LAZY_FETCH", "1")],
            "the local lane refuses the promisor fetch, and only that"
        );

        #[cfg(not(windows))]
        assert_eq!(
            table::PASSTHROUGH,
            table::UNIX_PASSTHROUGH,
            "the live table on this platform is the Unix one"
        );
        #[cfg(windows)]
        assert_eq!(
            table::PASSTHROUGH,
            table::WINDOWS_PASSTHROUGH,
            "the live table on this platform is the Windows one"
        );

        // And the tables are not merely written: the values a parent holds for
        // them reach the child, and the values ocx sets are the ones set.
        let parent: Vec<(&str, &str)> = table::PASSTHROUGH
            .iter()
            .map(|name| (*name, "carried-verbatim"))
            .collect();
        let child = git_child_env(parent_env(&parent), None, LazyFetch::Allow);
        for name in table::PASSTHROUGH {
            assert_eq!(
                child_value(&child, name).as_deref(),
                Some("carried-verbatim"),
                "{name} is in the passthrough table but did not reach the child"
            );
        }
        for (name, value) in table::SET {
            assert_eq!(
                child_value(&child, name).as_deref(),
                Some(*value),
                "ocx must set {name} in the child"
            );
        }
    }

    /// The NEVER table is load-bearing only as **data**: asking the built child
    /// whether it holds `GIT_TRACE` is green in every state of the code,
    /// including with the whole table deleted, because nothing is copied unless
    /// a table names it. Disjointness is the assertion that reds.
    ///
    /// Reds on: adding `GIT_TRACE` (or any other forbidden prefix) to a
    /// passthrough table — which is precisely the change the NEVER table exists
    /// to stop.
    #[test]
    fn the_never_table_is_disjoint_from_every_name_the_child_gets() {
        let admitted: Vec<&str> = table::UNIX_PASSTHROUGH
            .iter()
            .chain(table::WINDOWS_PASSTHROUGH.iter())
            .copied()
            .chain(table::SET.iter().map(|(name, _)| *name))
            .chain(table::INJECTED.iter().copied())
            .chain(table::NO_LAZY_FETCH.iter().map(|(name, _)| *name))
            .collect();

        for name in &admitted {
            for forbidden in table::NEVER {
                assert!(
                    !name.starts_with(forbidden),
                    "{name} is admitted into the child but {forbidden} must never reach it"
                );
            }
        }
        // The falsifying half: an empty NEVER table satisfies the loop above.
        assert!(
            table::NEVER.contains(&"GIT_TRACE") && table::NEVER.contains(&"CI_JOB_TOKEN"),
            "the NEVER table must actually name the families it forbids"
        );
    }

    /// A passthrough name that is present but **empty** is carried verbatim.
    ///
    /// `http_proxy=` means "no proxy" to git. Dropping it instead lets
    /// `$HOME/.gitconfig`'s `http.proxy` win — a different answer, arrived at
    /// silently.
    ///
    /// Reds on: filtering empty values out of the passthrough loop.
    #[test]
    fn an_empty_passthrough_value_reaches_the_child_verbatim() {
        let child = git_child_env(
            parent_env(&[("http_proxy", ""), ("PATH", "/usr/bin")]),
            None,
            LazyFetch::Allow,
        );
        assert_eq!(
            child_value(&child, "http_proxy").as_deref(),
            Some(""),
            "an empty http_proxy means `no proxy` and must not be dropped"
        );
    }

    /// The credential triple is a **conditional group**, not three more `SET`
    /// rows: it is present on an invocation that injects an ocx credential and
    /// on no other.
    ///
    /// C-035's own prose lists the three flat, which contradicts C-034's "only
    /// those" and would make "no `extraHeader` is configured" unassertable —
    /// under a flat table every invocation carries one. When nothing is
    /// injected, git's own credential helpers stay in charge, which is the whole
    /// point of the third precedence rung.
    ///
    /// Reds on: setting the triple unconditionally.
    #[test]
    fn the_credential_triple_is_conditional_on_an_injected_credential() {
        let without = git_child_env(parent_env(&[("PATH", "/usr/bin")]), None, LazyFetch::Allow);
        for name in table::INJECTED {
            assert_eq!(
                child_value(&without, name),
                None,
                "{name} must be absent when no ocx credential is injected"
            );
        }

        let credential = GitPushCredential {
            username: "gitlab-ci-token".to_string(),
            secret: ForgeToken::new("glpat-notarealpushvalue".to_string()),
        };
        let scope = project_scope();
        let injection = CredentialInjection {
            url_prefix: &scope,
            credential: &credential,
        };
        let with = git_child_env(parent_env(&[("PATH", "/usr/bin")]), Some(&injection), LazyFetch::Allow);

        // Built from the injection's own prefix rather than from a parallel
        // literal: a scope assertion fed from a second copy of the string agrees
        // with itself no matter what the builder placed.
        let expected_key = format!("http.{}.extraHeader", injection.url_prefix);
        assert_eq!(child_value(&with, "GIT_CONFIG_COUNT").as_deref(), Some("1"));
        assert_eq!(
            child_value(&with, "GIT_CONFIG_KEY_0").as_deref(),
            Some(expected_key.as_str()),
            "the scope must carry the full project path, never just the host"
        );
    }

    /// `GIT_NO_LAZY_FETCH` is conditional on the lane, and the lane is not the
    /// credential.
    ///
    /// Both halves matter. Absent on a network invocation, `fetch` and `push`
    /// keep the promisor semantics their pack generation depends on. Present on
    /// a local one, a plumbing command that needs a missing object fails as
    /// `invalid object` instead of dialling a remote it holds no credential for
    /// and being reported as an auth error ([#428]).
    ///
    /// The pairing with `injection` is what pins the seam: an implementation
    /// that derived the lane from `injection.is_none()` would set the name on
    /// the credential-less network fetch of precedence rung three, so both rows
    /// here carry a credential state that contradicts their lane.
    ///
    /// Reds on: moving the name into `SET`; keying it on `injection`.
    ///
    /// [#428]: https://github.com/ocx-sh/ocx/issues/428
    #[test]
    fn the_lazy_fetch_refusal_is_conditional_on_the_lane_not_the_credential() {
        let credential = GitPushCredential {
            username: "gitlab-ci-token".to_string(),
            secret: ForgeToken::new("glpat-notarealpushvalue".to_string()),
        };
        let scope = project_scope();
        let injection = CredentialInjection {
            url_prefix: &scope,
            credential: &credential,
        };

        // Local, and injecting: still refused.
        let local = git_child_env(parent_env(&[("PATH", "/usr/bin")]), Some(&injection), LazyFetch::Refuse);
        assert_eq!(
            child_value(&local, "GIT_NO_LAZY_FETCH").as_deref(),
            Some("1"),
            "a local invocation must not resolve a missing object over the network"
        );

        // Network, and injecting nothing — rung three's own shape: still allowed.
        let network = git_child_env(parent_env(&[("PATH", "/usr/bin")]), None, LazyFetch::Allow);
        assert_eq!(
            child_value(&network, "GIT_NO_LAZY_FETCH"),
            None,
            "fetch and push generate packs against the promisor remote"
        );
    }

    /// The header the child is given decodes back to the credential that was
    /// resolved — the C-022 agreement between injector and redactor, asserted at
    /// the one place both are fed from.
    ///
    /// A wrong-value agreement is the failure this catches: injecting the API
    /// credential while the redactor masks the push credential produces a run
    /// that authenticates as nobody *and* prints the injected secret.
    ///
    /// The encoding is also what neutralises a username or secret carrying
    /// `\r\n`: base64's alphabet cannot spell a header separator. The real
    /// mutation is therefore *removing* the encoding, which the shape assertion
    /// catches.
    ///
    /// Reds on: encoding `secret:username`; emitting `Basic {user}:{secret}`
    /// unencoded; splitting on the last `:` instead of the first.
    #[test]
    fn injected_header_decodes_to_the_resolved_credential() {
        let credential = GitPushCredential {
            username: "gitlab-ci-token".to_string(),
            // A secret carrying a colon and a CRLF: HTTP Basic has no escaping,
            // so only "split on the first colon" and the encoding keep it whole.
            secret: ForgeToken::new("glpat-not:a:real\r\nvalue".to_string()),
        };
        let scope = project_scope();
        let injection = CredentialInjection {
            url_prefix: &scope,
            credential: &credential,
        };
        let child = git_child_env(parent_env(&[]), Some(&injection), LazyFetch::Allow);

        let value = child_value(&child, "GIT_CONFIG_VALUE_0").expect("the credential value must be injected");
        let header = value
            .strip_prefix("Authorization: ")
            .unwrap_or_else(|| panic!("the injected value must be an Authorization header: {value}"));
        let encoded = header
            .strip_prefix("Basic ")
            .unwrap_or_else(|| panic!("the credential must travel as HTTP Basic: {header}"));
        assert!(
            encoded
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=')),
            "the credential reached the header unencoded: {header}"
        );

        let decoded = BASE64_STANDARD.decode(encoded).expect("the header must be base64");
        let pair = String::from_utf8(decoded).expect("the pair must be UTF-8");
        let (username, secret) = pair
            .split_once(':')
            .unwrap_or_else(|| panic!("the pair must be `user:secret`: {pair:?}"));
        // Read back through the injection, not through the local binding: the
        // agreement C-022 asks for is between what the injector was *given* and
        // what the child got, and a second reference to the same literal would
        // hold even if the injection carried something else entirely.
        assert_eq!(
            username, injection.credential.username,
            "the user half must be the resolved username"
        );
        assert_eq!(
            secret, injection.credential.secret.0,
            "the secret half must be the resolved secret"
        );
    }

    /// The child process runs the `git` the gate resolved, in the workspace, and
    /// nothing else: the builder takes a [`GitBinary`], so a sibling module that
    /// routes through this file can start *git* and no other program. That is
    /// C-021's "no wrapper that lets a sibling spawn" honoured by signature,
    /// which is the only way to honour it — the firewall's source-text search
    /// cannot see the difference between a helper that takes a resolved binary
    /// and one that takes an arbitrary path.
    ///
    /// Reds on: spawning `"git"` by name; ignoring `workdir`.
    #[test]
    fn git_child_command_runs_the_resolved_binary_in_the_workspace() {
        let git = GitBinary {
            path: PathBuf::from("/opt/ocx/libexec/git"),
            version: GitVersion::MINIMUM,
        };
        let workdir = Path::new("/tmp/ocx-claim-workspace");

        let command = git_child_command(&git, workdir, &Env::clean());
        let raw = command.as_std();

        assert_eq!(
            raw.get_program(),
            git.path.as_os_str(),
            "the resolved binary must be spawned"
        );
        assert_eq!(raw.get_current_dir(), Some(workdir), "git must run in the workspace");
    }

    /// A scope that names no project is refused, so a producer cannot widen the
    /// credential to a whole host or a whole group.
    ///
    /// The falsifying assertion the string-typed field could not carry: the old
    /// scope test built its expectation *from* `url_prefix`, so it agreed with a
    /// host-only value in every state. Here the constructor is the subject.
    ///
    /// Reds on: accepting any prefix (`Ok(Self(..))` unconditionally); counting
    /// empty segments, which makes `https://gitlab.example//` pass with two;
    /// requiring only one segment, which admits the group scope.
    #[test]
    fn a_credential_scope_refuses_a_prefix_that_names_no_project() {
        for too_broad in [
            "https://gitlab.example",
            "https://gitlab.example/",
            "https://gitlab.example//",
            "https://gitlab.example:8443",
            "https://gitlab.example/acme",
            "https://gitlab.example/acme/",
        ] {
            let error = CredentialScope::new(too_broad)
                .err()
                .unwrap_or_else(|| panic!("a scope naming no project must be refused: {too_broad}"));
            let ForgeError::GitUnavailable { reason } = &error else {
                panic!("expected GitUnavailable, got {error:?}");
            };
            assert!(
                reason.contains(too_broad),
                "the refusal must name the scope it refused: {reason}"
            );
        }

        for project in [
            "https://gitlab.example/acme/index",
            "https://gitlab.example/acme/platform/tooling/index",
            "https://gitlab.example:8443/acme/index",
        ] {
            let scope = CredentialScope::new(project).expect("a scope naming a project must be accepted");
            assert_eq!(scope.to_string(), project, "the scope must render what it was given");
        }
    }

    /// [`run_git`] forwards its arguments, builds the C-035 environment itself,
    /// and hands the whole capture back.
    ///
    /// The environment assertion is the one that pins the seam: `run_git` takes
    /// no `Env`, so a caller cannot compose one — the child's
    /// `GIT_TERMINAL_PROMPT=0` can only have come from [`git_child_env`] inside
    /// this function.
    ///
    /// Reds on: dropping `args`; accepting a caller-built environment (or
    /// starting from `Env::new()`); ignoring `workdir`; discarding the capture
    /// or the exit status.
    #[cfg(unix)]
    #[tokio::test]
    async fn run_git_forwards_its_arguments_and_builds_its_own_environment() {
        let directory = tempfile::TempDir::new().expect("temp dir");
        let path = shim(
            directory.path(),
            "echo \"args=$*\"; echo \"prompt=${GIT_TERMINAL_PROMPT-unset}\"; pwd -P; exit 7",
        );
        let git = GitBinary {
            path,
            version: GitVersion::MINIMUM,
        };
        let workdir = std::fs::canonicalize(directory.path()).expect("canonical workdir");

        let output = run_git(
            &git,
            &workdir,
            None,
            LazyFetch::Refuse,
            &[OsStr::new("rev-parse"), OsStr::new("--verify"), OsStr::new("HEAD")],
        )
        .await
        .expect("the shim must run");

        assert_eq!(
            output.status.code(),
            Some(7),
            "the child's status must reach the caller"
        );
        let stdout = String::from_utf8(output.stdout).expect("the shim prints UTF-8");
        assert!(
            stdout.contains("args=rev-parse --verify HEAD"),
            "the arguments must reach the child in order: {stdout}"
        );
        assert!(
            stdout.contains("prompt=0"),
            "the child environment must be built here, from the C-035 tables: {stdout}"
        );
        assert!(
            stdout.contains(&workdir.display().to_string()),
            "git must run in the workspace: {stdout}"
        );
    }
}
