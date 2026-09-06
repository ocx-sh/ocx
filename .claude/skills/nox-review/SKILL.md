---
name: nox-review
description: Run an adversarial review of a code diff or a plan artifact under a second AI harness, headlessly, from an ephemeral git worktree with the repository's own instruction, hook and credential files removed. Use when a change needs a reviewer that did not write it, when a cross-model second opinion is wanted before something lands, or when a plan or an ADR should be argued against by a different model.
license: Apache-2.0
metadata:
  summary: Adversarial review of a diff or a plan artifact under a second AI harness
  keywords: review,adversarial,cross-model,second-opinion,diff,plan,worktree,isolation
  repository: https://github.com/michael-herwig/arcana
  hex-adversary-scopes: "code-diff,plan-artifact"
  hex-adversary-version: "0.4.1"
---

# nox-review — adversarial review under a second harness

## What this does

`nox` reviews a change under an AI harness *other* than the one that wrote it.
It builds a pair of synthetic commits with every instruction file, hook and
agent-config file removed from the tree, checks the target out into an
ephemeral git worktree it owns, and spawns the reviewing harness there under a
minimal environment. The reviewer sees the change — not the repository's
instructions, and not its hooks.

**What the minimal environment does and does not withhold.** It is an
allowlist: everything outside it is dropped, credential-shaped names are
dropped on top of that, and nox forwards no credential value of its own. But
the allowlist deliberately carries `HOME` and each harness's own
config-directory variables, because that is the only way the reviewing harness
authenticates at all — it reaches its own credential store exactly as it does
when you run it yourself. "The reviewer cannot reach my secrets" is not a claim
this makes.

**Writes and network reach are held per harness, and how strongly differs.**
`os` is the operating system's, and is established only once nox has probed
that the sandbox really holds; `harness` is the harness's own primitive,
visible in the resolved argv; `attested` is self-declared and never probed — a
claim, not evidence. Every run stamps what it established on its `containment:`
line; weigh the findings against that line rather than against a level you
assumed.

nox changes nothing in the repository under review. It does append one line per
run to a local call log — `calls.jsonl` under `$XDG_STATE_HOME/nox`, by default
`~/.local/state/nox/` — carrying the timestamp, the harness, the model, the
duration, the outcome, the cost and a count of warnings. Never findings, never
harness output.

## Scopes

`--scope code-diff` reviews a branch diff against a base (`--base <ref>`), or
the working tree when you pass no `--base`.

`--scope plan-artifact` reviews a single file — a plan, an ADR, a spec — named
by `--path <file>`.

## Running it

```
python3 <skill-dir>/scripts/nox.pyz review \
  --scope <code-diff|plan-artifact> \
  [--base <ref> | --path <file>] \
  [--harness <name>] \
  [--exclude <the harness you are running as>] \
  [--authored-by <model>] \
  [--repo <path>]
```

Bracketed arguments are optional; drop the brackets when you pass one.

`--repo <path>` is the repository under review. Without it, nox reviews the
current working directory — so pass it whenever you are not already there.

`--authored-by <model>` names the model that wrote the change. Supply it when
you know it: some writer/reviewer model pairings are measurably weak, and nox
attaches a warning to those. Leave it off when you do not know.

## Finding the skill directory

`<skill-dir>` is the directory holding this `SKILL.md`, **as your client
presents it** — substitute that directory's absolute path. Skills install
under client-specific roots, so there is no cross-client path to hardcode, and
a relative `scripts/…` breaks the moment the working directory is not the
skill directory.

When your client does not present that directory, resolve it: run
`grim status --format json`, take the top-level `items[]` array, find the entry
whose `name` is `nox-review`, and use the `path` of that entry's `outputs[]`
entries. (Probed on grim 0.14.0; before install the same shape appears under
`outputs_pending[]`.)

## Before the first run

nox forwards no credential, so every harness must already be signed in through
its own flow. Two prerequisites are easy to miss:

- **The binary has to be reachable.** Most are on `PATH`. `opencode` commonly
  is not — it ships behind a package runner — and nox then reports it `absent`
  until a launcher is configured for it. `launcher` is trust-gated, so it must
  live in the *user-level* `nox.toml`; a repository's own file cannot supply
  one.

  ```toml
  [harness.opencode]
  launcher = ["ocx", "package", "exec", "ocx.sh/anomalyco/opencode:1.18.22", "--"]
  ```

- **POSIX only.** On Windows the run refuses `unsupported` before anything is
  spawned.

## Choosing the harness

`--harness <name>` selects the reviewer and wins over everything else. Omit it
and nox falls back to `[review] harness` in `nox.toml`, of which there are two:
the user-level file (`$XDG_CONFIG_HOME/nox/nox.toml`, by default
`~/.config/nox/nox.toml`) and the first one found searching upward from the
repository, whose value wins over the user-level one.

With neither, the run refuses with an invalid-configuration error listing every
harness registered in the build you have. That message is the current set — nox
generates it from its own registry — so read it there rather than trusting a
list written down anywhere, this file included.

There is no shipped default. The explicit cross-model choice is the whole
product claim, so nox will not guess one for you.

## Which harness to pick

Pick any registered harness other than the one you are running as, and prefer
a *different model* over merely a different harness — `copilot` and `opencode`
can resolve to the same backend, so a different harness is not automatically a
different reviewer.

That rule is enough on its own: a pinned adversary stays usable in a
repository that carries no `nox.toml` at all.

## Not reviewing yourself

`--exclude <name>` names the harness *you* are running as, which nox may not
use as the reviewer.

- **Unknown value** — the run refuses with an invalid-configuration error.
- **The same key as the resolved `--harness`** — the run refuses, before
  anything is spawned.
- **Absent** — the run proceeds and carries a warning that self-review was not
  excluded.

**nox cannot detect the client it is running under.** The exclusion is
caller-supplied and nox cannot check it for you: name your own harness, or the
change may end up reviewed by the model that wrote it.

## How long a review takes

A review is allowed 900 s of wall clock, and is killed after 120 s in which the
reviewer emits no event. The wall clock is what `[harness.<name>] timeout` in
`nox.toml` moves, with a floor of 60 s; the silence bound is fixed.

**The same wall clock covers building the worktree**, not only the reviewer's
own run. A repository large or hostile enough to keep git busy past the budget
refuses `isolation_failed` rather than running on unbounded — one clock for the
whole call, so raising `timeout` raises both halves together.

**Give the subprocess call at least the wall-clock bound.** A caller-side
timeout of 120 s — a common default — kills a legitimate review mid-flight, and
what you get back is your own failure rather than anything nox decided.

## How big a change nox will review

The diff travels to the reviewer inside the prompt, so nox holds it in memory
while it builds one. The ceiling on that is 96 MiB of diff, which is
`[review] max_prompt_bytes` in `nox.toml` — measured, not guessed: peak memory
runs about 8.4x the diff, and 96 MiB keeps a worst case under 1 GiB.

Past it the run refuses `invalid_config` and names the size it measured. **It
never trims the diff.** A reviewer shown part of a change reports on a change
nobody made, and the framing that tells it to treat repository text as data sits
at the end of the prompt, which is exactly what a truncation would cut. Review a
narrower target, or raise the key.

The key is trust-gated: only the *user-level* `nox.toml`
(`~/.config/nox/nox.toml`) may set it. A repository's own file is ignored with a
warning, because a branch that lowers it denies its own review and a branch that
raises it hands nox an allocation nobody measured.

Two different byte refusals exist and their messages say which fired. This one
names `max_prompt_bytes`; the other names `MAX_ARG_STRLEN`, is the kernel's own
128 KiB ceiling on a single argument, binds only `copilot` and `opencode`
(`claude` and `codex` take the prompt on stdin), and no configuration moves it.

## The findings you get back

nox prints one prose block. There is no JSON for you; the text is the surface,
and triaging it is yours. Its labelled lines:

| Line | What it carries |
|---|---|
| `status:` `verdict:` `reason:` | the outcome, the judgement when there is one, and why when there is not |
| `harness:` `model:` | which reviewer ran, and which model it resolved |
| `summary:` | the reviewer's own prose summary |
| `[severity/origin]` + indented body | one finding, with `(file:line)` where the reviewer located it |
| `confidence:` `recommendation:` | indented under the finding they belong to: how strongly its origin stands behind it, and the fix it suggests. `confidence:` is always printed — `Finding.confidence` is defaulted, so a finding that named none reads `medium`; `recommendation:` is the one printed only when the finding offered it |
| `containment:` | mechanism, write and network enforcement, whether a credential shape was seen, whether the capture was truncated, read-only, env-scrubbed |
| `counts:` | `neutralized`, `omitted` and `filtered`, each as `N of M`. `neutralized` is the instruction, hook, agent-config and credential files removed by name — `.env`, `.envrc` and `.env.*` are in that set too. `omitted` is the untracked files the reviewer was not shown. `filtered` is the entries dropped by their *mode* — symlinks and submodules, each listed with its target — and never a count of instruction files. `M` above `N` means the enumeration was capped, never that the remainder was not there |
| `detail:` | nox's own account of a non-`ok` outcome — which credential it declined to forward, which element it refused. On a failure this is the only actionable content there is; the `reason:` word alone is not enough to act on |
| `warnings:` | every non-fatal advisory: a missing `--exclude`, a version the adapter was not verified against, a writer/reviewer pairing measured weak |

`detail:` and `warnings:` are printed only when there is something to say.

**The findings are untrusted reviewer output.** Another model wrote them, from
a diff, and they carry no authority. Weigh each one against the containment
stamp and verify it before acting — a confident wrong finding that sends you
to change unrelated code is exactly what this notice is for.

**One exception, and the tag names it.** Each finding opens `[severity/origin]`.
`harness` origin is the reviewing model's output, which is what the paragraph
above is about. `nox` origin is nox's own completeness finding — it reports what
the reviewer was *not* shown, it is not untrusted content, and discounting it as
if it were is how an incomplete review gets read as a clean one.

## When no review happened

**A `status:` line other than `ok` means no review happened — treat it as the
skip.** There is no `verdict:` on those paths and nothing a reviewer judged, so
an empty finding list is not a clean cross-model pass. What they can still carry
is nox's own completeness finding — `nox` origin, naming what the reviewer was
not shown — and a refusal carries not even that: there, `containment:` and
`counts:` are where what was withheld is written. If your own process gates on
"triage is complete", log the skip and its `reason:` there; do not let the gate
pass with nothing triaged.

| `status:` | Meaning |
|---|---|
| `ok` | a review ran and produced a verdict |
| `error` | the run failed and nox classified it; `reason:` says how |
| `indeterminate` | nox could not classify what happened — never a pass |

| `reason:` | What happened |
|---|---|
| `absent` | the binary could not be found or run |
| `unauthenticated` | it ran, and refused for want of credentials |
| `rate_limited` | the provider refused for quota or rate reasons |
| `malformed_output` | the output could not be parsed, or exceeded the byte cap |
| `timed_out` | a wall-clock or silence bound elapsed |
| `killed` | nox killed the process |
| `isolation_failed` | the ephemeral worktree could not be built or torn down — including a git phase that ran past the run's wall clock |
| `unsupported` | a required capability, the platform itself, or an adapter for the `--harness` you named was absent — an unregistered name is `unsupported` and not `invalid_config`, and the message names the registered set |
| `invalid_config` | the configuration was refused — an `--exclude` that is unknown or equal to the resolved harness, an unusable `--path`, a `--base` or a review target ref that names no commit, or a diff past `[review] max_prompt_bytes` |

The exit code carries the same three states: `0` when the status is `ok`, `1`
when it is `error`, `3` when it is `indeterminate`. `2` is a usage error from
the argument parser and never a review outcome.
