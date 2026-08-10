# Research: Unified Interpolation Token Grammar

**Date:** 2026-08-09 · **For:** [ocx-sh/ocx#303](https://github.com/ocx-sh/ocx/issues/303) —
`${self.*}` namespace, scoped env, render modifiers, `$${…}` escape
**Feeds:** `adr_interpolation_token_grammar.md`
**Supersedes nothing.** Builds on [`research_interpolation_capability.md`](./research_interpolation_capability.md)
(per-context allow-sets, the GitHub Actions error-conflation anti-pattern) — that artifact's
verdict still holds and is not restated here.

> Three research axes were run in parallel (grammar prior art, parser design, path rendering).
> They are consolidated into one artifact rather than three files: the axes cross-reference each
> other heavily, and the project convention is one `research_<topic>.md` per topic.

---

## PART 0 — Current state (discovery)

Five read-only explorers mapped the live code. These findings are the load-bearing constraints;
several contradict what the prior ADR text implies.

### 0.1 There is no tokenizer

`adr_entrypoint_args_interpolation.md` D6 states "One tokenizer scans `${…}` segments and
classifies them." **That type was never built.** The live implementation is three independent
mechanisms:

| # | Mechanism | Location | Shape |
|---|---|---|---|
| 1 | Literal substring match + `str::replace` | `template.rs:174`, `:209` | `template.contains("${installPath}")` — no regex, no variable body |
| 2 | Dep regex | `slug.rs:25-29` — `DEP_TOKEN_PATTERN` | `\$\{deps\.([a-z0-9][a-z0-9_-]*)\.([a-zA-Z]+)\}` |
| 3 | Leftover catch-all | `validation.rs:35-36` — `UNKNOWN_TOKEN_RE` | `\$\{[^}]*\}`, publish-time rejection only, never substitutes |

No `Token`/`Segment` enum exists. The "shared tokenizer" is two free functions —
`disallowed_dep_token` (`template.rs:64-68`) and `first_unknown_placeholder`
(`validation.rs:42-48`) — plus the one regex.

### 0.2 The grammar is closed-world today; #303 flips it open

`first_unknown_placeholder` is an allowlist: anything that is not exactly `${installPath}` or a
`DEP_TOKEN_PATTERN` match is rejected at publish with `TemplateError::UnknownPlaceholder`
(exit 65). Consequences:

- `${workspaceFolder}`, `${localEnv:HOME}`, `${env:HOME}` are **unpublishable today**.
- There is no existing pass-through behaviour to preserve — #303's pass-through guarantee is
  wholly new grammar, not a relaxation.
- **Forward-compat is already safe on the reject side**: an old `ocx` reading metadata published
  under the new grammar fails closed rather than mis-substituting. Only the accept side is new work.

Repo-wide census: no `${workspaceFolder}` or `${localEnv:` string exists anywhere in `crates/`,
`website/`, `test/`, or fixtures. Clean slate.

### 0.3 `$${installPath}` today silently double-resolves

`.contains("${installPath}")` is a plain substring test, so `${installPath}` matches at index 1 of
`$${installPath}`. `str::replace` substitutes the inner occurrence and leaves the leading `$`:

```
"$${installPath}"  →  "$/home/…/content"
```

Not an error, not an escape — a stray `$` prefix. **A naive `str::replace`/`captures_iter` cannot
implement the escape**; it requires a real left-to-right scanner.

### 0.4 Composition and resolution are fused per-var — the central constraint

`composer::compose` (`composer.rs:196`) walks deps in topological order, then the root. Per package
it builds that package's own `dep_contexts` (`build_dep_context_map`, `:507-528`, **direct deps
only**), then loops `for var in env { EnvResolver::resolve(var) }` (`:545`, `:575`), pushing each
`Entry` immediately into the shared `entries: Vec<Entry>`.

There is **no "resolve then compose" ordering to exploit.** `TemplateResolver`'s only inputs are
`install_path: &Path` and `dep_contexts` — it has no concept of a composed env at all. A var that
fails `carrier_crosses` is `continue`d *before* `resolve()` is called (`:551-553`, `:578-580`).

