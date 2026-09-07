// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The `git` executable this crate drives, and the one seam that runs it.
//!
//! **This file is the only one on the git-transport path permitted to name a
//! child-process builder.** Everything else — the workspace, the push-option
//! renderer, the stderr classifier — asks this module. It exports no alias, no
//! wrapper and no re-export that would let a sibling start a process without
//! naming one itself, because that is the single evasion the process firewall's
//! source-text search structurally cannot see.
//!
//! The child's environment is built from an allowlist and never inherited, the
//! credential travels through git's own configuration environment rather than
//! argv or a URL, and every captured byte is masked before it reaches an error
//! or a log.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use super::{ForgeError, GitPushCredential};
use crate::env::Env;

/// A resolved `git` executable and the version it reported.
///
/// Produced once, by the argv-boundary gate, and carried into the forge
/// constructor — so the version check happens before any network call, and the
/// row it produces in the write preflight is rendered from the same value that
/// was checked rather than from a second probe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitBinary {
    /// The executable as it was resolved on `PATH`.
    pub path: PathBuf,
    /// The version it reported.
    pub version: GitVersion,
}

/// A `git` version as the plain `(major, minor, patch)` triple, ordered
/// lexicographically by the derive.
///
/// Deliberately **not** [`crate::package::version::Version`]. That type
/// implements rolling-parent ordering, under which a two-component `2.31`
/// compares *greater* than `2.31.0` — correct for a package tag that stands for
/// "the newest 2.31.x", and wrong here, where a `parsed >= floor` gate written
/// against it would reject git 2.31.0: the exact release the floor is named for.
/// A local triple with a derived `Ord` is the whole of what this comparison
/// needs.
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
    ///
    /// An associated constant rather than a module-level one so the floor is
    /// reachable from outside the crate through the same re-export the type
    /// travels on, and so the gate and every test naming the boundary read one
    /// value.
    pub const MINIMUM: Self = Self::new(2, 31, 0);

    /// The triple, as a `const` so a floor can be written as one.
    #[must_use]
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self { major, minor, patch }
    }

    /// The version `git --version` reported, or `None` when its output carries
    /// none.
    ///
    /// **Anchored on a line beginning `git version `**, and the first
    /// `<digits>.<digits>[.<digits>]` on *that* line. C-020's original wording —
    /// "the first `\d+\.\d+(\.\d+)?` out of `git --version`" — ships a defect
    /// and is corrected here (DX-96): a `git` whose stdout carries a preamble
    /// (`warning: something 1.2 happened` before the real line) parses as `1.2`
    /// under the unanchored rule, so a perfectly good git 2.31.0 is refused. The
    /// same shape reaches the probe from `GIT_TRACE=1`, whose timestamp
    /// (`28.682964`) matches the naive pattern too — which is why `GIT_TRACE*`
    /// sits in the NEVER table *and* why this reads stdout only. Two guards, one
    /// property; both are mutated separately.
    ///
    /// A two-component `2.31` yields `(2, 31, 0)`. No `git` in the wild prints
    /// two components — 2.54.0 prints three and git-for-Windows prints four
    /// (`2.54.0.windows.1`, which yields `(2, 54, 0)`) — so the two-component
    /// arm is a parser contract over a fabricated line, not observed behaviour.
    ///
    /// Returns `None` rather than panicking or saturating when a component does
    /// not fit a `u32`: a shim printing `999999999999.1.0` is unusable input,
    /// not a crash and not a version.
    #[must_use]
    pub fn from_version_output(stdout: &str) -> Option<Self> {
        // The anchor is the whole correction: read unanchored, a preamble's
        // `warning: something 1.2 happened` — or a `GIT_TRACE` timestamp —
        // parses as the version and a perfectly good git 2.31.0 is refused.
        let reported = stdout
            .lines()
            .find_map(|line| line.strip_prefix("git version "))?
            .split_whitespace()
            .next()?;

        let mut components = reported.split('.');
        let major: u32 = components.next()?.parse().ok()?;
        let minor: u32 = components.next()?.parse().ok()?;
        // Anything past the third component is build metadata
        // (`2.54.0.windows.1`); a missing third component is zero, never "the
        // newest 2.31.x".
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

/// Resolve `git` on `PATH` and read its version, refusing anything below the
/// floor the git write transport needs.
///
/// Runs at the argv boundary, before any network call, so a host that cannot
/// serve the transport costs one local invocation and reaches no forge.
///
/// Resolution is ocx's own, against the `PATH` **ocx** reads
/// ([`crate::env::var`]); the child's own `PATH` comes from
/// `std::env::var_os` through the passthrough table, which is the same value
/// outside the test seam. The absolute result is what is spawned. `Command::new("git")` would resolve
/// against the *parent* process's `PATH` through whichever of `posix_spawnp` /
/// fork-exec the standard library picked, so [`GitBinary::path`]'s promise —
/// "the executable as it was resolved on `PATH`" — would be a claim about a
/// search ocx did not perform.
///
/// # Errors
///
/// Returns [`ForgeError::GitUnavailable`] when `git` is absent, cannot be run,
/// reports a version that cannot be parsed, or reports one below the floor.
pub async fn probe_git_binary() -> Result<GitBinary, ForgeError> {
    // A failed `current_dir` only costs the resolution of a *relative* `PATH`
    // entry, which no sane `PATH` carries; an absolute one resolves either way.
    let cwd = std::env::current_dir().unwrap_or_default();
    let search_path = crate::env::var("PATH");
    let resolved = which::which_in("git", search_path.as_deref(), cwd).map_err(|error| ForgeError::GitUnavailable {
        reason: format!("git could not be resolved on PATH: {error}"),
    })?;
    probe_git_binary_at(&resolved).await
}

/// [`probe_git_binary`] against an explicitly named executable.
///
/// Split out because `probe_git_binary` takes no argument and so can only ever
/// observe *this host's* `git` — its green says nothing about the floor, the
/// parse, or the absent case, and the three tests named for those properties
/// could not discriminate at all (DX-95). Everything except the `PATH` search
/// lives here, where a test can point it at a shim.
///
/// A non-zero exit carries **git's own stderr** into the reason, redacted and
/// capped. That is not defensive padding: `GIT_CONFIG_NOSYSTEM=1` suppresses
/// `/etc/gitconfig` and nothing else, `HOME` is a deliberate passthrough
/// (C-033), and a malformed `$HOME/.gitconfig` therefore makes `git --version`
/// exit 128 with **empty stdout** on a perfectly healthy `git` — measured. A
/// reason reading "git is absent" sends that operator hunting for a binary they
/// already have.
///
/// # Errors
///
/// Returns [`ForgeError::GitUnavailable`] when `program` cannot be run, exits
/// non-zero, prints output no version can be parsed from, or reports a version
/// below [`GitVersion::MINIMUM`].
pub(super) async fn probe_git_binary_at(program: &Path) -> Result<GitBinary, ForgeError> {
    let unavailable = |reason: String| ForgeError::GitUnavailable { reason };

    // Built here rather than through `git_child_command`: a `--version` probe
    // has no workspace to run in and no resolved version to hand that builder,
    // which is the very thing this call is about to establish.
    let output = tokio::process::Command::new(program)
        .arg("--version")
        .env_clear()
        .envs(git_child_env(|name| std::env::var_os(name), None, LazyFetch::Refuse).iter())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|error| unavailable(format!("{} could not be run: {error}", program.display())))?;

    // Every captured byte goes through `redact` before it is formatted into
    // anything (C-019). Nothing is injected on a `--version`, so the secret
    // slice is empty and this call buys the ordering, not a mask.
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

/// C-035's child-environment allowlist, as data.
///
/// One table per platform rather than one table with `#[cfg]` rows, so the
/// Windows arm is **pinned and reviewable on Linux where it cannot be
/// exercised**. The naive spelling of that — a `#[cfg(windows)]` test over a
/// `#[cfg(windows)]` table — never runs on Linux CI and is therefore green
/// because it never ran. Here the tables are unconditional data and only
/// [`PASSTHROUGH`] is selected by `#[cfg]`, so a Linux run asserts both.
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
    /// Upper- and lower-case proxy spellings are **distinct names** on Unix —
    /// `EnvKey` folds case only under `#[cfg(windows)]` — so a table holding
    /// only the lowercase forms loses the proxy on every runner that exports
    /// `HTTPS_PROXY`. Both forms are listed on both platforms; the Windows
    /// duplicate collapses harmlessly under the folding.
    ///
    /// `PATH` stays here even though the probe resolves `git` absolutely: `git`
    /// itself resolves `git-remote-https`, `ssh` and every hook off `PATH`.
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

    /// The Windows arm of [`UNIX_PASSTHROUGH`]: the same proxy, TLS and
    /// temporary-directory names, with Windows' own home-directory triple and
    /// `SYSTEMROOT`, which the Windows CRT and the TLS stack both need.
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
    /// The commit identity is the fixed `ocx <noreply@ocx.sh>` of C-045 and
    /// never tracks `--owner`; setting all four names is what makes the identity
    /// ocx's rather than whatever the operator's `$HOME/.gitconfig` says.
    ///
    /// `LANGUAGE=` is **behaviourally redundant** given `LC_ALL=C` — measured:
    /// `LC_ALL=C LANGUAGE=de git rev-parse` answers in English, while
    /// `LC_ALL=en_AU.utf8 LANGUAGE=de` answers in German. It is kept because it
    /// costs nothing, but it is falsifiable only by the by-name assertion over
    /// this table: a behavioural locale test stays green with the row deleted,
    /// which is the "two guards, one property" case `quality-core.md` says to
    /// keep mutating past.
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

    /// The credential triple, present **only** on an invocation that injects an
    /// ocx credential.
    ///
    /// A conditional group, not three more [`SET`] rows. C-035's own prose lists
    /// them flat, which contradicts C-034's "every invocation that injects an ocx
    /// credential and **only those**" and would make the scenario that asserts no
    /// `extraHeader` is configured unimplementable — under a flat table every
    /// invocation carries one (DX-97).
    pub const INJECTED: &[&str] = &["GIT_CONFIG_COUNT", "GIT_CONFIG_KEY_0", "GIT_CONFIG_VALUE_0"];

    /// Set on a **local** invocation and on no other.
    ///
    /// A conditional group for the same reason [`INJECTED`] is, and the
    /// condition is the opposite one. The fetch is deliberately blobless
    /// (C-036), so every blob the base tree names is a promisor object the
    /// checkout does not hold — and git resolves a missing object by *fetching
    /// it*, from a local plumbing command that carries no credential. That dial
    /// is answered 401, `GIT_TERMINAL_PROMPT` refuses the prompt, and the
    /// classifier reads git's own words as a rejected credential: an auth error
    /// naming a credential that is fine.
    ///
    /// So on a git that honours this variable, the local lane refuses the lazy
    /// fetch outright and a missing object fails as `invalid object` in the
    /// command that needed it — the truthful error. It cannot ride [`SET`]:
    /// `fetch` and `push` generate packs against the promisor remote and
    /// legitimately depend on the behaviour this removes.
    ///
    /// **Best-effort below the git that documents honouring it.** `GIT_NO_LAZY_FETCH`
    /// itself is undocumented; its flag twin, `--no-lazy-fetch`, first appears in
    /// git's release notes at 2.45.0 — above [`GitVersion::MINIMUM`] (2.31.0), the
    /// floor this crate otherwise enforces. Below whatever version actually reads
    /// it, this row is inert and a missing object still triggers the
    /// credential-less fetch it exists to prevent; `write-tree --missing-ok`
    /// (`git_workspace`) closes that gap independently of the git version.
    pub const NO_LAZY_FETCH: &[(&str, &str)] = &[("GIT_NO_LAZY_FETCH", "1")];

    /// Name **prefixes** that must never reach the child, whatever the parent
    /// holds.
    ///
    /// Prefixes rather than exact names because `GIT_TRACE` is a family
    /// (`GIT_TRACE2`, `GIT_TRACE_PACKET`, `GIT_TRACE_CURL`…) and `OCX_ANNOUNCE_`
    /// covers every ocx credential variable at once.
    ///
    /// This table cannot be checked by asking the child whether it holds one of
    /// these: the child is built by construction, so nothing is copied unless a
    /// table above names it, and "`GIT_TRACE` is absent" is green in every state
    /// of the code — including with this whole table deleted. What makes it
    /// load-bearing is the **disjointness** assertion against the tables above,
    /// which reds the moment a name is added to one of them.
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
/// C-034's rule — the prefix carries the **full project path**, never just the
/// host — is what bounds how far the operator's credential travels. A prefix
/// that is too narrow fails closed and loudly; one that is too broad **fails
/// open and silently**, applying the `Authorization` header to every repository
/// under it for the lifetime of the invocation.
///
/// As a `&str` that rule was prose held at one site and violable at another: the
/// producer is a *different* file (the workspace), and the scope test could not
/// see a violation because it built its expectation *from* the string it was
/// handed. So it is a type. The field is private and this module has no
/// submodules, which makes [`CredentialScope::new`] — and its refusal — the only
/// way to obtain one; a struct literal in any sibling under `forge` is `E0451`.
/// Same placement argument [`Redacted`] and [`super::PushAccess`] record for
/// their own private fields.
///
/// *Reading* is unrestricted: the scope is not a secret. Its width is the
/// decision, and that is what the constructor gates.
pub(super) struct CredentialScope(String);

impl CredentialScope {
    /// The scope for `prefix`, refusing one that names no project.
    ///
    /// A forge coordinate is `NAMESPACE/PROJECT` and the namespace may itself
    /// nest (`acme/platform/tooling/index`), so **every** real coordinate has at
    /// least two path segments. Fewer means the caller widened the scope:
    /// `https://gitlab.example` covers a whole host and
    /// `https://gitlab.example/acme` a whole group — both the silent widening
    /// C-034 forbids, neither distinguishable from the correct value by any
    /// assertion written against the string alone.
    ///
    /// # Errors
    ///
    /// Returns [`ForgeError::GitUnavailable`] when `prefix` carries fewer than
    /// two path segments. That is C-034's "fails closed and loudly" half: the
    /// transport refuses to start rather than injecting the credential wider
    /// than the repository it is announcing to.
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

/// The credential C-034 injects, and the [`CredentialScope`] it is injected
/// under.
///
/// The scope is the workspace's to compute; this module only places it, and the
/// type is what stops it being computed too widely.
pub(super) struct CredentialInjection<'a> {
    /// The `http.<prefix>.extraHeader` scope, e.g. `https://gitlab.example/acme/index`.
    pub url_prefix: &'a CredentialScope,
    /// The credential whose `Authorization` header is injected under it.
    pub credential: &'a GitPushCredential,
}

/// Build the child environment for a `git` invocation from
/// [`Env::clean`] and the tables above.
///
/// `lookup` is the parent-environment reader — `std::env::var_os` in
/// production. It is a parameter rather than a direct call because
/// [`Env::new`] bypasses `crate::env`'s test seam entirely (it reads
/// `std::env::vars_os` itself), `std::env::set_var` is `unsafe` in Rust 2024 and
/// races every other test in the binary, and without an injectable reader not
/// one passthrough row would be falsifiable.
///
/// A passthrough name that is **present but empty** is carried verbatim.
/// `http_proxy=` means "no proxy" to git; dropping it instead lets
/// `$HOME/.gitconfig`'s `http.proxy` win, which is a different answer arrived at
/// silently.
pub(super) fn git_child_env(
    lookup: impl Fn(&str) -> Option<OsString>,
    injection: Option<&CredentialInjection<'_>>,
    lazy_fetch: LazyFetch,
) -> Env {
    // `Env::clean` is the whole allowlist: `Env::new` (and `Env::default`,
    // which delegates to it) collects `std::env::vars_os`, so the child would
    // start from everything the parent holds and the tables below would only
    // ever add to it.
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
/// The distinction the credential cannot express: precedence rung three injects
/// nothing and still talks to the remote, so `injection.is_none()` is not "this
/// is local". Local and network are a property of the *call*, and the caller is
/// the only thing that knows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LazyFetch {
    /// A network invocation: promisor semantics stay on.
    Allow,
    /// A local plumbing invocation: a missing object is an error, never a fetch.
    Refuse,
}

/// Run one `git` invocation to completion and hand back what it printed.
///
/// **The only way any other module starts a `git`.** [`git_child_command`]
/// below stays private, and no item this file exports is a
/// `tokio::process::Command` or yields one — which is what C-021's "no alias,
/// re-export or wrapper that would let a sibling spawn" actually asks for.
/// Exporting the *builder* would have satisfied the firewall's source-text
/// search and defeated the rule it encodes: a sibling holding a `Command` can
/// put `GIT_TRACE` back into the environment and a credential-bearing URL onto
/// argv without naming a `SPAWN_TOKENS` spelling anywhere in its own text, and
/// the Constitution deviation this file is filed under says in its own words
/// that "privacy is what actually holds", not the search.
///
/// So a caller steers the two things it legitimately owns — the arguments, and
/// whether a credential is injected — and nothing else. The program is the
/// resolved [`GitBinary`], and the environment is built *here* from
/// [`git_child_env`] rather than accepted, so C-019's `Env::clean()` clause and
/// the whole of C-035 hold for every invocation instead of being a step each
/// caller has to remember.
///
/// Returns the raw capture, C-019's `(ExitStatus, Vec<u8>, Vec<u8>)`. Masking is
/// the caller's, at the error boundary: [`redact`] is [`Redacted`]'s only
/// constructor, and the stderr classifier needs the whole uncapped text before
/// anything is shortened.
///
/// This is also the one place the deferred `tokio::time::timeout` lands if that
/// decision is reopened — one seam, not one per call site.
///
/// # Errors
///
/// Returns [`ForgeError::GitUnavailable`] when the child cannot be started at
/// all. A `git` that ran and failed is **not** an error here: its status and
/// stderr come back in the capture, because only the caller knows whether a
/// non-zero exit is a [`ForgeError::GitCommandFailed`] or a
/// [`ForgeError::GitPushFailed`].
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
        // Redacted for the ordering C-019 asks for rather than for a mask: a
        // spawn failure is the OS talking about a path, and the credential
        // lives in an environment this call never handed to anyone.
        reason: redact(&format!("{} could not be run: {error}", git.path.display()), &[])
            .capped()
            .to_string(),
    })
}

