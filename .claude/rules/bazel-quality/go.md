---
title: Go under Bazel
summary: The BZL-GO family, if you adopt Bazel for Go - SDK sourcing from go.mod, go_deps and use_repo, nogo registration and its analyzer gap, the race flag, cross-compiling with platforms, x_defs stamping, stripping and binary-mode audit
---

# Go under Bazel

Owns `BZL-GO`, and binds only if you adopt Bazel for Go: `go_sdk` sourcing and
the extra SDK a library tests against, `go_deps` and its `use_repo` names,
analyzer modules kept out of `go.mod`, per-module Gazelle overrides, nogo
registration and its `deps`, the external vet that nogo cannot replace, the race
flag, `--platforms` cross-compilation, `x_defs` stamping, `--strip` and the
binary-mode audit of a Bazel-built artifact. It reopens the `rules_go` scope the
rest of this set leaves closed. It does not own `*.go`, `go.mod`, `go.sum`,
`.golangci.*` or the release tooling. The `go-quality` and `go-modules` rule
sets own those files, and a rule here cites their GO-* IDs where the edit is
Bazel-specific. Sibling families, cited never restated: BZL-MOD owns
`MODULE.bazel.lock` and the `bazel mod tidy` diff gate (`bzlmod.md`). BZL-HERM
owns stable versus volatile status and what `--stamp` does not guarantee
(`hermeticity.md`). BZL-CACHE owns what may sit behind a `STABLE_` key
(`caching.md`). BZL-TEST owns test sizing, timeouts and coverage reading
(`testing.md`). BZL-FLAG owns rc-file discipline, the `WORKSPACE` removal and
ruleset version sourcing (`flags.md`). BZL-ARCH owns the `gazelle_test` drift
gate, generator maturity and `config_setting` on `constraint_values`
(`architecture.md`). BZL-LARK owns BUILD and `.bzl` authoring and the buildifier
gate (`starlark.md`). BZL-CI owns matrix legs and target selection (`ci.md`).
`rust.md` carries the Rust twin of the stamping row, BZL-RUST-13.

