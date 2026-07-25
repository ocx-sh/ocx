// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fork identity built and verified from forge API response bodies only.
//!
//! Ported from grimoire `src/catalog/forge.rs` (`github_fork_target`, same
//! owner), transport-adjusted to REST-only and owned by OCX (design register
//! S5). Two X5 invariants live here: (1) a fork's `parent.full_name` must equal
//! the upstream — otherwise it is a same-named stranger repository, refused
//! before any write; (2) every identity field is read from the response body's
//! own `full_name`, never composed as `{login}/{basename}`, so a renamed fork
//! resolves to its real name.

use serde_json::Value;

use super::{ForgeError, ForkIdentity, RepoCoordinate};

/// Verify a fork API response body against `upstream` and build its identity.
///
/// # Errors
///
/// Returns [`ForgeError::ForkParentMismatch`] / [`ForgeError::ForkParentAbsent`]
/// when the response parent does not match the upstream, and
/// [`ForgeError::ForkFieldMissing`] / [`ForgeError::MalformedForkFullName`] when
/// the response `full_name` is absent or not in `owner/repo` form.
pub fn verify_fork_identity(fork: &Value, upstream: &RepoCoordinate) -> Result<ForkIdentity, ForgeError> {
    let upstream_full_name = upstream.full_name();
    match fork
        .get("parent")
        .and_then(|parent| parent.get("full_name"))
        .and_then(Value::as_str)
    {
        Some(parent) if parent.eq_ignore_ascii_case(&upstream_full_name) => {}
        Some(parent) => {
            return Err(ForgeError::ForkParentMismatch {
                expected: upstream_full_name,
                actual: parent.to_string(),
            });
        }
        None => {
            return Err(ForgeError::ForkParentAbsent {
                expected: upstream_full_name,
            });
        }
    }
    // The `full_name` from the response body is the single source of the fork's
    // identity: owner and repo are its two halves. Never reassembled from the
    // upstream basename, so a renamed fork keeps its real name.
    let full_name = fork
        .get("full_name")
        .and_then(Value::as_str)
        .ok_or_else(|| ForgeError::ForkFieldMissing {
            field: "full_name".to_string(),
        })?;
    let (owner, repo) = full_name
        .split_once('/')
        .filter(|(owner, repo)| !owner.is_empty() && !repo.is_empty() && !repo.contains('/'))
        .ok_or_else(|| ForgeError::MalformedForkFullName {
            full_name: full_name.to_string(),
        })?;
    Ok(ForkIdentity {
        full_name: full_name.to_string(),
        owner: owner.to_string(),
        repo: repo.to_string(),
    })
}

/// Verify a fork lives under `expected_owner`.
///
/// For a personal fork this is the token identity; for the shared
/// `ocx-contrib/index` fork (S12) it is the requested organization — the
/// verified identity must match, not just the token account.
///
/// # Errors
///
/// Returns [`ForgeError::ForkOwnerMismatch`] when the identity's owner does not
/// match `expected_owner`.
pub fn verify_fork_owner(identity: &ForkIdentity, expected_owner: &str) -> Result<(), ForgeError> {
    if identity.owner.eq_ignore_ascii_case(expected_owner) {
        Ok(())
    } else {
        Err(ForgeError::ForkOwnerMismatch {
            expected: expected_owner.to_string(),
            actual: identity.owner.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn upstream() -> RepoCoordinate {
        "ocx-sh/index".parse().expect("valid coordinate")
    }

    #[test]
    fn rejects_a_fork_whose_parent_is_a_stranger_repository() {
        let fork = json!({
            "full_name": "forkuser/index",
            "owner": { "login": "forkuser" },
            "parent": { "full_name": "stranger/index" },
        });
        let error = verify_fork_identity(&fork, &upstream()).expect_err("parent mismatch must be rejected");
        assert!(matches!(error, ForgeError::ForkParentMismatch { .. }), "got {error:?}");
    }

    #[test]
    fn rejects_a_fork_response_with_no_parent() {
        let fork = json!({ "full_name": "forkuser/index", "owner": { "login": "forkuser" } });
        let error = verify_fork_identity(&fork, &upstream()).expect_err("absent parent must be rejected");
        assert!(matches!(error, ForgeError::ForkParentAbsent { .. }), "got {error:?}");
    }

    #[test]
    fn accepts_a_matching_parent_case_insensitively() {
        let fork = json!({
            "full_name": "forkuser/index",
            "parent": { "full_name": "OCX-SH/index" },
        });
        let identity = verify_fork_identity(&fork, &upstream()).expect("matching parent");
        assert_eq!(identity.full_name, "forkuser/index");
        assert_eq!(identity.owner, "forkuser");
        assert_eq!(identity.repo, "index");
    }

    #[test]
    fn rebuilds_a_renamed_fork_identity_from_full_name_not_the_basename() {
        // The fork was renamed away from the upstream basename `index`. Endpoints
        // must be rebuilt from the response `full_name`, not `{login}/index`.
        let fork = json!({
            "full_name": "forkuser/grimoire-index",
            "owner": { "login": "forkuser" },
            "parent": { "full_name": "ocx-sh/index" },
        });
        let identity = verify_fork_identity(&fork, &upstream()).expect("renamed fork resolves");
        assert_eq!(identity.full_name, "forkuser/grimoire-index");
        assert_eq!(identity.owner, "forkuser");
        assert_eq!(identity.repo, "grimoire-index");
    }

    #[test]
    fn rejects_a_malformed_full_name() {
        let fork = json!({ "full_name": "no-slash", "parent": { "full_name": "ocx-sh/index" } });
        let error = verify_fork_identity(&fork, &upstream()).expect_err("malformed full_name rejected");
        assert!(
            matches!(error, ForgeError::MalformedForkFullName { .. }),
            "got {error:?}"
        );
    }

    #[test]
    fn verify_fork_owner_accepts_the_requested_owner_and_rejects_others() {
        let identity = ForkIdentity {
            full_name: "ocx-contrib/index".to_string(),
            owner: "ocx-contrib".to_string(),
            repo: "index".to_string(),
        };
        assert!(verify_fork_owner(&identity, "ocx-contrib").is_ok());
        assert!(verify_fork_owner(&identity, "OCX-CONTRIB").is_ok());
        let error = verify_fork_owner(&identity, "someone-else").expect_err("wrong owner rejected");
        assert!(matches!(error, ForgeError::ForkOwnerMismatch { .. }), "got {error:?}");
    }
}
