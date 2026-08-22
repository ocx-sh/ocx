// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fork identity built and verified from forge API response bodies only.
//!
//! Two X5 invariants live here, and both are per forge only in their wire
//! spelling — the guard itself is the same on every forge, which is why every
//! spelling of it sits in this one file rather than beside its client:
//!
//! 1. **A fork's parent must be the upstream.** A repository sitting at the
//!    conventional fork path that is not actually a fork of the upstream is a
//!    same-named stranger, and pushing an announce branch to it leaks the branch
//!    — and anything the write carries — to an unrelated owner. GitHub answers
//!    the question with `parent.full_name`, GitLab with
//!    `forked_from_project.id`; a missing answer is a refusal, never a pass.
//! 2. **Identity is read, never composed.** Every field comes from the response
//!    body's own path, so a fork renamed away from the upstream's project name
//!    (or living in a nested group) resolves to where it really is. A
//!    `{login}/{basename}` guess would target a different repository entirely.

use serde_json::Value;

use super::{ForgeError, ForkIdentity, RepoCoordinate};

/// Split a forge's own `namespace/project` path into a [`ForkIdentity`].
///
/// The path from the response body is the single source of the fork's identity;
/// `id` carries the forge's opaque handle where it has one. The namespace keeps
/// every segment but the last, so a fork in a nested group survives the split
/// intact.
///
/// # Errors
///
/// Returns [`ForgeError::MalformedForkFullName`] when the path is not at least
/// `namespace/project` with no empty segment.
pub fn fork_identity_from_path(full_path: &str, id: Option<u64>) -> Result<ForkIdentity, ForgeError> {
    let malformed = || ForgeError::MalformedForkFullName {
        full_path: full_path.to_string(),
    };
    let mut segments: Vec<&str> = full_path.split('/').collect();
    if segments.len() < 2 || segments.iter().any(|segment| segment.is_empty()) {
        return Err(malformed());
    }
    let project = segments.pop().ok_or_else(malformed)?.to_string();
    Ok(ForkIdentity {
        full_path: full_path.to_string(),
        namespace: segments.join("/"),
        project,
        id,
    })
}

/// Read a string field from a response body, or fail naming it.
fn string_field<'a>(body: &'a Value, path: &[&str], field: &str) -> Result<&'a str, ForgeError> {
    path.iter()
        .try_fold(body, |value, key| value.get(key))
        .and_then(Value::as_str)
        .ok_or_else(|| ForgeError::ForkFieldMissing {
            field: field.to_string(),
        })
}

/// Verify a GitHub fork response against `upstream` and build its identity.
///
/// The parent comparison is ASCII-case-insensitive: GitHub routes owner and
/// repository names case-insensitively, and the upstream half is spelled by the
/// publisher on the command line while the parent half comes from the API, so a
/// case-sensitive compare would refuse a legitimate fork of `Acme/index`.
///
/// # Errors
///
/// Returns [`ForgeError::ForkParentMismatch`] / [`ForgeError::ForkParentAbsent`]
/// when the response parent does not match the upstream, and
/// [`ForgeError::ForkFieldMissing`] / [`ForgeError::MalformedForkFullName`] when
/// the response path is absent or not in `namespace/project` form.
pub fn verify_github_fork(fork: &Value, upstream: &RepoCoordinate) -> Result<ForkIdentity, ForgeError> {
    let expected = upstream.full_path();
    match fork
        .get("parent")
        .and_then(|parent| parent.get("full_name"))
        .and_then(Value::as_str)
    {
        Some(parent) if parent.eq_ignore_ascii_case(&expected) => {}
        Some(parent) => {
            return Err(ForgeError::ForkParentMismatch {
                expected,
                actual: parent.to_string(),
            });
        }
        None => return Err(ForgeError::ForkParentAbsent { expected }),
    }
    fork_identity_from_path(string_field(fork, &["full_name"], "full_name")?, None)
}

/// Verify a GitLab project response is a fork of `upstream_id` and build its
/// identity.
///
/// GitLab answers the parent question with a **numeric project id**, which is
/// immutable — unlike a path, which a rename or a group transfer changes under
/// you. Comparing ids is therefore strictly stronger than GitHub's path compare,
/// and needs no case folding.
///
/// # Errors
///
/// Returns [`ForgeError::ForkParentMismatch`] / [`ForgeError::ForkParentAbsent`]
/// when `forked_from_project.id` does not match `upstream_id` or is absent, and
/// [`ForgeError::ForkFieldMissing`] / [`ForgeError::MalformedForkFullName`] when
/// `path_with_namespace` is absent or malformed.
pub fn verify_gitlab_fork(project: &Value, upstream_id: u64) -> Result<ForkIdentity, ForgeError> {
    let expected = upstream_id.to_string();
    match project
        .get("forked_from_project")
        .and_then(|parent| parent.get("id"))
        .and_then(Value::as_u64)
    {
        Some(parent) if parent == upstream_id => {}
        Some(parent) => {
            return Err(ForgeError::ForkParentMismatch {
                expected,
                actual: parent.to_string(),
            });
        }
        None => return Err(ForgeError::ForkParentAbsent { expected }),
    }
    let full_path = string_field(project, &["path_with_namespace"], "path_with_namespace")?;
    let id = project.get("id").and_then(Value::as_u64);
    fork_identity_from_path(full_path, id)
}

