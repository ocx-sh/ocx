# The Rust branch

Read this when the pilot is Rust. It holds the ruleset and toolchain pins, the
crate_universe lock gate and why it is the inverse of Bzlmod's, the
`crates_vendor` and prost sub-branches, and the first three rules to satisfy.

Contents: [At a glance](#at-a-glance) · [Toolchain pin](#toolchain-pin) ·
[The lock gate](#the-lock-gate) · [Repinning](#repinning) ·
[Sub-branch: crates_vendor](#sub-branch-crates_vendor) ·
[Sub-branch: prost and tonic](#sub-branch-prost-and-tonic) ·
[Generation](#generation) · [Build scripts](#build-scripts) ·
[Tests and coverage](#tests-and-coverage) ·
[First three rules](#first-three-rules)

## At a glance

| | |
|---|---|
| Ruleset | `rules_rust` **0.74.0** (2026-09-06). Pre-1.0, so staleness is judged on the **second** component: `0.73 → 0.74` is a major bump (BZL-FLAG-29) |
| Bazel floor | CI-tested at 7.4.1 exactly; the released module declares no `bazel_compatibility`, which reads as "undocumented", never "no floor" (BZL-FLAG-27) |
| Toolchain pin | `rust.toolchain(versions = [...], edition = "…")`, explicit, in the root module |
| Lock mechanism | `crate_universe` with an explicit `lockfile =`; the Bazel-side lock is generated, `Cargo.toml` stays authoritative |
| Freshness gate | Runs on **every ordinary build**; what must be absent is an environment variable |
| Generator | `gazelle_rust` **0.1.0**, single maintainer, one tag — experimental. Coarse hand-written BUILD files are the default |
| Platform note | The ruleset's own docs disclaim reliable Windows support in the maintainers' words; treat any Windows leg as measured-or-nothing |

## Toolchain pin

Call `rust.toolchain(versions = [...], edition = "…")` explicitly and pin every
entry. The tag class's default is hermetic but **not pinned**, and it moves: the
default Rust version changed in four of five consecutive ruleset releases. Give a
`beta` or `nightly` entry its ISO date, and drop the channel you do not select
(BZL-RUST-31).

```sh
grep -n 'rust.toolchain(' MODULE.bazel
```

**Empty output on a repository that uses the `rust` extension is the finding** —
it is riding a default that moves on every `bazel_dep` bump with no error and no
warning.

A `rust-toolchain.toml` alongside Bazel is **not read by the ruleset at all**: a
grep for the filename across the whole 0.74.0 tag returns zero matches. Where one
exists, add a CI step comparing its `channel` against the first stable entry of
`rust.toolchain(versions = [...])` and fail on mismatch. Never state or imply the
ruleset reconciles the two.

On a Rust-only repository, set both `--repo_env=BAZEL_DO_NOT_DETECT_CPP_TOOLCHAIN=1`
and the ruleset's `use_cc_toolchain=false` setting; in a mixed Rust/C workspace use
per-crate annotations instead, never the global flag (BZL-RUST-14).

Set `supported_platform_triples` to exactly the platforms CI and developer hosts
build for. Splicing is quadratic per triple added, so the curated default is
deliberately narrow and omits some real hosts; add what you need, remove what no
target resolves to, and never reason about this list from the toolchain's
`extra_target_triples` or the reverse (BZL-RUST-08).

## The lock gate

**crate_universe's freshness gate is the inverse of Bzlmod's, and this is the
single most important thing to get right on day one.** Bzlmod's lock is checked
by a flag you add on one CI leg. crate_universe's is checked by
`determine_repin()`, which runs on **every ordinary build** and hard-`fail()`s on
a digest mismatch — unless `CARGO_BAZEL_REPIN` is set. So there is no flag to
add; what must be **absent** is the environment variable (BZL-RUST-02).

Set an explicit `lockfile =` on every crate_universe instance —
`crate.from_cargo(lockfile = …)` or `from_specs(lockfile = …)` under Bzlmod —
**including in a non-root module**, where omitting it is an outright `fail()`
because repinning is disabled there unconditionally.

```sh
grep -n 'from_cargo(\|from_specs(\|crates_repository(' MODULE.bazel *.bzl
```

Read each hit forward to its closing paren and confirm a `lockfile =`. **A hit
with no `lockfile` is the worst configuration in the ruleset**: `determine_repin()`
returns `True` unconditionally, every build silently re-splices forever, and no
freshness check is even possible (BZL-RUST-01).

Give the generated lock a **JSON-aware** git merge driver, never `union`, `ours`
or any line-based driver, and never Bazel's own `MODULE.bazel.lock` driver, which
is schema-specific to that file (BZL-RUST-05). Same silent-corruption shape as the
module lock: a line-based merge desyncs an opaque generator-computed checksum with
no conflict markers at all.

```sh
git check-attr merge <the-generated-lock>
```

**A generic driver name, or `unspecified`, is the finding.**

After any interrupted repin — a killed CI job, a Ctrl-C, an OOM — validate the
lock parses and carries a non-empty `checksum` before trusting it. The writer is a
plain write, not temp-file-then-rename, so a mid-write kill can leave truncated
JSON; the next query fails loudly but nothing repairs it.

## Repinning

```sh
CARGO_BAZEL_REPIN=1 bazel fetch --repo=@<crates-repo>
```

Never `bazel sync --only=<repo>`, whatever the ruleset's own documentation prints
— and it does still print it, on `main` and on the rendered docs page. `bazel sync`
was **removed at 9.0.0**; the recipe hard-fails the day the pin crosses it.
`bazel fetch --repo=` is valid on 8.7.0 and 9.2.0 alike, so it needs no version
branch (BZL-RUST-03).

Scope every `CARGO_BAZEL_*` variable to the single job that needs it:
`CARGO_BAZEL_REPIN` to a `workflow_dispatch`- or `schedule`-triggered repin job.
An ordinary build job that leaves it unset **is** the drift gate (BZL-RUST-02).

```sh
grep -rn 'CARGO_BAZEL_' .github/workflows/
```

**Empty output = every ordinary lane is a drift gate = pass.** A hit outside a
dedicated repin or private-registry job is the finding.

## Sub-branch: crates_vendor

Default to `crate.from_cargo` for a repository nothing else builds against.
Switch to `crates_vendor` in `mode = "remote"` the moment something does — a
registry-published module, or an internal library another module depends on. A
non-root consumer of `from_cargo` can never repin and must trust a
producer-owned lock (BZL-RUST-07).

`crates_vendor` writes committed source that no ordinary build re-derives, so the
build-time gate above does not cover it. Its CI check is regenerate-then-diff, and
the target is tagged `manual` so `//...` does not run it:

```sh
bazel run //<pkg>:crates_vendor && git diff --exit-code -- <vendor_path>
```

**Exit 0 with an empty diff = the vendored tree is fresh.** A non-empty diff is
the finding; a green build alone proves nothing here (BZL-RUST-04).

One security note before wiring a vendor repin into CI: the wrapper ends in a full
ambient-environment passthrough minus exactly one variable, so read that job's
environment as if every variable in it were being handed to an arbitrary
subprocess, because it is (BZL-RUST-36).

## Sub-branch: prost and tonic

Generate Rust protobuf and gRPC code with `rust_prost_library` on a
`proto_library`, adding the prost rules as their own `bazel_dep` at the same
version as the main ruleset, and author a toolchain naming **this repository's
own** crate_universe-resolved prost and tonic versions rather than accepting the
shipped default (BZL-RUST-33).

Never cite `rust_proto_library` — the ruleset no longer ships it, though its own
example README still links the page — and never keep a `build.rs` running a
protobuf code generator as the Bazel path.

```sh
grep -rn 'rust_proto_library' --include='BUILD*' --include='*.bzl' .
```

**Empty = pass.** Any hit names a rule that does not exist on the current ruleset.

One `proto_library` per `.proto` set, with each consuming language attached
through its own wrapper or aspect — never a per-language duplicate (BZL-ARCH-31).

## Generation

`gazelle_rust` is experimental: one maintainer, one published tag, a roadmap that
is a personal backlog issue. **Coarse, hand-maintained BUILD files at directory
level are the correct default for Rust** (BZL-ARCH-11).

Independently of the generator, every internal edge between first-party workspace
crates is a hand-written `deps = ["//crates/<name>"]` entry. crate_universe filters
every workspace-member node out before rendering, so there is no code path that
could wire it (BZL-RUST-18). This is the migration's largest hand-authored surface
and the number to estimate before committing to a date.

If the generator is adopted anyway, four things bind:

- Wire the `gazelle_test` freshness target yourself; no ecosystem ships it
  (BZL-ARCH-12). `bazel query 'kind("sh_test", //:gazelle*)'` — **empty output on
  a repository that believes it has a freshness gate is the finding.**
- Point it at the bzlmod-produced crate_universe lockfile, never a raw
  `Cargo.lock`, whose transitive entries both break unused-dependency reporting
  and can add a `deps` entry for a crate with no top-level target (BZL-RUST-34).
  `grep -rn 'rust_cargo_lockfile'` — **empty = pass.**
- A `use` behind a `#[cfg(...)]` resolving through a `select()` needs both an
  explicit ignore attribute on the `use` and a `# keep` comment on the BUILD
  `deps` line, or the generator rewrites the `select()` away (BZL-RUST-35).
- Review the regenerated BUILD diff by hand on every version bump of a pre-1.0
  plugin, in the same PR as the bump (BZL-ARCH-13).

## Build scripts

A Cargo build script becomes a `cargo_build_script` target, with three traps:

- `cargo:rerun-if-changed` and `cargo:rerun-if-env-changed` are parsed and
  **discarded** by design; Bazel re-executes on declared inputs. Leaving them in
  for Cargo's benefit is fine; treating them as load-bearing is not.
- `cargo:rustc-link-arg-bin`, `-bins` and `cargo:rustc-cdylib-link-arg` are
  recognised as unsupported, warned about and dropped. Per-binary link-arg scoping
  silently stops applying with no build failure.
- Any build script that opens `.git` — directly or through a provenance crate —
  hits its no-git fallback on **every** Bazel build, not only tarball builds, and
  that fallback must be non-panicking (BZL-RUST-12). A provenance field that must
  actually resolve goes through `--workspace_status_command` plus the compile
  action's env-file mechanism, with `stamp` set explicitly (BZL-RUST-13).

A third-party crate whose build script probes the host gets an annotation naming
the exact variable that forces the self-contained path — never a patched fork
(BZL-RUST-16).

## Tests and coverage

Cargo auto-discovers unit and integration tests from one crate layout; Bazel needs
an explicit target for each, and a missing one is a silent green.

- Every library or binary carrying a `#[cfg(test)]` module gets a matching
  `rust_test(crate = …)`; every file under a crate's `tests/` directory gets one,
  with `crate_root` set explicitly when `srcs` holds more than one file. **When
  certifying migration parity, diff test names, not counts** (BZL-RUST-27).
- Doc tests are the one Cargo test kind Bazel never auto-generates under any
  mechanism: every crate with runnable fenced blocks in doc comments needs an
  explicit `rust_doc_test(crate = …)` (BZL-RUST-28).
- A test-suite macro whose `srcs` glob matches nothing yields a target that
  exists, is empty, and passes green with zero tests run (BZL-RUST-30).
- Every fixture a test reads at run time goes in `data`, never `compile_data` and
  never an ambient path. A test with neither passes locally against the real
  filesystem and fails hermetically (BZL-RUST-29).

Lint aspects are registered in `.bazelrc` with `--output_groups=`; there is no
per-target attribute to reach for. Register the formatter aspect in a **CI-only**
config, on the ruleset's own recommendation; the linter aspect stays in the
default config (BZL-RUST-23, BZL-RUST-24).

Before handing the repository back, run the ruleset's IDE setup target, commit
the editor settings it writes, and gitignore its launcher cache — without it the
repository has a working build and no working IDE (BZL-RUST-20).

## First three rules

Satisfy these before the pilot's first green build; everything else follows.

1. **BZL-RUST-01** — an explicit `lockfile =` on every crate_universe instance.
2. **BZL-RUST-31** — `rust.toolchain(versions = [...], edition = …)` pinned
   explicitly in the root module.
3. **BZL-RUST-18** — every first-party internal edge hand-written in `deps`.
