---
topic: "User-declarable environment variables in project-level toolchain config files"
sources:
  - https://doc.rust-lang.org/cargo/reference/config.html
  - https://github.com/rust-lang/cargo/issues/9539
  - https://github.com/rust-lang/cargo/issues/10273
  - https://mise.jdx.dev/environments/
  - https://mise.jdx.dev/configuration/environments.html
  - https://github.com/jdx/mise/discussions/6766
  - https://github.com/jdx/mise/security/advisories/GHSA-436v-8fw5-4mj8
  - https://mise.jdx.dev/cli/trust.html
  - https://direnv.net/
  - https://direnv.net/man/direnv-stdlib.1.html
  - https://direnv.net/man/direnv.toml.1.html
  - https://taskfile.dev/usage/#environment-variables
  - https://github.com/go-task/task/discussions/449
  - https://docs.github.com/en/actions/writing-workflows/choosing-what-your-workflow-does/store-information-in-variables
  - https://docs.github.com/en/actions/reference/workflows-and-actions/variables
  - https://github.blog/changelog/2020-10-01-github-actions-deprecating-set-env-and-add-path-commands/
  - https://github.com/advisories/GHSA-7r3h-m5j6-3q42
  - https://docs.npmjs.com/files/npmrc/
  - https://be.bazel.build/designs/2016/06/21/environment.html
  - https://github.com/bazelbuild/bazel/issues/8074
  - https://docs.docker.com/compose/how-tos/environment-variables/envvars-precedence/
  - https://nix.dev/tutorials/first-steps/declarative-shell.html
  - https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.core/about/about_environment_provider?view=powershell-7.5
  - https://docs.astral.sh/uv/concepts/projects/config/
date: 2026-07-25
---

# Prior Art: Project-Level Env Var Declaration

## Tool-by-tool

**Cargo `[env]`** (`.cargo/config.toml`). Two forms: plain string (constant, always applied) or table `{ value, force, relative }`. `force` (default `false`) controls whether the declared value overrides a var already present in the *process* environment — default is non-destructive (existing env wins). `relative` (default `false`) resolves `value` as a path relative to the parent of the `.cargo` dir, emitting an absolute path; it is **not** a PATH-prepend primitive, just relative-path resolution. Zero interpolation — no templating engine at all. Config merging follows Cargo's normal directory-cascade rule: deeper (closer to CWD) config overrides ancestor/home config, key-by-key. Gated behind `-Z configurable-env` before stabilization (rust-lang/cargo#9539); no documented public regret found for the feature itself. [Reference](https://doc.rust-lang.org/cargo/reference/config.html)

