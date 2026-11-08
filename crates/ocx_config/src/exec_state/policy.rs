// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Policy precedence in a child: what a process reads and what it writes into a child's state.

use std::path::PathBuf;

use super::types::{Narrowing, Policy};

/// The five root flags that may reach child policy, by construction nothing else can.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RootFlags {
    /// `--config` -> `policy.config_path`.
    pub config: Option<PathBuf>,
    /// `--remote` -> `policy.remote`.
    pub remote: bool,
    /// `--offline` -> `narrowing.offline`.
    pub offline: bool,
    /// `--frozen` -> `narrowing.frozen`.
    pub frozen: bool,
    /// `--index` -> `policy.index_dir`.
    pub index: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PolicyLayer {
    pub narrowing: Narrowing,
    pub home: Option<PathBuf>,
    pub policy: Policy,
}

impl PolicyLayer {
    /// The layer of explicitly set `OCX_*` variables, each read through its declaration in `ocx_env`.
    ///
    /// Unset and empty are absent; a boolean that does not parse is absent too, so the seam that
    /// owns the variable reports it when it reads it.
    pub fn from_ambient() -> Self {
        let flag = |var: &'static ocx_env::EnvVar| bool_set(var).unwrap_or(false);
        let path = |var: &'static ocx_env::EnvVar| var.get_os().map(PathBuf::from);
        Self {
            narrowing: Narrowing {
                offline: flag(&ocx_env::OCX_OFFLINE),
                frozen: flag(&ocx_env::OCX_FROZEN),
                no_consent: flag(&ocx_env::OCX_NO_CONSENT),
                credential_helper: flag(&ocx_env::__OCX_CREDENTIAL_HELPER),
            },
            // A header home is absolute; a relative `OCX_HOME` is the seam's to absolutize.
            home: path(&ocx_env::OCX_HOME).filter(|home| home.is_absolute()),
            policy: Policy {
                remote: bool_set(&ocx_env::OCX_REMOTE),
                no_project: bool_set(&ocx_env::OCX_NO_PROJECT),
                no_config_refresh: bool_set(&ocx_env::OCX_NO_CONFIG_REFRESH),
                no_update_check: bool_set(&ocx_env::OCX_NO_UPDATE_CHECK),
                config_path: path(&ocx_env::OCX_CONFIG),
                no_config: bool_set(&ocx_env::OCX_NO_CONFIG),
                index_dir: path(&ocx_env::OCX_INDEX),
                toolchain_dir: path(&ocx_env::OCX_TOOLCHAIN_DIR),
                mirrors: ocx_env::OCX_MIRRORS.get(),
                patches: ocx_env::OCX_PATCHES.get(),
                patch_snapshot_path: path(&ocx_env::OCX_PATCH_SNAPSHOT),
                managed_config: ocx_env::OCX_MANAGED_CONFIG.get(),
                no_verify: bool_set(&ocx_env::OCX_NO_VERIFY),
                records_dir: path(&ocx_env::OCX_RECORDS_DIR),
                records_name: ocx_env::OCX_RECORDS_NAME.get(),
                allow_yanked: bool_set(&ocx_env::OCX_ALLOW_YANKED),
                default_registry: ocx_env::OCX_DEFAULT_REGISTRY.get(),
                insecure_registries: ocx_env::OCX_INSECURE_REGISTRIES.get(),
            },
        }
    }
}

/// `Some` only when the variable is set to a boolean spelling.
///
/// `bool_or` answers its default for an unset or an unparsable value alike, so two probes with
/// opposite defaults agree exactly when the variable parsed.
fn bool_set(var: &'static ocx_env::EnvVar) -> Option<bool> {
    var.get_os()?;
    let low = var.bool_or(false).ok()?;
    let high = var.bool_or(true).ok()?;
    (low == high).then_some(low)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyInputs<'a> {
    pub flags: &'a RootFlags,
    /// The one narrowing exception: `--no-consent`.
    pub no_consent_flag: bool,
    /// Header always; payload when readable.
    pub received: Option<&'a PolicyLayer>,
    /// Explicitly set `OCX_*` only.
    pub ambient: &'a PolicyLayer,
}

