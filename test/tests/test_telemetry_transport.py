# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the telemetry transport: a dead collector never changes
a command, nothing secret is printed, and the request carries only what the
design allows.

Specs: system_design_otel_telemetry.md § 3.2, § 6 (the HTTP client, the export,
the audit lines). Each negative result pairs with
a control on the same fixture (the request did arrive, the secret did reach the
wire) so a test cannot pass because the exporter never ran.
"""

from __future__ import annotations

import json
import re
import ssl
import subprocess
import sys
import time
from pathlib import Path

import pytest

from src.forward_proxy import ForwardProxy
from src.otlp_fixture import LOGS_PATH, REDIRECT_TARGET, OtlpCollector, closed_port
from src.runner import OcxRunner, PackageInfo
from src.telemetry_helpers import write_config
from src.tls_index import mint_ca_and_leaf

pytestmark = pytest.mark.command("install", "package_pull")

TIMEOUT_MS = 300
RETRY_WINDOW_MS = 5000  # export timeout under which a retry would still land
EPSILON_S = 1.5  # slack over the export timeout for process and scheduling noise
SECRETS = ("s3cr3t", "t0k3n", "o1", "q5ecr3t")


# Windows reports a refused loopback connect only after about 2 s, past these
# export timeouts, so there the same dead port ends the export as a timeout.
DEAD_PORT_CLASS = "timeout" if sys.platform == "win32" else "connect"


def _armed(endpoint: str, **extra: str) -> dict[str, str]:
    return {
        "OCX_TELEMETRY": "1",
        "OTEL_EXPORTER_OTLP_ENDPOINT": endpoint,
        "OTEL_EXPORTER_OTLP_TIMEOUT": str(TIMEOUT_MS),
        **extra,
    }


def _leaked(text: str, secret: str) -> bool:
    """True when `secret` appears as a token; `o1` must not match inside a hex digest."""
    return (
        re.search(rf"(?<![A-Za-z0-9]){re.escape(secret)}(?![A-Za-z0-9])", text)
        is not None
    )


def _assert_no_secrets(
    result: subprocess.CompletedProcess[str], secrets=SECRETS
) -> None:
    for secret in secrets:
        assert not _leaked(result.stdout, secret), (
            f"{secret} on stdout:\n{result.stdout}"
        )
        assert not _leaked(result.stderr, secret), (
            f"{secret} on stderr:\n{result.stderr}"
        )


def _timed(
    ocx: OcxRunner, *args: str, **kwargs
) -> tuple[subprocess.CompletedProcess[str], float]:
    start = time.monotonic()
    result = ocx.run(*args, **kwargs)
    return result, time.monotonic() - start


# --- dead collector --------------------------------------------------

DEAD_MODES = [
    "closed-port",
    "status=503",
    "hang",
    "delay=3000",
    "redirect",
    "stream-body",
]


@pytest.mark.parametrize(
    "failing", [False, True], ids=["succeeding", "failing-exit-79"]
)
@pytest.mark.parametrize("mode", DEAD_MODES)
def test_dead_collector_never_changes_outcome(
    ocx: OcxRunner,
    published_package: PackageInfo,
    otlp_collector: OtlpCollector,
    mode: str,
    failing: bool,
) -> None:
    """exit code, stdout and (bounded) wall time match the unarmed run of the same command."""
    pkg = published_package
    target = f"{pkg.repo}:9.9.9" if failing else pkg.short
    args = ("package", "install", target)
    ocx.run(
        "package", "pull", pkg.short
    )  # warm the store so both runs do the same work

    unarmed, unarmed_s = _timed(ocx, *args, check=False)
    assert unarmed.returncode == (79 if failing else 0)
    assert json.loads(unarmed.stdout), "baseline run printed no JSON document"

    if mode == "closed-port":
        endpoint = f"http://127.0.0.1:{closed_port()}"
    else:
        otlp_collector.mode = mode
        endpoint = otlp_collector.endpoint
    armed, armed_s = _timed(
        ocx,
        *args,
        check=False,
        env_overrides=_armed(endpoint),
        log_level="debug" if mode == "closed-port" else None,
    )

    assert armed.returncode == unarmed.returncode
    assert armed.stdout == unarmed.stdout
    bound = unarmed_s + TIMEOUT_MS / 1000 + EPSILON_S
    assert armed_s <= bound, (
        f"armed run took {armed_s:.2f}s, bound {bound:.2f}s (mode {mode})"
    )
    if mode == "closed-port":
        # A binary that never exports would also pass the checks above: the debug
        # line proves the export was attempted and failed at the dead port.
        assert f"telemetry: export failed: {DEAD_PORT_CLASS}" in armed.stderr, (
            armed.stderr
        )
    else:
        assert len(otlp_collector.requests) >= 1, (
            "control: the export never reached the collector"
        )


# --- through an HTTP proxy -------------------------------------------

# The proxy is named `localhost`, so the client resolves and pins its name inside
# the deadline. Windows refuses `::1` only after about 2 s and the IPv4 attempt
# starts 300 ms in, so a 300 ms export would end before it reached the proxy.
PROXIED_TIMEOUT_MS = 2000


def _carried(proxy: ForwardProxy, collector: OtlpCollector) -> list[tuple[str, str]]:
    """`(method, absolute URI)` of each request `proxy` took for `collector`."""
    authority = f"127.0.0.1:{collector.port}"
    return [
        (r.method, r.absolute_uri)
        for r in proxy.requests
        if r.forwarded_to == authority
    ]


def test_export_goes_through_the_http_proxy(
    ocx: OcxRunner,
    published_package: PackageInfo,
    otlp_collector: OtlpCollector,
    forward_proxy: ForwardProxy,
) -> None:
    """With `HTTP_PROXY` set, the one export travels through the proxy to the collector."""
    ocx.run(
        "package",
        "install",
        published_package.short,
        env_overrides=_armed(
            otlp_collector.endpoint,
            HTTP_PROXY=forward_proxy.url,
            OTEL_EXPORTER_OTLP_TIMEOUT=str(PROXIED_TIMEOUT_MS),
        ),
    )

    # The collector alone would also pass with the proxy bypassed.
    assert _carried(forward_proxy, otlp_collector) == [
        ("POST", otlp_collector.url(LOGS_PATH))
    ], forward_proxy.requests
    (export,) = otlp_collector.exports
    assert export["records"], "the relayed export carries no record"


def test_stalled_proxy_never_changes_outcome(
    ocx: OcxRunner,
    published_package: PackageInfo,
    otlp_collector: OtlpCollector,
    forward_proxy: ForwardProxy,
) -> None:
    """A proxy that takes the export and never answers costs at most the export timeout.

    Exit code and stdout match the unarmed run under the same proxy.
    """
    pkg = published_package
    args = ("package", "install", pkg.short)
    proxied = {"HTTP_PROXY": forward_proxy.url}
    # Warm the store so both runs do the same work.
    ocx.run("package", "pull", pkg.short)

    unarmed, unarmed_s = _timed(ocx, *args, env_overrides=proxied)
    assert forward_proxy.requests == [], "the warm install must not need the proxy"

    forward_proxy.stalled = True
    armed, armed_s = _timed(
        ocx,
        *args,
        check=False,
        env_overrides=_armed(
            otlp_collector.endpoint,
            **proxied,
            OTEL_EXPORTER_OTLP_TIMEOUT=str(PROXIED_TIMEOUT_MS),
        ),
        log_level="debug",
    )

    assert armed.returncode == unarmed.returncode
    assert armed.stdout == unarmed.stdout
    bound = unarmed_s + PROXIED_TIMEOUT_MS / 1000 + EPSILON_S
    assert armed_s <= bound, f"armed run took {armed_s:.2f}s, bound {bound:.2f}s"
    # Controls: the export reached the proxy and went no further, and ended as a timeout.
    assert _carried(forward_proxy, otlp_collector) == [
        ("POST", otlp_collector.url(LOGS_PATH))
    ], forward_proxy.requests
    assert otlp_collector.requests == []
    assert "telemetry: export failed: timeout" in armed.stderr, armed.stderr


@pytest.mark.parametrize("otlp_collector", ["status=503"], indirect=True)
def test_no_retry_on_503(
    ocx: OcxRunner, published_package: PackageInfo, otlp_collector: OtlpCollector
) -> None:
    """A 503 is final: exactly one request, exit code unchanged.

    The timeout is long enough that a retry after the instant 503 would land
    inside it, so a second request is observable rather than cut off. The 503 is
    instant, so a retry is seen only if it lands within that RETRY_WINDOW_MS (5 s)
    window; a retry delayed past it would go unseen.
    """
    result = ocx.run(
        "package",
        "install",
        published_package.short,
        env_overrides=_armed(
            otlp_collector.endpoint, OTEL_EXPORTER_OTLP_TIMEOUT=str(RETRY_WINDOW_MS)
        ),
        log_level="debug",
    )
    assert result.returncode == 0
    assert len(otlp_collector.requests) == 1
    assert "telemetry: export failed: status 503" in result.stderr, result.stderr


@pytest.mark.parametrize("otlp_collector", ["redirect"], indirect=True)
def test_redirect_is_not_followed(
    ocx: OcxRunner, published_package: PackageInfo, otlp_collector: OtlpCollector
) -> None:
    """A 307 is final: the redirect target receives nothing."""
    ocx.run(
        "package",
        "install",
        published_package.short,
        env_overrides=_armed(otlp_collector.endpoint),
    )
    assert len(otlp_collector.requests_to(LOGS_PATH)) == 1, (
        "control: the original POST must arrive"
    )
    assert otlp_collector.requests_to(REDIRECT_TARGET) == []


# --- secrets ---------------------------------------------------------

_SECRET_CONFIG = (
    "[telemetry]\nenabled = true\nendpoint = {endpoint!r}\n"
    '[telemetry.headers]\nauthorization = "Bearer s3cr3t"\n'
)
_SECRET_ENV = {
    "OCX_TELEMETRY_HEADERS": "x-token=t0k3n",
    "OTEL_EXPORTER_OTLP_HEADERS": "x-otel=o1",
}


@pytest.mark.parametrize("mode", ["ok", "status=503"])
def test_secrets_never_printed(
    ocx: OcxRunner,
    published_package: PackageInfo,
    otlp_collector: OtlpCollector,
    mode: str,
) -> None:
    """header values reach the wire but never stdout or stderr; the debug line names them."""
    otlp_collector.mode = mode
    write_config(ocx, _SECRET_CONFIG.format(endpoint=otlp_collector.endpoint))

    result = ocx.run(
        "package",
        "install",
        published_package.short,
        log_level="trace",
        env_overrides=_SECRET_ENV,
    )

    posts = otlp_collector.log_posts
    assert len(posts) == 1
    assert posts[0].headers["authorization"] == "Bearer s3cr3t", (
        "control: the secret must be in play"
    )
    assert posts[0].headers["x-token"] == "t0k3n"
    _assert_no_secrets(result)

    destination = [
        line for line in result.stderr.splitlines() if "telemetry: destination" in line
    ]
    assert len(destination) == 1, result.stderr
    assert (
        "authorization" in destination[0].lower()
        and "x-token" in destination[0].lower()
    )


def test_secrets_never_printed_for_malformed_config(
    ocx: OcxRunner, published_package: PackageInfo, otlp_collector: OtlpCollector
) -> None:
    """a TOML error names path and position, never the broken line holding the secret."""
    write_config(
        ocx,
        f'[telemetry]\nenabled = true\nendpoint = "{otlp_collector.endpoint}"\n'
        'authorization = "Bearer s3cr3t" broken\n',
    )
    result = ocx.run(
        "package",
        "install",
        published_package.short,
        log_level="trace",
        check=False,
        env_overrides=_SECRET_ENV,
    )
    assert result.returncode == 78, "control: the config must have been rejected"
    assert "config.toml" in result.stdout, "control: the error document names the file"
    _assert_no_secrets(result)


@pytest.mark.parametrize(
    "malformed", ["no-equals-sign-SENTINEL", "good=1,SENTINEL-bad", "bad name=SENTINEL"]
)
def test_malformed_headers_variable_turns_export_off_with_one_warning(
    ocx: OcxRunner,
    published_package: PackageInfo,
    otlp_collector: OtlpCollector,
    malformed: str,
) -> None:
    """a malformed `OCX_TELEMETRY_HEADERS` on an armed run: exit unchanged, one warning, value not echoed."""
    write_config(
        ocx,
        f'[telemetry]\nenabled = true\nendpoint = "{otlp_collector.endpoint}"\n',
    )
    baseline = ocx.run("package", "install", published_package.short)
    assert len(otlp_collector.log_posts) == 1, "control: the unbroken run must send"

    result = ocx.run(
        "package",
        "install",
        published_package.short,
        env_overrides={"OCX_TELEMETRY_HEADERS": malformed},
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout == baseline.stdout
    assert len(otlp_collector.requests) == 1, "a malformed list must not send"
    warnings = [w for w in result.stderr.splitlines() if w.startswith("warning:")]
    assert len(warnings) == 1, result.stderr
    assert "OCX_TELEMETRY_HEADERS" in warnings[0]
    assert "SENTINEL" not in result.stderr


@pytest.mark.parametrize("mode", ["closed-port", "status=503", "refused-uri"])
def test_endpoint_query_string_never_printed(
    ocx: OcxRunner,
    published_package: PackageInfo,
    otlp_collector: OtlpCollector,
    mode: str,
) -> None:
    """an api key in the endpoint's query string never reaches stdout or stderr."""
    if mode == "closed-port":
        base = f"http://127.0.0.1:{closed_port()}"
    elif mode == "refused-uri":
        base = "http://[not-an-authority"
    else:
        otlp_collector.mode = mode
        base = otlp_collector.endpoint
    write_config(
        ocx, f'[telemetry]\nenabled = true\nendpoint = "{base}?api-key=q5ecr3t"\n'
    )

    result = ocx.run("package", "install", published_package.short, log_level="trace")

    assert result.returncode == 0
    _assert_no_secrets(result, ("q5ecr3t",))
    # Control: the export was attempted and failed with the class named, so a
    # binary that never exports cannot pass.
    if mode == "status=503":
        assert len(otlp_collector.requests) == 1, (
            "control: the export reached the collector"
        )
        assert "telemetry: export failed: status 503" in result.stderr, result.stderr
    elif mode == "closed-port":
        assert f"telemetry: export failed: {DEAD_PORT_CLASS}" in result.stderr, (
            result.stderr
        )
    else:
        # An unparsable endpoint is refused at load: no export is attempted, and the
        # warning proves the config was read and ruled out rather than ignored.
        assert "telemetry is off" in result.stderr, result.stderr
        assert "invalid endpoint" in result.stderr, result.stderr


