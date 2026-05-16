# Scripted smoke test for the published ocx.sh/shfmt mirror package.
#
# Run via: ocx package test --script tests/smoke.star ...
# (the materialized shfmt package env is interpreted by the embedded engine).
#
# Deterministic, no network, no external state beyond the package binary.

# 1. The mirrored binary runs and self-reports a v3 release. That `shfmt`
#    resolves at all proves the composed package PATH (from metadata.json) is
#    in effect — `ocx.env("PATH")` is intentionally None (reserved key, C3).
r = ocx.run("shfmt", "--version")
expect.ok(r)
expect.true(r.exit_code == 0)
expect.contains(r.stdout, "v3")

# 2. Host platform is exposed as a dict {"os":..., "arch":...}.
plat = ocx.platform()
expect.true(len(plat["os"]) > 0)
expect.true(len(plat["arch"]) > 0)

# 3. Scratch round-trip: write then read back inside the sandbox.
ocx.write_file("probe.txt", "shfmt-smoke-ok")
expect.true(ocx.exists("probe.txt"))
expect.eq(ocx.read_file("probe.txt"), "shfmt-smoke-ok")