/// Layer and Isolate map to `Inherit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteMode {
    Inherit,
    Clean,
}

/// A relaxing field where an explicit ambient value met a different received one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Disagreement {
    pub field: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldClass {
    Narrowing,
    Neutral,
    Relaxing,
}

/// The class of every policy field, `home` included.
///
/// The destructure names every [`Policy`] field and has no `..`, so a new field fails to compile
/// until it is classed here (and merged in `env_slot`, whose `Policy { .. }` literal is exhaustive too).
pub fn field_classes() -> Vec<(&'static str, FieldClass)> {
    let Policy {
        remote: _,
        no_project: _,
        no_config_refresh: _,
        no_update_check: _,
        config_path: _,
        no_config: _,
        index_dir: _,
        toolchain_dir: _,
        mirrors: _,
        patches: _,
        patch_snapshot_path: _,
        managed_config: _,
        no_verify: _,
        records_dir: _,
        records_name: _,
        allow_yanked: _,
        default_registry: _,
        insecure_registries: _,
    } = Policy::default();
    use FieldClass::{Neutral, Relaxing};
    vec![
        ("remote", Neutral),
        ("no_project", Neutral),
        ("no_config_refresh", Relaxing),
        ("no_update_check", Relaxing),
        ("config_path", Relaxing),
        ("no_config", Relaxing),
        ("index_dir", Relaxing),
        ("toolchain_dir", Relaxing),
        ("mirrors", Relaxing),
        ("patches", Relaxing),
        ("patch_snapshot_path", Relaxing),
        ("managed_config", Relaxing),
        ("no_verify", Relaxing),
        ("records_dir", Relaxing),
        ("records_name", Relaxing),
        ("allow_yanked", Relaxing),
        ("default_registry", Relaxing),
        ("insecure_registries", Relaxing),
        ("home", Relaxing),
    ]
}

/// Flag ▸ received ▸ ambient.
fn neutral<T>(flag: Option<T>, received: Option<&T>, ambient: Option<&T>) -> Option<T>
where
    T: Clone,
{
    flag.or_else(|| received.cloned()).or_else(|| ambient.cloned())
}

/// Flag ▸ explicitly set ambient ▸ received; a received value an ambient one overrides is a disagreement.
fn relaxing<T>(
    field: &'static str,
    flag: Option<T>,
    received: Option<&T>,
    ambient: Option<&T>,
    disagreements: &mut Vec<Disagreement>,
) -> Option<T>
where
    T: Clone + PartialEq,
{
    if flag.is_some() {
        return flag;
    }
    if let (Some(received), Some(ambient)) = (received, ambient)
        && received != ambient
    {
        disagreements.push(Disagreement { field });
    }
    ambient.or(received).cloned()
}

