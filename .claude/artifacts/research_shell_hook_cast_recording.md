# Research: Recording Terminal Casts of Interactive Prompt-Hook Behaviour

**Date:** 2026-08-25
**Axis:** technology / tooling
**Consumer:** [`adr_shell_env_overhaul.md`](./adr_shell_env_overhaul.md) documentation work package — the website
section on the shell hook needs asciinema casts for "add a package", "cd into a project", "cd out".

## The question

Every existing OCX doc cast replays literal non-interactive command lines. A prompt hook fires *between*
commands, driven by the shell's own prompt machinery. It was not obvious the shipped recorder could
capture `cd project/` producing a visible env change.

## Answer: yes — the shipped recorder already drives a real interactive bash

`CastRecorder.open()` (`test/recordings/cast_recorder.py`) uses `pexpect.spawn("/bin/bash",
["--norc", "--noprofile"], …)`. `pexpect.spawn` always allocates a real pty, and bash decides
interactivity from stdin/stderr being terminals with no `-c`/script argument — `--norc`/`--noprofile`
suppress startup-file *sourcing*, not interactivity
([GNU Bash manual, "Is this Shell Interactive?"](https://www.gnu.org/software/bash/manual/html_node/Is-this-Shell-Interactive_003f.html)).
So bash re-enters its prompt cycle and evaluates `PROMPT_COMMAND` after every command.

`run_command()` sends each line with `sendline()` and blocks on a private sentinel `PS1` reappearing
(`_read_until_prompt`). **Anything a `PROMPT_COMMAND` hook prints between a command's output and the next
prompt is therefore captured verbatim.** Nothing in `open()`/`run_command()` touches `PROMPT_COMMAND`.

**Two preconditions for a hook to fire during replay:**

1. The activation line that *registers* the hook must itself be inside the `# region cast` block —
   `--norc --noprofile` means nothing auto-activates.
2. The subsequent `cd` must go through `run_command`, so the recorder waits for the next sentinel.

**Two real limits, neither a blocker:**

- The shell is hardcoded to `/bin/bash`. Proving the bash `PROMPT_COMMAND` path needs no change; a fish
  (`fish_prompt` event), zsh (`precmd_functions`) or pwsh (`prompt`) cast needs a shell parameter on
  `CastRecorder.open()`.
- `run_command()` issues a second, silent prompt cycle for `echo $?` right after the visible one, so a
  bash hook fires **twice** per recorded line. Harmless for an idempotent hook; worth a comment.

## Prior art — how comparable tools record per-prompt demos

| Tool | Demo of directory-change behaviour | Producer | Scripted? | Regenerated? |
|---|---|---|---|---|
| **[mise](https://github.com/jdx/mise)** | **Yes — the decisive precedent** | **VHS**, [`docs/tapes/demo.tape`](https://github.com/jdx/mise/blob/main/docs/tapes/demo.tape) | Yes — `Set Shell "bash"`, `eval "$(mise activate bash)"`, `cd` in/out showing versions appear/disappear | Task-driven (`docs:demos` in [`tasks.toml`](https://github.com/jdx/mise/blob/main/tasks.toml)), VHS in Docker for glyph reproducibility; not per-PR gated |
| [direnv](https://github.com/direnv/direnv) | No README embed; historical hand-recorded [asciinema](https://asciinema.org/a/351507) | asciinema, external | No | No |
| [starship](https://github.com/starship/starship) | `media/demo.gif` | not established | not established | not established |
| [zoxide](https://github.com/ajeetdsouza/zoxide) | `contrib/tutorial.gif` | not established | no generation script in repo | No |
| [atuin](https://github.com/atuinsh/atuin) | `demo.gif` | not established | not established | not established |
| [oh-my-posh](https://github.com/JanDeDobbeleer/oh-my-posh), [nvm](https://github.com/nvm-sh/nvm) | none | — | — | — |
| [pyenv](https://github.com/pyenv/pyenv) | `install_local_python.gif` | not established | not established | No |

mise's tape is empirical proof that a VHS-spawned bash is a real interactive shell in which
`PROMPT_COMMAND` fires — otherwise its own flagship demo would not work
([mise activate docs](https://mise.jdx.dev/cli/activate.html)).

## Tooling landscape, 2026

| Tool | Real interactive shell? | CI-reproducible? | Output | Adoption |
|---|---|---|---|---|
| **asciinema 3.x** | yes (spawns `$SHELL`), but it is a *capture* tool — something else must drive keystrokes | format deterministic ([asciicast v3](https://docs.asciinema.org/manual/asciicast/v3/): nested `term`, relative intervals, `"x"` exit-status event, `"m"` marker, `"r"` resize) | `.cast` | 17.7k★, [v3.2.1](https://github.com/asciinema/asciinema/releases), Aug 2026 |
| **[agg](https://github.com/asciinema/agg)** | n/a | deterministic given a `.cast` | GIF | 1.7k★, v1.9.0 (2026-05-29) |
| **[VHS](https://github.com/charmbracelet/vhs)** | **yes** — `Set Shell`, `Type`/`Enter` are literal keystrokes | yes, with the Docker image pinning fonts; **timing is not automatic** — the default primitive is `Sleep`, `Wait /regex/` is opt-in per line ([#537](https://github.com/charmbracelet/vhs/issues/537)) | GIF/MP4/WebM/PNG/`.txt` | 20.7k★, v0.11.0, very active |
| **[autocast](https://github.com/k9withabone/autocast)** | yes (`expectrl`, waits for the real prompt — same architecture as OCX's recorder) | yes | `.cast` | 144★, **last push 2024-05** — stale |
| **[termsvg](https://github.com/MrMarble/termsvg)** | yes (live capture) | same caveat as asciinema | `.cast` + SVG | 384★ |

## Recommendation — extend the existing recorder; do NOT add a VHS path

**One sentence: add an optional `# shell:` header key and a shell parameter to `CastRecorder`, and let a
cast script's `# region cast` block include its own hook-registration line.**

The repo has a deliberately closed drift class. `test/recordings/conftest.py` documents **EQ3 —
one-tree convergence**: cast scripts come from "the same one tree the publish seam
(`doc_scripts_export`) and the drift gate read; there is no legacy `recordings/scripts/` glob and no
second discovery path." `design_spec_doc_command_scripts.md` §6i states EQ1/EQ2/EQ3 as tested
invariants.

A VHS path is a genuine second pipeline: a second script format (`.tape`, not `.sh`), a second
discovery mechanism, a second build stage, and — because `.tape` has no cast-region or state-provider
concept — no reuse of `resolve_state` / `display_map` / `rewrite_command`, so either duplicated
sanitization or an un-sanitized second class of cast. That is exactly the shape EQ3 exists to prevent.
It also adds a Go binary plus Docker to a repo whose tech strategy has no Go entry.

**The gap VHS closes, OCX does not have.** `pexpect.spawn("/bin/bash", …)` is already a real
interactive shell; the missing pieces are a selectable shell and letting the cast region contain the
activation line. Both are a few lines in `CastRecorder.__init__`/`open()`. The header grammar already
has the extension point: `test/src/doc_scripts.py::parse_doc_header` parses `# state:`, `# doc:`,
`# cast:`, `# title:`, `# description:`, `# expect:` into one `DocScriptMeta` — a new optional
`# shell:` key defaulting to bash fits the same one-tree grammar, same discovery, same orphan sweep.

Concrete script shape:

```sh
#!/usr/bin/env bash
# state: setup:full-catalog
# doc: in-depth/shell-integration-enter-leave
# title: The toolchain appears and disappears with cd
# region cast
eval "$(ocx self activate --shell=bash)"
ocx add "$PKG_KITWARE_CMAKE"
cmake --version          # on PATH — no `ocx exec`, no `ocx run --`
cd /tmp
cmake --version || true  # gone — the prompt hook reconciled PATH on the way out
# endregion cast
```

**Rejected options and their concrete cost.** (b) A VHS path reopens EQ3: the `.tape` is not the
transcluded prose source, so "one script = prose + cast" breaks for this page class and a human can
edit either half without the other noticing. (c) Hand-recording and committing the `.cast` violates the
"never edit generated files" rule and the drift class the one-tree invariant closed.

## Pitfalls, and how OCX's recorder already handles each

| Pitfall | Field practice | OCX recorder |
|---|---|---|
| Prompt-string non-determinism (hostname, cwd, git branch, starship glyphs) | VHS has no scrubbing — author controls `PS1` in the tape or records in a clean container | **Already solved** — `open()` overwrites `PS1` with a private sentinel never emitted to the cast; the real prompt never appears |
| Timing / waiting for a slow command | VHS: manual `Sleep`, opt-in `Wait /regex/` | **Already better** — `_read_until_prompt` polls the sentinel automatically on every command, no per-line authoring burden |
| ANSI colour | preserved by asciicast v3 and VHS | preserved; only spinner-erase noise is stripped (`strip_progress`, `realign_tables`) |
| Terminal width | VHS `Set Width/Height`; asciinema `--cols/--rows` | `CastRecorder(width=100, height=24)`; `auto_height()` trims post-hoc |
| `$?` preservation across the hook | the ADR names it as an acceptance requirement; bash 5.1 added the array form of `PROMPT_COMMAND` ([bash NEWS](https://tiswww.case.edu/php/chet/bash/NEWS)) | the silent `echo $?` cycle runs *after* the pty already returned to a prompt — worth a regression check once a real hook exists |
| Hostname/username leakage | asciicast v3 captures only `SHELL` by default; VHS relies on a clean environment | structurally impossible via the sentinel-`PS1` trick |

## Sources

Repo: `test/recordings/{cast_recorder,conftest,cast_layer}.py`, `test/src/doc_scripts.py`,
`website/recordings.taskfile.yml`, `test/tests/test_shell_activation.py`,
`.claude/artifacts/design_spec_doc_command_scripts.md` §6i,
`.claude/artifacts/adr_tested_doc_command_mechanism.md`.
External: links inline above.
