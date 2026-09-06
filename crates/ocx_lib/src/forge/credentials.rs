// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The credentials a forge run carries, and the CI identity they were resolved
//! under.
//!
//! Two credentials, not one, because the two halves of a GitLab write have
//! different requirements: a CI job token can push over HTTP but cannot open a
//! merge request, while a project or personal access token can call the API but
//! is not the pipeline's own identity. Resolution happens once, at the CLI
//! boundary, and the resolved pair travels down as one value — a forge never
//! reads the environment for itself.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;

use super::{ForgeToken, WriteTransport, is_valid_path_segment};

/// Every environment variable the credential precedence ladder reads, in one
/// place.
///
/// Grouped so the ladder's whole input surface is a list a reviewer can hold in
/// their head — and so a test cannot isolate half of it. `crate::env::var`'s
/// test seam falls through to the real process environment for any key a test
/// did not override (`get_override` returns `None`, and `var` then calls
/// `std::env::var`), so a partially-overridden ladder test's green is the
/// **ambient environment's** green, on whatever machine happened to run it.
/// [`ALL`] is what the tests isolate against.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "read by the precedence ladder, which lands with its body; delete this attribute in the same change (DX-98)"
    )
)]
mod var {
    /// The two forge-credential names, re-exported from [`crate::env::keys`]
    /// rather than re-spelled here: `OCX_ANNOUNCE_TOKEN` (the operator's own
    /// announce credential, rung 1 of the API ladder) and
    /// `OCX_ANNOUNCE_GIT_TOKEN` (a push-only credential overriding the API
    /// credential for the git half alone, rung 1 of the push ladder).
    ///
    /// One literal per variable, workspace-wide, and the split it prevents is a
    /// real one: `OCX_ANNOUNCE_GIT_TOKEN` is read *here* and scrubbed from
    /// plugin child environments *there*
    /// ([`CREDENTIAL_KEYS`](crate::env::keys::CREDENTIAL_KEYS)), so two copies
    /// mean a rename that keeps working while the push credential silently
    /// stops being stripped. `OCX_ANNOUNCE_TOKEN` is a documented non-member of
    /// that list and is spelled once for the same reason — the CLI's exit-80
    /// refusal names it, and a refusal naming a variable the ladder no longer
    /// reads is worse than no message.
    pub use crate::env::keys::{OCX_ANNOUNCE_GIT_TOKEN, OCX_ANNOUNCE_TOKEN};

    /// The user half of the push pair, defaulting to
    /// [`super::GitPushCredential::DEFAULT_USERNAME`].
    ///
    /// Stays local: it is not a credential, so it has no `CREDENTIAL_KEYS` row
    /// to drift from, and this is its only spelling in the workspace.
    pub const OCX_ANNOUNCE_GIT_USERNAME: &str = "OCX_ANNOUNCE_GIT_USERNAME";

    /// GitLab's own marker that this process is a CI job. One of the three
    /// conjuncts guarding the job-token rung.
    pub const GITLAB_CI: &str = "GITLAB_CI";

    /// GitLab's own name for the token a job runs under. Read here so the API
    /// credential can be recognised *as* it without any caller saying so.
    pub const CI_JOB_TOKEN: &str = "CI_JOB_TOKEN";

    /// GitLab's own name for the full path of the project a job runs in — the
    /// **publishing** project, which the index project's job-token allowlist is
    /// checked against.
    pub const CI_PROJECT_PATH: &str = "CI_PROJECT_PATH";

    /// GitLab's numeric id for the same project. Not read by the ladder itself,
    /// and listed so a test isolating the ladder isolates the whole CI identity
    /// rather than the half that happens to be read today.
    pub const CI_PROJECT_ID: &str = "CI_PROJECT_ID";

    /// Every name above.
    ///
    /// Never read in production — its consumer is the test that isolates the
    /// ladder — and that is why it is here rather than inlined into the test
    /// module: a rung added without a row here is a rung whose variable the
    /// isolation misses.
    pub const ALL: &[&str] = &[
        OCX_ANNOUNCE_TOKEN,
        OCX_ANNOUNCE_GIT_TOKEN,
        OCX_ANNOUNCE_GIT_USERNAME,
        GITLAB_CI,
        CI_JOB_TOKEN,
        CI_PROJECT_PATH,
        CI_PROJECT_ID,
    ];
}

