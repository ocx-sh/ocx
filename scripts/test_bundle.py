#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Cross-built unit tests as a bundle that a native runner executes without a toolchain.

Linux builds (`bundle`, `bin`); Windows and macOS only unpack and execute (`run`, stdlib-only, Python 3.8+).

  test_bundle.py bundle --target <triple> [--out PATH] [--cache-rc ARG] [--with-ocx] [--with-test-bin]
  test_bundle.py run    --bundle PATH [--source-root DIR] [--junit PATH] [--timeout SECS] [--filter SUBSTR]
  test_bundle.py bin    --target <triple> [--out DIR] [--cache-rc ARG]

`--cache-rc` is the root taskfile's `CACHE_RC`; `--with-*` put `cross:build`'s and `bin`'s outputs in one `bazel build`.
"""

from __future__ import annotations

import argparse
import io
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import xml.etree.ElementTree as ET
from pathlib import Path
from typing import Any, TypedDict


class Entry(TypedDict):
    """One bundle entry, as `cmd_bundle` writes it to `manifest.json`."""

    label: str
    dir: str
    exe: str
    args: list[str]
    env: dict[str, str]
    skips: list[str]


class Result(TypedDict, total=False):
    label: str
    cases: list[tuple[str, str]]
    passed: int
    failed: int
    ignored: int
    ok: bool
    log: str
    secs: float


ROOT = Path(__file__).resolve().parent.parent
BAZEL = ["ocx", "exec", "bazel", "--", "bazel"]
# Bazel's default test toolchain pins the exec platform to the target platform,
# which no cross target has; the flag lets a foreign-target test rule analyse.
NO_TEST_TOOLCHAIN = (
    "--@bazel_tools//tools/test:incompatible_use_default_test_toolchain=false"
)
# label -> triple fragment of the one platform where the target runs cases. On any
# other platform its `#![cfg]` leaves it empty by design; there, and only there,
# a 0-case summary is not an under-run.
CFG_GATED = {"//crates/ocx_cli:macos_self_contained": "-apple-darwin"}
SKIP = "--skip="
# Skipped testcases that shell out to `cargo metadata`/`cargo tree`. They read
# the workspace, not the platform, and need its submodules, which a native
# runner's checkout lacks; Linux runs them (`rust:test:workspace-cargo`).
# `crates/ocx_test_support/BUILD.bazel` `_NEEDS_CARGO` names the same set.
NEEDS_CARGO = {
    "anyhow_only_in_cli_runtime",
    "clap_env_feature_is_off_in_the_release_build",
    "crate_map_toml_matches_rust_table",
    "deps_direction",
    "release_feature_set_excludes_testing_seams",
}
LIBTEST_CASE = re.compile(r"^test (.+) \.\.\. (ok|FAILED|ignored)\b", re.MULTILINE)
LIBTEST_RESULT = re.compile(
    r"^test result: .*?(\d+) passed; (\d+) failed; (\d+) ignored", re.MULTILINE
)


def bazel(*args: str, rc: str = "", capture: bool = False) -> str:
    cmd = [*BAZEL, *([rc] if rc else []), *args]
    res = subprocess.run(
        cmd, cwd=ROOT, capture_output=capture, text=True, encoding="utf-8", check=False
    )
    if res.returncode != 0:
        sys.exit(
            f"{' '.join(cmd[:8])} ... failed ({res.returncode})\n{res.stderr if capture else ''}"
        )
    return res.stdout


def flags(target: str) -> list[str]:
    return [
        "-c",
        "opt",
        f"--platforms=@rules_rs//rs/platforms:{target}",
        NO_TEST_TOOLCHAIN,
    ]


def exe_name(name: str, target: str) -> str:
    return name + (".exe" if "-windows-" in target else "")


def cquery(expr: str, target: str, *output: str, rc: str = "") -> str:
    return bazel("cquery", expr, *flags(target), *output, rc=rc, capture=True)


# ---------------------------------------------------------------- bundle


def rust_test_labels(query_out: str) -> list[str]:
    """The candidate labels, refused when fewer than the tree's `rust_test` count.

    A query that stopped matching would otherwise bundle a smaller set and still
    agree with its own `candidates` count.
    """
    # Imported here, not at the top: `run` must stay stdlib-only.
    from bazel_gate_proofs import CRATES_TEST_TARGETS

    labels = query_out.split()
    if len(labels) < CRATES_TEST_TARGETS:
        sys.exit(f"query found {len(labels)} rust_test targets, the tree has at least {CRATES_TEST_TARGETS}")
    return labels


def cmd_bundle(a: argparse.Namespace) -> None:
    out = Path(a.out or ROOT / "target/cross" / a.target / "unit-tests.tar.gz")
    rc = a.cache_rc
    # Plain `query`, not `cquery`: cquery analyses the whole `//crates/...`
    # universe, doctests included, and those cannot resolve a foreign target.
    labels = rust_test_labels(bazel("query", 'kind("^rust_test", //crates/...)', rc=rc, capture=True))
    expr = "set(" + " ".join(labels) + ")"
    # What `cross:build` and `bin` stage rides in the same invocation: ocx and the test-bin set.
    ocx = [(OCX, exe_name("ocx", a.target))] if a.with_ocx else []
    test_bin = bin_layout(a.target) if a.with_test_bin else []
    extra = list(dict.fromkeys(label for label, _ in [*ocx, *test_bin]))
    with tempfile.NamedTemporaryFile("w", suffix=".txt", delete=False) as pf:
        pf.write("\n".join([*labels, *extra]))
    try:
        bazel("build", "--jobs=8", *flags(a.target), f"--target_pattern_file={pf.name}", rc=rc)
    finally:
        os.unlink(pf.name)
    cross_dir = ROOT / "target/cross" / a.target
    stage(ocx, cross_dir, a.target, rc)
    stage(test_bin, cross_dir / "test-bin", a.target, rc)
    # Attributes (args/env) resolved for this configuration, plus the executable.
    attrs: dict[str, dict[str, Any]] = {}
    for r in json.loads(cquery(expr, a.target, "--output=jsonproto", rc=rc))["results"]:
        rule = r["target"]["rule"]
        at = {x["name"]: x for x in rule["attribute"] if x.get("explicitlySpecified")}
        attrs[rule["name"]] = {
            "args": at.get("args", {}).get("stringListValue", []),
            "env": {
                e["key"]: e["value"]
                for e in at.get("env", {}).get("stringDictValue", [])
            },
        }
    star = (
        'str(target.label)+"|"+(providers(target)["FilesToRunProvider"].executable.path'
        ' if "FilesToRunProvider" in providers(target) else "")'
    )
    execroot = Path(bazel("info", "execution_root", rc=rc, capture=True).strip())
    tests: list[tuple[str, Path, Path, dict[str, Any]]] = []
    for line in cquery(
        expr, a.target, "--output=starlark", f"--starlark:expr={star}", rc=rc
    ).splitlines():
        label, _, exe = line.partition("|")
        label = label.lstrip("@")
        if not exe:  # target_compatible_with dropped it: a silent under-run, so refuse
            sys.exit(f"{label} is incompatible with {a.target}; the bundle would omit it")
        exe = Path(exe)
        # `bazel-out/<cfg>/bin/<short_path>`; the runfiles tree sits beside it.
        short = Path(*exe.parts[3:])
        runfiles = execroot / (str(exe) + ".runfiles")
        if not (runfiles / "_main" / short).is_file():
            sys.exit(
                f"{label}: runfiles tree {runfiles} lacks _main/{short.as_posix()}"
            )
        tests.append((label, short, runfiles, attrs[label]))
    if not tests:
        sys.exit("every rust_test is incompatible with " + a.target)
    manifest: dict[str, Any] = {"target": a.target, "candidates": len(labels), "tests": []}
    out.parent.mkdir(parents=True, exist_ok=True)
    with tarfile.open(out, "w:gz", compresslevel=6, dereference=True) as tar:
        for label, short, runfiles, at in tests:
            slot = re.sub(r"[^A-Za-z0-9_.-]+", "__", label)
            tar.add(runfiles / "_main", arcname=f"{slot}/_main")
            manifest["tests"].append(
                {
                    "label": label,
                    "dir": slot,
                    "exe": short.as_posix(),
                    "args": at["args"],
                    "env": at["env"],
                    "skips": [x[len(SKIP) :] for x in at["args"] if x.startswith(SKIP)],
                }
            )
        data = json.dumps(manifest, indent=1).encode()
        info = tarfile.TarInfo("manifest.json")
        info.size = len(data)
        info.mtime = int(time.time())
        tar.addfile(info, io.BytesIO(data))
    print(f"{len(tests)} tests, {out.stat().st_size / 2**20:.0f} MiB: {out}")


# ---------------------------------------------------------------- run


def run_one(
    t: Entry,
    root: Path,
    timeout: int,
    args: list[str] | None = None,
    cwd: Path | None = None,
    label: str | None = None,
) -> Result:
    base = root / t["dir"]
    main = base / "_main"
    exe = main / t["exe"]
    res: Result = {
        "label": label or t["label"],
        "cases": [],
        "passed": 0,
        "failed": 0,
        "ignored": 0,
        "ok": False,
        "log": "",
    }
    if not exe.is_file():
        res["log"] = f"missing executable {exe}"
        return res
    tmp = base / "tmp"
    tmp.mkdir(exist_ok=True)
    env = dict(os.environ)
    env.update(
        TEST_SRCDIR=str(base),
        RUNFILES_DIR=str(base),
        TEST_WORKSPACE="_main",
        TEST_TMPDIR=str(tmp),
        TEST_TARGET=t["label"],
    )
    env.update(t["env"])
    start = time.monotonic()
    try:
        p = subprocess.run(
            [str(exe), *(t["args"] if args is None else args)],
            cwd=cwd or main,
            env=env,
            timeout=timeout,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            encoding="utf-8",
            errors="replace",
            check=False,
        )
        out, code = p.stdout, p.returncode
    except OSError as e:  # unrunnable executable (corrupt, wrong platform)
        out, code = f"cannot execute {exe}: {e}", -1
    except subprocess.TimeoutExpired as e:
        out = (
            e.stdout.decode("utf-8", "replace")
            if isinstance(e.stdout, bytes)
            else e.stdout
        ) or ""
        out, code = out + f"\nTIMEOUT after {timeout}s", -1
    res["secs"] = time.monotonic() - start
    res["log"] = out
    res["cases"] = LIBTEST_CASE.findall(out)
    m = LIBTEST_RESULT.findall(out)
    if m:  # one summary line per libtest binary; a target runs exactly one
        res["passed"], res["failed"], res["ignored"] = (
            sum(int(x[i]) for x in m) for i in range(3)
        )
    # No `test result:` line means the harness never finished (crash, timeout).
    res["ok"] = code == 0 and bool(m) and res["failed"] == 0
    return res


def run_skipped(t: Entry, root: Path, timeout: int, source_root: Path) -> list[Result]:
    """Each `--skip`ped testcase on its own, as the `<crate>_isolated` targets run it on Linux.

    The cwd is the checkout, so the `crates/*/src` walks that `CARGO_MANIFEST_DIR`
    names relatively resolve. A skip that matches no testcase runs 0 and reds,
    the refusal `run_each_case.sh` makes too.
    """
    rest = [x for x in t["args"] if x != "--exact" and not x.startswith(SKIP)]
    out = []
    for name in dict.fromkeys(t["skips"]):
        if name in NEEDS_CARGO:
            print(f"not run (reads the workspace): {t['label']} [skipped] {name}", flush=True)
            continue
        r = run_one(
            t,
            root,
            timeout,
            args=["--exact", name, *rest],
            cwd=source_root,
            label=f"{t['label']} [skipped] {name}",
        )
        r["ok"] = r["ok"] and r["passed"] >= 1
        out.append(r)
    return out


def write_junit(results: list[Result], path: Path) -> None:
    suites = ET.Element("testsuites")
    for r in results:
        s = ET.SubElement(
            suites, "testsuite", name=r["label"], tests=str(max(len(r["cases"]), 1))
        )
        for name, status in r["cases"]:
            tc = ET.SubElement(s, "testcase", classname=r["label"], name=name)
            if status == "FAILED":
                ET.SubElement(tc, "failure", message="FAILED").text = r["log"][-8000:]
            elif status == "ignored":
                ET.SubElement(tc, "skipped")
        if not r["cases"] or (not r["ok"] and not r["failed"]):
            tc = ET.SubElement(s, "testcase", classname=r["label"], name="<target>")
            if not r["ok"]:
                ET.SubElement(
                    tc, "failure", message="target did not complete"
                ).text = r["log"][-8000:]
    path.parent.mkdir(parents=True, exist_ok=True)
    ET.ElementTree(suites).write(path, encoding="utf-8", xml_declaration=True)


def report(r: Result) -> None:
    print(
        f"{'PASS' if r['ok'] else 'FAIL'} {r['label']}  passed={r['passed']} failed={r['failed']} "
        f"ignored={r['ignored']}  {r.get('secs', 0):.1f}s",
        flush=True,
    )
    if not r["ok"]:
        print("\n".join("    " + l for l in r["log"].splitlines()[-40:]))


def totals(results: list[Result]) -> tuple[int, ...]:
    return tuple(sum(r[k] for r in results) for k in ("passed", "failed", "ignored"))


def cmd_run(a: argparse.Namespace) -> None:
    junit = Path(a.junit or ROOT / "target/cross/unit-junit.xml")
    source_root = Path(a.source_root).resolve() if a.source_root else None
    problems = []
    with tempfile.TemporaryDirectory(prefix="ocx-unit-") as td:
        root = Path(td)
        with tarfile.open(a.bundle) as tar:
            if hasattr(tarfile, "data_filter"):
                tar.extractall(root, filter="data")
            else:
                tar.extractall(root)
        manifest = json.loads((root / "manifest.json").read_text(encoding="utf-8"))
        tests = [t for t in manifest["tests"] if a.filter in t["label"]]
        if not tests:
            sys.exit(
                "bundle lists no test" + (f" matching {a.filter!r}" if a.filter else "")
            )
        if not a.filter and manifest.get("candidates") != len(manifest["tests"]):
            problems.append(
                f"manifest lists {len(manifest['tests'])} tests for {manifest.get('candidates')} candidates"
            )
        results, skipped = [], []
        for t in tests:
            r = run_one(t, root, a.timeout)
            # A libtest summary with 0 cases is a target that ran nothing.
            gate = CFG_GATED.get(t["label"])
            empty_ok = gate is not None and gate not in manifest["target"]
            if r["ok"] and r["passed"] + r["failed"] == 0 and not empty_ok:
                r["ok"], r["log"] = False, r["log"] + "\nthe target ran 0 cases"
            results.append(r)
            report(r)
            if source_root:
                for sr in run_skipped(t, root, a.timeout, source_root):
                    skipped.append(sr)
                    if not sr["ok"]:
                        report(sr)
        write_junit(results + skipped, junit)
    ran = sum(1 for r in results if r["ok"])
    p, f, i = totals(results)
    print(
        f"{ran}/{len(tests)} targets ok; {p + f} cases ({p} passed, {f} failed, {i} ignored); junit: {junit}"
    )
    if source_root:
        sp, sf, _ = totals(skipped)
        print(f"skipped pass: {len(skipped)} skipped testcases run ({sp} passed, {sf} failed) in {source_root}")
    else:
        print("skipped pass: NOT run (no --source-root); the --skip'd testcases ran nowhere")
    problems += [r["label"] for r in results + skipped if not r["ok"]]
    if p + f == 0:
        problems.append("no case ran")
    if problems:
        sys.exit("FAILED: " + ", ".join(problems))


# ---------------------------------------------------------------- bin


OCX = "//crates/ocx_cli:ocx"


def bin_layout(target: str) -> list[tuple[str, str]]:
    """The acceptance suite's test/bin layout, mirroring test/taskfile.yml `.build-binaries`."""
    return [
        (OCX, exe_name("ocx", target)),
        ("//crates/ocx_shim:ocx_shim", exe_name("ocx-shim", target)),
        (
            "//crates/ocx_schema:ocx_schema_bin",
            "schema/" + exe_name("ocx_schema", target),
        ),
    ]


