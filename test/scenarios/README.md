# `test/scenarios/` — shell-driven acceptance scenarios

Each `.sh` file is a self-contained pytest case. Discovered by
`test/tests/test_scenarios_smoke.py` via glob and run inside the
`Scenario` harness defined in `test/src/scenarios/__init__.py`.

## File header

```bash
#!/usr/bin/env bash
# scenario: <ScenarioClassName>     # optional; omitted = no pre-publish setup
# title: <one-line summary>         # informational
# description: <longer summary>     # informational
set -euo pipefail
…
```

## Available substitutions

The harness sets these environment variables for every script:

| Variable | Value |
|---|---|
| `OCX` | Absolute path to the test `ocx` binary |
| `OCX_HOME` | Per-test isolated `OCX_HOME` |
| `REGISTRY` | Test registry (default `localhost:5000`) |
| `OCX_DEFAULT_REGISTRY`, `OCX_INSECURE_REGISTRIES` | Forwarded from `OcxRunner` |
| `SCENARIO_TMP` | Per-test scratch directory (also `cwd`) |
| `PATH` | The ocx binary's parent prepended; bare `ocx` works |

For each `self.packages[name]` set up by the scenario subclass:

| Variable | Example for `name="hello"` |
|---|---|
| `PKG_<NAME>` | `s_abcd1234_hello:1.0.0` |
| `FQ_<NAME>` | `localhost:5000/s_abcd1234_hello:1.0.0` |
| `REPO_<NAME>` | `s_abcd1234_hello` |
| `TAG_<NAME>` | `1.0.0` |
| `MARKER_<NAME>` | `marker-<random hex>` |

## Adding a new scenario class

1. Create `test/src/scenarios/<topic>.py`.
2. Subclass `Scenario`, set `name = "<ClassName>"`, override `setup()`.
3. Import the module from `test/src/scenarios/__init__.py` (eager import
   block at the bottom) so registration runs at module load.
4. Reference the class from a `.sh` script via `# scenario: <ClassName>`.

## Adding a new script

Drop a `.sh` under `test/scenarios/<topic>/`. No registration needed —
`pytest_generate_tests` discovers it. Run:

```sh
cd test && uv run pytest tests/test_scenarios_smoke.py -v --no-build
```

## Distinction from `test/recordings/scripts/`

`test/recordings/scripts/*.sh` produces `.cast` files for the website
(asciinema). The recording infrastructure asserts on sanitised PTY
output. `test/scenarios/*.sh` produces no artifacts; assertion happens
**inside the script** (assert via `[[ … ]]` + `exit 1`). When a single
shell flow is useful for both, write it twice — different goals, different
constraints (recordings need stable wall-clock pacing, sanitised paths;
scenarios need exit-code assertions).
