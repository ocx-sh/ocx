// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The credentials a forge run carries, and the CI identity they were resolved
//! under.
//!
//! Two credentials, because a GitLab CI job token can push over HTTP but cannot
//! open a merge request. Resolved once at the CLI boundary; a forge never reads
//! the environment itself.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;

use super::{ForgeToken, WriteTransport, is_valid_path_segment};

/// Every environment variable the credential precedence ladder reads, by registry name.
///
/// Tests isolate against [`ALL`]: a key a test did not override falls through to the real
/// environment, so a partly isolated test passes or fails on the ambient environment.
#[cfg(test)]
mod var {
    use ocx_env::EnvVar;

    pub const OCX_ANNOUNCE_TOKEN: &EnvVar = ocx_env::OCX_ANNOUNCE_TOKEN.declaration();
    pub const OCX_ANNOUNCE_GIT_TOKEN: &EnvVar = ocx_env::OCX_ANNOUNCE_GIT_TOKEN.declaration();
    pub const OCX_ANNOUNCE_GIT_USERNAME: &EnvVar = &ocx_env::OCX_ANNOUNCE_GIT_USERNAME;
    pub const GITLAB_CI: &EnvVar = &ocx_env::GITLAB_CI;
    pub const CI_JOB_TOKEN: &EnvVar = ocx_env::CI_JOB_TOKEN.declaration();
    pub const CI_PROJECT_PATH: &EnvVar = &ocx_env::CI_PROJECT_PATH;

    /// Not read by the ladder and not declared; listed so a ladder test isolates the whole CI identity.
    pub const CI_PROJECT_ID: &str = "CI_PROJECT_ID";

    /// Every declared variable above; a rung whose variable is missing here escapes test isolation.
    pub const ALL: &[&EnvVar] = &[
        OCX_ANNOUNCE_TOKEN,
        OCX_ANNOUNCE_GIT_TOKEN,
        OCX_ANNOUNCE_GIT_USERNAME,
        GITLAB_CI,
        CI_JOB_TOKEN,
        CI_PROJECT_PATH,
    ];
}

/// The credential a `git push` carries.
///
/// Travels as a base64 `user:secret` `Authorization` header through git's
/// configuration environment, never in a URL and never in argv.
#[derive(Clone, Debug)]
pub struct GitPushCredential {
    /// The user half of the pair.
    pub username: String,
    /// The secret half, redacted in `Debug` by [`ForgeToken`].
    pub secret: ForgeToken,
}

impl GitPushCredential {
    /// The user GitLab expects for a job token; for an access token GitLab
    /// ignores which non-empty user carries it.
    pub const DEFAULT_USERNAME: &'static str = "gitlab-ci-token";

    /// The `Authorization` value: `Basic` plus base64 of `username:secret`.
    ///
    /// Base64 is what neutralises a `\r\n` in either half. A `:` in the username
    /// would re-partition the pair server-side, so the ladder refuses one first.
    #[must_use]
    pub fn basic_authorization(&self) -> String {
        let encoded = BASE64_STANDARD.encode(format!("{}:{}", self.username, self.secret.0));
        format!("Basic {encoded}")
    }
}

/// Everything a forge needs to authenticate, resolved once at the CLI boundary.
///
/// The flags and CI identity are private because they are derived: a settable
/// `api_is_job_token` would send a `JOB-TOKEN` header for a credential that is not one.
#[derive(Clone, Debug)]
pub struct ForgeCredentials {
    /// Empty means unauthenticated — the `--output` path.
    api: ForgeToken,
    /// `None` leaves git's own credential helpers in charge.
    push: Option<GitPushCredential>,
    api_is_job_token: bool,
    push_is_job_token: bool,
    /// Recorded by the ladder, never derived later: rungs 1 and 2 yield the same
    /// credential when one value is exported under both names.
    push_is_explicit: bool,
    /// Recorded here rather than re-read at the CLI, or a second `GITLAB_CI` reader
    /// puts the credential-exemption table in `subsystem-cli.md` out of step with the code.
    in_gitlab_ci: bool,
    /// Only when every segment is a legal forge path segment.
    publishing_project: Option<String>,
}

impl ForgeCredentials {
    /// Resolve the API half, with no push credential.
    #[must_use]
    pub fn new(api: ForgeToken) -> Self {
        Self {
            api_is_job_token: is_job_token(&api.0),
            api,
            push: None,
            push_is_job_token: false,
            push_is_explicit: false,
            in_gitlab_ci: in_gitlab_ci(),
            publishing_project: ocx_env::CI_PROJECT_PATH
                .get()
                .filter(|path| is_valid_project_path(path)),
        }
    }

