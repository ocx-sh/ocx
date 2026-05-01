# Research: Launcher Exec-Mode Flag Prior Art

**Date:** 2026-04-29
**Driven by:** [`adr_visibility_two_axis_and_exec_modes.md`](./adr_visibility_two_axis_and_exec_modes.md) Tension 5 (`--mode=consumer|self|full`) and §521 (launcher byte-format)
**Companion:** [`research_launcher_patterns.md`](./research_launcher_patterns.md) (prior OCX launcher research)

## Question

ADR Tension 5 introduces `ocx exec --mode=consumer|self|full` and proposes embedding `--mode=self` in the generated launcher body for both Unix `.sh` and Windows `.cmd`. Vocabulary check + format-convention check before locking the byte-exact format into tests.

Phase 1 confirmed current launcher format strings:
- Unix (`crates/ocx_lib/src/package_manager/entrypoints.rs:153-157`)
- Windows (`:184-188`)

Both lack `--mode=self`. Both have byte-exact tests at lines 348/364.

## Industry Context and Trends

- **Established:** `--strategy=enum-value` flag pattern (Bazel `--spawn_strategy`). Single verb, multiple modes via flag.
- **Established:** Install-time-baked launcher contents (npm `cmd-shim`, Nix `makeWrapper`). Reproducible, test-friendly.
- **Distinct architecture:** mise/asdf shims are interceptors with no self-vs-consumer distinction — does not apply to OCX where the launcher *is* the package.
- **Hard rule from prior OCX launcher research:** No `set -e` in single-exec wrappers — `exec` propagates exit codes naturally; `set -e` adds noise without benefit.

## Per-Tool Findings

### CMake `--mode=` Vocabulary Check

CMake's CLI has **no `--mode=` flag** anywhere in its surface. Operational modes use **positional prefix flags** that each open a distinct subcommand:

- `cmake --build <dir>` — build mode
- `cmake --install <dir>` — install mode
- `cmake -E <cmd>` — utility command mode (positional, not `--mode=<cmd>`)
- `cmake -P <script>` — script mode
- `cmake --workflow <options>` — workflow mode

**Conflict assessment:** Zero. OCX's `--mode=consumer|self|full` does not collide with any CMake flag. CMake uses `PRIVATE|PUBLIC|INTERFACE` only as property scoping in CMakeLists, not as exec-mode terminology.

