# Fetches, extensions and target selection

Read this while running section F or G. It holds the repository-rule and
module-extension diagnosis, the repo-contents-cache and lockfile behaviour per
version, and the two selection tools' flag surfaces and measured blind spots.

Contents: [Which phase is the problem in](#which-phase-is-the-problem-in) ·
[The workspace-rules log](#the-workspace-rules-log) ·
[Re-fetching one repo](#re-fetching-one-repo) ·
[Extension purity, dynamically](#extension-purity-dynamically) ·
[The repo contents cache](#the-repo-contents-cache) ·
[Lockfile failures](#lockfile-failures) ·
[Target selection](#target-selection)

Measured on Bazel 8.7.0, 8.8.0 and 9.2.0 on Linux. Every grep in this file is
blind to `BUILD` and `.bzl` text that a repository rule or module extension
**generates as a string** into another repository — only a test that actually
builds from the generated repo sees that content.

## Which phase is the problem in

Repository rules and module extensions run in the **loading phase**. Three
consequences that redirect a diagnosis immediately:

- They never appear in the execution log. An empty execution-log diff says
  nothing about them.
- No sandbox flag reaches them. `--sandbox_default_allow_network=false` is
  execution-tagged and cannot govern a `repository_ctx.download`/`.execute`
  (BZL-FLAG-24, BZL-HERM-02).
- A repository rule cannot be invoked outside module-extension evaluation on
  either major, and **the error text differs**: 8.7.0 says `repository rules can
  only be used while evaluating a WORKSPACE file`, 9.2.0 says `repo rules can
  only be called from within module extension impl functions`. A log grep written
  against one wording silently misses the other — match on
  `Error in repository_rule:` alone (BZL-MOD-31, BZL-TEST-16).

## The workspace-rules log

```
bazel clean --expunge
bazel build --experimental_workspace_rules_log_file=/tmp/wsl.log //...
# in a bazelbuild/bazel source checkout at the same tag as your pin: the
# workspacelog parser ships in no release archive and needs a local JDK
# (the same constraint BZL-CACHE-16 states for the execlog parser)
bazel build src/tools/workspacelog:parser
bazel-bin/src/tools/workspacelog/parser --log_path=/tmp/wsl.log > /tmp/wsl.txt
grep -nE '"which"|sha256: ""' /tmp/wsl.txt
```

The flag name and its `experimental_` prefix are unchanged on both majors. Any
hit names a non-hermetic call — a host binary resolved by `which`, or a download
with no checksum (BZL-HERM-16, BZL-MOD-22).

**EMPTY reads narrowly**: no host probe and no unchecksummed download *on this
host, this run, with this `--repo_env`*. It does not clear the rule for another
platform or another environment. The `clean --expunge` is load-bearing — without
it the rules do not re-run and the log is empty for the wrong reason.

## Re-fetching one repo

| Intent | Command | Note |
|---|---|---|
| Re-fetch one repository | `bazel fetch --repo=@<repo>` | Works on both majors |
| Re-fetch, ignoring existing state | `bazel fetch --force --repo=@<repo>` | Re-runs regardless of the `configure` attribute |
| Re-run only host-probing rules | `bazel fetch --force --configure` | Measured: re-executes only rules declaring `configure = True`; a non-configure rule's fetched state is left byte-identical (BZL-MOD-26) |
| ~~`bazel sync --only=<repo>`~~ | — | **Deleted at 9.0.0.** Several rulesets' own live documentation still prints it; it is a hard failure on a 9.x pin (BZL-FLAG-31) |

A rule that inspects the host (`ctx.os`, `ctx.which`, toolchain autodetection)
sets `configure = True` — the observable consequence of omitting it is that
`fetch --force --configure` will not refresh it after a host change, which
presents as "the toolchain is stale and nothing re-detects it".

## Extension purity, dynamically

The static check is a grep for `ctx.os`/`getenv` in the extension's own impl and
every non-repository-rule `.bzl` it loads (BZL-MOD-14). The dynamic check, which
is what settles an argument:

1. `bazel mod deps --lockfile_mode=update`, then copy `MODULE.bazel.lock`.
2. Re-run unchanged as a control — the lock must be byte-identical.
3. Re-run under a changed environment, e.g.
   `--repo_env=<SITE_VAR>=<other value>` and `--repo_env=HOME=/tmp/other`.
4. Diff the extension's lock entry on five fields: `bzlTransitiveDigest`,
   `usagesDigest`, `recordedRepoMappingEntries`, `generatedRepoSpecs`,
   `envVariables`.

**All five identical across steps 2–4 confirms purity** — an EMPTY diff is the
pass here, the inverse of most steps in this skill. Any field moving under an
environment change refutes a `reproducible = True` claim, and that claim being
wrong is silent by construction: the divergent repos carry no lockfile diff to
catch them (BZL-MOD-15).

The environment reads usually live in the **repository rules the extension
instantiates**, not in the extension itself; those are evaluated later and
separately and are outside what `mod deps` inspects. A clean extension does not
clear its repository rules — that is what the workspace-rules log is for.

## The repo contents cache

| Version | `--repo_contents_cache` | `--experimental_remote_repo_contents_cache` |
|---|---|---|
| 8.7.0 | `""` — **opt-in**, disabled by default | absent entirely |
| 8.8.0 | `""` — opt-in | present, `false`, **a STARTUP option** — invisible to `help build --long` |
| 9.2.0 | **on by default**, deriving to `{--repository_cache}/contents` | present, `false`, startup option |

Two readings that are easy to get backwards (BZL-MOD-16):

- The observed **local**-cache gate is an explicit
  `repository_ctx.repo_metadata(reproducible = True)` return. An implicit
  `return None` grants nothing. `repo_metadata()` itself works on 8.7.0, 8.8.0
  and 9.2.0.
- A `getenv()` call inside the rule does **not** exclude it from the local cache.
  Measured: a rule reading `HOME` was cached and hit across `clean --expunge`
  exactly like a rule that read nothing. The `getenv`/`watch` exclusion belongs
  to the remote cache discussion only.

Confirming a hit: fetch twice with `clean --expunge` between, with a `print()` in
the rule body. **No DEBUG line on the second fetch is the hit** — a populated
cache directory alone is a weaker claim, since a populated-but-unused directory
looks identical.

Cache growth attribution, since these are three different directories with three
different GC stories: the classic repository cache has **no** automatic cleanup
and updates **mtime** (never atime) on a hit, so an `-atime` sweep measures the
wrong timestamp (BZL-CACHE-32); the repo contents cache has age-only GC
(`--repo_contents_cache_gc_max_age`, default `14d`) and **no size cap**;
`--disk_cache` has both bounds available and both default to unbounded
(BZL-CACHE-17).

## Lockfile failures

`--lockfile_mode=error` in a dedicated CI leg is the freshness gate (BZL-MOD-02).
Gate it on the **exit code**, never on the message, because there are three
distinct shapes and all exit **37**:

| Trigger | Message shape |
|---|---|
| An **existing** dependency's locked version changed (bump or downgrade) | An internal crash: `java.lang.IllegalStateException: Cannot fetch a file without a checksum in ENFORCE mode. This is a bug in Bazel, please report…`, through `YankedVersionsFunction` → `IndexRegistry.getYankedVersions`. Reproduces on both majors; not a network artefact |
| `MODULE.bazel` gained a **brand-new** `bazel_dep` | The clean documented `Missing checksum for registry file … run 'bazel mod deps --lockfile_mode=update'` |
| The lock's schema version is not supported by this binary | `The version of MODULE.bazel.lock is not supported by this version of Bazel` |

A CI consumer parsing stderr for a specific string will not find one in the first
case. `bazel mod deps` alone reproduces the crash; an explicit `--registry`
changes nothing.

**Schema versions move inside a major**, so never cite one from documentation or
memory — read it from the file in hand (BZL-MOD-05): `lockFileVersion` is `24`
on 8.7.0 and `28` on **8.8.0** and 9.2.0, the later shape adding a
`factsVersions` key. Treating a same-major patch bump as lockfile-format-stable
is wrong at exactly the 8.7.0 → 8.8.0 step.

Merge-driver note for a conflicted lock: configure Bazel's own
`bazel-lockfile-merge` jq driver, scoped to `MODULE.bazel.lock` only, and never a
generic line-based driver such as `union` — a JSON-unaware line driver writes no
conflict markers, so Bazel's own conflict-marker detection never fires and a
merged lock can silently carry both sides (BZL-MOD-04). That driver is
schema-specific: never point it at another tool's lockfile.

## Target selection

A change broke something the tool did not schedule. Read what actually ran before
changing anything.

**`bazel-diff`.** `bazel-diff -v generate-hashes …` prints a literal
`Executing Query:` line. `deps(//...:all-targets)` means `--useCquery` is on;
anything else is the default plain-`query` mode. That default is **blind to any
transitive dependency introduced only by executing a repository rule or module
extension** — pip, npm, Maven, most Bzlmod extensions (BZL-CI-21). Two ways to
close the class: `--useCquery` (Bazel ≥6.2.0, no list to maintain) or an
exhaustive `--fineGrainedHashExternalRepos` allow-list that nothing verifies.
Choosing neither is allowed only if the wrapper states the miss class is live
(BZL-CI-30). Also: `--excludeExternalTargets` means two different things per
subcommand — on `generate-hashes` it skips querying `//external:all-targets`, on
`get-impacted-targets` it drops `//external:*` from the output — and a raw
`//external:…` label must never be fed into `bazel build`/`test` on a Bzlmod-only
repo (BZL-CI-23).

**`target-determinator`.** Its results cache does not key on a home or system
`.bazelrc`, an environment variable affecting `cquery` output, or the host
machine — pass `--nocache_results` whenever any of those changed, and never share
a cache directory across runners of differing OS or arch (BZL-CI-22).
`-filter-incompatible-targets` defaults **true** and silently drops
platform-incompatible targets before the caller sees them;
`-before-query-error-behavior` defaults to `ignore-and-build-all`, so a failed
"before" query degrades to building everything rather than erroring.

**How an empty diff reads.** "No evidence of a difference under this tool's own
blind spots, with these flags" — never "definitely nothing changed". Name the
flags that were in effect for that specific invocation before acting on it
(BZL-CI-25).

**One class neither tool models by default**: a target that reads undeclared
workspace state at execution time — a repo-scanning linter run as a test — hashes
as unchanged and is wrongly skipped. Tag it and pass the tag to
`bazel-diff --alwaysAffectedTags`; `target-determinator` publishes no equivalent
flag, so on that tool the same target needs a different mitigation (BZL-CI-24).

**The backstop is not optional.** `grep -rn -e 'schedule:\|cron' --include=*.yml .github/workflows`
for an unrestricted `bazel test //...`. EMPTY on a repo already using selection is
itself the finding (BZL-CI-27): both miss classes are silent by construction, so a
periodic full run is the only check that does not depend on the tool being right.

**Two things not to invent.** No source publishes a target-count threshold at
which selection becomes correct to adopt — any guidance quoting one is inventing
provenance (BZL-CI-01). And `--useCquery`'s cost at scale is untested: the
measurement showing it closing the miss class is one small reproduction, not a
performance result.