**mise `[env]`** (`mise.toml`). Full Tera templating (`{{config_root}}`, `{{env.X}}`) plus a post-Tera shell-style `$VAR`/`${VAR:-default}` expansion pass. `_.path` adds PATH entries explicitly (distinct from a plain string assignment); `_.file` / `_.source` load dotenv/JSON/YAML/TOML files or extract vars from sourced shell scripts, both support `redact = true`. `required = true` fails hard commands but only warns on shell activation. Mise is now **walking back** templating scope: Tera functions for task-argument definition (`{{arg()}}`, `{{option()}}`, `{{flag()}}`) are deprecated as of 2026.5.0 (removal 2026.11.0) in favor of a plain `usage` field, explicitly because shell-escaping differs per shell and produced unpredictable behavior with spaces/special characters. [Environments](https://mise.jdx.dev/environments/), [deprecation](https://github.com/jdx/mise/discussions/6766)

**direnv** `.envrc`. `PATH_add <path>` prepends an absolute-expanded path to `PATH`, specifically to avoid the common mistake of *replacing* PATH. `layout <lang>` is a dispatcher to per-ecosystem setup (venvs, GOPATH, etc.). Trust model: any `.envrc` is inert until `direnv allow .` is run in that directory (or the path matches a `whitelist` prefix/exact rule in `direnv.toml`) — the strongest trust gate of any tool surveyed, explicitly designed so cloning an untrusted repo does not silently execute its declared env. [stdlib](https://direnv.net/man/direnv-stdlib.1.html), [trust](https://direnv.net/)

**Taskfile/go-task**. `env:` at global (Taskfile-wide) and per-task level; task-level overrides global, which overrides ambient OS env — i.e. **most-specific-scope wins**. `dotenv:` lists files at global/task level; for duplicate keys across dotenv files, **first file listed wins**. No path-typed primitive exists — users write `PATH: "{{.MY_BIN}}:$PATH"` by hand; go-task/task#449 documents this breaking cross-shell and on Windows (`;` vs `:` separator, and re-running the task can double-prepend since there's no idempotent add). [Docs](https://taskfile.dev/usage/#environment-variables), [issue](https://github.com/go-task/task/discussions/449)

**GitHub Actions**. `env:` nests workflow → job → step, with **step > job > workflow** (most specific wins). `GITHUB_*`/`RUNNER_*` reserved vars are hard-protected — attempts to set them via `env:` or `$GITHUB_ENV` are silently ignored, the only reserved-key protection found in this survey. The dynamic channel (`$GITHUB_ENV`/`$GITHUB_PATH` file writes) replaced the `set-env`/`add-path` *workflow commands*, deprecated Oct 2020 after a moderate-severity vuln: any workflow that logged untrusted data to stdout could inject `::set-env::` sequences and set arbitrary env/PATH. The file-based replacement then had its **own** narrower version of the same bug class: `@actions/core`'s `exportVariable` used a fixed delimiter string, so untrusted values containing that delimiter could break out and set unrelated vars (CVE-2022-35954, fixed in `@actions/core` v1.9.1). [Precedence](https://docs.github.com/en/actions/writing-workflows/choosing-what-your-workflow-does/store-information-in-variables), [deprecation](https://github.blog/changelog/2020-10-01-github-actions-deprecating-set-env-and-add-path-commands/), [CVE-2022-35954](https://github.com/advisories/GHSA-7r3h-m5j6-3q42)

**npm/pnpm/yarn**. No declarative project env table. `.npmrc` supports `${VAR}` *interpolation of existing env into config values* (not the reverse), and `package.json#config` exposes values to scripts as `npm_package_config_*` — neither is a general env-declaration mechanism. Ecosystem convention punts to `.env` + `dotenv`/`cross-env` in scripts. [npmrc](https://docs.npmjs.com/files/npmrc/)

**Bazel `--action_env` / `--repo_env`**. Both CLI-flag-only (no config-file table); `--action_env` affects build actions, `--repo_env` was added later specifically to scope repository-rule env separately from action env, because `--action_env` conflated the two and caused unwanted cache invalidation. Precedence follows Bazel's general rc-file rule: more-specific invocation flags beat common/inherited ones; within a level, later rc-file order wins. No path-typed primitive; no interpolation. [Design](https://be.bazel.build/designs/2016/06/21/environment.html), [issue](https://github.com/bazelbuild/bazel/issues/8074)

**docker-compose `environment:` vs `env_file:`**. Precedence (highest→lowest): `docker compose run -e` CLI > `environment:` in the compose file > `env_file:`. Multiple `env_file` entries: **later file wins** for duplicate keys (opposite of Taskfile's dotenv rule — worth noting as a real divergence). [Docs](https://docs.docker.com/compose/how-tos/environment-variables/envvars-precedence/)

**Nix `mkShell` / `shellHook`**. Any non-reserved attribute passed to `mkShell` becomes an env var directly; a small reserved set (e.g. `PS1`) is silently protected by nix-shell itself and must instead be set via `shellHook` (arbitrary shell code run on shell entry) — i.e. Nix *does* have a narrow, undocumented-as-a-list reserved-key concept, but only for a couple of shell-internal variables, not a general self-reconfiguration guard. [Tutorial](https://nix.dev/tutorials/first-steps/declarative-shell.html)

**asdf / uv**: neither supports declaring arbitrary env vars in `.tool-versions` or `pyproject.toml`. asdf only reads `ASDF_<TOOL>_VERSION` overrides for version selection; uv's `[tool.uv]` in `pyproject.toml` has no general env-declaration section. Both implicitly defer to direnv/mise for this. [uv config](https://docs.astral.sh/uv/concepts/projects/config/)

## Cross-cut comparison

| Tool | Value grammar | Interpolation | Path-typed entry | Scope layers | Precedence | Self-var protection | Trust model |
|---|---|---|---|---|---|---|---|
| Cargo | string \| `{value,force,relative}` | none | no (`relative` = path resolve, not PATH-prepend) | dir cascade (home→project) | deeper dir wins; `force` controls vs ambient env | none found | none (executes on `cargo` invocation) |
| mise | string \| Tera \| `_.path`/`_.file`/`_.source` | full Tera + shell-style | yes (`_.path`) | global/profile/task | later/more-specific wins | none found | `mise trust` prompt — 2 CVEs on bypass |
| direnv | shell (`export`, `PATH_add`) | shell substitution | yes (`PATH_add`) | directory (`.envrc` per dir) | deepest allowed `.envrc` wins | n/a (shell script, no config-var self-hazard) | `direnv allow` — explicit, strongest surveyed |
| Taskfile | string | Go templates (`{{.X}}`) | no (manual `$PATH:` string) | global/task | task > global > OS env; dotenv: first file wins | none found | none |
| GH Actions | string | `${{ }}` expressions | no (separate `$GITHUB_PATH` channel) | workflow/job/step | step > job > workflow | **yes** — `GITHUB_*`/`RUNNER_*` hard-blocked | n/a (CI-native, no clone-trust question) |
| Bazel | CLI flag only | none | no | invocation flags / rc-files | more-specific invocation > common; later rc wins | none found | none |
| docker-compose | string | shell-style `${VAR}` in file | no | `environment:` vs `env_file:` | environment > env_file; later env_file wins | none found | none |

## Convergent norms vs divergent patterns

**Convergent**: (1) "most specific scope wins" is universal wherever scopes exist (Taskfile task>global, GH Actions step>job>workflow, docker-compose environment>env_file, Bazel invocation>rc). (2) No tool except GitHub Actions protects its own configuration vars from being set by the same file/flag that declares user env. (3) Config-file-only tools (Cargo, Taskfile, Bazel, docker-compose) run env declarations unconditionally on invocation — no trust gate; only shell-activation tools (direnv, mise) gate on trust, because they run on `cd`, not on explicit invocation.

**Divergent**: interpolation is a hard split — Cargo/Bazel/Taskfile-env (mostly literal) vs mise (full templating engine) vs GH Actions (its own expression language). Multi-file merge-order also diverges: Taskfile dotenv = *first* file wins, docker-compose env_file = *last* file wins — same shape, opposite convention, both undocumented as a general principle (had to be found per-tool).

## Recommendations for OCX

- **User-declared wins after package-composed env** — matches the near-universal "most specific/explicit layer applied last" pattern (Taskfile, GH Actions, docker-compose, Bazel all agree). Defensible, mainstream.
- **No interpolation in v1** — matches Cargo's deliberate stance, and is *better supported by evidence now* than a year ago: mise is actively retreating from unrestricted templating due to shell-escaping unpredictability. Defensible; validated by mise's own regret.
- **`{ type = "path" | "constant" }` distinguishing PATH-prepend from constant** — matches mise `_.path` and direnv `PATH_add`; closes a gap that OCX's own upstream reference tool (go-task) does **not** close (go-task/task#449: hand-rolled `$PATH:` strings break on Windows separators and aren't idempotent on re-entry). Defensible, and directly relevant since OCX wraps go-task.
- **Repeatable `--env KEY=VALUE` CLI flag** — justified by a confirmed real gap: PowerShell (`$env:X`) and `cmd.exe` (`%X%`) have no POSIX-style inline invocation prefix (`FOO=bar cmd`), per Microsoft's own environment-provider docs. A CLI flag is the only portable per-invocation override on Windows. Defensible.
- **Warning sign — no reserved-key guard found in the plan.** Of all tools surveyed, only GitHub Actions blocks a config surface from setting the tool's own control variables. Cargo, mise, Taskfile, Bazel, docker-compose all leave this open, and it shows: it is exactly the kind of gap that becomes a CVE once someone treats project config as adversarial input (see mise's `credential_command`-before-trust-check bug, same causal shape as "the config controls its own governance"). OCX's `[env]` / `[group.*.env]` / `--env` should explicitly reject or warn on `OCX_*` keys before this ships — recommend closing this now rather than joining the majority who left it open.
- **Open, not yet a deviation — trust boundary for `ocx.toml [env]` in a freshly cloned repo.** Cargo and Taskfile accept silent unconditional env execution (their answer: you already trust a build tool with arbitrary build scripts). direnv gates hard. There's no consensus; OCX should decide deliberately rather than by default, given `ocx.toml` sits closer to "declarative package config" than "build script."

## Known regrets in prior art

- **GitHub Actions**: `set-env`/`add-path` stdout-injection (deprecated 2020) → replaced by file-channel → file-channel got its own narrower delimiter-injection vuln (CVE-2022-35954, 2022). Two generations of the same bug class in one feature's history — the sharpest cautionary tale for any tool that lets automation write env vars via a side-channel file.
- **mise**: Tera templating in run-script task-argument definitions is being deprecated (2026.5.0→2026.11.0 removal) — direct, in-repo admission that unrestricted templating there was a complexity/escaping footgun. Also two 2026 CVEs where local, untrusted config could control mise's own trust evaluation before the trust check ran (GHSA-436v-8fw5-4mj8; a related `credential_command` pre-trust execution issue) — the self-governance-hazard pattern flagged above, materialized.
- **Cargo**: no public regret found for `[env]` itself — noting the absence explicitly rather than asserting one (`UNVERIFIED` if claimed otherwise).