# --- what the request carries ----------------------------------------


def test_ambient_otel_headers_never_reach_an_ocx_destination(
    ocx: OcxRunner, published_package: PackageInfo, otlp_collector: OtlpCollector
) -> None:
    """A `[telemetry].endpoint` destination sends its own headers and none of OTEL_EXPORTER_OTLP_*."""
    write_config(
        ocx,
        f'[telemetry]\nenabled = true\nendpoint = "{otlp_collector.endpoint}"\n'
        '[telemetry.headers]\nx-cfg = "c1"\n',
    )
    ocx.run(
        "package",
        "install",
        published_package.short,
        env_overrides={
            "OTEL_EXPORTER_OTLP_HEADERS": "x-otel=o1",
            "OTEL_EXPORTER_OTLP_LOGS_HEADERS": "x-otel-logs=o2",
        },
    )
    (post,) = otlp_collector.log_posts
    assert post.headers["x-cfg"] == "c1", "control: configured headers are sent"
    assert post.headers["x-otel"] is None
    assert post.headers["x-otel-logs"] is None


def test_logs_endpoint_never_gets_generic_headers(
    ocx: OcxRunner,
    published_package: PackageInfo,
    otlp_collector: OtlpCollector,
) -> None:
    """The logs-specific endpoint takes OTEL_EXPORTER_OTLP_LOGS_HEADERS only; the generic endpoint takes the generic ones."""
    generic = {"OTEL_EXPORTER_OTLP_HEADERS": "x-generic=g1"}
    logs_endpoint = {
        "OCX_TELEMETRY": "1",
        "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT": otlp_collector.url(LOGS_PATH),
    }

    ocx.run(
        "package",
        "install",
        published_package.short,
        env_overrides={**logs_endpoint, **generic},
    )
    ocx.run(
        "package",
        "install",
        published_package.short,
        env_overrides={
            **logs_endpoint,
            **generic,
            "OTEL_EXPORTER_OTLP_LOGS_HEADERS": "x-logs=l1",
        },
    )
    ocx.run(
        "package",
        "install",
        published_package.short,
        env_overrides={
            "OCX_TELEMETRY": "1",
            "OTEL_EXPORTER_OTLP_ENDPOINT": otlp_collector.endpoint,
            **generic,
        },
    )

    first, second, third = otlp_collector.log_posts
    assert first.headers["x-generic"] is None
    assert second.headers["x-generic"] is None
    assert second.headers["x-logs"] == "l1", (
        "control: logs headers are sent to the logs endpoint"
    )
    assert third.headers["x-generic"] == "g1", (
        "control: generic headers go to the generic endpoint"
    )


