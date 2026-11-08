---
title: "ocx shell"
---
<!-- doc_type: reference -->

## `ocx shell` {#shell}

Open your shell inside the project environment.

Bare `ocx shell` composes the environment like `ocx exec` and starts your shell in it: `-g` selects groups, `--with` adds packages not in `ocx.toml`. The composition layers onto the calling environment; `--isolate` first reverts what ocx applied to it, `--clean` starts from nothing.

The shell stays tied to its project: its hook refreshes that layer when `ocx.toml` or `ocx.lock` change, and `cd` elsewhere applies nothing. `exit` returns to the parent shell.

The subcommands generate completions, grant or revoke activation and report per-prompt state. Login activation is `$OCX_HOME/env.sh`.

```text
ocx shell [OPTIONS]
ocx shell <COMMAND>
```

| Flag | Value | Description |
| --- | --- | --- |
| `--clean` |  | Start the child from an empty environment holding only the composed variables |
| `--consent` |  | Record a consent stamp (the default unless OCX_NO_CONSENT is set) |
| `--env` | `NAME\|KEY[:TYPE[:SEP]]=VALUE` | Set an environment variable, or pass one by name. An unset NAME adds nothing; a TYPE without a value exits 64 |
| `--group`, `-g` | `GROUPS` | Restrict to the named group(s) |
| `--isolate` |  | Remove every value ocx applied to the calling environment before composing |
| `--no-consent` |  | Run without consenting to this project's shell activation |
| `--no-pinned` |  | Compose through the toolchain links instead of digest paths |
| `--pinned` |  | Compose digest paths instead of the toolchain links |
| `--with` | `IDENTIFIER` | Add a package that `ocx.toml` does not pin to the composed environment |

## `ocx shell allow` {#shell-allow}

Consent to a project's shell activation.

Records a consent stamp for the project governing PATH (default: the current directory), so the next shell prompt applies that project's binaries and environment. This is the same stamp `ocx init` writes for the project it creates.

`ocx add`, `ocx lock`, `ocx remove`, `ocx update`, `ocx upgrade`, `ocx pull` and `ocx exec` write it as a side effect, because running a mutating command in a directory is itself consent. `--no-consent` or `OCX_NO_CONSENT` suppresses the side effect, not this command.

The stamp records the source set the project's `ocx.lock` resolves from at the time it is written. A binding added from a new registry or organisation invalidates it; run this again to consent to the wider set.

Undo with `ocx shell revoke`. See which grant is in effect with `ocx shell state`.

Grants that live in configuration instead - a directory under `[shell.consent] paths`, an organisation under `[shell.consent] namespaces` - are edited in `config.toml`, not here.

Exits 64 when no `ocx.toml` governs PATH, and when PATH names the ocx home: the global toolchain is always consented and never carries a stamp.

https://ocx.sh/docs/explanation/shell-integration/

```text
ocx shell allow [PATH]
```

## `ocx shell completion` {#shell-completion}

Generate shell completion scripts.

Prints the completion script for the shell named by `--shell` to stdout, for sourcing from a profile or writing into the shell's completion directory. With `--if-enabled` it prints nothing when completions are switched off for the session.

Install recipes per shell: <https://ocx.sh/docs/troubleshooting/shell/#commands>

```text
ocx shell completion [OPTIONS]
```

| Flag | Value | Description |
| --- | --- | --- |
| `--if-enabled` |  | Print nothing when completions are switched off for this session |
| `--shell` | `SHELL` | The shell to generate the completions for: bash, elvish, fish, powershell or zsh. Omitted, it is detected from the parent shell |

## `ocx shell revoke` {#shell-revoke}

Withdraw a project's consent stamp.

Removes the consent stamp for the project governing PATH (default: the current directory). The next shell prompt stops applying that project's binaries and environment, unless a `[shell.consent]` grant still covers it.

Those grants live in `config.toml` and are removed by editing it.

`ocx shell state` reports which grant is in effect.

Revoking a project that carries no stamp is not an error: the result is the state you asked for either way.

A later `ocx add`, `ocx lock`, `ocx pull`, `ocx exec`, `ocx remove`, `ocx update` or `ocx upgrade` in that directory writes the stamp again.

Exits 64 when no `ocx.toml` governs PATH.

https://ocx.sh/docs/explanation/shell-integration/

```text
ocx shell revoke [PATH]
```

## `ocx shell state` {#shell-state}

Report the shell integration's state, and why it is inert when it is.

Prints the decoded `__OCX_ENV_STATE` as fields. They show what each scope applied, whether the watch-set fingerprint still matches, and whether the project scope's constant priors are intact.

Above all they show the enumerated reason the shell is not active.

The shell is inert for one of these reasons, each with its remedy.

The hook is disabled by `--no-hook`, `OCX_NO_HOOK` or `[shell] hook`; the report names the deciding rung and tier. Enable the hook.

The shell yielded to direnv, mise or an enclosing `ocx exec`; the report names the live signal. Stop that tool or accept the yield.

`ocx.lock` is unavailable: absent, unreadable or unparseable. Run `ocx lock`.

A `[shell.consent] namespaces` grant matches the lock's claim, but the package store does not corroborate those sources. The report names both source sets.

Consent with `ocx shell allow`.

The source set drifted from the stamp; the report names the new sources. Run `ocx shell allow` again.

There is no consent stamp and no matching grant. Run `ocx shell allow`.

The state exceeded its cap. Declare less environment in the abandoned scope.

The state is absent. In a shell `ocx self setup` wired, that is the first prompt and none is needed; the next prompt applies it.

If setup never wired this shell, run `ocx self setup` and start a new shell.

The state is present but corrupt. Start a new shell, or run `unset __OCX_ENV_STATE`.

A consented project the state does not yet show reports `active: not yet`, and applies at the next prompt. If its lock is absent or stale, it reports `active: no`, names the refusal and points at `ocx lock`.

Read-only: it never writes a consent stamp, never repairs the state and never emits a plan. The repair gesture is `unset __OCX_ENV_STATE` (which destroys the priors, so a new shell is the cleaner floor); this command is how you check it worked.

The output is diagnostics for a human to read and is never valid shell source. It is deliberately not interchangeable with `ocx self activate`.

Exits 0 in every reportable state, including an inert shell: the reason is the payload, not a failure. Exits 74 only when `$OCX_HOME` cannot be read.

The text output is the answer and nothing else. It shows where `$OCX_HOME` is, which project is in effect, and whether the integration is active.

When it is not active, it also shows the reason and the line that says what to do. `--verbose` only adds the evidence behind it.

It is a rendering tier, not a payload, so `--format json` emits the same complete report with or without it.

When a project is active the report names the grant that activated it: `grant` in JSON, a `granted by:` line in text. The value is `stamp`, `path` or `namespace`.

A stamp outranks `[shell.consent]`, so a project can be active with nothing in `config.toml` to explain it. A `namespace` grant authorizes the project's packages, never its own `[env]`.

Every text line starts with a fixed label and every interpolated value is quoted. No line is valid export syntax in any supported shell dialect.

https://ocx.sh/docs/explanation/shell-integration/

```text
ocx shell state [OPTIONS]
```

| Flag | Value | Description |
| --- | --- | --- |
| `--verbose`, `-v` |  | Add the diagnostics behind the answer - the decoded state, the fingerprint watch set and the hook ladder |
