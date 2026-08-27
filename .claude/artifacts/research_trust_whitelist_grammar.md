# Research: Trust whitelist grammar and distribution channels

**Date:** 2026-08-24
**Axis:** 2 of 4 — env overhaul ADR (`brief_env_overhaul.md`, "Owner decisions")
**Consumed by:** `adr_shell_env_overhaul.md`

Constraint set: whitelist lives in user `config.toml` only (never `ocx.toml`); the managed/fleet
tier may ship it; an env var must also work (devcontainer pre-whitelist).

## direnv

- Grammar (`direnv.toml [whitelist]`), two independent lists, OR'd:
  - `prefix = [...]` — string-prefix match against the **absolute path of the `.envrc`**. Any
    listed string that is a literal prefix implicitly allows it, regardless of content or prior
    `allow`/`deny`. `prefix = ["/home/user/project-a"]` also matches `/home/user/project-a-evil`
    — classic prefix footgun, no trailing separator enforced.
  - `exact = [...]` — a directory name (implicitly `+ /.envrc`) or a full file path, matched
    exactly. No footgun, no subdirectory implication.
  [direnv.toml(1)](https://direnv.net/man/direnv.toml.1.html)
- Separately the **allow store** (not the whitelist) is content-addressed: `direnv allow` writes a
  file under `~/.local/share/direnv/allow/` named `sha256(path + "\n" + contents)`. Editing an
  allowed `.envrc` changes the hash and re-prompts. So whitelist and allow-store are two different
  trust mechanisms — whitelist is a static, content-blind bypass; allow-store is drift-sensitive
  but per-user/per-machine and not shippable as config.
  [Discussion #1092](https://github.com/orgs/direnv/discussions/1092)
- **No env-var channel** for the whitelist — `direnv.toml`-only, so a devcontainer must write that
  file. This is precisely the gap that makes direnv unable to answer the devcontainer use case.
  [direnv.1](https://direnv.net/man/direnv.1.html)

## mise

- `trusted_config_paths` — `string[]`, env `MISE_TRUSTED_CONFIG_PATHS`, default `[]`.
  **Prefix-directory semantics**: config files under these paths are trusted without prompting.
  `["/"]` trusts everything. Env form is OS-PATH-separator-joined (`:` Unix, `;` Windows).
  [Settings](https://mise.jdx.dev/configuration/settings.html)
- General config precedence: walks the tree up to `MISE_CEILING_PATHS`, merging with
  **closest-to-cwd wins** (most-specific-wins, not union).
  [Configuration](https://mise.jdx.dev/configuration.html)
- `mise trust [CONFIG_FILE]`: `-a/--all` trusts cwd + parents + subdirs; `--untrust` revokes;
  `--ignore` blacklists; `--show` lists status. No content-hash re-trust by default.
  [trust](https://mise.jdx.dev/cli/trust.html). **Paranoid mode** (`MISE_PARANOID=1`) adds
  content hashing — modified trusted files require re-trusting. Global/system configs stay
  implicitly trusted even under paranoid. [Paranoid](https://mise.jdx.dev/paranoid.html)
- **GHSA-436v-8fw5-4mj8 / CVE-2026-35533** (High, ≥2026.2.18 – <2026.6.4): `Settings::try_get()`
  preloads the **local project's own `.mise.toml`** before `trust_check()` runs, so a
  project-committed `trusted_config_paths = ["/"]` (or `yes=true`/`ci=true`) makes the untrusted
  file trust itself, reaching `[env] _.source`, templates, hooks and tasks. Fix: never honor
  trust-control keys from non-global config.
  [Advisory](https://github.com/jdx/mise/security/advisories/GHSA-436v-8fw5-4mj8)
- Sibling advisory, same class: **CVE-2026-33646** — `.tool-versions` files are run through the
  Tera template engine (with `exec()` registered) during parsing and are **not subject to trust
  verification at all** in non-paranoid mode: arbitrary command execution on `cd`, zero prompt.
  Distinct bug (missing gate, not bypassed gate), same lesson: every project-committed file format
  needs the *same* trust gate, none exempted by format.
  [GitLab Advisory DB](https://advisories.gitlab.com/cargo/mise/CVE-2026-33646/)
- The owner's `config.toml`-only decision structurally forecloses the GHSA-436v-8fw5-4mj8 class:
  if the project's own file is never a candidate source for trust-control keys, there is no
  preload-ordering bug to have.

## git `safe.directory`

- `safe.directory = *` is the literal documented full opt-out. Otherwise each entry is one
  directory (repeatable multi-value key), **no glob support** — `/workspaces/*` does not work;
  every repo path is listed individually.
  [safe.adoc](https://raw.githubusercontent.com/git/git/master/Documentation/config/safe.adoc) ·
  [capistrano#2109](https://github.com/capistrano/capistrano/issues/2109) ·
  [Ken Muse](https://www.kenmuse.com/blog/avoiding-dubious-ownership-in-dev-containers/)
- **"Only respected in protected configuration"** — system + global + `-c`/env, **never repo-local
  `.git/config`**. Exactly the pattern chosen for OCX's whitelist.
- No dedicated env var; injected via `-c safe.directory=...` or `GIT_CONFIG_*`.
  `GIT_CEILING_DIRECTORIES` is unrelated (stops upward `.git` search, not a trust decision).
- Devcontainer pattern for a path unknown at image-build time: **`postStartCommand` /
  `postAttachCommand`**, not `postCreateCommand` — the checkout must already exist.
  `"postStartCommand": "git config --global --add safe.directory ${containerWorkspaceFolder}"`.
  [Ken Muse](https://www.kenmuse.com/blog/marking-workspaces-safe/) ·
  [Codespaces security](https://docs.github.com/en/codespaces/reference/security-in-github-codespaces)

## VS Code Workspace Trust

- Settings: `security.workspace.trust.enabled` (default true), `.startupPrompt` (default `never`),
  `.emptyWindow`, `.untrustedFiles`, `.banner`; plus `extensions.supportUntrustedWorkspaces`.
  [Docs](https://code.visualstudio.com/docs/editing/workspaces/workspace-trust)
- **Parent-folder trust implies all subfolders trusted**, unconditionally — no documented carve-out
  for an untrusted child under a trusted parent.
- Storage format/location of the trusted-folders list is **not publicly documented**; live feature
  request [vscode#279653](https://github.com/microsoft/vscode/issues/279653) asks for a JSON form
  precisely because no user-editable file exists — so a devcontainer feature cannot pre-seed it by
  dropping a config file (contrast direnv and git, both one append away). Path-keyed only, no
  content hash, so no self-invalidation on drift.
- Managed/remote environments (Codespaces, attached containers) are auto-trusted by policy, not by
  a whitelist entry anyone declares.

## Comparison

| Tool | Grammar | Tiers | Precedence | Env channel | Subdir semantics | Drift handling |
|---|---|---|---|---|---|---|
| direnv | `prefix` (footgun) + `exact` in `direnv.toml [whitelist]` | one file, no fleet tier | n/a | none | `prefix` implies descendants; `exact` does not | whitelist: none. Separate allow-store: SHA256(path+contents), re-prompts on edit |
| mise | `trusted_config_paths` prefix list | project / user / system, walked to `MISE_CEILING_PATHS` | closest-to-cwd wins — this is what let a project's own file win, pre-fix | `MISE_TRUSTED_CONFIG_PATHS`, OS-PATH-sep list | prefix implies descendants | none by default; paranoid adds content hash |
| git `safe.directory` | exact path per entry; `*` = full opt-out; **no glob** | system / global / `-c`-env only; repo-local never consulted | union across protected tiers | via `-c` or `GIT_CONFIG_*` | none — every path listed | none (static) |
| VS Code | opaque internal list, UI/API-managed | single user-level list | n/a | none documented | parent trust implies all subfolders | none documented |
| Codespaces/devcontainer | n/a — policy-level auto-trust of the environment | n/a | n/a | n/a | n/a | n/a |

## Threat model

1. **Prefix vs exact vs glob.** Every tool shipping a prefix grammar (direnv, mise) has the same
   footgun: `/home/user/project` also matches `/home/user/project-evil`, and anyone who can get a
   sibling directory created under a trusted prefix (temp checkout, symlink, reused CI workspace)
   inherits trust free. `exact` and per-path enumeration close it at one entry per repo. **No tool
   surveyed ships true glob** — git explicitly documents that it does not, and the devcontainer
   workaround is a `postStartCommand` that writes an exact entry per session.
2. **Precedence: override vs union.** mise's most-specific-wins is exactly the shape that became
   GHSA-436v-8fw5-4mj8 once a local tier could write trust-control keys — override precedence is
   dangerous precisely when the override authority and the config author are the same untrusted
   party. git sidesteps it by **unioning** system+global (both operator-controlled) and refusing
   repo-local input outright. Safest for OCX: **union across `config.toml` tiers, no precedence
   logic at all**, because `ocx.toml` is structurally excluded from the union.
3. **Env var channel.** mise's shape (`MISE_TRUSTED_CONFIG_PATHS`, OS-PATH-separator list) is the
   directly reusable precedent for a devcontainer pre-whitelisting a checkout whose final path is
   unknown at build time. direnv has no env channel at all, which is why it cannot answer this
   case. **None** of the surveyed tools treat "hostile parent process sets this env var" as
   in-scope — all assume the process env is already trusted context, the same boundary as the
   shell profile. Consistent with OCX's existing shell-activation trust boundary.
4. **Subdirectory implication.** direnv `exact` and git `safe.directory` require one entry per repo
   and imply nothing; direnv `prefix`, mise, and VS Code parent-trust all imply descendants,
   trading ergonomics for the footgun in (1). For OCX's target user (fleet trusting internal
   namespaces) descendant implication is the *product requirement*, not a mistake — and the
   mitigation is to do it at the **OCI-namespace level** rather than filesystem path prefix, which
   removes the sibling-typosquat vector entirely because there is no filesystem to typosquat into.
5. **Revocation/drift.** Only direnv's allow-store and mise's paranoid mode re-check content. Every
   default-mode whitelist surveyed is static: once listed, no re-prompt on content change, only on
   path change. This matches OD-3 in the brief — industry-standard tradeoff, not an OCX gap. To
   beat the field, mise's paranoid pattern (opt-in content hash over a static list) is the
   precedent, not a default to ship.
6. **Other advisories.** CVE-2026-33646 is the instructive sibling: the lesson is not about
   grammar but "every parseable project file needs the same trust gate." Relevant because
   `ocx.toml` — excluded from ever holding whitelist entries — must not later gain some *other*
   ungated path to execution (a build hook, a templated `[env]` value) that bypasses the consent
   stamp. Nothing else in this class turned up beyond the mise pair and the git
   `safe.directory` CVE-2022-24765 background.

## Recommendation

**Grammar — OCI-namespace prefix, not filesystem path.** Key the whitelist on registry/namespace
strings (`ocx.sh/acme-corp/*`), not filesystem paths. The brief's own example ("fleet pre-trusts
internal namespaces") already implies this, and it sidesteps the prefix footgun: there is no
sibling-directory typosquat against an operator-controlled OCI namespace. Ship **one** grammar
(glob-suffix namespace prefix), not direnv's two-list split — the namespace framing removes the
reason `exact` exists.

**Precedence — union, never override.** Follow git exactly: entries from user `config.toml` and
managed `config.toml` are unioned. Free to get right today because `ocx.toml` is structurally
excluded, so there is no untrusted tier in the union. Do not add conditional precedence logic that
is not needed, and do not let a future third tier introduce override semantics without revisiting.

**Env var — `OCX_TRUST_WHITELIST`, OS-PATH-separator list, additive union** with the config tiers
(not a replacement, not higher precedence). Mirrors `MISE_TRUSTED_CONFIG_PATHS`, and answers the
devcontainer case via `onCreateCommand`. A hostile parent process setting it is out of scope,
consistent with every surveyed tool.

**Do not build** content-hash drift re-checking on the whitelist itself. Ship the static-list
default; defer hash-based re-confirm to OD-3, and even then scope it to the consent stamp rather
than the whitelist grammar.
