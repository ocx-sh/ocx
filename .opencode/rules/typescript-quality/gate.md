---
title: The TypeScript Gate
summary: Type-aware lint wiring, the extension-rule trap, Biome/ESLint parity, and one command that runs locally and in CI
---

# The TypeScript Gate

What runs, in what order, and whether it can go red. Owns the wiring that makes
type-aware rules available, the config traps that silently disable a check, and
the single command CI invokes. It does not own code shape, error handling, or
async semantics — those are separate rule sets, and a rule here never restates
one of theirs.

Contents: [One Command](#one-command) ·
[Turning Type-Aware Linting On](#turning-type-aware-linting-on) ·
[Choosing Rules](#choosing-rules) · [Biome](#biome) ·
[What No Linter Enforces](#what-no-linter-enforces) ·
[What Agents Get Wrong](#what-agents-get-wrong-here)

Enabling a type-checked preset at all is **TS-TOOL-03**; the `typescript`
version ceiling that makes it installable is **TS-TOOL-01**; a `lint` script
that resolves to a real config is **TS-TOOL-04**. This file is everything
*after* the preset is on.

Version-bound claims below were measured 2026-08-29 against
typescript-eslint 8.68.0 and Biome v2.5.11. Rule group membership and preset
membership both move between releases — re-read the generated config source at
your pinned tag rather than a rendered table or a blog post.

## One Command

The gate is one named target that chains lint → typecheck → test, and CI calls
that target. Everything in this block is caught by running it and reading its
exit code — a gate whose exit code is always `0` is the failure this section
exists to prevent, and it looks identical to a passing gate from the outside.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-GATE-07 | A repo whose lint config sets any rule to `warn` runs its linter with `--max-warnings 0`. | Without the flag, every `warn` rule fires and the process still exits `0` — the rules are on, the gate reports, and nothing blocks. | `rg -n -e '"warn"' -e "'warn'" eslint.config.*` — a union over both quote styles. Non-zero output with no `max-warnings` hit from `rg -n 'max-warnings' package.json` is the violation; empty from the first is the pass. | MUST |
| TS-GATE-05 | One named target (`task check`, `npm run check`, or equivalent) chains lint, typecheck and test, and every CI job's `run:` line invokes that target rather than restating its steps. | A hand-copied step list drifts from the local gate silently: the contributor's command and CI's diverge from the first day, and the difference surfaces as a failure nobody can reproduce. | `rg -n -e 'eslint' -e 'tsc ' -e 'vitest' -e 'biome' .github/workflows` — a union over the tool names. Any hit is CI restating a step; the pass is CI naming only the aggregate target. | MUST |
| TS-GATE-03 | A repo that enables typed linting keeps its standalone `tsc --noEmit` (or `vue-tsc --noEmit`) script and runs it as a separate gate step. | Typed lint is not a diagnostic superset: a plain type mismatch with no rule probing for it produces no lint output at all. Nor is it cheaper — the typed lint run builds its own program and does not reuse the typecheck's. | `rg -n -e '"typecheck"' -e '"check-types"' package.json` — a union over the two common script names; empty output in a repo whose lint is type-aware is the violation. | MUST |
| TS-GATE-06 | The lint invocation's path argument is `.`, and every `ignores` entry resolves to build output or `node_modules`. | An `ignores` entry or a narrowed glob removes *all* rules from those files, not only the type-aware ones — `no-unused-vars` included. Build scripts and the lint config itself are the files most often dropped this way. | `rg -n '"lint"' package.json` shows the argument. Then read every `ignores` entry: one naming a file that ships or executes is the violation. Out-of-tsconfig files are fixed by TS-GATE-01, never by `ignores`. | MUST |

Measured on two small repos (8.3k and 4.5k source LOC), typed ESLint cost
2.0–2.2× the bare `tsc --noEmit` it is often assumed to replace. Upstream's own
guidance is that lint time should be roughly build time; budget for double the
typecheck instead, and time both on the target repo before promising a number.

## Turning Type-Aware Linting On

Flipping `projectService: true` and stopping is the single most common way this
lands broken. Every rule in this block is caught by one command — run
`npx eslint .` over the **whole repo**, not `src`, and read the parsing errors:

```
error  Parsing error: <path> was not found by the project service.
       Consider either including it in the tsconfig.json or including it in
       allowDefaultProject
```

Any such line means the wiring is incomplete. It never means the file should be
ignored.

| Repo shape | Wiring it needs |
|---|---|
| One `tsconfig.json` whose `include` already covers everything you lint | `projectService: true`, nothing else |
| `include` narrower than the lint glob — the common case, and usually true of the lint config file itself | plus `allowDefaultProject` listing each out-of-project file |
| Solution-style root (`"files": []` plus `references`) | `projectService: true` alone; the references resolve |
| A sibling tsconfig **not** named `tsconfig.json` | plus a `files`-scoped block using legacy `project`, with `projectService: false` inside that block |
| More than 8 files matched by `allowDefaultProject` | raise `maximumDefaultProjectFileMatchCount_THIS_WILL_SLOW_DOWN_LINTING`, or move the files into a tsconfig |

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-GATE-01 | Every config that sets `projectService` also sets `allowDefaultProject`, listing every file the repo's tsconfigs do not `include` — at minimum `eslint.config.*`, plus the test tree and each `*.config.ts` at the repo root. | A tsconfig that includes only `src` throws a hard parsing error on the very file defining the lint config. The reflex fix is an `ignores` entry, which removes those files from every rule instead of one. | `npx eslint .` after the flip; any `was not found by the project service` line means the list is incomplete. Empty output is the pass. | MUST |
| TS-GATE-02 | A repo holding a tsconfig not literally named `tsconfig.json` gets a `files`-scoped block using the legacy `project` option, with `projectService: false` inside that block, covering exactly that tree. | `projectService` auto-discovery walks up directories looking only for the literal name `tsconfig.json`, and has no config surface to add a second one. The excluded tree is then unlinted, with no error to say so. Setting both options in one block is itself an error. | `find . -iname 'tsconfig*.json' -not -path '*/node_modules/*'` — every result whose basename is not `tsconfig.json` needs its own block, or its `include` tree is silently unlinted. | MUST |

## Choosing Rules

Caught by reading the config's rules block against the preset's generated source
at your pinned tag — not against the rendered rules table, which has given
inconsistent counts across fetches.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-GATE-13 | Enabling a typescript-eslint extension rule sets the identically-named core ESLint rule `'off'` in the same config object. | 25 typescript-eslint rules shadow a core rule. With both on, either both fire or the later-declared one silently wins depending on config order. `no-throw-literal` → `only-throw-error` and `no-return-await` → `return-await` are full renames: the old names no longer exist, so a hit on either is stale config doing nothing. | `rg -n -e 'no-unused-vars' -e 'no-shadow' -e 'no-use-before-define' -e 'no-magic-numbers' -e 'max-params' -e 'require-await' -e 'no-throw-literal' -e 'no-return-await' eslint.config.*` — a union over the highest-traffic extension names. A hit lacking the `@typescript-eslint/` prefix, in a config that also enables the prefixed twin, is the violation. | MUST |
| TS-GATE-04 | A `no-unsafe-*` rule is disabled only inside a config object carrying a `files:` array naming the files that touch the untyped surface, with a comment naming that surface. | This family is the only mechanical defence against `any` flowing out of an untyped dependency. A repo-wide disable to quiet a handful of import sites deletes it everywhere, and the config then claims less than it does. | `rg -n 'no-unsafe-' eslint.config.*` — for every hit set to `"off"`, confirm a sibling `files:` key in the same object. A disable with no `files:` key is the violation. | MUST |
| TS-GATE-14 | Do not add `prefer-readonly-parameter-types`, `detect-object-injection`, `detect-non-literal-require`, `detect-child-process`, or `detect-possible-timing-attacks`. Overridable per repo — unban one only after counting real hits in your own tree. | The first self-disqualifies in its own docs ("skip this rule if your project does not attempt to enforce strong immutability guarantees of parameters"). The other four come from a plugin whose README concedes it "finds a lot of false positives which need triage by a human" — a direct conflict with a gate that has no human in the loop. Every bracket-index lookup on a typed object trips `detect-object-injection`. | `rg -n -e 'prefer-readonly-parameter-types' -e 'detect-object-injection' -e 'detect-non-literal-require' -e 'detect-child-process' -e 'detect-possible-timing-attacks' eslint.config.* package.json` — a union; any hit is review-blocking, empty output is the pass. | MUST |
| TS-GATE-15 | Do not hand-pick a type-aware rule outside the repo's chosen preset unless a measured hit on that repo's own code justifies it, named in a comment beside the rule. | 13 of the 61 type-aware rules ship in no preset at all, and the ones that catch real bugs on first run are already inside `recommendedTypeChecked`. "More rules is safer" is how a config becomes unmaintainable, and each addition costs program-build time on every run. | `rg -n '@typescript-eslint/' eslint.config.*` — each hit outside the preset must carry a comment naming the finding that justified it. An uncommented addition is the finding. | SHOULD |

## Biome

Caught by `biome lint` and by reading `biome.json`. The trap is that Biome
accepts a misplaced rule key without complaint: a "clean" run after an edit is
not evidence the edit took effect.

```jsonc
// wrong — silently ignored, no validation error, lint stays green
{ "linter": { "rules": { "noFloatingPromises": "error" } } }
```

```jsonc
// right — every rule key nests under its group
{ "linter": { "rules": { "nursery": { "noFloatingPromises": "error" } } } }
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-GATE-09 | Write every Biome rule key nested under its group (`linter.rules.<group>.<ruleName>`), and look the group up per rule rather than deriving it from the name. | An ungrouped key is silently ignored — not a config error. ESLint's flat rule namespace trains exactly the wrong reflex, so the tell is a lint run showing zero new diagnostics after "enabling" a rule, never a validation failure. | Run `biome lint` before and after the edit and compare the diagnostic count. An unchanged count means the key did not take. | MUST |
| TS-GATE-12 | After `biome migrate eslint`, hand-check that `noFloatingPromises`, `noMisusedPromises`, `noUnsafeTypeAssertion` and `useReadonlyClassProperties` were actually ported. | Biome's published rule-sources cross-reference — which the migration draws from — omits all four typescript-eslint counterparts, even though three name their source on their own rule page. The tool's silence is not evidence the rule has no equivalent. | `rg -n -e 'noFloatingPromises' -e 'noMisusedPromises' -e 'noUnsafeTypeAssertion' -e 'useReadonlyClassProperties' biome.json` — a union; empty output after a migration is the finding, not the pass. | MUST |
| TS-GATE-10 | Set `nursery.noFloatingPromises`, `nursery.noMisusedPromises` and `style.useThrowOnlyError` to `"error"`. Overridable — `nursery` is opt-in upstream precisely because those rules may still have bugs. | These are the three real default-severity gaps against typescript-eslint's `recommendedTypeChecked`. Biome's own team measures `noFloatingPromises` at ~75% of the tsc-backed rule's catch rate; partial coverage of an unawaited promise beats none. | `rg -n -e 'noFloatingPromises' -e 'noMisusedPromises' -e 'useThrowOnlyError' biome.json` — a union; each hit must sit under its stated group. Then time `biome check`: any `types`-domain rule triggers a full-project scan. | SHOULD |
| TS-GATE-16 | Set `complexity.noExcessiveCognitiveComplexity` and `complexity.noExcessiveLinesPerFunction` to `"error"`. Overridable — raise the thresholds rather than deleting the keys. | Both are single-threshold, zero-config and off by default (15 and 50). This is the maintainability class an agent editing without human review most needs a mechanical backstop for, at the cost of one config line. | `rg -n -e 'noExcessiveCognitiveComplexity' -e 'noExcessiveLinesPerFunction' biome.json` — a union; empty output is the finding. | SHOULD |

## What No Linter Enforces

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-GATE-11 | No `any` crosses a typed parameter, property or return without an explicit narrowing check. In a Biome-only repo, write this contract into the repo's own rule text and label it unenforced-by-tooling. | Biome has no equivalent to `no-unsafe-assignment`, `no-unsafe-member-access`, `no-unsafe-call`, `no-unsafe-argument` or `no-unsafe-return` in any group, at any severity — confirmed against both its rule index and its cross-reference at v2.5.11. A reviewer who assumes the linter catches it is wrong, and nothing in the tool says so. | On ESLint repos: `rg -n -e 'no-unsafe-assignment' -e 'no-unsafe-member-access' -e 'no-unsafe-call' -e 'no-unsafe-argument' -e 'no-unsafe-return' eslint.config.*` — a union; each must be at `"error"` under a `*TypeChecked` config. On Biome repos the standing check is `rg -n --glob '*.ts' --glob '*.tsx' 'as unknown as' src` — every hit read by hand, because nothing else will. (TS-TOOL-10 owns the test-fake form of that cast.) | MUST |

## What Agents Get Wrong Here

1. **Flipping `projectService: true` and stopping.** The repo then throws on its
   own `eslint.config.js` and its test tree, and the next move is an `ignores`
   entry — which removes those files from every rule, not just the typed ones.
2. **Copying a working config wholesale from another repo.** That carries its
   carve-outs with it: an unscoped `no-unsafe-*` disable written for one repo's
   untyped dependency deletes the whole family in a repo that has no such seam.
3. **Dropping `tsc --noEmit` because "typed lint covers it."** It does not, and
   it is also slower — typed lint being slower is the proof it built its own
   program rather than reusing the typecheck's.
4. **Writing a Biome rule key ungrouped.** Silently ignored, no validation
   error, so the clean run afterwards reads as success.
5. **Trusting `biome migrate eslint` or the published rule-sources table.** Four
   rules that matter here are absent from Biome's own cross-reference.
6. **Reaching for a security plugin's full recommended set when asked to
   "harden" a project**, because it advertises itself as recommended while its
   README concedes high false-positive rates.
7. **Enabling a typescript-eslint extension rule beside its core twin**, or
   writing a name that no longer exists (`no-throw-literal`, `no-return-await`).
8. **Leaving `warn` severities behind a lint script with no
   `--max-warnings 0`** — a gate that reports everything and blocks nothing.
9. **Citing a stale rule count** from a blog post or a rendered table. Re-measure
   against the generated config source at your pinned tag.
