# Scripted smoke test for the published ocx.sh/cmake mirror package.
#
# Run via: ocx package test --script tests/smoke.star ...
# cmake ships launcher entrypoints (cmake/ctest/cpack); this exercises the
# launcher resolution path end-to-end.
#
# Deterministic, no network, no external state beyond the package binary.

# 1. The mirrored cmake launcher runs and self-reports a version.
r = ocx.run("cmake", "--version")
expect.ok(r)
expect.true(r.exit_code == 0)
expect.contains(r.stdout, "cmake version ")

# 2. ctest entrypoint resolves through the same launcher mechanism.
c = ocx.run("ctest", "--version")
expect.ok(c)
expect.contains(c.stdout, "ctest version ")

# 3. Host platform is exposed as a dict {"os":..., "arch":...}.
plat = ocx.platform()
expect.true(len(plat["os"]) > 0)
expect.true(len(plat["arch"]) > 0)

# 4. Scratch round-trip: write then read back inside the sandbox.
ocx.write_file("probe.txt", "cmake-smoke-ok")
expect.true(ocx.exists("probe.txt"))
expect.eq(ocx.read_file("probe.txt"), "cmake-smoke-ok")
