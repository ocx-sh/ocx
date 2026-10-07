// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The owner ladder.
//!
//! Which logins: `--owner` if given at all, else the CI environment, else the token identity, else
//! [`ClaimError::NoActingIdentity`](super::ClaimError::NoActingIdentity). Who confirms: the users API
//! when reachable, else the operator's `LOGIN:ID`, or the CI environment when the list came from there.

use super::error::ClaimError;
use super::request::{OwnerIdentitySource, OwnerSpec};
use crate::forge::{Forge, ForgeError};

/// Every CI variable the ladder reads; a ladder test must override all four, or on a GitHub runner an
/// unset override falls through to the process environment and the test measures it, not the code.
pub const CI_IDENTITY_VARS: [&ocx_env::EnvVar; 4] = [
    &ocx_env::GITLAB_USER_LOGIN,
    &ocx_env::GITLAB_USER_ID,
    &ocx_env::GITHUB_ACTOR,
    &ocx_env::GITHUB_ACTOR_ID,
];

/// One confirmed owner, as it is written into the root and the report.
///
/// `login`/`id` only; the four-key form in the vendored golden roots is indexbot's output, not ocx's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedOwner {
    /// The forge's canonical login spelling.
    pub login: String,
    /// The forge's numeric account id.
    pub id: u64,
}

/// The owner list, the provenance label that produced it, and who authored the run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnerResolution {
    /// The resolved owners, in first-occurrence order.
    pub owners: Vec<ResolvedOwner>,
    /// Which rung answered, rendered once per run.
    pub source: OwnerIdentitySource,
    /// The authoring identity: the token identity, else the CI-environment identity, else `None`.
    pub author: Option<ResolvedOwner>,
    /// Which rung produced [`Self::author`]: [`OwnerIdentitySource::Resolved`] for the credential's own
    /// account, [`OwnerIdentitySource::CiEnvironment`] for the CI pair, which any earlier pipeline step
    /// can set; never `Asserted`. `None` exactly when [`Self::author`] is.
    pub author_identity_source: Option<OwnerIdentitySource>,
}

/// Run the ladder; an empty `explicit` (the `--owner` list) means "not given".
///
/// # Errors
///
/// [`ClaimError::NoActingIdentity`] at the terminal rung; [`ClaimError::OwnerUnknown`],
/// [`ClaimError::OwnerIdMismatch`], [`ClaimError::BotIdentity`] or [`ClaimError::DuplicateOwner`] for a
/// refused owner; [`ClaimError::InvalidOwnerLogin`] for a supplied or forge-answered login outside
/// `[A-Za-z0-9._-]`; [`ClaimError::Forge`] when the users API is unreachable and a bare `LOGIN` has no id.
pub async fn resolve_owners(forge: &dyn Forge, explicit: &[OwnerSpec]) -> Result<OwnerResolution, ClaimError> {
    let (seeds, rung) = seed_logins(forge, explicit).await?;

    // Bot shape before charset: `dependabot[bot]` fails both, and "bot account" is the actionable diagnosis.
    for (position, spec) in seeds.iter().enumerate() {
        let login = spec_login(spec);
        if login_has_bot_shape(login) {
            return Err(ClaimError::BotIdentity {
                login: login.to_string(),
            });
        }
        if !login_charset_is_valid(login) {
            return Err(ClaimError::InvalidOwnerLogin {
                login: login.to_string(),
            });
        }
        // Refused here, not only unconfirmed, or `--owner alice --owner alice` exits 0 or 64 by
        // users-API reachability.
        // Never fold case: the server decides which spellings are one account, and folding refuses
        // `--owner AliCe --owner alice:7`.
        if seeds[..position].iter().any(|earlier| spec_login(earlier) == login) {
            return Err(ClaimError::DuplicateOwner {
                login: login.to_string(),
            });
        }
    }

    // After the refusals, so a rejected list costs no identity call; the token rung's seed is reused.
    let (author, author_identity_source) = match (rung, seeds.as_slice()) {
        (Rung::Token, [OwnerSpec::Resolved { login, id }]) => Some((
            ResolvedOwner {
                login: login.clone(),
                id: *id,
            },
            OwnerIdentitySource::Resolved,
        )),
        _ => resolve_author(forge).await,
    }
    .unzip();

    match confirm_with_forge(forge, &seeds).await? {
        Some(owners) => Ok(OwnerResolution {
            owners,
            // Server-confirmed is `resolved`, whatever rung supplied the login.
            source: OwnerIdentitySource::Resolved,
            author,
            author_identity_source,
        }),
        None => Ok(OwnerResolution {
            owners: take_operator_word(&seeds)?,
            source: match rung {
                // Unconfirmed is `asserted` even from the token rung; only a CI-sourced list keeps its own word.
                Rung::Explicit | Rung::Token => OwnerIdentitySource::Asserted,
                Rung::Ci => OwnerIdentitySource::CiEnvironment,
            },
            author,
            // Kept apart from `source`: an unreachable users API says nothing about the author's rung.
            author_identity_source,
        }),
    }
}

/// Who authored the run: the token identity, else the CI-environment identity, else `None`, each with
/// its rung's word. `--owner` is never consulted, and a failed identity call falls through at debug.
///
/// A login outside `[A-Za-z0-9._-]` yields `None`, never the next rung, or the CI actor is published as
/// author of a run a differently named credential made.
async fn resolve_author(forge: &dyn Forge) -> Option<(ResolvedOwner, OwnerIdentitySource)> {
    let token = match forge.authenticated_identity().await {
        Ok(Some(identity)) => Some(ResolvedOwner {
            login: identity.login,
            id: identity.id,
        }),
        // A credential with no account of its own — an App installation token.
        Ok(None) => None,
        Err(error) => {
            log::debug!("no token identity for the author field, trying the CI environment: {error}");
            None
        }
    };
    token
        .map(|owner| (owner, OwnerIdentitySource::Resolved))
        // The ladder's own reader, or two readers could disagree about a non-numeric id.
        .or_else(|| ci_identity().map(|(login, id)| (ResolvedOwner { login, id }, OwnerIdentitySource::CiEnvironment)))
        .filter(|(owner, _)| login_charset_is_valid(&owner.login))
}