/// The child-process builder for one `git` invocation.
///
/// The only item in this crate outside `launch` that names one, which is what
/// the `SPAWN_ALLOWED` row for this file buys — and it is **private**, so the
/// row's claim is held by the module system rather than by a search. Sibling
/// modules reach the spawn through [`run_git`] alone.
fn git_child_command(git: &GitBinary, workdir: &Path, env: &Env) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(&git.path);
    command
        .current_dir(workdir)
        .env_clear()
        .envs(env.iter())
        // A cancelled announce must not leave a `git` holding the workspace
        // open past the guard that removes it.
        .kill_on_drop(true);
    command
}

/// Text that has been through `redact` below, and the only thing the two
/// `stderr`-bearing [`ForgeError`] variants accept.
///
/// The obligation lives on the type because `git`'s stderr is the one place a
/// secret arrives *inside* forge-controlled bytes rather than beside them — a
/// credential helper, a proxy, or a server echoing a request header back writes
/// the credential into the very text an error is then built from. A doc comment
/// asking a future author to mask it first is advice; a field that cannot be
/// filled without calling `redact` is a compile error.
///
/// The field is private and this module has no submodules, so the only code
/// that can write one is this file. Every other forge module — the stderr
/// classifier, the workspace, `error.rs` itself — is a *sibling* under `forge`,
/// not a descendant, so a struct literal there is `E0451`. Declaring the type
/// one level up in `forge.rs` would have inverted exactly that: a private field
/// is visible to descendants, and every forge submodule is one. Same placement
/// reasoning [`super::PushAccess`] records for its own private field.
///
/// *Reading* is unrestricted — the classifier has to match phrases against
/// these bytes. It is construction that is gated.
#[derive(Debug)]
pub struct Redacted(String);

