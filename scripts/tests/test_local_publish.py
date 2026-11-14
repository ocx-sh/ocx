"""`local_publish.py`'s proofs: the argv it builds, what it prints, and when it refuses."""

from __future__ import annotations

import json
import platform
from datetime import UTC, datetime
from pathlib import Path

import local_publish as lp
import pytest

TOKEN = "PLANTED_FAKE_TOKEN_0123456789"  # canary, not a credential
NOW = datetime(2026, 11, 13, 9, 8, 7, tzinfo=UTC)
ROOT = Path(__file__).resolve().parents[2]


@pytest.fixture
def calls(
    monkeypatch: pytest.MonkeyPatch,
) -> list[tuple[list[str], dict[str, str] | None]]:
    seen: list[tuple[list[str], dict[str, str] | None]] = []
    monkeypatch.setattr(
        lp, "run", lambda cmd, *, cwd, env=None: seen.append((cmd, env))
    )
    monkeypatch.setattr(lp, "registry_up", lambda _port: True)
    monkeypatch.setattr(platform, "system", lambda: "Linux")
    monkeypatch.setattr(platform, "machine", lambda: "x86_64")
    monkeypatch.setattr(lp, "cargo_version", lambda _root: "0.7.0")
    monkeypatch.delenv("OCX_LOCAL_REGISTRY_PORT", raising=False)
    monkeypatch.setenv("GITHUB_TOKEN", TOKEN)
    return seen


def test_publish_builds_stamped_scans_then_publishes_locally(calls) -> None:
    lines = lp.publish(ROOT, None, NOW)
    build, scan, push = (c[0] for c in calls)
    assert build == [
        "task",
        "cross:build:release",
        "TARGET=x86_64-unknown-linux-gnu",
        "CHANNEL=dev",
        "VERSION=0.7.0-dev+20261113090807",
    ]
    assert scan[1:3] == ["scripts/release_provenance_check.py", "--scan"]
    assert push[:2] == ["bash", "scripts/publish_ocx_package.sh"]
    assert (
        push[push.index("--identifier") + 1]
        == "localhost:15000/ocx-sh/dev/cli:0.7.0-dev_20261113090807"
    )
    assert "--no-keep-tag" in push and "--cascade" in push and "--sign" not in push
    assert calls[2][1]["OCX_INSECURE_REGISTRIES"] == "localhost:15000"  # type: ignore[index]
    assert len(calls) == 3, "no ghcr push without PUSH=ghcr"
    assert lines[0] == "tag: 0.7.0-dev_20261113090807"
    assert json.loads(lines[1].split("'")[1]) == {"ghcr.io": "http://localhost:15000"}
    assert lines[2] == "export OCX_INSECURE_REGISTRIES='localhost:15000'"
    assert 'ocx = "ghcr.io/ocx-sh/dev/cli:0.7.0-dev_20261113090807"' in lines[3]
    assert "local-only" in lines[4]
    assert lines[5] == "check the build in use with: ocx exec -- ocx version --verbose"


