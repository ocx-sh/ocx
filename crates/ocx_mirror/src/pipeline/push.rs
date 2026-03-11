// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::Result;
use ocx_lib::package::info::Info;
use ocx_lib::package::version::Version;
use ocx_lib::publisher::Publisher;

use super::mirror_result::MirrorResult;

/// Push a bundled package to the registry and optionally cascade to rolling tags.
///
/// `cascade_versions` is the set of build-tagged versions used to compute
/// cascade blockers. Rolling tags are excluded — build-tagged versions already
/// provide correct blocking semantics.
pub async fn push_and_cascade(
    publisher: &Publisher,
    info: Info,
    bundle_path: &Path,
    cascade: bool,
    cascade_versions: &BTreeSet<Version>,
) -> Result<MirrorResult> {
    let version_str = info.identifier.tag_or_latest().to_string();
    let platform = info.platform.clone();

    if cascade {
        publisher
            .push_cascade(info, bundle_path, cascade_versions.clone())
            .await?;

        return Ok(MirrorResult::Pushed {
            version: version_str,
            platform,
            digest: String::new(),
        });
    }

    publisher.push(info, bundle_path).await?;

    Ok(MirrorResult::Pushed {
        version: version_str,
        platform,
        digest: String::new(),
    })
}