@pytest.mark.parametrize("source", ["env", "config"])
def test_content_type_cannot_be_overridden(
    ocx: OcxRunner,
    published_package: PackageInfo,
    otlp_collector: OtlpCollector,
    source: str,
) -> None:
    """`content-type` is a refused header name: the selected source is invalid, telemetry is off for the run."""
    env: dict[str, str] = {}
    config = f'[telemetry]\nenabled = true\nendpoint = "{otlp_collector.endpoint}"\n'
    if source == "env":
        env["OCX_TELEMETRY_HEADERS"] = "Content-Type=text/plain"
        named = "OCX_TELEMETRY_HEADERS"
    else:
        config += '[telemetry.headers]\nContent-Type = "text/plain"\n'
        named = "headers"
    write_config(ocx, config)

    result = ocx.run("package", "install", published_package.short, env_overrides=env)

    assert result.returncode == 0
    assert otlp_collector.requests == []
    warnings = [
        line for line in result.stderr.splitlines() if line.startswith("warning:")
    ]
    assert len(warnings) == 1, result.stderr
    assert named in warnings[0]
    assert "text/plain" not in result.stderr
    if source == "config":
        assert str(ocx.ocx_home / "config.toml") in warnings[0]

    # Control: a benign header on the same fixture does send, as protobuf.
    write_config(
        ocx,
        f'[telemetry]\nenabled = true\nendpoint = "{otlp_collector.endpoint}"\n'
        '[telemetry.headers]\nx-ok = "1"\n',
    )
    ocx.run("package", "install", published_package.short)
    (post,) = otlp_collector.log_posts
    assert post.headers["x-ok"] == "1"
    assert post.headers.get_all("content-type") == ["application/x-protobuf"]