/// Verify a fork lives under `expected_namespace`.
///
/// For a personal fork this is the token identity; for a shared organization or
/// group fork (design register S12) it is the requested namespace — the verified
/// identity must match what was asked for, not merely the token account.
///
/// The comparison is on the **whole** namespace path, not its root: a fork at
/// `acme/other-group/index` is not the fork that was requested at `acme/index`,
/// and accepting it would push the announce branch into a group the publisher
/// never named.
///
/// # Errors
///
/// Returns [`ForgeError::ForkOwnerMismatch`] when the identity's namespace does
/// not match `expected_namespace`.
pub fn verify_fork_namespace(identity: &ForkIdentity, expected_namespace: &str) -> Result<(), ForgeError> {
    if identity.namespace.eq_ignore_ascii_case(expected_namespace) {
        Ok(())
    } else {
        Err(ForgeError::ForkOwnerMismatch {
            expected: expected_namespace.to_string(),
            actual: identity.namespace.clone(),
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
        let error = verify_github_fork(&fork, &upstream()).expect_err("parent mismatch must be rejected");
        assert!(matches!(error, ForgeError::ForkParentMismatch { .. }), "got {error:?}");
    }

    #[test]
    fn rejects_a_fork_response_with_no_parent() {
        let fork = json!({ "full_name": "forkuser/index", "owner": { "login": "forkuser" } });
        let error = verify_github_fork(&fork, &upstream()).expect_err("absent parent must be rejected");
        assert!(matches!(error, ForgeError::ForkParentAbsent { .. }), "got {error:?}");
    }

    #[test]
    fn accepts_a_matching_parent_case_insensitively() {
        let fork = json!({
            "full_name": "forkuser/index",
            "parent": { "full_name": "OCX-SH/index" },
        });
        let identity = verify_github_fork(&fork, &upstream()).expect("matching parent");
        assert_eq!(identity.full_path, "forkuser/index");
        assert_eq!(identity.namespace, "forkuser");
        assert_eq!(identity.project, "index");
    }

    #[test]
    fn rebuilds_a_renamed_fork_identity_from_the_path_not_the_basename() {
        // The fork was renamed away from the upstream basename `index`. Endpoints
        // must be rebuilt from the response path, not `{login}/index`.
        let fork = json!({
            "full_name": "forkuser/grimoire-index",
            "owner": { "login": "forkuser" },
            "parent": { "full_name": "ocx-sh/index" },
        });
        let identity = verify_github_fork(&fork, &upstream()).expect("renamed fork resolves");
        assert_eq!(identity.full_path, "forkuser/grimoire-index");
        assert_eq!(identity.namespace, "forkuser");
        assert_eq!(identity.project, "grimoire-index");
    }

    #[test]
    fn rejects_a_malformed_full_name() {
        let fork = json!({ "full_name": "no-slash", "parent": { "full_name": "ocx-sh/index" } });
        let error = verify_github_fork(&fork, &upstream()).expect_err("malformed full_name rejected");
        assert!(
            matches!(error, ForgeError::MalformedForkFullName { .. }),
            "got {error:?}"
        );
    }

    #[test]
    fn gitlab_fork_is_verified_by_immutable_project_id() {
        let project = json!({
            "id": 77,
            "path_with_namespace": "forkuser/index",
            "forked_from_project": { "id": 42 },
        });
        let identity = verify_gitlab_fork(&project, 42).expect("matching parent id");
        assert_eq!(identity.full_path, "forkuser/index");
        assert_eq!(identity.id, Some(77));

        let stranger = json!({
            "id": 78,
            "path_with_namespace": "forkuser/index",
            "forked_from_project": { "id": 9 },
        });
        assert!(
            matches!(
                verify_gitlab_fork(&stranger, 42),
                Err(ForgeError::ForkParentMismatch { .. })
            ),
            "a project forked from something else must be refused"
        );

        let unforked = json!({ "id": 79, "path_with_namespace": "forkuser/index" });
        assert!(
            matches!(
                verify_gitlab_fork(&unforked, 42),
                Err(ForgeError::ForkParentAbsent { .. })
            ),
            "a project that is not a fork at all must be refused"
        );
    }

    #[test]
    fn a_nested_group_fork_keeps_every_namespace_segment() {
        let project = json!({
            "id": 5,
            "path_with_namespace": "acme/platform/tooling/index",
            "forked_from_project": { "id": 42 },
        });
        let identity = verify_gitlab_fork(&project, 42).expect("nested fork resolves");
        assert_eq!(identity.namespace, "acme/platform/tooling");
        assert_eq!(identity.project, "index");
        assert_eq!(identity.namespace_root(), "acme");
        assert!(verify_fork_namespace(&identity, "acme/platform/tooling").is_ok());
        // The root alone is NOT the namespace — accepting it would place the
        // announce branch in a group the publisher never named.
        assert!(verify_fork_namespace(&identity, "acme").is_err());
    }

    #[test]
    fn verify_fork_namespace_accepts_the_requested_namespace_and_rejects_others() {
        let identity = fork_identity_from_path("ocx-contrib/index", None).expect("valid path");
        assert!(verify_fork_namespace(&identity, "ocx-contrib").is_ok());
        assert!(verify_fork_namespace(&identity, "OCX-CONTRIB").is_ok());
        let error = verify_fork_namespace(&identity, "someone-else").expect_err("wrong namespace rejected");
        assert!(matches!(error, ForgeError::ForkOwnerMismatch { .. }), "got {error:?}");
    }
}