    /// Resolve both credentials from the environment by the precedence ladders.
    ///
    /// API: `OCX_ANNOUNCE_TOKEN`, then `CI_JOB_TOKEN` when `transport` is
    /// [`WriteTransport::Git`] and `GITLAB_CI` is set, else an empty [`Self::api`]
    /// (not an error; the CLI owns the exit-80 refusal). Push: `OCX_ANNOUNCE_GIT_TOKEN`,
    /// then the resolved API credential, else `None`. Empty counts as unset throughout.
    #[must_use]
    pub fn resolve(transport: WriteTransport) -> Self {
        let job_token = (transport == WriteTransport::Git && in_gitlab_ci())
            .then(|| ocx_env::CI_JOB_TOKEN.get())
            .flatten();
        // `get` reads empty as unset: CI runners export empty variables, and an empty
        // `OCX_ANNOUNCE_TOKEN` would win rung 1 and leave a good `CI_JOB_TOKEN` unused.
        let api = ocx_env::OCX_ANNOUNCE_TOKEN
            .get()
            .or(job_token)
            .map(ocx_env::Sensitive::into_inner)
            .unwrap_or_default();

        // An empty API credential falls through to `None`, or the push injects a header that authenticates as nobody.
        let explicit_push = ocx_env::OCX_ANNOUNCE_GIT_TOKEN
            .get()
            .map(ocx_env::Sensitive::into_inner);
        let push_is_explicit = explicit_push.is_some();
        let push_secret = explicit_push.or_else(|| (!api.is_empty()).then(|| api.clone()));

        // Through `new`, so `api_is_job_token` and the `CI_PROJECT_PATH` filter cannot drift from it.
        let mut credentials = Self::new(ForgeToken::new(api));
        credentials.push = push_secret.map(|secret| GitPushCredential {
            username: push_username(),
            secret: ForgeToken::new(secret),
        });
        credentials.push_is_explicit = push_is_explicit;
        // From the resolved secret, never the rung: rungs 1 and 2 can each carry the job token without saying so.
        credentials.push_is_job_token = credentials
            .push
            .as_ref()
            .is_some_and(|push| is_job_token(&push.secret.0));
        credentials
    }

    /// The REST credential.
    #[must_use]
    pub fn api(&self) -> &ForgeToken {
        &self.api
    }

    /// The push credential, or `None` when git's own helpers are in charge.
    #[must_use]
    pub fn push(&self) -> Option<&GitPushCredential> {
        self.push.as_ref()
    }

    /// Whether any rung resolved an API credential.
    ///
    /// Ask this, never re-read `OCX_ANNOUNCE_TOKEN`, which misses the job-token rung.
    #[must_use]
    pub fn api_is_present(&self) -> bool {
        !self.api.0.is_empty()
    }

    /// Whether the REST credential is this environment's own CI job token, which
    /// decides the `JOB-TOKEN` header.
    #[must_use]
    pub fn api_is_job_token(&self) -> bool {
        self.api_is_job_token
    }

    /// Whether the **push** credential is this environment's own CI job token.
    ///
    /// Not [`Self::api_is_job_token`]: rung 1 can replace the push half, and a
    /// preflight keyed on the API flag would refuse an announce whose project has job-token push off.
    #[must_use]
    pub fn push_is_job_token(&self) -> bool {
        self.push_is_job_token
    }

    /// Whether the operator chose the push credential by exporting `OCX_ANNOUNCE_GIT_TOKEN`.
    ///
    /// False inside a GitLab job means the push authenticates as someone the
    /// operator never nominated for it.
    #[must_use]
    pub fn push_is_explicit(&self) -> bool {
        self.push_is_explicit
    }

    /// Whether this run is a GitLab CI job (`GITLAB_CI` non-empty).
    ///
    /// Ask this rather than re-read `GITLAB_CI`. Not interchangeable with
    /// `ocx_shell::ci`'s detection, which requires `GITLAB_CI == "true"`.
    #[must_use]
    pub fn in_gitlab_ci(&self) -> bool {
        self.in_gitlab_ci
    }

    /// The full path of the project this run publishes from, when it runs in CI.
    #[must_use]
    pub fn publishing_project(&self) -> Option<&str> {
        self.publishing_project.as_deref()
    }
}

/// Whether every `/`-separated segment of a CI-supplied project path is legal.
///
/// `CI_PROJECT_PATH` is untrusted and reaches request URLs raw (`acme?x=1/index`
/// would retarget one). Not `RepoCoordinate`'s parser, which reads
/// `acme.team/platform/index` as host-qualified. A failing value counts as
/// absent, not an error, so announces off the job-token path keep working.
fn is_valid_project_path(path: &str) -> bool {
    path.split('/').all(is_valid_path_segment)
}

/// The operator's push username, or [`GitPushCredential::DEFAULT_USERNAME`].
///
/// A `:` makes it count as absent: HTTP Basic splits on the first colon, so
/// `a:b` would re-partition the pair into a 401 that reads like a bad token.
fn push_username() -> String {
    ocx_env::OCX_ANNOUNCE_GIT_USERNAME
        .get()
        .filter(|username| !username.contains(':'))
        .unwrap_or_else(|| GitPushCredential::DEFAULT_USERNAME.to_string())
}

