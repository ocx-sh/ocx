# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors

import os
import time

from ocx_gen.cache import FileCache


def test_put_and_get(tmp_path):
    cache = FileCache("test", root=tmp_path)
    cache.put("key", b"hello")
    assert cache.get("key") == b"hello"


def test_miss_on_empty(tmp_path):
    cache = FileCache("test", root=tmp_path)
    assert cache.get("nonexistent") is None


def test_miss_on_expired(tmp_path):
    cache = FileCache("test", max_age=1, root=tmp_path)
    path = tmp_path / "test" / "key"
    path.parent.mkdir(parents=True)
    path.write_bytes(b"old")
    # Backdate mtime
    old_time = time.time() - 10
    os.utime(path, (old_time, old_time))
    assert cache.get("key") is None


def test_hit_within_ttl(tmp_path):
    cache = FileCache("test", max_age=3600, root=tmp_path)
    cache.put("key", b"fresh")
    assert cache.get("key") == b"fresh"


def test_disabled_when_max_age_zero(tmp_path):
    cache = FileCache("test", max_age=0, root=tmp_path)
    cache.put("key", b"value")
    assert cache.get("key") is None


def test_nested_key(tmp_path):
    cache = FileCache("test", root=tmp_path)
    cache.put("owner/repo.json", b"data")
    assert cache.get("owner/repo.json") == b"data"


def test_fetch_calls_loader_on_miss(tmp_path):
    cache = FileCache("test", root=tmp_path)
    calls = []

    def loader():
        calls.append(1)
        return b"loaded"

    result = cache.fetch("key", loader)
    assert result == b"loaded"
    assert len(calls) == 1


def test_fetch_uses_cache_on_hit(tmp_path):
    cache = FileCache("test", root=tmp_path)
    calls = []

    def loader():
        calls.append(1)
        return b"loaded"

    cache.fetch("key", loader)
    result = cache.fetch("key", loader)
    assert result == b"loaded"
    assert len(calls) == 1


def test_fetch_bypassed_when_disabled(tmp_path):
    cache = FileCache("test", max_age=0, root=tmp_path)
    calls = []

    def loader():
        calls.append(1)
        return b"value"

    cache.fetch("key", loader)
    cache.fetch("key", loader)
    assert len(calls) == 2


def test_domain_separation(tmp_path):
    cache_a = FileCache("domain-a", root=tmp_path)
    cache_b = FileCache("domain-b", root=tmp_path)
    cache_a.put("key", b"a-value")
    cache_b.put("key", b"b-value")
    assert cache_a.get("key") == b"a-value"
    assert cache_b.get("key") == b"b-value"


def test_binary_data(tmp_path):
    cache = FileCache("test", root=tmp_path)
    data = bytes(range(256))
    cache.put("binary", data)
    assert cache.get("binary") == data


# -- JSON convenience methods -------------------------------------------------


def test_put_json_and_get_json(tmp_path):
    cache = FileCache("test", root=tmp_path)
    cache.put_json("key", {"count": 42, "items": [1, 2]})
    assert cache.get_json("key") == {"count": 42, "items": [1, 2]}


def test_get_json_miss(tmp_path):
    cache = FileCache("test", root=tmp_path)
    assert cache.get_json("missing") is None


def test_get_json_expired(tmp_path):
    cache = FileCache("test", max_age=1, root=tmp_path)
    cache.put_json("key", {"val": 1})
    path = tmp_path / "test" / "key"
    old_time = time.time() - 10
    os.utime(path, (old_time, old_time))
    assert cache.get_json("key") is None


def test_fetch_json_calls_loader_on_miss(tmp_path):
    cache = FileCache("test", root=tmp_path)
    calls = []

    def loader():
        calls.append(1)
        return {"loaded": True}

    result = cache.fetch_json("key", loader)
    assert result == {"loaded": True}
    assert len(calls) == 1


def test_fetch_json_uses_cache_on_hit(tmp_path):
    cache = FileCache("test", root=tmp_path)
    calls = []

    def loader():
        calls.append(1)
        return [1, 2, 3]

    cache.fetch_json("key", loader)
    result = cache.fetch_json("key", loader)
    assert result == [1, 2, 3]
    assert len(calls) == 1


def test_fetch_json_bypassed_when_disabled(tmp_path):
    cache = FileCache("test", max_age=0, root=tmp_path)
    calls = []

    def loader():
        calls.append(1)
        return "value"

    cache.fetch_json("key", loader)
    cache.fetch_json("key", loader)
    assert len(calls) == 2
