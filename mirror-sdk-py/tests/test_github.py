# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors

from unittest.mock import MagicMock, patch

from ocx_gen.github import Asset, Release, list_releases


def _make_asset(name: str, url: str) -> MagicMock:
    asset = MagicMock()
    asset.name = name
    asset.browser_download_url = url
    return asset


def _make_release(
    tag: str,
    body: str = "",
    prerelease: bool = False,
    draft: bool = False,
    assets: list | None = None,
) -> MagicMock:
    release = MagicMock()
    release.tag_name = tag
    release.body = body
    release.prerelease = prerelease
    release.draft = draft
    release.assets.return_value = assets or []
    return release


@patch("ocx_gen.github._cache")
@patch("ocx_gen.github._login")
def test_list_releases_basic(mock_login, mock_cache):
    asset = _make_asset("tool.tar.gz", "https://example.com/tool.tar.gz")
    release = _make_release("v1.0.0", body="Release notes", assets=[asset])

    repo = MagicMock()
    repo.releases.return_value = [release]
    gh = MagicMock()
    gh.repository.return_value = repo
    mock_login.return_value = gh
    mock_cache.fetch_json.side_effect = lambda key, loader: loader()

    results = list_releases("owner", "repo")

    assert len(results) == 1
    r = results[0]
    assert isinstance(r, Release)
    assert r.tag_name == "v1.0.0"
    assert r.body == "Release notes"
    assert r.prerelease is False
    assert r.draft is False
    assert len(r.assets) == 1
    assert isinstance(r.assets[0], Asset)
    assert r.assets[0].name == "tool.tar.gz"


@patch("ocx_gen.github._cache")
@patch("ocx_gen.github._login")
def test_list_releases_empty(mock_login, mock_cache):
    repo = MagicMock()
    repo.releases.return_value = []
    gh = MagicMock()
    gh.repository.return_value = repo
    mock_login.return_value = gh
    mock_cache.fetch_json.side_effect = lambda key, loader: loader()

    results = list_releases("owner", "repo")
    assert results == []


@patch("ocx_gen.github._cache")
@patch("ocx_gen.github._login")
def test_list_releases_repo_not_found(mock_login, mock_cache):
    gh = MagicMock()
    gh.repository.return_value = None
    mock_login.return_value = gh
    mock_cache.fetch_json.side_effect = lambda key, loader: loader()

    try:
        list_releases("owner", "nonexistent")
        assert False, "Expected ValueError"
    except ValueError as e:
        assert "not found" in str(e)


@patch("ocx_gen.github._cache")
@patch("ocx_gen.github._login")
def test_list_releases_prerelease_and_draft(mock_login, mock_cache):
    r1 = _make_release("v2.0.0-rc1", prerelease=True)
    r2 = _make_release("v1.0.0", draft=True)

    repo = MagicMock()
    repo.releases.return_value = [r1, r2]
    gh = MagicMock()
    gh.repository.return_value = repo
    mock_login.return_value = gh
    mock_cache.fetch_json.side_effect = lambda key, loader: loader()

    results = list_releases("owner", "repo")
    assert results[0].prerelease is True
    assert results[1].draft is True


@patch("ocx_gen.github._cache")
@patch("ocx_gen.github._login")
def test_list_releases_null_body(mock_login, mock_cache):
    release = _make_release("v1.0.0")
    release.body = None

    repo = MagicMock()
    repo.releases.return_value = [release]
    gh = MagicMock()
    gh.repository.return_value = repo
    mock_login.return_value = gh
    mock_cache.fetch_json.side_effect = lambda key, loader: loader()

    results = list_releases("owner", "repo")
    assert results[0].body == ""


@patch("ocx_gen.github._cache")
@patch("ocx_gen.github._login")
def test_list_releases_multiple_assets(mock_login, mock_cache):
    assets = [
        _make_asset("tool-linux.tar.gz", "https://example.com/linux.tar.gz"),
        _make_asset("tool-darwin.tar.gz", "https://example.com/darwin.tar.gz"),
    ]
    release = _make_release("v1.0.0", assets=assets)

    repo = MagicMock()
    repo.releases.return_value = [release]
    gh = MagicMock()
    gh.repository.return_value = repo
    mock_login.return_value = gh
    mock_cache.fetch_json.side_effect = lambda key, loader: loader()

    results = list_releases("owner", "repo")
    assert len(results[0].assets) == 2
    names = {a.name for a in results[0].assets}
    assert names == {"tool-linux.tar.gz", "tool-darwin.tar.gz"}


@patch("ocx_gen.github._cache")
@patch("ocx_gen.github._login")
def test_exclude_prereleases(mock_login, mock_cache):
    stable = _make_release("v1.0.0")
    pre = _make_release("v2.0.0-rc1", prerelease=True)

    repo = MagicMock()
    repo.releases.return_value = [stable, pre]
    gh = MagicMock()
    gh.repository.return_value = repo
    mock_login.return_value = gh
    mock_cache.fetch_json.side_effect = lambda key, loader: loader()

    results = list_releases("owner", "repo", include_prereleases=False)
    assert len(results) == 1
    assert results[0].tag_name == "v1.0.0"


@patch("ocx_gen.github._cache")
@patch("ocx_gen.github._login")
def test_exclude_drafts(mock_login, mock_cache):
    stable = _make_release("v1.0.0")
    draft = _make_release("v2.0.0", draft=True)

    repo = MagicMock()
    repo.releases.return_value = [stable, draft]
    gh = MagicMock()
    gh.repository.return_value = repo
    mock_login.return_value = gh
    mock_cache.fetch_json.side_effect = lambda key, loader: loader()

    results = list_releases("owner", "repo", include_drafts=False)
    assert len(results) == 1
    assert results[0].tag_name == "v1.0.0"


@patch("ocx_gen.github._cache")
def test_cache_key_format(mock_cache):
    mock_cache.fetch_json.return_value = []

    list_releases("corretto", "corretto-21")

    mock_cache.fetch_json.assert_called_once()
    key = mock_cache.fetch_json.call_args[0][0]
    assert key == "corretto/corretto-21/releases"


@patch("ocx_gen.github._cache")
def test_cache_key_ignores_filters(mock_cache):
    mock_cache.fetch_json.return_value = []

    list_releases("o", "r", include_prereleases=False, include_drafts=False)

    key = mock_cache.fetch_json.call_args[0][0]
    assert key == "o/r/releases"


def test_release_round_trip():
    release = Release(
        tag_name="v1.0.0",
        body="notes",
        prerelease=False,
        draft=False,
        assets=[Asset(name="f.tar.gz", browser_download_url="https://x/f.tar.gz")],
    )
    assert Release.from_dict(release.to_dict()) == release