/// Whether `secret` is this environment's own `CI_JOB_TOKEN`.
///
/// Requires a non-empty `CI_JOB_TOKEN`, or `"" == ""` claims a job token outside CI.
fn is_job_token(secret: &str) -> bool {
    ocx_env::CI_JOB_TOKEN
        .get()
        .is_some_and(|token| token.expose() == secret)
}

/// Whether this process is a GitLab CI job; the one spelling the ladder and
/// [`ForgeCredentials::in_gitlab_ci`] share, so they cannot disagree.
fn in_gitlab_ci() -> bool {
    ocx_env::GITLAB_CI.get().is_some()
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;

    use super::*;

    /// The environment lock with **every** ladder input explicitly removed.
    ///
    /// Taking `ocx_env::overrides::lock()` alone isolates nothing:
    /// a key no test set falls through to the process environment (DX-22). A
    /// ladder test that overrides three of its seven inputs is therefore
    /// reading the ambient environment for the other four, and its green is that
    /// machine's green. Clearing [`var::ALL`] up front is what makes every case
    /// below a statement about the code.
    fn isolated() -> ocx_env::overrides::EnvLock {
        let env = ocx_env::overrides::lock();
        for var in var::ALL {
            env.remove(var);
        }
        env.remove_raw(var::CI_PROJECT_ID);
        env
    }

    /// The push half, as the ladder would have built it, for a test that cares
    /// about the secret rather than the plumbing.
    fn push_credential(username: &str, secret: &str) -> GitPushCredential {
        GitPushCredential {
            username: username.to_string(),
            secret: ForgeToken::new(secret.to_string()),
        }
    }

    /// Both credential-bearing structs derive `Debug`, and both hold a
    /// [`ForgeToken`]. `forge_token_debug_is_redacted` pins the leaf; this pins
    /// the composites, which is where the guarantee is actually consumed —
    /// nothing formats a bare `ForgeToken`, while a `ForgeCredentials` reaches
    /// a `{:?}` in any error chain or trace that carries one.
    ///
    /// Redaction here is correct purely by delegation to the leaf's hand-written
    /// impl, so a future field holding a bare `String` secret — the push half is
    /// about to grow one — would leak with nothing reding. That is the mutation
    /// this test exists for.
    ///
    /// Reds on: giving [`ForgeToken`] a derived `Debug` (proved).
    #[test]
    fn credentials_debug_redacts_both_halves() {
        let env = isolated();
        let _ = &env;
        let mut credentials = ForgeCredentials::new(ForgeToken::new("glpat-notarealapivalue".to_string()));
        // Set directly rather than through a constructor: the push half has no
        // setter yet, and this module is the struct's own, so the private field
        // is reachable exactly here.
        credentials.push = Some(push_credential("gitlab-ci-token", "glpat-notarealpushvalue"));

        let rendered = format!("{credentials:?}");
        assert!(
            !rendered.contains("notarealapivalue"),
            "the API credential survived: {rendered}"
        );
        assert!(
            !rendered.contains("notarealpushvalue"),
            "the push credential survived: {rendered}"
        );
        // The falsifying half: a `Debug` that rendered nothing at all would
        // satisfy both assertions above. Two markers means both tokens were
        // reached and both were masked.
        assert_eq!(
            rendered.matches("ForgeToken(***)").count(),
            2,
            "both tokens must be rendered, and rendered redacted: {rendered}"
        );
        assert!(
            rendered.contains("gitlab-ci-token"),
            "the non-secret half must survive, or the assertions above pass vacuously: {rendered}"
        );
    }

    /// `CI_PROJECT_PATH` is chosen by whoever wrote the `.gitlab-ci.yml`, and it
    /// reaches an allowlist comparison, a request URL and a report. A segment
    /// carrying `?`, `#` or a traversal is treated as absent rather than
    /// interpreted.
    ///
    /// Tests the predicate directly and exhaustively, rather than only through
    /// the constructor: enumerating every shape below via
    /// [`ForgeCredentials::new`] would mean serialising through `EnvLock` once
    /// per case for a property that belongs to [`is_valid_project_path`]
    /// alone. The constructor's own call to this filter is covered separately,
    /// by `a_publishing_project_path_reaching_the_constructor_is_refused_unless_every_segment_is_legal`
    /// below, which exercises `CI_PROJECT_PATH` through
    /// the `ocx_env` override seam.
    ///
    /// Reds on: dropping the `filter` in the constructor is caught by the
    /// sibling constructor-level test below; this one pins the predicate's own
    /// segment-legality rules.
    #[test]
    fn a_publishing_project_path_is_refused_unless_every_segment_is_legal() {
        for legal in ["acme/index", "acme/platform/tooling/index", "a.c-me_1/index"] {
            assert!(is_valid_project_path(legal), "{legal} is an ordinary GitLab path");
        }
        for illegal in [
            "acme?x=1/index",
            "acme/index#fragment",
            "acme/../index",
            "acme//index",
            "acme/in dex",
            "acme@evil.example/index",
            "/acme/index",
        ] {
            assert!(
                !is_valid_project_path(illegal),
                "{illegal} must be treated as absent, never interpolated"
            );
        }
    }

    /// The constructor-level sibling of the predicate test above: exercises
    /// [`ForgeCredentials::new`] itself against a live (overridden)
    /// `CI_PROJECT_PATH`, on the [`ocx_env::overrides::EnvLock`] seam.
    ///
    /// This is the check that catches a dropped `.filter(is_valid_project_path)`
    /// call in the constructor — the predicate test above cannot, by
    /// construction, since it never calls the constructor.
    #[test]
    fn a_publishing_project_path_reaching_the_constructor_is_refused_unless_every_segment_is_legal() {
        let env = isolated();

        env.set(var::CI_PROJECT_PATH, "acme?x=1/index");
        assert_eq!(
            ForgeCredentials::new(ForgeToken::new(String::new())).publishing_project(),
            None,
            "an illegal segment must not reach the constructor's output"
        );

        env.set(var::CI_PROJECT_PATH, "acme/index");
        assert_eq!(
            ForgeCredentials::new(ForgeToken::new(String::new())).publishing_project(),
            Some("acme/index"),
            "an ordinary GitLab path must survive the constructor unchanged"
        );
    }

    /// The same segment filter, asserted through [`ForgeCredentials::resolve`].
    ///
    /// Not redundant with the sibling above, and the reason is a dated one.
    /// [`ForgeCredentials::new`] is still the live constructor today — one
    /// production caller, in `package_announce.rs` — so the sibling's green does
    /// describe a path production takes. The moment WP-14 repoints that call
    /// site at the ladder, it stops: `new` becomes test-only, and a dropped
    /// `.filter(is_valid_project_path)` in the ladder would red nothing at all.
    /// The ladder is the entry point that survives the repoint, so the filter
    /// needs its assertion here too, or the check becomes one that cannot be
    /// told from never having run.
    ///
    /// Reds on: dropping `.filter(is_valid_project_path)` from the ladder;
    /// reading `CI_PROJECT_PATH` with `get_raw` instead of `get`
    /// (the illegal row is unaffected, but a future empty-path row would be).
    #[test]
    fn the_ladder_refuses_a_publishing_project_path_with_an_illegal_segment() {
        let env = isolated();

        env.set(var::CI_PROJECT_PATH, "acme?x=1/index");
        assert_eq!(
            ForgeCredentials::resolve(WriteTransport::Git).publishing_project(),
            None,
            "an illegal segment must not reach the ladder's output either"
        );

        env.set(var::CI_PROJECT_PATH, "acme/index");
        assert_eq!(
            ForgeCredentials::resolve(WriteTransport::Git).publishing_project(),
            Some("acme/index"),
            "an ordinary GitLab path must survive the ladder unchanged, or the row above passes vacuously"
        );
    }

    /// `api_is_job_token` is true only when the API credential equals a
    /// **non-empty** `CI_JOB_TOKEN`.
    ///
    /// Runs on [`isolated`], not on a bare `lock()`: the earlier version of this
    /// test overrode `CI_JOB_TOKEN` and nothing else, so `publishing_project`
    /// read whatever `CI_PROJECT_PATH` the running machine happened to export
    /// (DX-22). Harmless while nothing asserted on it, and exactly the shape
    /// that makes the ladder tests below unfalsifiable if repeated there.
    ///
    /// Reds on: comparing `Option<String>` equality directly instead of
    /// gating on `get`'s non-empty filter — the third case sets `CI_JOB_TOKEN` to
    /// the *present but empty* string (not absent), which only a working
    /// non-emptiness qualifier tells apart from a genuine match against an
    /// equally empty API token.
    #[test]
    fn api_is_job_token_only_when_it_equals_a_non_empty_ci_job_token() {
        let env = isolated();

        env.set(var::CI_JOB_TOKEN, "glcbt-64-notarealjobtoken");
        assert!(
            ForgeCredentials::new(ForgeToken::new("glcbt-64-notarealjobtoken".to_string())).api_is_job_token(),
            "the API credential equals a non-empty CI_JOB_TOKEN"
        );

        env.set(var::CI_JOB_TOKEN, "glcbt-64-notarealjobtoken");
        assert!(
            !ForgeCredentials::new(ForgeToken::new("a-different-token".to_string())).api_is_job_token(),
            "the API credential differs from CI_JOB_TOKEN"
        );

        env.set(var::CI_JOB_TOKEN, "");
        assert!(
            !ForgeCredentials::new(ForgeToken::new(String::new())).api_is_job_token(),
            "both are present but empty — the non-emptiness qualifier must refuse this, not just an absent CI_JOB_TOKEN"
        );
    }

    // ---- the API precedence ladder (C-063, C-015) ---------------------------

    /// Rung 1 beats rung 2, and an **empty** rung 1 does not.
    ///
    /// The second case is C-063's own named trap and the whole reason the
    /// non-emptiness qualifier exists: a wrapper that exports
    /// `OCX_ANNOUNCE_TOKEN=` inside a CI job would otherwise suppress a
    /// perfectly good `CI_JOB_TOKEN` and produce a silently unauthenticated run.
    ///
    /// Reds on: consulting the job-token rung first; reading rung 1 with
    /// `get_raw` instead of the non-empty `get`.
    #[test]
    fn the_api_ladder_prefers_a_non_empty_ocx_token_over_the_job_token() {
        let env = isolated();
        env.set(var::GITLAB_CI, "true");
        env.set(var::CI_JOB_TOKEN, "glcbt-64-notarealjobtoken");

        env.set(var::OCX_ANNOUNCE_TOKEN, "glpat-notarealapivalue");
        let credentials = ForgeCredentials::resolve(WriteTransport::Git);
        assert_eq!(
            credentials.api().0,
            "glpat-notarealapivalue",
            "the operator's own credential must win rung 1"
        );
        assert!(
            !credentials.api_is_job_token(),
            "a credential that is not the job token must not be announced as one"
        );

        env.set(var::OCX_ANNOUNCE_TOKEN, "");
        let credentials = ForgeCredentials::resolve(WriteTransport::Git);
        assert_eq!(
            credentials.api().0,
            "glcbt-64-notarealjobtoken",
            "an empty OCX_ANNOUNCE_TOKEN must not win rung 1"
        );
        assert!(
            credentials.api_is_job_token(),
            "the job token picked up by rung 2 must be recognised as one"
        );
    }

    /// The job-token rung fires only when **all three** conjuncts hold:
    /// `CI_JOB_TOKEN` non-empty, the transport is `git`, and `GITLAB_CI` is set.
    ///
    /// Stated in prose in the contract and easy to ship with one of the three
    /// dropped — the transport is a parameter while the other two are
    /// environment reads, so it is the one an implementer most naturally loses.
    /// Each row below falsifies exactly one conjunct, so each drop reds on its
    /// own.
    ///
    /// `GITLAB_CI=` (present but empty) is ruled to mean **unset**, for
    /// consistency with the non-emptiness rule its two sibling rungs carry
    /// rather than as a second rule a reader has to remember.
    ///
    /// Reds on: dropping any one conjunct; dropping the non-emptiness filter on
    /// `CI_JOB_TOKEN` (the empty-token row then yields an empty API credential
    /// that `api_is_job_token` also calls a job token — a `JOB-TOKEN` header for
    /// no token, the worse of the two failures).
    #[test]
    fn the_job_token_rung_needs_all_three_conjuncts() {
        for (job_token, gitlab_ci, transport, expected, why) in [
            (
                "glcbt-64-notarealjobtoken",
                Some("true"),
                WriteTransport::Git,
                "glcbt-64-notarealjobtoken",
                "all three conjuncts hold",
            ),
            (
                "",
                Some("true"),
                WriteTransport::Git,
                "",
                "a present-but-empty CI_JOB_TOKEN is not a credential",
            ),
            (
                "glcbt-64-notarealjobtoken",
                Some("true"),
                WriteTransport::Api,
                "",
                "the REST transport never authors a push, so it never picks up a job token",
            ),
            (
                "glcbt-64-notarealjobtoken",
                None,
                WriteTransport::Git,
                "",
                "a job token pasted into a developer's shell is not a CI job",
            ),
            (
                "glcbt-64-notarealjobtoken",
                Some(""),
                WriteTransport::Git,
                "",
                "GITLAB_CI= is read as unset, like its two sibling rungs",
            ),
        ] {
            let env = isolated();
            env.set(var::CI_JOB_TOKEN, job_token);
            if let Some(marker) = gitlab_ci {
                env.set(var::GITLAB_CI, marker);
            }

            let credentials = ForgeCredentials::resolve(transport);
            assert_eq!(credentials.api().0, expected, "{why}");
            assert_eq!(
                credentials.api_is_job_token(),
                !expected.is_empty(),
                "the job-token flag must follow the rung that fired: {why}"
            );
        }
    }

    /// `api_is_job_token` is derived from the **environment**, not from which
    /// rung fired.
    ///
    /// The discriminating case: an operator exports the job token under the ocx
    /// name, so rung 1 wins with a value that *is* the job token. A derivation
    /// written as `matches!(rung, Rung::JobToken)` compiles cleanly and passes
    /// every other test in this module, and answers `false` here — which sends
    /// GitLab a `PRIVATE-TOKEN` header for a job token and reads back as a
    /// permission problem.
    ///
    /// The flag is also unsettable by a caller by construction: the field is
    /// private and this module has no submodules, so a struct literal naming it
    /// from a sibling is `E0451`. That is held by the compiler and reviewed, not
    /// asserted — a test that tried would break the build rather than red.
    ///
    /// Reds on: deriving the flag from the rung that fired; deriving it from the
    /// *push* credential.
    #[test]
    fn credentials_derive_api_is_job_token_internally() {
        let env = isolated();
        env.set(var::GITLAB_CI, "true");
        env.set(var::CI_JOB_TOKEN, "glcbt-64-notarealjobtoken");
        env.set(var::CI_PROJECT_PATH, "acme/publisher");
        env.set_raw(var::CI_PROJECT_ID, "4711");

        // Rung 1 wins with the job token's own value.
        env.set(var::OCX_ANNOUNCE_TOKEN, "glcbt-64-notarealjobtoken");
        let credentials = ForgeCredentials::resolve(WriteTransport::Git);
        assert_eq!(credentials.api().0, "glcbt-64-notarealjobtoken");
        assert!(
            credentials.api_is_job_token(),
            "the API credential IS this environment's job token, whichever rung produced it"
        );
        assert_eq!(
            credentials.publishing_project(),
            Some("acme/publisher"),
            "the CI identity travels with the credentials it was resolved beside"
        );

        // The independence half: a separate push PAT must not change the answer.
        env.set(var::OCX_ANNOUNCE_GIT_TOKEN, "glpat-notarealpushvalue");
        let credentials = ForgeCredentials::resolve(WriteTransport::Git);
        assert!(
            credentials.api_is_job_token(),
            "the flag describes the API half; the push half is a different credential"
        );
        assert_eq!(
            credentials
                .push()
                .expect("a git token resolves a push credential")
                .secret
                .0,
            "glpat-notarealpushvalue",
            "the push half must carry the git token, not the API credential"
        );

        // And the negative pole, so the assertions above are not vacuous.
        env.remove(var::OCX_ANNOUNCE_TOKEN);
        env.set(var::OCX_ANNOUNCE_TOKEN, "glpat-notarealapivalue");
        assert!(
            !ForgeCredentials::resolve(WriteTransport::Git).api_is_job_token(),
            "a credential that differs from CI_JOB_TOKEN is not a job token"
        );
    }

    /// The terminal rung: no credential anywhere yields an **empty** API token
    /// and no push credential — not an error and not `None`.
    ///
    /// WP-14 raises the `AuthError` (80) this earns for a write, and exempts
    /// `--output`; both need a value to look at, and this is it.
    ///
    /// Reds on: returning `Option<ForgeToken>`/`None` from the ladder; raising
    /// an error at this rung.
    #[test]
    fn the_terminal_rung_yields_an_empty_api_credential() {
        let env = isolated();
        let _ = &env;

        for transport in [WriteTransport::Api, WriteTransport::Git] {
            let credentials = ForgeCredentials::resolve(transport);
            assert!(
                credentials.api().0.is_empty(),
                "the terminal rung is an empty credential, not a guess"
            );
            assert!(
                !credentials.api_is_job_token(),
                "an empty credential is not a job token"
            );
            assert!(
                credentials.push().is_none(),
                "nothing injected means git's own helpers stay in charge"
            );
        }
    }

    // ---- the push precedence ladder (C-063, C-015) --------------------------

    /// `OCX_ANNOUNCE_GIT_TOKEN` overrides the push half **only**: the API half
    /// keeps its own credential.
    ///
    /// The two are independent by construction, which is exactly why an
    /// implementer may fuse them — and a job carrying a job-token API credential
    /// plus a push PAT is the shape the whole two-credential split exists for.
    ///
    /// Reds on: having the push rung also assign `api`.
    #[test]
    fn the_git_token_overrides_the_push_half_only() {
        let env = isolated();
        env.set(var::OCX_ANNOUNCE_TOKEN, "glpat-notarealapivalue");
        env.set(var::OCX_ANNOUNCE_GIT_TOKEN, "glpat-notarealpushvalue");

        let credentials = ForgeCredentials::resolve(WriteTransport::Git);
        assert_eq!(
            credentials.api().0,
            "glpat-notarealapivalue",
            "the REST half must keep its own credential"
        );
        assert_eq!(
            credentials
                .push()
                .expect("a git token resolves a push credential")
                .secret
                .0,
            "glpat-notarealpushvalue",
            "the push half must carry the git token"
        );
    }

    /// An **empty** `OCX_ANNOUNCE_GIT_TOKEN` does not win its rung; the API
    /// credential does.
    ///
    /// Without the filter the push half becomes
    /// `Basic base64("gitlab-ci-token:")` — a header that authenticates as
    /// nobody, injected alongside a `-c credential.helper=` that suppresses the
    /// operator's own helpers, which would have worked.
    ///
    /// Reds on: dropping the non-emptiness filter from the push rung.
    #[test]
    fn an_empty_git_token_falls_through_to_the_api_credential() {
        let env = isolated();
        env.set(var::OCX_ANNOUNCE_TOKEN, "glpat-notarealapivalue");
        env.set(var::OCX_ANNOUNCE_GIT_TOKEN, "");

        let push = ForgeCredentials::resolve(WriteTransport::Git)
            .push()
            .expect("the API credential is the push ladder's second rung")
            .clone();
        assert_eq!(
            push.secret.0, "glpat-notarealapivalue",
            "an empty git token must not win rung 1"
        );
    }

    /// Rung 3 of the push ladder: an empty API credential and no git token means
    /// **nothing injected** — `None`, never `Some` with an empty secret.
    ///
    /// A `Some("")` here silently disables the operator's own credential helpers
    /// (C-034 carries `-c credential.helper=` on exactly the invocations that
    /// inject) and then authenticates as nobody. `None` is the state in which
    /// git's helpers are still in charge, and it is the state the "no
    /// `extraHeader` is configured" scenario asserts.
    ///
    /// Reds on: returning `Some(GitPushCredential { secret: empty, .. })`.
    #[test]
    fn nothing_is_injected_when_no_credential_resolved() {
        let env = isolated();
        let _ = &env;
        assert!(
            ForgeCredentials::resolve(WriteTransport::Git).push().is_none(),
            "an empty credential must not be injected as a header"
        );
    }

    /// `push_is_job_token` walks all three push rungs, in both the equal and the
    /// differing direction, plus the empty-value cell each rung's non-emptiness
    /// qualifier exists for.
    ///
    /// The flag has two consumers and neither can be served by
    /// `api_is_job_token`: C-029's readable-`false` refusal at exit 82 fires
    /// only when the **push** credential is a job token, and C-060's
    /// `push_credential_kind` names the push half. Each row asserts the resolved
    /// secret beside the flag, so a row cannot go green by resolving a different
    /// credential than the one it describes.
    ///
    /// Reds on: deriving the flag from the rung that fired rather than from the
    /// resolved secret (rows 1 and 4 disagree with row 5 under that rule);
    /// deriving it from the API half (row 2); letting an empty
    /// `OCX_ANNOUNCE_GIT_TOKEN` win rung 1 (row 3); injecting an empty
    /// credential at rung 3 (row 6); dropping the non-emptiness filter on
    /// `CI_JOB_TOKEN`.
    #[test]
    fn push_is_job_token_follows_the_credential_the_push_ladder_resolved() {
        const JOB_TOKEN: &str = "glcbt-64-notarealjobtoken";
        const API_PAT: &str = "glpat-notarealapivalue";
        const PUSH_PAT: &str = "glpat-notarealpushvalue";

        for (job_token, gitlab_ci, api_token, git_token, expected_secret, expected_flag, why) in [
            (
                JOB_TOKEN,
                Some("true"),
                None,
                Some(JOB_TOKEN),
                Some(JOB_TOKEN),
                true,
                "rung 1 carrying the job token itself is a job-token push",
            ),
            (
                JOB_TOKEN,
                Some("true"),
                None,
                Some(PUSH_PAT),
                Some(PUSH_PAT),
                false,
                "rung 1 replaces the push half with a credential that is not the job token",
            ),
            (
                JOB_TOKEN,
                Some("true"),
                None,
                Some(""),
                Some(JOB_TOKEN),
                true,
                "an empty rung 1 does not win, so the job-token API half is the push credential",
            ),
            (
                JOB_TOKEN,
                Some("true"),
                None,
                None,
                Some(JOB_TOKEN),
                true,
                "rung 2 is the API half, which the job-token rung filled here",
            ),
            (
                JOB_TOKEN,
                Some("true"),
                Some(API_PAT),
                None,
                Some(API_PAT),
                false,
                "rung 2 is the API half, which is the operator's own credential here",
            ),
            (
                JOB_TOKEN,
                None,
                Some(""),
                None,
                None,
                false,
                "an empty API half leaves rung 2 unfilled, and rung 3 injects nothing to be a job token",
            ),
            (
                "",
                Some("true"),
                None,
                Some(PUSH_PAT),
                Some(PUSH_PAT),
                false,
                "a present-but-empty CI_JOB_TOKEN names no token for a push credential to equal",
            ),
            (
                "",
                None,
                None,
                None,
                None,
                false,
                "nothing anywhere: rung 3 injects nothing",
            ),
        ] {
            let env = isolated();
            env.set(var::CI_JOB_TOKEN, job_token);
            if let Some(marker) = gitlab_ci {
                env.set(var::GITLAB_CI, marker);
            }
            if let Some(token) = api_token {
                env.set(var::OCX_ANNOUNCE_TOKEN, token);
            }
            if let Some(token) = git_token {
                env.set(var::OCX_ANNOUNCE_GIT_TOKEN, token);
            }

            let credentials = ForgeCredentials::resolve(WriteTransport::Git);
            assert_eq!(
                credentials.push().map(|push| push.secret.0.as_str()),
                expected_secret,
                "the push ladder resolved the wrong credential: {why}"
            );
            assert_eq!(
                credentials.push_is_job_token(),
                expected_flag,
                "the push job-token flag must describe that credential: {why}"
            );
        }
    }

    /// S-028: the two job-token flags **diverge**. Inside a GitLab job with no
    /// `OCX_ANNOUNCE_TOKEN`, the API half is the job token while
    /// `OCX_ANNOUNCE_GIT_TOKEN` replaces the push half with a PAT.
    ///
    /// This divergence is the whole reason the second flag exists, and it is
    /// the one case a test asserting only that the two agree would pass with
    /// either flag deleted. C-029's exit-82 refusal keys on the push flag:
    /// keyed on the API flag it would fire here, on a run whose push credential
    /// the project's job-token setting does not govern at all.
    ///
    /// Reds on: `push_is_job_token` returning `api_is_job_token` (proved);
    /// deriving the push flag from the API half.
    #[test]
    fn the_job_token_flags_diverge_when_a_git_token_replaces_the_push_half() {
        let env = isolated();
        env.set(var::GITLAB_CI, "true");
        env.set(var::CI_JOB_TOKEN, "glcbt-64-notarealjobtoken");
        env.set(var::OCX_ANNOUNCE_GIT_TOKEN, "glpat-notarealpushvalue");

        let credentials = ForgeCredentials::resolve(WriteTransport::Git);
        assert_eq!(
            credentials.api().0,
            "glcbt-64-notarealjobtoken",
            "rung 2 of the API ladder picks up the job token"
        );
        assert!(credentials.api_is_job_token(), "the API half IS this job's own token");
        assert_eq!(
            credentials
                .push()
                .expect("a git token resolves a push credential")
                .secret
                .0,
            "glpat-notarealpushvalue",
            "rung 1 of the push ladder replaces the push half"
        );
        assert!(
            !credentials.push_is_job_token(),
            "the push half is a PAT, not the job token — the two flags must disagree here"
        );
    }

    /// The push username: honoured when set, defaulted when not, and defaulted
    /// again when set to something HTTP Basic cannot carry.
    ///
    /// Both directions of the default are needed — hardcoding it passes the
    /// unset case, and deleting it passes the set case, so one alone leaves half
    /// the branch unproved.
    ///
    /// The colon row is a correctness hole, not cosmetics: HTTP Basic has no
    /// escaping and a server splits the decoded pair on the **first** colon, so
    /// `OCX_ANNOUNCE_GIT_USERNAME=a:b` silently re-partitions the pair into user
    /// `a` and secret `b:<secret>` — a 401 that reads like a bad token. An
    /// **empty** username produces `base64(":secret")`, which GitLab reads as an
    /// empty user, for the same symptom. Both are treated as absent, the way
    /// `CI_PROJECT_PATH` already is in this module, rather than minting an error
    /// for a variable ocx did not ask for.
    ///
    /// Reds on: hardcoding the default; deleting the default; reading the
    /// username with `get_raw` instead of `get`; accepting a username
    /// containing `:`.
    #[test]
    fn the_push_username_is_honoured_defaulted_and_sanitised() {
        for (configured, expected, why) in [
            (
                Some("deploy-token-42"),
                "deploy-token-42",
                "a configured username is honoured",
            ),
            (None, GitPushCredential::DEFAULT_USERNAME, "an unset username defaults"),
            (
                Some(""),
                GitPushCredential::DEFAULT_USERNAME,
                "an empty username is not a user",
            ),
            (
                Some("a:b"),
                GitPushCredential::DEFAULT_USERNAME,
                "a colon re-partitions the HTTP Basic pair, so it is treated as absent",
            ),
        ] {
            let env = isolated();
            env.set(var::OCX_ANNOUNCE_GIT_TOKEN, "glpat-notarealpushvalue");
            if let Some(username) = configured {
                env.set(var::OCX_ANNOUNCE_GIT_USERNAME, username);
            }

            let credentials = ForgeCredentials::resolve(WriteTransport::Git);
            assert_eq!(
                credentials
                    .push()
                    .expect("a git token resolves a push credential")
                    .username,
                expected,
                "{why}"
            );
        }
    }

    // ---- the wire form of the push credential (C-022, C-034) ----------------

    /// The header encodes `username:secret`, in that order, and decodes back to
    /// exactly the pair it was built from.
    ///
    /// The pure half of C-022's agreement check: the workspace injects this
    /// value and the redactor masks the same base64 blob, so both are fed from
    /// one function and a wrong-value agreement between them cannot arise. The
    /// secret below carries a colon and a CRLF on purpose — HTTP Basic has no
    /// escaping, so only "split on the **first** colon" recovers it, and only
    /// base64 (whose alphabet cannot spell a header separator) stops the CRLF
    /// from ending the header early.
    ///
    /// Reds on: encoding `secret:username`; emitting `Basic user:secret`
    /// unencoded; percent-encoding or otherwise mangling the pair.
    #[test]
    fn basic_authorization_encodes_the_username_then_the_secret() {
        let credential = push_credential("gitlab-ci-token", "glpat-not:a:real\r\nvalue");
        let header = credential.basic_authorization();

        let encoded = header
            .strip_prefix("Basic ")
            .unwrap_or_else(|| panic!("the credential must travel as HTTP Basic: {header}"));
        assert!(
            encoded
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=')),
            "the pair reached the header unencoded: {header}"
        );

        let decoded = BASE64_STANDARD.decode(encoded).expect("the header must be base64");
        let pair = String::from_utf8(decoded).expect("the pair must be UTF-8");
        let (username, secret) = pair
            .split_once(':')
            .unwrap_or_else(|| panic!("the pair must be `user:secret`: {pair:?}"));
        assert_eq!(username, credential.username, "the user half comes first");
        assert_eq!(secret, credential.secret.0, "the secret half survives its own colons");
    }
}
