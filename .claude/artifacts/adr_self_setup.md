# ADR: `ocx self setup` — own shell scaffold in the binary

## Metadata

**Status:** Accepted
**Date:** 2026-06-04
**Deciders:** Michael Herwig (architect-driven)
**Beads Issue:** N/A (GitHub issue to be filed on acceptance)
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md` (Rust 2024, lib-hosts-substance)
- [ ] OR deviation justified in Rationale section
**Domain Tags:** infrastructure | devops | api
**Supersedes:** N/A (amends the activation-setup half of `handshake_toolchain_cli.md` / `adr_global_toolchain_tier.md`)
**Superseded By:** N/A

## Context

Shell integration for OCX is created **only** by the website install scripts
(`website/src/public/install.sh`, `install.ps1`). Each script does three jobs:

1. **Bootstrap** — download the release binary, verify checksum, then run
   `ocx --remote package install --select ocx.sh/ocx/cli:<version>` to pull ocx
   into its own CAS and set the `current` symlink (`install.sh:915`).
2. **Scaffold** — write per-shell env shims into `$OCX_HOME`
   (`env.sh`, `env.fish`, `env.ps1`, `env.nu`, `env.elv`), each a thin loader
   that delegates to `ocx self activate` at shell startup (`install.sh:383-618`).
3. **RC inject** — append a managed block to user profiles, idempotent via a
   literal `grep -qF "# BEGIN ocx"` (`install.sh:786-851`); `install.ps1`
   appends an `env.ps1` source-line to `$PROFILE` (`install.ps1:388`).

This creates three problems:

- **No bare-binary parity.** A user who downloads the binary from GitHub
  Releases (the standard answer to the "I will not pipe network shell into my
  interpreter" security objection) has no single command to reach the same
  state. They get a binary with no shell integration and no documented way to
  create it.
- **Scaffold logic is duplicated across two scripts and five shell dialects**,
  with install-time-substitution edge cases and a parallel maintenance burden.
  The `Shell` abstraction that would express this once already exists in
  `crates/ocx_lib/src/shell.rs` (10 shells, `export_path`/`export_constant`/
  `comment`/`escape_value`).
- **Stale-scaffold drift on update.** `ocx self update` is binary-swap-only
  (`tasks/update_check.rs::self_update` → `install_all(candidate=false,
  select=true)`); it never refreshes env shims or the RC block. The dynamic half
  rides the swap for free (env.sh re-invokes the *new* binary's `self activate`
  each shell), but a change to the static shim contract or the RC marker format
  silently never reaches existing users.
- **The RC marker is unversioned and edit-blind.** `# BEGIN ocx` / `# END ocx`
  with a literal grep cannot tell "ocx wrote this, unchanged" from "the user
  edited it", and cannot detect that the block's format changed across a
  release. It is also visually heavy for what is a one-line payload.

We want `ocx self setup`: a single command that (re)creates the full installed
state from a bare binary, that `self update` can safely re-run, and that manages
its RC footprint without clobbering user edits.

## Decision Drivers

- **Bare-binary parity** — download from GitHub → `ocx self setup` → identical state, no `curl | sh`.
- **Single source of truth** — scaffold generation in one place (Rust), not two shell scripts × five dialects.
- **Safe re-application** — idempotent, diff-gated, never silently overwrites user dotfile edits.
- **Backward compatibility** — already-installed users carry `# BEGIN ocx` blocks; migration must be seamless. The on-disk RC marker is a compatibility surface (a near-one-way door).
- **Boring technology budget** — prefer proven patterns (rustup/conda env-file indirection); spend innovation tokens deliberately.
- **Cross-platform uniformity** — Unix RC files and Windows `$PROFILE` should follow the same model.

## Industry Context & Research

**Research artifact:** inline (worker-researcher sweep, 2026-06-04). Key findings:

- **Env-file indirection is the dominant pattern** (rustup `~/.cargo/env`, uv
  `~/.local/bin/env`, deno `~/.deno/env`). The RC file holds one stable
  source-line; all volatile logic lives in the tool-owned env file. **OCX
  already implements this** (`env.sh` → `ocx self activate`), on both Unix and
  Windows. This is the single most important fact: the RC payload is *designed
  to never change across versions*.
