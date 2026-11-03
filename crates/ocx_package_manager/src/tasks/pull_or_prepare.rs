// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_project::lazy::LazyMode;

use super::super::PackageManager;
use super::lazy_advisory::LazyAdvisory;
use crate::Concurrency;

impl PackageManager {
    /// Warms a lazy-mode pull set into the object store: a `never` tool in full, a deferred one prepared only
    /// (metadata and launchers). Never touches `symlinks/`.
    ///
    /// # Errors
    ///
    /// A failed pull, or a deferred tool that cannot be prepared (`ResolveFailed`).
    pub async fn pull_or_prepare(
        &self,
        set: &[(LazyMode, ocx_oci::PinnedPackageRef)],
        platform: ocx_oci::Platform,
        concurrency: Concurrency,
    ) -> Result<Vec<LazyAdvisory>, crate::error::Error> {
        let (eager, deferred): (Vec<_>, Vec<_>) = set.iter().partition(|(mode, _)| *mode == LazyMode::Never);
        let eager: Vec<ocx_oci::PackageRef> = eager.into_iter().map(|(_, id)| id.clone().into()).collect();
        self.pull_all(&eager, platform.clone(), concurrency, false).await?;
        let mut advisories = Vec::new();
        for (_, identifier) in deferred {
            let identifier: ocx_oci::PackageRef = identifier.clone().into();
            let prepared = self.prepare_lazy(&identifier, platform.clone()).await.map_err(|kind| {
                crate::error::Error::ResolveFailed(vec![crate::error::PackageError::new(identifier.clone(), kind)])
            })?;
            advisories.extend(prepared.advisories);
        }
        Ok(advisories)
    }
}
