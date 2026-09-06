---
title: Bzlmod and Repository Rules
summary: The BZL-MOD family — MODULE.bazel, the lockfile and its modes, module-extension purity, repository-rule hermeticity and caching, and publishing to a registry
---

# Bzlmod and Repository Rules

Owns `BZL-MOD`: what `MODULE.bazel` declares, how `MODULE.bazel.lock` is
generated, merged and gated, what a module extension may read, what a repository
rule must watch and checksum, which caches either one qualifies for, and what a
registry submission commits you to. WORKSPACE is gone — Bazel 9.0.0 deleted the
support code rather than disabling it — so every row assumes Bzlmod. It does not
own the remote cache and its flags (`BZL-CACHE`), the flags checklist or the
Bazel version pin itself (`BZL-FLAG`), `--action_env` versus `--repo_env`
placement (`BZL-HERM`), offline tests for an extension or repository rule
(`BZL-TEST`), a generator-owned lockfile such as a crate-universe lock
(`BZL-RUST`), the override-versus-dependency decision for a vendored fork
(`BZL-ARCH`), or buildifier and Starlark shape (`BZL-LARK`).

Contents: [The Lockfile](#the-lockfile) ·
[What MODULE.bazel Declares](#what-modulebazel-declares) ·
[Repository Rules](#repository-rules) ·
[Module Extensions](#module-extensions) ·
[Run It, Do Not Recite It](#run-it-do-not-recite-it) ·
[Publishing to a Registry](#publishing-to-a-registry) · [Gaps](#gaps) ·
[What Agents Get Wrong Here](#what-agents-get-wrong-here)

Every version-bound claim below was measured 2026-09-06 against real Bazel
8.7.0, 8.8.0 and 9.2.0 binaries on a Linux host; a row that binds to another
version names it in its own cell. This family is core Bazel, so no ruleset
version gates any row — with one consequence worth knowing: `rules_python`
2.3.3's `pip` extension is not reproducible and carries a `moduleExtensions`
lockfile entry, so in a Python repository every dependency edit rewrites digest
fields and BZL-MOD-03 and BZL-MOD-04 bite first. The two permanent flips this
family rests on are in the [Bazel 8.0.0 release
notes](https://github.com/bazelbuild/bazel/releases/tag/8.0.0); WORKSPACE
deletion, the `single_version_override` hard error and `facts` are in the [Bazel
9.0.0 notes](https://github.com/bazelbuild/bazel/releases/tag/9.0.0). Before
citing any flag, re-read it on **both** help surfaces at your pinned version —
`bazel help <command> --long` **and** `bazel help startup_options` — because a
flag lives in exactly one of them. Every grep here reads checked-in text: a
BUILD or `.bzl` file that a repository rule materialises into
`$(bazel info output_base)/external/` is out of reach of all of them, which is
why each grep is paired with a `bazel mod` step, a `bazel query`, or a named
reading heuristic. Severity maps onto the house tiers: MUST = Block,
SHOULD = Warn, CONSIDER = Suggest.

## The Lockfile

Three commands cover this whole section, run from the workspace root, identical
on 8.7.0, 8.8.0 and 9.2.0:

```bash
git ls-files MODULE.bazel.lock            # tracked?
git check-attr merge MODULE.bazel.lock    # JSON-aware merge driver?
bazel mod deps --lockfile_mode=error      # fresh? CI leg only
```

The lockfile is machine output with exactly two hand-mergeable fields.
Everything else in it is reset-and-regenerate.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-MOD-01 | Track `MODULE.bazel.lock` in version control. The one exception is a disposable fixture module under an `examples/` or `e2e/` tree, and that exception is written down beside the `.gitignore` line that creates it. | An untracked lockfile re-resolves the whole module graph on every fresh checkout and hides drift between two machines. Nothing is printed when it happens. | `git ls-files MODULE.bazel.lock` — **empty output is the finding** (untracked), never the pass. | MUST |
| BZL-MOD-04 | Point `MODULE.bazel.lock` at Bazel's own `bazel-lockfile-merge` jq driver, never at a line-based git driver such as `union` or `ours`. Aim that driver at no other file — a generator-owned JSON lockfile needs its own JSON-aware driver. | A line-based driver interleaves or duplicates a digest key **without writing a conflict marker**, which also defeats Bazel's own merge-conflict detector: that detector looks for `<<<<<<<` in the text. A silent bad merge is strictly worse than a visible conflict. Aiming Bazel's driver at a foreign lockfile parses a schema it was not written for, trading one silent corruption for another. | `git check-attr merge MODULE.bazel.lock` must print `bazel-lockfile-merge`; **any other value — `union`, `ours`, `unspecified` — is the finding**. The driver needs a one-time `git config merge.bazel-lockfile-merge.driver …` per clone and per CI runner, so also run `git config --get merge.bazel-lockfile-merge.driver`: empty output there is the finding. Then `grep -n -e 'bazel-lockfile-merge' .gitattributes` — a second path pattern naming that driver is the finding (wrong schema). | MUST |
| BZL-MOD-03 | On a `MODULE.bazel.lock` conflict, keep both sides only inside `registryFileHashes` and `selectedYankedVersions`. For anything else, run `git checkout MODULE.bazel.lock`, resolve `MODULE.bazel` itself, regenerate with `bazel mod deps`, and commit the regenerated file. Never hand-type a value into `moduleExtensions`, `bzlTransitiveDigest`, `usagesDigest` or `generatedRepoSpecs`. | Those four are opaque, extension-computed digests. A value that merely parses as JSON desynchronises from whatever produced it, and Bazel trusts it until that extension is next evaluated. This is Bazel's own documented procedure and the only one that does not require the editor to know what a digest means. | Reading heuristic: a diff touching any of those four keys that is not the whole-file output of a `bazel mod deps` run in the same change is the finding — no grep separates a good hash from a bad one. After regenerating, diff the new lockfile against the last known good one: **any** change to a module the edit did not touch earns a second look, because a whole-file delete re-resolves the entire graph. An empty diff outside the edited module is the pass. | MUST |
| BZL-MOD-02 | Run `--lockfile_mode=error` in one dedicated CI leg and nowhere else, add a `schedule:`-triggered leg on `--lockfile_mode=refresh`, and decide both legs on the **exit code** — never on a string matched out of stderr. | `error` is the only mode that both fails on a stale lockfile and issues zero network requests while resolving, so staleness is never mistaken for a flake; `refresh` is the only mode that re-checks mutable registry data (yanked versions, previously-missing entries). A bare `error` in a committed base rc turns every legitimate dependency edit into a failure a developer cannot self-serve. Three unrelated stderr texts share exit 37: a `lockFileVersion` mismatch, the clean `Missing checksum for registry file … run 'bazel mod deps --lockfile_mode=update'` that a brand-new `bazel_dep` produces, and — for the commonest case, an **existing** dependency whose locked version moved — an unhandled `java.lang.IllegalStateException: Cannot fetch a file without a checksum in ENFORCE mode. This is a bug in Bazel, please report` ([bazelbuild/bazel#29497](https://github.com/bazelbuild/bazel/issues/29497), fixed only from 9.3.0 and declined for the 8.x line). No single pattern matches more than one of the three. | `bazel mod deps --lockfile_mode=error` alone reproduces every shape — no build step, no explicit `--registry`. Exit 37 is the finding and exit 0 the pass, on 8.7.0 and 9.2.0 alike. Then `grep -rn -e 'lockfile_mode' .bazelrc .bazelrc.* .github/workflows` — **no hit anywhere is the finding** (no freshness gate); a hit on an unscoped `build --lockfile_mode=error` line in a committed base rc is also the finding, and so is any step that decides pass or fail by grepping stderr for a lockfile message. | MUST (the `error` leg); SHOULD (the `refresh` canary) |
| BZL-MOD-06 | A change to `.bazelversion` regenerates `MODULE.bazel.lock` and reviews the diff in the same commit. This binds on a patch or Maintenance bump exactly as on a major one. | A version-mismatched lockfile is not read as stale, it is read as absent: Bazel matches `lockFileVersion` with a regex before parsing and, on any mismatch, silently re-resolves the entire graph in every mode except `error`. Measured: 8.7.0 → 8.8.0 crosses the same 24 → 28 schema boundary as 8.7.0 → 9.2.0, so "we are staying on 8.x" is not an exemption. | `git show --stat <bump-commit> -- MODULE.bazel.lock` — **empty output is the finding**, however small the version step. The pass is one commit listing both `.bazelversion` and `MODULE.bazel.lock`. | MUST |
| BZL-MOD-05 | Read `lockFileVersion` out of the file in hand. Never cite a `MODULE.bazel.lock` schema version from documentation, from memory, or from another repository. | Measured five ways: the docs example says `10`, an 8.7.0-produced lockfile says `24`, and 8.8.0, 9.2.0 and current master all say `28` — with a top-level `factsVersions` key that no prose source documents. Each number is right for its vantage point and every one is wrong to hardcode, `24` included: it was current for exactly one Maintenance release. | `python3 -c "import json;print(json.load(open('MODULE.bazel.lock'))['lockFileVersion'])"` — there is no empty case. A missing key or a parse error is itself the finding: a corrupt or textually merged lockfile. | MUST |

## What MODULE.bazel Declares

Caught by reading `MODULE.bazel` top to bottom and running `bazel mod graph`,
which behaves identically on all three pinned versions.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-MOD-07 | Mark every dev-only `bazel_dep` — test, lint, docs and formatting tooling — `dev_dependency = True`. | The attribute's own doc text: a dev dependency "will be ignored if the current module is not the root module". Omitting it leaks your build and test toolchain into every consumer's resolved graph, where it costs them resolution time and version conflicts whose cause is invisible from their side. | `grep -n -e 'bazel_dep' MODULE.bazel`, then classify each hit: referenced from the public `.bzl` or BUILD surface (production, plain) versus test, lint or docs only (dev, must carry the attribute). **Empty output means no dependencies are declared at all** — its own question, not a pass. | MUST for a published module; SHOULD for a root-only one |
| BZL-MOD-08 | Never write a `single_version_override` whose `version` is lower than any live `bazel_dep` requirement for the same module. | Silently accepted and silently harmful through Bazel 8; a hard, named resolution error from 9.0.0. No `--incompatible_*` flag gates it, so there is nothing to flip early on 8.x to find out. | On 9.x, `bazel mod graph` prints `is overridden to use version '…', which is lower than the version '…' requested by the root module` — **a clean graph is the pass**. On 8.x the same pattern produces no output at all: compare each `single_version_override` version against every `bazel_dep` for that module by hand. | MUST on Bazel 9.0+; SHOULD on 8.x |
| BZL-MOD-10 | Run `bazel mod tidy` only behind a diff gate, never as a silent auto-commit step. | It rewrites the hand-authored `MODULE.bazel` — formatting plus every `use_repo()` call — and ships with no `--check` and no dry-run flag, so nothing inside the command previews what it is about to do. | `bazel mod tidy && git diff --exit-code MODULE.bazel`. **Exit 0 (empty diff) is the pass** — already tidy. Non-zero means a human reads the rewrite before it lands. Below 9.2.0, `mod tidy` also pulls an implicit `buildozer` dependency that vendoring does not cover (BZL-MOD-11). | SHOULD |

## Repository Rules

One sweep catches most of this block. It reads checked-in Starlark only, so a
repository rule that arrived inside a fetched dependency needs
`bazel mod show_repo` or a source read of that dependency instead:

```bash
grep -rn --include='*.bzl' -e 'os.environ' -e 'environ *= *\[' -e 'ctx.download' \
     -e 'ctx.execute(' -e 'ctx.watch' -e 'repo_metadata' -e '@@' .
```

`ctx.execute()` is the boundary that defeats every static inventory: a local
`execute()` seeds the child from the Bazel server's full ambient environment and
only then applies `environment=`, which
[`--repo_env`'s own reference entry](https://bazel.build/reference/command-line-reference#flag--repo_env)
states outright — "repository rules see the full environment anyway".

```starlark
v = ctx.os.environ.get("CC")                               # reads fine, tracks nothing
repository_rule(implementation = _impl, environ = ["CC"])  # Deprecated at 8.7.0 and current
```

```starlark
v = ctx.getenv("CC")                                       # the read IS the dependency
repository_rule(implementation = _impl)                    # no environ= beside it
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-MOD-20 | Read every environment variable through `repository_ctx.getenv()`. Never through `repository_ctx.os.environ` or `module_ctx.os.environ`, and never add `environ = [...]` to a `repository_rule()` or `module_extension()` call alongside `getenv()`. | `os.environ` is documented as establishing **no** dependency: the same value, none of the re-fetch tracking, so editing the variable serves a stale fetch with no error, no lint and no failing test. `environ=` carries the verbatim marker `Deprecated. … Migrate to repository_ctx.getenv` in the 8.7.0-pinned and the current API doc alike, and Bazel's re-fetch trigger list joins the two with **or** — a pair is not redundancy, it is a tell that the snippet came from pre-`getenv()` material. Buildifier covers none of this: its 82 warning categories return zero hits for `getenv`, `environ`, `repository_rule` or `module_extension`, so the grep is the only mechanical check that exists. | `grep -rn --include='*.bzl' -e 'os.environ' .` — **empty is the pass**, any hit is a finding by definition. Then `grep -rn --include='*.bzl' -e 'environ *= *\[' .` — empty means nothing to migrate. | MUST (`os.environ`); SHOULD (the `environ=` migration) |
| BZL-MOD-21 | Pass every `attr.label()` declared in a `repository_rule`'s `attrs=` to `ctx.path()`, `ctx.read()` or `ctx.execute()`, or watch it explicitly with `ctx.watch()`/`ctx.watch_tree()`. Never treat `watch = "auto"` as coverage for a path inside the repository being fetched, or, in a module extension, outside the workspace. | `--incompatible_no_implicit_watch_label` defaults true from Bazel 8.0: a `Label` attribute no longer auto-watches its file and `repository_ctx.path()` no longer implicitly watches its argument, so editing the referenced file stops triggering a re-fetch and nothing reports it. `"auto"` degrades to *do not watch* in exactly the two illegal-to-watch cases instead of erroring — silence that reads as coverage. | For each label attribute name, grep the implementation for that name next to `ctx.path(ctx.attr.`, `ctx.read(ctx.attr.`, `ctx.watch(` or `ctx.watch_tree(`. **Missing on all four is the finding**; an empty finding list is the pass. For any `read()`, `extract()`, `template()` or `patch()` on a path you cannot prove is external, set `watch = "yes"` once and let the resulting error settle it. | MUST |
| BZL-MOD-22 | Give every `ctx.download()` and `ctx.download_and_extract()` call a `sha256=` or `integrity=` argument. | An unchecksummed fetch is non-hermetic — content moves under a floating URL — and it is invisible to the repository cache, which is keyed on the **expected** sha256 of the request. Supply-chain hole and permanent cache miss at once. Nothing lints it: buildifier's warning catalogue contains zero occurrences of `sha256`, `integrity`, `checksum` or `hash`. | `grep -rn --include='*.bzl' -e 'ctx.download' .`, then read forward from each hit to the call's closing paren — the read-forward step is what makes this work on a multi-line call. **No unchecksummed hit is the pass.** Beware the name collision with a tag class called `download` in a user-facing `MODULE.bazel` DSL. | MUST |
| BZL-MOD-23 | Where a repository rule shells out through `ctx.execute()`, name the invoked binary's own environment-variable surface at the call site, and never claim the rule is hermetic on the strength of a `getenv`/`watch` inventory. | Local `execute()` layers `environment=` **onto** the Bazel server's full ambient environment rather than replacing it. A Starlark-side inventory covers only what the Starlark code reads to build that dict; the binary's own reads of `PATH`, `LANG` or a proxy variable pass through unlisted and untracked. Remote execution is the sole exception, and it is not available for repository rules. | Named heuristic: every `ctx.execute(` site carries a comment or linked doc naming the tool's environment surface — **a call site with neither that note nor `getenv()` coverage is the finding** — and any README or rule text claiming hermeticity from "N getenv sites, M watch sites" is incomplete on its face. For a specific run: `bazel clean --expunge`, then `bazel build --experimental_workspace_rules_log_file=<path> <target>`, and read the log with the workspacelog parser, grepping the six non-hermetic categories (`execute`, `download*`, `file`/`template`, `os`, `symlink`, `which`). **The expunge is mandatory** — cached fetches never appear in the log, so a partial run under-reports with no indication that it did. An empty `grep -rn --include='*.bzl' -e 'ctx.execute(' .` means the ruleset never shells out and this row does not bind: that is the pass. | MUST (the claim check); SHOULD (the per-call-site note) |
| BZL-MOD-25 | Treat `repository_ctx.attr.name` as the canonical repository name: never parse it for structure, never hard-code the pre-8.0 `~` separator, and take an apparent name as an explicit attribute when you need one. | `--incompatible_use_plus_in_repo_names` flipped default-true in Bazel 8.0 **and became a no-op flag in the same release** — `@@foo~1.0.0` became `@@foo+1.0.0` with no way back — so every piece of code parsing canonical-name structure broke at that boundary. The `name` attribute is magic in both directions: apparent going in, canonical coming out. | `grep -rn --include='*.bzl' -e '~' -e '@@' .`, and the same over scripts and rc files, reading each hit for a canonical-name-shaped string. Then `grep -rn --include='*.bzl' -e 'ctx.attr.name' .` to confirm no user-facing display string is built from it. **Empty on both is the pass.** | MUST for anything parsing repository-name structure; CONSIDER as general awareness |
| BZL-MOD-26 | Set `configure = True` on a `repository_rule` that inspects the host (`ctx.os`, `ctx.which`, toolchain autodetection). Do not set `local = True` on a rule that is not restart-sensitive. | `configure` is the only knob `bazel fetch --force --configure` respects: measured on 8.7.0, that command re-ran only the `configure`-marked rule and left an unmarked host-probing rule's fetched state byte-identical. `local = True` re-fetches on every server restart — every CI job — for no benefit when the rule is not host-sensitive across restarts. | `grep -rn --include='*.bzl' -e 'configure *=' -e 'local *=' .`, matched against the rules that call `ctx.os` or `ctx.which`. **Empty `configure` output on a host-probing rule is the finding**; empty `local` output on a non-probing rule is the pass. | SHOULD |
| BZL-MOD-31 | Call a `repository_rule` symbol — or an `_impl` that calls one — only from a module extension's own evaluation: never from a BUILD file, never from a `.bzl` helper, never from inside a `rule()` implementation. When matching the resulting error in a log, a runbook or a test, match `Error in repository_rule:` and nothing after it. | The restriction is a Skyframe-level gate on which evaluation context may request repository creation, not a check on the calling code's shape — measured firing identically from a loading-phase BUILD top level and from an analysis-phase `rule()` harness, on 8.7.0 and 9.2.0. **The sentence after the colon differs by major**: 8.7.0 says `repository rules can only be used while evaluating a WORKSPACE file` (naming a file type Bazel 9 does not have), 9.2.0 says `repo rules can only be called from within module extension impl functions`. A matcher written against either silently never fires on the other. | `REPO_RULE=my_repo_rule; grep -rn --include='*.bzl' --include='BUILD*' -e "$REPO_RULE(" .` (substitute the repository rule's own exported symbol name for `my_repo_rule`) and confirm every call site sits inside a `module_extension` implementation, or inside a function only that implementation calls. **No call site outside an extension implementation is the pass.** For a log or CI matcher, `grep -c 'Error in repository_rule:'` — a matcher pinned to either major's full sentence is the finding. | MUST |

## Module Extensions

Two greps over the extension `.bzl` and its transitive `load()` closure, plus
one `bazel mod deps` run, catch this block. The trap that costs the most:
`reproducible = True` on a module extension and `repo_metadata(reproducible =
True)` on a repository rule are two APIs on two objects gating two different
things — lockfile exclusion and repo-contents-cache eligibility — and satisfying
one says nothing about the other.

```starlark
return module_ctx.extension_metadata(reproducible = True)  # extension: lockfile only
return {"reproducible": True}                              # repo rule: pre-8.3, grants nothing
```

```starlark
return module_ctx.extension_metadata(reproducible = True)  # extension: lockfile exclusion
return repository_ctx.repo_metadata(reproducible = True)   # repo rule: cache eligibility
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-MOD-14 | A module extension's implementation function — and every `.bzl` file it `load()`s that is not itself a repository rule — calls neither `module_ctx.os` nor `.getenv()`. Push all host and environment access down into the repository rules it instantiates. | That boundary is exactly what `reproducible = True` claims, and nothing re-checks the claim: an impure implementation marked reproducible produces silently divergent repositories across machines with **no lockfile entry to diff**, because the flag also removes the extension from the lockfile. | Static: `grep -n -e 'ctx.os.' -e 'ctx.getenv(' <extension>.bzl` (the substring matches any parameter name ending in `ctx`), repeated over the transitive `load()` closure enumerated with `bazel query 'buildfiles(@<repo>//...)'`. **Empty across the closure is the pass**; any hit outside the repository-rule files is the finding. Dynamic, executed on 8.7.0: drop `reproducible = True`, run `bazel mod deps --lockfile_mode=update`, re-run once unchanged as a control, then re-run under `--repo_env=<VAR>=<value>` for each variable the extension's repositories consult, and diff that extension's `bzlTransitiveDigest`, `usagesDigest`, `recordedRepoMappingEntries`, `generatedRepoSpecs` and `envVariables`. **All five identical means pure**; any field moving under an environment change while the grep is clean means the grep missed a read. Restore the flag and the lockfile afterwards. | MUST |
| BZL-MOD-17 | Never instantiate a repository and `load()` a `.bzl` file from it inside the same module extension. When the error fires, split into two extensions, or into a plain shared `.bzl` with no repository dependency — never reorder statements. | It raises the hard, build-breaking `Circular definition of repositories generated by module extensions or files in external repositories`. The cycle is a Skyframe dependency-graph property, so statement order cannot affect it, and the message's cycle diagram reads exactly like an ordering problem — which is why the wrong fix gets attempted first. | Reading heuristic with a grep proxy: `grep -n -e '^load(' <extension>.bzl`, cross-checked against the repository names the same file's `_impl` passes to a `repository_rule` call. **No overlap is the pass.** After a fix, `bazel mod deps` must evaluate both extensions cleanly. | MUST |
| BZL-MOD-18 | Never call `native.register_toolchains()`, `native.register_execution_platforms()` or `native.bind()` inside a module extension implementation; register a toolchain from `MODULE.bazel` instead. | All three raise a hard error under Bzlmod: registration is a `MODULE.bazel`-only API and `bind()` is unsupported outright. A WORKSPACE macro doing exactly this is a working pre-Bzlmod pattern, so a mechanical port hits the error with no "did you mean `MODULE.bazel`" hint. | `grep -rn --include='*.bzl' -e 'native.register_toolchains' -e 'native.register_execution_platforms' -e 'native.bind(' .` — **empty is the pass**. Run it as a step of any WORKSPACE-to-Bzlmod port. | MUST |
| BZL-MOD-15 | An extension whose implementation is a pure function of its tags returns `module_ctx.extension_metadata(reproducible = True)`. Never set it beside `os_dependent` or `arch_dependent`; bump `facts_version` on any incompatible change to `facts` data; drop the claim in the same change that breaks it. | Skipping the opt-in pays the lockfile-churn and merge-conflict cost for nothing. Setting `reproducible` next to `os_dependent`/`arch_dependent` asserts two contradictory things — those two exist precisely to force re-evaluation on a host change. `facts` survive an extension code change untouched, so an old, incompatibly-shaped fact dict feeds new code unless the integer moves. A stale reproducibility claim poisons a cross-workspace cache that has no correctness oracle. | `grep -n -e 'extension_metadata' -e 'os_dependent' -e 'arch_dependent' -e 'facts_version' <extension>.bzl`. Pass = `reproducible = True` present while BZL-MOD-14's grep is empty, and no `os_dependent`/`arch_dependent` on the same extension. **Empty output while BZL-MOD-14 is also empty is the finding** — a missed opt-in. Review trigger: a diff adding a `getenv`, a `watch` or an unchecksummed fetch under a `reproducible` claim must touch the claim too. `facts`/`facts_version` exist from 9.0.0 only. | SHOULD |
| BZL-MOD-16 | A repository rule whose fetch is deterministic returns `repository_ctx.repo_metadata(reproducible = True)` explicitly. An implicit `return None` grants nothing. | The pre-8.3.0 contract (return `None` or a dict) is what every doc page still shows, and it grants no cache eligibility at all. The explicit return is the **only** local-cache gate: measured on 8.7.0, two rules both returning it — one of them calling `getenv("HOME")` — were both cached and both hit across `bazel clean --expunge`. `getenv()` and `watch()` disqualify a rule from the experimental **remote** repo-contents cache only, a separate startup-only mechanism. A rule marked `local = True` is re-fetched on every server restart and is not a cache candidate at all. | `grep -n -e 'repo_metadata' <repo_rule>.bzl` — **empty on an otherwise deterministic rule is a missed opt-in**, not a bug. Read the live default before relying on the cache: `bazel help build --long \| grep -A6 -e '--repo_contents_cache'` prints `""` on 8.7.0 and 8.8.0 (opt-in) and `see description` on 9.2.0, where it derives from `{--repository_cache}/contents` and is already on. The remote flag lives on the other surface: `bazel help startup_options \| grep -e experimental_remote_repo_contents_cache` — absent on 8.7.0, present and `false` on 8.8.0 and 9.2.0, invisible to `help build --long` and `help fetch --long` alike. | SHOULD |
| BZL-MOD-32 | Split a module extension's `_impl` into a pure `module_ctx -> data` decision function and a thin tail that does nothing but loop over that data invoking repository rules. Keep every `fail()`, every tag-validation branch and every version or platform choice in the pure half. | The tail is untestable offline by construction (BZL-MOD-31), so every line left in it is a line no test reaches — and the tag-validation branches are exactly where the `fail()`s that gate security and root-only policy live. | Reading heuristic over `REPO_RULE=my_repo_rule; grep -n -e '^def ' -e "$REPO_RULE(" <extension>.bzl` (substitute the repository rule's own exported symbol name for `my_repo_rule`): the function passed to `module_extension(implementation = …)` contains a call to a named decision function and a loop of repository-rule invocations, and **no `fail()` of its own**. A `fail()` or a tag-parsing loop in the same function as the repository-rule invocations is the finding. An extension with one tag class and no validation is trivially compliant. **Empty grep output means you are reading a file that declares no extension — an answer about the file, never a pass.** | SHOULD |
| BZL-MOD-30 | **pinned** — A root-only tag class carrying a security or policy decision `fail()`s when a non-root module uses it, instead of ignoring the tag. An adopter may override this once, for their own extension, never per tag class. | The ecosystem default is the opposite: a survey of five Bzlmod rulesets found all of them silently ignoring a non-root customisation and none calling `fail()`. That default is right for an ergonomic knob and wrong for a security posture — a dependency's attempt to weaken verification must be loud. Recorded as a deliberate break with convention so nobody "fixes" it back. | `grep -n -e 'is_root' <extension>.bzl` — every security-relevant root-only tag class reaches a `fail()` on the non-root branch. **Empty output while such a tag class exists is the finding** (silently ignored). A non-security tag class with no `is_root` check is not a finding. | SHOULD |

## Run It, Do Not Recite It

One check for all three rows: run the thing on the pinned binary instead of
reciting what it does. Doc lag here is measured, not hypothetical — six
instances in this family alone, including a rule whose own verification command
named a Bazel subcommand that does not exist.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-MOD-13 | Verify every flag name, flag default, subcommand and presubmit flag list against the live artefact — the pinned binary's own help output, the release notes for that exact minor, or the tagged source. Never from a prose docs page, a changelog line alone, memory, or an older example. Treat a registry's `incompatible_flags.yml` and a community rc preset's flag catalogue as hand-maintained third-party lists: read them for "is this presubmit-tested", never for "does this flag exist and what is its default". | Six measured doc-lags here: `bazel mod tidy` and `dump_repo_mapping` are absent from the rendered `mod` page in both source trees; the repository-rule doc page never mentions `repository_ctx.repo_metadata()`; the 9.0.0 notes restate `--repo_contents_cache`'s original default without the 8.4.0 walkback and never mention the 9.x re-enablement; `incompatible_flags.yml` lags a flag already flipped; 6 of 8 `--incompatible_*` flags in the canonical 2022 always-on rc post return zero hits in the current CLI reference. Inventing a fifth `--lockfile_mode` value or a `mod tidy --check` is the same failure from the other direction — there are exactly four modes (`update`, `refresh`, `error`, `off`). | `bazel help mod` for subcommands (9.2.0 prints `tidy` in the usage line plus eight query types), then **both** of `bazel help build --long \| grep -A6 -e '--<flag>'` and `bazel help startup_options \| grep -e '<flag>'` — a flag lives in exactly one surface, and `--experimental_remote_repo_contents_cache` is startup-only. `bazel help all --long` is **not a command**, and its `ERROR: 'all' is not a known command` must never be read as "flag absent". **Empty output across both help surfaces means the flag does not exist on that version — that is an answer, not a pass.** | MUST for anyone authoring guidance; SHOULD for a reviewer spot-checking |
| BZL-MOD-33 | On Bazel 9.x, keep `--output_user_root` and `--repository_cache` outside the main repository's own directory tree, including in a scratch or agent workspace. | The repo-contents cache is on by default at 9.2.0 and derives its path from `{--repository_cache}/contents`, so an output user root inside the workspace puts that cache inside the main repository and Bazel hard-fails **every command, `bazel help` included**: `ERROR: The repo contents cache [<path>] is inside the main repo [<path>]. This can cause spurious failures.` The same invocation works on 8.7.0 and 8.8.0, where the cache is off by default — so this surfaces only at the version bump, and surfaces as a total failure rather than a warning. | Run any command on the 9.x pin from the workspace: **the `is inside the main repo` error is the finding**, and the fix is an output root outside the tree or an explicit `--repo_contents_cache=` (empty, to disable). A clean run is the pass. Grep proxy for a committed setup: `grep -rn -e 'output_user_root' -e 'repository_cache' .bazelrc .bazelrc.* .github/workflows` — a path under the repository root is the finding; **empty means the defaults apply, which are outside the tree, and is the pass**. | MUST on Bazel 9.0+; N/A on 8.x |
| BZL-MOD-11 | Never state that a vendored (`--vendor_dir`) build is offline- or airgap-capable without having run it from a clean output user root with the network blocked, on the target Bazel version. | Two open upstream issues show Bazel-internal repositories and output-user-root state escaping a full `bazel vendor //...`, and `bazel mod tidy`'s implicit `buildozer` dependency escaped vendoring entirely before 9.2.0 — so every 8.x pin still carries that hole. The documentation's framing reads as a guarantee at a skim. | Delete the output user root, block the network, then build. **A clean success is the pass; any fetch attempt is the finding.** Below 9.2.0 with `mod tidy` in the pipeline, also vendor `@bazel_tools//tools:tools_for_bazel_subcommands` and require `bazel mod tidy --vendor_dir=<dir> --nofetch` to exit 0. | MUST (as a claim); SHOULD (as a periodic verification) |

## Publishing to a Registry

Caught by one `bazel query` plus a read of `presubmit.yml` before the first
submission. A registry entry is add-only: a published version's `MODULE.bazel`,
`source.json` and patches can never be edited, and a fix is a new version or a
`.bcr.<N>` suffix — so both rows bind before you submit, not after. The
visibility expectations below are the registry's own
[policy playbook](https://github.com/bazelbuild/bazel-central-registry/blob/main/docs/bcr-policies.md).

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-MOD-34 | On a module published to a registry, default every package to `package(default_visibility = ["//visibility:private"])` and mark exactly one target `//visibility:public` per logical entry point — the module-named target, or a private target plus a public `alias`. Never ship a package-wide public default; never leave the canonical entry-point target private. | Every publicly visible target is a public API commitment that cannot be narrowed again without breaking consumers, and the registry runs **no automated check for it** — the gate is human and bot review, so a mistake ships and the add-only rule makes it permanent. The playbook says "keep the set of publicly visible targets small", the registry's automated reviewer repeats it, and across 15 sampled merged pull requests a package-wide public default drew a change request every time it appeared. The opposite failure sits in the same bullet: an entry point that forgets `//visibility:public` is unusable by every consumer. A Starlark `visibility()` call in a `.bzl` file restricts `load()`, not target visibility — two independent mechanisms, and only the target-level one is read here. | `bazel query 'attr(visibility, "//visibility:public", //...)'` — the expected output is exactly the intended entry-point list. Measured on 8.7.0 and 9.2.0 under `query` and `cquery`: it lists a target inheriting public from a `package(default_visibility=)` and omits an explicitly private target in that same package, so a package-wide default is caught rather than hidden. **Empty output is the finding** — no public target means the entry point is unreachable to every consumer; it never reads as "nothing to expose". More labels than entry points is the finding. Never substitute `visible(//..., //...)`: it computes "visible to every target in the queried universe", and `cquery` rejects it outright. | SHOULD (keep the surface minimal); MUST (at least one public entry point) |
| BZL-MOD-09 | On a module published to a registry, never delete or renumber `compatibility_level` on the grounds that the resolver ignores it. Use a per-check skip comment only for a deliberate, stated exception. | The no-op is resolver-only (8.6.0 and 9.1.0); the registry's `bcr_validation.py` still diffs the field against the previous version and blocks the pull request. Reading "it is a no-op" as "delete it" fails the next submission. A skip comment defeats exactly the protection its check provides. | `grep -n -e 'compatibility_level' MODULE.bazel` before and after any edit destined for the registry: a diff on that line is intentional and paired with the skip comment in the pull request. **Empty output on both sides is the pass** — never declared, never changed. A skip label whose thread states no reason is the finding. | MUST when publishing to a registry that gates on it; N/A for a private-registry-only module |

## Gaps

- darwin and Windows are unmeasured: every measurement behind this file ran on a Linux host, so cache paths, `ctx.execute()` environment inheritance and fetch behaviour on those platforms are open.
- The remote repo-contents cache is half-settled: the flag is startup-only and defaults `false` (absent on 8.7.0, present on 8.8.0 and 9.2.0), but eligibility was never exercised against a real server, so BZL-MOD-16's `.marker` line-count test is read, never run.
- `--repo_contents_cache`'s default was read on 8.7.0, 8.8.0 and 9.2.0 only; 9.0.0 and 9.1.0 were not measured, so BZL-MOD-33 binds from 9.0 by derivation rather than by reading.
- The `--lockfile_mode=error` crash was reproduced on 8.7.0 and 9.2.0 on a Linux host and is registry-fetch-side, not filesystem-side; it has not been re-run on a hosted CI runner.
- Registry practice has no automated visibility check, the playbook's wording is scoped to C++ modules with overlays, and BZL-MOD-34's query has never been run against a registry-sized module.

## What Agents Get Wrong Here

1. **Resolving a `MODULE.bazel.lock` conflict textually** — correct for
   `registryFileHashes`, actively wrong for a digest field, and the result
   parses as valid JSON either way (BZL-MOD-03).
2. **Setting `merge=union` on the lockfile** because "it is JSON, and union
   merges are fine" — the one merge shape that writes no conflict markers for
   Bazel's own detector to find (BZL-MOD-04).
3. **Reaching for `ctx.os.environ` because it looks like a normal environment
   dict**, or adding `environ = [...]` beside `getenv()` to be safe: the first
   breaks re-fetch tracking silently, the second marks stale source material
   (BZL-MOD-20).
4. **Assuming a `Label`-typed attribute auto-watches its file** — true before
   Bazel 8.0, false since, and every public example predates the flip
   (BZL-MOD-21).
5. **Treating a `getenv`/`watch` count as proof that a rule which shells out is
   hermetic** — the most consequential misconception in this family
   (BZL-MOD-23).
6. **Carrying WORKSPACE-era vocabulary forward** — `native.register_toolchains()`
   inside an extension, `bazel sync`, a `WORKSPACE.bzlmod` beside a real
   `MODULE.bazel`, or a hard-coded `~` canonical-name separator (BZL-MOD-18,
   BZL-MOD-25).
7. **Citing a lockfile schema number, or inventing a flag that sounds right** —
   a fifth `--lockfile_mode` value, `mod tidy --check`, `bazel help all --long`
   (BZL-MOD-05, BZL-MOD-13).
8. **Building a `--lockfile_mode=error` gate that greps stderr** — the commonest
   staleness case exits 37 through a JVM crash whose text says "This is a bug in
   Bazel, please report", with no lockfile wording in it at all (BZL-MOD-02).
