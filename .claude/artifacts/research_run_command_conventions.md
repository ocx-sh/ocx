---
topic: project-runner CLI conventions
sources:
  - https://docs.npmjs.com/cli/v11/commands/npm-run-script
  - https://pnpm.io/cli/run
  - https://pnpm.io/filtering
  - https://yarnpkg.com/cli/run
  - https://yarnpkg.com/features/scripting
  - https://doc.rust-lang.org/cargo/commands/cargo-run.html
  - https://just.systems/man/en/recipe-parameters.html
  - https://just.systems/man/en/listing-available-recipes.html
  - https://taskfile.dev/usage/
  - https://mise.jdx.dev/cli/exec.html
  - https://mise.jdx.dev/cli/run.html
  - https://nix.dev/manual/nix/2.18/command-ref/new-cli/nix3-run
  - https://nix.dev/manual/nix/2.26/command-ref/new-cli/nix3-develop
  - https://nix.dev/manual/nix/2.18/command-ref/new-cli/nix3-shell
  - https://direnv.net/man/direnv.1.html
  - https://devenv.sh/scripts/
  - https://docs.astral.sh/uv/concepts/tools/
  - https://bazel.build/docs/user-manual
date: 2026-05-07
---

# Research: Project-Runner CLI Conventions

Informs design of `ocx run [-g GROUP[,GROUP...]] [NAME...] -- ARGV...` for the OCX project toolchain.

## 1. Tool-by-Tool Summaries

**npm run** — `npm run <script> [-- ARGV]`. Script name is the first positional; `--` is mandatory to forward flags to the subprocess (without it, flags are consumed by npm). Missing script errors with a list of available scripts. Exit code forwarded; known workspace bugs where some npm versions return 1 instead of the child's code.

**pnpm run** — `pnpm run <script> [args]`, optionally prefixed with `--filter <selector>` (repeatable, supports package-name globs like `*`). Args follow the script name without `--`; `--if-present` suppresses exit 1 when the script doesn't exist. `*` is a glob matcher, not a reserved name. Exit code forwarded.

**yarn run (v4)** — `yarn run <script> [args]` or `yarn <script>`. Args appended automatically to the script's command, or referenced via `$@` in the script body. `-T` (top-level workspace) and `-B` (binaries-only) scope flags. No `--` documented as separator. Cross-workspace run via `yarn workspaces foreach --include <pattern>`.

**cargo run** — `cargo run [--bin NAME] [--features F] [--profile P] [-- ARGV]`. `--` is mandatory; without it, args are consumed by Cargo. Multiple binaries require `--bin`; `default-run` in Cargo.toml declares a default. Error (exit 101) when ambiguous. Subprocess exit code forwarded intact.

**just** — `just [recipe] [args...]`. No `--` separator; args are positional parameters in the recipe body. First recipe or explicit `default` recipe runs when no name given; common idiom: `default: @just --list`. Variadic `+`/`*` params allow zero-or-more forwarding. No reserved keyword names documented.

**task** — `task [name] [VAR=val...] [-- CLI_ARGS]`. `--` populates special `.CLI_ARGS` variable. `default` task runs with no name. `internal: true` hides a task from `--list`. No names beyond `default` are reserved.

**mise exec / mise run** — `mise exec [TOOL@VERSION...] [-- COMMAND ARGV]`. `--` mandatory. Tools load from `mise.toml` by default; CLI args add overrides. Inherits environment by default; `--deny-env` for a near-clean env. No group concept; profiles use `--env` flag selecting `mise.<env>.toml`. `mise run` is the task-runner side; not the env-exec side.

**nix run / nix develop -c / nix shell** — `nix run <installable> -- ARGV` (positional after installable); `nix develop [flake] --command CMD ARGS` (flag-based, `-c`); `nix shell <pkgs> --command CMD ARGS` (same). Attribute selection is implicit (system-keyed defaults searched) or explicit via flake fragment. Environment is Nix-derived, not inherited PATH; `--ignore-environment` for fully clean. `nix run` is the most common single-shot exec idiom.