Contents: [SDK and Dependencies](#sdk-and-dependencies) ·
[nogo Is an Extra Layer](#nogo-is-an-extra-layer) ·
[Test and Build Invocations](#test-and-build-invocations) ·
[Stamping and Shipping a Release](#stamping-and-shipping-a-release) ·
[Gaps](#gaps) · [What Agents Get Wrong Here](#what-agents-get-wrong-here)

Measured 2026-09-26 against Bazel 9.2.0, rules_go 0.63.0, gazelle 0.54.0, Go
1.27.1 and 1.27.0 SDKs, staticcheck 2026.2 (`honnef.co/go/tools` v0.8.1) and
`golang.org/x/tools` v0.50.0, on a `linux_amd64` host. **Unlike `java.md`, a
`bazel` binary was run**: every row except BZL-GO-12 and BZL-GO-13 was watched
fail on a planted violation and pass on a compliant twin. No 8.x leg was run.
Only 2 of 35 public Go exemplars measured build Go with Bazel, both of them the
rulesets' own repositories, so whether to adopt Bazel at all stays with the
`bazel-adopt` skill. Read a ruleset's current version from the BCR's
`metadata.json` (BZL-ARCH-11, BZL-FLAG-28). The rules_go `bzlmod.md` example pins 0.57.0 and
0.45.0, six and nine minors stale on the date above. Every grep reads your own
`MODULE.bazel`, BUILD, `.bzl`, rc and workflow text, so run it from the
repository root. MUST = Block, SHOULD = Warn, CONSIDER = Suggest. **pinned**
marks a default an adopter overrides once, in their own rc file or
`MODULE.bazel`, never per target. Library, SDK and CLI are GO-MOD-01's code
kinds.

## SDK and Dependencies

Bazel picks the Go SDK and the dependency versions itself, from `MODULE.bazel`
and from every Bazel module's `go.mod`, rules_go's own included. These rows are
caught by reading `MODULE.bazel` and by `bazel run @rules_go//go -- version`.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-GO-01 | Declare the SDK with `go_sdk.from_file(go_mod = "//:go.mod")`, or `go_work =`, never `go_sdk.host()`. Treat the `toolchain` line, when present, as the version Bazel builds with, not the `go` line. | `host()` follows the OS package manager, so a host upgrade silently changes the compiler. A reader who takes the `go` line as authoritative misjudges the SDK: `go 1.27.0` plus `toolchain go1.27.1` builds with go1.27.1. | `grep -rn --include='MODULE.bazel' -e 'go_sdk.host(' .`: **empty output is the pass, a hit is the finding.** Then `bazel run @rules_go//go -- version` must print the `toolchain` line, or the `go` line when there is none. Measured rules_go 0.63.0. | MUST |
| BZL-GO-02 | Library or SDK: keep `from_file` for the floor and add `go_sdk.download(version = "1.27.1")` for the current stable patch, with a CI leg running `bazel test --@rules_go//go/toolchain:sdk_version=1.27.1 //...`. CLI: rely on GO-MOD-02's `toolchain` line, and never add a `download` tag to paper over a missing one. | GO-MOD-01 puts a library at `go 1.26.0`, and under Bazel that `.0` floor is the only SDK it builds and tests with, so GO-MOD-16's `stable` leg disappears without a message. | `bazel run --@rules_go//go/toolchain:sdk_version=1.27.1 @rules_go//go -- version` **must print `go1.27.1`**, and the default invocation prints the floor. Watched: default printed `go1.27.0`, the flag printed `go1.27.1`. | MUST for a library or SDK, N/A for a CLI |
| BZL-GO-03 | Never hand-write a `use_repo(go_deps, ...)` name. Run `bazel mod tidy` behind BZL-MOD-10's diff gate. Names are reverse-domain with underscores: `rsc.io/quote` is `io_rsc_quote`, `honnef.co/go/tools` is `co_honnef_go_tools`. | A forward-order guess (`rsc_io_quote`) or a missing entry is an unresolved-repository build break. Only the root module's direct dependencies need an entry. | `bazel mod tidy && git diff --exit-code MODULE.bazel`: **exit 0 with an empty diff is the pass.** Watched on 9.2.0: with `io_rsc_quote` deleted and committed, the build failed (`unknown repo 'io_rsc_quote'`), tidy restored the entry and the diff gate exited 1. On the tidied commit it exited 0. | MUST |
| BZL-GO-04 | Library or SDK: declare an analyzer or tool module, such as `honnef.co/go/tools` for nogo, through a `use_extension(..., "go_deps", dev_dependency = True)` proxy with `go_deps.module(path, version, sum)`, and list every one of its requirements by hand. Never add a `tools.go` blank import or a `tool` line to feed nogo. A CLI may use `tool`, which gazelle 0.47.0 and later surfaces as `GO_TOOLS`. | A `require` in `go.mod` leaks into every consumer's build list (GO-MOD-11), and `//:go.mod` also picks the SDK. `go_deps.module` does not walk the module's own `go.mod`: declaring only honnef failed with `unknown repo 'org_golang_x_exp_typeparams'`. | `grep -rn --include='go.mod' -e 'honnef.co/go/tools' .`: **empty output is the pass** in a library or SDK. Then `bazel build` of a nogo-validated target must succeed, and a planted SA9010 violation must fail it. | MUST for an SDK, SHOULD for a library |
| BZL-GO-12 | Fix one external module's BUILD generation with a per-module `go_deps.gazelle_override`, and upstream it to gazelle's `default_gazelle_overrides.bzl`. Never use `go_deps.gazelle_default_attributes`. | Per gazelle's `bzlmod.md`, `gazelle_default_attributes` switches off the public registry overrides for **every** module, so fixing one module silently breaks others. | `grep -rn --include='MODULE.bazel' -e 'gazelle_default_attributes' .`: **empty output is the pass.** The grep was watched red, the precedence it guards is doc-derived (gazelle 0.54.0, read 2026-09-26). | SHOULD |
| BZL-GO-13 | When a Go module's version looks wrong under Bazel, look in three places in order: a `bazel_dep` that provides that Go module always wins, then `go_deps.config(check_direct_dependencies = ...)`, then the rulesets' own `go.mod` files (BZL-GO-11). `replace` in `go.mod` takes effect only in the root Bazel module. | A plain-`go` mental model answers none of the three. The attribute is `check_direct_dependencies`, not `checks`, and its default only prints a warning. `"error"` fails the build. | Reading heuristic, in the order stated. `grep -rn --include='MODULE.bazel' -e 'check_direct_dependencies' .`: **empty output means the default warning applies**, not that versions agree. Doc-derived, gazelle 0.54.0 source read 2026-09-26. | CONSIDER |

## nogo Is an Extra Layer

**pinned**: nogo is optional. It suits a repository where Bazel is the only
build path, BZL-GO-05 and BZL-GO-06 bind only once a `nogo()` target exists, and
BZL-GO-07 binds always. The gate of record stays `go vet` plus golangci-lint or
staticcheck on the same `go.mod`, because nogo at rules_go 0.63.0 omits four of
go vet 1.27.1's 35 analyzers, cannot host golangci-lint and cannot apply fixes.
The registered shape, measured red and green:

```starlark
# root MODULE.bazel, after go_sdk = use_extension(...)
go_sdk.nogo(nogo = "//:my_nogo")

# root BUILD.bazel
load("@rules_go//go:def.bzl", "TOOLS_NOGO", "nogo")

nogo(
    name = "my_nogo",
    visibility = ["//visibility:public"],
    deps = TOOLS_NOGO + [
        "@co_honnef_go_tools//staticcheck/sa4023",
        "@co_honnef_go_tools//staticcheck/sa9010",
        "@org_golang_x_tools//go/analysis/passes/hostport:go_default_library",
        "@org_golang_x_tools//go/analysis/passes/waitgroup:go_default_library",
    ],
)
```

The registration check, run from the repository root. **Empty output is the
pass, `FINDING` is the finding:**

```bash
if grep -rqs --include='BUILD*' -e '^nogo(' . && ! grep -qs -e '^go_sdk.nogo(' MODULE.bazel; then echo FINDING; fi
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-GO-05 | Register nogo with `go_sdk.nogo(nogo = "//:my_nogo")` in the **root** `MODULE.bazel`. A `nogo()` target alone, or a `WORKSPACE` `go_register_nogo`, analyzes nothing under Bzlmod. | Only the root module's tag is honored. `WORKSPACE` is gone on 9.x (BZL-FLAG-12). Unregistered, the build goes green over every violation, and both public Bazel-for-Go repositories ship exactly that. | The registration check above. Watched: unregistered, planted copylocks and waitgroup violations built with exit 0. Registered, the build failed with `passes lock by value (copylocks)`. | MUST when nogo is used |
| BZL-GO-06 | Build nogo's `deps` from `TOOLS_NOGO`, plus `passes/hostport` and `passes/waitgroup` from `@org_golang_x_tools`, plus the staticcheck analyzers your go-quality gate blocks on (**pinned**: SA9010 and SA4023). Never write `vet = True`, and never add `passes/stdversion`. Re-derive the gap on every rules_go or Go bump. | `vet = True` adds only the 5 analyzers rules_go's `vet = True` names, and beside `TOOLS_NOGO` their labels collide in a duplicate-label analysis error. `TOOLS_NOGO` includes `nilness` (GO-GATE-11) but lacks `cgocall`, `hostport`, `stdversion` and `waitgroup`. `stdversion` is inert under nogo, which reports the SDK's Go version for every package (see [Gaps](#gaps)). | `grep -rn --include='BUILD*' -e 'vet = True' .` and `grep -rn --include='BUILD*' --include='*.bzl' -e 'passes/stdversion' .`: **empty output from both is the pass.** Then the gap audit: diff `go tool vet help`'s analyzers against the uncommented `passes/*` labels in `TOOLS_NOGO` (rules_go's `go/def.bzl`), normalizing `composite` to `composites` and `copylock` to `copylocks`. At rules_go 0.63.0 and Go 1.27.1 it printed `cgocall hostport stdversion waitgroup`, and the accepted residue after this row is `cgocall stdversion`. A planted `WaitGroup.Add` inside a new goroutine built green on `TOOLS_NOGO` alone and failed once `waitgroup` was added. | MUST when nogo is used |
| BZL-GO-07 | Keep the gate of record outside nogo: run `bazel run @rules_go//go -- vet ./...` (GO-GATE-02) and golangci-lint or staticcheck on the same `go.mod` in every Bazel-for-Go repository. Never treat a green `bazel build //...` as the vet or lint pass, and never wire golangci-lint into nogo. | This external vet runs the real `go` command, which reads `go.mod`, so it is the only Bazel-side route that enforces GO-MOD-01's `go`-line floor. The only public golangci-to-nogo bridge calls its golangci half POC-only, has no `MODULE.bazel` and no BCR entry. | `bazel run @rules_go//go -- vet ./...`: **exit 0 with empty output is the pass.** Watched: a waitgroup plant failed it. With `go 1.26.0` and `toolchain go1.27.1`, `vet -stdversion` reported `bytes.CutLast requires go1.27 or later` where nogo stayed green. Plant a floor violation with an ordinary new API: x/tools excludes `testing/synctest` from `stdversion` on every toolchain. | MUST |

## Test and Build Invocations

`bazel test //...` and `bazel build //...` exit 0 in both cases below while
doing something other than what was asked.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-GO-08 | Run the race lane as `bazel test --@rules_go//go/config:race //...` on the host platform (GO-GATE-04's Bazel form). Set a per-target `race = "on"` only on a test that must always run raced. Never write `race` or `pure` as a boolean. | `race` and `pure` are the strings `"on"`, `"off"` and `"auto"`, and `auto` follows the flag, so a plain `bazel test` runs racy code with no detector. Race needs cgo, and cross-compiling defaults to pure. | `grep -rn --include='BUILD.bazel' --include='BUILD' -e 'race = True' -e 'race = False' -e 'pure = True' -e 'pure = False' .`: **empty output is the pass.** Then the lane command must fail on a racy plant. Watched: with no flag the racy test PASSED, with the flag it reported `WARNING: DATA RACE` and FAILED (exit 3). | MUST |
| BZL-GO-10 | Cross-compile with `--platforms=@rules_go//go/toolchain:linux_arm64`, and the matching label for each target, never with `GOOS` or `GOARCH` in the environment of a `bazel` invocation. | Bazel ignores both variables and builds the host binary with exit 0, so the wrong-architecture artifact ships without a message. `config_setting` for these builds follows BZL-ARCH-16. | `bazel build --platforms=@rules_go//go/toolchain:linux_arm64 //cmd/x`, then `file` on the output **must name the requested architecture**. Watched: `GOOS=windows GOARCH=arm64 bazel build` gave `x86-64`, the flag gave `ARM aarch64`. Then `grep -rn -e 'GOOS=' -e 'GOARCH=' -e 'GOOS:' -e 'GOARCH:' .github/workflows`: **a hit on, or an `env:` map above, a step that runs `bazel` is the finding.** | MUST |

## Stamping and Shipping a Release

rules_go never runs the `go` command, so `debug.ReadBuildInfo` carries no
`vcs.*` settings, and the fallback in GO-REL-06 prints nothing under Bazel. A
default `bazel build` is fastbuild, and rules_go 0.63.0 turns Bazel's
`--strip=sometimes` into link `-s -w` for it. The release recipe, measured
byte-identical across two clones in differently named directories with separate
output bases (`linux_amd64` only), is one checked-in rc line:

```bash
build:release -c opt --strip=never --stamp --workspace_status_command=tools/status.sh
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-GO-09 | CLI: stamp **both** the semver and the VCS revision through `x_defs` (`"example.com/x/internal/version.Commit": "{STABLE_GIT_COMMIT}"`) from `--stamp` and a `--workspace_status_command` in the checked-in rc file, written as a workspace-relative path with a directory component (`tools/status.sh`) or an absolute path. Never write `%workspace%/...`, a bare `status.sh` or `./status.sh`, and never put `-X` into `gc_linkopts` or a wrapper script. | GO-REL-06's `ReadBuildInfo` fallback is empty under Bazel, so `x_defs` is the only source. Bazel 9.2.0 runs the command through `/bin/sh -c` in the workspace root without expanding `%workspace%` (the shell fails with `fg: no job control`), and it normalizes `./status.sh` to `status.sh`, a `PATH` lookup (exit 127). `*_test` targets are always unstamped (BZL-HERM-18). | `bazel run --config=release //cmd/x` **must print the stamped version**, and with `--nostamp` the literal default. Then `grep -rnE --include='*bazelrc*' -e 'workspace_status_command[= ]+"?%workspace%' -e 'workspace_status_command[= ]+"?(\./)?[^/[:space:]"]+["[:space:]]' -e 'workspace_status_command[= ]+"?(\./)?[^/[:space:]"]+$' .` and `grep -rn --include='BUILD*' --include='*.bzl' -e 'gc_linkopts.*-X' -e 'ldflags.*-X' -e '"-X"' -e '"-X ' .`: **empty output from both is the pass.** Both greps watched red on the three failing path forms and on a buildifier-formatted `gc_linkopts` plant, where `"-X"` sits on its own line. | MUST for a CLI |
| BZL-GO-14 | A release binary's `x_defs` references only `STABLE_*` keys whose values are a pure function of the commit (BZL-CACHE-20). Never `{BUILD_TIMESTAMP}`, `{BUILD_HOST}`, `{BUILD_USER}` or an unprefixed custom key. Derive any timestamp from the commit (`STABLE_COMMIT_TIME`). | A volatile key breaks byte-for-byte reproducibility, and `BUILD_HOST` and `BUILD_USER` are stable keys that still differ per machine. An `x_defs` target the program never reads is dead-code-eliminated, so the trap hides until the value is printed. | `grep -rnoP --include='BUILD*' --include='*.bzl' -e '\{(?!STABLE_)[A-Z][A-Z0-9_]*\}' .`: **empty output is the pass.** Then build twice from two clones with separate `--output_base` and compare `sha256sum`. Watched: the compliant twin was identical, the `{BUILD_TIMESTAMP}` twin was not. Byte identity is measured for `linux_amd64` only. | MUST for a CLI release |
| BZL-GO-15 | **pinned**: Set `--strip=never` explicitly in the release config, carrying the go-modules set's do-not-strip default (GO-REL-05) into Bazel. Never rely on Bazel's `--strip=sometimes` default, and never assume `-c opt` strips. | `sometimes` strips fastbuild, Bazel's default mode, so a plain `bazel build //cmd/x` release strips and a `-c opt` one does not, the reverse of what agents expect. For pure Go, rules_go 0.63.0 uses the compilation mode for little besides stripping. The unstripped release stayed byte-reproducible. | `file` on the release artifact **must contain `not stripped`**. Then `grep -rn --include='*bazelrc*' -e 'build:release.*--strip=never' -e 'build:release.*--strip never' .`: **empty output is the finding, not the pass.** Watched: default gave `stripped`, `-c opt` and `--strip=never` gave `not stripped`. | SHOULD for a CLI release |
| BZL-GO-11 | Audit a Bazel-built binary from the **unstripped** artifact: bind it first, `ARTIFACT=bazel-bin/cmd/x/x_/x`, then run `go version -m "$ARTIFACT"` and `govulncheck -mode=binary "$ARTIFACT"`. Never audit it from `go list -m all` or source-mode govulncheck alone. A repository that strips anyway triages each finding against a `--strip=never` twin (GO-REL-05). | `go_deps` runs MVS over every Bazel module's `go.mod`, so rules_go 0.63.0's own `go.mod` raised a root's 2017 `golang.org/x/text` pin to v0.26.0 in the linked binary while `go list -m` still showed 2017. A stripped binary lacks the symbols binary mode needs and over-reports: the same tree scanned red stripped and clean unstripped. | `file "$ARTIFACT"` **must say `not stripped`**, then `govulncheck -mode=binary "$ARTIFACT"`: **exit 3 (text format) is a finding, exit 0 the pass.** Watched: an unstripped plant calling `norm.NFC.Bytes` exited 3 on GO-2026-5970 at the uplifted v0.26.0 and 0 after the bump. This is GO-MOD-13's gate, mandatory for a Bazel-built release. | MUST for a CLI release |

## Gaps

- **nogo cannot enforce GO-MOD-01's `go`-line floor.** Under rules_go 0.63.0
  nogo hands every analyzer `pass.Pkg.GoVersion()` equal to the selected SDK
  (`go1.27.1`), never the module's `go 1.26.0`, so `stdversion` in `deps` built a
  too-new-API plant green. BZL-GO-07's external vet is the only Bazel-side floor
  check. No rules_go issue tracks this (searched 2026-09-26). Re-run the probe
  on every rules_go bump.
- `go_deps.from_file(go_work = ...)` and the dependency-cycle divergence in
  [bazel-gazelle#1797](https://github.com/bazel-contrib/bazel-gazelle/issues/1797)
  (open on 2026-09-26) were not fixture-run.
- A `windows_amd64` artifact was compiled and identified by `file`, never run.
  Byte identity under `--strip=never --stamp` was not measured for cross-built
  `darwin` or `windows` artifacts.
- `cgocall` stays out of `TOOLS_NOGO`
  ([rules_go#2396](https://github.com/bazel-contrib/rules_go/issues/2396)) and
  is covered only by BZL-GO-07.
- BZL-GO-12 and BZL-GO-13 are doc-derived. Reproducing BZL-GO-12's precedence
  needs a module that ships a registry override.

## What Agents Get Wrong Here

1. **Declaring `nogo()` and assuming it runs**, or registering it through
   `WORKSPACE` `go_register_nogo`. The build stays green over planted violations
   (BZL-GO-05).
2. **Copying `bazel_dep` versions from the `bzlmod.md` example or a ruleset's
   own `MODULE.bazel`**, six to nine minors stale (BZL-ARCH-11, BZL-FLAG-28).
3. **Treating a green `bazel test //...` as the race lane.** `race = "auto"`
   without the flag runs no detector (BZL-GO-08).
4. **Reading `TOOLS_NOGO` as "go vet" and dropping the vet or golangci step**,
   or writing `vet = True` beside it "to be thorough" (BZL-GO-06, BZL-GO-07).
5. **Adding `passes/stdversion` to nogo and believing the `go 1.26.0` floor is
   enforced.** It builds green whatever the code calls (BZL-GO-06, BZL-GO-07).
6. **Adding `tools.go` or a `tool` line to a library to get staticcheck into
   nogo**, leaking a `require` to every consumer (BZL-GO-04).
7. **Copying `--workspace_status_command=%workspace%/status.sh` into the rc
   file, or "fixing" it to `./status.sh`.** Both fail the stamped build
   (BZL-GO-09).
8. **Setting `GOOS` and `GOARCH` around `bazel build`.** It exits 0 with a host
   binary (BZL-GO-10).
9. **Porting `-ldflags -X` into `gc_linkopts`, or trusting `ReadBuildInfo` for
   the commit.** `go version -m` on a Bazel binary shows no `vcs.*` lines
   (BZL-GO-09).
10. **Stamping `{BUILD_TIMESTAMP}` or `{BUILD_HOST}` and assuming `--stamp`
    keeps the build reproducible** (BZL-GO-14, BZL-HERM-18).
11. **Shipping a plain `bazel build //cmd/x`, which strips, or assuming `-c opt`
    strips.** Both are backwards for Go under Bazel (BZL-GO-15).
12. **Auditing a Bazel binary with `go list -m all` or source-mode govulncheck,
    or treating a stripped scan's count as final** (BZL-GO-11).
13. **Writing `race = True`, then "fixing" the type error with `"true"`**
    (BZL-GO-08).
14. **Hand-typing `use_repo` names in forward domain order** (`rsc_io_quote`)
    instead of running `bazel mod tidy` (BZL-GO-03).
15. **Wiring `gazelle -mode=diff` as a shell step and calling the drift gate
    done.** The gate is `bazel test //:gazelle_test` (BZL-ARCH-12).
16. **Reaching for `go_sdk.host()` because it needs no version** (BZL-GO-01),
    or `gazelle_default_attributes` to fix one module, which disables every
    public override (BZL-GO-12).
17. **Planting a `testing/synctest` call to test a stdversion gate, seeing
    green, and "fixing" working wiring.** x/tools excludes that package on every
    toolchain (BZL-GO-07).