def stage(wanted: list[tuple[str, str]], out: Path, target: str, rc: str) -> None:
    """Copy each already-built label to `out/<rel>`, executable."""
    for label, rel in wanted:
        found = cquery(label, target, "--output=files", rc=rc).strip().splitlines()
        if not found:
            sys.exit(f"{label} produced no file for {target}")
        src = found[-1]
        dst = out / rel
        dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ROOT / src if not Path(src).is_absolute() else src, dst)
        dst.chmod(0o755)
        print(dst)


def cmd_bin(a: argparse.Namespace) -> None:
    out = Path(a.out or ROOT / "target/cross" / a.target / "test-bin")
    wanted = bin_layout(a.target)
    f = flags(a.target)[:3]  # no test toolchain flag: these are not test rules
    bazel("build", "--jobs=8", *f, *[w[0] for w in wanted], rc=a.cache_rc)
    stage(wanted, out, a.target, a.cache_rc)


def main() -> None:
    p = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    sub = p.add_subparsers(dest="cmd", required=True)
    b = sub.add_parser("bundle")
    b.add_argument("--target", required=True)
    b.add_argument("--out")
    b.add_argument("--cache-rc", default="", help="the task's CACHE_RC: Bazel's first startup option, or empty")
    b.add_argument("--with-ocx", action="store_true", help="also build and stage ocx beside the bundle (what `cross:build` stages)")
    b.add_argument("--with-test-bin", action="store_true", help="also build and stage test-bin/ beside the bundle (what `bin` stages)")
    r = sub.add_parser("run")
    r.add_argument("--bundle", required=True)
    r.add_argument("--source-root", help="checkout to run the --skip'd testcases in")
    r.add_argument("--junit")
    r.add_argument("--timeout", type=int, default=900)
    r.add_argument("--filter", default="")
    n = sub.add_parser("bin")
    n.add_argument("--target", required=True)
    n.add_argument("--out")
    n.add_argument("--cache-rc", default="", help="the task's CACHE_RC: Bazel's first startup option, or empty")
    a = p.parse_args()
    {"bundle": cmd_bundle, "run": cmd_run, "bin": cmd_bin}[a.cmd](a)


if __name__ == "__main__":
    main()