def test_the_port_comes_from_the_environment(
    calls, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("OCX_LOCAL_REGISTRY_PORT", "15999")
    lines = lp.publish(ROOT, None, NOW)
    assert (
        "localhost:15999" in lines[1]
        and calls[2][1]["OCX_INSECURE_REGISTRIES"] == "localhost:15999"
    )  # type: ignore[index]


def test_no_registry_names_the_task_that_starts_one(
    calls, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(lp, "registry_up", lambda _port: False)
    with pytest.raises(lp.Refused, match="task release:local:registry"):
        lp.publish(ROOT, None, NOW)
    assert calls == []


def test_push_ghcr_refuses_when_the_package_is_absent(
    calls, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(lp, "ghcr_package_exists", lambda _token: False)
    with pytest.raises(lp.Refused, match=r"ghcr.io/ocx-sh/dev/cli does not exist"):
        lp.publish(ROOT, "ghcr", NOW)
    assert calls == [], "refusal comes before any build"


def test_push_ghcr_publishes_with_the_token_in_the_environment_only(
    calls, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(lp, "ghcr_package_exists", lambda _token: True)
    lines = lp.publish(ROOT, "ghcr", NOW)
    argv, env = calls[3]
    assert (
        argv[argv.index("--identifier") + 1]
        == "ghcr.io/ocx-sh/dev/cli:0.7.0-dev_20261113090807"
    )
    assert env["OCX_AUTH_ghcr_io_TOKEN"] == TOKEN  # type: ignore[index]
    assert TOKEN not in "\n".join(lines) + " ".join(a for c, _ in calls for a in c)


def test_push_ghcr_without_a_token_refuses(
    calls, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.delenv("GITHUB_TOKEN")
    monkeypatch.delenv("GH_TOKEN", raising=False)
    with pytest.raises(lp.Refused, match="GITHUB_TOKEN or GH_TOKEN"):
        lp.publish(ROOT, "ghcr", NOW)


def test_the_zot_config_syncs_ghcr_on_demand(tmp_path: Path) -> None:
    config = lp.zot_config(tmp_path, 15000)
    assert config["http"] == {"address": "127.0.0.1", "port": "15000"}
    sync = config["extensions"]["sync"]  # type: ignore[index]
    assert sync["enable"] is True
    assert (
        sync["registries"][0]["urls"] == ["https://ghcr.io"]
        and sync["registries"][0]["onDemand"] is True
    )


def test_starting_a_running_registry_is_a_no_op(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.setattr(lp, "registry_up", lambda _port: True)
    monkeypatch.setattr(
        lp.subprocess, "Popen", lambda *a, **k: pytest.fail("started a second zot")
    )
    assert lp.start_registry(15000, tmp_path) == "http://localhost:15000"


@pytest.mark.parametrize("bad", ["abc", "0", "65536", "-1", ""])
def test_a_bad_port_refuses_in_one_line_not_a_traceback(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str], bad: str
) -> None:
    monkeypatch.setenv("OCX_LOCAL_REGISTRY_PORT", bad)
    assert lp.main(["registry"]) == 1
    err = capsys.readouterr().err
    assert err.startswith("local_publish: ") and len(err.splitlines()) == 1
    assert lp.main(["publish"]) == 1


def test_no_zot_on_path_refuses_naming_the_toolchain(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.setattr(lp, "registry_up", lambda _port: False)
    monkeypatch.setattr(lp.shutil, "which", lambda _name: None)
    monkeypatch.setattr(
        lp.subprocess, "Popen", lambda *a, **k: pytest.fail("started without zot")
    )
    with pytest.raises(lp.Refused, match=r"zot.*toolchain"):
        lp.start_registry(15000, tmp_path)


def test_a_failed_start_on_another_port_leaves_the_running_registry_state_alone(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.setenv("XDG_STATE_HOME", str(tmp_path))
    running = lp.state_dir(15000)
    running.mkdir(parents=True)
    (running / "zot.pid").write_text("4242\n", encoding="utf-8")
    (running / "config.json").write_text("{}\n", encoding="utf-8")

    class Dead:
        pid = 99999
        returncode = 3

        def poll(self) -> int:
            return 3

    monkeypatch.setattr(lp, "registry_up", lambda _port: False)
    monkeypatch.setattr(lp.shutil, "which", lambda _name: "/bin/zot")
    monkeypatch.setattr(lp.subprocess, "Popen", lambda *a, **k: Dead())
    with pytest.raises(lp.Refused, match="zot exited"):
        lp.start_registry(15001, lp.state_dir(15001))
    assert lp.state_dir(15001) != running
    assert (running / "zot.pid").read_text(encoding="utf-8") == "4242\n"
    assert (running / "config.json").read_text(encoding="utf-8") == "{}\n"


def test_a_failed_start_in_the_same_state_dir_keeps_the_recorded_pid(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    (tmp_path / "zot.pid").write_text("4242\n", encoding="utf-8")

    class Dead:
        pid = 99999
        returncode = 3

        def poll(self) -> int:
            return 3

    monkeypatch.setattr(lp, "registry_up", lambda _port: False)
    monkeypatch.setattr(lp.shutil, "which", lambda _name: "/bin/zot")
    monkeypatch.setattr(lp.subprocess, "Popen", lambda *a, **k: Dead())
    with pytest.raises(lp.Refused):
        lp.start_registry(15000, tmp_path)
    assert (tmp_path / "zot.pid").read_text(encoding="utf-8") == "4242\n"


@pytest.mark.parametrize(
    "failure",
    [
        lp.urllib.error.HTTPError("u", 500, "boom", {}, None),
        lp.urllib.error.URLError("no route"),
        TimeoutError("timed out"),
    ],
    ids=["http-500", "url-error", "timeout"],
)
def test_a_network_failure_on_the_package_check_refuses_without_the_token(
    failure: BaseException, monkeypatch: pytest.MonkeyPatch
) -> None:
    def boom(*_a: object, **_k: object) -> None:
        raise failure

    monkeypatch.setattr(lp._OPENER, "open", boom)
    with pytest.raises(lp.Refused) as info:
        lp.ghcr_package_exists("s3cret-token")
    assert "s3cret-token" not in str(info.value)


def test_a_redirect_on_the_package_check_is_refused_and_never_followed(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import http.server
    import threading

    hits: list[str] = []

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self) -> None:  # noqa: N802
            hits.append(self.path)
            if self.path == "/v2/ocx-sh/dev/cli/tags/list":
                self.send_response(302)
                self.send_header("Location", "/stolen")
                self.end_headers()
            else:
                self.send_response(200)
                self.end_headers()

        def log_message(self, *_a: object) -> None:
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        real = lp.urllib.request.Request
        monkeypatch.setattr(
            lp.urllib.request,
            "Request",
            lambda url, **kw: real(
                url.replace(f"https://{lp.GHCR}", f"http://127.0.0.1:{server.server_address[1]}"),
                **kw,
            ),
        )
        with pytest.raises(lp.Refused, match="HTTP 302"):
            lp.ghcr_package_exists("s3cret-token")
    finally:
        server.shutdown()
    assert hits == ["/v2/ocx-sh/dev/cli/tags/list"], "the redirect target was fetched"