def test_ambient_compression_leaves_the_request_uncompressed(
    ocx: OcxRunner, published_package: PackageInfo, otlp_collector: OtlpCollector
) -> None:
    """OTEL_EXPORTER_OTLP_*COMPRESSION=gzip is not honoured: no content-encoding, plain protobuf."""
    ocx.run(
        "package",
        "install",
        published_package.short,
        env_overrides=_armed(
            otlp_collector.endpoint,
            OTEL_EXPORTER_OTLP_COMPRESSION="gzip",
            OTEL_EXPORTER_OTLP_LOGS_COMPRESSION="gzip",
        ),
    )
    (post,) = (
        otlp_collector.log_posts
    )  # the fixture only records a post it decoded as plain protobuf
    assert post.headers["content-encoding"] is None
    assert not post.body.startswith(b"\x1f\x8b"), "body is gzip"


# --- extra CA roots --------------------------------------------------


def _serve_tls(collector: OtlpCollector, tmp_path: Path) -> bytes:
    """Wrap the collector's listening socket in TLS; returns the CA PEM that signs its leaf."""
    pki = mint_ca_and_leaf()
    cert, key = tmp_path / "leaf.pem", tmp_path / "leaf.key"
    cert.write_bytes(pki.leaf_cert_pem)
    key.write_bytes(pki.leaf_key_pem)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(str(cert), str(key))
    server = collector._server
    server.socket = context.wrap_socket(server.socket, server_side=True)
    return pki.ca_cert_pem


def test_collector_behind_extra_ca(
    ocx: OcxRunner,
    published_package: PackageInfo,
    otlp_collector: OtlpCollector,
    tmp_path: Path,
) -> None:
    """An HTTPS collector whose CA is trusted only via OCX_EXTRA_CA_CERTS receives the request."""
    ca_pem = _serve_tls(otlp_collector, tmp_path)
    ca_path = tmp_path / "collector-ca.pem"
    ca_path.write_bytes(ca_pem)
    endpoint = f"https://127.0.0.1:{otlp_collector.port}"

    red = ocx.run(
        "package", "install", published_package.short, env_overrides=_armed(endpoint)
    )
    assert red.returncode == 0
    assert otlp_collector.requests == [], "without the CA the handshake must fail"

    ocx.run(
        "package",
        "install",
        published_package.short,
        env_overrides=_armed(endpoint, OCX_EXTRA_CA_CERTS=str(ca_path)),
    )
    assert len(otlp_collector.log_posts) == 1
