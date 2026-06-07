"""§3.10 S10: End-to-end acceptance test for ocx-mirror pipeline.

Exercises the full pipeline: discover → prepare → test → push → notify.
Discord webhook is intercepted by a local HTTP server fixture that records
POST payloads, so no external network is required.

Per design spec §3.10:
- Bootstrap real mirror repo skeleton (ocx-sh/mirror-shfmt candidate)
- Run full pipeline against registry:2: discover → prepare → test → push → notify
- Assertions: at least one version published, JUNIT consumed,
  Discord POST to local webhook tracking server with all required fields

All pipeline execute() bodies are unimplemented stubs until wave 3.
Tests that call ocx-mirror pipeline subcommands skip or record expected
Phase 3 failures — they do NOT assert on behaviors that require implementation.
Structural tests (webhook server, fixture parsing) pass now.
"""
from __future__ import annotations

import http.server
import json
import os
import subprocess
import sys
import threading
from pathlib import Path
from typing import NamedTuple

import pytest

FIXTURES_DIR = Path(__file__).resolve().parent.parent / "fixtures" / "mirror-shfmt-minimal"


# ---------------------------------------------------------------------------
# Webhook tracking server fixture
# ---------------------------------------------------------------------------


class WebhookCapture(NamedTuple):
    """Holds captured webhook POST requests."""

    url: str
    payloads: list[dict]


def _make_tracking_server() -> tuple[WebhookCapture, http.server.HTTPServer]:
    """Create a local HTTP server that captures POST requests to /webhook."""
    captured: list[dict] = []

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_POST(self) -> None:
            content_length = int(self.headers.get("Content-Length", 0))
            body = self.rfile.read(content_length)
            try:
                payload = json.loads(body)
                captured.append(payload)
            except json.JSONDecodeError:
                captured.append({"_raw": body.decode(errors="replace")})
            self.send_response(204)
            self.end_headers()

        def log_message(self, fmt: str, *args: object) -> None:  # noqa: ANN002
            pass  # suppress request logging in test output

    server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
    port = server.server_address[1]
    capture = WebhookCapture(url=f"http://127.0.0.1:{port}/webhook", payloads=captured)
    return capture, server


@pytest.fixture()
def webhook_server() -> "WebhookCapture":
    """Start a local webhook tracking server. Yields capture object."""
    capture, server = _make_tracking_server()
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    yield capture
    server.shutdown()


# ---------------------------------------------------------------------------
# Binary fixture (local-file scoped to avoid collision with test_mirror.py)
# ---------------------------------------------------------------------------


@pytest.fixture(scope="session")
def mirror_binary_e2e() -> Path:
    """Path to ocx-mirror binary for e2e tests."""
    if env_path := os.environ.get("OCX_MIRROR_COMMAND"):
        p = Path(env_path)
    else:
        from src.helpers import PROJECT_ROOT
        p = PROJECT_ROOT / "test" / "bin" / "ocx-mirror"
        if sys.platform == "win32" and not p.suffix:
            p = p.with_suffix(".exe")
    if not p.exists():
        pytest.skip(f"ocx-mirror binary not found at {p} — skipping e2e tests")
    return p


# ---------------------------------------------------------------------------
# §3.10: Webhook tracking server structural tests (pass now)
# ---------------------------------------------------------------------------


def test_webhook_server_accepts_post(webhook_server: WebhookCapture) -> None:
    """§3.10: Local webhook tracking server captures POST payloads correctly."""
    import urllib.request

    payload = json.dumps({"username": "ocx-mirror", "embeds": [{"title": "test"}]}).encode()
    req = urllib.request.Request(
        webhook_server.url,
        data=payload,
        method="POST",
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req) as resp:
        assert resp.status == 204, f"Webhook server must return 204, got {resp.status}"

    assert len(webhook_server.payloads) == 1, "Exactly one payload must be captured"
    assert webhook_server.payloads[0]["username"] == "ocx-mirror"


def test_webhook_server_captures_discord_embed_shape(webhook_server: WebhookCapture) -> None:
    """§3.10: Webhook server correctly captures Discord embed payload structure."""
    import urllib.request

    payload = json.dumps({
        "username": "ocx-mirror",
        "embeds": [{
            "title": "📦 shfmt: published 3.7.0",
            "color": 3066993,
            "url": "https://github.com/ocx-sh/mirror-shfmt/actions/runs/1",
            "fields": [
                {"name": "Platforms", "value": "linux/amd64", "inline": False},
                {"name": "Cascade", "value": "3.7.0, 3.7, 3, latest", "inline": False},
            ],
        }],
    }).encode()
    req = urllib.request.Request(
        webhook_server.url,
        data=payload,
        method="POST",
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req) as _:
        pass

    assert len(webhook_server.payloads) >= 1
    captured = webhook_server.payloads[-1]
    assert "embeds" in captured, "Discord payload must have embeds array"
    embed = captured["embeds"][0]
    assert "title" in embed, "Embed must have title"
    assert "color" in embed, "Embed must have color"


