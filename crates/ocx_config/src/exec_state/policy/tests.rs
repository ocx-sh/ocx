// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use super::RootFlags;

#[test]
fn root_flags_has_exactly_the_five_root_flag_fields() {
    // No `..`: adding or removing a field stops this compiling.
    let RootFlags {
        config,
        remote,
        offline,
        frozen,
        index,
    } = RootFlags::default();
    assert!(config.is_none() && index.is_none());
    assert!(!remote && !offline && !frozen);
}

use std::path::PathBuf;

use super::{FieldClass, PolicyInputs, PolicyLayer, WriteMode, field_classes};
use crate::exec_state::types::{Narrowing, Policy};

fn layer_with(policy: Policy) -> PolicyLayer {
    PolicyLayer {
        policy,
        ..PolicyLayer::default()
    }
}

fn narrowing(offline: bool) -> Narrowing {
    Narrowing {
        offline,
        ..Narrowing::default()
    }
}

fn inputs<'a>(flags: &'a RootFlags, received: Option<&'a PolicyLayer>, ambient: &'a PolicyLayer) -> PolicyInputs<'a> {
    PolicyInputs {
        flags,
        no_consent_flag: false,
        received,
        ambient,
    }
}

#[test]
fn every_policy_field_has_a_class() {
    let classes = field_classes();
    let class_of = |name: &str| {
        classes
            .iter()
            .find(|(field, _)| *field == name)
            .map(|(_, class)| *class)
    };
    assert_eq!(classes.len(), 19);
    assert_eq!(class_of("remote"), Some(FieldClass::Neutral));
    assert_eq!(class_of("no_project"), Some(FieldClass::Neutral));
    assert_eq!(class_of("no_config_refresh"), Some(FieldClass::Relaxing));
    assert_eq!(class_of("no_update_check"), Some(FieldClass::Relaxing));
    assert_eq!(class_of("home"), Some(FieldClass::Relaxing));
    assert_eq!(class_of("no_verify"), Some(FieldClass::Relaxing));
}

#[test]
fn narrowing_ors_flag_received_and_ambient() {
    let received = PolicyLayer {
        narrowing: Narrowing {
            frozen: true,
            ..Narrowing::default()
        },
        ..PolicyLayer::default()
    };
    // An ambient `OCX_OFFLINE=0` is an absent `true`: it cannot undo the received `frozen` or a flag.
    let ambient = PolicyLayer::default();
    let flags = RootFlags {
        offline: true,
        ..RootFlags::default()
    };
    let (slot, disagreements) = inputs(&flags, Some(&received), &ambient).env_slot();
    assert!(slot.narrowing.offline && slot.narrowing.frozen);
    assert!(!slot.narrowing.no_consent && !slot.narrowing.credential_helper);
    assert!(disagreements.is_empty());

    let ambient = PolicyLayer {
        narrowing: narrowing(true),
        ..PolicyLayer::default()
    };
    let (slot, _) = inputs(&RootFlags::default(), Some(&received), &ambient).env_slot();
    assert!(slot.narrowing.offline && slot.narrowing.frozen);
}

#[test]
fn no_consent_flag_is_a_narrowing_input() {
    let flags = RootFlags::default();
    let ambient = PolicyLayer::default();
    let mut input = inputs(&flags, None, &ambient);
    input.no_consent_flag = true;
    assert!(input.env_slot().0.narrowing.no_consent);
    assert!(input.to_write(WriteMode::Inherit).narrowing.no_consent);
    assert!(input.to_write(WriteMode::Clean).narrowing.no_consent);
}

#[test]
fn neutral_fields_prefer_flag_then_received_then_ambient() {
    let received = layer_with(Policy {
        remote: Some(false),
        no_project: Some(true),
        ..Policy::default()
    });
    let ambient = layer_with(Policy {
        remote: Some(true),
        no_project: Some(false),
        ..Policy::default()
    });
    let (slot, disagreements) = inputs(&RootFlags::default(), Some(&received), &ambient).env_slot();
    assert_eq!(slot.policy.remote, Some(false));
    assert_eq!(slot.policy.no_project, Some(true));
    assert!(disagreements.is_empty(), "a neutral field never disagrees");

    let flags = RootFlags {
        remote: true,
        ..RootFlags::default()
    };
    let (slot, _) = inputs(&flags, Some(&received), &ambient).env_slot();
    assert_eq!(slot.policy.remote, Some(true));

    let (slot, _) = inputs(&RootFlags::default(), None, &ambient).env_slot();
    assert_eq!(slot.policy.remote, Some(true));
}

