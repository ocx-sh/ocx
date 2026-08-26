---
outline: deep
---
# Shell Integration {#shell-integration}

Every time you sit down at a shell, OCX has to answer two questions before it can put anything on `PATH`: *should this project's tools be here at all*, and *are they still the right ones*. The first question is about trust — a project's `ocx.toml` can name any OCI registry, and nothing stops a clone from naming an attacker's. The second is about staleness — an `ocx --global add` or an `ocx update` run five minutes ago should not need a fresh terminal to take effect. This page covers the mechanism that answers both: the per-prompt shell hook, the consent model that gates it, and the commands you have when something looks wrong.

## From inert to active {#activation}

A freshly cloned repository with an `ocx.toml` you have never seen has to stay inert the moment you `cd` into it. Nothing runs at clone time — OCX never executes package code during install — but the risk is not hypothetical: the first tool invocation in that directory would put whatever `ocx.toml` names in front of `cmake`, `cargo`, or `git` on `PATH`. [mise][mise-security]'s own history is the cautionary tale, not a hypothetical one: [GHSA-436v-8fw5-4mj8][mise-ghsa] shipped for four months because a project's own trust-control settings were read *before* the trust check that was supposed to gate them, so a malicious repository could self-declare itself trusted.

OCX closes that ordering gap structurally: the only project-supplied bytes read before consent is established are a directory walk and the lock file's own source list — the check itself needs that much to decide whether to proceed — and `ocx.toml` is never parsed until consent says yes. A fresh clone with no consent stamp, no matching grant, and no lock at all changes nothing and prints one hint line. See [Consent grants](#consent) for exactly what makes a project stop being inert.

Once a project is consented — or for the global toolchain, which needs no consent at all, since `$OCX_HOME/ocx.toml` is your own file — OCX keeps `PATH` converged with what is declared, at every prompt, without an `eval` step you run by hand. `cd` into a consented project and its locked tools land on `PATH` at the next prompt. `cd` back out and they leave again. Add a tool to the global toolchain from another terminal and the shell you are already sitting in picks it up on its next prompt too.

<Terminal src="/casts/in-depth/shell-integration/adding-a-package.cast" title="Adding a tool self-consents the project" collapsed />

::: info Same shape as mise's typed diff, not direnv's byte snapshot
[mise][mise-security]'s `EnvDiffOperation::{Add,Change,Remove}` and [direnv][direnv]'s untyped `{Prev,Next}` snapshot solve the same problem two different ways. direnv's diff stores whole before/after values with no record of *which tool* wrote which key, so restoring it can clobber whatever else touched the environment since — the complaint behind [direnv#82][direnv-82] and [direnv#1249][direnv-1249]. OCX's reconciler follows mise's typed shape: every element it adds is provenance-tagged as its own, so reverting removes exactly what OCX put there and nothing a foreign edit added since.
:::

### The state carrier {#activation-state-carrier}

The reconciler needs to remember, from one prompt to the next, exactly what it applied — otherwise it cannot tell "a value OCX wrote" from "a value you typed," and reverting one would risk clobbering the other. That memory lives in one private environment variable, [`__OCX_ENV_STATE`][env-ocx-env-state]: a compact, base64-encoded ledger recording what is currently applied per scope (global and project, tracked separately) and what each constant looked like before OCX touched it. It travels with the shell like any other variable, so a subshell or a nested `bash -c` inherits it along with the environment it describes.

The ledger is capped at 16 KiB. If it ever grows past that — an unlikely monorepo-scale case — OCX drops to a marker-only record rather than losing track of the fingerprint entirely, and the affected scope's tools stop being revertible for the rest of that session (one line printed once, not on every prompt). A ledger that is missing, truncated, or unreadable is treated the same way a first prompt is: OCX rebuilds what it knows from the project files on disk and removes anything under `$OCX_HOME` that it no longer recognizes as declared.

**Per-shell coverage.** A real per-prompt hook — one that fires on every prompt without you doing anything — needs an append-safe extension point in the shell's own prompt machinery. Not every shell has one:

| Shell | Per-prompt hook | What it reconciles |
|---|---|---|
| bash, zsh | `PROMPT_COMMAND` / `precmd_functions` append | global + project |
| fish | `--on-event fish_prompt` | global + project |
| PowerShell | wraps `prompt`, restoring the previous definition | global + project |
| nushell | `env_change.PWD` (fires on directory change, not every prompt) | global toolchain only — no project reconcile, no revert, no consent gate, today |
| elvish | `$edit:before-readline` append | global + project — guard is carrier-and-`$pwd` only, no watch-set mtime term |
| ash, dash, ksh, Batch | none — no append-safe prompt-hook point exists | shell start only, both scopes |