impl PolicyInputs<'_> {
    fn received_or_default(&self) -> PolicyLayer {
        self.received.cloned().unwrap_or_default()
    }

    fn flag_remote(&self) -> Option<bool> {
        self.flags.remote.then_some(true)
    }

    /// The value each seam reads where it reads the variable today, plus disagreements.
    pub fn env_slot(&self) -> (PolicyLayer, Vec<Disagreement>) {
        let received = self.received_or_default();
        let ambient = self.ambient;
        let mut out = Vec::new();

        // Narrowing: a boolean only ever tightens, so an ambient `0` cannot undo a received `true`.
        let Narrowing {
            offline,
            frozen,
            no_consent,
            credential_helper,
        } = received.narrowing;
        let narrowing = Narrowing {
            offline: self.flags.offline || offline || ambient.narrowing.offline,
            frozen: self.flags.frozen || frozen || ambient.narrowing.frozen,
            no_consent: self.no_consent_flag || no_consent || ambient.narrowing.no_consent,
            credential_helper: credential_helper || ambient.narrowing.credential_helper,
        };

        let home = relaxing("home", None, received.home.as_ref(), ambient.home.as_ref(), &mut out);

        let Policy {
            remote,
            no_project,
            no_config_refresh,
            no_update_check,
            config_path,
            no_config,
            index_dir,
            toolchain_dir,
            mirrors,
            patches,
            patch_snapshot_path,
            managed_config,
            no_verify,
            records_dir,
            records_name,
            allow_yanked,
            default_registry,
            insecure_registries,
        } = &received.policy;
        let a = &ambient.policy;
        let policy = Policy {
            remote: neutral(self.flag_remote(), remote.as_ref(), a.remote.as_ref()),
            no_project: neutral(None, no_project.as_ref(), a.no_project.as_ref()),
            no_config_refresh: relaxing(
                "no_config_refresh",
                None,
                no_config_refresh.as_ref(),
                a.no_config_refresh.as_ref(),
                &mut out,
            ),
            no_update_check: relaxing(
                "no_update_check",
                None,
                no_update_check.as_ref(),
                a.no_update_check.as_ref(),
                &mut out,
            ),
            config_path: relaxing(
                "config_path",
                self.flags.config.clone(),
                config_path.as_ref(),
                a.config_path.as_ref(),
                &mut out,
            ),
            no_config: relaxing("no_config", None, no_config.as_ref(), a.no_config.as_ref(), &mut out),
            index_dir: relaxing(
                "index_dir",
                self.flags.index.clone(),
                index_dir.as_ref(),
                a.index_dir.as_ref(),
                &mut out,
            ),
            toolchain_dir: relaxing(
                "toolchain_dir",
                None,
                toolchain_dir.as_ref(),
                a.toolchain_dir.as_ref(),
                &mut out,
            ),
            mirrors: relaxing("mirrors", None, mirrors.as_ref(), a.mirrors.as_ref(), &mut out),
            patches: relaxing("patches", None, patches.as_ref(), a.patches.as_ref(), &mut out),
            patch_snapshot_path: relaxing(
                "patch_snapshot_path",
                None,
                patch_snapshot_path.as_ref(),
                a.patch_snapshot_path.as_ref(),
                &mut out,
            ),
            managed_config: relaxing(
                "managed_config",
                None,
                managed_config.as_ref(),
                a.managed_config.as_ref(),
                &mut out,
            ),
            no_verify: relaxing("no_verify", None, no_verify.as_ref(), a.no_verify.as_ref(), &mut out),
            records_dir: relaxing(
                "records_dir",
                None,
                records_dir.as_ref(),
                a.records_dir.as_ref(),
                &mut out,
            ),
            records_name: relaxing(
                "records_name",
                None,
                records_name.as_ref(),
                a.records_name.as_ref(),
                &mut out,
            ),
            allow_yanked: relaxing(
                "allow_yanked",
                None,
                allow_yanked.as_ref(),
                a.allow_yanked.as_ref(),
                &mut out,
            ),
            default_registry: relaxing(
                "default_registry",
                None,
                default_registry.as_ref(),
                a.default_registry.as_ref(),
                &mut out,
            ),
            insecure_registries: relaxing(
                "insecure_registries",
                None,
                insecure_registries.as_ref(),
                a.insecure_registries.as_ref(),
                &mut out,
            ),
        };

        (
            PolicyLayer {
                narrowing,
                home,
                policy,
            },
            out,
        )
    }

    /// What this process writes into a child.
    ///
    /// `Inherit` writes the root flags, `--no-consent` and the received state; ambient values reach
    /// that child through its inherited environment. `Clean` drops the environment, so the state
    /// carries what this process resolved: the [`PolicyInputs::env_slot`] result.
    pub fn to_write(&self, mode: WriteMode) -> PolicyLayer {
        match mode {
            WriteMode::Clean => self.env_slot().0,
            WriteMode::Inherit => {
                let mut layer = self.received_or_default();
                layer.narrowing.offline |= self.flags.offline;
                layer.narrowing.frozen |= self.flags.frozen;
                layer.narrowing.no_consent |= self.no_consent_flag;
                if let Some(config) = &self.flags.config {
                    layer.policy.config_path = Some(config.clone());
                }
                if let Some(index) = &self.flags.index {
                    layer.policy.index_dir = Some(index.clone());
                }
                if self.flags.remote {
                    layer.policy.remote = Some(true);
                }
                layer
            }
        }
    }
}

#[cfg(test)]
mod tests;
