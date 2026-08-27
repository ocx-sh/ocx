# Research: Shell integration rollout vs version lag

**Date:** 2026-08-24
**Axis:** 3 of 4 — env overhaul ADR (`brief_env_overhaul.md` item 4)
**Consumed by:** `adr_shell_env_overhaul.md`

## Per-tool findings

### conda — versioned managed block, blind overwrite, no compat contract

`conda init` writes a fenced block into rc files:

```sh
# >>> conda initialize >>>
# !! Contents within this block are managed by 'conda init' !!
...
# <<< conda initialize <<<
```

`conda/core/initialize.py` finds the block by **regex on the fence markers only** — no version
number embedded — and blind-replaces in place on every run:

```python
replace_str = "__CONDA_REPLACE_ME_123__"
rc_content = re.sub(CONDA_INITIALIZE_RE_BLOCK, replace_str, rc_content, flags=re.MULTILINE)
rc_content = rc_content.replace(replace_str, conda_initialize_content)
```

**No staleness detection at all**: a shell that sourced an old block before a `conda update conda`
keeps running that logic until the user manually re-runs `conda init`. No old-block/new-binary
contract is stated; it mostly works because the block is thin.

The line-based, non-syntax-aware replace has a real failure mode:
[conda/conda#11030](https://github.com/conda/conda/issues/11030) — `conda init` comments out a line
unconditionally, including one nested inside a user's shell function, breaking it. That is the risk
of overwriting a fenced region without parsing shell syntax. `conda init --reverse` removes the
block by the same marker regex regardless of content version.

[conda init](https://docs.conda.io/projects/conda/en/latest/commands/init.html) ·
[activation deep-dive](https://docs.conda.io/projects/conda/en/stable/dev-guide/deep-dives/activation.html)

### rustup — deliberately near-empty stub; nothing to version

`~/.cargo/env` is a ~10-line POSIX script written once at install:

```sh
case ":${PATH}:" in
    *:"{cargo_bin}":*) ;;
    *) export PATH="{cargo_bin}:$PATH" ;;
esac
```

That is the whole hook — a static PATH-prepend guard. All real logic (toolchain selection, override
resolution) lives in the binaries, invoked fresh per command. Because the stub does one static thing,
rustup never needed a versioning or staleness story, and `rustup self update` does not rewrite it.
[env.sh template](https://raw.githubusercontent.com/rust-lang/rustup/master/src/cli/self_update/env.sh)

### mise — two shapes side by side, tradeoff reasoned explicitly

`mise activate <shell>` prints a snippet. For every shell **except nushell** the snippet installs a
per-prompt hook re-invoking the binary (`hook-env`) — the thin-stub shape. A mise upgrade therefore
takes effect at the next prompt with zero rc-file change.

Nushell is the deliberate exception: no `eval`, so mise **statically generates** a config file.
Stated tradeoffs for going static: saves ~10 ms of shell startup, is pure (no dependency on a file
outside the store), follows existing nushell conventions. Notably absent: any staleness or
versioning story — the file serves stale logic until the generator is re-run, the same exposure
class as conda's block, scoped to the one shell that cannot `eval`.
[activate](https://mise.jdx.dev/cli/activate.html) ·
[home-manager#7829](https://github.com/nix-community/home-manager/pull/7829)

### starship / zoxide / atuin / fnm / pyenv / Homebrew — the eval-pattern family

All six ship `eval "$(tool init shell)"` (or piped `source`), re-invoking the binary **every shell
start** to regenerate the integration fresh. [starship#2637](https://github.com/starship/starship/issues/2637)
debated dropping `--print-full-init` specifically because it invoked the binary twice during init;
the proposed fix was native shell substitution per shell, not dropping the eval. Starship renders in
<10 ms, so the per-start cost is negligible — the debate was about the *second* invocation.

**Propagation cost for a logic change: zero rc-file changes, ever.** A binary upgrade changes what
the next `tool init shell` prints, and every new shell picks it up. **Staleness exists only for an
already-running shell**, which already evaluated the old output. That limit is universal — a running
shell cannot un-define a function it already sourced.

None of these states an explicit old-hook/new-binary contract; it works because the hook is thin.

### nvm.sh — counter-example: logic lives in the sourced file

nvm has no compiled binary. `nvm.sh` is a large POSIX script sourced directly from the rc file, and
the `nvm()` function holds essentially all logic. Upgrading means re-fetching `nvm.sh` itself — no
thin stub, so every logic change requires the sourced file to change and every open shell is stale
until restart. This is the shape most commonly blamed for slow shell startup in this space.
[nvm README](https://github.com/nvm-sh/nvm/blob/master/README.md)

## Comparison

| Tool | Shape | Propagation of a logic change | Staleness detection | Old-hook/new-binary contract | Per-shell-start cost |
|---|---|---|---|---|---|
| conda | generated rc block, rewritten by `conda init` | needs re-run of `conda init` | none — blind marker-only overwrite | none stated; works because block is thin | ~0 |
| rustup | generated stub, content never changes | n/a | none needed | n/a | ~0 |
| mise (POSIX/fish/zsh) | thin stub, binary re-invoked every prompt | instant, next prompt | none needed (dynamic) | implicit — shells out fresh each time | small |
| mise (nushell) | statically generated file | needs re-run of generator | none documented | none documented | ~0 (saves ~10 ms) |
| starship / zoxide / atuin / fnm / pyenv / Homebrew | thin stub, `eval $(tool init)` per shell start | instant, next new shell | none needed; open shells stale regardless | none stated | ~10 ms (starship) |
| nvm | none — full logic sourced directly | needs `nvm.sh` re-fetched | none | none | materially higher |
| **OCX (current)** | **thin stub** — `env.sh`/`.fish`/`.ps1`/`.elv` call `ocx self activate`; `env.nu` applies structured JSON | **instant, next new shell** | diff-gate only (`needs_write` / `refresh_shims`), no version stamp in the shim body | not written down, true by construction | small — one `self activate` subprocess |

## OCX's actual exposure (verified against the code)

1. **Activation logic already lives in the binary, not the shim.** Every POSIX/fish/PowerShell/elvish
   shim is a dispatcher: resolve `$OCX_HOME`, find `ocx` through the `current` symlink, then
   `eval`/`source` the output of `ocx self activate` (`shims.rs:63`, `:103`, `:140`, `:239`). A unit
   test asserts every shim `invokes_binary` (`shims.rs:461-464`). That output is generated fresh by
   the *currently running* binary at every shell start — the mise-POSIX/starship/zoxide shape. A
   change to what gets exported needs **no shim rewrite** and reaches every new shell at next start.
2. **`env.nu` is the one exception**, matching what mise chose for the same reason (no `eval`):
   it applies the global env as structured JSON rather than delegating to `self activate` text.
3. **Only the shim's own wrapper shape goes stale** — comment header, `_ocx_shell` detection,
   interactive-completion re-injection, fish's `conf.d/ocx.fish` indirection. Far smaller and rarer
   than "hook logic", and diff-gated (`needs_write`) so `refresh_shims` self-heals it on
   `self setup` / `self update`. There is no version marker inside the shim body the way
   `rc_block.rs` has one for the RC line. An open shell keeps its old wrapper until restart — the
   universal limit, not an OCX gap.
4. **`rc_block.rs` already solves the harder problem better than conda.** The fence carries a format
   version and a content hash (low 4 bytes of SHA-256), driving a `Fresh`/`Current`/`FormatUpgraded`/
   `Dirty` state machine that detects user edits and exits 82 — exactly the class of bug conda#11030
   is. Prior art to cite, not reinvent.

## Recommendation

**Keep hook logic in the binary behind the existing thin stub; do not move it into the regenerated
shim.** OCX already made the right structural choice — document it as an invariant rather than
revisit it. Every tool that got this wrong (conda's blind rc rewrite, nvm's sourced function
library) pays in surprising mutation or slow startup; every tool that got it right converges on
OCX's current shape.

For the ADR's regeneration section:

1. **State the invariant, not the symptom.** The `env.*` shim bodies are pure dispatchers with no
   ocx-specific business logic; every export decision is made by the currently-running
   `ocx self activate` at each shell start, so a behavior change requires no shim rewrite and
   reaches every new shell immediately. Frame it like rustup's `~/.cargo/env` — intentionally
   near-content-free, which is *why* it needs no versioning story.
2. **Scope "lag" correctly** — only (a) shim wrapper-shape changes, which are rare and diff-gated,
   and (b) an already-running shell, which no surveyed tool solves. Do not scope the ADR's risk
   section to "hook logic changes" broadly; that framing is false for this codebase.
3. **Optional, low priority** — a version marker in the shim body (`# ocx-shim-schema: 1`) mirroring
   `rc_block.rs`, purely for observability (`ocx about` could flag a stale shim). No surveyed tool
   needs this for the thin-stub shape; not a blocker.
4. **Do not generalize mise's static-nushell shape to other shells.** OCX already made that call
   correctly; extending it adds a staleness surface with no benefit.
