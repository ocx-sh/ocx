// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_oci::PackageRef;
use ocx_package_manager::composer::ComposeRequest;
use ocx_project::lazy::LazyMode;
use ocx_project::{SelectedTool, ToolSource};

use super::Identifier;

/// Extra packages composed into a project-tier environment for one invocation.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct With {
    /// Add a package that `ocx.toml` does not pin to the composed environment
    ///
    /// Repeatable. The package is installed when missing and layered after
    /// the project's tools, so a project tool wins a name clash on PATH.
    /// A project tool of the same repository is replaced by the `--with`
    /// package instead, so `--with tool:2` runs version 2 of a pinned tool.
    /// Nothing is written to `ocx.toml` or `ocx.lock`.
    ///
    /// <https://ocx.sh/docs/guides/project-toolchain/>
    #[clap(long = "with", value_name = "IDENTIFIER")]
    with: Vec<Identifier>,
}

impl With {
    /// The requested packages as parsed identifiers, in the order typed.
    ///
    /// # Errors
    ///
    /// Returns an error when an identifier cannot be parsed against the default registry.
    pub fn packages(&self, default_registry: &str) -> anyhow::Result<Vec<PackageRef>> {
        Identifier::transform_all(self.with.clone(), default_registry)
    }

    /// Drops the project tools that a `--with` package replaces, matched on registry and repository.
    ///
    /// Left in, the composer refuses the second version of one repository and the run fails.
    /// Runs on the selection, before duplicate checks and host-leaf resolution, so a replaced
    /// binding that has no leaf for this host never fails the run.
    pub fn drop_overridden(packages: &[PackageRef], tools: &mut Vec<SelectedTool>) {
        tools.retain(|tool| {
            let identifier = match &tool.source {
                ToolSource::Locked(bound) => bound.declared(),
                ToolSource::Explicit(identifier) => identifier,
            };
            !Self::overrides(packages, identifier)
        });
    }

    /// Whether a `--with` package shares `identifier`'s registry and repository, at any tag.
    pub fn overrides(packages: &[PackageRef], identifier: &PackageRef) -> bool {
        packages.iter().any(|package| {
            package.registry() == identifier.registry() && package.repository() == identifier.repository()
        })
    }

    /// Compose requests for `packages`, to be placed before the project tools (the last request leads PATH).
    pub fn requests(packages: &[PackageRef], mode_for: impl Fn(&PackageRef) -> LazyMode) -> Vec<ComposeRequest> {
        packages
            .iter()
            .map(|identifier| ComposeRequest {
                identifier: identifier.clone(),
                mode: mode_for(identifier),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    #[derive(clap::Parser)]
    struct Probe {
        #[clap(flatten)]
        with: With,
    }

    #[test]
    fn absent_yields_no_packages() {
        let probe = Probe::parse_from(["probe"]);
        assert!(probe.with.packages("ocx.sh").unwrap().is_empty());
    }

    #[test]
    fn repeats_keep_typed_order() {
        let probe = Probe::parse_from(["probe", "--with", "node:20", "--with", "cmake:3"]);
        let packages = probe.with.packages("ocx.sh").unwrap();
        assert_eq!(packages.len(), 2);
        assert!(packages[0].to_string().contains("node"));
        assert!(packages[1].to_string().contains("cmake"));
    }

    #[test]
    fn requests_preserve_order() {
        let probe = Probe::parse_from(["probe", "--with", "node:20", "--with", "cmake:3"]);
        let packages = probe.with.packages("ocx.sh").unwrap();
        let requests = With::requests(&packages, |_| LazyMode::Never);
        assert_eq!(requests[0].identifier, packages[0]);
        assert_eq!(requests[1].identifier, packages[1]);
        assert_eq!(requests[0].mode, LazyMode::Never);
    }

    fn selected(binding: &str, repository: &str, registry: &str, tag: &str) -> SelectedTool {
        SelectedTool {
            binding: binding.to_owned(),
            origin: ocx_project::Origin::Explicit,
            source: ToolSource::Explicit(PackageRef::new_registry(repository, registry).clone_with_tag(tag)),
        }
    }

    #[test]
    fn drop_overridden_removes_only_tools_of_a_with_repository() {
        let mut tools = vec![
            selected("hello", "hello", "ocx.sh", "1.0.0"),
            selected("other", "other", "ocx.sh", "1.0.0"),
            selected("mirror", "hello", "mirror.example", "1.0.0"),
        ];
        let probe = Probe::parse_from(["probe", "--with", "ocx.sh/hello:2.0.0"]);

        With::drop_overridden(&probe.with.packages("ocx.sh").unwrap(), &mut tools);

        let remaining: Vec<&str> = tools.iter().map(|tool| tool.binding.as_str()).collect();
        assert_eq!(
            remaining,
            ["other", "mirror"],
            "same registry and repository only, any tag"
        );
    }

    /// A locked binding with no host leaf is dropped before resolution, so the `--with`
    /// package that replaces it does not hit `NoHostLeaf`.
    #[test]
    fn a_replaced_locked_binding_never_reaches_host_leaf_resolution() {
        use ocx_project::{LockMetadata, LockVersion, LockedTool, ProjectConfig, ProjectLock, select_tool_set};

        let config = ProjectConfig::from_toml_str("[tools]\nhello = \"ocx.sh/hello:1\"\n").unwrap();
        let lock = ProjectLock {
            metadata: LockMetadata {
                lock_version: LockVersion::V4,
                declaration_hash_version: 1,
                declaration_hash: config.declaration_hash_cached().to_string(),
                generated_by: "test".to_owned(),
                generated_at: "2026-01-01T00:00:00Z".to_owned(),
            },
            tools: vec![LockedTool {
                name: "hello".to_owned(),
                group: "default".to_owned(),
                repository: ocx_oci::Repository::new("ocx.sh", "hello"),
                declared: Some("ocx.sh/hello:1".to_owned()),
                platforms: Default::default(),
            }],
        };
        let host: ocx_oci::Platform = "linux/amd64".parse().unwrap();
        let mut selection = select_tool_set(&config, Some(&lock), &["default".to_owned()], &[]).unwrap();
        assert!(
            ocx_project::resolve_selected_tools(&selection, &host).is_err(),
            "control: the lock has no host leaf"
        );

        let probe = Probe::parse_from(["probe", "--with", "ocx.sh/hello:2.0.0"]);
        With::drop_overridden(&probe.with.packages("ocx.sh").unwrap(), &mut selection);

        assert!(
            ocx_project::resolve_selected_tools(&selection, &host)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn malformed_identifier_is_a_parse_error() {
        assert!(Probe::try_parse_from(["probe", "--with", "::bad::"]).is_err());
    }
}
