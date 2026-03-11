// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;

use regex::Regex;

use super::VersionInfo;

/// Parse a GitHub release into a `VersionInfo`, if its tag matches the pattern.
/// Returns `None` for drafts or non-matching tags.
fn parse_release(tag_pattern: &Regex, release: &octocrab::models::repos::Release) -> Option<VersionInfo> {
    if release.draft {
        return None;
    }

    let tag = release.tag_name.as_str();
    let captures = tag_pattern.captures(tag)?;

    let version = captures.name("version")?.as_str().to_string();
    let prerelease_suffix = captures.name("prerelease").map(|m| m.as_str().to_string());

    let full_version = match &prerelease_suffix {
        Some(pre) => format!("{version}-{pre}"),
        None => version,
    };

    let mut assets = HashMap::new();
    for asset in &release.assets {
        assets.insert(asset.name.clone(), asset.browser_download_url.clone());
    }

    Some(VersionInfo {
        version: full_version,
        assets,
        is_prerelease: release.prerelease,
    })
}

/// List all versions from GitHub Releases, paginating through all pages.
pub async fn list_versions(
    octocrab: &octocrab::Octocrab,
    owner: &str,
    repo: &str,
    tag_pattern: &Regex,
    rate_limit_ms: u64,
) -> anyhow::Result<Vec<VersionInfo>> {
    let mut versions = Vec::new();
    let mut page = 1u32;

    loop {
        let releases = octocrab
            .repos(owner, repo)
            .releases()
            .list()
            .per_page(100)
            .page(page)
            .send()
            .await?;

        let items = releases.items;
        if items.is_empty() {
            break;
        }

        for release in &items {
            if let Some(version_info) = parse_release(tag_pattern, release) {
                versions.push(version_info);
            }
        }

        if releases.next.is_none() {
            break;
        }

        page += 1;

        if rate_limit_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(rate_limit_ms)).await;
        }
    }

    Ok(versions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_pattern(pattern: &str) -> Regex {
        Regex::new(pattern).unwrap()
    }

    fn author_json() -> serde_json::Value {
        serde_json::json!({
            "login": "user",
            "id": 1,
            "node_id": "MDQ6",
            "avatar_url": "https://avatars.githubusercontent.com/u/1",
            "gravatar_id": "",
            "url": "https://api.github.com/users/user",
            "html_url": "https://github.com/user",
            "followers_url": "https://api.github.com/users/user/followers",
            "following_url": "https://api.github.com/users/user/following{/other_user}",
            "gists_url": "https://api.github.com/users/user/gists{/gist_id}",
            "starred_url": "https://api.github.com/users/user/starred{/owner}{/repo}",
            "subscriptions_url": "https://api.github.com/users/user/subscriptions",
            "organizations_url": "https://api.github.com/users/user/orgs",
            "repos_url": "https://api.github.com/users/user/repos",
            "events_url": "https://api.github.com/users/user/events{/privacy}",
            "received_events_url": "https://api.github.com/users/user/received_events",
            "type": "User",
            "site_admin": false
        })
    }

    fn make_release(
        tag: &str,
        draft: bool,
        prerelease: bool,
        asset_names: &[&str],
    ) -> octocrab::models::repos::Release {
        let author = author_json();

        let assets: Vec<serde_json::Value> = asset_names
            .iter()
            .map(|name| {
                serde_json::json!({
                    "url": "https://api.github.com/repos/test/test/releases/assets/1",
                    "id": 1,
                    "node_id": "MDEyO",
                    "name": name,
                    "label": null,
                    "content_type": "application/gzip",
                    "state": "uploaded",
                    "size": 1024,
                    "download_count": 0,
                    "created_at": "2026-01-01T00:00:00Z",
                    "updated_at": "2026-01-01T00:00:00Z",
                    "browser_download_url": format!("https://github.com/test/test/releases/download/{tag}/{name}"),
                    "uploader": author,
                })
            })
            .collect();

        serde_json::from_value(serde_json::json!({
            "url": "https://api.github.com/repos/test/test/releases/1",
            "html_url": "https://github.com/test/test/releases/tag/v1.0.0",
            "assets_url": "https://api.github.com/repos/test/test/releases/1/assets",
            "upload_url": "https://uploads.github.com/repos/test/test/releases/1/assets{?name,label}",
            "id": 1,
            "node_id": "MDc6",
            "tag_name": tag,
            "target_commitish": "main",
            "name": tag,
            "draft": draft,
            "prerelease": prerelease,
            "created_at": "2026-01-01T00:00:00Z",
            "published_at": "2026-01-01T00:00:00Z",
            "author": author,
            "assets": assets,
        }))
        .unwrap()
    }

    #[test]
    fn parse_standard_version_tag() {
        let pattern = make_pattern(r"^v(?P<version>\d+\.\d+\.\d+)$");
        let release = make_release("v3.28.0", false, false, &["cmake-3.28.0-linux-x86_64.tar.gz"]);

        let info = parse_release(&pattern, &release).unwrap();
        assert_eq!(info.version, "3.28.0");
        assert!(!info.is_prerelease);
        assert_eq!(info.assets.len(), 1);
        assert!(info.assets.contains_key("cmake-3.28.0-linux-x86_64.tar.gz"));
    }

    #[test]
    fn parse_prerelease_tag() {
        let pattern = make_pattern(r"^v(?P<version>\d+\.\d+\.\d+)(?:-(?P<prerelease>[0-9a-zA-Z]+))?$");
        let release = make_release("v3.28.0-rc1", false, true, &[]);

        let info = parse_release(&pattern, &release).unwrap();
        assert_eq!(info.version, "3.28.0-rc1");
        assert!(info.is_prerelease);
    }

    #[test]
    fn skip_draft_release() {
        let pattern = make_pattern(r"^v(?P<version>\d+\.\d+\.\d+)$");
        let release = make_release("v1.0.0", true, false, &[]);

        assert!(parse_release(&pattern, &release).is_none());
    }

    #[test]
    fn skip_non_matching_tag() {
        let pattern = make_pattern(r"^v(?P<version>\d+\.\d+\.\d+)$");
        let release = make_release("nightly-2026-01-01", false, false, &[]);

        assert!(parse_release(&pattern, &release).is_none());
    }

    #[test]
    fn multiple_assets_collected() {
        let pattern = make_pattern(r"^v(?P<version>\d+\.\d+\.\d+)$");
        let release = make_release(
            "v1.0.0",
            false,
            false,
            &[
                "tool-1.0.0-linux-amd64.tar.gz",
                "tool-1.0.0-darwin-arm64.tar.gz",
                "tool-1.0.0-windows-amd64.zip",
            ],
        );

        let info = parse_release(&pattern, &release).unwrap();
        assert_eq!(info.assets.len(), 3);
    }
}
