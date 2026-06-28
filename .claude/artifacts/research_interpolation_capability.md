# Research: Context-Scoped Interpolation — validating OCX `Usage`→`AllowedTokens`

**Date:** 2026-06-28 · **For:** [`adr_entrypoint_args_interpolation.md`](./adr_entrypoint_args_interpolation.md) D6
**Question:** Do mature tools model interpolation where different call sites permit different
token subsets, and does OCX's locked `Usage` enum → `AllowedTokens { deps: bool }` shape hold?

## VERDICT

**Design holds. No hard blocker.** One implementation-correctness fix (not a design change):
the capability gate must fire on `${deps.` detection **before** the dep-token regex/substitution,
producing a dedicated `DisallowedToken` error — otherwise the case collapses to a misleading
`UnknownPlaceholder` (and, with real `dep_contexts`, would *resolve* a dep that args must forbid).

## Precedent table

| Tool | Mechanism | Per-context restriction? | Error when token disallowed-but-recognized? |
|---|---|---|---|
| **GitHub Actions** | static `Map<Location, AllowedContextSet>` (17+ YAML locations × 8+ contexts) | Yes — explicit table | `Unrecognized named-value: 'secrets'` — **same as unknown** (anti-pattern) |
| **mise** | per-stage Tera context constructor (`.miserc.toml` vs task script get different var sets) | Yes — structural (var absent from context object) | Tera "variable not found" — also conflated |
| **Nix** | string *context* as a type-level set propagated through ops | Yes — implicit/type-level | type error — opaque |
| **Cargo `[env]`** | no interpolation in values; build scripts get fixed Cargo env | n/a | n/a |
| **Ansible/Jinja2** | single global scope, no per-location gating | No | n/a |
| **Handlebars** | `strict` / `knownHelpersOnly` are binary, not per-site | Binary only | "missing helper" / silent empty |

**Dominant pattern:** GitHub Actions + mise both use a per-context allow-set (lookup table /
constructor), **not** a trait registry or token-kind polymorphism. OCX's shape matches the
proven pattern.

## Pitfalls of the intent-enum → capability-set shape

1. **Error-path conflation (the one that matters).** GHA's documented mistake
   ([actions/runner#520](https://github.com/actions/runner/issues/520)): a globally-recognized
   token used in a disallowed location errors identically to a typo. OCX must avoid this →
   distinct `DisallowedToken` variant. **Correctness corollary:** `${deps.foo.installPath}`
   matches `DEP_TOKEN_PATTERN`; gating must happen *before* that regex, or with real
   `dep_contexts` the engine would substitute a dep path in args (violating D3). Gate first.
2. **Validation site must carry the usage.** `validate_entrypoint_args` must invoke the engine
   with `Usage::EntryPointArgs`, not `Environment`. Correct separation; means validation.rs grows
   a second pass with the restricted cap set.
3. **`bool` vs enum for `deps` — negligible.** `AllowedTokens { deps: bool }` is right-sized; a
   third kind (`${env.NAME}`) makes it `{ deps: bool, env_refs: bool }` — still flat, readable.

## Error-UX norm

No surveyed tool emits a distinct "disallowed in this context" error — all conflate it with
"unknown/not-found." OCX's dedicated error is an **improvement over every comparable tool** and
correct for publisher-facing tooling (saves debugging the difference between typo and wrong-place).

## YAGNI verdict on trait/registry

GHA handles 8+ context kinds × 17+ locations with a flat table; mise handles 10+ vars with a
per-context struct. At OCX's 2 token kinds / 2 consumers, a trait-per-token registry is pure
over-engineering. `Usage`→`AllowedTokens` is the simplest correct shape. Confirmed.

## Actionable for the plan

- Add `TemplateError::DisallowedToken { token: String }` (carries the offending token text);
  classify `DataError`. Fire it as the **first** step in resolve when `!allowed.deps` and the
  template contains `${deps.`, before any regex/substitution.
- Both consumers' publish-validation route the unknown-placeholder scan through the shared
  tokenizer; args validation uses `Usage::EntryPointArgs`.

## Sources
- GitHub Actions context availability matrix — https://docs.github.com/en/actions/reference/workflows-and-actions/contexts
- actions/runner#520 (conflation anti-pattern) — https://github.com/actions/runner/issues/520
- mise templates (per-context var sets) — https://mise.jdx.dev/templates.html
- Nix string context — https://nix.dev/manual/nix/2.26/language/string-context