/// Which rung of the ladder supplied the logins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rung {
    /// `--owner`, given at all.
    Explicit,
    /// `GITLAB_USER_*` or `GITHUB_ACTOR*`.
    Ci,
    /// The credential's own account.
    Token,
}

/// The supplied login of one spec, whichever form it took.
fn spec_login(spec: &OwnerSpec) -> &str {
    match spec {
        OwnerSpec::Login(login) | OwnerSpec::Resolved { login, .. } => login,
    }
}

/// Step one of the ladder: which logins. The detected identity is never appended to `--owner`, or the
/// invoker is silently written into a governance field they did not name themselves in.
async fn seed_logins(forge: &dyn Forge, explicit: &[OwnerSpec]) -> Result<(Vec<OwnerSpec>, Rung), ClaimError> {
    if !explicit.is_empty() {
        return Ok((explicit.to_vec(), Rung::Explicit));
    }
    if let Some((login, id)) = ci_identity() {
        return Ok((vec![OwnerSpec::Resolved { login, id }], Rung::Ci));
    }
    match forge.authenticated_identity().await {
        Ok(Some(identity)) => {
            // Strong bot check: the forge's own assertion, never a login heuristic.
            if identity.bot {
                return Err(ClaimError::BotIdentity { login: identity.login });
            }
            Ok((
                vec![OwnerSpec::Resolved {
                    login: identity.login,
                    id: identity.id,
                }],
                Rung::Token,
            ))
        }
        // Both mean "no answer", whose remedy is `--owner`, not the `LOGIN:ID` a re-raise would suggest.
        Ok(None) | Err(ForgeError::UsersApiUnavailable) => Err(ClaimError::NoActingIdentity),
        Err(other) => Err(ClaimError::Forge(other)),
    }
}

/// The CI-provided identity, GitLab before GitHub, both halves required.
///
/// A non-numeric id falls through, never `unwrap_or(0)`: indexbot's auto-merge matches on that id.
fn ci_identity() -> Option<(String, u64)> {
    let pairs = [
        (&ocx_env::GITLAB_USER_LOGIN, &ocx_env::GITLAB_USER_ID),
        (&ocx_env::GITHUB_ACTOR, &ocx_env::GITHUB_ACTOR_ID),
    ];
    for (login_var, id_var) in pairs {
        // `get` reads empty as unset, so an empty login falls through like an absent one.
        let (Some(login), Some(id)) = (login_var.get(), id_var.get()) else {
            continue;
        };
        let Ok(id) = id.parse::<u64>() else {
            continue;
        };
        return Some((login, id));
    }
    None
}

/// Step two when the users API answers: the server confirms.
///
/// `Ok(None)` means the users API is unreachable; the caller falls back to the operator's own word.
async fn confirm_with_forge(forge: &dyn Forge, seeds: &[OwnerSpec]) -> Result<Option<Vec<ResolvedOwner>>, ClaimError> {
    let mut owners: Vec<ResolvedOwner> = Vec::new();
    for spec in seeds {
        let login = spec_login(spec);
        match forge.resolve_user(login).await {
            Ok(Some(identity)) => {
                // Catches a service account with an ordinary login, which no login shape can.
                if identity.bot {
                    return Err(ClaimError::BotIdentity { login: identity.login });
                }
                // The server's spelling replaces the checked one, so a self-hosted or proxied forge could
                // otherwise ship `[`, `@` or a control byte into the root and the request body.
                if !login_charset_is_valid(&identity.login) {
                    return Err(ClaimError::InvalidOwnerLogin { login: identity.login });
                }
                // The lookup is case-insensitive, so `AliCe:8` against `alice`/`7` is a mismatch (64),
                // never unknown (79).
                if let OwnerSpec::Resolved { id, .. } = spec
                    && *id != identity.id
                {
                    return Err(ClaimError::OwnerIdMismatch {
                        login: login.to_string(),
                        supplied: *id,
                        actual: identity.id,
                    });
                }
                // Dedup by resolved id, first occurrence wins. Deliberately unguarded: `--owner alice
                // --owner AliCe` collapses here, while `take_operator_word` refuses the same pair.
                if !owners.iter().any(|owner| owner.id == identity.id) {
                    owners.push(ResolvedOwner {
                        login: identity.login,
                        id: identity.id,
                    });
                }
            }
            Ok(None) => {
                return Err(ClaimError::OwnerUnknown {
                    login: login.to_string(),
                });
            }
            Err(ForgeError::UsersApiUnavailable) => return Ok(None),
            Err(other) => return Err(ClaimError::Forge(other)),
        }
    }
    Ok(Some(owners))
}

/// Step two when the users API is unreachable: the operator's word, and only where they wrote a pair.
///
/// A bare `LOGIN` re-raises the unreachable-API error, whose message names the `LOGIN:ID` fix.
fn take_operator_word(seeds: &[OwnerSpec]) -> Result<Vec<ResolvedOwner>, ClaimError> {
    let mut owners: Vec<ResolvedOwner> = Vec::new();
    for spec in seeds {
        match spec {
            OwnerSpec::Login(_) => return Err(ClaimError::Forge(ForgeError::UsersApiUnavailable)),
            OwnerSpec::Resolved { login, id } => {
                // Only case-differing spellings reach here; with no id to collapse them, two entries in
                // a field that gates auto-merge would be worse than a refusal.
                if owners.iter().any(|owner| owner.login.eq_ignore_ascii_case(login)) {
                    return Err(ClaimError::DuplicateOwner { login: login.clone() });
                }
                owners.push(ResolvedOwner {
                    login: login.clone(),
                    id: *id,
                });
            }
        }
    }
    Ok(owners)
}

/// The weak bot detector: the bot login shapes `<login>[bot]`, `project_<n>_bot*` and `group_<n>_bot*`.
///
/// The only detector on an unconfirmed list; a GitLab service account with a chosen login matches no
/// shape, and the human reviewer is that control. A substring test would refuse `robotics`.
#[must_use]
pub fn login_has_bot_shape(login: &str) -> bool {
    login.ends_with("[bot]") || numbered_bot_shape(login, "project_") || numbered_bot_shape(login, "group_")
}

