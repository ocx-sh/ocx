// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::Result;
use ocx_lib::package::info::Info;
use ocx_lib::package::version::Version;
use ocx_lib::publisher::Publisher;

use super::mirror_result::MirrorResult;
use super::mirror_task::VariantContext;

/// Push a bundled package to the registry and optionally cascade to rolling tags.
///
/// `cascade_versions` is the set of build-tagged versions used to compute
/// cascade blockers. Rolling tags are excluded — build-tagged versions already
/// provide correct blocking semantics.
///
/// When `variant` indicates a default variant, a second cascade pass generates
/// unadorned alias tags (e.g., `3.12.5`, `3.12`, `3`, `latest`) pointing to
/// the same manifest as the variant-prefixed tags.
pub async fn push_and_cascade(
    publisher: &Publisher,
    info: Info,
    bundle_path: &Path,
    cascade: bool,
    cascade_versions: &BTreeSet<Version>,
    variant: Option<&VariantContext>,
) -> Result<MirrorResult> {
    let version_str = info.identifier.tag_or_latest().to_string();
    let platform = info.platform.clone();

    if cascade {
        publisher
            .push_cascade(info.clone(), bundle_path, cascade_versions.clone())
            .await?;

        // Default variant aliasing: generate unadorned tags for the default variant.
        // e.g., pushing `pgo.lto-3.12.5_b1` also cascades `3.12.5`, `3.12`, `3`, `latest`.
        if let Some(ctx) = variant
            && ctx.is_default
            && let Some(version) = Version::parse(&version_str)
            && version.variant().is_some()
        {
            let bare = version.without_variant();
            let bare_tag = bare.to_string();
            let bare_id = info.identifier.clone_with_tag(bare_tag);
            let bare_info = Info {
                identifier: bare_id,
                metadata: info.metadata.clone(),
                platform: info.platform,
            };
            publisher
                .push_cascade(bare_info, bundle_path, cascade_versions.clone())
                .await?;
        }

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