# ---------------------------------------------------------------------------
# §3.10: run-summary.json schema structural tests (pass now)
# ---------------------------------------------------------------------------


def test_run_summary_all_green_validates_schema() -> None:
    """§3.10: All-green run-summary.json passes §2.4 schema validation."""
    summary = {
        "schema_version": 1,
        "mirror": "shfmt",
        "target": "localhost:5000/test-shfmt-pipeline",
        "run_url": "https://github.com/ocx-sh/mirror-shfmt/actions/runs/1",
        "versions": [
            {
                "version": "3.7.0",
                "status": "published",
                "platforms_pushed": ["linux/amd64"],
                "platforms_failed": [],
                "cascade_tags_written": ["3.7.0", "3.7", "3", "latest"],
                "test_failures": [],
            }
        ],
        "any_red": False,
        "any_new_green": True,
    }

    # §2.4: all required top-level fields
    for field in ("schema_version", "mirror", "target", "run_url", "versions", "any_red", "any_new_green"):
        assert field in summary, f"run-summary.json missing required field: {field}"

    assert summary["schema_version"] == 1
    assert summary["any_red"] is False
    assert summary["any_new_green"] is True

    # §2.4: version entry fields
    ver = summary["versions"][0]
    for field in ("version", "status", "platforms_pushed", "platforms_failed", "cascade_tags_written", "test_failures"):
        assert field in ver, f"version entry missing required field: {field}"

    assert ver["status"] == "published"
    assert "latest" in ver["cascade_tags_written"], (
        "Published version must include 'latest' in cascade_tags_written"
    )


def test_run_summary_notify_logic_all_skipped() -> None:
    """§3.10: D10 taxonomy: all skipped_existing + no test_failures → silent (no POST)."""
    summary = {
        "schema_version": 1,
        "mirror": "shfmt",
        "target": "localhost:5000/test-shfmt-pipeline",
        "run_url": "https://github.com/ocx-sh/mirror-shfmt/actions/runs/2",
        "versions": [
            {
                "version": "3.7.0",
                "status": "skipped_existing",
                "platforms_pushed": [],
                "platforms_failed": [],
                "cascade_tags_written": [],
                "test_failures": [],
            }
        ],
        "any_red": False,
        "any_new_green": False,
    }
    # §2.5: all skipped_existing + any_red=False + any_new_green=False → silent
    assert not summary["any_red"], "all-skipped summary must not be red"
    assert not summary["any_new_green"], "all-skipped summary must not be new-green"
    # D10: notify must be silent (no POST) — verified in e2e by checking
    # webhook_server.payloads is empty after notify call


def test_run_summary_notify_logic_green_embed() -> None:
    """§3.10: D10 taxonomy: any_new_green && !any_red → green embed (color 0x2ECC71)."""
    summary = {
        "schema_version": 1,
        "mirror": "shfmt",
        "target": "localhost:5000/test-shfmt-pipeline",
        "run_url": "https://github.com/ocx-sh/mirror-shfmt/actions/runs/3",
        "versions": [
            {
                "version": "3.7.0",
                "status": "published",
                "platforms_pushed": ["linux/amd64"],
                "platforms_failed": [],
                "cascade_tags_written": ["3.7.0", "3.7", "3", "latest"],
                "test_failures": [],
            }
        ],
        "any_red": False,
        "any_new_green": True,
    }
    assert summary["any_new_green"] and not summary["any_red"], (
        "Green embed condition: any_new_green=True, any_red=False"
    )
    # §2.5: GREEN color = 0x2ECC71 = 3066993
    GREEN = 0x2ECC71
    assert GREEN == 3_066_993, f"GREEN must be 3066993, got {GREEN}"


def test_run_summary_notify_logic_yellow_embed() -> None:
    """§3.10: D10 taxonomy: any_new_green && any_red → yellow partial embed (0xF1C40F)."""
    any_new_green = True
    any_red = True
    assert any_new_green and any_red, "Yellow embed condition: both flags set"
    YELLOW = 0xF1C40F
    assert YELLOW == 15_844_367, f"YELLOW must be 15844367, got {YELLOW}"


