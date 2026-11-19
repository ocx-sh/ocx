# Research: Operator-injected configuration in CLI tools — mechanisms and telemetry destination

## Metadata

**Date:** 2026-11-18
**Domain:** cli
**Triggered by:** hex-discuss otel-telemetry
**Expires:** 2027-05-18

Scope: evidence only, no recommendation, no ranking. Mechanisms (a)-(h) as named in the brief: (a) system file at fixed path, (b) env var naming one file, (c) env var naming an ordered list of files, (d) drop-in dir, (e) include directives, (f) inline env overrides, (g) managed/remote policy, (h) flags injected by a wrapper.

Verification legend: **[doc]** read from the tool's own docs this session; **[src]** read from the tool's source this session; **[run]** reproduced locally (git 2.43.0); **[2nd]** secondary source only; **[mem]** not re-verified this session. Every source fetched was dated 2026 unless flagged "OLD".

## Direct answer

### Table 1 — tool x mechanism

`Y` = offered; `-` = not offered or not found in docs; names in parentheses.

| Tool | (a) system file | (b) env -> 1 file | (c) env -> file list | (d) drop-in dir | (e) include | (f) env per key | (g) managed/remote | (h) wrapper flags |
|---|---|---|---|---|---|---|---|---|
| git | Y `$(prefix)/etc/gitconfig` | Y `GIT_CONFIG_GLOBAL`, `GIT_CONFIG_SYSTEM` (replace the level) | - | - | Y `include.path`, `includeIf` | Y `GIT_CONFIG_COUNT`/`KEY_n`/`VALUE_n` | - | Y `git -c k=v` |
| kubectl | - (only `~/.kube/config`) | Y `--kubeconfig` (flag) | Y `KUBECONFIG` (`:` Unix, `;` Windows) | - | - | - | - | Y `--kubeconfig`, `--server` etc. |
| Bazel | Y `/etc/bazel.bazelrc`, `%ProgramData%\bazel.bazelrc` | Y `--bazelrc=f` | Y `BAZELRC` (comma-separated); `--bazelrc` repeatable | - | Y `import`, `try-import`, `try-import-if-bazel-version` | - (flags only) | - | Y flags override rc; `--ignore_all_rc_files` |
| cargo | - (`$CARGO_HOME/config.toml` is per-user) | - (no env var names a config file) | - | - | Y `include = [{path, optional}]` | Y `CARGO_<KEY>` per key | - | Y `--config KEY=VAL` or `--config path.toml`, repeatable |
| mise | Y `/etc/mise/config.toml`, `/etc/mise/miserc.toml` | Y `MISE_GLOBAL_CONFIG_FILE`, `MISE_SYSTEM_CONFIG_FILE` | - | - (conf.d not documented; `config.override.toml` is a tier) | Y (include in config, not fetched at "safe" mode) | Y `MISE_<SETTING>` | partial (miserc environments; no remote fetch) | Y CLI flags |
| uv | Y `/etc/uv/uv.toml`, `%PROGRAMDATA%\uv\uv.toml` | Y `UV_CONFIG_FILE` (= `--config-file`) | - | - | - | Y `UV_*` per key (space/comma lists vary) | - | Y `--config-file`, `--no-config` |
| pip | Y global `/etc/pip.conf`, `/etc/xdg/pip/pip.conf`, `C:\ProgramData\pip\pip.ini` | Y `PIP_CONFIG_FILE` | - | - | - | Y `PIP_<OPT>` (space-separated lists) | - | Y flags |
| npm | partial builtin `npmrc` + global `$PREFIX/etc/npmrc` | Y `NPM_CONFIG_USERCONFIG`, `NPM_CONFIG_GLOBALCONFIG` (`userconfig`/`globalconfig`) | - | - | - | Y `npm_config_*` | - | Y flags |
| pnpm | - (global `<pnpm config>/config.yaml`) | Y `PNPM_CONFIG_NPMRC_AUTH_FILE`, `NPM_CONFIG_USERCONFIG` | - | - | - | Y `pnpm_config_*`, `pnpm_config_//host/:_authToken` | - | Y flags |
| Docker CLI | - (none documented) | Y `DOCKER_CONFIG` (a directory) | - | - | - | Y `DOCKER_CLI_OTEL_EXPORTER_OTLP_ENDPOINT` (one var; others per-var) | - | Y `--config`, `-H`, `-c` |
| Claude Code | Y `/etc/claude-code/managed-settings.json` (Linux), `/Library/Application Support/ClaudeCode/` (macOS), `C:\Program Files\ClaudeCode\` (Windows) | - (`--settings` takes file or JSON; `CLAUDE_CONFIG_DIR` a dir) | - | Y `managed-settings.d/*.json`, alphabetical | - | Y `env` block + real env | Y server-managed, macOS profile `com.anthropic.claudecode`, `HKLM\SOFTWARE\Policies\ClaudeCode` | Y `--settings` |
| GitHub CLI | - | Y `GH_CONFIG_DIR` (a directory) | - | - | - | Y `GH_TOKEN`, `GH_HOST`, ... | - | - |
| Nix | Y `/etc/nix/nix.conf` (`NIX_CONF_DIR`) | - | Y `NIX_USER_CONF_FILES` (`:` / `;`, reverse load) | - | Y `include`, `!include` | Y `NIX_CONFIG` (a whole config as text) | - | Y `--option`, `--extra-<name>` |
| systemd (reference for d) | Y `/usr/lib`, `/etc`, `/run` trees | - | - | Y `<unit>.d/*.conf` | - | - | - | - |
| Dagger | Y engine `/etc/dagger/engine.json` | - | - | - | - | Y `OTEL_EXPORTER_OTLP_*` | Dagger Cloud token | - |

### Table 2 — merge / enforcement / trust / introspection

| Tool | Merge across layers (scalar / list / table) | Operator value non-overridable? | Can repo-level file change security-relevant settings? | Introspection (value + origin) |
|---|---|---|---|---|
| git | last wins / multi-valued keys accumulate across all files / (sections merge per key); order documented. Env pairs sit above files, below `-c` | No general lock. Only "protected configuration" (system, global, command scopes): some keys ignored from repo config (`safe.directory`, `safe.bareRepository`) | Repo `.git/config` is read and can set hook-like keys; mitigated by ownership check (`safe.directory`) and by protected-scope-only keys | `git config --list --show-origin --show-scope` **[run]** (env pairs show `command line:`) |
| kubectl | first wins per key; map keys never overwritten; lists not described in docs | No | Kubeconfig is code-like (exec plugins); docs warn only "use trusted files" | `kubectl config view` (merged; `--flatten`); origin per key not shown (not found) |
| Bazel | later file wins on same flag; command specificity (`common` < `build` < `test`) beats file order; repeated flags combine | No: docs describe no enforcement; user can pass `--nosystem_rc`, `--ignore_all_rc_files`; system path only changeable by building a custom binary | Workspace `.bazelrc` is repo-controlled and read by default; no trust gate documented | `bazel info`/`--announce_rc` (not fetched, **[mem]**) |
| cargo | scalars: deeper dir wins, home lowest; arrays: concatenated, higher precedence later; tables merged. `--config` > env > files | No | Repo `.cargo/config.toml` is read from cwd upward; docs give no trust model | `cargo config get --show-origin --merged` is nightly-only (`-Zunstable-options`), TOML only **[2nd]** |
| mise | `[tools]` nearest replaces whole tool entry; `[env]`/`[vars]`/`[settings]` per key nearest wins; tasks per name. Override file tier above project | Partial: "global-only" settings are ignored from project config; `miserc` environments from `/etc/mise/miserc.toml` cannot be removed by project; env-only settings ignored in every file | Gated by `mise trust`; history of trust bypasses (see incidents) | `mise config` / `mise config ls` list files, lowest precedence first; no per-setting origin command found |
| uv | scalars: project beats user beats system; arrays concatenated, project entries first; env beats files; CLI beats env | No: `--no-config` / `--config-file` replace discovery | Project `uv.toml`/`pyproject.toml` can set indexes; no trust gate documented | not found in docs fetched |
| pip | later file overrides earlier; repeatable options (`find-links`, `trusted-host`) append; env beats files; CLI beats env | No: `PIP_CONFIG_FILE=os.devnull` disables all files | Site config (`$VIRTUAL_ENV/pip.conf`) is environment-local, not repo | `pip config debug` lists file paths and values; `pip config list` not confirmed in fetched page |
| npm | cli > env > project > user > global > builtin; lists via `key[]` repeated | No | Project `.npmrc` is read; auth keys must be URI-scoped | `npm config ls -l` (docs list it for defaults); origin display not covered |
| pnpm | for auth/TLS keys first file wins: project `.npmrc` > `auth.ini` > `~/.npmrc` | Partial: `_auth` and `tokenHelper` only from user-level files | Env expansion in repo `.npmrc`/workspace yaml removed after GHSA-3qhv-2rgh-x77r; credential keys in repo files ignored with warning | not covered in fetched page |
| Docker CLI | docs: CLI > env > `config.json`. OTLP endpoint: context metadata `otel` map, overridden by `DOCKER_CLI_OTEL_EXPORTER_OTLP_ENDPOINT` | No | `DOCKER_CONFIG` selects a directory; no project-file layer exists | not found |
| Claude Code | managed > `--settings` > project local > project shared > user. Single values: later wins; lists: union with dedupe; objects (`env`): key-by-key. Across managed sources: `first-wins` default or `merge` | **Yes**: managed settings sit above every layer; OTLP destination pinned by deleting developer-set per-signal variables at startup | **No for OTEL**: repo settings `env` ignore OTEL exporter variables; some other keys wait for folder trust | `/status` shows `Setting sources`; "does not show which file supplied each key"; `claude doctor` |
| GitHub CLI | `GH_TOKEN` over stored credentials; no layering docs | - | - | not found |
| Nix | later wins; `extra-<name>` appends for lists; `NIX_CONFIG` last | Partial: daemon-side settings (`post-build-hook`, `diff-hook`) only from global `nix.conf` or trusted users; `trusted-substituters` limits what unprivileged users may pass | Nix does not read a repo-level conf (flake `nixConfig` exists but was not researched) | `nix config show` (experimental); whether it reports origin **not confirmed** |
| systemd | drop-ins merged lexicographically by filename across directories; `/etc` beats `/run` beats `/usr/lib`; empty assignment resets a list | `/etc` over `/usr/lib` only (admin vs vendor); no user-vs-admin lock | n/a | `systemctl cat` prints fragment and drop-ins |
| Dagger | OTel envs in the engine process | - | - | - |

## Git

- Files in order: system `$(prefix)/etc/gitconfig`, `$XDG_CONFIG_HOME/git/config`, `~/.gitconfig`, `$GIT_DIR/config`, `$GIT_DIR/config.worktree`. Last value wins; multi-valued keys use all values. Missing global/system files ignored. https://git-scm.com/docs/git-config
- `GIT_CONFIG_GLOBAL` / `GIT_CONFIG_SYSTEM` replace their level; if `GIT_CONFIG_SYSTEM` is set the built-in system file is not read, likewise for global; `/dev/null` skips a level. https://git-scm.com/docs/git (source: https://raw.githubusercontent.com/git/git/master/Documentation/git.adoc)
- `GIT_CONFIG_COUNT` with `GIT_CONFIG_KEY_<n>`/`GIT_CONFIG_VALUE_<n>`: zero-indexed pairs added to runtime config; a missing key or value is an error; empty count equals 0; env pairs override files, `git -c` overrides env. https://git-scm.com/docs/git-config
- Include: relative path is relative to the including file; contents inserted at the directive. A missing included file is silently ignored **[run]** (git 2.43.0: `include.path=nope.cfg` produced no error; the docs page does not state this). https://raw.githubusercontent.com/git/git/master/Documentation/config.adoc
- Protected configuration = system, global, command scopes; some options are honoured only there. `safe.bareRepository` and `safe.directory` are examples; `safe.directory` multi-valued with empty value resetting the list. https://raw.githubusercontent.com/git/git/master/Documentation/config/safe.adoc
- Telemetry: none. There is no operator layer above the user: system file is overridable by user file and by `-c`.
- Introspection: `git config --list --show-origin --show-scope`; env pairs shown with origin `command line:` **[run]**.

## kubectl / kubeconfig

- `KUBECONFIG`: `:` on Linux/macOS, `;` on Windows; unset means `~/.kube/config`, no merge. Empty names ignored; undeserializable content errors; first file to set a value or map key wins and it is never changed (first file's `current-context` kept). `--kubeconfig` uses only that file, no merge. Docs warn: only use kubeconfigs from trusted sources, a crafted file can execute code or expose files. https://kubernetes.io/docs/concepts/configuration/organize-cluster-access-kubeconfig/
- Missing file in the list: skipped (`os.IsNotExist`), with a warning only when every listed file is missing; an explicit `--kubeconfig` path that is missing errors immediately. Paths inside a kubeconfig resolve against that file's directory. Duplicates removed. **[src]** https://raw.githubusercontent.com/kubernetes/client-go/master/tools/clientcmd/loader.go
- Relative paths *in the `KUBECONFIG` variable itself*: the docs say nothing; the loader splits with `filepath.SplitList` and opens them relative to cwd (inferred from source, not tested; kubectl not installed here).
- Lists (e.g. `clusters` entries) are keyed by name, so "first wins" is per named entry; the docs do not describe list merge beyond that.
- Complaint: "first file's `current-context` wins" confuses tools that switch context in a later file: https://github.com/kubernetes-client/javascript/issues/579 (**[2nd]**, issue age not checked, may be older than 18 months — re-verify).
- No telemetry. No operator layer; `KUBECONFIG` is the only injection point.

## Bazel

- Rc order: system (`/etc/bazel.bazelrc`; `%ProgramData%\bazel.bazelrc`), workspace `.bazelrc`, home `~/.bazelrc`, `BAZELRC` env (comma-separated paths), `--bazelrc=file` (repeatable). Later overrides earlier on conflict. Missing system file not an error. `--bazelrc=/dev/null` stops later `--bazelrc` files. Startup options `--nosystem_rc`, `--noworkspace_rc`, `--nohome_rc`, `--ignore_all_rc_files`. https://bazel.build/run/bazelrc
- `import` fails on a missing file; `try-import` silently continues; `%workspace%` prefix; imported file overrides what precedes the line, lines after the import override it. Same page.
- Repeated flags: the later value wins (`--test_tmpdir`); config groups `command:name` activate via `--config=name`. Same page.
- Enforcement: the page describes none; inference: a user can skip with `--nosystem_rc`/`--ignore_all_rc_files`. Changing the system rc path needs a custom binary built with `BAZEL_SYSTEM_BAZELRC_PATH` (**[2nd]** via https://bazel.googlesource.com/bazel/+/HEAD/docs/run/bazelrc.mdx).
- Trust: the workspace `.bazelrc` is repository-controlled and read by default; no trust gate found. No documented incident found.
- Telemetry: Bazel's Build Event Service destination is set by flags (`--bes_backend`) and so lives in rc layers — **[mem], not verified this session**.

## cargo

- Discovery: `.cargo/config.toml` in cwd and each ancestor, then `$CARGO_HOME/config.toml`. Scalars: deeper dir wins, home lowest. Arrays: joined, higher precedence placed later. Tables merged. Precedence: `--config` > env > files. https://doc.rust-lang.org/cargo/reference/config.html
- `include = [{path=..., optional=true}]`: relative to the including file, `.toml` only; loaded left to right, later overrides earlier, then the including file's own values on top. Same page.
- Env: `CARGO_FOO_BAR` for `foo.bar`; only integer/boolean/string "and some array values" supported; the page does not define an array separator (per-key: `RUSTFLAGS` whitespace-split, `CARGO_ENCODED_RUSTFLAGS` uses 0x1f). https://doc.rust-lang.org/cargo/reference/environment-variables.html
- `--config KEY=VALUE` or `--config path`, repeatable, merged left to right; paths in env/`--config KEY=VAL` are relative to cwd, in files relative to the parent of the `.cargo` dir. Same page.
- No env var names a config file; no system-wide file (confirmed absent in the env-var page).
- `[patch]` note: config files "are not usually checked into source control". No trust model documented for config in untrusted checkouts (searched; none found).
- Introspection: `cargo config get` with `--show-origin`/`--merged` is unstable (tracking issue https://github.com/rust-lang/cargo/issues/9301, **[2nd]**), TOML only for origin, includes tokens by default.
- No telemetry.

## mise

- Order (low to high): system `/etc/mise` (`MISE_SYSTEM_CONFIG_DIR`, `MISE_SYSTEM_CONFIG_FILE`), global `~/.config/mise` (`MISE_CONFIG_DIR`, `MISE_GLOBAL_CONFIG_FILE`), project dirs root-to-cwd (`mise.toml`, `mise.local.toml`), global `config.override.toml` (outranks all project config). `MISE_CONFIG_FILE` is **not** documented on the pages read. https://mise.jdx.dev/configuration.html
- Merge: `[tools]` nearest entry replaces the whole tool entry including options; `[env]`, `[vars]`, `[settings]` per key; `[tasks]` per name. Same page.
- Env vs files: `MISE_<SETTING>`, below CLI flags, above every config file. https://mise.jdx.dev/configuration/settings.html
- Lock-down: "global-only" settings (e.g. `paranoid`, `trusted_config_paths`) take effect only in global/system config or via env var; project config ignores them with a warning; env-only settings ignored in all files; safe mode ignores `[settings]` in project config. Environments loaded from `/etc/mise/miserc.toml` cannot be removed by a project file. Same pages.
- Trust: untrusted project cannot make mise fetch URLs on `cd`; `mise trust`. Same page.
- Introspection: `mise config`, `mise config ls` (tier column), `mise doctor`; no per-setting origin command found.
- No telemetry destination setting seen.

## uv

- Locations: project (`uv.toml` / `pyproject.toml [tool.uv]`), user `~/.config/uv/uv.toml` / `%APPDATA%\uv\uv.toml`, system `/etc/uv/uv.toml` / `%PROGRAMDATA%\uv\uv.toml`; only the first system file is used. https://docs.astral.sh/uv/concepts/configuration-files/
- Precedence project > user > system; env > files; CLI > env. Scalars: project value used; arrays concatenated with project entries first. Same page.
- `--no-config` / `UV_NO_CONFIG` disable discovery; `--config-file` / `UV_CONFIG_FILE` (one `uv.toml`) replaces all discovered files. https://docs.astral.sh/uv/reference/environment/
- Env list separators differ per variable: `UV_INDEX` and `UV_EXTRA_INDEX_URL` whitespace; `UV_FIND_LINKS` comma; `UV_CONSTRAINT`/`UV_OVERRIDE` whitespace. Same page. (Axis-7 relevant: inconsistent separators.)
- No enforcement documented; no telemetry setting found.

## pip

- Layers: global (Unix `/etc/xdg/pip/pip.conf`, `/etc/pip.conf`; Windows `C:\ProgramData\pip\pip.ini`; macOS `/Library/Application Support/pip/pip.conf`), user, site (`$VIRTUAL_ENV/pip.conf`), then `PIP_CONFIG_FILE`. Each overrides the previous; repeatable options append. `PIP_CONFIG_FILE=os.devnull` disables all files. https://pip.pypa.io/en/stable/topics/configuration/
- Env: `PIP_<LONG_NAME>`; list values space-separated; `PIP_VERBOSE=3` counted form; empty string is not false. Same page.
- Page does not say whether other levels are skipped when `PIP_CONFIG_FILE` points to a file (it says the user file is not loaded).
- Introspection: `pip config debug`. Same page.
- Incident class: `PIP_INDEX_URL`/`PIP_EXTRA_INDEX_URL` inherited from a base image silently override `pip.conf` (**[2nd]**, https://dev.to/brabster/how-to-get-pwned-with-extra-index-url-462g, date not checked); dependency-confusion via `--extra-index-url` (https://portswigger.net/daily-swig/dependency-confusion-attack-mounted-via-pypi-repo-exposes-flawed-package-installer-behavior, 2021 — OLD, re-verify).

## npm and pnpm

- npm: four files (project `.npmrc`, user `~/.npmrc` or `userconfig`, global `$PREFIX/etc/npmrc` or `globalconfig`, built-in). Priority: CLI > `npm_config_*` env > project > user > global > built-in > defaults. Lists via `key[]=`. Underscored `.npmrc` keys cannot be overridden by env because env underscores convert to hyphens. https://docs.npmjs.com/cli/v11/configuring-npm/npmrc and https://docs.npmjs.com/cli/v11/using-npm/config
- npm auth keys must be scoped to a registry URI prefix "so npm will never send credentials to the wrong host". Same page.
- pnpm: for auth/TLS keys, first wins: `<workspace>/.npmrc` > `<pnpm config>/auth.ini` > `~/.npmrc`. `_auth` only from `config.yaml`/`pnpm_config__auth`. `tokenHelper` only from user-level files, never env. Project `.npmrc` gets no env expansion for registry/proxy URLs, `//` keys, credentials, `tokenHelper`, `cert`, `key`; ignored with warning. `PNPM_CONFIG_NPMRC_AUTH_FILE` (v11) / `NPM_CONFIG_USERCONFIG` (v10) declare a repo `.npmrc` trusted and "disable the protection". https://pnpm.io/npmrc
- Incident: GHSA-3qhv-2rgh-x77r / CVE-2026-55180 — see Incidents.

## Docker CLI

- Docs: CLI flag > env > `config.json`; `DOCKER_CONFIG` (directory, overridden by `--config`); `DOCKER_CONTEXT` over `DOCKER_HOST`. No system-wide CLI config. https://docs.docker.com/reference/cli/docker/#environment-variables
- OTel enable: `DOCKER_CLI_OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317`; off by default; one metric `command.time`; docs give no fleet guidance and do not mention `TRACEPARENT` or `config.json`. https://docs.docker.com/engine/cli/otel/
- Source (docker/cli master, cloned 2026-11-18, commit dated 2026-11-17): `cli/command/telemetry_docker.go` reads `OTEL_EXPORTER_OTLP_ENDPOINT` from the current **docker context metadata** field `otel`; the `DOCKER_CLI_`-prefixed env var overrides it only when non-empty; `unix`, `http`, `https` schemes; export timeout is 50 ms (`ExportTimeout` in `telemetry.go`); retry disabled; gRPC exporters only (`otlpmetricgrpc`, `otlptracegrpc`). **[src]**
  - So the destination layer is "context metadata, then one prefixed env var". The generic `OTEL_EXPORTER_OTLP_ENDPOINT` env var is deliberately not read (the `DOCKER_CLI_` prefix is the override), which isolates the CLI from ambient CI values.
- `TRACEPARENT`: `grep -rniE "traceparent|TraceContext|propagation\."` over the Go sources outside `vendor/` returned nothing **[src]** — the Docker CLI does not read an inbound `TRACEPARENT` in this checkout. (Plugins and buildx not checked.)
- Contrast with Claude Code, which reads inbound `TRACEPARENT` only in non-interactive modes (below).

## Claude Code

- Scopes and precedence, highest first: managed, command line (`--settings`), project local, project shared, user. Lists merge (union), single values replaced by the higher scope, `env` merges key-by-key. https://code.claude.com/docs/en/settings
- Managed delivery: server-managed; macOS profile `com.anthropic.claudecode`; `HKLM\SOFTWARE\Policies\ClaudeCode` value `Settings` (REG_SZ/REG_EXPAND_SZ); HKCU same key (user-writable, never above an admin source); file `managed-settings.json` at `/Library/Application Support/ClaudeCode/`, `/etc/claude-code/`, `C:\Program Files\ClaudeCode\` (legacy `C:\ProgramData\ClaudeCode\` not read). Optional `managed-settings.d/*.json` merged after `managed-settings.json`, alphabetical, numeric prefixes advised; single values: later replaces; lists: union with dedupe; objects: key-by-key. https://code.claude.com/docs/en/managed-settings
- Across managed sources: `managedSourcesBehavior` `first-wins` (default; highest-ranked source with any policy key, others ignored with no warning) or `merge` (v2.1.242+). `env` is merged per variable across admin sources (v2.1.223+). Same page.
- Enforcement exceptions: some keys honour a stricter lower-level value even over managed (e.g. `disableClaudeAiConnectors: true` from any scope). https://code.claude.com/docs/en/settings
- Introspection: `/status` shows `Setting sources` and names the managed source and skipped ones; it "doesn't show which file supplied each key"; `claude doctor` lists rejected entries. Same page.
- Telemetry destination (https://code.claude.com/docs/en/monitoring-usage):
  - Enable: `CLAUDE_CODE_ENABLE_TELEMETRY=1`; exporters `OTEL_METRICS_EXPORTER`/`OTEL_LOGS_EXPORTER`/`OTEL_TRACES_EXPORTER`; endpoint `OTEL_EXPORTER_OTLP_ENDPOINT` with per-signal overrides; headers `OTEL_EXPORTER_OTLP_HEADERS`, per-signal headers merged with generic; dynamic `otelHeadersHelper` setting.
  - Admin path: managed settings `env` block. Developer-set per-signal endpoint/protocol/headers/cert variables are **removed at startup** when managed settings set the generic one (debug-log warning). Exporter selectors follow normal precedence, so a developer can still disable a signal unless the selector is also set in managed settings (v2.1.223+). Per-signal variables set in managed settings are kept, so one signal can go to a different collector. Before v2.1.217 per-signal vars in user settings or shell could redirect away from the managed collector.
  - Repo layer: repo `.claude/settings.json` and `.local.json` **ignore OTEL exporter variables**; a repo may still turn a signal off (`OTEL_LOGS_EXPORTER=none`) unless managed settings, `--settings` or the launching environment set it. `OTEL_LOG_MANAGED_SETTINGS` cannot be enabled from project/local settings.
  - Child processes: `OTEL_*` variables are not passed to Bash tool, hooks, MCP servers.
  - `TRACEPARENT`: subprocesses inherit it when tracing is active; `traceparent` header sent to the Anthropic API only (or with `CLAUDE_CODE_PROPAGATE_TRACEPARENT=1` through a custom `ANTHROPIC_BASE_URL`); inbound `TRACEPARENT`/`TRACESTATE` read only in Agent SDK and `claude -p`, ignored in interactive sessions "to avoid picking up ambient CI or container values".
  - Cloud sessions: no collector credentials in cloud environment variables (readable by anyone using the environment); use server-managed settings.
- The "ignored variables in project settings" list itself (settings-reference section) could not be retrieved in full; only the monitoring page's statement and per-row notes in https://code.claude.com/docs/en/env-vars were read.

## GitHub CLI

- `GH_CONFIG_DIR` (directory; default `$XDG_CONFIG_HOME/gh`, `$AppData/GitHub CLI`, `$HOME/.config/gh`); `GH_TOKEN` over stored credentials; no system-wide or admin-enforced config documented. https://cli.github.com/manual/gh_help_environment
- Telemetry: not researched (**not verified**).

## Nix

- `nix.conf`: system dir `/etc/nix` (`NIX_CONF_DIR`); user via `NIX_USER_CONF_FILES` — each path loaded "in reverse order", separator `:` on Unix, `;` on Windows (so the first-listed file is loaded last and wins); `NIX_CONFIG` content treated as a config file with newline-separated settings, loaded after files. https://nix.dev/manual/nix/latest/command-ref/conf-file
- `include <path>` relative to the current file; "A missing file is an error unless `!include`". Unknown names in files: warning; on CLI: error. `extra-<name>` appends for list settings. Same page.
- Trust model: `trusted-users` (essentially root), `allowed-users`, `trusted-substituters` (unprivileged users may only pass these), `post-build-hook` only from global `nix.conf` or CLI by trusted users, `diff-hook`/`run-diff-hook` with the daemon must be in `nix.conf`. Same page. System file values "are not forwarded to the Nix daemon".
- Introspection: `nix config show` exists (experimental); origin reporting not confirmed. https://nix.dev/manual/nix/2.34/command-ref/new-cli/nix3-config-show
- No telemetry.

## systemd (reference for d)

- `foo.service.d/*.conf` merged in lexicographic filename order "regardless of which of the directories they reside in"; `/etc` beats `/run` beats `/usr/lib`; same-named file in a higher-priority directory shadows the lower; type-wide `service.d/` has lower precedence than name-specific; prefix hierarchy (`foo-.service.d/`). Empty assignment resets a list (stated for `Documentation=` and `Condition…=`/`Assert…=`; no general rule given on that page). https://www.freedesktop.org/software/systemd/man/latest/systemd.unit.html
- Introspection: `systemctl cat` prints fragment and drop-ins; reflects files on disk, not necessarily the loaded state. https://www.freedesktop.org/software/systemd/man/latest/systemctl.html
- Systemd has no user-vs-admin lock: the layering separates vendor from admin, and an admin can always add a drop-in.

## Dagger

- Earlier 404 was the docs URL, not absence of docs. Current docs in the repo (cloned 2026-11-18; `docs/current_docs/reference/config-files/engine-json.mdx`): the engine reads `/etc/dagger/engine.json` (a missing file uses defaults; unknown key means the engine does not start); engine resource metrics are off, enabled by `{"telemetry": {"resourceMetrics": true}}` and a **required** `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT` in the engine process environment (a generic `OTEL_EXPORTER_OTLP_ENDPOINT` alone does not enable it); protocol `OTEL_EXPORTER_OTLP_METRICS_PROTOCOL` beats `OTEL_EXPORTER_OTLP_PROTOCOL`. Separate meter provider from client telemetry. **[src]** https://github.com/dagger/dagger/blob/main/docs/current_docs/reference/config-files/engine-json.mdx (not fetched via the website URL, which I could not identify).
- Client side: `DAGGER_CLOUD_TOKEN` makes the CLI send traces to Dagger Cloud (`docs/current_docs/self-hosting/index.mdx`). Spans nest under `TRACEPARENT` injected into exec'd processes (`hack/designs/entrypoint-span-tree.md`, a design doc, not user docs).
- 2023 blog (OLD, > 18 months, re-verify): user-set `OTEL_EXPORTER_OTLP_TRACES_*` did not take effect in the engine container because the engine set its own. https://zerokspot.com/weblog/2023/01/31/dagger-otel-environment/

## Mechanism evidence

### (a) System-wide file at a fixed path

For:
- Used by git, Bazel, mise, uv, pip, Nix, Claude Code, Dagger engine. Root-owned location gives an operator layer the user cannot edit without privilege (Claude Code: managed file; Nix: `post-build-hook` only from global `nix.conf`). https://nix.dev/manual/nix/latest/command-ref/conf-file
- Works in a container image: the operator bakes a file into the image.
Against:
- By itself not enforcement: git system config is overridden by global, repo and `-c`; Bazel system rc is overridden by workspace/home/flags and skipped with `--nosystem_rc`; uv system config lowest priority and bypassed by `--no-config`. https://bazel.build/run/bazelrc, https://docs.astral.sh/uv/concepts/configuration-files/
- Path differs per OS and per tool; pip alone has three macOS variants and the XDG rules changed in 26.2. https://pip.pypa.io/en/stable/topics/configuration/
- On WSL, `/etc/claude-code` is user-writable (Claude Code docs, managed-settings page).
- Changing the path can need a rebuild (Bazel `BAZEL_SYSTEM_BAZELRC_PATH`).

### (b) Environment variable naming one config file

For:
- Cheap for CI and wrappers: `GIT_CONFIG_GLOBAL`, `UV_CONFIG_FILE`, `PIP_CONFIG_FILE`, `NPM_CONFIG_USERCONFIG`, `MISE_GLOBAL_CONFIG_FILE`. Gives a predictable environment (git docs: used "to create a predictable environment for a picky script").
- Replace-semantics are clear for git (level replaced, `/dev/null` skips).
Against:
- Replaces rather than layers: uv `--config-file` replaces all discovered files so user config is ignored. Operator then must own the whole file.
- An env var is controlled by whoever launches the process, so it is not enforcement. In pnpm the same mechanism (`PNPM_CONFIG_NPMRC_AUTH_FILE`) "disables the protection". https://pnpm.io/npmrc
- pip: only the user file is documented as skipped; whether global/site are skipped is unstated.

### (c) Environment variable naming an ordered list of files

Report per brief (separator / precedence / missing file / relative):

| Tool | Variable | Unix sep | Windows sep | Precedence | Missing file | Relative paths |
|---|---|---|---|---|---|---|
| kubectl | `KUBECONFIG` | `:` | `;` | first wins, never overwritten | skipped; warn only if all missing; explicit `--kubeconfig` missing errors **[src]** | not documented for the variable; relative resolved from cwd by inference; paths inside files resolve against the file's directory |
| Nix | `NIX_USER_CONF_FILES` | `:` | `;` | loaded in reverse order, so first listed wins | not stated for this variable; `include` errors unless `!include` | `include` relative to current file; variable form not stated |
| Bazel | `BAZELRC` | `,` (comma) on all OS, per docs | `,` | later wins (listed after home rc) | `--bazelrc` path missing: not stated in fetched text; `try-import` silent, `import` fails | `%workspace%` prefix for imports |
| git | none (single file per level) | - | - | - | - | - |
| cargo | none (`--config` repeatable on CLI instead) | - | - | left-to-right, later wins | `include` has `optional = true` | include relative to including file |

For:
- kubectl's list lets an operator ship one file and a user add their own, and a missing optional file does not break the command.
- Bazel's per-OS-independent comma avoids the `:`/`;` split.
Against:
- Three tools in the sample give three different precedence directions (kubectl first wins, Bazel later wins, Nix first listed wins via reverse load). https://kubernetes.io/docs/concepts/configuration/organize-cluster-access-kubeconfig/, https://bazel.build/run/bazelrc, https://nix.dev/manual/nix/latest/command-ref/conf-file
- Separator differs by OS for `KUBECONFIG` and Nix, which is the platform trap named in axis 7.
- Silent skip of missing files hides typos (kubectl warns only when all are missing).
- JS client issue: first-file `current-context` surprises: https://github.com/kubernetes-client/javascript/issues/579 (**[2nd]**).

### (d) Drop-in directory (`conf.d`)

For:
- Separate owners can each ship a file with no edit of a shared file; ordering by filename is explicit (systemd, Claude Code `managed-settings.d` with `10-telemetry.json` example). https://www.freedesktop.org/software/systemd/man/latest/systemd.unit.html, https://code.claude.com/docs/en/managed-settings
- Introspection exists in the reference tool (`systemctl cat`).
Against:
- Only two tools in the sample offer it (systemd as reference, Claude Code); git, cargo, uv, pip, mise, Bazel, npm do not.
- Needs an explicit list-reset rule (systemd empty assignment) or else lists only grow; Claude Code unions lists with dedupe, so a later file cannot remove an entry (managed-settings page).
- Name-collision shadowing across directories (systemd) is a documented gotcha.
- Claude Code: `managed-settings.d` is only read from the same source as the base file, and under `first-wins` a lower-ranked source's files are ignored without a warning.

### (e) Include directives inside a config file

For:
- git `include`/`includeIf`, cargo `include` (with `optional`), Nix `include`/`!include`, mise includes: lets the operator file pull in a fragment the user cannot edit, or a user file pull in a team file.
- Missing-file policy is explicit in Nix (`!include`) and cargo (`optional`).
Against:
- git: missing include silently ignored **[run]** and undocumented; `hasconfig:remote.*.url` includes cannot contain remote URLs (cycle prevention). https://raw.githubusercontent.com/git/git/master/Documentation/config.adoc
- Does not give enforcement: it sits inside a file the user may own. mise safe mode never fetches includes for project config (mise config page).
- Relative-path base differs: git/cargo/Nix relative to the including file; cargo env/`--config` relative to cwd.

### (f) Inline overrides through environment variables

For:
- Universal: `CARGO_*`, `PIP_*`, `UV_*`, `npm_config_*`, `MISE_*`, `NIX_CONFIG`, git `GIT_CONFIG_COUNT`. Needs no file in a container; wins over files in all tools sampled (git, cargo, pip, npm, uv, mise).
- git's counted form (`GIT_CONFIG_COUNT`) carries multi-valued keys and errors on a missing key or value.
- Claude Code: an operator who sets `OTEL_EXPORTER_OTLP_ENDPOINT` in managed `env` also *removes* developer per-signal variables, so env names stay a delivery format while precedence is enforced by policy.
Against:
- Beats files, so an env var set by the user or a parent process beats an operator file; not enforcement (pnpm docs: env can override project `.npmrc`).
- List separator differs per variable inside one tool (uv: space for `UV_INDEX`, comma for `UV_FIND_LINKS`); cargo documents no general array separator; pip uses spaces. https://docs.astral.sh/uv/reference/environment/, https://doc.rust-lang.org/cargo/reference/config.html
- Ambient values leak in: Claude Code ignores inbound `TRACEPARENT` interactively "to avoid picking up ambient CI or container values"; Docker's CLI endpoint uses a `DOCKER_CLI_` prefix rather than the generic OTel variable. https://code.claude.com/docs/en/monitoring-usage, **[src]** docker/cli `telemetry_docker.go`.
- npm: underscored `.npmrc` keys cannot be overridden by env.
- Inheritance: Claude Code does not pass `OTEL_*` to its children, otherwise instrumented child processes would export to the same collector.

### (g) Managed or remote policy (MDM, registry, signed artifact)

For:
- Only mechanism in the sample that is both above every user/project layer and OS-protected: Claude Code managed settings (server-managed, macOS profile, HKLM, root-owned file) with documented precedence and a `/status` line naming the source. https://code.claude.com/docs/en/managed-settings
- Enforced telemetry destination: developer-set endpoints removed at startup.
- mise `miserc` environments from `/etc/mise/miserc.toml` cannot be removed by a project file. https://mise.jdx.dev/configuration.html
Against:
- Delivery depends on the platform (plist, registry, file, server), and the behaviours differ per source: `first-wins` ignores lower sources silently; invalid entries drop or fall back to a stricter value (managed-settings page).
- WSL, HKCU and local admin rights weaken it ("What a developer can change"); even Claude Code has documented exceptions where a lower-level stricter value wins.
- Remote fetch adds a failure and cache mode: server-managed settings keep a local cache applied at startup.
- No signed or digest-pinned *config artifact* mechanism was found in any tool sampled (searched; none for git, cargo, uv, pip, npm, mise, Nix `nix.conf`). Nix flake `nixConfig` and Bazel remote config were not researched.

### (h) Command-line flags injected by a wrapper

For:
- Highest precedence in every tool sampled (`git -c`, `cargo --config`, Bazel flags, `--settings` in Claude Code below managed only).
- Works for one invocation with no state on disk (`claude --settings`, `uv --config-file`).
Against:
- Whoever owns the command line owns the value; a user can call the binary directly, so a wrapper is not enforcement. Bazel docs: command-line flags always beat rc files and `--ignore_all_rc_files` exists.
- Claude Code: `--settings` sits below managed; flags that pick a model only choose from managed-allowed ones. https://code.claude.com/docs/en/settings
- Requires the operator to control how the tool is launched (shims, `PATH` ordering), which is not documented as a pattern by any tool read.

## Incidents, complaints, CVEs (axis 7)

- **Claude Code, project settings redirected API traffic** — CVE-2026-21852: repo `.claude/settings.json` `env` could set `ANTHROPIC_BASE_URL`, key sent to attacker before the trust dialog; fixed by deferring network operations until consent (published 2026-01-21; also CVE-2025-59536 for hooks and MCP consent). https://research.checkpoint.com/2026/rce-and-api-token-exfiltration-through-claude-code-project-files/ (primary: vendor-independent researcher; fixed version not stated in the article, secondary sources say 2.0.65 — **not verified**).
- **Claude Code, per-signal OTLP variables bypassed managed collector** — before v2.1.217, per-signal endpoints in user settings or shell could redirect signals away from the managed collector. https://code.claude.com/docs/en/monitoring-usage
- **pnpm, repo `.npmrc` expanded `${ENV}` into registry destinations and credentials** — GHSA-3qhv-2rgh-x77r (CVE-2026-55180); fixed 10.34.2 and 11.5.3 per pnpm's blog; sources disagree on the affected range. https://pnpm.io/blog/2026/06/11/env-variables-in-repository-npmrc, https://osv.dev/vulnerability/CVE-2026-55180 (**[2nd]**, ranges unreconciled)
- **mise, local project config set its own trust keys** — GHSA-436v-8fw5-4mj8 / CVE-2026-35533: `.mise.toml` could set `trusted_config_paths`, `yes`, `ci`, `paranoid` before the trust check; introduced 2026.2.18, fixed 2026.6.4; CVSS 7.7. https://osv.dev/vulnerability/GHSA-436v-8fw5-4mj8 (**primary: advisory db**). Related trust-ordering bugs (task includes CVE-2026-55441, `github.credential_command` CVE-2026-55448, `.tool-versions` CVE-2026-33646) are **[2nd]** via vendor-database pages only.
- **git, repository-supplied config executes commands** — CVE-2022-24765: `git status` could run a command via a repo's `core.fsmonitor` in a directory owned by another user; fix is the ownership check plus `safe.directory`, and keys like `safe.bareRepository` honoured only in protected scopes. https://github.blog/engineering/git-security-vulnerability-announced/ (OLD, 2022, re-verify); https://raw.githubusercontent.com/git/git/master/Documentation/config/safe.adoc (current).
- **kubectl, first-file wins** — `current-context` of the first `KUBECONFIG` file wins; client tools break when switching context in a later file. https://github.com/kubernetes-client/javascript/issues/579 (**[2nd]**, age not checked).
- **pip / uv, env or config redirects the index** — `PIP_INDEX_URL`/`PIP_EXTRA_INDEX_URL` inherited from a base image silently beat `pip.conf`; `--extra-index-url` dependency confusion. https://dev.to/brabster/how-to-get-pwned-with-extra-index-url-462g (**[2nd]**), https://portswigger.net/daily-swig/dependency-confusion-attack-mounted-via-pypi-repo-exposes-flawed-package-installer-behavior (2021, OLD).
- **Separator drift** — `KUBECONFIG` and `NIX_USER_CONF_FILES` use `:` vs `;`; `BAZELRC` uses `,`; uv list variables use space vs comma; cargo documents no array separator. Sources as above (docs, not incident reports; no filed bug found).
- **Dagger, user OTEL variables did not apply in the engine container** — 2023 blog, OLD, re-verify. https://zerokspot.com/weblog/2023/01/31/dagger-otel-environment/

## Sources

- https://git-scm.com/docs/git-config — scopes, `GIT_CONFIG_COUNT`, include, protected configuration (first 100k chars read; variable list not read).
- https://raw.githubusercontent.com/git/git/master/Documentation/git.adoc — `GIT_CONFIG_GLOBAL`/`SYSTEM`/`NOSYSTEM` text.
- https://raw.githubusercontent.com/git/git/master/Documentation/config.adoc — include relative path; `hasconfig` limits.
- https://raw.githubusercontent.com/git/git/master/Documentation/config/safe.adoc — `safe.directory`, `safe.bareRepository`, protected-scope only.
- https://github.blog/engineering/git-security-vulnerability-announced/ — CVE-2022-24765 announcement (OLD; only seen via search result).
- https://kubernetes.io/docs/concepts/configuration/organize-cluster-access-kubeconfig/ — `KUBECONFIG` separators, first-wins, trust warning.
- https://raw.githubusercontent.com/kubernetes/client-go/master/tools/clientcmd/loader.go — missing-file skip, warn-if-all-missing, path resolution.
- https://github.com/kubernetes-client/javascript/issues/579 — first-file context complaint.
- https://bazel.build/run/bazelrc — rc order, `BAZELRC`, import/try-import, flags.
- https://bazel.googlesource.com/bazel/+/HEAD/docs/run/bazelrc.mdx — same doc via mirror; system rc path rebuild note (via search snippet).
- https://doc.rust-lang.org/cargo/reference/config.html — hierarchy, merge, include, env, `--config`.
- https://doc.rust-lang.org/cargo/reference/environment-variables.html — `CARGO_HOME`, array formats.
- https://github.com/rust-lang/cargo/issues/9301 — `cargo config get` unstable (via search snippet).
- https://mise.jdx.dev/configuration.html — hierarchy, merge, trust, miserc, introspection.
- https://mise.jdx.dev/configuration/settings.html — env mapping, global-only and env-only settings (first 100k of 229k chars).
- https://osv.dev/vulnerability/GHSA-436v-8fw5-4mj8 — mise local-settings trust bypass.
- https://www.sentinelone.com/vulnerability-database/cve-2026-55441/ — mise task-include trust bypass (not opened; via search).
- https://docs.astral.sh/uv/concepts/configuration-files/ — uv layers, merge, `--no-config`.
- https://docs.astral.sh/uv/reference/environment/ — `UV_CONFIG_FILE`, list separators.
- https://pip.pypa.io/en/stable/topics/configuration/ — pip layers, `PIP_CONFIG_FILE`, env.
- https://dev.to/brabster/how-to-get-pwned-with-extra-index-url-462g — env override pitfall (not opened).
- https://portswigger.net/daily-swig/dependency-confusion-attack-mounted-via-pypi-repo-exposes-flawed-package-installer-behavior — 2021 dependency confusion (OLD, not opened).
- https://docs.npmjs.com/cli/v11/configuring-npm/npmrc — npmrc files, auth scoping.
- https://docs.npmjs.com/cli/v11/using-npm/config — precedence, env naming.
- https://pnpm.io/npmrc — pnpm auth layers, repo-file protections.
- https://pnpm.io/blog/2026/06/11/env-variables-in-repository-npmrc — pnpm incident and migration.
- https://osv.dev/vulnerability/CVE-2026-55180 — pnpm CVE record (not opened; via search).
- https://docs.docker.com/reference/cli/docker/#environment-variables — `DOCKER_CONFIG`, precedence.
- https://docs.docker.com/engine/cli/otel/ — Docker CLI OTel enable.
- https://github.com/docker/cli (files `cli/command/telemetry_docker.go`, `telemetry.go`, `contrib/otel/README.md`) — endpoint from context metadata plus `DOCKER_CLI_` override; no `TRACEPARENT` code. Cloned 2026-11-18.
- https://code.claude.com/docs/en/monitoring-usage — OTel variables, managed pinning, repo-ignore, `TRACEPARENT`.
- https://code.claude.com/docs/en/settings — scopes, precedence, merge, exceptions, `/status`.
- https://code.claude.com/docs/en/managed-settings — delivery mechanisms, drop-in dir, managed-source combination.
- https://code.claude.com/docs/en/env-vars — `env` key notes (first 100k of 192k chars).
- https://research.checkpoint.com/2026/rce-and-api-token-exfiltration-through-claude-code-project-files/ — CVE-2026-21852, CVE-2025-59536.
- https://cli.github.com/manual/gh_help_environment — gh env variables.
- https://nix.dev/manual/nix/latest/command-ref/conf-file — nix.conf, `NIX_CONFIG`, include, `extra-`, trust.
- https://nix.dev/manual/nix/2.34/command-ref/new-cli/nix3-config-show — `nix config show` (via search; not opened).
- https://www.freedesktop.org/software/systemd/man/latest/systemd.unit.html — drop-in semantics.
- https://www.freedesktop.org/software/systemd/man/latest/systemctl.html — `systemctl cat`.
- https://github.com/dagger/dagger (files `docs/current_docs/reference/config-files/engine-json.mdx`, `docs/current_docs/self-hosting/index.mdx`, `hack/designs/entrypoint-span-tree.md`) — Dagger engine OTel, cloned 2026-11-18.
- https://zerokspot.com/weblog/2023/01/31/dagger-otel-environment/ — OLD 2023 blog (via search only).

negative:
- No tool in the sample offers a signed or digest-pinned config artifact; mechanism (g) in practice means MDM/registry/root-owned file or a vendor server.
- Docker CLI documents no admin layer and no fleet guidance for its OTLP endpoint; the destination lives in per-user context metadata plus one env override. Docker's source does not read `TRACEPARENT`.
- git has no operator-over-user lock at all; "protected configuration" only keeps repo files from setting some keys. `GIT_CONFIG_*` env vars are user-controlled.
- Bazel docs describe no enforcement; its system rc is overridable and skippable.
- uv system config and cargo have no enforcement; cargo has no system-wide file and no env var naming a config file.
- Only Claude Code (and mise partially, Nix partially) make an operator value non-overridable; of those, only Claude Code does so for a telemetry destination.
- `MISE_CONFIG_FILE` is not documented on the pages read (the brief assumed it); mise names `MISE_GLOBAL_CONFIG_FILE`/`MISE_SYSTEM_CONFIG_FILE`.
- Dagger earlier-404 was a bad docs URL; destination is `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT` in the engine environment, not the generic variable.
- No documented incident was found for Bazel workspace rc, cargo config in untrusted directories, or uv project config.

leads:
- Nix flake `nixConfig` and `accept-flake-config`: a repo-level file that can set nix.conf keys behind a prompt; the closest comparison to a project file touching security settings.
- Bazel `--bes_backend` / `--remote_cache` in rc layers: the build-system analogue of a telemetry destination; not verified here.
- Claude Code settings-reference "Variables Claude Code ignores in env": full list and off-values; only the summary was read.
- GitHub CLI telemetry and its config keys: not checked.
- Docker CLI plugins (buildx, compose) OTel and `TRACEPARENT` handling: only the core CLI was grepped.
- git `GIT_CONFIG_PARAMETERS` and how `-c` is propagated to child git processes: relevant to whether a wrapper's overrides survive subprocesses.
- Helm, terraform (`TF_CLI_CONFIG_FILE`, `.terraformrc` `provider_installation`, `TF_CLI_ARGS`) and gradle (`GRADLE_USER_HOME` init scripts): possible additional comparators for (b), (d) and (h); not researched.
- Re-verify the OLD sources: CVE-2022-24765 announcement, 2021 dependency-confusion article, 2023 Dagger blog, kubernetes-client/javascript#579.