- **Managed-block conventions:** conda's `# >>> conda initialize >>>` /
  `# <<< conda initialize <<<` fenced block with **full-replace-between-fences +
  diff-gate** is the most robust prior art. **No surveyed tool embeds a version
  or content hash in its marker.** A content hash for dirty-detection is novel
  (innovation token).
- **Dirty detection is rare.** Only pyenv refuses to overwrite when it detects
  pre-existing integration (via grep, not hash). rustup/uv/fnm/bun either
  double-inject or silently clobber. fnm and bun unconditionally append →
  duplicate PATH entries (known footgun).
- **Self-update side effects:** the prevailing norm is **binary-only**
  (rustup, deno, bun). uv's update re-running PATH setup is filed as a **bug**
  (astral-sh/uv#7319). The env-file indirection is *why* binary-only is safe —
  RC content is eternally stable, so nothing to refresh there.
- **Windows:** rustup/uv use `HKCU\Environment` + `REG_EXPAND_SZ` + broadcast
  `WM_SETTINGCHANGE`. **OCX deliberately does not** — it sources `env.ps1` from
  `$PROFILE`, the same indirection as Unix. This avoids the `REG_SZ`-vs-
  `REG_EXPAND_SZ` corruption class and the broadcast dance, and keeps setup
  reversible.

**Key insight:** because OCX already uses env-file indirection, the RC marker
is stable by construction. The marker's real jobs shrink to (a) idempotent
re-application, (b) **dirty detection** (the only feature with genuine marginal
value), and (c) not being ugly. This reframes Decision 3 below.

## Considered Options

This ADR records **four coupled decisions**. Options are enumerated per
decision; the Decision Outcome states the chosen combination.

### Decision 1 — Scaffold ownership boundary

**1A — Status quo (scaffold in shell scripts).** Keep env-shim and RC
generation in `install.sh`/`install.ps1`.

| Pros | Cons |
|------|------|
| No code movement | No bare-binary parity (core requirement unmet) |
| | Duplicated across 2 scripts × 5 shells; drift risk |
| | Latent single-source gap: env.sh hardcodes the store path as a literal while Rust computes it via `ocx_install_bin_path` |

**1B — Move scaffold into `ocx_lib`; expose `ocx self setup`; shrink scripts to bootstrap.** (CHOSEN) The scripts keep only what must exist before the binary does (platform detect, download, checksum) and then `exec`/`&` into `ocx self setup`. All shim + RC generation lives in `ocx_lib`, reusing the `Shell` abstraction.

| Pros | Cons |
|------|------|
| Bare-binary parity for free (it's the same command) | One-time migration of generation logic + its acceptance tests |
| One source of truth; `Shell` enum already models 10 shells | install scripts and Rust must agree on the bootstrap handoff contract |
| Closes the env.sh-literal-vs-computed-path drift gap | |
| Aligns with "lib hosts orchestration, CLI = thin wrapper" (MEMORY) | |

**1C — Shared template via hidden emit command.** Scripts call a hidden `ocx self emit-env-files`; RC injection stays in shell.

| Pros | Cons |
|------|------|
| Smaller Rust surface | Splits one concern across two languages — worst of both |
| | Still no clean bare-binary parity (RC half still shell) |

### Decision 2 — Self-install bootstrap (how the loose binary reaches the CAS)

`env.sh` resolves `_ocx_bin` to
`$OCX_HOME/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx`. (Confirmed correct:
`to_relaxed_slug` preserves `.`, so `ocx.sh` stays `ocx.sh`.) For setup to
produce a working state, *something* must populate that path. **There is no
lib-level API to inject a raw binary into the CAS** — every production
store-write path goes through an OCI pull (`install_all` → blob/layer/package
assembly → `wire_selection`). The only non-OCI path today is the test-mode
filesystem shortcut (`__OCX_TESTING_INSTALL_BINARY`) which bypasses the CAS.

**2A — Pull-from-registry (reuse `install_all`).** (CHOSEN as default) `self
setup` runs the existing self-update install path against
`ocx.sh/ocx/cli:<own-version>`, exactly as `install.sh:915` does today.

| Pros | Cons |
|------|------|
| Zero new CAS-integrity surface; reuses the proven path | Requires network + the running version published to the registry |
| Full OCI manifest/blob/digest verification | Re-fetches the canonical artifact (the user already has *a* binary) |
| The bare-GitHub user has network by definition (they just downloaded) — requirement met | Pure air-gapped sideload not served |

**2B — Self-copy `current_exe()` into the CAS.** Promote the test shortcut to a real "ingest running binary as a package" path: synthesize package root + `content/bin` + symlink + minimal metadata.

| Pros | Cons |
|------|------|
| Offline-capable; no re-download | Bypasses OCI manifest/digest → an unverified "loose" package |
| | New CAS-synthesis code; must reconcile with `self update`'s OCI expectations |
| | Two ways to populate the store = divergent invariants |

**2C — Two-mode (2A default, `--from-self` escape hatch).** Default pull-from-registry; `--from-self`/`--offline` falls back to 2B only when no registry is reachable.

| Pros | Cons |
|------|------|
| Covers air-gapped sideload | Carries 2B's complexity behind a flag |
| | YAGNI until a concrete air-gap requirement exists |

### Decision 3 — RC marker scheme, state machine, legacy migration

Because of env-file indirection (see Industry Context), the RC payload is a
single stable line: `. "$OCX_HOME/env.sh"` (POSIX), the elvish `eval (slurp …)`
variant, or the `$PROFILE` source-line on Windows. fish/nushell own dedicated
files (`conf.d/ocx.fish`, vendor autoload) — no marker, just rewrite the file.

**3A — Status quo fence + grep.** `# BEGIN ocx`/`# END ocx`, literal grep.

| Pros | Cons |
|------|------|
| No change | No format-upgrade detection; no dirty detection; visually heavy |

**3B — conda-style fence + full-replace + diff-gate.** Match `# >>> ocx … >>>` / `# <<< ocx … <<<`, replace whole block, write only if changed.

| Pros | Cons |
|------|------|
| Proven (conda); idempotent via diff-gate | Silently overwrites user edits inside the fence |
| | Still a 3-line block for a 1-line payload |

**3C — Single-line trailing marker with version + content hash.**
`. "$OCX_HOME/env.sh" # ocx:v1:<hash8>` where `<hash8>` is a short hash of the
canonical block body this ocx version emits. Drives a three-hash state machine.

| Pros | Cons |
|------|------|
| Cleanest visual — one self-describing line (directly fixes the "ugly" complaint) | Innovation token: no peer embeds a hash |
| Enables **dirty detection** (the user's explicit ask) | Hash discipline must be tested (canonical hash is compile-time embedded) |
| Enables format-upgrade rewrites from the marker line alone (no body read) | |

**3D — Fenced + version/hash in the BEGIN line.** (CHOSEN) `# >>> ocx v1 <hash8> >>>` … `# <<< ocx <<<`. conda robustness + drift/dirty detection.

| Pros | Cons |
|------|------|
| Future-proof for multi-line payloads | Heavier than 3C for a payload that is one line by design |
| Same detection power as 3C | |

**Three-hash state machine (3C / 3D):**

| `canonical` (embedded) | `marker` (parsed) | `actual` (block body) | State | Action |
|---|---|---|---|---|
| — | absent | — | fresh | write block |
| = | = | = | current | no-op (diff-gate) |
| ≠ | = | = | format upgraded | safe rewrite |
| any | present | ≠ marker | **dirty** (user edited) | warn; skip; `--force` to rewrite |

- `canonical` = hash of what *this* binary would write — compile-time constant,
  needs no file read. The upgrade decision (`canonical ≠ marker`) is a
  marker-line read only, matching the user's "not required to read the next
  line" intuition.
- `actual` = hash of the block body actually on disk — requires reading the
  body, but we read the file to find the marker anyway, so it is free. Dirty
  detection is the reason to read it.

**Legacy migration:** `self setup` recognises the legacy `# BEGIN ocx` /
`# END ocx` block (format v0) and the legacy `ocx shell init` lines and the old
extensionless `$OCX_HOME/env` reference (the strip logic already in
`install.sh:621-719`). On legacy detected → remove the old block + lines once,
write the new v1 marker. Idempotent thereafter.

### Decision 4 — self-update side-effect policy

**4A — Binary-only (status quo).** Update never touches shims or RC.

| Pros | Cons |
|------|------|
| Simplest; matches rustup/deno norm | Stale shim if the shim contract ever changes |

**4B — Update re-runs full setup (shims + RC).**

| Pros | Cons |
|------|------|
| Always fresh | Reproduces uv#7319: surprising RC edits in a non-interactive update; clobber risk |

**4C — Split: update rewrites ocx-owned shims, never touches user RC; advises if the contract changed.** (CHOSEN) `self update`, after the binary swap, idempotently rewrites `$OCX_HOME/env.*` (ocx owns them, no user edits expected). It never modifies user RC profiles. If the RC indirection contract version advanced, it prints one line: `run 'ocx self setup' to refresh shell integration`.

| Pros | Cons |
|------|------|
| Dynamic logic already rides the swap (env.sh → new `self activate`) | A rare RC-contract change needs an explicit user action |
| Static shims refreshed safely (ocx-owned, diff-gated) | |
| Sidesteps uv#7319 entirely; matches research norm | |

## Decision Outcome

**Chosen combination:** **1B + 2A + 3D + 4C** (2C `--from-self` deferred).

**Rationale:**

- **1B** is the only option that delivers bare-binary parity, and it collapses
  five shell dialects × two scripts into one Rust generator over the existing
  `Shell` abstraction. It aligns with the project's "lib hosts orchestration,
  CLI is a thin wrapper" rule.
- **2A** reuses the exact bootstrap path the installer already runs; it adds no
  CAS-integrity surface and serves the actual user (bare-GitHub download → has
  network). Air-gapped sideload (**2C `--from-self`**) is deferred to a
  follow-up until a concrete requirement exists (YAGNI).
- **3D** (conda-style fence `# >>> ocx v1 <hash8> >>>` … `# <<< ocx <<<`) gives
  the same drift + dirty detection as 3C while staying future-proof for a
  multi-line payload, and reuses the most battle-tested managed-block convention
  in the survey (conda). The marker carries a version (`v1`) for format-upgrade
  decisions plus a content `<hash8>` for edit-detection; the state machine below
  is identical to 3C's. **Fallback:** if review judges the content hash to be
  YAGNI, degrade `<hash8>` to a bare version tag — this keeps format-upgrade
  rewrites but drops dirty detection.
- **4C** keeps the safe, prevailing binary-only posture for user dotfiles while
  still refreshing the files ocx fully owns. It is the direct lesson of
  uv#7319.

### Consequences

**Positive:**
- `ocx self setup` is the documented, scriptable, `curl`-free install completion.
- Scaffold generation has one home, one test surface, one shell abstraction.
- RC management gains format-upgrade and edit-safety it never had.
- Windows and Unix converge on one model and one command.

> **Correction (item 17):** an earlier "Positive" claimed `ocx self setup` *closes
> the env.sh literal-path-vs-computed-path drift gap*. That cannot materialize:
> the env.* shims stay **byte-identical with no install-time substitution**, so
> the shim continues to embed the literal store path (`$OCX_HOME/symlinks/.../bin`).
> Honest note: `ocx_install_bin_path` is lifted to `ocx_lib` only for the
> bootstrap / computed-path uses (and the identifier-dedup of item 12) — NOT for
> the shim bytes. No drift gap is closed by this feature.

**Negative:**
- Generation logic and its acceptance tests migrate from shell to Rust (mechanical but broad).
- install scripts and `ocx self setup` must agree on a bootstrap-handoff contract.
- A new compatibility surface (the v1 marker) must be carried forward forever.
- **`self update`'s shim refresh (Decision 4C) is NOT side-effect-free.** It is
  safe for *user RC* (never touched), but **destructive-by-design for the
  ocx-owned `$OCX_HOME/env.*` files**: it overwrites them to the current
  canonical bytes WITHOUT warning, discarding any manual edits a user made to
  those files. This is intended (ocx owns env.*), but it must be stated plainly
  rather than dressed up as "safe" — the setup success message says so.
- **Dedicated-shell files are ocx-owned, like `env.*`.** fish `conf.d/ocx.fish`
  and nushell `vendor/autoload/ocx.nu` live in tool-managed auto-load dirs and
  are full-rewritten on drift (diff-gated, no fence, no `--force` gate). The
  no-clobber-without-`--force` bar applies ONLY to the managed block inside a
  user's OWN RC files. (Cross-model review 2026-06-04 flagged the asymmetry;
  resolution: documented as intended ownership, mirroring conda/rustup vendor
  files.)

**Risks:**
- *Clobbering user dotfiles.* Mitigation: atomic writes, diff-gate, dirty-state warn-and-skip, `--dry-run`, `--no-modify-path` parity with the env var.
- *Bootstrap chicken-and-egg* (setup runs from a not-yet-installed binary). Mitigation: 2A populates the CAS first, then writes shims pointing at `current`; the loose binary is single-use.
- *Marker parse edge cases* (user reflows the line, CRLF, multiple blocks). Mitigation: tolerant regex + golden tests; on ambiguity, treat as dirty and skip.
- *Hash-discipline regressions.* Mitigation: a unit test asserting the embedded canonical hash equals the hash of the emitted block.
- *Bootstrap installs the registry's live latest-published tag, not a release-bound immutable digest* (cross-model review 2026-06-04). The store install is OCI-content-addressed (verified blobs), but Decision 2A resolves "latest" at setup time, so a moved tag / publish skew between the install-script checksum-verify and the registry resolve can change what lands. Accepted for v1 (2A + the never-pin rule); the digest-pin / signed release→OCI mapping is tracked as follow-up #150.

## Technical Details

### Architecture (C4 Component)

```
install.sh / install.ps1  (shrinks to bootstrap only)
  detect platform → download release binary → verify checksum
      └─ exec ──▶ ocx self setup
                     │
crates/ocx_cli/command/self_group/setup.rs   (thin: parse flags, take Context)
                     │  context.manager()…  + ocx_lib::setup orchestration
                     ▼
crates/ocx_lib/src/setup/                     (NEW module — single source of truth)
  ├─ bootstrap   → install LATEST PUBLISHED ocx.sh/ocx/cli via check_update → install_all   [Decision 2A]
  ├─ shims       → write $OCX_HOME/env.{sh,fish,ps1,nu,elv}      (byte-identical const templates; reuse Shell)
  └─ rc_block    → per-profile state machine (Decision 3D) + legacy migration
                     ▲
crates/ocx_cli/command/self_group/update.rs   → after swap: setup::refresh_shims() only   [Decision 4C]
```

> **Note (Decision D2):** `$GITHUB_PATH` / CI sink stays in the install scripts
> (`install.sh:1250 export_github_path`) — NOT folded into `ocx self setup`. The
> canonical CI env channel is already `ocx env --ci` / `ocx package env --ci`
> (`adr_ci_env_export_flag.md`); duplicating it here would couple scaffold
> generation to CI detection. Removed from the diagram accordingly.

### CLI Contract

```
ocx self setup [--no-modify-path] [--profile <path>...] [--dry-run] [--force] [--from-self]
  --no-modify-path : write shims only; skip RC injection (parity with OCX_NO_MODIFY_PATH)
  --profile <path> : explicit target profile(s); default = auto-detected set
  --dry-run        : report intended actions; touch nothing
  --force          : overwrite a dirty RC block (state = dirty)
  --from-self      : DEFERRED — Decision 2C; NOT declared in v1 (no clap flag emitted)

Placement: SelfGroup::Setup, added to app.rs should_check_for_update skip list.
Context:   full Context (needs PackageManager for the pull) — like `self update`, unlike `self activate`.
Exit codes: sysexits-aligned (ExitCode enum). Dirty-skip without --force → ExitCode::DirtyRcBlock (82),
            a NEW variant (plan D3 — reserve-private-range slot above NotFound/AuthError/OfflineBlocked).
            Dirty-skip is an `Ok` outcome carried in the report data, NOT an error; the CLI decides 82 by
            inspecting the outcome, routed OUTSIDE classify.rs (classify.rs maps error subtrees only).
```

### RC marker (v1, conda-style fence — Decision 3D)

```sh
# >>> ocx v1 a1b2c3d4 >>>
. "$OCX_HOME/env.sh"
# <<< ocx <<<
```

- Fence header carries `v1` (format version, for upgrade decisions) and
  `a1b2c3d4` (content hash of the block body, for dirty detection). Body is
  full-replaced between fences; `actual` hash recomputed from the body lines.
- elvish: same fence around `eval (slurp < "$OCX_HOME/env.elv")`.
- Windows `$PROFILE`: same fence (PowerShell `# >>> … >>>` / `# <<< … <<<`
  comment lines) around an `$env:OCX_HOME`-rooted source line (item 4 — honors
  an `OCX_HOME` override, consistent with the POSIX body that also keys off
  `$OCX_HOME`). PowerShell has no `:=` default-assign, so the body computes a
  local fallback for an unset `OCX_HOME`:

  ```powershell
  $_ocxHome = if ($env:OCX_HOME) { $env:OCX_HOME } else { "$env:USERPROFILE\.ocx" }
  if (Test-Path "$_ocxHome\env.ps1") { . "$_ocxHome\env.ps1" }
  ```

  (Earlier draft used a hardcoded `$env:USERPROFILE\.ocx\env.ps1` — superseded by
  the `$env:OCX_HOME`-with-fallback form so an override-driven install works.)
- fish / nushell: dedicated file, full-rewrite, no inline fence.

### Data / invariants

- env.* shim content is a compile-time `const`/`include_str!` template — **no install-time substitution** (preserves the byte-identical-across-users invariant; `$OCX_HOME` resolved at runtime by the shim).
- Canonical hash is computed in Rust from the same const → testable equality.
- Multi-profile targeting preserved (`.bash_profile`/`.profile`, `.bashrc`, `.zprofile`, `.zshrc`) — both login and interactive, per the existing rationale at `install.sh:727`.

## Implementation Plan

1. [ ] `ocx_lib::setup` module: shim templates as consts; `Shell`-driven RC block emit; canonical-hash fn.
2. [ ] RC state machine (Decision 3D) + legacy (`# BEGIN ocx`, `ocx shell init`, extensionless `env`) migration; atomic per-profile writes.
3. [ ] `bootstrap` step: resolve the LATEST PUBLISHED release via `check_update` then `install_all(candidate=false, select=true)` (Decision 2A) — NOT a pin on the running binary's exact version (avoids the build-timestamp-pin anti-pattern; the loose binary is single-use).
4. [ ] `ocx self setup` CLI (SelfGroup variant, flags, skip-list, Context wiring, exit codes).
5. [ ] `self update` → `setup::refresh_shims()` post-swap; contract-version advisory (Decision 4C). On a `refresh_shims` error, fire the `run 'ocx self setup'` advisory regardless of contract-version bump.
6. [ ] Slim `install.sh` / `install.ps1` to bootstrap + handoff; delete migrated generators.
7. [ ] Migrate acceptance tests (`test_install_sh.py`, relevant `test_self_activate.py`) to drive `ocx self setup`; add Rust unit tests (canonical-hash equality, state-machine table, byte-identical shims).
8. [ ] Docs (see Validation).

## Validation

- [ ] Rust unit tests: canonical-hash == hash(emitted block); state-machine truth table; shim byte-identity across `$OCX_HOME` values.
- [ ] Acceptance tests: fresh setup; idempotent re-run (no-op); format-upgrade rewrite; dirty-skip + `--force`; legacy-block migration; `--no-modify-path`; `--dry-run`.
- [ ] Cross-platform: POSIX shells, fish, nushell, elvish, Windows `$PROFILE` (WinPS 5.1 + pwsh 7 profile-path divergence handled in Rust).
- [ ] Security review (new attack surface: writing user dotfiles + ingesting a binary). Handoff to `/security-auditor`.
- [ ] **Documentation surfaces** (per "plans must list docs"):
  - `website/src/docs/reference/environment.md` — `OCX_NO_MODIFY_PATH` (any-non-empty → true; must be repeated per-invocation), `OCX_HOME`, profile-indirection limitation (PATH only inside PowerShell/POSIX sessions, not cmd.exe/GUI), setup semantics
  - `website/src/docs/user-guide.md` — install/activation + a "bare-binary (no `curl | sh`) setup" path
  - `website/src/public/install.sh`, `install.ps1` — slimmed; comments updated
  - `ocx self setup --help` text — per `quality-cli-help.md`
  - `.claude/rules/subsystem-cli.md` — add `self setup` to the `ocx self` taxonomy
  - `.claude/rules/arch-principles.md` — "Where Features Land" + activation/setup model note
  - `.claude/rules/product-context.md` — canonical identity file (item 19). This ADR motivates a sharpened
    **"Why not just `curl | sh`?"** rebuttal: download the binary from GitHub Releases + run `ocx self setup`
    for identical state — bare-binary parity for the security-conscious CI persona who refuses to pipe
    network shell into an interpreter. Worth a "Why Not Just..." row update on acceptance.
  - cross-check `handshake_toolchain_cli.md` / `adr_global_toolchain_tier.md` activation wording

## Future Work / Out of Scope

**Live in-session env reload after a global-toolchain mutation** — sibling ADR
`adr_live_env_reload.md` (to be drafted). Today, after `ocx --global add … &&
ocx --global lock`, the *running* interactive shell does not see the new
package until the profile is re-sourced (a child process cannot mutate its
parent shell's environment). Two cases:

- **First install via `curl | sh`** — the pipe runs in a subshell; the
  interactive shell is unreachable. Inherent, not fixable (rustup/uv have the
  same one-time "source your profile" step). `ocx self setup` should print a
  one-time hint at the end. **In scope for this ADR** (a print line).
- **Global mutation in a live shell** — fixable. Preferred approach: a
  `precmd`/prompt hook with an `$OCX_HOME/ocx.lock` **mtime staleness** check
  (direnv pattern) — catches mutations from any source, no shell arg-parsing,
  one `stat` per prompt. The reload must **recompose** (conda pattern: stash
  base in `OCX_PATH_BACKUP` at first activate, restore + re-prepend on reload),
  not merely prepend, so `ocx remove`/downgrade cleans the live PATH. Requires
  adding a deactivate/backup capability to `ocx self activate`. **Out of scope
  here** — distinct decision (runtime behavior vs initial scaffold).

**Coupling to this ADR:** the live-reload feature ships by changing the
`env.sh`/`env.ps1` shim. The **versioned shim contract (Decision 4C)** is its
delivery vehicle — `ocx self setup` rewrites the shims and the contract-version
advisory tells existing users to refresh. This ADR's machinery is deliberately
sufficient to carry that later change without re-touching user RC files.

Caveat for the sibling ADR: auto-reload on every prompt can surprise (clobber
in-session manual env edits, shift PATH precedence). Scope the reload to
ocx-managed keys and/or make it opt-in.

## Links

- [`subsystem-cli.md`](../rules/subsystem-cli.md) — `ocx self` group, `--shell` convention, skip-list
- [`subsystem-package-manager.md`](../rules/subsystem-package-manager.md) — `self_update`, `install_all`, throttle
- [`subsystem-file-structure.md`](../rules/subsystem-file-structure.md) — SymlinkStore `current`, slug rules
- [`adr_global_toolchain_tier.md`](./adr_global_toolchain_tier.md) — activation via `$OCX_HOME/env.sh`
- [`handshake_toolchain_cli.md`](./handshake_toolchain_cli.md) — current CLI authority
- rustup `src/cli/self_update/` · conda `core/initialize.py` · astral-sh/uv#7319 (update side-effect bug)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-06-04 | Architect (Michael Herwig) | Initial draft — four coupled decisions (scaffold ownership, bootstrap, RC marker state machine, update policy) |
| 2026-06-04 | Architect (post-review) | Amendments from review panel: bootstrap = latest-published (not version-pin); exit code 82 `DirtyRcBlock` (was "e.g. 78"); PowerShell fence body uses `$env:OCX_HOME`-with-fallback (was hardcoded `$env:USERPROFILE`); removed `ci_sink` from C4 diagram (D2); struck the false "drift-gap closes" consequence; 4C shim refresh stated as destructive-by-design for env.* + advisory-on-refresh-error; `--from-self` deferral note; added `product-context.md` to docs + "Why not curl\|sh" differentiator |