#[test]
fn an_explicit_ambient_false_beats_a_received_true_for_the_two_refresh_toggles() {
    let received = layer_with(Policy {
        no_config_refresh: Some(true),
        no_update_check: Some(true),
        ..Policy::default()
    });
    let ambient = layer_with(Policy {
        no_config_refresh: Some(false),
        no_update_check: Some(false),
        ..Policy::default()
    });
    let (slot, disagreements) = inputs(&RootFlags::default(), Some(&received), &ambient).env_slot();
    assert_eq!(slot.policy.no_config_refresh, Some(false));
    assert_eq!(slot.policy.no_update_check, Some(false));
    let fields: Vec<_> = disagreements.iter().map(|d| d.field).collect();
    assert_eq!(fields, ["no_config_refresh", "no_update_check"]);
}

#[test]
fn relaxing_fields_prefer_flag_then_ambient_then_received() {
    let received = layer_with(Policy {
        index_dir: Some(PathBuf::from("/received")),
        config_path: Some(PathBuf::from("/received.toml")),
        mirrors: Some("received".into()),
        ..Policy::default()
    });
    let ambient = layer_with(Policy {
        index_dir: Some(PathBuf::from("/ambient")),
        config_path: Some(PathBuf::from("/ambient.toml")),
        ..Policy::default()
    });
    let (slot, disagreements) = inputs(&RootFlags::default(), Some(&received), &ambient).env_slot();
    assert_eq!(slot.policy.index_dir, Some(PathBuf::from("/ambient")));
    assert_eq!(slot.policy.config_path, Some(PathBuf::from("/ambient.toml")));
    assert_eq!(
        slot.policy.mirrors.as_deref(),
        Some("received"),
        "received fills an unset ambient"
    );
    assert_eq!(disagreements.len(), 2);

    let flags = RootFlags {
        index: Some(PathBuf::from("/flag")),
        ..RootFlags::default()
    };
    let (slot, disagreements) = inputs(&flags, Some(&received), &ambient).env_slot();
    assert_eq!(slot.policy.index_dir, Some(PathBuf::from("/flag")));
    assert_eq!(
        disagreements.len(),
        1,
        "a flag decided index_dir, so only config_path disagrees"
    );
    assert_eq!(disagreements[0].field, "config_path");
}

#[test]
fn equal_relaxing_values_do_not_disagree() {
    let both = layer_with(Policy {
        no_verify: Some(true),
        ..Policy::default()
    });
    let (slot, disagreements) = inputs(&RootFlags::default(), Some(&both), &both).env_slot();
    assert_eq!(slot.policy.no_verify, Some(true));
    assert!(disagreements.is_empty());
}

#[test]
fn home_is_relaxing() {
    let received = PolicyLayer {
        home: Some(PathBuf::from("/received")),
        ..PolicyLayer::default()
    };
    let ambient = PolicyLayer {
        home: Some(PathBuf::from("/ambient")),
        ..PolicyLayer::default()
    };
    let (slot, disagreements) = inputs(&RootFlags::default(), Some(&received), &ambient).env_slot();
    assert_eq!(slot.home, Some(PathBuf::from("/ambient")));
    assert_eq!(disagreements.len(), 1);
    assert_eq!(disagreements[0].field, "home");
    let (slot, _) = inputs(&RootFlags::default(), Some(&received), &PolicyLayer::default()).env_slot();
    assert_eq!(slot.home, Some(PathBuf::from("/received")));
}

#[test]
fn without_received_the_slot_is_flags_over_ambient() {
    let ambient = layer_with(Policy {
        no_verify: Some(true),
        ..Policy::default()
    });
    let flags = RootFlags {
        config: Some(PathBuf::from("/flag.toml")),
        ..RootFlags::default()
    };
    let (slot, disagreements) = inputs(&flags, None, &ambient).env_slot();
    assert_eq!(slot.policy.no_verify, Some(true));
    assert_eq!(slot.policy.config_path, Some(PathBuf::from("/flag.toml")));
    assert!(disagreements.is_empty());
}