/// The credential a `git push` carries.
///
/// The username is not cosmetic: GitLab's HTTP endpoint authenticates the pair,
/// and a job token is presented under the conventional `gitlab-ci-token` user,
/// which is also the default when the operator sets no
/// `OCX_ANNOUNCE_GIT_USERNAME`. The pair travels as a base64 `user:secret`
/// `Authorization` header injected through git's own configuration environment,
/// never in a URL and never in argv.
#[derive(Clone, Debug)]
pub struct GitPushCredential {
    /// The user half of the pair.
    pub username: String,
    /// The secret half. Redacted in `Debug` by [`ForgeToken`]'s own impl.
    pub secret: ForgeToken,
}

impl GitPushCredential {
    /// The user half GitLab expects when no operator names one.
    ///
    /// GitLab authenticates a job token under this conventional user, and the
    /// same default applies to a project or personal access token presented over
    /// HTTP — GitLab reads the secret and ignores which non-empty user carries
    /// it.
    pub const DEFAULT_USERNAME: &'static str = "gitlab-ci-token";

    /// The `Authorization` header value this pair travels as: `Basic` plus the
    /// base64 of `username:secret`.
    ///
    /// Lives here, on the credential, rather than in the workspace that injects
    /// it, for two reasons. It is the **pure half** of C-022's agreement check —
    /// the injected header and the redactor's secret list must be fed from one
    /// value, and one function both can be tested against is what makes "they
    /// agree" a statement rather than a hope. And the encoding is what
    /// neutralises a username or secret carrying `\r\n`: base64's alphabet
    /// cannot spell a header separator, so no escaping rule of ocx's own is
    /// needed or written.
    ///
    /// HTTP Basic has no escaping on the *pair*: a server splits the decoded
    /// text on the **first** colon. A username carrying one would therefore
    /// re-partition the pair silently, which is why the ladder refuses one
    /// before a credential ever reaches here.
    #[must_use]
    pub fn basic_authorization(&self) -> String {
        let encoded = BASE64_STANDARD.encode(format!("{}:{}", self.username, self.secret.0));
        format!("Basic {encoded}")
    }
}

/// Everything a forge needs to authenticate, resolved once at the CLI boundary.
///
/// Every field is private, and the four `bool`/CI-identity ones are private for
/// a reason a reader should not have to infer: they are **derived**, never
/// supplied. `api_is_job_token` and `push_is_job_token` each answer "is this
/// half of the pair this environment's own job token", and a caller that could
/// set either could make ocx send a `JOB-TOKEN` header for a credential that is
/// not one, or refuse a push at exit 86 over a job-token setting that governs no
/// credential the run holds — both failing in a way that reads like a permission
/// problem. `push_is_explicit` and `in_gitlab_ci` are the same class: they say
/// which rung answered and where the process is running, and C-064's notice
/// fires off them. The derivation is the constructor's, so there is exactly one
/// place each question is answered.
#[derive(Clone, Debug)]
pub struct ForgeCredentials {
    /// The REST credential. Empty means unauthenticated — the `--out` path.
    api: ForgeToken,
    /// The push credential, when the git transport is selected and one was
    /// resolved. `None` leaves git's own credential helpers in charge.
    push: Option<GitPushCredential>,
    /// Whether [`Self::api`] is this environment's own `CI_JOB_TOKEN`.
    api_is_job_token: bool,
    /// Whether [`Self::push`]'s secret is this environment's own
    /// `CI_JOB_TOKEN` — a different question from [`Self::api_is_job_token`],
    /// which the push ladder's rung 1 lets it diverge from.
    push_is_job_token: bool,
    /// Whether [`Self::push`]'s secret came from `OCX_ANNOUNCE_GIT_TOKEN` —
    /// rung 1 of the push ladder — rather than from rung 2's copy of the API
    /// half.
    ///
    /// Not derivable from the two credentials: rung 1 and rung 2 produce the
    /// same [`GitPushCredential`] whenever the operator exported one value under
    /// both names, so "did the operator choose this push credential" has no
    /// answer once the ladder has run unless the ladder records it.
    push_is_explicit: bool,
    /// Whether this process is a GitLab CI job — `GITLAB_CI` set and non-empty,
    /// the same read the API ladder's second rung is guarded by.
    ///
    /// Recorded rather than re-read at the CLI: C-063 pins
    /// `OCX_ANNOUNCE_GIT_TOKEN`'s and the CI family's read site to this module,
    /// and a second reader elsewhere would put the credential-exemption table in
    /// `subsystem-cli.md` in disagreement with the code. Distinct from
    /// [`Self::publishing_project`], which is `CI_PROJECT_PATH` and can be
    /// absent or malformed inside a perfectly ordinary job.
    in_gitlab_ci: bool,
    /// The full path of the project this run publishes from, when it runs in
    /// CI, and only when every segment of it is a legal forge path segment.
    ///
    /// Carried here rather than passed as a second argument to the preflight:
    /// it is the same class of value as `api_is_job_token` — CI-environment
    /// identity, resolved once at the boundary, unsettable by a caller — and it
    /// is meaningless on a forge that has no job tokens, so widening a trait
    /// signature every implementor pays for would be the wrong home for it.
    publishing_project: Option<String>,
}