The four shells in the last row still activate correctly the moment the shell starts — the initial compose is unaffected — but nothing re-checks after that. If you add a global tool or `cd` into a newly consented project mid-session in one of those shells, open a new one to see it. nushell sits in between: its `env_change.PWD` hook keeps the global toolchain live on every directory change, but does not yet reconcile a project scope at all — no activation, no revert, no consent check. A nushell project still goes through [`ocx direnv export`][cmd-direnv-export] or [`ocx run`][cmd-run]. elvish reconciles both scopes at every prompt, same as bash, zsh, fish, and PowerShell, but with a narrower guard — see [Elvish's guard](#activation-elvish-guard) below.

### Elvish's guard {#activation-elvish-guard}

Elvish registers its reconcile the same append-safe way the other hooked shells do, just on a different seam: `set edit:before-readline = [$@edit:before-readline { … }]`, elvish's documented idiom for adding a hook without discarding one another module already installed. Where bash, zsh, fish, and PowerShell compare a stamp against a watch set of file mtimes — `ocx.toml`, `ocx.lock`, the selected binary — elvish's guard has only two terms: the private carrier [`__OCX_ENV_STATE`][env-ocx-env-state] being empty, and the current directory differing from the one the last successful reconcile ran for. Elvish 0.21 has nothing to build a third term from: `os:stat` documents `name`, `size`, `type`, `perm`, `special-modes`, and `sys` as its fields and states timestamps are not exposed, and elvish ships no clock module, so there is no stamp and nothing to compare one against. Elvish's `ocx` wrapper compensates for the missing term: on the way out of every `ocx` invocation run **in that same shell**, it clears the recorded directory, so the next prompt reconciles regardless of what the command changed. That is a narrower net than it sounds, because only two things clear it — an `ocx` command typed in this shell, and a `cd`. The residual is anything OCX watches that changes by neither route: `ocx.toml` or `ocx.lock` edited by hand in an editor is one example, and so is an `ocx add --global` run in a **different** terminal, a `git checkout` that swaps `ocx.lock` on disk, a config file edited by hand, or `ocx self update` run from elsewhere. None of these reconcile in this elvish shell until its next `cd` or its next `ocx` command. Every other hooked shell (bash, zsh, fish, PowerShell) notices all of these immediately, at the very next prompt, because their guard compares file mtimes directly instead of relying on a wrapper. Reaching for an external `test -nt` on every prompt would add one process spawn to the quiet path — exactly the per-prompt cost this design exists to avoid.

## Consent grants {#consent}

A project activating on `cd` — pulling whatever `ocx.toml` names onto `PATH` with no confirmation — is the same shape of risk [mise][mise-security]'s [GHSA-436v-8fw5-4mj8][mise-ghsa] exploited, just aimed the other direction: instead of a project declaring itself trusted, a project here would simply *be* trusted by default. OCX refuses that default. Activation requires one of three independent grants; without any of them, a project is inert regardless of what it declares.

- **A consent stamp** — written automatically the first time you run [`ocx add`][cmd-add], [`ocx remove`][cmd-remove], [`ocx lock`][cmd-lock], [`ocx update`][cmd-update], [`ocx pull`][cmd-pull], or [`ocx run`][cmd-run] against a project. It records which OCI sources that project's lock resolved against at the time. Re-running one of those commands after the lock picks up a source outside the stamped set re-confirms; ordinary growth inside already-consented sources does not. The stamp lives under `$OCX_HOME/state/` — safe to delete at any time, which simply makes the project inert again until you run one of those commands. [`ocx clean`][cmd-clean] does this for you once a project's directory is confirmed gone, in the same pass that garbage-collects its packages; see [Remove and clean up][user-guide-cleanup] for the full behavior.
- **A path grant** — an exact, canonicalized directory an operator has pre-authorized, without knowing in advance what that checkout will resolve against. This is the [devcontainer feature][devcontainer-features] and CI-image case: the image build knows the checkout path but not its eventual lock contents.
- **A namespace grant** — a set of OCI sources (`<registry>/<org>`) an operator trusts, without knowing every path a matching project might be checked out to. This is the fleet case: pre-approve `ocx.sh/acme-corp` once, and a project whose tools all came from inside that namespace activates. What is matched is *not* the lock's text. The package store records, for every package it fetched, the coordinate it resolved and got digest-verified content for, and the grant is decided against that record: a lock naming `ocx.sh/acme-corp/anything` buys nothing unless this machine genuinely fetched those digests under `ocx.sh/acme-corp`. The practical consequence is that a namespace grant activates a project whose tools this machine has already fetched — a warm shared store, which is the fleet case — and stays inert on a cold one until the first [`ocx pull`][cmd-pull], which writes a consent stamp anyway. [`ocx shell state`](#diagnostics) names the gap when a lock's claim and the store's record disagree. A namespace grant also stops at the package boundary: it authorizes the tools `ocx.lock` resolved, never the project's own `[env]` table in `ocx.toml`, because that table has no publisher to hold accountable — a bare `type = "path"` entry works from clone content alone, with no registry involved. A namespace-granted project that declares `[env]` still gets its tools; OCX withholds the table and prints a hint naming the fix: run [`ocx pull`][cmd-pull] there once (which also writes a consent stamp), or add the directory to `[shell.consent] paths`. See [What consent does not cover](#residual) for what it still does not buy.

Path and namespace grants are independent and additive — either alone is sufficient, neither constrains the other, and an absent or empty grant means nothing is authorized, never "everything is." Neither grant ever writes a consent stamp; drift for a namespace grant is re-checked on every prompt against the current lock, and a path grant is deliberately drift-blind, since an operator naming a checkout in advance cannot enumerate its future sources.

<Terminal src="/casts/in-depth/shell-integration/inert-to-consented.cast" title="A path grant activates a project before any lock exists" collapsed />

### Where a grant can live {#consent-tiers}

Path and namespace grants share the same `[shell.consent]` table wherever OCX reads [configuration][config-ref] from — system, user, and home tiers, an explicit [`--config`][arg-config] / [`OCX_CONFIG`][env-ocx-config] file, and the [managed tier][config-managed], but only when the managed source is digest-pinned; an unpinned managed tag has `[shell.consent]` stripped with a warning, the same rule OCX already applies to Sigstore trust roots. Two environment variables reach the same table without a file: [`OCX_CONSENT_PATHS`][env-ocx-consent-paths] and [`OCX_CONSENT_NAMESPACES`][env-ocx-consent-namespaces]. Every source unions — nothing in a lower tier overrides a higher one, only adds to it.

**`ocx.toml` cannot carry `[shell.consent]`.** The project's own config file rejects an unknown `[shell]` section outright — a hard parse error, not a silent skip — because a project-writable consent grant would let a clone consent to itself.

```toml
# config.toml (system, user, or home tier — never ocx.toml)
[shell]
hook        = true
completions = true

[shell.consent]
paths      = ["/workspaces/acme-monorepo"]
namespaces = "ocx.sh/acme-corp"

# or the carve-out form, for withdrawing one namespace another tier granted:
# namespaces = { include = ["ocx.sh/acme-corp", "ocx.sh/acme-labs"], exclude = ["ocx.sh/acme-labs"] }
```

::: warning What consent grants — and what they cannot buy back
None of the three grants above authenticate *who* published the bytes a consented namespace resolves to. See [What consent does not cover](#residual) for the honest boundary and the control that actually answers that question.
:::

## Commands {#commands}

Both the hook and shell completions follow the same `--flag` / `--no-flag` / `OCX_NO_*` / config-key / auto ladder [`ocx self setup`][cmd-self-setup] already uses. `self setup --hook` / `--no-hook` and the newly added `--completion` / `--no-completion` write `[shell] hook` / `[shell] completions` to your home-tier `config.toml`; leaving a flag off writes nothing, and the previously configured (or default) value applies.

```sh
ocx self setup --hook --completion       # write [shell] hook = true, completions = true
ocx self setup --no-hook                 # write [shell] hook = false
```

[`ocx self activate`][cmd-self-activate] — the command your shim already calls at shell start — accepts the same `--hook` / `--no-hook` pair for a one-off override, and reads `[shell]` once, at shell start. It never reads configuration again on the per-prompt path; that reserved budget is what keeps an unchanged prompt effectively free.

Disabling the hook entirely, for a single shell or every shell, is [`OCX_NO_HOOK`][env-ocx-no-hook] — see its reference entry for the exact rules, including why it only takes effect at the next shell start.

## Diagnosing a shell {#diagnostics}

Everything documented so far is deliberately quiet: the hook logs at debug, an absent ledger is the ordinary first-prompt case, an inert project prints one hint line at most, and a yielded scope prints one info line. That is right for a path that runs on every keystroke's worth of prompts and wrong the moment you are staring at a missing tool wondering why.

[`ocx shell state`][cmd-shell-state] is the read-only answer. It never mutates anything — no stamp, no ledger repair, no plan — and it exits `0` in every state it reports, including every flavor of "not active." It prints:

- the decoded ledger, as fields rather than base64 — what is currently applied, per scope;
- fingerprint status: the watch set OCX is comparing against, and whether it still matches what is recorded;
- whether the priors needed to restore a constant on scope exit are still intact;
- and, when a project is not active, *why* — a consent stamp missing, a stamp present but the lock outgrowing it, the hook disabled and which config tier decided that, a [yield to direnv or mise](#coexistence) naming the live signal it saw, or a ledger reduced to a marker because it went over the size cap.

```sh
ocx shell state                 # human-readable
ocx shell state --format json   # same content, machine-readable
```

Its output is never `eval`-able — no line is valid shell-assignment syntax in any supported shell — on purpose. `ocx self activate` emits text meant to be evaluated; `ocx shell state` emits text meant to be read, and a surface where those two are interchangeable is one copy-paste away from evaluating a diagnostic dump into your live shell.

Exit code `0` covers every reportable state. The only non-zero exit is `74`, and only when `$OCX_HOME` itself cannot be read.

## Coexisting with direnv and mise {#coexistence}

[direnv][direnv] and [mise][mise-security] both prepend to `PATH` from a per-prompt hook of their own, and mise's own documentation is upfront that combining the two is not a supported configuration — two hooks racing to reorder the same `PATH` has no well-defined outcome. OCX's mechanism is the same shape, so it is the same class of collision, and OCX does not try to referee it. Instead, it yields.

The yield check looks at **live session state**, never a file on disk: `DIRENV_DIR` naming the current project, or `MISE_SHELL` / `__MISE_ORIG_PATH`. A `.envrc` or `mise.toml` checked into a repository whose owner is not actually hooked into *this* shell is not evidence of a live hook — it is evidence of someone else's workflow, and treating it as a yield signal would leave the project silently managed by nobody. When either tool is genuinely active for the current directory, OCX narrows to the global toolchain only, reverts any project scope it had already applied, and prints one info line naming the tool it yielded to.

Hook registration order relative to direnv's or mise's own entries is unspecified and deliberately not refereed — no reordering logic, no cross-tool coordination, no retry. If ocx's hook runs first on the prompt where the other tool activates, the project scope may apply and then revert within that one prompt, and the shell is converged by the next prompt.

::: info direnv and mise are not mutually aware either
This is not a gap unique to OCX: [direnv's stdlib][direnv] and [mise's own hooks][mise-security] make no attempt to detect each other, so the same "two hooks, one PATH" ordering question exists between them already. OCX's yield rule keeps it a two-party problem instead of a three-party one.
:::

## Repairing a stuck shell {#repair}

If a shell's ledger falls out of sync with reality — a write landed inside the same filesystem-timestamp granularity as OCX's change-detection window, or the carrier itself looks corrupted — the repair gesture is `unset __OCX_ENV_STATE`. Clearing the variable makes the next prompt see an absent ledger, which is a state the reconciler already handles in full: it rebuilds the desired environment from the project files on disk, removes anything under `$OCX_HOME` it no longer recognizes, and leaves any other constant alone rather than guessing at it.

**Its cost, stated plainly.** Clearing the ledger destroys the recorded *priors* — the values OCX saw before it applied anything. If you had, say, a hand-set `JAVA_HOME` before entering a project, the priors bullet is what remembers that value so leaving the project can restore it. After `unset __OCX_ENV_STATE`, that memory is gone: `JAVA_HOME` keeps whatever the project set for the rest of that shell's life, because nothing records what it was before. A brand-new shell is the clean floor whenever one is cheap to open — it starts with no priors, so it has nothing to lose.

The gesture is silent by construction: OCX cannot distinguish a deliberately cleared ledger from the ordinary absence on a shell's first prompt, and that first-prompt case logs at debug by design. [`ocx shell state`](#diagnostics) is how you confirm the repair actually took.

<Terminal src="/casts/in-depth/shell-integration/cd-into-project.cast" title="Landing inside a consented project" collapsed />

<Terminal src="/casts/in-depth/shell-integration/cd-out-of-project.cast" title="Leaving a project's directory" collapsed />

## What consent does not cover {#residual}

Consent answers one question: *may this project's toolchain reach my `PATH` at all*. It does not answer a second, related one: *did the identity I expect actually publish these bytes*. Within an already-consented namespace, whoever can publish gets code in front of your `PATH` with no signal from the consent mechanism — accepted, by design, because content-hash re-confirmation on every ordinary `git pull` or `ocx update` would train users to click through prompts without reading them, which is worse.

A [namespace grant](#consent) sits inside that same residual, and it is deliberately not decided by the lock. `ocx.lock` is project-supplied text — a clone can name any source it likes — so matching a namespace pattern against it would let a clone borrow a trusted organization's name for content that never came from there. The grant is decided instead against the package store's own record of the coordinate each locked digest was materialized under *on this machine*. A lock is text a clone's author writes; the record takes an act of pulling here under that name. On a machine that has not seen the content those coincide — the bytes come off the wire under that name, and publishing into a listed organization needs that organization's publish credential. Where the layer cache already holds the digest they do not: one `ocx pull` naming the granted organization writes the record with no registry in the loop. The record is evidence about what this machine did, not about what a registry attested. That is also why there is **no whole-registry pattern** — `ocx.sh/*` and a bare `ocx.sh` are both refused at parse, because a grant spanning every organization on a host trusts every publisher on it wherever anyone can register. List organizations one at a time.

What the grant still cannot tell you is whether the *expected* identity published those bytes: a credential inside a listed organization is enough, and the record says which coordinate the digest was fetched under, never who signed it. The record is that coordinate as *you* named it, so if you have configured a [`[mirrors]`][config-mirrors] entry or an index that redirects it, the bytes came from your own routing rather than from the upstream host — the content is digest-verified either way, and *who published* it is the same open question. That is the residual above, and the control for it is the [`[[trust.policy]]`][config-trust-policy] below.

The real control for that residual already exists and answers the right question: an operator-tier [`[[trust.policy]]`][config-trust-policy] plus [`ocx package verify`][cmd-package-verify]. Consent decides whether to run a project's toolchain at all; a Sigstore signature decides whether the expected identity published what you are about to run. They are deliberately separate mechanisms — folding signature verification into the consent stamp would produce one system doing both jobs worse.

::: warning This mitigation is opt-in, not default
With no `[[trust.policy]]` configured, automatic verification is a no-op — someone who cloned a repository and wrote no operator config gets no signature check on the hook's path. Turning it on means writing an operator-tier `[[trust.policy]]` yourself; see [Signing][in-depth-signing] for the full model. A project's own `ocx.toml` cannot enable this on your behalf, by the same logic that keeps `[shell.consent]` out of it.
:::

<!-- external -->
[mise-security]: https://mise.jdx.dev/
[mise-ghsa]: https://github.com/jdx/mise/security/advisories/GHSA-436v-8fw5-4mj8
[direnv]: https://direnv.net/
[direnv-82]: https://github.com/direnv/direnv/issues/82
[direnv-1249]: https://github.com/direnv/direnv/issues/1249
[devcontainer-features]: https://containers.dev/implementors/features/

<!-- commands -->
[cmd-self-setup]: ../reference/command-line.md#self-setup
[cmd-self-activate]: ../reference/command-line.md#self-activate
[cmd-direnv-export]: ../reference/command-line.md#direnv-export
[cmd-run]: ../reference/command-line.md#run
[cmd-add]: ../reference/command-line.md#add
[cmd-remove]: ../reference/command-line.md#remove
[cmd-lock]: ../reference/command-line.md#lock
[cmd-update]: ../reference/command-line.md#update
[cmd-pull]: ../reference/command-line.md#pull
[cmd-package-verify]: ../reference/command-line.md#package-verify
[cmd-shell-state]: ../reference/command-line.md#shell-state
[cmd-clean]: ../reference/command-line.md#clean

<!-- reference -->
[config-ref]: ../reference/configuration.md
[config-managed]: ../reference/configuration.md#keys-managed
[config-trust-policy]: ../reference/configuration.md#keys-trust
[config-mirrors]: ../reference/configuration.md#keys-mirrors
[arg-config]: ../reference/command-line.md#arg-config
[env-ocx-env-state]: ../reference/environment.md#ocx-env-state
[env-ocx-no-hook]: ../reference/environment.md#ocx-no-hook
[env-ocx-config]: ../reference/environment.md#ocx-config
[env-ocx-consent-paths]: ../reference/environment.md#ocx-consent-paths
[env-ocx-consent-namespaces]: ../reference/environment.md#ocx-consent-namespaces

<!-- in-depth -->
[in-depth-signing]: ./signing.md

<!-- cross-page -->
[user-guide-cleanup]: ../user-guide.md#cleanup
