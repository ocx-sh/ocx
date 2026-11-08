// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The pieces of the spawn-site adapter that do not need the codec.

use ocx_config::exec_state::{IdentityRecord, Owner};
use ocx_package::launch::LaunchIdentities;

use super::{LaunchIdentity, launch_identities_of};

const DIGEST: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000001";

#[test]
fn a_site_that_composed_no_identities_passes_none() {
    assert!(launch_identities_of(None).is_empty());
}

#[test]
fn identities_become_one_digest_names_and_opt_out_entry_each() {
    let record = IdentityRecord {
        digest: DIGEST.to_owned(),
        names: vec!["example.com/tool:1".to_owned()],
        owner: Owner::Inherited,
        no_patches: true,
    };
    let identities = LaunchIdentities::from_replay(&[record]);

    assert_eq!(
        launch_identities_of(Some(&identities)),
        vec![LaunchIdentity {
            digest: DIGEST.to_owned(),
            names: vec!["example.com/tool:1".to_owned()],
            no_patches: true,
        }]
    );
}