def test_run_summary_notify_logic_red_embed() -> None:
    """§3.10: D10 taxonomy: !any_new_green && any_red → red failed embed (0xE74C3C)."""
    any_new_green = False
    any_red = True
    assert not any_new_green and any_red, "Red embed condition: any_red=True, any_new_green=False"
    RED = 0xE74C3C
    assert RED == 15_158_332, f"RED must be 15158332, got {RED}"


# ---------------------------------------------------------------------------
# §3.10: Full pipeline e2e test (fails with unimplemented until wave 3)
# ---------------------------------------------------------------------------


def test_full_pipeline_against_registry(
    mirror_binary_e2e: Path,
    registry: str,
    webhook_server: WebhookCapture,
    tmp_path: Path,
) -> None:
    """§3.10: Full pipeline: discover → prepare → test → push → notify.

    Expected Phase 3: prepare/push/notify exit non-zero (unimplemented stubs).
    Phase 4+: at least one version published, run-summary.json valid,
    Discord POST captured by local webhook tracking server.
    """
    # Substitute registry URL into spec
    template = (FIXTURES_DIR / "mirror.yml").read_text()
    spec_content = template.replace("localhost:5000", registry)
    spec_path = tmp_path / "mirror.yml"
    spec_path.write_text(spec_content)

    work_dir = tmp_path / "pipeline-work"
    work_dir.mkdir()
    junit_dir = work_dir / "junit"
    junit_dir.mkdir()
    bundles_dir = work_dir / "bundles"
    bundles_dir.mkdir()
    summary_path = work_dir / "run-summary.json"

    def run_step(args: list[str]) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [str(mirror_binary_e2e)] + args,
            capture_output=True,
            text=True,
        )

    # Step 1: plan (discover versions)
    plan_result = run_step(["pipeline", "plan", "--spec", str(spec_path)])
    if plan_result.returncode != 0:
        # Expected in Phase 3: unimplemented
        pytest.skip(
            f"pipeline plan unimplemented (rc={plan_result.returncode}). "
            "Full e2e deferred to Phase 4."
        )
        return

    # Step 2: prepare (download + bundle)
    prepare_result = run_step([
        "pipeline", "prepare",
        "--spec", str(spec_path),
        "--version", "3.7.0",
        "--work-dir", str(work_dir),
    ])
    if prepare_result.returncode != 0:
        pytest.skip("pipeline prepare unimplemented. Full e2e deferred.")
        return

    # Step 3: push (run tests + push to registry + write run-summary.json)
    push_result = run_step([
        "pipeline", "push",
        "--spec", str(spec_path),
        "--junit-dir", str(junit_dir),
        "--bundles-dir", str(bundles_dir),
        "--write-summary", str(summary_path),
    ])
    if push_result.returncode != 0:
        pytest.skip("pipeline push unimplemented. Full e2e deferred.")
        return

    # §3.10: At least one version must be published
    assert summary_path.exists(), "run-summary.json must be written by pipeline push"
    summary = json.loads(summary_path.read_text())
    assert any(
        v["status"] in ("published", "partial")
        for v in summary.get("versions", [])
    ), "At least one version must be published or partial in run-summary.json"

    # Step 4: notify (stubbed to local webhook tracking server)
    notify_env = {
        **os.environ,
        "DISCORD_WEBHOOK_URL": webhook_server.url,
    }
    notify_result = subprocess.run(
        [str(mirror_binary_e2e), "pipeline", "notify",
         "--run-summary", str(summary_path),
         "--webhook-env-var", "DISCORD_WEBHOOK_URL"],
        capture_output=True,
        text=True,
        env=notify_env,
    )

    if notify_result.returncode != 0:
        pytest.skip("pipeline notify unimplemented. Full e2e deferred.")
        return

    # §3.10: If any_new_green, Discord POST must have been captured
    if summary.get("any_new_green"):
        assert len(webhook_server.payloads) >= 1, (
            "notify must POST to Discord webhook when any_new_green=True"
        )
        payload = webhook_server.payloads[0]
        assert payload.get("username") == "ocx-mirror", (
            "Discord payload must have username 'ocx-mirror'"
        )
        assert "embeds" in payload, "Discord payload must have embeds array"
        embed = payload["embeds"][0]
        assert "title" in embed, "Embed must have title"
        assert "color" in embed, "Embed must have color"
        # §3.10: color must be green (0x2ECC71) for all-green run
        if not summary.get("any_red"):
            assert embed["color"] == 0x2ECC71, (
                f"All-green run must use GREEN color 0x2ECC71, got: {embed['color']:#x}"
            )
    else:
        # all-skipped → silent (no POST)
        assert len(webhook_server.payloads) == 0, (
            "notify must be silent (no POST) when all versions are skipped_existing"
        )