impl Redacted {
    /// The cap, in **characters**, on redacted text placed in an error.
    ///
    /// The same number and the same character-counted rule as
    /// `error.rs`'s `status_detail`, deliberately: two caps on the same class of
    /// disclosure that disagreed would be two rules, and a byte-counted slice
    /// panics the moment index `CAP` lands inside a multi-byte sequence — which
    /// a runner's non-ASCII path in a `fatal:` line reaches on an ordinary day.
    pub const CAP: usize = 300;

    /// The masked text, for a classifier matching phrases against it.
    ///
    /// No `dead_code` expectation despite having no caller until the stderr
    /// classifier lands: the type is re-exported from `forge`, so this is
    /// reachable from outside the crate and the lint never fires on it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The same text, shortened to [`Self::CAP`] characters.
    ///
    /// **Applied to the payload of an error, never to the classifier's input.**
    /// The stderr classifier matches phrases the server writes behind its own
    /// `remote:` banner, i.e. at the *tail*; a cap applied before classification
    /// silently destroys the phrase table's inputs and turns every recognised
    /// refusal into an unclassified exit 1.
    ///
    /// Capping cannot be applied before redaction: it takes a [`Redacted`], and
    /// `redact` is that type's only constructor, so the wrong order is `E0308`
    /// rather than a leak. That is the ordering C-019 needs — a cap applied to
    /// raw text truncates the tail of a secret straddling the boundary and
    /// leaves its head in the surviving prefix — and it is held by the type
    /// rather than by a test, because a mutation that will not compile is not a
    /// red.
    #[must_use]
    pub fn capped(self) -> Self {
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
/// Takes a **slice** rather than one value because the credential exists in
/// more than one live form at once — the API credential, the push secret, and
/// the base64 `user:secret` blob it becomes on the wire — and masking only the
/// form the caller happened to think of leaves the others in the output. It is
/// fed from the same values the injector used, so the two cannot disagree about
/// what the secret is.
///
/// The sole constructor of [`Redacted`]: no `From<String>`, no `new`, no public
/// field, so unmasked bytes cannot reach [`ForgeError::GitCommandFailed`] or
/// [`ForgeError::GitPushFailed`] at all.
///
/// Masks **whole text, uncapped**. Shortening is [`Redacted::capped`]'s, applied
/// at the error boundary only, so the classifier reads everything the server
/// said.
///
/// An empty secret is skipped rather than replaced. `str::replace` matches an
/// empty needle at every character boundary, so a blank push secret — the
/// ordinary state whenever git's own credential helpers are in charge — would
/// otherwise shred the message into markers instead of masking anything.
/// `error.rs`'s `status_detail` guards the same edge for the same reason.
///
/// Masking is plain substring replacement, not word-boundary matching: a secret
/// embedded in a longer token is still a disclosure of the secret. The one shape
/// no substring redactor can reach is a secret broken by a newline *inside
/// itself* in git's own output — an accepted residual, recorded rather than
/// claimed as covered.
#[must_use]
pub fn redact(text: &str, secrets: &[&str]) -> Redacted {
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
    /// [`crate::package::version::Version`].
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
        let env = crate::test::env::lock();
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