Source: [CMake CLI reference](https://cmake.org/cmake/help/latest/manual/cmake.1.html)

### Cargo Subcommand vs Flag Design

Cargo's subcommand-per-verb architecture (`cargo build`, `cargo test`, `cargo run`) is driven by **extensibility**, not ergonomics: external-tool lookup (`cargo-${command}` binary in PATH) requires the verb to be a positional subcommand. Built-ins and third-party tools are indistinguishable at the shell layer.

**Implication for OCX:** ADR's Option C (two verbs: `ocx run` + `ocx exec`) was correctly rejected. Cargo splits verbs because they represent fundamentally different pipelines (compile vs execute vs test). OCX's consumer/self/full distinction is **the same verb** with different env-filter semantics — single flag is the right shape. Cargo would not split `cargo build --release` and `cargo build --debug` into separate verbs; it uses a flag.

Source: [Cargo external tools](https://doc.rust-lang.org/cargo/reference/external-tools.html)

### Bazel `--config` and Exec-Strategy Patterns

Bazel uses `--spawn_strategy=local|sandboxed|remote|worker|docker` — precisely a `--strategy=value` enum pattern. Vocabulary axis: execution location/method (where the action runs), not caller-identity. Bazel does not have a `consumer|self|full` concept.

`--config=<name>` groups flag sets defined in `.bazelrc` — named profile, not inline enum. Would correspond to OCX having named exec profiles, not the current `--mode` design.

**Takeaway:** Bazel confirms `--strategy-enum-flag` is a legitimate, widely-used CLI pattern. OCX's `--mode=<enum>` follows the same shape. No vocabulary collision.

Source: [Bazel execution strategy](https://bazel.build/docs/user-manual#execution-strategy)

### mise/asdf Shim Conventions — No Mode Distinction

**mise:** Shims are **symlinks to the `mise` binary itself**. When invoked, `mise` reads `argv[0]` (the shim name) to identify the target tool. **No mode parameter** embedded in the shim — `mise` always re-invokes itself in one unified execution mode. No self-vs-consumer split exists because mise has no concept of "the shim itself being the package."

**asdf:** Shims are POSIX shell scripts that call `exec asdf exec <tool> "$@"`:
```sh
#!/usr/bin/env bash
# asdf-plugin: <tool-name>
# asdf-plugin-version: <version>
exec ~/.asdf/bin/asdf exec <tool> "$@"
```
**No mode parameter**, one unified dispatcher, tool identity baked as a comment.

**Key finding:** Neither mise nor asdf has a self-vs-consumer mode distinction. OCX's launcher embedding `--mode=self` is **a novel design** with no direct prior art in the shim ecosystem. This is appropriate — OCX launchers are not interceptors; they are one-shot exec wrappers that know they ARE the package being invoked.

Sources: [mise shims](https://mise.jdx.dev/dev-tools/shims.html), [asdf shim generation](https://deepwiki.com/asdf-vm/asdf/6.1-shim-generation)

### Launcher Byte-Format Conventions

| Ecosystem | Install-time baked? | What's baked | Runtime computed |
|---|---|---|---|
| pip `console_scripts` | No | Nothing — shebang points to venv Python | Module name, function name resolved by Python import |
| asdf shims | Partial | Tool name as comment; `asdf exec` is fixed target | Version, real path resolved by asdf |
| mise shims | Partial | Symlink to `mise`; argv[0] is baked by filename | Version, env, real path resolved by mise |
| npm `cmd-shim` | Yes | Absolute path to Node.js + script | Nothing |
| Nix `makeWrapper` | Yes | Entire env (`export VAR=val`) baked as shell statements | Nothing |
| OCX (current) | Yes | `file://{pkg_root}` URI | Nothing |
| OCX (proposed) | Yes | `--mode=self 'file://{pkg_root}'` | Nothing |

**Install-time-fixed args vs runtime-computed:** ADR proposes baking `--mode=self`. Consistent with `npm cmd-shim` and Nix `makeWrapper` (mature, test-friendly, reproducible). The `pip console_scripts` runtime-resolution approach is used where the target changes (venv upgrade, editable installs) — OCX packages are content-addressed and never move; runtime resolution adds only latency and fragility.

**Trade-off confirmed:** Baking `--mode=self` means a future flag rename invalidates old launchers. ADR classifies `--mode` as Two-Way reversible (additive flag mechanism); flag value vocabulary is stable. Break risk is low; testability benefit is high. **Bake it.**

**`set -euf` consideration:** Prior OCX launcher research recommends against `set -e` — `exec` never returns on success and exit code propagates naturally. `set -u` would catch unset variable bugs (none exist — `pkg_root` is always substituted at generation time). `set -f` disables glob expansion (no globs in body). Adding `set -euf` is unnecessary noise; current minimal `#!/bin/sh` + `exec` pattern is correct.

Sources: [Python entry points spec](https://packaging.python.org/en/latest/specifications/entry-points/), [pip distlib scripts.py](https://github.com/pypa/pip/blob/main/src/pip/_vendor/distlib/scripts.py)

### Vocabulary Final-Check: `consumer|self|full`

**"Consumer" collision survey:**
- Kafka, RabbitMQ, gRPC: heavy use as message-queue subscriber/reader.
- CLI ecosystems: rare — not used by Cargo, CMake, Bazel, mise, or asdf in flag vocabulary.
- OCX's own docs: "consumer" appears in the dependency-vocabulary context (a package consuming another's env). Consistent and reinforcing.
- Grep-noise risk: low within OCX docs.

**Alternatives considered:**
- `caller|self|full` — clearer but less idiomatic vs CMake parallel.
- `transitive|self|debug` — implies graph traversal, not exec identity.
- `external|internal|full` — risks confusion with package visibility (public/private already covers that axis).
- `user|self|full` — vague (conflicts with "end user" product context).

**Verdict:** `consumer|self|full` holds. "Consumer" is the right word for "entity that uses this package's published contract" — CMake `INTERFACE` mental model maps exactly. No rename needed.

## Recommendation for OCX

**Confirm the ADR's launcher format as-is.** No changes needed:

Unix:
```sh
#!/bin/sh
# Generated by ocx at install time. Do not edit.
exec ocx exec --mode=self 'file:///<pkg-root>' -- "$(basename "$0")" "$@"
```

Windows:
```bat
@ECHO off
SETLOCAL DisableDelayedExpansion
ocx exec --mode=self "file://<pkg-root>" -- "%~n0" %*
```

- `--mode=self` before package ref — correct per Clap convention (flags before positional, matches OCX auto-memory `feedback_flags_before_args`); tested against Cargo/Bazel prior art.
- `consumer|self|full` vocabulary — no conflict found; endorses CMake mental model.
- Byte-embedding `--mode=self` — consistent with npm `cmd-shim` and Nix `makeWrapper`. Test-friendly, reproducible.
- No `set -euf` — `exec` propagates exit codes naturally; `set -e` in single-exec wrapper is noise.
- "consumer" — not overloaded in OCX's CLI ecosystem context.

## Sources

- [CMake CLI reference](https://cmake.org/cmake/help/latest/manual/cmake.1.html)
- [Cargo external tools](https://doc.rust-lang.org/cargo/reference/external-tools.html)
- [Bazel execution strategy](https://bazel.build/docs/user-manual#execution-strategy)
- [mise shims](https://mise.jdx.dev/dev-tools/shims.html)
- [asdf shim generation](https://deepwiki.com/asdf-vm/asdf/6.1-shim-generation)
- [Python entry points spec](https://packaging.python.org/en/latest/specifications/entry-points/)
- [pip distlib scripts.py](https://github.com/pypa/pip/blob/main/src/pip/_vendor/distlib/scripts.py)
- [`research_launcher_patterns.md`](./research_launcher_patterns.md) — prior OCX research
