#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""`bazel:pin:check` — C-003 (pin drift) and C-026 (the Rust toolchain twin).

    scripts/bazel_pin_check.py --check

**C-003.** `bazel_gate_proofs.py` (WP-13) already proved the comparator
(`pin_drift`) red and green on inputs it built by hand; this file supplies the
two live readings WP-13 left for WP-14 to own: `.bazelversion`'s text and
`bazel --version`'s captured stdout/rc, both read here and handed to the
imported comparator unchanged. `ocx.lock` is the pin authority (per-platform
digest); `.bazelversion` is kept and format-checked only (BZL-FLAG-01), so
this gate never reads `ocx.lock` itself — only the two spellings a human or a
`bazelisk` reaches for, `.bazelversion` and the resolved binary.

**C-026.** The same shape, a different pair: `rust-toolchain.toml`'s
`[toolchain].channel` against `MODULE.bazel`'s `rust.toolchain(versions =
[...])`. This project ships binaries, so the two pins are policy-required to
name the same compiler; nothing upstream enforces that, so this is the gate
that would catch it drifting. Two independent reader floors, one per file:
absent, malformed, or missing the field all count as "could not read a
version from this source" rather than as agreement between two `None`s.

Every mutation in the proofs (`scripts/tests/test_bazel_pin_check.py`, run as
pytest) is proven to have landed before its result is trusted. Mutations run
on scratch copies under pytest's own `tmp_path`, never on the real
`.bazelversion`, `rust-toolchain.toml` or `MODULE.bazel`, and nothing is ever
restored with `git checkout --`, which restores from the index and would make
the whole run vacuous.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import tomllib
from pathlib import Path

from bazel_gate_proofs import (
    REPO_ROOT,
    Finding,
    codes,
    pin_drift,
    read_bazelversion,
    report,
)

# ---------------------------------------------------------------------------
# C-026 — the Rust toolchain twin.
# ---------------------------------------------------------------------------

TOOLCHAIN_DRIFT_MSG = (
    "rust toolchain drift: rust-toolchain.toml channel={channel!r}, MODULE.bazel "
    "rust.toolchain(versions=[...]) has {versions} — this project ships binaries, "
    "so both pins must name the same compiler"
)
TOOLCHAIN_FILE_MSG = "could not read a rust channel from rust-toolchain.toml: {reason}"
TOOLCHAIN_MODULE_MSG = "could not read rust.toolchain(versions=[...]) from MODULE.bazel: {reason}"

#: Not full Starlark parsing — a `rust.toolchain(...)` call is a keyword-only
#: function call over a small, fixed vocabulary, and this file's only reader,
#: so a regex extraction is proportionate (quality-core.md "Don't Own
#: Non-Domain Code" rung 3: a few lines, no edge cases this repo's own
#: MODULE.bazel exercises).
RUST_TOOLCHAIN_CALL = re.compile(r"rust\.toolchain\((?P<body>.*?)\)", re.DOTALL)
VERSIONS_LIST = re.compile(r"versions\s*=\s*\[(?P<items>[^\]]*)\]")
QUOTED = re.compile(r'"([^"]+)"')


def read_rust_channel(text: str | None) -> tuple[str | None, Finding | None]:
    """`rust-toolchain.toml`'s `[toolchain].channel`, or a reader-floor Finding."""
    if text is None:
        return None, Finding("toolchain-file", TOOLCHAIN_FILE_MSG.format(reason="<absent>"))
    try:
        parsed = tomllib.loads(text)
    except tomllib.TOMLDecodeError as error:
        return None, Finding("toolchain-file", TOOLCHAIN_FILE_MSG.format(reason=f"invalid TOML: {error}"))
    channel = parsed.get("toolchain", {}).get("channel")
    if not isinstance(channel, str) or not channel:
        return None, Finding(
            "toolchain-file", TOOLCHAIN_FILE_MSG.format(reason="no [toolchain].channel string")
        )
    return channel, None


def read_module_versions(text: str | None) -> tuple[list[str] | None, Finding | None]:
    """`MODULE.bazel`'s `rust.toolchain(versions = [...])`, or a reader-floor Finding."""
    if text is None:
        return None, Finding("toolchain-module", TOOLCHAIN_MODULE_MSG.format(reason="<absent>"))
    call = RUST_TOOLCHAIN_CALL.search(text)
    if call is None:
        return None, Finding(
            "toolchain-module", TOOLCHAIN_MODULE_MSG.format(reason="no rust.toolchain(...) call")
        )
    versions_match = VERSIONS_LIST.search(call.group("body"))
    if versions_match is None:
        return None, Finding(
            "toolchain-module", TOOLCHAIN_MODULE_MSG.format(reason="no versions = [...] in the call")
        )
    versions = QUOTED.findall(versions_match.group("items"))
    if not versions:
        return None, Finding(
            "toolchain-module",
            TOOLCHAIN_MODULE_MSG.format(reason="versions = [...] has no quoted entries"),
        )
    return versions, None


def toolchain_drift(rust_toolchain_text: str | None, module_bazel_text: str | None) -> list[Finding]:
    """C-026, in `pin_drift`'s shape: two independent reader floors, then a compare.

    "Both absent" must produce two loud findings, one per source, never a
    silent agreement between two `None`s — the same rule `pin_drift` enforces
    for `.bazelversion` and the binary.
    """
    findings: list[Finding] = []

    channel, file_finding = read_rust_channel(rust_toolchain_text)
    if file_finding is not None:
        findings.append(file_finding)

    versions, module_finding = read_module_versions(module_bazel_text)
    if module_finding is not None:
        findings.append(module_finding)

    if channel is not None and versions is not None and channel not in versions:
        findings.append(
            Finding("toolchain-drift", TOOLCHAIN_DRIFT_MSG.format(channel=channel, versions=versions))
        )
    return findings


# ---------------------------------------------------------------------------
# C-003 — the binary reading WP-14 owns. The comparator itself is imported.
# ---------------------------------------------------------------------------


def run_bazel_version(bazel_bin: str) -> tuple[str | None, int | None]:
    """`<bazel_bin> --version`'s stdout and return code.

    An unreachable binary (bad `--bazel`, or "bazel" absent from PATH) reads
    as `(None, None)` — `pin_drift` already treats a non-zero/`None` rc as the
    reader floor, so no special case is needed here for "does not exist".
    """
    try:
        result = subprocess.run(
            [bazel_bin, "--version"],
            capture_output=True,
            text=True,
            encoding="utf-8",
            check=False,
            timeout=30,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None, None
    return result.stdout, result.returncode


def check_pin(*, bazelversion_path: Path, bazel_bin: str) -> list[Finding]:
    bazelversion_text = read_bazelversion(bazelversion_path)
    stdout, rc = run_bazel_version(bazel_bin)
    return pin_drift(bazelversion_text, stdout, rc)


def check_toolchain(*, rust_toolchain_path: Path, module_bazel_path: Path) -> list[Finding]:
    # `read_bazelversion` is a generic "text or None on FileNotFoundError"
    # reader despite its name (WP-13's own helper) — reused rather than
    # re-spelled for the other two files, per the same rule this file applies
    # to `pin_drift`.
    return toolchain_drift(
        read_bazelversion(rust_toolchain_path), read_bazelversion(module_bazel_path)
    )


def run_check(
    *,
    bazelversion_path: Path,
    bazel_bin: str,
    rust_toolchain_path: Path,
    module_bazel_path: Path,
) -> list[Finding]:
    return check_pin(bazelversion_path=bazelversion_path, bazel_bin=bazel_bin) + check_toolchain(
        rust_toolchain_path=rust_toolchain_path, module_bazel_path=module_bazel_path
    )


# ---------------------------------------------------------------------------
# Self-test.
# ---------------------------------------------------------------------------


def expect(condition: bool, problem: str) -> None:
    """A loud exit — a bare `assert` vanishes under `python3 -O`."""
    if not condition:
        raise SystemExit(f"bazel pin check self-test: {problem}")


def _write_executable(path: Path, script: str) -> Path:
    path.write_text(script, encoding="utf-8")
    path.chmod(0o755)
    return path


def prove_pin_gate(scratch: Path) -> int:
    """C-003, through the I/O this file owns: a real `.bazelversion`, four fake
    `bazel` binaries covering every reader-floor shape, and the five red
    states the brief requires — every one of them run through `check_pin`,
    never through the imported `pin_drift` directly."""
    checks = 0
    pin = scratch / ".bazelversion"
    ok_bin = _write_executable(scratch / "bazel_ok.sh", "#!/bin/sh\nprintf 'bazel 9.2.0\\n'\nexit 0\n")
    fail_bin = _write_executable(scratch / "bazel_fail.sh", "#!/bin/sh\nexit 1\n")
    noversion_bin = _write_executable(
        scratch / "bazel_noversion.sh", "#!/bin/sh\nprintf 'bazel no_version\\n'\nexit 0\n"
    )
    missing_bin = scratch / "bazel_missing"  # never created — PATH-miss / bad --bazel

    good = "9.2.0\n"
    pin.write_text(good, encoding="utf-8")
    green = check_pin(bazelversion_path=pin, bazel_bin=str(ok_bin))
    expect(green == [], f".bazelversion 9.2.0 vs `bazel 9.2.0` must be silent, got {green}")
    print("C-003 GREEN: .bazelversion 9.2.0 vs `bazel 9.2.0` (fake binary) — no findings")
    checks += 1

    # --- case 1: drift, naming both versions.
    pin.write_text("9.1.0\n", encoding="utf-8")
    expect(pin.read_text(encoding="utf-8").strip() == "9.1.0", "the 9.1.0 mutation did not land")
    drift = check_pin(bazelversion_path=pin, bazel_bin=str(ok_bin))
    expect(codes(drift) == ["pin-drift"], f"expected only pin-drift, got {codes(drift)}")
    expect("9.1.0" in drift[0].message and "9.2.0" in drift[0].message, "the line does not name both")
    print(f"C-003 RED  : {drift[0].message}")
    checks += 1
    pin.write_text(good, encoding="utf-8")
    expect(pin.read_text(encoding="utf-8") == good, "the restore to 9.2.0 did not land")

    # --- case 2: .bazelversion absent — must be an exact three-component semver.
    pin.unlink()
    expect(not pin.exists(), "the deletion of .bazelversion did not land")
    absent_file = check_pin(bazelversion_path=pin, bazel_bin=str(ok_bin))
    expect(codes(absent_file) == ["pin-file"], f"expected only pin-file, got {codes(absent_file)}")
    print(f"C-003 RED  : {absent_file[0].message}")
    checks += 1
    pin.write_text(good, encoding="utf-8")
    expect(pin.read_text(encoding="utf-8") == good, "the restore after deletion did not land")

    # --- case 3: `bazel --version` rc 1, empty stdout.
    rc1 = check_pin(bazelversion_path=pin, bazel_bin=str(fail_bin))
    expect(codes(rc1) == ["pin-reader-floor"], f"expected only pin-reader-floor, got {codes(rc1)}")
    print(f"C-003 RED  : {rc1[0].message} (rc 1, no output)")
    checks += 1

    # --- case 4: rc 0 but unparseable — unparseable is not agreement.
    unparseable = check_pin(bazelversion_path=pin, bazel_bin=str(noversion_bin))
    expect(codes(unparseable) == ["pin-reader-floor"], f"got {codes(unparseable)}")
    print(f"C-003 RED  : {unparseable[0].message} (`bazel no_version`, rc 0)")
    checks += 1

    # --- case 5: both absent — two findings, never a silent agreement.
    pin.unlink()
    expect(not pin.exists(), "the second deletion of .bazelversion did not land")
    both_absent = check_pin(bazelversion_path=pin, bazel_bin=str(missing_bin))
    expect(
        codes(both_absent) == ["pin-file", "pin-reader-floor"],
        f"a run that read zero sources must red on both, got {codes(both_absent)}",
    )
    print(f"C-003 RED  : {[f.message for f in both_absent]} (zero version sources read)")
    checks += 1

    # Restore from our own bytes — never `git checkout --`, which restores
    # from the index and would make every result above unattributable.
    pin.write_text(good, encoding="utf-8")
    expect(pin.read_text(encoding="utf-8") == good, "the final restore did not land")
    expect(
        check_pin(bazelversion_path=pin, bazel_bin=str(ok_bin)) == [],
        "restoring .bazelversion must green again",
    )
    return checks


def prove_toolchain_gate(scratch: Path) -> int:
    """C-026, red-proven one failure mode at a time, on scratch copies. The
    green half is also run against the real, shipped pair — free, since they
    already agree, and it is the case that matters most to not false-positive
    on."""
    checks = 0
    rust_toolchain = scratch / "rust-toolchain.toml"
    module_bazel = scratch / "MODULE.bazel"

    good_toolchain = '[toolchain]\nchannel = "1.95.0"\nprofile = "default"\n'
    good_module = 'rust.toolchain(\n    edition = "2024",\n    versions = ["1.95.0"],\n)\n'
    rust_toolchain.write_text(good_toolchain, encoding="utf-8")
    module_bazel.write_text(good_module, encoding="utf-8")

    green = check_toolchain(rust_toolchain_path=rust_toolchain, module_bazel_path=module_bazel)
    expect(green == [], f"the agreeing pair must be silent, got {[f.message for f in green]}")
    print("C-026 GREEN: rust-toolchain.toml channel 1.95.0 is in MODULE.bazel's versions=[...]")
    checks += 1

    real_channel = REPO_ROOT / "rust-toolchain.toml"
    real_module = REPO_ROOT / "MODULE.bazel"
    if real_channel.is_file() and real_module.is_file():
        real_green = check_toolchain(rust_toolchain_path=real_channel, module_bazel_path=real_module)
        expect(
            real_green == [],
            f"the real, shipped pair must currently agree, got {[f.message for f in real_green]}",
        )
        print("C-026 GREEN: the real rust-toolchain.toml and MODULE.bazel currently agree")
        checks += 1
    else:
        print(f"C-026 SKIP : {real_channel} or {real_module} absent, real-pair agreement not asserted")

    # --- drift: mismatched versions, both named.
    module_bazel.write_text(good_module.replace("1.95.0", "1.94.0"), encoding="utf-8")
    expect("1.94.0" in module_bazel.read_text(encoding="utf-8"), "the 1.94.0 mutation did not land")
    drift = check_toolchain(rust_toolchain_path=rust_toolchain, module_bazel_path=module_bazel)
    expect(codes(drift) == ["toolchain-drift"], f"expected only toolchain-drift, got {codes(drift)}")
    expect(
        "1.95.0" in drift[0].message and "1.94.0" in drift[0].message,
        f"the line does not name both versions: {drift[0].message}",
    )
    print(f"C-026 RED  : {drift[0].message}")
    checks += 1
    module_bazel.write_text(good_module, encoding="utf-8")
    expect(module_bazel.read_text(encoding="utf-8") == good_module, "restore of MODULE.bazel did not land")

    # --- rust-toolchain.toml absent.
    rust_toolchain.unlink()
    expect(not rust_toolchain.exists(), "the deletion of rust-toolchain.toml did not land")
    absent_file = check_toolchain(rust_toolchain_path=rust_toolchain, module_bazel_path=module_bazel)
    expect(codes(absent_file) == ["toolchain-file"], f"got {codes(absent_file)}")
    print(f"C-026 RED  : {absent_file[0].message}")
    checks += 1
    rust_toolchain.write_text(good_toolchain, encoding="utf-8")

    # --- rust-toolchain.toml present, valid TOML, but no [toolchain].channel.
    rust_toolchain.write_text('[toolchain]\nprofile = "default"\n', encoding="utf-8")
    expect("channel" not in rust_toolchain.read_text(encoding="utf-8"), "the channel drop did not land")
    missing_key = check_toolchain(rust_toolchain_path=rust_toolchain, module_bazel_path=module_bazel)
    expect(codes(missing_key) == ["toolchain-file"], f"got {codes(missing_key)}")
    print(f"C-026 RED  : {missing_key[0].message} (no channel key)")
    checks += 1
    rust_toolchain.write_text(good_toolchain, encoding="utf-8")

    # --- rust-toolchain.toml present but not valid TOML at all.
    broken_toml = '[toolchain\nchannel = "1.95.0"\n'
    rust_toolchain.write_text(broken_toml, encoding="utf-8")
    expect(rust_toolchain.read_text(encoding="utf-8") == broken_toml, "the TOML-breaking edit did not land")
    bad_toml = check_toolchain(rust_toolchain_path=rust_toolchain, module_bazel_path=module_bazel)
    expect(codes(bad_toml) == ["toolchain-file"], f"got {codes(bad_toml)}")
    print(f"C-026 RED  : {bad_toml[0].message} (invalid TOML)")
    checks += 1
    rust_toolchain.write_text(good_toolchain, encoding="utf-8")

    # --- MODULE.bazel absent.
    module_bazel.unlink()
    expect(not module_bazel.exists(), "the deletion of MODULE.bazel did not land")
    absent_module = check_toolchain(rust_toolchain_path=rust_toolchain, module_bazel_path=module_bazel)
    expect(codes(absent_module) == ["toolchain-module"], f"got {codes(absent_module)}")
    print(f"C-026 RED  : {absent_module[0].message}")
    checks += 1
    module_bazel.write_text(good_module, encoding="utf-8")

    # --- MODULE.bazel present, but no rust.toolchain(...) call at all.
    no_call_text = 'bazel_dep(name = "rules_rust", version = "0.74.0")\n'
    module_bazel.write_text(no_call_text, encoding="utf-8")
    expect(module_bazel.read_text(encoding="utf-8") == no_call_text, "the call-removal edit did not land")
    no_call = check_toolchain(rust_toolchain_path=rust_toolchain, module_bazel_path=module_bazel)
    expect(codes(no_call) == ["toolchain-module"], f"got {codes(no_call)}")
    print(f"C-026 RED  : {no_call[0].message} (no rust.toolchain(...) call)")
    checks += 1
    module_bazel.write_text(good_module, encoding="utf-8")

    # --- MODULE.bazel has the call, but no versions = [...] inside it.
    no_versions_text = 'rust.toolchain(\n    edition = "2024",\n)\n'
    module_bazel.write_text(no_versions_text, encoding="utf-8")
    expect("versions" not in module_bazel.read_text(encoding="utf-8"), "the versions-drop did not land")
    no_versions = check_toolchain(rust_toolchain_path=rust_toolchain, module_bazel_path=module_bazel)
    expect(codes(no_versions) == ["toolchain-module"], f"got {codes(no_versions)}")
    print(f"C-026 RED  : {no_versions[0].message} (no versions = [...])")
    checks += 1
    module_bazel.write_text(good_module, encoding="utf-8")

    # --- both absent — two findings, never a silent agreement.
    rust_toolchain.unlink()
    module_bazel.unlink()
    both_absent = check_toolchain(rust_toolchain_path=rust_toolchain, module_bazel_path=module_bazel)
    expect(
        codes(both_absent) == ["toolchain-file", "toolchain-module"],
        f"a run that read zero sources must red on both, got {codes(both_absent)}",
    )
    print(f"C-026 RED  : {[f.message for f in both_absent]} (zero version sources read)")
    checks += 1

    rust_toolchain.write_text(good_toolchain, encoding="utf-8")
    module_bazel.write_text(good_module, encoding="utf-8")
    expect(
        check_toolchain(rust_toolchain_path=rust_toolchain, module_bazel_path=module_bazel) == [],
        "restoring both files must green again",
    )
    return checks


def prove_entry_point(scratch: Path) -> int:
    """The shipped `--check` path (`run_check` + `report`), not just the two
    comparators in isolation — a green run and a red run through the same
    function `main()` calls."""
    checks = 0
    pin = scratch / ".bazelversion"
    rust_toolchain = scratch / "rust-toolchain.toml"
    module_bazel = scratch / "MODULE.bazel"
    ok_bin = _write_executable(scratch / "bazel_ok2.sh", "#!/bin/sh\nprintf 'bazel 9.2.0\\n'\nexit 0\n")

    pin.write_text("9.2.0\n", encoding="utf-8")
    rust_toolchain.write_text('[toolchain]\nchannel = "1.95.0"\n', encoding="utf-8")
    module_bazel.write_text('rust.toolchain(\n    versions = ["1.95.0"],\n)\n', encoding="utf-8")
    clean = run_check(
        bazelversion_path=pin,
        bazel_bin=str(ok_bin),
        rust_toolchain_path=rust_toolchain,
        module_bazel_path=module_bazel,
    )
    expect(report(clean) == 0, "run_check + report must exit 0 when every pair agrees")
    print("entry point GREEN: run_check() -> report() exits 0 when pin and toolchain both agree")
    checks += 1

    pin.write_text("9.1.0\n", encoding="utf-8")
    expect(pin.read_text(encoding="utf-8").strip() == "9.1.0", "the entry-point drift mutation did not land")
    dirty = run_check(
        bazelversion_path=pin,
        bazel_bin=str(ok_bin),
        rust_toolchain_path=rust_toolchain,
        module_bazel_path=module_bazel,
    )
    expect(codes(dirty) == ["pin-drift"], f"got {codes(dirty)}")
    expect(report(dirty) == 1, "run_check + report must exit 1 on pin drift")
    print("entry point RED  : run_check() -> report() exits 1 on pin drift, aggregated with C-026")
    checks += 1
    return checks


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument(
        "--check", action="store_true", required=True,
        help="gate the live tree: C-003 pin drift, C-026 toolchain twin",
    )
    parser.add_argument(
        "--bazel",
        default="bazel",
        help="the bazel binary to run --version against (default: resolved from PATH, as the "
        "per-prompt hook and CI's setup-ocx action both leave it)",
    )
    parser.add_argument("--bazelversion", type=Path, default=REPO_ROOT / ".bazelversion")
    parser.add_argument("--rust-toolchain", type=Path, default=REPO_ROOT / "rust-toolchain.toml")
    parser.add_argument("--module-bazel", type=Path, default=REPO_ROOT / "MODULE.bazel")
    args = parser.parse_args()

    findings = run_check(
        bazelversion_path=args.bazelversion,
        bazel_bin=args.bazel,
        rust_toolchain_path=args.rust_toolchain,
        module_bazel_path=args.module_bazel,
    )
    return report(findings)


if __name__ == "__main__":
    raise SystemExit(main())