/// `<prefix><digits>_bot…` — GitLab's project and group bot login shapes.
fn numbered_bot_shape(login: &str, prefix: &str) -> bool {
    let Some(rest) = login.strip_prefix(prefix) else {
        return false;
    };
    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    digits > 0 && rest[digits..].starts_with("_bot")
}

/// Whether `login` is safe to interpolate into a request body: `[A-Za-z0-9._-]` only.
///
/// The login is the only operator free-text channel into an artifact a human merges, and the
/// push-option allowlist lets `[`, `]`, `(`, `)` and `@` through.
#[must_use]
pub fn login_charset_is_valid(login: &str) -> bool {
    !login.is_empty()
        && login
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-'))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::claim::tests::{Call, FakeForge, identity};
    use crate::forge::ForgeError;

    /// Takes the env lock and **removes all four** CI identity variables.
    ///
    /// Not two. The override seam falls through to the process environment for any
    /// key with no override, and GitHub Actions exports `GITHUB_ACTOR` and
    /// `GITHUB_ACTOR_ID` on every runner — including the one this repository's
    /// gate runs on. A ladder test that overrides only the pair it exercises
    /// therefore takes the GitHub CI rung in CI and the rung it meant to test on a
    /// laptop: green in both, measuring the environment in one.
    fn ladder_lock() -> ocx_env::overrides::EnvLock {
        let lock = ocx_env::overrides::lock();
        for key in CI_IDENTITY_VARS {
            lock.remove(key);
        }
        lock
    }

    /// A forge whose users API is reachable and knows `alice` (id 7) and `bob`
    /// (id 8). Keys are lower-cased, modelling both forges' case-insensitive
    /// login lookup.
    fn reachable_forge(token_identity: Option<crate::forge::ForgeIdentity>) -> FakeForge {
        let mut users = HashMap::new();
        users.insert("alice".to_string(), identity("alice", 7, false));
        users.insert("bob".to_string(), identity("bob", 8, false));
        users.insert("dependabot[bot]".to_string(), identity("dependabot[bot]", 99, true));
        // A service account with an operator-chosen login: `bot` is true but no
        // documented login shape matches, so only the strong form can see it.
        users.insert("release_engineer".to_string(), identity("release_engineer", 55, true));
        FakeForge {
            users: Some(users),
            token_identity,
            ..FakeForge::default()
        }
    }

    /// A forge whose users API refuses every call — a GitLab CI job token.
    fn unreachable_forge() -> FakeForge {
        FakeForge {
            users: None,
            ..FakeForge::default()
        }
    }

    fn login(value: &str) -> OwnerSpec {
        OwnerSpec::Login(value.to_string())
    }

    fn pair(value: &str, id: u64) -> OwnerSpec {
        OwnerSpec::Resolved {
            login: value.to_string(),
            id,
        }
    }

    // ── Which logins ─────────────────────────────────────────────────────────

    /// C-048 — `--owner` given **at all** replaces the list; the detected identity
    /// is never appended.
    ///
    /// Reds on: appending the token identity (or the CI identity) to an explicit
    /// list — the invoker is then silently written into a governance field they
    /// did not name themselves in.
    #[tokio::test]
    async fn owner_ladder_explicit_replaces() {
        let lock = ladder_lock();
        lock.set(&ocx_env::GITHUB_ACTOR, "carol");
        lock.set(&ocx_env::GITHUB_ACTOR_ID, "5");
        let forge = reachable_forge(Some(identity("dave", 6, false)));

        let resolution = resolve_owners(&forge, &[login("alice"), login("bob")])
            .await
            .expect("both logins resolve");

        assert_eq!(
            resolution.owners,
            vec![
                ResolvedOwner {
                    login: "alice".to_string(),
                    id: 7
                },
                ResolvedOwner {
                    login: "bob".to_string(),
                    id: 8
                },
            ],
            "exactly the two given, in order — neither carol nor dave joins them"
        );
        assert_eq!(resolution.source, OwnerIdentitySource::Resolved);
    }

    /// `--owner ""` is one entry with an empty login, not "not given".
    ///
    /// A `Vec<OwnerSpec>` cannot distinguish "empty" from "absent" — clap cannot
    /// produce an empty repeated list — so the empty *string* is the only
    /// reachable ambiguity, and treating it as absence silently writes whoever
    /// the CI environment names into a governance field.
    ///
    /// Reds on: filtering empty logins out before the ladder runs.
    #[tokio::test]
    async fn empty_explicit_login_is_refused_not_treated_as_absent() {
        let lock = ladder_lock();
        lock.set(&ocx_env::GITHUB_ACTOR, "carol");
        lock.set(&ocx_env::GITHUB_ACTOR_ID, "5");
        let forge = reachable_forge(None);

        let error = resolve_owners(&forge, &[login("")])
            .await
            .expect_err("an empty login is an operator error, not an absent list");
        assert!(
            matches!(&error, ClaimError::InvalidOwnerLogin { login } if login.is_empty()),
            "{error:?}"
        );
    }

    /// C-048 — the CI rung, under an **unreachable** users API, is labelled
    /// `ci_environment`.
    ///
    /// The discriminating input is the API state, not the rung: the same CI list
    /// under a reachable API is `resolved` (see below). Both rows are needed —
    /// either alone passes for an implementation that hardcodes one label.
    ///
    /// Reds on: labelling it `asserted`.
    #[tokio::test]
    async fn owner_ladder_ci_environment() {
        let lock = ladder_lock();
        lock.set(&ocx_env::GITLAB_USER_LOGIN, "carol");
        lock.set(&ocx_env::GITLAB_USER_ID, "5");
        let forge = unreachable_forge();

        let resolution = resolve_owners(&forge, &[]).await.expect("the CI pair is carried");

        assert_eq!(
            resolution.owners,
            vec![ResolvedOwner {
                login: "carol".to_string(),
                id: 5
            }]
        );
        assert_eq!(resolution.source, OwnerIdentitySource::CiEnvironment);
    }

    /// A CI-derived list under a **reachable** users API is still server-confirmed.
    ///
    /// The highest-value uncovered cell: the natural implementation shortcuts the
    /// CI arm straight to `ci_environment`, and then a `GITHUB_ACTOR` spelling the
    /// server disagrees with is written verbatim into the root. The server is the
    /// authority even when the CI environment already supplied a login and an id.
    ///
    /// Reds on: short-circuiting the CI arm past the confirmation step.
    #[tokio::test]
    async fn ci_list_is_still_server_confirmed_when_the_users_api_is_reachable() {
        let lock = ladder_lock();
        lock.set(&ocx_env::GITHUB_ACTOR, "AliCe");
        lock.set(&ocx_env::GITHUB_ACTOR_ID, "7");
        let forge = reachable_forge(None);

        let resolution = resolve_owners(&forge, &[]).await.expect("the CI login is confirmed");

        assert_eq!(
            resolution.owners,
            vec![ResolvedOwner {
                login: "alice".to_string(),
                id: 7
            }],
            "the server's canonical spelling replaces the CI variable's"
        );
        assert_eq!(
            resolution.source,
            OwnerIdentitySource::Resolved,
            "server-confirmed is `resolved`, whatever rung supplied the login"
        );
    }

    /// C-048 — the token-identity rung, with no `--owner` and no CI pair.
    ///
    /// All four CI variables are removed, not two. Without that this test takes
    /// the GitHub CI rung on the runner and never reaches the code it names.
    ///
    /// Reds on: consulting the CI environment before the explicit list, or
    /// skipping the token identity.
    #[tokio::test]
    async fn owner_ladder_token_identity() {
        let _lock = ladder_lock();
        let forge = reachable_forge(Some(identity("alice", 7, false)));

        let resolution = resolve_owners(&forge, &[]).await.expect("the token identity answers");

        assert_eq!(
            resolution.owners,
            vec![ResolvedOwner {
                login: "alice".to_string(),
                id: 7
            }]
        );
        assert_eq!(resolution.source, OwnerIdentitySource::Resolved);
    }

    /// C-048 — a CI pair needs **both** halves; one alone falls through.
    ///
    /// Reds on: `||` instead of `&&`, or `.or_else` per half — the login is then
    /// taken with a defaulted id and written into `owners[]`.
    #[tokio::test]
    async fn ci_pair_needs_both_halves() {
        for (key, value) in [
            (&ocx_env::GITHUB_ACTOR, "carol"),
            (&ocx_env::GITHUB_ACTOR_ID, "5"),
            (&ocx_env::GITLAB_USER_LOGIN, "carol"),
            (&ocx_env::GITLAB_USER_ID, "5"),
        ] {
            let lock = ladder_lock();
            lock.set(key, value);
            let forge = reachable_forge(Some(identity("alice", 7, false)));

            let resolution = resolve_owners(&forge, &[])
                .await
                .expect("a half pair falls through to the token identity");
            assert_eq!(
                resolution.owners,
                vec![ResolvedOwner {
                    login: "alice".to_string(),
                    id: 7
                }],
                "{} alone must not seed the list",
                key.name
            );
        }
    }

    /// C-048's precedence: both CI pairs present ⇒ GitLab wins.
    ///
    /// Stated only by list order in the design, so it needs pinning.
    ///
    /// Reds on: swapping the two arms.
    #[tokio::test]
    async fn gitlab_ci_pair_wins_over_github() {
        let lock = ladder_lock();
        lock.set(&ocx_env::GITLAB_USER_LOGIN, "carol");
        lock.set(&ocx_env::GITLAB_USER_ID, "5");
        lock.set(&ocx_env::GITHUB_ACTOR, "dave");
        lock.set(&ocx_env::GITHUB_ACTOR_ID, "6");
        let forge = unreachable_forge();

        let resolution = resolve_owners(&forge, &[]).await.expect("a CI pair answers");
        assert_eq!(
            resolution.owners,
            vec![ResolvedOwner {
                login: "carol".to_string(),
                id: 5
            }]
        );
    }

    /// A **non-numeric** CI id makes the pair unusable and falls through — it is
    /// never `unwrap_or(0)`.
    ///
    /// The ladder is a fall-through ladder and a malformed CI variable is an
    /// environment defect, not an operator error; the terminal `NoActingIdentity`
    /// still fires if nothing else answers. A zero id in a governance field that
    /// indexbot's auto-merge matches on is the worst available outcome, so the
    /// second half asserts it explicitly.
    ///
    /// Reds on: `unwrap_or(0)` (a zero id reaches the list), or refusing at 64
    /// (the fall-through never happens).
    #[tokio::test]
    async fn non_numeric_ci_id_falls_through_and_never_defaults_to_zero() {
        for spelling in ["", "not-a-number", "12x", "-1", " 5", "5 "] {
            let lock = ladder_lock();
            lock.set(&ocx_env::GITHUB_ACTOR, "carol");
            lock.set(&ocx_env::GITHUB_ACTOR_ID, spelling);
            let forge = reachable_forge(Some(identity("alice", 7, false)));

            let resolution = resolve_owners(&forge, &[])
                .await
                .expect("a malformed id falls through to the next rung");
            assert_eq!(
                resolution.owners,
                vec![ResolvedOwner {
                    login: "alice".to_string(),
                    id: 7
                }],
                "id spelling {spelling:?} must not seed the list"
            );
            assert!(
                !resolution.owners.iter().any(|owner| owner.id == 0),
                "no zero id ever reaches a governance field"
            );
        }
    }

    /// C-048's terminal rung: nothing answers ⇒ [`ClaimError::NoActingIdentity`]
    /// (64), naming `--owner`.
    ///
    /// The second row is the one an exit-code assertion cannot see: when
    /// `authenticated_identity` **errs** with `UsersApiUnavailable`, that error is
    /// swallowed and the terminal rung still fires — both outcomes exit 64, but
    /// they name different remedies, and telling an operator with no `--owner` to
    /// write `LOGIN:ID` is the wrong instruction. So the **variant** is asserted.
    ///
    /// Reds on: falling back to an empty owner list (the claim then writes
    /// `"owners": []`, which the index refuses at review time rather than at the
    /// CLI), or propagating `UsersApiUnavailable` out of the step-1 arm.
    #[tokio::test]
    async fn no_acting_identity_is_the_terminal_rung() {
        let _lock = ladder_lock();

        for forge in [reachable_forge(None), unreachable_forge()] {
            let error = resolve_owners(&forge, &[])
                .await
                .expect_err("nothing answers, so nothing is claimed for");
            assert!(
                matches!(error, ClaimError::NoActingIdentity),
                "the remedy is --owner, not the LOGIN:ID form: {error:?}"
            );
            assert!(error.to_string().contains("--owner"), "{error}");
        }
    }

    // ── Who confirms ─────────────────────────────────────────────────────────

    /// C-048 — a bare `--owner LOGIN` with the users API unreachable is
    /// **re-raised** at 64, naming the `LOGIN:ID` form.
    ///
    /// Distinct from the terminal rung above: the operator did name someone, and
    /// the fix is to spell the pair out.
    ///
    /// Reds on: swallowing the error and carrying the login with a fabricated id,
    /// or falling through to the CI rung with an explicit list in hand.
    #[tokio::test]
    async fn bare_login_with_unreachable_users_api_is_reraised() {
        let lock = ladder_lock();
        lock.set(&ocx_env::GITHUB_ACTOR, "carol");
        lock.set(&ocx_env::GITHUB_ACTOR_ID, "5");
        let forge = unreachable_forge();

        let error = resolve_owners(&forge, &[login("alice")])
            .await
            .expect_err("a bare login cannot be resolved without the users API");
        assert!(
            matches!(&error, ClaimError::Forge(ForgeError::UsersApiUnavailable)),
            "{error:?}"
        );
        assert!(error.to_string().contains("LOGIN:ID"), "{error}");
    }

    /// C-048 — an explicit `LOGIN:ID` survives an unreachable users API, labelled
    /// `asserted`.
    ///
    /// This is the zero-config CI path: without it, a job token could never claim.
    ///
    /// Reds on: re-raising (turning the zero-config path into a 64), or labelling
    /// it `resolved` (which claims a server confirmation that never happened).
    #[tokio::test]
    async fn asserted_pair_survives_an_unreachable_users_api() {
        let _lock = ladder_lock();
        let forge = unreachable_forge();

        let resolution = resolve_owners(&forge, &[pair("alice", 7)])
            .await
            .expect("the operator's word is taken");
        assert_eq!(
            resolution.owners,
            vec![ResolvedOwner {
                login: "alice".to_string(),
                id: 7
            }]
        );
        assert_eq!(resolution.source, OwnerIdentitySource::Asserted);
    }

    /// C-048 — a supplied id that disagrees is refused at 64.
    ///
    /// Reds on: dropping the comparison — `alice:<someone-else's-id>` then reaches
    /// `owners[]`.
    #[tokio::test]
    async fn owner_id_mismatch_is_refused() {
        let _lock = ladder_lock();
        let forge = reachable_forge(None);

        let error = resolve_owners(&forge, &[pair("alice", 9999)])
            .await
            .expect_err("a disagreeing id is refused");
        assert!(
            matches!(&error, ClaimError::OwnerIdMismatch { login, supplied, actual }
                if login == "alice" && *supplied == 9999 && *actual == 7),
            "{error:?}"
        );
    }

    /// The login lookup runs **first** and is case-insensitive; the id comparison
    /// follows.
    ///
    /// `--owner AliCe:8` against a server answering `alice`/`7` is therefore an id
    /// mismatch (64), never an unknown login (79). C-048's canonical-spelling rule
    /// already implies a case-insensitive lookup, so case can never be the
    /// discriminator and the id is the only one left. Two exit codes hang on this
    /// ordering.
    ///
    /// Reds on: swapping the two checks, or comparing logins case-sensitively —
    /// the exit code flips 64↔79.
    #[tokio::test]
    async fn login_check_precedes_the_id_check_and_is_case_insensitive() {
        let _lock = ladder_lock();
        let forge = reachable_forge(None);

        let error = resolve_owners(&forge, &[pair("AliCe", 8)])
            .await
            .expect_err("the id disagrees");
        assert!(
            matches!(&error, ClaimError::OwnerIdMismatch { actual, .. } if *actual == 7),
            "a known account with a wrong id is a mismatch, never an unknown login: {error:?}"
        );

        let matching = resolve_owners(&forge, &[pair("AliCe", 7)])
            .await
            .expect("the same login in another case is the same account");
        assert_eq!(
            matching.owners,
            vec![ResolvedOwner {
                login: "alice".to_string(),
                id: 7
            }],
            "and the server's spelling wins"
        );
    }

    /// C-048 — a login the forge does not know is [`ClaimError::OwnerUnknown`]
    /// (79), naming the login.
    ///
    /// Reds on: mapping `Ok(None)` to a carried-unverified owner — the claim then
    /// silently writes a non-existent account into a governance field.
    #[tokio::test]
    async fn unknown_owner_is_refused_at_not_found() {
        let _lock = ladder_lock();
        let forge = reachable_forge(None);

        let error = resolve_owners(&forge, &[login("nobody")])
            .await
            .expect_err("an unknown login is refused");
        assert!(
            matches!(&error, ClaimError::OwnerUnknown { login } if login == "nobody"),
            "{error:?}"
        );
    }

    /// A mixed list where one login resolves and one does not refuses the **whole**
    /// claim; no partial list is written.
    ///
    /// Reds on: `filter_map` over the resolutions — the unknown owner is silently
    /// dropped and the claim proceeds with a shorter list.
    #[tokio::test]
    async fn a_mixed_list_with_one_unknown_owner_refuses_the_whole_claim() {
        let _lock = ladder_lock();
        let forge = reachable_forge(None);

        let error = resolve_owners(&forge, &[login("alice"), login("nobody")])
            .await
            .expect_err("one unknown owner refuses the list");
        assert!(
            matches!(&error, ClaimError::OwnerUnknown { login } if login == "nobody"),
            "{error:?}"
        );
    }

    /// DX-79 — a login repeated **verbatim** is refused at 64 on **both** paths,
    /// over the supplied spellings; two spellings differing only in case are not
    /// a verbatim repeat and keep their per-path behaviour.
    ///
    /// The first two rows are the defect this shape exists to close: refused
    /// only on the unconfirmed arm, `--owner alice --owner alice` exits 0
    /// recording one owner when the users API answers and 64 when it does not —
    /// one argv, two outcomes, decided by an external service. The third row is
    /// the residual DX-79 records rather than guards, and it is what keeps the
    /// refusal from being written case-insensitively: the server is what makes
    /// `alice` and `AliCe` one account.
    ///
    /// Reds on: moving the duplicate refusal back into `take_operator_word`
    /// (the confirmed row exits 0 with one owner); folding case into the
    /// supplied-list refusal (the case row reds at 64); accepting the repeat on
    /// the unconfirmed arm.
    #[tokio::test]
    async fn duplicate_owners_are_refused_on_both_paths() {
        let _lock = ladder_lock();

        let confirmed = reachable_forge(None);
        let error = resolve_owners(&confirmed, &[login("alice"), login("alice")])
            .await
            .expect_err("a verbatim repeat is an operator error whatever the users API says");
        assert!(
            matches!(&error, ClaimError::DuplicateOwner { login } if login == "alice"),
            "{error:?}"
        );

        let unconfirmed = unreachable_forge();
        let error = resolve_owners(&unconfirmed, &[pair("alice", 7), pair("alice", 7)])
            .await
            .expect_err("the same argv is refused with the users API out of reach");
        assert!(
            matches!(&error, ClaimError::DuplicateOwner { login } if login == "alice"),
            "{error:?}"
        );

        // The residual: only the server can say these are one account, so a
        // confirmed list collapses them by resolved id, first occurrence first.
        let resolution = resolve_owners(&confirmed, &[login("bob"), login("alice"), login("AliCe")])
            .await
            .expect("two spellings of one account collapse");
        assert_eq!(
            resolution.owners,
            vec![
                ResolvedOwner {
                    login: "bob".to_string(),
                    id: 8
                },
                ResolvedOwner {
                    login: "alice".to_string(),
                    id: 7
                },
            ],
            "deduped by resolved id, in first-occurrence order"
        );
    }

    // ── C-049: bots ──────────────────────────────────────────────────────────

    /// C-049, **strong** form — the forge's own `bot` field refuses the owner at
    /// 64, on an explicit list as much as on a detected identity.
    ///
    /// The contract text reads unconditional and the ADR's exit table narrows it
    /// to a detected identity; the unconditional reading is the one that keeps
    /// both strengths reachable, because unconfirmed lists are overwhelmingly
    /// `--owner`-supplied. An operator typing `--owner dependabot[bot]` is the
    /// *more* likely mistake, not the less.
    ///
    /// Reds on: deleting the `bot` guard in the consumer arm, or gating it on
    /// "no `--owner` was given". Mutating `ForgeIdentity::bot` off the struct is a
    /// build break, not a red.
    #[tokio::test]
    async fn bot_identity_is_refused() {
        let _lock = ladder_lock();

        let detected = reachable_forge(Some(identity("dependabot[bot]", 99, true)));
        let error = resolve_owners(&detected, &[])
            .await
            .expect_err("a detected bot cannot own a package");
        assert!(
            matches!(&error, ClaimError::BotIdentity { login } if login == "dependabot[bot]"),
            "{error:?}"
        );
        assert!(error.to_string().contains("--owner"), "{error}");

        let explicit = reachable_forge(None);
        let error = resolve_owners(&explicit, &[login("dependabot[bot]")])
            .await
            .expect_err("an explicitly named bot is refused too");
        assert!(matches!(&error, ClaimError::BotIdentity { .. }), "{error:?}");

        // The row that makes the STRONG form the only guard that can answer.
        // `dependabot[bot]` is caught by the login shape before any lookup
        // runs, so without a bot whose login has no documented shape, deleting
        // the `bot` check in `confirm_with_forge` reds nothing.
        assert!(
            !login_has_bot_shape("release_engineer"),
            "the row is only a control while no login shape matches it"
        );
        let service_account = reachable_forge(None);
        let error = resolve_owners(&service_account, &[login("release_engineer")])
            .await
            .expect_err("the forge's own `bot` field refuses an ordinary-looking service account");
        assert!(
            matches!(&error, ClaimError::BotIdentity { login } if login == "release_engineer"),
            "{error:?}"
        );
    }

    /// C-049, **weak** form — the documented bot login shapes, with the negative
    /// controls that separate a shape check from `login.contains("bot")`.
    ///
    /// Without the negatives, a substring predicate passes every positive row and
    /// the check is a habit rather than a check — while wrongly refusing
    /// `robotics` and `bot-alice`, both perfectly ordinary logins. The hole the
    /// weak form cannot close — a GitLab service account with an operator-chosen
    /// login — is recorded in the function's own doc comment and deliberately not
    /// asserted as coverage: a test pinning that it is *not* caught would be
    /// asserting a defect.
    ///
    /// Reds on: `login.contains("bot")` (the negative rows red), or dropping any
    /// documented shape (its positive row reds).
    #[tokio::test]
    async fn bot_login_shapes_are_refused_on_an_unconfirmed_list() {
        for shape in ["alice[bot]", "project_42_bot", "project_42_bot_a1b2", "group_7_bot"] {
            assert!(login_has_bot_shape(shape), "documented bot shape: {shape}");
        }
        for human in [
            "robotics",
            "bot-alice",
            "project_manager",
            "alice[bot]x",
            "alice",
            "botany",
        ] {
            assert!(
                !login_has_bot_shape(human),
                "an ordinary login must not be refused: {human}"
            );
        }

        // And the shape check is actually consulted on an unconfirmed list.
        let _lock = ladder_lock();
        let forge = unreachable_forge();
        let error = resolve_owners(&forge, &[pair("project_42_bot", 3)])
            .await
            .expect_err("an asserted bot pair is refused by the weak form");
        assert!(
            matches!(&error, ClaimError::BotIdentity { login } if login == "project_42_bot"),
            "{error:?}"
        );
    }

    // ── C-060: the authoring identity ────────────────────────────────────────

    /// C-060 / DX-51 — the author ladder is the **reverse** of the owner
    /// ladder: the token identity outranks the CI pair.
    ///
    /// The two fields are asserted in one function because their disagreement is
    /// the property: an explicit `--owner bob` seeds the owners while the
    /// author comes from the credential, and a set `GITHUB_ACTOR` pair is
    /// present precisely so a reversed ladder has somewhere wrong to land.
    ///
    /// Reds on: swapping the two rungs inside `resolve_author` (`author`
    /// becomes carol/5); consulting `explicit` there (`author` becomes bob/8);
    /// returning `None` unconditionally.
    #[tokio::test]
    async fn author_prefers_the_token_identity_over_the_ci_environment() {
        let lock = ladder_lock();
        lock.set(&ocx_env::GITHUB_ACTOR, "carol");
        lock.set(&ocx_env::GITHUB_ACTOR_ID, "5");
        let forge = reachable_forge(Some(identity("alice", 7, false)));

        let resolution = resolve_owners(&forge, &[pair("bob", 8)])
            .await
            .expect("the explicit owner resolves");

        assert_eq!(
            resolution.author,
            Some(ResolvedOwner {
                login: "alice".to_string(),
                id: 7
            }),
            "authorship is the credential's, and the token identity outranks the CI pair"
        );
        assert_eq!(
            resolution.owners,
            vec![ResolvedOwner {
                login: "bob".to_string(),
                id: 8
            }],
            "ownership is the explicit list's, and `--owner` is never consulted for the author"
        );
    }

    /// C-060 / DX-51 — an `Err` from the identity call is **not** fatal: the
    /// author falls through to the CI pair, and to `None` when there is none.
    ///
    /// `unreachable_forge` is the GitLab job-token shape — `authenticated_identity`
    /// answers `UsersApiUnavailable` — which is the commonest state this
    /// leniency exists for. Both halves are in one function: the fall-through
    /// alone passes for an implementation that ignores the token rung entirely,
    /// and the `None` half alone passes for one that returns `None` on `Err`.
    ///
    /// Reds on: `Err(error) => return None` in `resolve_author` (the CI half
    /// becomes `None`); propagating the `Err` instead (both halves panic on the
    /// `expect`, and every job-token run loses its claim).
    #[tokio::test]
    async fn author_falls_through_to_the_ci_pair_when_the_identity_call_errs() {
        // One lock for both halves: `ocx_env::overrides::lock` is exclusive, so a
        // second acquisition inside the same test would deadlock rather than
        // fail.
        let lock = ladder_lock();
        lock.set(&ocx_env::GITLAB_USER_LOGIN, "carol");
        lock.set(&ocx_env::GITLAB_USER_ID, "5");

        let resolution = resolve_owners(&unreachable_forge(), &[pair("bob", 8)])
            .await
            .expect("an unreachable users API does not fail a claim carrying a LOGIN:ID pair");
        assert_eq!(
            resolution.author,
            Some(ResolvedOwner {
                login: "carol".to_string(),
                id: 5
            }),
            "an unreachable users API falls through to the CI pair rather than nulling the field"
        );

        for key in CI_IDENTITY_VARS {
            lock.remove(key);
        }
        let resolution = resolve_owners(&unreachable_forge(), &[pair("bob", 8)])
            .await
            .expect("a bare job token with no CI user variables still claims");
        assert_eq!(
            resolution.author, None,
            "neither rung answered, so the key is null - and the claim still succeeds"
        );
    }

    /// C-060 — `author_identity_source` names **which rung** produced the
    /// author, and is `None` exactly when the author is.
    ///
    /// The word is the whole point of the field: both rungs yield the same
    /// `{login, id}` shape, so nothing downstream can recover from the login
    /// whether the forge asserted it about the credential or an earlier
    /// pipeline step wrote it into `GITHUB_ACTOR`. One is the forge's answer;
    /// the other is not attested at all.
    ///
    /// All three states in one function, because each alone passes for a
    /// constant: `resolved` alone passes for a hardcoded `Some(Resolved)`,
    /// `ci_environment` alone for its opposite, and the `None` row is what
    /// forbids a word without an author to describe.
    ///
    /// Reds on: swapping the two words in `resolve_author`; returning one word
    /// for both rungs; setting the word where the author is `None`.
    #[tokio::test]
    async fn author_identity_source_names_the_rung_that_answered() {
        let lock = ladder_lock();
        lock.set(&ocx_env::GITHUB_ACTOR, "carol");
        lock.set(&ocx_env::GITHUB_ACTOR_ID, "5");

        // Rung one: the credential's own account, which the forge asserted.
        let resolution = resolve_owners(&reachable_forge(Some(identity("alice", 7, false))), &[pair("bob", 8)])
            .await
            .expect("the explicit owner resolves");
        assert_eq!(
            resolution.author_identity_source,
            Some(OwnerIdentitySource::Resolved),
            "the token identity is the forge's own answer about the credential"
        );
        assert_eq!(
            resolution.source,
            OwnerIdentitySource::Resolved,
            "and the owner list keeps its own word, which this one never overwrites"
        );

        // Rung two: an ordinary environment read, with no token identity at all.
        let resolution = resolve_owners(&reachable_forge(None), &[pair("bob", 8)])
            .await
            .expect("the explicit owner resolves");
        assert_eq!(
            resolution.author_identity_source,
            Some(OwnerIdentitySource::CiEnvironment),
            "the CI pair is not attested, and the word is what says so"
        );

        // Neither rung: both halves absent together.
        for key in CI_IDENTITY_VARS {
            lock.remove(key);
        }
        let resolution = resolve_owners(&reachable_forge(None), &[pair("bob", 8)])
            .await
            .expect("the explicit owner resolves");
        assert_eq!(resolution.author, None);
        assert_eq!(
            resolution.author_identity_source, None,
            "no author, no word describing one"
        );
    }

    /// C-060 / DX-51 — the token rung reuses `seed_logins`' single answer, so a
    /// run asks the users API for its own identity **at most once**.
    ///
    /// A call-count assertion, because no return value can express it: the
    /// reused seed and a second lookup produce byte-identical `author` and
    /// `owners`. `FakeForge` records every call, which is what makes the absent
    /// second one assertable.
    ///
    /// Reds on: deleting the `(Rung::Token, [OwnerSpec::Resolved { .. }])` arm
    /// in `resolve_owners` — the `_` arm then calls `resolve_author`, and the
    /// count is 2.
    #[tokio::test]
    async fn the_token_rung_asks_for_its_own_identity_exactly_once() {
        let _lock = ladder_lock();
        let forge = reachable_forge(Some(identity("alice", 7, false)));

        let resolution = resolve_owners(&forge, &[])
            .await
            .expect("the token identity seeds the list");
        assert_eq!(
            resolution.author,
            Some(ResolvedOwner {
                login: "alice".to_string(),
                id: 7
            }),
            "the seed the token rung produced IS the author"
        );
        assert_eq!(
            forge
                .calls()
                .iter()
                .filter(|call| matches!(call, Call::AuthenticatedIdentity))
                .count(),
            1,
            "the token rung's own answer is reused; asking twice doubles the identity requests on the commonest path"
        );
    }

    // ── C-067: the login charset ─────────────────────────────────────────────

    /// C-067 — a login carrying any character outside `[A-Za-z0-9._-]` is refused
    /// at the point it enters the request body.
    ///
    /// The login is the **only** operator free-text channel into an artifact a
    /// human merges under G-04: the logical name and the physical repository are
    /// constrained to `[a-z0-9._-]` by the identifier grammar, and the
    /// `--upstream-*` values reach the root file alone. The push-option allowlist
    /// does not catch it either — `[`, `]`, `(`, `)` and `@` are all printable
    /// ASCII and none is `;`. Refusing the charset is what restores C-067's
    /// literal reading rather than weakening it to a provenance-label argument.
    ///
    /// Note the interaction with the bot check: `alice[bot]` is refused as a bot
    /// on an unconfirmed list *and* as a charset violation, so the charset rows
    /// below deliberately avoid bot shapes.
    ///
    /// Reds on: dropping the charset guard — `--owner "[x](https://evil):7"` then
    /// reaches the body verbatim under an unreachable users API.
    #[tokio::test]
    async fn owner_login_charset_is_constrained() {
        for accepted in ["alice", "Alice", "a.b_c-d", "user123", "_leading", "9"] {
            assert!(login_charset_is_valid(accepted), "both forges' logins fit: {accepted}");
        }
        for refused in [
            "",
            "[x](https://evil)",
            "@alice",
            "ali ce",
            "alice/bob",
            "alice;rm",
            "ali\nce",
            "álice",
            "alice*",
        ] {
            assert!(
                !login_charset_is_valid(refused),
                "must not reach a request body: {refused}"
            );
        }

        let _lock = ladder_lock();
        let forge = unreachable_forge();
        let error = resolve_owners(&forge, &[pair("[x](https://evil)", 7)])
            .await
            .expect_err("markdown in a login is refused before it reaches the body");
        assert!(
            matches!(&error, ClaimError::InvalidOwnerLogin { login } if login == "[x](https://evil)"),
            "{error:?}"
        );
    }

    /// C-067 over the spelling that **ships** — the forge's canonical login is
    /// charset-checked too, not only the operator's.
    ///
    /// The supplied spelling is thrown away: C-048 makes the server's answer
    /// authoritative, so it is that string which reaches the committed root and
    /// the request body. A guard applied only to the supplied value therefore
    /// protects a value nothing renders — the premise `request_body`'s own
    /// comment states ("each pair from a charset-guarded login") holds only
    /// once this refusal exists.
    ///
    /// The supplied login here is clean and the answer is not, which is the
    /// only arrangement that can tell the two guards apart: a dirty supplied
    /// login is refused by the earlier one and never reaches this code.
    ///
    /// Reds on: dropping the charset guard from `confirm_with_forge` — the
    /// resolution then succeeds carrying `ali(ce)`.
    #[tokio::test]
    async fn a_forge_answer_outside_the_login_charset_is_refused() {
        let _lock = ladder_lock();
        let mut users = HashMap::new();
        // A reachable users API whose canonical spelling for `alice` carries
        // characters no ocx-side guard had yet seen.
        users.insert("alice".to_string(), identity("ali(ce)", 7, false));
        let forge = FakeForge {
            users: Some(users),
            ..FakeForge::default()
        };

        let error = resolve_owners(&forge, &[login("alice")])
            .await
            .expect_err("the server's own spelling is held to the same charset");
        assert!(
            matches!(&error, ClaimError::InvalidOwnerLogin { login } if login == "ali(ce)"),
            "the refusal names the spelling that would have shipped, not the one supplied: {error:?}"
        );
    }

    /// C-067 on the author field — an identity outside the login charset is
    /// reported as **no** author, on both rungs.
    ///
    /// `author` is published output and no refusal path guards it: the owner
    /// ladder's charset check runs over the owner seeds, and an explicit
    /// `--owner` list means the author is resolved from a credential (or from
    /// `GITHUB_ACTOR`/`GITLAB_USER_LOGIN`) that the seeds never contained. The
    /// second rung is an ordinary environment read an earlier pipeline step can
    /// set to anything, which is the reaching case.
    ///
    /// `None` rather than an error, and deliberately: the field's contract is
    /// "when known", and refusing the whole claim would fail a run whose root
    /// and request body are both unaffected. Both rungs are asserted in one
    /// function — guarding either alone leaves the other open, and the CI half
    /// is the reachable one.
    ///
    /// Reds on: dropping the `login_charset_is_valid` filter from
    /// `resolve_author` — `author` then carries the offending spelling.
    #[tokio::test]
    async fn an_author_outside_the_login_charset_is_reported_as_none() {
        let lock = ladder_lock();
        let token = reachable_forge(Some(identity("ev(il)", 9, false)));

        let resolution = resolve_owners(&token, &[login("alice")])
            .await
            .expect("the owner list is untouched by the author's spelling");
        assert_eq!(
            resolution.author, None,
            "a token identity outside the charset is not a known author"
        );
        assert_eq!(
            resolution.owners,
            vec![ResolvedOwner {
                login: "alice".to_string(),
                id: 7
            }],
            "and the claim itself still resolves — the author field is not a refusal path"
        );

        // The second rung, reachable from an ordinary environment read: no
        // token identity at all, and a hostile `GITHUB_ACTOR`.
        lock.set(&ocx_env::GITHUB_ACTOR, "ev(il)");
        lock.set(&ocx_env::GITHUB_ACTOR_ID, "9");
        let ci = reachable_forge(None);
        let resolution = resolve_owners(&ci, &[login("alice")])
            .await
            .expect("the owner list is untouched by the author's spelling");
        assert_eq!(
            resolution.author, None,
            "nor is a CI actor an earlier pipeline step wrote outside the charset"
        );
    }
}
