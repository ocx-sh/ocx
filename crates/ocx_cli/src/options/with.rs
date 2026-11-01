// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_oci::PackageRef;
use ocx_package_manager::composer::ComposeRequest;
use ocx_project::lazy::LazyMode;

use super::Identifier;

/// Extra packages composed into a project-tier environment for one invocation.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct With {
    /// Add a package that is not in ocx.toml to the composed environment
    ///
    /// Repeatable. The package is installed when missing and layered after
    /// the project's tools, so a project tool wins a name clash on PATH.
    /// Nothing is written to ocx.toml or ocx.lock.
    ///
    /// https://ocx.sh/docs/guides/project-toolchain/
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

    #[test]
    fn malformed_identifier_is_a_parse_error() {
        assert!(Probe::try_parse_from(["probe", "--with", "::bad::"]).is_err());
    }
}