#[test]
fn inherit_writes_flags_and_received_and_never_ambient() {
    let received = PolicyLayer {
        narrowing: narrowing(true),
        home: Some(PathBuf::from("/received-home")),
        policy: Policy {
            no_verify: Some(true),
            index_dir: Some(PathBuf::from("/received")),
            ..Policy::default()
        },
    };
    let ambient = PolicyLayer {
        narrowing: Narrowing {
            frozen: true,
            ..Narrowing::default()
        },
        home: Some(PathBuf::from("/ambient-home")),
        policy: Policy {
            no_verify: Some(false),
            records_dir: Some(PathBuf::from("/ambient-records")),
            ..Policy::default()
        },
    };
    let flags = RootFlags {
        config: Some(PathBuf::from("/flag.toml")),
        remote: true,
        frozen: true,
        index: Some(PathBuf::from("/flag-index")),
        ..RootFlags::default()
    };
    let written = inputs(&flags, Some(&received), &ambient).to_write(WriteMode::Inherit);
    assert!(written.narrowing.offline && written.narrowing.frozen);
    assert_eq!(written.home, Some(PathBuf::from("/received-home")));
    assert_eq!(written.policy.no_verify, Some(true), "a received no_verify passes on");
    assert_eq!(
        written.policy.records_dir, None,
        "ambient never reaches an Inherit child"
    );
    assert_eq!(written.policy.config_path, Some(PathBuf::from("/flag.toml")));
    assert_eq!(written.policy.index_dir, Some(PathBuf::from("/flag-index")));
    assert_eq!(written.policy.remote, Some(true));
}

#[test]
fn only_the_five_root_flags_and_no_consent_reach_the_child() {
    let flags = RootFlags::default();
    let written = inputs(&flags, None, &PolicyLayer::default()).to_write(WriteMode::Inherit);
    assert_eq!(written, PolicyLayer::default());
    assert_eq!(written.policy.no_verify, None, "--no-verify is never written");
}

#[test]
fn clean_equals_the_env_slot() {
    let received = layer_with(Policy {
        no_verify: Some(true),
        remote: Some(true),
        ..Policy::default()
    });
    let ambient = PolicyLayer {
        narrowing: narrowing(true),
        policy: Policy {
            no_verify: Some(false),
            records_name: Some("n".into()),
            ..Policy::default()
        },
        ..PolicyLayer::default()
    };
    let flags = RootFlags {
        index: Some(PathBuf::from("/i")),
        ..RootFlags::default()
    };
    let input = inputs(&flags, Some(&received), &ambient);
    assert_eq!(input.to_write(WriteMode::Clean), input.env_slot().0);
}

#[test]
fn a_local_no_verify_zero_over_a_received_true_writes_false_under_clean() {
    let received = layer_with(Policy {
        no_verify: Some(true),
        ..Policy::default()
    });
    let ambient = layer_with(Policy {
        no_verify: Some(false),
        ..Policy::default()
    });
    let flags = RootFlags::default();
    let input = inputs(&flags, Some(&received), &ambient);
    assert_eq!(input.to_write(WriteMode::Clean).policy.no_verify, Some(false));
    assert_eq!(input.to_write(WriteMode::Inherit).policy.no_verify, Some(true));
}

#[test]
fn from_ambient_reads_only_explicitly_set_variables() {
    let env = ocx_env::overrides::lock();
    env.hermetic(std::env::temp_dir());
    assert_eq!(PolicyLayer::from_ambient(), PolicyLayer::default());

    env.set(&ocx_env::OCX_OFFLINE, "1");
    env.set(&ocx_env::OCX_NO_CONFIG_REFRESH, "0");
    env.set(&ocx_env::OCX_NO_VERIFY, "yes");
    env.set(&ocx_env::OCX_INDEX, "/idx");
    env.set(&ocx_env::OCX_MIRRORS, "{}");
    env.set(&ocx_env::OCX_RECORDS_NAME, "name");
    let home = ocx_test_support::paths::abs("/ocx-home");
    env.set(&ocx_env::OCX_HOME, home.to_str().expect("utf-8"));
    env.set(&ocx_env::__OCX_CREDENTIAL_HELPER, "1");
    let layer = PolicyLayer::from_ambient();
    assert!(layer.narrowing.offline && layer.narrowing.credential_helper);
    assert!(!layer.narrowing.frozen && !layer.narrowing.no_consent);
    assert_eq!(
        layer.policy.no_config_refresh,
        Some(false),
        "an explicit 0 is a value, not absence"
    );
    assert_eq!(layer.policy.no_verify, Some(true));
    assert_eq!(layer.policy.index_dir, Some(PathBuf::from("/idx")));
    assert_eq!(layer.policy.mirrors.as_deref(), Some("{}"));
    assert_eq!(layer.policy.records_name.as_deref(), Some("name"));
    assert_eq!(layer.policy.remote, None);
    assert_eq!(layer.home, Some(home));
}

#[test]
fn from_ambient_treats_an_unparsable_boolean_as_unset() {
    let env = ocx_env::overrides::lock();
    env.hermetic(std::env::temp_dir());
    env.set(&ocx_env::OCX_NO_VERIFY, "maybe");
    env.set(&ocx_env::OCX_OFFLINE, "maybe");
    let layer = PolicyLayer::from_ambient();
    assert_eq!(layer.policy.no_verify, None);
    assert!(!layer.narrowing.offline);
}