impl ForgeCredentials {
    /// Resolve the API half, with no push credential.
    ///
    /// `api_is_job_token` is true only when `api` equals a **non-empty**
    /// `CI_JOB_TOKEN` — see [`is_job_token`] for why that qualifier carries the
    /// whole check. `push_is_job_token` and `push_is_explicit` are false here by
    /// construction: there is no push credential for either to describe.
    #[must_use]
    pub fn new(api: ForgeToken) -> Self {
        Self {
            api_is_job_token: is_job_token(&api.0),
            api,
            push: None,
            push_is_job_token: false,
            push_is_explicit: false,
            in_gitlab_ci: in_gitlab_ci(),
            publishing_project: non_empty(var::CI_PROJECT_PATH).filter(|path| is_valid_project_path(path)),
        }
    }

    /// Resolve **both** credentials from the environment, by C-063's precedence
    /// ladder.
    ///
    /// The API ladder: `OCX_ANNOUNCE_TOKEN` when set and non-empty, then
    /// `CI_JOB_TOKEN` when non-empty **and** `transport` is
    /// [`WriteTransport::Git`] **and** `GITLAB_CI` is set, then nothing. All
    /// three conjuncts on the second rung are load-bearing and each is dropped
    /// independently by a plausible implementation, so each is asserted alone.
    ///
    /// The push ladder: `OCX_ANNOUNCE_GIT_TOKEN` when set and non-empty, then
    /// the resolved API credential, then **nothing injected** — `None`, never
    /// `Some` with an empty secret. The two halves are independent by
    /// construction: a job carrying a job-token API credential and a separate
    /// push PAT sends `JOB-TOKEN` on its reads and the PAT on its push.
    ///
    /// The username for both push rungs is `OCX_ANNOUNCE_GIT_USERNAME` when set,
    /// non-empty and free of `:`, else [`GitPushCredential::DEFAULT_USERNAME`].
    ///
    /// **The non-emptiness qualifiers carry the whole ladder.** A wrapper
    /// exporting `OCX_ANNOUNCE_TOKEN=` would otherwise win rung 1, suppress the
    /// job-token pickup, and produce a silently unauthenticated run inside a CI
    /// job that had a perfectly good `CI_JOB_TOKEN`; an empty
    /// `OCX_ANNOUNCE_GIT_TOKEN` winning its own rung 1 would inject
    /// `Basic base64("gitlab-ci-token:")` — a header that authenticates as
    /// nobody, while `-c credential.helper=` suppresses the operator's helpers
    /// that would have worked. `GITLAB_CI=` is read the same way, for
    /// consistency with its two sibling rungs rather than as a second rule.
    ///
    /// The terminal rung yields an **empty** [`Self::api`], not an error: the
    /// `AuthError` (80) that an unauthenticated write earns, and the `--out`
    /// carve-out that exempts it, are raised at the CLI boundary where the write
    /// mode is known.
    #[must_use]
    pub fn resolve(transport: WriteTransport) -> Self {
        let job_token = (transport == WriteTransport::Git && in_gitlab_ci())
            .then(|| non_empty(var::CI_JOB_TOKEN))
            .flatten();
        let api = non_empty(var::OCX_ANNOUNCE_TOKEN).or(job_token).unwrap_or_default();

        // Rung 2 of the push ladder is the *resolved* API credential, so an
        // empty one falls through to rung 3 rather than injecting a header that
        // authenticates as nobody. Which rung answered is recorded here and
        // nowhere else: the two rungs yield an identical credential whenever one
        // value was exported under both names, so nothing downstream can
        // reconstruct it.
        let explicit_push = non_empty(var::OCX_ANNOUNCE_GIT_TOKEN);
        let push_is_explicit = explicit_push.is_some();
        let push_secret = explicit_push.or_else(|| (!api.is_empty()).then(|| api.clone()));

        // Through `new` rather than a second struct literal: `api_is_job_token`
        // and the `CI_PROJECT_PATH` filter are answered in exactly one place,
        // so the ladder cannot drift from the constructor it replaces.
        let mut credentials = Self::new(ForgeToken::new(api));
        credentials.push = push_secret.map(|secret| GitPushCredential {
            username: push_username(),
            secret: ForgeToken::new(secret),
        });
        credentials.push_is_explicit = push_is_explicit;
        // Derived from the credential the ladder actually resolved, never from
        // the rung that produced it: an operator may export the job token under
        // the ocx name, and rung 2 may hand the push half a credential that is
        // one without any rung having said so.
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

    /// Whether an API credential was resolved at all.
    ///
    /// The terminal rung of [`Self::resolve`] yields an **empty** [`ForgeToken`]
    /// rather than an error, and every field here is private, so this is the
    /// only way to observe that outcome. Two consumers ask it, and both must ask
    /// the *ladder* rather than re-reading `OCX_ANNOUNCE_TOKEN` themselves —
    /// which would miss the job-token rung entirely: the claim and announce
    /// reports, whose `credential_kind` is `"none"` exactly when this is false
    /// (C-060), and C-063's exit-80 refusal of an unauthenticated write.
    #[must_use]
    pub fn api_is_present(&self) -> bool {
        !self.api.0.is_empty()
    }

    /// Whether the REST credential is this environment's own CI job token — the
    /// single input to the `JOB-TOKEN` header decision.
    #[must_use]
    pub fn api_is_job_token(&self) -> bool {
        self.api_is_job_token
    }

    /// Whether the **push** credential is this environment's own CI job token.
    ///
    /// True exactly when a push credential was resolved **and** its secret
    /// equals a non-empty `CI_JOB_TOKEN`. Read off C-063's push ladder that is:
    /// at rung 1, true only when the operator exported the job token itself
    /// under `OCX_ANNOUNCE_GIT_TOKEN` and false for any other value; at rung 2,
    /// exactly whether the resolved API half is the job token; and at rung 3
    /// always false, because nothing is injected there.
    ///
    /// Not a synonym for [`Self::api_is_job_token`], and that is why it exists:
    /// rung 1 replaces the push half while the API half remains the job token,
    /// so the two genuinely differ. GitLab's job-token push setting and the
    /// allowlist behind it govern the **push** credential, so a preflight keyed
    /// on the API flag would refuse an ordinary announce whose project simply
    /// has that setting off.
    #[must_use]
    pub fn push_is_job_token(&self) -> bool {
        self.push_is_job_token
    }

    /// Whether the operator *chose* the push credential, by exporting
    /// `OCX_ANNOUNCE_GIT_TOKEN` — rung 1 of the push ladder.
    ///
    /// False at rung 2, where the push half is a copy of the API credential the
    /// operator set for the REST calls, and false at rung 3, where nothing is
    /// injected. C-064's notice is the consumer: a false here inside a GitLab
    /// job is the state where the push authenticates as somebody the operator
    /// never nominated for it.
    #[must_use]
    pub fn push_is_explicit(&self) -> bool {
        self.push_is_explicit
    }

    /// Whether this run is a GitLab CI job.
    ///
    /// The only way to observe `GITLAB_CI` **for the credential ladder** — C-063
    /// pins that read surface here, so a caller that needs the fact asks the
    /// resolved credentials for it rather than reading the variable again.
    /// [`crate::ci`]'s `gitlab_flavor::detect` reads the same variable for a
    /// different question — which `--ci` export flavor to write — and answers it
    /// more strictly (`== "true"`, not merely non-empty), so the two are not
    /// interchangeable and neither is a second reader of the other's rule.
    #[must_use]
    pub fn in_gitlab_ci(&self) -> bool {
        self.in_gitlab_ci
    }

    /// The full path of the project this run publishes from, when it runs in CI.
    ///
    /// The index project's job-token allowlist is read only when this differs
    /// from the index project, and the refusal message names both paths.
    #[must_use]
    pub fn publishing_project(&self) -> Option<&str> {
        self.publishing_project.as_deref()
    }
}

/// Whether every `/`-separated segment of a CI-supplied project path is a legal
/// forge path segment.
///
/// The same check [`super::RepoCoordinate`]'s parser applies to a coordinate,
/// for the same reason and against the same function: this value reaches a
/// `pub` accessor, an allowlist comparison, a request URL and a report's
/// `detail`, and the GitHub client interpolates a path into a URL raw — so
/// `acme?x=1/index` would silently retarget a request. In the fork-merge-request
/// threat model this feature exists for, the author of a `.gitlab-ci.yml` chooses
/// what `CI_PROJECT_PATH` says, which makes it untrusted input at a boundary
/// rather than a value the runner vouches for.
///
/// Deliberately **not** [`super::RepoCoordinate`]'s `FromStr`: that parser reads
/// a leading host-shaped segment as a host, so a real nested GitLab group path
/// like `acme.team/platform/index` would be mis-parsed into host `acme.team`.
/// Segment legality is the whole of what is needed here.
///
/// A failing value is treated as **absent**, not as an error: the allowlist
/// comparison it feeds already has a "not in CI" path, and refusing the whole
/// run over a variable ocx did not ask for would break announces that never
/// touch the job-token path.
fn is_valid_project_path(path: &str) -> bool {
    path.split('/').all(is_valid_path_segment)
}

/// The user half of the push pair: the operator's own, or
/// [`GitPushCredential::DEFAULT_USERNAME`].
///
/// A username carrying `:` is treated as **absent**, not as an error. HTTP
/// Basic has no escaping on the pair — a server splits the decoded text on the
/// first colon — so `a:b` would silently re-partition it into user `a` and
/// secret `b:<secret>`, a 401 that reads like a bad token. An empty one has the
/// same symptom via `base64(":secret")`. Both are refused the way
/// `CI_PROJECT_PATH` already is here, rather than by minting an error for a
/// variable ocx did not ask for.
fn push_username() -> String {
    non_empty(var::OCX_ANNOUNCE_GIT_USERNAME)
        .filter(|username| !username.contains(':'))
        .unwrap_or_else(|| GitPushCredential::DEFAULT_USERNAME.to_string())
}

/// Whether `secret` is this environment's own `CI_JOB_TOKEN`.
///
/// One function for both halves of the pair, so "is this a job token" is
/// answered in exactly one place. The **non-emptiness** qualifier carries the
/// whole check: outside a CI job both sides are ordinarily the empty string,
/// and `"" == ""` would otherwise claim a job token in an environment that has
/// none.
fn is_job_token(secret: &str) -> bool {
    non_empty(var::CI_JOB_TOKEN).is_some_and(|token| token == secret)
}

/// Whether this process is a GitLab CI job.
///
/// One spelling for the two readers — the API ladder's second rung and
/// [`ForgeCredentials::in_gitlab_ci`] — so the flag a caller branches on and the
/// flag that opened the job-token rung can never disagree about what "in CI"
/// means. Empty counts as unset for the reason [`non_empty`] gives.
fn in_gitlab_ci() -> bool {
    non_empty(var::GITLAB_CI).is_some()
}

/// An environment variable's value, or `None` when it is unset **or empty**.
///
/// Treating empty as unset is the point: CI runners routinely export a variable
/// with no value, and every reader here would otherwise have to remember to
/// filter it.
fn non_empty(key: &str) -> Option<String> {
    crate::env::var(key).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;

    use super::*;

    /// The environment lock with **every** ladder input explicitly removed.
    ///
    /// Taking `crate::test::env::lock()` alone isolates nothing:
    /// `get_override` returns `None` for a key no test set, and
    /// `crate::env::var` then falls through to `std::env::var` (DX-22). A
    /// ladder test that overrides three of its seven inputs is therefore
    /// reading the ambient environment for the other four, and its green is that
    /// machine's green. Clearing [`var::ALL`] up front is what makes every case
    /// below a statement about the code.
    fn isolated() -> crate::test::env::EnvLock {
        let env = crate::test::env::lock();
        for name in var::ALL {
            env.remove(*name);
        }
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
    /// [`crate::env::var`]'s test seam.
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
    /// `CI_PROJECT_PATH`, on the [`crate::test::env::EnvLock`] seam.
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
    /// reading `CI_PROJECT_PATH` with `crate::env::var` instead of `non_empty`
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
    /// gating on `non_empty`'s filter — the third case sets `CI_JOB_TOKEN` to
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
    /// `crate::env::var` instead of the non-empty filter.
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
        env.set(var::CI_PROJECT_ID, "4711");

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
    /// `--out`; both need a value to look at, and this is it.
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
    /// `api_is_job_token`: C-029's readable-`false` refusal at exit 86 fires
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
    /// either flag deleted. C-029's exit-86 refusal keys on the push flag:
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
    /// username with `var` instead of `non_empty`; accepting a username
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