Therefore `${self.env.VAR}` is **a materially new capability, not a grammar extension**:

- **Launcher-args case is easy.** `exec.rs:159-172` resolves baked `args` *after* the composed
  self-view `entries` already exists (`exec.rs:108-120`, `self_view` hardcoded `true` at `:111`).
  `entries` can simply be threaded into a new resolver capability. #303's hypothesis is confirmed
  by the code, not just convention.
- **Var-referencing-var case is hard.** `FOO=${self.env.BAR}` in the same package has no data
  structure to read from and no second pass. Needs either two-pass resolution or a topological
  order over vars — which itself needs cycle detection.

Related unresolved facts: duplicate `key`s across two `Var`s are permitted today (no uniqueness
enforced, `env.rs:16-19`); and it is undecided whether `${self.env.VAR}` sees surface-gated-out
vars or only vars surviving `carrier_crosses`.

### 0.5 Alias regression risk: `classify_install_path_rooted_dir`

`template.rs:86-97` parses the literal prefix `"${installPath}/"` to feed `bin_scan`'s executable
auto-scan. **If `${self.installPath}` becomes an accepted alias without updating this classifier,
`bin_scan` silently stops recognising `${self.installPath}`-authored `Path` vars as scan targets.**
Silent, not an error. Same class of hazard: `first_unknown_placeholder`'s exclusion set must gain
every new recognised form in the same commit, or newly-valid metadata fails to publish.

### 0.6 Path rendering today

Both substitution sites use the same idiom — `dunce::simplified(path).to_string_lossy()`
(`template.rs:193-194`, `env/dep_context.rs:94-97`). `env/resolver.rs:67-97` re-strips via
`dunce::simplified` after substitution, because a bare relative value that never went through
substitution still inherits the verbatim prefix through `install_path.join(relative)`.

