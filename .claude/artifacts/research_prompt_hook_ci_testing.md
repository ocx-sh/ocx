# Research: How Per-Prompt Shell Hooks Are Tested and Benchmarked in CI

**Date:** 2026-08-25
**Axis:** design patterns / known pitfalls
**Consumer:** [`adr_shell_env_overhaul.md`](./adr_shell_env_overhaul.md) Validation ladder (tiers 1–3),
the NFR Latency gate, and the Windows PowerShell 5.1 leg.

## 1. What the incumbents actually test

| Project | Harness | Shells | Real pty? | Windows PS leg |
|---|---|---|---|---|
| **[mise](https://github.com/jdx/mise/tree/main/e2e)** | bespoke bash e2e (`run_test`, `assert.sh`, `within_isolated_env`), `env -i` clean env | bash, zsh, fish, nu, xonsh | **No** — even `test_fish_first_prompt` execs fish non-interactively against a script file | none |
| **[direnv](https://github.com/direnv/direnv/tree/master/test)** | per-shell scripts (`direnv-test.{bash,zsh,fish,elv,tcsh,mx,ps1}`) | bash, zsh, fish, elvish, tcsh, murex, PowerShell | No | **script exists, CI skips it** — [`go.yml`](https://github.com/direnv/direnv/blob/master/.github/workflows/go.yml) gates those steps behind `if: runner.os != 'Windows'` |
| **[starship](https://github.com/starship/starship)** | `cargo test` only | none launched | No | Windows compiles/tests Rust; never sources `starship.ps1` |
| **[zoxide](https://github.com/ajeetdsouza/zoxide)** | `tests/completions.rs` only | none | No | no Windows leg (`ci.yml` is ubuntu-only) |
| **[atuin](https://github.com/atuinsh/atuin)** | `cargo nextest` on ubuntu/macos/windows | none | No | same as starship |
| **[oh-my-posh](https://github.com/JanDeDobbeleer/oh-my-posh/tree/main/e2e/harness)** | **real Go PTY harness** — `go-pty` + `vt10x` terminal emulator; `WaitForPrompt()` polls the rendered screen by regex every 50 ms, 30 s deadline | bash/zsh/fish/pwsh (Linux), pwsh + nu (Windows) | **Yes — the only one** | pwsh 7 only, **no 5.1** |

**Conclusion worth stating in the plan:** of six well-known incumbents, only oh-my-posh drives an
interactive shell to prove a hook fires, and even it skips WinPS 5.1. OCX's tier-3 pty requirement is
**already stricter than every incumbent's own suite** — a deliberate differentiator, not a bar to
water down toward an imagined industry norm.

## 2. The named hazards — upstream evidence

- **bash `PROMPT_COMMAND` array vs string, `$?` clobbering.**
  [direnv#155](https://github.com/direnv/direnv/issues/155): `_direnv_hook` `eval`'d ahead of the existing
  `PROMPT_COMMAND` and ate `$?`; fixed by save-on-entry / restore-on-exit.
  [starship#6962](https://github.com/starship/starship/issues/6962) (open) proposes the Bash 5.1+ array
  form to stop the class recurring. [atuin#2738](https://github.com/atuinsh/atuin/issues/2738) and
  [#1617](https://github.com/atuinsh/atuin/issues/1617) show the same coexistence failure between atuin
  and starship in the wild. Catchable only at tier 3: a command that fails immediately before the prompt,
  assert `$?` survives *through* the hook.
- **zsh `add-zsh-hook precmd` vs redefining `precmd()`.** No single canonical issue; the
  community-settled practice (starship, powerlevel10k docs,
  [p10k#1834](https://github.com/romkatv/powerlevel10k/issues/1834)) is unambiguous — register via
  `add-zsh-hook`, never overwrite `precmd`. A redefinition silently drops every other tool's hook, and
  only a tier-3 test with a *loaded* starship / oh-my-zsh / p10k catches it.
- **fish.** `--on-event fish_prompt` is additive by construction, but
  [fish-shell#8832](https://github.com/fish-shell/fish-shell/issues/8832) shows the event can silently
  fail to fire on a cancelled/syntax-error redraw — same class as nushell#14944. For index shift,
  [fish-shell#7776](https://github.com/fish-shell/fish-shell/issues/7776) (`fish_add_path`:
  `set --erase` incorrectly handles indices) is the **better citation than #8604/#9147**; fish's own
  guidance is one call with multiple indices (`set -e arr[1 2]`) or descending order.
- **nushell `env_change.PWD`.** [nushell#14944](https://github.com/nushell/nushell/issues/14944) —
  lowercase `pwd` never fired; fixed by [PR #18234](https://github.com/nushell/nushell/pull/18234),
  shipped in **v0.113.0**. Current stable **0.115.1** (2026-08-23). Not stale.
  **`hide-env` inside a hook block is NOT resolved**: [#6593](https://github.com/nushell/nushell/issues/6593)
  (hide works at the REPL but "not working right in the hook"),
  [#11818](https://github.com/nushell/nushell/issues/11818) and
  [#15872](https://github.com/nushell/nushell/issues/15872) (open, still tracking a real `unset`
  primitive), [#15013](https://github.com/nushell/nushell/issues/15013). Nushell's docs say hook blocks
  preserve their environment "in a similar way as `def --env`" — the exact scoping ambiguity the ADR's
  spike is right to distrust. **This validates gating the nushell work package on a red+green spike
  rather than citing upstream docs.**
- **PowerShell `$env:PATH` vs `$env:Path`.**
  [PowerShell#3571](https://github.com/PowerShell/PowerShell/issues/3571) is closed
  `Committee-Reviewed` / `Resolution-No Activity` — a **permanent platform divergence**, not a pending
  fix. Nobody in this survey asserts it in CI. The only approach shown to work is testing the real
  interpreter per platform, which OCX already does in
  `.github/workflows/shell-activation-deep.yml` (three legs: `shell: powershell` = WinPS 5.1,
  `shell: pwsh` = built-in PS7, a third for ocx-installed PS7).

## 3. Per-prompt latency in CI — the honest state of the art

**mise is the only incumbent that CI-gates a performance number, and it does not use wall-clock.**
[`perf.yml`](https://raw.githubusercontent.com/jdx/mise/main/.github/workflows/perf.yml) (post-merge
baseline) and [`perf-pr.yml`](https://raw.githubusercontent.com/jdx/mise/main/.github/workflows/perf-pr.yml)
(PR-gated) use **[jdx/tak](https://github.com/jdx/tak)**, which measures **retired CPU instruction
counts via Valgrind cachegrind**. Their published numbers: instruction counts vary by
**0.008–0.027 %**, wall-clock by **147–164 %** under CPU contention on the same runners. Their stated
rule: *gate on instruction counts; report wall time without gating on it.* Comparison is against the
**merge-base** (not branch tip) and pinned to one runner class, because absolute counts shift between
machine types by more than a real regression does.

Everyone else: **nobody CI-gates prompt latency.** starship's `starship timings` / `explain` are
user-facing diagnostics, never asserted. zoxide, atuin and direnv have no perf workflow. oh-my-posh
gates *binary size* (`binary_size.yml`), not render time; its widely-cited "<200 ms" is a docs goal.

**So the answer to "how do you make a wall-clock assert non-flaky on shared runners" is: you don't —
you change the metric.** The ADR's own design (`exec_floor + Δ`, floor measured in the *same job*,
relative rather than absolute, with a named fault-injected red state) is more rigorous than five of six
incumbents and structurally the same instinct as tak's. If cachegrind is available on OCX's Linux
runners it is worth evaluating as a second, harder-to-flake signal — **additive, not a substitute**,
since an instruction-count gate still needs its own red-state proof.

## 4. Fault injection / mutation proof

**Not established among any of the six.** No `cargo-mutants`, `mutmut` or stryker wired into CI for
mise, direnv, starship, zoxide, atuin or oh-my-posh. [`cargo-mutants`](https://mutants.rs/) is real and
adopted in the wider Rust ecosystem, and its own docs flag the prerequisite that matters here —
mutation testing needs deterministic, non-flaky tests, which is exactly why the ADR sequences fault
injection at tiers 1–2 rather than against tier-3 pty tests. No incumbent precedent exists for "prove
the mutation landed before trusting the result"; that is OCX's own practice (`quality-core.md`
"Unchecked Green"). State it plainly rather than inventing a citation.

## 5. Windows PowerShell 5.1 in CI

On `windows-latest`, `shell: powershell` invokes **`powershell.exe`** (WinPS 5.1) and `shell: pwsh`
invokes **`pwsh.exe`** (PS7); both are preinstalled
([GitHub Docs — Building and testing PowerShell](https://docs.github.com/en/actions/automating-builds-and-tests/building-and-testing-powershell)).
OCX's `shell-activation-deep.yml` already runs all three legs against
`test/manual/test-windows-activation.ps1`.

**Driving a genuinely interactive PS session in CI: not established, and nobody in this survey does
it.** direnv wrote a `.ps1` test and skips it on Windows. atuin/starship compile only. oh-my-posh's
`go-pty` harness does support the Windows ConPTY backend, so a true interactive Windows PTY is
technically reachable through it, but it targets pwsh 7 and not 5.1. `System.Management.Automation`
hosting and Pester are documented for unit-testing PowerShell *code*, not for driving a REPL.
**The field's practical technique — and OCX's existing gate — is non-interactive invocation of the
init script via `-File` under the real target interpreter**, which exercises activation-time behaviour
without a REPL. Tier-3 prompt-*firing* proof on Windows would need a bespoke ConPTY harness; do not
assume an off-the-shelf Python library covers it.

## 6. Techniques worth copying

1. **Gate perf on a relative/deterministic signal, never a raw wall-clock threshold** — keep the ADR's
   `exec_floor + Δ`; do not downgrade to an absolute number under review pressure. ([mise perf-pr.yml])
2. **Pin perf comparison to merge-base and to one runner class.** ([mise perf-pr.yml])
3. **Screen-scrape a rendered PTY via regex-on-`WaitForPrompt` with a bounded deadline**, not raw byte
   matching, if `script(1)` proves brittle for a shell. ([oh-my-posh e2e/harness])
4. **Treat every multi-element fish removal as index-shift-hazardous** — highest-index-first or one
   multi-index call. ([fish-shell#7776])
5. **Do not trust nushell's `hide-env` docs — verify on the pinned version.** ([nushell#6593], [#11818], [#15872])
6. **Save and restore `$?` explicitly around the bash hook body**, with a tier-2 fixture that fails a
   command immediately before the hook. ([direnv#155], [vscode#158090])
7. **Register zsh hooks via `add-zsh-hook`, and prove it under a loaded starship / oh-my-zsh / p10k**,
   not a bare shell — the one class no lower tier can catch.
8. **Test the real interpreter per platform for env-var casing**; extend the existing three-way Windows
   split rather than approximating 5.1 from Linux. ([PowerShell#3571])
