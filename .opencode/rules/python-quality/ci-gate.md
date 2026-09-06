# The CI Gate

What makes a rule enforced rather than merely written, and the order in which
to turn one on without ever landing red. Loads while adding a lint or type
gate, editing a workflow, or wiring an existing suite into CI.

Contents: [What Binds](#what-binds) · [Turning a Gate On](#turning-a-gate-on) ·
[Workflow Hardening](#workflow-hardening) ·
[What Agents Get Wrong](#what-agents-get-wrong-here)

Under agent-primary authorship, with no human on most diffs, the usual answer
changes. A pre-commit hook plus a lighter CI check is a reasonable pairing when
a person reviews the pull request; here the hook contributes approximately
nothing, and the whole file is built around that.

## What Binds

| Mechanism | Binds a human | Binds an unattended agent |
|---|---|---|
| Required, blocking CI status check | Yes | **Yes** — a red pull request cannot merge and there is no local step to skip; the only bypass is an explicit grant, itself a deliberate and logged act |
| Pre-commit hook | Weakly, by habit | **No** — needs a manual `pre-commit install` in that specific clone, and `--no-verify` or `SKIP=<hook-id>` bypasses it for one keystroke. An agent working in a fresh worktree has no reason to know it exists |
| Diff-scoped "no new violations" check | Only as a blocking check | Same as the blocking check — this is that mechanism with a different baseline, not a fourth one |
| Periodically-reviewed count | No | **No** — needs an attentive human on a cadence, which is exactly the resource agent authorship removed. A count nobody reads is a number, not a gate |

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-GATE-01 | A subject gets exactly one contributor-and-CI command (`task verify` or equivalent) **before** any lint or type rule is turned on for it. Turn a rule on first and the two invocations drift the moment someone runs the tool directly — which is then discovered as a CI failure nobody can reproduce locally. | `rg -n --glob 'taskfile.yml' --glob 'Taskfile.yml' --glob 'Makefile' -e 'verify:' -e 'ci:' <project-dir>` — empty output is the finding: no single command exists yet, so no gate may be added to this subject until one does. | MUST |
| PY-GATE-02 | Every gate is a required, blocking status check on the merge path — never a pre-commit hook as the primary mechanism, and never an advisory comment. A gate that an author can skip by choosing not to run it is documentation. It also has to be able to go red: see the softened-job check below. | `gh api repos/OWNER/REPO/rules/branches/main --jq '.[].type'` — the gate job's absence from a `required_status_checks` rule is the finding, and empty output means nothing gates the branch at all. Watched red on a live repository whose `main` had no ruleset. `git commit --no-verify` succeeding locally is expected and proves nothing either way. | MUST |
| PY-GATE-07 | A suite that is configured and passes but that no workflow invokes gets wired in during the same cycle that discovers it, not deferred. Wiring in a suite that is already green costs one line; leaving it costs a silent drift nobody sees until the day it is finally run and everything has broken at once. | `rg -l --glob 'pyproject.toml' 'tool.pytest.ini_options' <repo-root>` lists every project with a configured suite. For each one's directory name D: `rg -n --glob '*.yml' --glob '*.yaml' -e 'D/' -e 'D:' .github/workflows` — empty output for that name is the violation. Search the compound form, never the bare word: a bare directory name collides with unrelated workflow text and reports a false pass. | SHOULD |

A required check that cannot fail is worse than no check: it occupies the slot,
shows a green tick, and reports nothing. Two spellings soften a job into that
state, and both look like ordinary workflow hygiene in a diff —
`continue-on-error: true` on the job or the step, and a trailing `|| true` on
the `run:` line. Watched red on both, plus a bare `exit 0`:

```
rg -n --glob '*.yml' --glob '*.yaml' -e 'continue-on-error: true' -e 'run: .*\|\| true' -e 'exit 0$' .github/workflows
```

Every hit on a job named as a required check is the violation; empty output is
the pass. A deliberately advisory job — a trend report, an informational scan —
is a legitimate hit, and belongs nowhere in the required-checks list.

"Configured but unenforced" is a different finding from "never configured", and
only the first belongs here. A tree with no lint config is a decision someone
can make; a tree with a lint config nothing runs is a gate that reads as
present and is not.

Building that check is itself the cautionary tale. Three separate bugs made it
report a false pass before it was trusted: a `grep -q` under `pipefail`, where
the match closed the pipe, the upstream write took `SIGPIPE`, and the pipeline
reported failure *because* the search succeeded; and then two rounds of a token
search colliding with unrelated text — first a workflow whose job key happened
to be the searched word, then a doc comment mentioning a different directory of
the same name. Each fix was confirmed against both a known-guilty and a
known-clean subject, because the second bug's fix turned one guilty subject red
and left the other silently passing for a third, unrelated reason. **A check
that passes its first red test is not proven; it is proven once it has also
been run against everything it is supposed to leave green.**

## Turning a Gate On

Lint and type-check ride in the same job as the tests they gate, on every push
— they cost less than the run-to-run variance of the suite beside them. There
is no timing argument for deferring either to a nightly run or a merge queue,
and "we will add it later, for timing" should be named as the non-objection it
is when it is raised.

The sequence, in order, where every step leaves the repository green:

1. **Add the command, wire nothing.** `task lint` / `task types` / `task
   verify` exist; no workflow calls them. Zero violations possible.
2. **Land the config, safe autofix only.** Still not blocking.
3. **Fix the largest remaining bucket at its call sites**, with targeted
   suppressions naming the rule code — not a config-level ignore, which would
   also hide the one genuine instance living outside that bucket.
4. **Triage the named remainder, one pull request per rule code.**
5. **Turn the job blocking.** This is the only step that gates; everything
   before it was preparation that could not land red.
6. **Add the type check as a second blocking job**, scoped to the source tree
   first. Widening it to the test tree is its own project, not part of this one.

Step 3 is where the baseline argument is actually won. A large remainder is
never an undifferentiated pile — decompose it by rule code first and it turns
into a handful of independently landable slices, each with a different verdict:
one is a lint blind spot needing two targeted suppressions at named call sites,
one is real complexity to refactor, one is cosmetic and safe to fix in bulk,
one is worth a security read before touching. A baseline file exempts all of
them at once and erases exactly the structure that made them tractable.

The buckets also decide the *shape* of the suppression. A config-level ignore
for the largest bucket is tempting because it is one line — and it also hides
the one genuine instance of that rule living outside the bucket, which is
usually the only one that mattered.

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-GATE-03 | A lint adoption with more than roughly 200 violations left after safe autofix lands as named, bounded buckets per rule code — never a generated baseline, and never a directory-wide suppression. A baseline flattens the one structure that makes a large remainder tractable, and nothing ever forces it to shrink: every violation inside it is exempt forever, and "clean up the baseline" competes with every other backlog item indefinitely. | `rg -n --glob 'ruff.toml' --glob '.ruff.toml' --glob 'pyproject.toml' '"ALL"' <repo-root>` — a hit under `per-file-ignores` is the violation, empty is the pass; a hit under `select` is a different and legitimate choice. | MUST |
| PY-GATE-04 | Tools resolve through the project pin, never through `$PATH`. A globally installed linter shadowing the pinned one is not hypothetical — measured on a live development machine, `$PATH` resolved to 0.16.1 while the project pin was 0.16.3: two different linters, same machine, same moment, and the contributor sees a clean tree CI rejects. | `rg -n --glob '*.yml' --glob '*.yaml' -e 'run: ruff ' -e 'run: pyright' -e 'run: pytest' -e 'run: mypy' .github/workflows` — every hit invokes a bare tool name and is the violation; the pinned forms are `uv run …` or the task. Then run `ruff --version` and `uv run ruff --version` in the project directory: two different numbers is the violation. | MUST |
| PY-GATE-08 | A `# noqa` or `# type: ignore` always names its code. One line of config, not a paragraph: select ruff's `PGH` group, which denies both bare forms. Any new hit is an agent taking the fast path to green. | `ruff check --select PGH <repo-root>` — `PGH003` and `PGH004` findings are the violation. Watched red on a bare `# noqa` and a bare `# type: ignore`, silent on `# noqa: F401` and `# type: ignore[assignment]`. | SHOULD |

## Workflow Hardening

Not every workflow finding is worth the same attention. Interpolation into a
`run:` block is the one that converts data into script, and it is the one to
fix first even when the trigger is maintainer-only. Floating action refs are
next, and they cluster in the release workflow — the file whose pinning
discipline usually lapsed precisely because it is edited least. Missing
`persist-credentials: false` is high-volume, low-severity, and auto-fixable in
bulk; a flagged trigger class on a workflow that checks the untrusted tree out
as data, executes nothing from it, and runs at zero default permissions is a
tool correctly raising a flag that a correct implementation survives — document
the reasoning next to the trigger rather than suppressing the rule.

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-GATE-05 | `${{ }}` never appears directly inside a `run:` block; every value flows through an `env:`-declared intermediate variable first. A branch name, tag or input interpolated straight into a shell line is substituted before the shell parses it, so the value becomes script rather than data. | `rg -n --glob '*.yml' --glob '*.yaml' 'run: .*\$\{\{' .github/workflows` — every hit is the violation, and an `env:` intermediate is correctly not matched. That grep provably misses the multi-line block form: watched silent on an interpolation two lines below a `run:` key. So the check that binds is `zizmor --format plain .github/workflows` reporting zero `template-injection` findings; the grep is the zero-install partial, not a substitute. | MUST |
| PY-GATE-06 | Every third-party action is pinned by commit SHA with a version comment, in *every* workflow — the release one included. The convention usually holds everywhere except the file that ships the artifact, which is the highest-stakes place for it to lapse. | `rg -n --glob '*.yml' --glob '*.yaml' -e 'uses: [^@]+@v[0-9]' -e 'uses: [^@]+@main' -e 'uses: [^@]+@master' .github/workflows` — every hit is a floating ref and the violation; empty is the pass. A SHA pin and a local `uses: ./…` are both correctly unmatched. | SHOULD |

## What Agents Get Wrong Here

1. **Adding a pre-commit hook and calling the gate done.** It looks like
   enforcement, it is one flag away from nothing, and the agent that adds it is
   the same agent that will not run it.
2. **Generating a baseline to make a large adoption land in one pull
   request.** The diff looks decisive; the remainder never shrinks again.
3. **Landing the whole remainder in one pull request** by suppressing what the
   safe autofix left, rather than in slices anyone could review.
4. **Suppressing a whole directory to clear the last bucket** instead of the
   handful of call sites that actually need it.
5. **Invoking a bare tool name in a workflow** because it works locally —
   silently gating against whatever version the runner image happens to ship.
6. **Interpolating a ref or input straight into `run:`** while correctly using
   an `env:` intermediate three lines above, in the same file.
7. **Softening a required job with `continue-on-error` or a trailing truthy
   command** to unblock a merge, leaving a check that can only ever be green.
8. **Turning a gate on before the single command exists**, so the contributor's
   invocation and CI's diverge from the first day rather than the hundredth.