**direnv exec / devenv** — `direnv exec DIR COMMAND [ARGS]`: DIR is a mandatory positional (the directory whose `.envrc` to load), no `--`. devenv has no `run` subcommand; scripts become commands inside `devenv shell` and args pass via `$@`. Both inherit env with additions from the loaded config.

**uvx / pipx run** — `uvx <tool> [args]` (tool positional, args follow without `--` for typical use; `--` separates uv's own flags). Ephemeral virtualenv; latest cached version used by default. `pipx run <pkg> [-- args]` uses `--` to isolate pipx flags from the tool's args. Both inherit env.

**bazel run** — `bazel run <target> -- [args]`. Single target required (ambiguous or absent = error). `--` mandatory and explicitly documented: without it, Bazel consumes `--prefixed` flags. Exit code forwarded from subprocess. Env injected with `BUILD_WORKSPACE_DIRECTORY`, `BUILD_WORKING_DIRECTORY`, `BUILD_EXECROOT`.

## 2. Cross-Cut Comparison Table

| Tool | Positional shape | Scope/group flag | argv separator | No-target default | Ambiguity rule | Reserved keywords |
|---|---|---|---|---|---|---|
| npm run | `run SCRIPT` | none | `--` (mandatory) | error + list | error | none |
| pnpm run | `run SCRIPT` | `--filter <sel>` (repeatable) | none (args follow script) | error | error | none (`*` = glob) |
| yarn run | `run SCRIPT` | `--include` / `--exclude` (foreach) | none | error | error | none |
| cargo run | `run` | `--bin NAME` | `--` (mandatory) | default-run field or error | error (explicit `--bin` required) | none |
| just | `RECIPE [args]` | none | none | first recipe or `default` recipe | error | none |
| task | `NAME [VAR=v]` | none | `--` → `.CLI_ARGS` | `default` task | error | `default` |
| mise exec | `[TOOL@VER...] -- CMD` | `--env PROFILE` | `--` (mandatory) | interactive shell | loads all from config | none |
| nix run | `INSTALLABLE [-- ARGV]` | none (flake attr) | `--` (between nix opts and argv) | `apps/packages.default` | searches default attrs | none |
| nix develop | `[FLAKE] --command CMD` | `-c` / `--command` | flag-based | interactive shell | attr priority order | none |
| direnv exec | `DIR COMMAND [ARGS]` | none | none | n/a (DIR required) | n/a | none |
| uvx | `TOOL [args]` | `--with EXTRA` | `--` (uv flags only) | n/a | n/a | none |
| bazel run | `TARGET -- [args]` | none | `--` (mandatory) | error | error | none |

## 3. Convergent Norms vs Divergent Patterns

**Convergent — nearly universal:**

- `--` as the separator between the host tool's flags and the subprocess argv is the dominant convention (cargo, npm, bazel, mise exec, nix run, uvx). Every tool that uses it treats it as mandatory — not optional. Tools that omit it (just, pnpm, yarn) rely on the subprocess having no flag-prefixed arguments that would be ambiguous.
- A missing required positional (script/recipe/target) produces an error with a usage hint or available-item listing. No tool silently no-ops.
- Single scope specifier at a time is the norm; pnpm's repeatable `--filter` is the notable exception allowing multi-target.
- Exit code forwarding is near-universal; npm's workspace case is a known bug, not a design choice.

**Divergent — no consensus:**

- Scope/group selection varies widely: pnpm uses a pre-command `--filter` flag; cargo uses `--bin`; nix uses flake attribute fragments; mise uses `--env` for profile switching; yarn uses `workspaces foreach`. There is no dominant pattern.
- Default behavior when no name given splits three ways: error (npm, cargo, bazel), run a `default` entry (task, just), or open an interactive shell (mise exec, nix develop).
- Clean vs inherited env: most tools inherit; Nix and mise's `--deny-env` choose isolation. The tension mirrors OCX's own product principle (product-context.md principle 4: "clean-env execution").
- `--` placement relative to the scope selector: mise puts TOOL args before `--`, cargo puts `--bin` before `--`, nix run treats everything before installable as nix options. No universal position.

**Reserved keyword practice:**

No surveyed tool reserves the word `all` as a special scope selector. `default` appears only in task/just as a recipe/task name convention, not as a CLI keyword processed by the runner's argument parser. `*` is a glob pattern in pnpm's `--filter`, not a reserved argument. This means no prior art conflict with OCX proposing to reserve `all` as a group alias.

## 4. Recommendations for `ocx run`

**Shape matches the dominant pattern — good.**
`ocx run [-g GROUP,...] [NAME...] -- ARGV` follows the cargo/bazel/mise exec convention: scope selector is a named flag before the binary/command name, `--` is mandatory, and the full ARGV is preserved verbatim. This is the shape users of Rust tooling and CI tools will recognize.

**`--` mandatory — matches user expectations, document the reason.**
Cargo, bazel, and mise exec all make `--` mandatory because it prevents ambiguity: arguments beginning with `-` would otherwise be consumed by the host parser. OCX should do the same. The docs should call this out explicitly — users coming from just or pnpm may expect to drop `--`.

**`-g GROUP,GROUP` (comma-separated, single flag) — accept both forms.**
pnpm's repeatable `--filter` (`-f foo -f bar`) is more composable and easier to autocomplete. Comma-separated (`-g ci,release`) is terser but slightly non-standard; `--group ci --group release` matches pnpm convention more closely. Note: `ocx pull` already accepts both via clap `value_delimiter = ','` + repeatable `-g`. Keep that precedent — ergonomic and consistent.

**`default` group name — well-grounded.**
`default` as a reserved name aligns with task (default task), just (default recipe), and Cargo's `default` feature. Users will not be surprised.

**`all` group name — no prior-art conflict, but make it explicit.**
No surveyed tool uses `all` as a CLI keyword, so reserving it carries no confusion cost. Document it as a pseudo-group: "equivalent to specifying every declared group including the default `[tools]` table". This matches `pnpm --filter '*'` semantically with an ergonomic alias.

**No-NAME behavior: error with binding listing — matches npm/cargo/bazel.**
For OCX's "backend tool" positioning (product-context.md), error is correct — there is no sensible "run everything" default for an arbitrary command exec. The error should list available binding names in the selected group(s). `ocx run` with no `--` and no `NAME` and no ARGV should be a usage error (exit 64) that lists bindings.

**Exit code forwarding — must be exact, no wrapping.**
Every reference tool that advertises exit code forwarding does so exactly (cargo, bazel). OCX's product principle (backend-first, machine-consumable) requires that the child's exit code propagates byte-for-byte. `child_process::exec` already does this on Unix via `execvp`; Windows path uses `propagate_exit_code` helper. Document exit-code forwarding in `ocx run --help`.

**Ambiguity: NAME appearing in multiple groups.**
No surveyed tool addresses this (they use different scoping axes). OCX existing precedent: `ocx remove` errors when binding name is ambiguous across groups, suggesting `--group` to narrow. `ocx run` should mirror — exit 64 with a hint listing the groups containing NAME. Consistent with already-deployed UX.

**ENV composition: clean by default — aligned with product principle.**
OCX's product-context.md principle 4 is "clean-env execution". Unlike most surveyed tools (which inherit), `ocx run` should inherit shell env by default (matching `ocx exec` precedent — `Env::new()` not `Env::clean()`) but offer `--clean` flag for full isolation. This matches `ocx exec`'s flag exactly, keeping the two commands' env-composition surface identical.