On Windows this renders **native backslashes**. Three dedicated regression tests
(`env/resolver.rs:402-498`) pin the `\\?\`-verbatim-prefix behaviour — any `:posix` work must not
regress them. **Zero posix/native conversion code exists anywhere in the tree**, including
`ocx_shim`. `MAIN_SEPARATOR`, `to_slash`, `from_slash`, `.replace('\\', '/')` return no hits under
`crates/**` for this purpose. The `:posix` transform is net-new.

### 0.7 `Modifier` is a different axis — naming collision

`env::modifier::Modifier` answers "how does this var's value **combine** with an existing value?"
(`path`/`constant`/`list`) and is wire-visible as the `"type"` JSON tag. #303's `:posix` answers
"how is this resolved value **rendered**?" and is never serialized. **They must not both be called
"modifier" in user-facing docs** or a reader conflates `${installPath:posix}` with
`{"type":"path"}`.

### 0.8 Schema impact is prose-only

No token syntax is `pattern`-constrained anywhere in the generated schema — `Path.value`,
`Constant.value`, `List.value` are plain derived `String` fields whose `///` doc comments become
the schema `description`. `Entrypoints`' manual `JsonSchema` impl (`entrypoint.rs:311-328`) spells
the args rule in prose (line 320). So **no schema shape change is required** — but four
description sites plus the `template.rs:4-7` module doc go stale unless updated.

Also flagged: `ProjectEnv` (`project/env.rs:28`) states "Values are literal in v1. No interpolation
of any kind." A `${installPath}` typed into `ocx.toml`'s `[env]` passes through as a dead literal.
Any doc wording about "interpolation" must not imply the project tier participates.

---

## PART 1 — Grammar prior art (axis 1)

**Verdict: no surveyed tool combines dot-namespacing with colon-modifiers. #303's split — `.` for
namespace, `:` for a closed-enum modifier — is a safer synthesis of two separately-proven ideas,
not a copy of an established pattern.**

### 1.1 The colon-overload regret

devcontainer.json overloads `:` as *both* namespace divider and default-value divider:
`${localEnv:VAR}` extended to `${localEnv:VAR:default}`
([spec](https://containers.dev/implementors/json_reference/)). A default value containing its own
colons (a URL) truncates at the second colon —
[devcontainers/spec#565](https://github.com/devcontainers/spec/issues/565), **open, unresolved**.

**#303 is structurally immune**: its `:modifier` never carries free text (`posix`/`native` only).
State that as a designed invariant in the ADR so a future modifier addition cannot reintroduce it.

### 1.2 `token:modifier` is precedented and uncontroversial

- webpack `[contenthash:8]`, `[contenthash:base64:8]` — [docs](https://webpack.js.org/configuration/output/)
- CMake `$<TARGET_PROPERTY:tgt,prop>` — [cmake-generator-expressions(7)](https://cmake.org/cmake/help/latest/manual/cmake-generator-expressions.7.html)

Neither combines it with dotted namespacing, but both confirm `:` as a bare modifier separator is
familiar and safe.

### 1.3 Escape scope, not escape choice, is where tools get bitten

`$$` → literal `$` is the dominant convention: [GNU Make](https://www.gnu.org/software/make/manual/html_node/Variables-in-Recipes.html),
[Bazel](https://bazel.build/reference/be/make-variables),
[Docker Compose](https://docs.docker.com/reference/compose-file/interpolation/).

Kubernetes' *scoped* `$$(VAR)` escape had a real bug where a bare `$$` with no trailing paren was
still silently collapsed — [kubernetes#101137](https://github.com/kubernetes/kubernetes/issues/101137).

**Actionable:** OCX's escape must match `$$` immediately followed by `{`, never bare `$$`.
Add a test for a literal `$$` *not* followed by `{` surviving untouched.

GitHub Actions ships **no** escape for `${{ }}` at all — consistent with its "own a unique
delimiter, don't bother escaping" strategy. That strategy is unavailable to OCX, which must share
`${…}` with foreign consumers.

### 1.4 Over-claiming is a real bug class with no solved pattern

`envsubst` claims every `$VAR`/`${VAR}` unconditionally and is well known to corrupt Kubernetes
manifests — the entire reason `renvsubst` exists
([war story](https://jellepelgrims.com/posts/dollar_signs),
[Kustomize discussion](https://dev.to/zenika/kubernetes-a-convenient-variable-substitution-mechanism-for-kustomize-lhm)).

**No surveyed tool documents "claim only my namespace, pass the rest through byte-identical on the
same delimiter" as a tested pattern.** GHA and devcontainer.json both dodge the problem (unique
delimiter / closed vocabulary) rather than solve it. This is the one place #303 is at the frontier
rather than following precedent — so it needs its own golden-fixture test enumerating every foreign
token, not just spec prose.

### 1.5 Aliasing a bare token, keeping both forever

Closest precedent is Bazel: `$(location)` is documented as legacy — "not recommended unless you
know what it does" — while `$(execpath)`/`$(rootpath)` are the steered-toward successors. Both work
forever, no deprecation machinery
([Make Variables](https://bazel.build/reference/be/make-variables),
[bazelbuild/bazel#25204](https://github.com/bazelbuild/bazel/issues/25204)).

Matches OCX's pre-1.0 no-shim doctrine. `${installPath}` stays; docs steer to `${self.installPath}`.

Note: OCX's own archived research already anticipated this — `archive/research_env_interpolation.md`
says the bare form "should become an alias for `${self.installPath}` in a future migration."

### 1.6 "Own composed env" vs "ambient env"

GitHub Actions is the direct precedent: `${{ env.NAME }}` is interpolated pre-dispatch from the
workflow's own composed env block; `$NAME` is the runner's live OS environment, resolved later,
different syntax entirely
([contexts](https://docs.github.com/en/enterprise-server@3.6/actions/learn-github-actions/contexts),
[writeup](https://brandur.org/fragments/github-actions-env-vars-in-env-vars)).

The `self.` prefix already achieves the syntactic half of that separation. Nothing more needed.

---

## PART 2 — Parser design and pitfalls (axis 2)

**Verdict: build a hand-written single-pass scanner. No crate implements the requirement.**

### 2.1 Crate survey — seven checked, none fit

| Crate | Pass-through unknown | `$$`-style escape | Modifier suffix |
|---|---|---|---|
| [`shellexpand`](https://docs.rs/shellexpand/latest/shellexpand/) | **Yes** (opt-in `env_with_context_no_errors`) | No | default-value only |
| [`subst`](https://github.com/fizyr/subst) | No — errors | No | default-value only |
| [`envsubst`](https://github.com/coreos/envsubst-rs) | No (unset → empty) | No | No |
| `strfmt` | No | `{{`/`}}` only | No |
| [`tinytemplate`](https://docs.rs/tinytemplate/latest/tinytemplate/) | No | `{ }`-based | formatters, not `:modifier` |
| [`upon`](https://github.com/rossmacarthur/upon) | No | n/a | full templating syntax |
| [`minijinja`](https://docs.rs/minijinja/latest/minijinja/) | configurable undefined-handling | n/a (Jinja syntax) | full filter system |

Nearest is `shellexpand`'s opt-in pass-through — still no escape, no modifier, flat env-var names
only. Reaching for `minijinja`/`tera`/`upon` means importing Jinja's whole syntax to solve a
four-field grammar: textbook YAGNI violation.

`regex` and `winnow` are both already in `Cargo.lock` (the latter transitively via `toml_edit`), so
either is zero new supply-chain surface — but a hand-rolled scanner is simpler to audit at this
grammar size (~100–200 lines).

**This satisfies `quality-core.md` "Don't Own Non-Domain Code" criterion 1 outright** (no library
implements the requirement, verified by searching) — and the namespaces are OCX's own domain
concepts, not a generic templating problem.

### 2.2 Why regex fails here

Not nesting — **escape-before-match**. Regex has no clean way to say "this match is invalidated by
the two bytes before it" without backtracking assertions or a second pass, which is exactly the
`$${` vs `${` disambiguation. Adjacent evidence:
[minimatch CVE-2026-27904](https://explore.alas.aws.amazon.com/CVE-2026-27904.html) (catastrophic
backtracking in a glob-like grammar),
[regular-expressions.info/catastrophic](https://www.regular-expressions.info/catastrophic.html).
Jinja's own maintainers describe needing a lexer, not regex
([pallets/jinja#857](https://github.com/pallets/jinja/issues/857)).

### 2.3 Escape semantics — the standard reading

Across Make, Bazel, and Docker Compose the escape is a **positional prefix rule checked before
token recognition**, not conditioned on what follows:

- `$$` → emit literal `$`, resume scanning from the next character.
- `$${…}` → emit `$`, then `{…}` is ordinary text (no longer a token start).
- `$$foo` → same rule, different trailing bytes. Not a second rule.
- `$$$` → `$$` consumed as escape, remaining `$` scanned fresh as a token-start candidate.

No tool treats `$$$` as a special case. Real-world confusion reports
([docker/compose#9757](https://github.com/docker/compose/issues/9757),
[#5965](https://github.com/docker/compose/issues/5965),
[#8330](https://github.com/docker/compose/issues/8330)) are all about `$$` not surviving a *second*
layer (a shell), not about the rule itself.

### 2.4 Error taxonomy — add exactly two variants

Mature engines draw up to five distinctions. OCX already has three as separate variants
(`UnknownDependencyRef` = unknown field, `UnknownPlaceholder` = unrecognised shape, `DisallowedToken`
= capability gate). The `namespace.path:modifier` grammar needs the split one level finer at the
*parse* stage, before field lookup:

- `UnknownNamespace` — not `self`/`deps`
- `UnknownModifier` — `:frobnicate` isn't real

Cheap, because the scanner already separates namespace/path/modifier into distinct tokens.
Proportionate at 2 namespaces × a small modifier set. Going finer is noise.

Tera's `is defined` test exists precisely because "recognised + allowed + syntactically valid, but
the dynamic value is absent" is a *third* state
([Keats/tera#120](https://github.com/Keats/tera/issues/120)).

### 2.5 Undefined value — hard error, not empty string

| Tool | Policy | Why tolerable there |
|---|---|---|
| Docker Compose | warn + empty | same person edits and runs, tight loop |
| Make | silent empty | same |
| GitHub Actions | empty/falsy, no warning | author and CI log one click apart |
| Bash `set -u` | hard error (opt-in) | exists *because* silent-empty is dangerous in scripts others run |
| Tera | hard render error by default | — |

Every tool tolerating empty-string does so because the loop that catches the mistake is the same
machine, same minute. **OCX's model breaks that assumption by design** — offline-first index
snapshots mean resolution happens on a different machine, potentially months later, decoupled from
the publisher who typo'd. An empty-string `${self.env.VAR}` baked into a digest-pinned artifact is
a permanent, silently-wrong package that no human will ever see a warning about.

**Recommendation: hard error, same `DataError` (65) lane as today's `UnknownPlaceholder`.**
Orthogonal to the pass-through flip: the flip is for *unrecognised shapes*; a *recognised* OCX
token with a missing value stays a hard error.

### 2.6 Migration safety — #303 loosens, which is the dangerous direction

Most precedent runs the other way (ESLint/Helm/TS promoting warnings to errors behind an opt-in
flag — [eslint#16512](https://github.com/eslint/eslint/discussions/16512),
[helm#8596](https://github.com/helm/helm/issues/8596)). #303 goes error → silent pass-through.

Closest precedent for *that* direction failing is **Kubernetes CRD structural-schema pruning**:
unknown fields moved from visible/erroring to silently dropped, producing real data-loss
regressions ([blog](https://kubernetes.io/blog/2019/06/20/crd-structural-schema/),
[k8s#107688](https://github.com/kubernetes/kubernetes/pull/107688)). The mitigation they converged
on is a **narrowly-scoped opt-in escape hatch** (`x-kubernetes-preserve-unknown-fields`), not a
blanket permissive default. Same tension in miniature in
[serde-rs/serde#44](https://github.com/serde-rs/serde/issues/44): default-permissive is right for
forward-compat read paths, wrong for catching the author's own typos.

OCX's risk is narrower — a typo inside a *recognised* namespace (`${self.foo}`) still hard-errors
via `UnknownField`. The genuine hazard is a typo that lands *outside* every known namespace and
therefore reads as a foreign token: `${slef.env.VAR}` passes through silently as intended-looking
text.

**Optional belt-and-suspenders (not a blocker):** a publish-time *warning* flagging any `${…}` whose
namespace segment is edit-distance-1 from `self`/`deps`. Cheap, because the scanner already
tokenizes namespace candidates.

### 2.7 Cycle risk — free today, not free for `self`

`${deps.NAME.*}` can only reference an already-published, digest-pinned dependency, so the graph is
acyclic **by construction** — no runtime cycle check needed, unlike Terraform which must check a
mutable config graph ([Terraform graph](https://developer.hashicorp.com/terraform/internals/graph)).

**That guarantee does not extend to `self`.** An intra-document reference
(`FOO=${self.env.BAR}` where `BAR` is declared in the same metadata) gets zero protection from
digest pinning. Confirmed against the code: zero `cycle`-detection machinery exists anywhere in
composition (grep: `self\.env|\$\{self\.` → zero matches tree-wide).

Ordering precedent OCX already matches: Helm merges *all* values files first, then renders against
the merged stable result — never interleaves
([Helmfile values](https://helmfile.readthedocs.io/en/stable/values-and-merging/)).

---

## PART 3 — Path rendering (axis 3)

**Verdict: `:posix` = flip `\` to `/`, keep the drive letter (`C:\Users\x` → `C:/Users/x`).
No-op on Linux/macOS. Build inline; do not add a crate.**

### 3.1 "posix" is ambiguous across three incompatible families

`cygpath` exposes exactly the ambiguity — [MSYS2 filesystem paths](https://www.msys2.org/docs/filesystem-paths/):

| Form | Flag | Example |
|---|---|---|
| unix | `-u` | `/c/foo` |
| mixed | `-m` | `C:/foo` |
| native | `-w` | `C:\foo` |

WSL's `wslpath -u` is a *fourth* transform (`/mnt/c/…`) — the drive becomes a mount-point segment,
not a prefix ([wslpath2](https://github.com/michidk/wslpath2)).

**OCX must pick the mixed form and say so.** MSYS/WSL/Cygwin rooted forms are filesystem-namespace
remapping owned by the shell/kernel interop layer — OCX cannot know which emulation layer, if any,
the consumer runs under, and guessing wrong is worse than not guessing. That is the issue's own
stated motivation: don't normalize one carrier behind everyone's back.

### 3.2 The strongest practical case for `:posix` is JSON, not POSIX semantics

JSON requires `\\` escaping for a backslash. `C:/path` is valid unescaped JSON; `C:\path` is not.
VS Code users request forward slashes for exactly this reason —
[microsoft/vscode#109300](https://github.com/microsoft/vscode/issues/109300),
[#23601](https://github.com/Microsoft/vscode/issues/23601). Since #221's `customizations` payloads
are JSON carrying VS Code settings, this is the concrete consumer.

### 3.3 The two-knob shape is 20-year-proven

CMake's `file(TO_CMAKE_PATH)` / `cmake_path(NATIVE_PATH)` is precisely #303's design, and its docs
explicitly resolve the cross-compile ambiguity — "native refers to the host platform, not the
target platform" ([cmake_path](https://cmake.org/cmake/help/latest/command/cmake_path.html)).
MSBuild's alternative answer is "just always emit `/`" since Windows accepts
`AltDirectorySeparatorChar` ([MSBuild items](https://learn.microsoft.com/en-us/visualstudio/msbuild/msbuild-items)).

### 3.4 …but always-forward-slash is not safe, so the modifier earns its place

- `LoadLibrary`/`LoadLibraryEx` explicitly do **not** support forward slashes.
- `cmd.exe` "pretty exclusively requires backslashes" — real risk on `windows-latest` GitHub Actions
  runners using default batch steps, squarely in OCX's target-user surface.

Both from the [LLVM forward-slashes-on-Windows RFC](https://groups.google.com/g/llvm-dev/c/-lO08fUlkDc).
Keeping `:native` as the default and `:posix` as opt-in is correct.

### 3.5 Consumers that don't care

Node.js `path` and Python `os.path` both accept forward slashes on Windows and normalize
internally ([Node](https://nodejs.org/api/path.html), [Python](https://docs.python.org/3/library/os.path.html)).
Docker's native Windows CLI accepts `C:\Users\…` directly — the `//c/Users` form is an MSYS
path-conversion workaround, not canonical syntax, and must not leak into OCX's semantics
([Docker forum](https://forums.docker.com/t/whats-the-correct-way-to-mount-a-volume-on-docker-for-windows/58494)).

### 3.6 Verbatim prefixes are a separate, real hazard

`std::fs::canonicalize` on Windows emits `\\?\C:\…`, which breaks IntelliJ's diff editor
([jj-vcs#3986](https://github.com/jj-vcs/jj/issues/3986)) and crashed a Node.js daemon
([vercel-labs/agent-browser#393](https://github.com/vercel-labs/agent-browser/issues/393)).

Neither `:native` nor `:posix` may ever emit it. OCX already strips it with `dunce::simplified` at
both substitution sites (§0.6) — the ADR should carry a one-line implementation check that the
install-path value never routes through `std::fs::canonicalize` before rendering.

### 3.7 Crate survey — build, don't buy

| Crate | Status | Verdict |
|---|---|---|
| [`typed-path`](https://lib.rs/crates/typed-path) | v0.12.3 Feb 2026, ~5.6M dl/mo, active | **Wrong operation** — `with_unix_encoding()` *drops the drive letter* on Windows→Unix conversion |
| [`path-slash`](https://lib.rs/crates/path-slash) | last release 2022-08-06, **>18mo stale** | Right operation (keeps drive letter), unmaintained |
| [`normpath`](https://lib.rs/crates/normpath) | v1.5.1 May 2026, active | Orthogonal — solves canonicalize-without-verbatim-prefix, not slash direction |
| [`camino`](https://lib.rs/crates/camino) | active | UTF-8 guarantee only, irrelevant here |
| [`dunce`](https://lib.rs/crates/dunce) | **already a dependency** | Already used; keep as the pre-step |

**Build.** Qualifies for `quality-core.md` "Don't Own Non-Domain Code" exemption 3 — a few lines
with no edge cases — because OCX's input space is constrained to paths it generates itself:
`$OCX_HOME`-rooted, digest-sharded, ASCII-slugified. No UNC, no verbatim, no relative-drive case
ever reaches this code. Treat UNC/verbatim as an **explicit non-goal**, not a silently-mishandled
case. If a future feature must parse arbitrary/untrusted Windows paths, re-open the buy question —
`typed-path` is the best-engineered option but would need a drive-letter-preserving wrapper.

### 3.8 Vocabulary

Keep `:native` / `:posix`. Matches CMake's `NATIVE_PATH`/cmake-style split and `typed-path`'s
`with_windows_encoding`/`with_unix_encoding` family. Do **not** rename to `windows`/`unix` (implies
platform-conditional behaviour rather than a fixed rule). Do **not** add a `mixed`/msys option now —
YAGNI, and that transform is shell-specific in a way OCX structurally cannot pick from the
publisher side.

---

## Actionable summary for the ADR

1. **Hand-write a single-pass scanner.** Escape check first, unconditionally, everywhere; then `${`;
   then namespace; then dotted path; then optional `:modifier`; then `}`. Anything not matching this
   exact shape is emitted byte-identical. (§2.1, §2.2)
2. **Escape matches `$$` immediately followed by `{`**, never bare `$$`. Test that a literal `$$` not
   followed by `{` survives untouched. (§1.3)
3. **`.` = namespace, `:` = modifier, and a modifier never carries free text.** Record as a designed
   invariant. (§1.1)
4. **Add `UnknownNamespace` and `UnknownModifier`** as parse-time `TemplateError` variants ahead of
   the existing three. (§2.4)
5. **Undefined dynamic value = hard error**, exit 65, same lane as today. Not empty string. (§2.5)
6. **Golden-fixture the claiming rule** against every foreign token in the issue plus an
   unrecognised single-segment token — prove the *leave-alone* path, not only the resolve path.
   This is the one frontier area with no battle-tested precedent. (§1.4)
7. **`:posix` = slash-flip preserving the drive letter; no-op on POSIX.** UNC/verbatim are explicit
   non-goals. Compose after `dunce::simplified`, never instead of it. (§3.1, §3.6, §3.7)
8. **Decide `${self.env.VAR}`'s self-reference question before ruling cycle detection out of scope.**
   Launcher-args is easy (composed `entries` already exist); var-referencing-var needs two-pass
   resolution plus a cycle check. (§0.4, §2.7)
9. **Update `classify_install_path_rooted_dir` and `first_unknown_placeholder` in the same commit**
   as the alias, or `bin_scan` silently degrades / valid metadata fails to publish. (§0.5)
10. **Do not call `:posix` a "modifier" in user-facing docs** without disambiguating from the
    wire-visible `"type"` modifier. (§0.7)
11. **Optional:** publish-time warning for a namespace segment edit-distance-1 from `self`/`deps`.
    (§2.6)

---

## Sources

Consolidated; each claim above links its own source inline.

**Grammar** — [containers.dev reference](https://containers.dev/implementors/json_reference/) ·
[devcontainers/spec#565](https://github.com/devcontainers/spec/issues/565) ·
[webpack output](https://webpack.js.org/configuration/output/) ·
[cmake-generator-expressions(7)](https://cmake.org/cmake/help/latest/manual/cmake-generator-expressions.7.html) ·
[GNU Make variables](https://www.gnu.org/software/make/manual/html_node/Variables-in-Recipes.html) ·
[Bazel Make Variables](https://bazel.build/reference/be/make-variables) ·
[bazelbuild/bazel#25204](https://github.com/bazelbuild/bazel/issues/25204) ·
[Compose interpolation](https://docs.docker.com/reference/compose-file/interpolation/) ·
[kubernetes#101137](https://github.com/kubernetes/kubernetes/issues/101137) ·
[GHA contexts](https://docs.github.com/en/enterprise-server@3.6/actions/learn-github-actions/contexts) ·
[brandur.org env-in-env](https://brandur.org/fragments/github-actions-env-vars-in-env-vars) ·
[actions/runner#520](https://github.com/actions/runner/issues/520) ·
[envsubst war story](https://jellepelgrims.com/posts/dollar_signs) ·
[Kustomize var substitution](https://dev.to/zenika/kubernetes-a-convenient-variable-substitution-mechanism-for-kustomize-lhm)

**Parser** — [shellexpand](https://docs.rs/shellexpand/latest/shellexpand/) ·
[subst](https://github.com/fizyr/subst) · [envsubst-rs](https://github.com/coreos/envsubst-rs) ·
[tinytemplate](https://docs.rs/tinytemplate/latest/tinytemplate/) ·
[upon](https://github.com/rossmacarthur/upon) · [minijinja](https://docs.rs/minijinja/latest/minijinja/) ·
[pallets/jinja#857](https://github.com/pallets/jinja/issues/857) ·
[minimatch CVE-2026-27904](https://explore.alas.aws.amazon.com/CVE-2026-27904.html) ·
[catastrophic backtracking](https://www.regular-expressions.info/catastrophic.html) ·
[Keats/tera#120](https://github.com/Keats/tera/issues/120) ·
[docker/compose#9757](https://github.com/docker/compose/issues/9757) ·
[k8s structural schema](https://kubernetes.io/blog/2019/06/20/crd-structural-schema/) ·
[k8s#107688](https://github.com/kubernetes/kubernetes/pull/107688) ·
[serde-rs/serde#44](https://github.com/serde-rs/serde/issues/44) ·
[Terraform graph](https://developer.hashicorp.com/terraform/internals/graph) ·
[Helmfile values merging](https://helmfile.readthedocs.io/en/stable/values-and-merging/) ·
[winnow toolbox](https://epage.github.io/blog/2023/02/winnow-toml-edit-combine-nom/)

**Paths** — [cmake_path](https://cmake.org/cmake/help/latest/command/cmake_path.html) ·
[CMake file()](https://cmake.org/cmake/help/latest/command/file.html) ·
[MSYS2 filesystem paths](https://www.msys2.org/docs/filesystem-paths/) ·
[wslpath2](https://github.com/michidk/wslpath2) ·
[Bazel on Windows](https://bazel.build/rules/windows) ·
[microsoft/vscode#109300](https://github.com/microsoft/vscode/issues/109300) ·
[Microsoft/vscode#23601](https://github.com/Microsoft/vscode/issues/23601) ·
[Docker Windows mounts](https://forums.docker.com/t/whats-the-correct-way-to-mount-a-volume-on-docker-for-windows/58494) ·
[Node path](https://nodejs.org/api/path.html) · [Python os.path](https://docs.python.org/3/library/os.path.html) ·
[LLVM forward-slash RFC](https://groups.google.com/g/llvm-dev/c/-lO08fUlkDc) ·
[jj-vcs#3986](https://github.com/jj-vcs/jj/issues/3986) ·
[vercel-labs/agent-browser#393](https://github.com/vercel-labs/agent-browser/issues/393) ·
[typed-path](https://lib.rs/crates/typed-path) · [path-slash](https://lib.rs/crates/path-slash) ·
[normpath](https://lib.rs/crates/normpath) · [camino](https://lib.rs/crates/camino) ·
[dunce](https://lib.rs/crates/dunce)
