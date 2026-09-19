# Research: Cargo Workspace Idioms for the `ocx_*` Crate Split

## Metadata

**Date:** 2026-09-16
**Domain:** packaging (Rust / Cargo workspace tooling)
**Triggered by:** `adr_crate_split_workspace.md` — splitting `ocx_lib` into 17
`ocx_*` library crates, resolver 3, one binary, cargo-dist release, two
lockstep path-dependency satellites via git submodule.
**Expires:** 2027-03-16 (6 months)
**Toolchain checked against:** `cargo 1.95.0` / `rustc 1.95.0`
(`rust-toolchain.toml` pin on this host). **Current upstream stable is newer:
Rust 1.97.1** (2026-07-16, [blog.rust-lang.org/2026/07/16/Rust-1.97.1](https://blog.rust-lang.org/2026/07/16/Rust-1.97.1/)),
two minors ahead — Q1 has a finding gated on that gap.

## Direct Answer

1. Add `edition` + `rust-version` to `[workspace.package]`; every new manifest
   inherits all five fields plus `lints.workspace = true` and `publish = false`.
2. Keep the existing explicit forward-list pattern for `__testing`; no
   `unexpected_cfgs`/`check-cfg` config needed anywhere — it's a real feature.
3. No action for resolver 3 itself (already the default); rule the
   `unreachable_pub` ratchet through LINT-16's per-lint baseline, not
   `[workspace.lints]`.
4. The ADR's three ecosystem-contract lines are correct and undisturbed by
   any 2025–2026 Cargo change.
5. `cargo nextest run -p <crate>` per changed crate; doctests need a separate
   `cargo test --doc` this repo doesn't run today — pre-existing gap, not a
   split regression.
6. `cargo metadata --no-deps` for path→package mapping (no submodules
   needed); full `cargo metadata` for reverse-dependent counts (submodules
   must be checked out — reproduced the failure on this host).
7. Add none of cargo-hack/udeps/machete now; none is wired into `task
   verify` today. Revisit cargo-machete only past ~30 members or a real
   unused-dep incident.
8. `dist-workspace.toml`'s `members = ["cargo:."]` and `cliff.toml`'s
   `*({{commit.scope}})*` are both crate-count-agnostic — zero edits needed.
9. sccache and `incremental` are mutually exclusive by design; the ADR's
   2.5–3s leaf-touch projection is directionally right, but a published 2026
   case study says link time/caching, not the split itself, is the bigger
   lever — and this host has neither mold nor `ld.lld` on `PATH`.
10. Nothing in the 2025–2026 Cargo pipeline changes the calculus here:
    public/private dependencies and `cargo script` are both still
    nightly-only; `[workspace.lints]` (2023) and `build.build-dir` (2026) are
    already settled and orthogonal to this migration.

---

## Q1 — Manifest inheritance

**Recommendation: add `edition`/`rust-version` to `[workspace.package]`
(currently absent — root only has `version`/`license`/`description`/
`repository`/`homepage`); every new crate is the template below.**

```toml
[package]
name = "ocx_oci"
version.workspace = true
edition.workspace = true      # NEW at root
license.workspace = true
repository.workspace = true
rust-version.workspace = true # NEW at root
publish = false

[lints]
workspace = true

[dependencies]
ocx_util = { workspace = true }
ocx_console = { workspace = true }
ocx_exit = { workspace = true }
serde = { workspace = true }

[dev-dependencies]
ocx_test_support = { workspace = true }
```

Root delta: two new `[workspace.package]` keys, plus one `[workspace.dependencies]`
row per new crate, following the existing `ocx_lib`/`ocx` path-dep pattern.

**Pitfalls, most load-bearing first:**

- **`default-features` override needs Rust 1.99+, which doesn't exist yet.**
  Cargo's docs: "`default-features` (Edition 2024, requires Rust 1.99+):
  Overrides the value set in `[workspace.dependencies]`... Before Rust 1.99
  ... package-level `default-features = false` may be ignored or rejected"
  ([specifying-dependencies.md](https://github.com/rust-lang/cargo/blob/master/doc/book/src/reference/specifying-dependencies.md)).
  Current stable is 1.97.1 — the override isn't available on any released
  toolchain. If two `ocx_*` crates need different `default-features` on the
  same third-party dep, don't rely on a per-crate override of an inherited
  entry; declare that dependency directly in whichever crate needs the
  non-default value.
- **Feature lists are additive, never overriding** — a crate's `features =
  [...]` on an inherited dep unions with `[workspace.dependencies]`'s own list.
- **`optional` is package-only** — `[workspace.dependencies]` cannot declare
  it; a crate opts in with `foo = { workspace = true, optional = true }`.
- **`[dev-dependencies]` inherits identically** (`rand.workspace = true`) —
  `ocx_test_support` should be one `[workspace.dependencies]` entry.

Citations: [Workspaces § dependencies table](https://github.com/rust-lang/cargo/blob/master/doc/book/src/reference/workspaces.md); [Specifying Dependencies § Inheriting](https://github.com/rust-lang/cargo/blob/master/doc/book/src/reference/specifying-dependencies.md).

## Q2 — Feature forwarding across crates (`__testing`)

**Recommendation: keep the existing explicit-list pattern
(`__testing = ["ocx_oci/__testing", ...]` in `ocx`'s `[features]`), one entry
per crate carrying the seam. No `dep:` syntax (non-optional path deps). No
`[lints.rust.unexpected_cfgs]` block needed anywhere.**

Already the idiom today: `ocx_lib/Cargo.toml:26` declares `__testing = []`,
`ocx_cli/Cargo.toml:24` forwards `__testing = ["ocx_lib/__testing"]`. The
split only widens the forward list from one name to seven.

**CI assertion:** the ADR's own item (line 829) is right — assert the
forward list equals `grep -rl 'feature = "__testing"' crates/*/src` mapped to
crate names. `cargo metadata --format-version 1 --locked` (no extra
`--features`) exposes `resolve.nodes[].features` per package id — assert
`"__testing"` is absent from every `ocx_*` node with no build flags. `cargo
tree -e features -p ocx --no-default-features` is a fine manual check but is
text, not a structural assertion.

**`unexpected_cfgs`: not applicable — load-bearing finding.** Cargo
auto-declares the cfg for every `[features]` entry as expected: "Cargo
automatically declares corresponding cfgs for every feature as expected"
([rustc book — Cargo Specifics](https://doc.rust-lang.org/rustc/check-cfg/cargo-specifics.html)).
Because `__testing` is a real `[features]` entry everywhere it's read (not a
raw injected `--cfg`), `#[cfg(feature = "__testing")]` never trips
`unexpected_cfgs` — zero extra lint config across all 17 manifests. One
related bug to track if a *raw* cfg is ever added workspace-wide:
[rust-lang/cargo#15933](https://github.com/rust-lang/cargo/issues/15933) —
`unexpected_cfgs` settings in `[workspace.lints.rust]` don't propagate to
members (closed as dup of #15579, unresolved) — such a cfg's `check-cfg`
entry would need repeating per crate, not hoisting to the workspace table.

## Q3 — Resolver 3 + edition 2024 specifics

**Recommendation: no manifest action for resolver 3 (already the default,
`resolver = "3"` at root); its only delta is `rust-version`-aware resolution,
which needs a per-crate `rust-version` to bite (Q1). For `unreachable_pub`:
follow the LINT-16 baseline-and-ratchet path exactly as the ADR specifies —
do not add it to `[workspace.lints.rust]` until its count is zero.**

Resolver 3 changes `resolver.incompatible-rust-versions` from `allow` to
`fallback` (MSRV-aware selection, stabilized Rust 1.84,
[PR #14639](https://github.com/rust-lang/cargo/pull/14639)). It has no
observable effect today since `rust-toolchain.toml` pins one exact channel
for every build; it matters the day a satellite builds at an older toolchain
than ocx's `rust-version` floor.

**Ratchet precedent:** no single strongly-sourced "here's a repo that
ratchets `unreachable_pub`" writeup surfaced, but `rust-lang/rust` itself
rolls the lint out crate-by-crate rather than one repo-wide flip — [PR
#134286, "Enable `unreachable_pub` lint in core"](https://github.com/rust-lang/rust/pull/134286)
and [PR #136595 for the hermit target](https://github.com/rust-lang/rust/pull/136595)
are separate, sequential landings against separate crates in the same tree —
structurally the same shape LINT-11/LINT-16 already specify. The plan needs
no new mechanism, only the phase-0.8 baseline file that doesn't exist yet
(confirmed: `find . -iname clippy-warn-baseline.json` → zero hits).

## Q4 — Path-dependency consumers via git submodule

**Recommendation: the ADR's three ecosystem-contract lines are correct and
untouched by any 2025–2026 Cargo change. Consumer-side manifest block:
unchanged from what `ocx-mirror` already carries — repeat `[patch.crates-io]`
verbatim, pointed at the consumer's own submodule checkout, and re-declare
`serde_json/preserve_order` + `serde_json/raw_value` wherever the consumer
takes `ocx_index`/`ocx_sign`.**

**Line 1** — Cargo's docs, verbatim: "`[patch]` is applicable *transitively*
but can only be defined at the *top level* so the consumers of `my-library`
have to repeat the `[patch]` section if necessary"
([Overriding Dependencies](https://github.com/rust-lang/cargo/blob/master/doc/book/src/reference/overriding-dependencies.md)) —
the documented contract, not a corner case. No unstable flag changes it: the
only nightly patch-related work is `-Z patch-in-config`
([#9269](https://github.com/rust-lang/cargo/issues/9269)), which moves
*where* `[patch]` lives (`.cargo/config.toml`) but a config file isn't shared
across two git repos either. `-Zpatch-files` and a `[workspace.patch]` table
do not exist as of 1.97.1.

**Line 2** — for `Cargo.lock` generation, the resolver treats all workspace
members' features as enabled; for an actual build, activation is selected
per the invoking workspace ([resolver.md § Features](https://github.com/rust-lang/cargo/blob/master/doc/book/src/reference/resolver.md)).
A satellite's own workspace decides `serde_json/preserve_order`, never
inherited across the path-dep boundary — the named lines stay mandatory,
unchanged in 2025–2026.

**Line 3** — confirmed open: [rust-lang/cargo#14946](https://github.com/rust-lang/cargo/issues/14946),
filed 2024-12-17, "needs more info," no fix landed. The described failure (a
git-fetched crate's own path dep into a sibling workspace in the same
monorepo isn't found) is exactly what full-tree submodule vendoring avoids by
construction.

## Q5 — Testing per crate

**Recommendation: `cargo nextest run -p <crate> --locked` for the scoped
gate, not `--workspace --exclude` (which still plans the full graph);
doctests stay outside nextest and need `cargo test --doc` — this repo runs
neither today.**

`cargo nextest run -p my-package`
([running.md](https://github.com/nextest-rs/nextest/blob/main/site/src/docs/running.md))
builds and runs only the named package. `--workspace-remap` is
CI-archive-specific (relocating a workspace root between build/run machines)
and irrelevant to a local gate; archive/partition matter only once a single
crate's own suite needs sharding, which none do yet.

**Doctests aren't run by nextest at all**: "Doctests are currently not
supported by Nextest due to limitations in stable Rust... continue to run
[them] separately using the standard `cargo test` command"
([index.md](https://github.com/nextest-rs/nextest/blob/main/site/src/index.md)).
Verified in this repo: `grep -rn "test --doc\|doctest"` across
`taskfiles/`, `taskfile.yml`, `.github/workflows/` → zero hits. Pre-existing
gap, not a split regression.

**Integration-test fixtures** move cleanly — `CARGO_MANIFEST_DIR` is
per-crate, so `tests/fixtures/...` under a moved `crates/ocx_index/tests/`
resolves with zero code change.

**`cargo clippy -p <crate> --all-targets`** lints lib/bins/examples/tests/
benches in one pass; dev-dependencies compile because `--all-targets`
includes the targets that pull them in — standard behaviour, and the only
per-crate change is that it now compiles just that crate's dev graph instead
of all 3,818 `#[test]` items' worth at once.

## Q6 — Changed-crate detection without new tools

**Recommendation: `cargo metadata --no-deps` for path→crate-name mapping
(cheap, submodule-independent); full `cargo metadata` for reverse-dependent
counts (needs the resolve graph and every patched submodule checked out).**

Measured on this host: `cargo metadata --no-deps --format-version 1
--locked` completes in 0.02s, 30 KB JSON, no submodule dependency. The same
command **without** `--no-deps` fails outright here: `external/sigstore-rs`
is an uninitialized submodule (`git submodule status` shows a `-` prefix),
and full metadata needs every patched target's manifest to build the resolve
graph. This reproduces the known "fresh worktree needs submodules"
precondition — the scoped-gate's hub-detection step (needs
`resolve.nodes[].deps`, only in full metadata) inherits it. Run `--no-deps`
first; escalate to full metadata only once path-resolution says a member
changed.

```python
#!/usr/bin/env python3
"""Map changed paths -> workspace crate names, flag hubs (>=4 reverse deps).
Stdlib only. Run from repo root with submodules initialized for step 2."""
import json, subprocess, sys
from pathlib import Path

def metadata(no_deps: bool) -> dict:
    cmd = ["cargo", "metadata", "--format-version", "1", "--locked"]
    if no_deps:
        cmd.insert(2, "--no-deps")
    return json.loads(subprocess.check_output(cmd, text=True))

def changed_crates(paths: list[str], md: dict) -> set[str | None]:
    root = Path(md["workspace_root"])
    dirs = sorted(((Path(p["manifest_path"]).parent, p["name"])
                   for p in md["packages"]), key=lambda t: -len(str(t[0])))
    hit: set[str | None] = set()
    for rel in paths:
        abspath = (root / rel).resolve()
        hit.add(next((n for d, n in dirs if abspath.is_relative_to(d)), None))
    return hit

def reverse_dep_counts(md_full: dict) -> dict[str, int]:
    ws_ids = set(md_full["workspace_members"])
    id_to_name = {p["id"]: p["name"] for p in md_full["packages"]}
    counts = {i: 0 for i in ws_ids}
    for node in md_full["resolve"]["nodes"]:
        if node["id"] not in ws_ids:
            continue
        for dep in node["deps"]:
            if dep["pkg"] in ws_ids:
                counts[dep["pkg"]] += 1
    return {id_to_name[i]: c for i, c in counts.items()}

if __name__ == "__main__":
    base = sys.argv[1]  # merge-base SHA
    changed = subprocess.check_output(
        ["git", "diff", "--name-only", base, "HEAD"], text=True).splitlines()
    crates = changed_crates(changed, metadata(no_deps=True))
    if None in crates:
        sys.exit("ESCALATE: a changed path is outside every workspace member")
    revdeps = reverse_dep_counts(metadata(no_deps=False))
    hubs = {c for c in crates if revdeps.get(c, 0) >= 4}
    print("changed:", sorted(crates), "hubs:", sorted(hubs))
```

Citations: [cargo-metadata man page](https://github.com/rust-lang/cargo/blob/master/doc/man/cargo-metadata.md) (`resolve.nodes[].deps[].pkg`, `workspace_members`); [`--no-deps`](https://doc.rust-lang.org/cargo/commands/cargo-metadata.html).

## Q7 — cargo-hack / cargo-deny / cargo-udeps / cargo-machete relevance

**Recommendation: add none of the three unused-dependency/feature-matrix
tools now. Keep `cargo-deny` exactly as configured; revisit
`[bans].workspace-default-features` the day two `ocx_*` crates genuinely need
different default-features on a shared dep (a real possibility per Q1's
1.99 gate), and `skip-tree` only if a specific duplicate-version tree gets
noisy.**

- **cargo-hack**: confirmed absent. Its job — feature-powerset testing — has
  no target: the only feature anywhere is `__testing`, one on/off switch, one
  owner. Revisit when a second, independently-toggleable feature appears on
  any `ocx_*` crate.
- **cargo-machete**: compile-free, fast enough for every PR, but imprecise by
  its own author's framing; not installed, not wired into `task verify`. 21
  members isn't yet the scale where this beats manual review — revisit after
  a real unused-dependency incident lands on `main`.
- **cargo-udeps**: needs nightly plus a full compile, and doesn't work
  cleanly with workspaces per its own stated limitation. Skip.
- **`[bans]` interplay**: `workspace-default-features` controls whether
  workspace-declared deps must agree on default-features across members
  (newly relevant because of Q1's override gate); `skip-tree` exempts a
  crate's transitive tree from duplicate-version detection to a depth limit.
  Neither needs touching today — `deny.toml` already resolves via
  cargo-deny's own workspace discovery, a non-event for this file at 20
  members.

## Q8 — cargo-dist + git-cliff with 20 `publish = false` members

**Recommendation: no change to either file — both are already
crate-count-agnostic, confirmed against the live config in this repo.**

`dist-workspace.toml`'s `members = ["cargo:."]` delegates member discovery to
`cargo metadata` at that path — cargo-dist's own "figure it out yourself"
prefix, seen elsewhere pointing at a submodule workspace (`cargo:./ruff`).
With `[[bin]] name = "ocx"` and `publish = false` + `[package.metadata.dist]
dist = true` (already present, confirmed in `crates/ocx_cli/Cargo.toml`),
cargo-dist scans the resolved graph, finds the one binary target, and
produces one artifact set at 4 members or 21 — no shape change.

`cliff.toml` renders `*({{ commit.scope }})*` from whatever string the
author writes; it doesn't validate scope against a crate list.
`refactor(ocx_oci)!: ...` renders exactly as `refactor(oci)!: ...` did before
the split. No config edit — commit authors just start writing the new names.

## Q9 — sccache / incremental compile across many small crates

**Recommendation: treat the ADR's 2.5–3s leaf-touch projection as an upper
bound, not a guarantee. sccache and Cargo's `incremental` mode are mutually
exclusive by design, and a comparable 2026 case study found the crate split
alone was the *weaker* lever versus linker choice and cache hit rate — and
this host has neither mold nor `ld.lld` on `PATH` (confirmed: "command not
found" for both).**

sccache "doesn't work with incremental builds... disable incremental
compilation (`CARGO_INCREMENTAL=0`) when using sccache" — its caching needs
whole compilation units to hash, which per-object incremental artifacts
don't provide. `RUSTC_WRAPPER=sccache` is live here via ambient shell env,
not committed config, and Cargo's dev profile defaults to incremental for
workspace members — so the split's rebuild-cost win and sccache's
cache-hit win partially compete for the same knob, 17x more consequential
now than with one crate.

**Counter-evidence to weigh:** a 2026-08 case study
([How we brought our Rust CI from 20 minutes to less than 5](https://blog.waleson.com/2026/08/how-we-brought-our-rust-ci-from-20.html))
split an app into 18 crates (54,600 → 5,700 LOC root) and found "splitting a
large crate does not help when all the resulting crates still need to be
rebuilt" — linker/cache changes "produced clearer benchmark results than the
crate split" itself. Counter-counter-example: Feldera's SQL-to-Rust
compiler cut compile time to 2m10s by emitting many small crates
([Cutting Down Rust Compile Times](https://www.feldera.com/blog/cutting-down-rust-compile-times-from-30-to-2-minutes-with-one-thousand-crates)),
but that's codegen-driven parallelism, not a hand-layered codebase. The split
is still worth it for D1/D2 on its own merits — compile speed is "a
consequence, not the driver" per the dossier — but the dev-loop number should
be *measured* post-split, and the mold/sccache spikes the ADR already marks
out of scope are exactly the missing lever the case study says matters more.

## Q10 — Trend scouting, 2025–2026 Cargo changes

**`[workspace.lints]`** — old news: stabilized **Rust 1.74.0**
(2023-11-16). Already adopted here (non-empty `[workspace.lints.rust/clippy]`,
every current crate has `lints.workspace = true`) — continue the pattern
into 17 new manifests, nothing else to do.

**`cargo script`** — still nightly-only; RFC 3502 approved, a stabilization
PR ([#16569](https://github.com/rust-lang/cargo/pull/16569)) under
discussion, no stable release ships it at 1.97.1. **Ignore** — targets
single-file scripts, irrelevant to a multi-crate split either way.

**`build.build-dir`** — did move in this window: layout v2 stabilized around
1.94–1.97 with some churn getting there
([PR #17354](https://github.com/rust-lang/cargo/pull/17354)). **Ignore for
this ADR** — changes artifact layout on disk, not workspace structure; a
check of `.cargo/config.toml` (one line, `[build] jobs = 12`) shows no
hardcoded `target/` assumption that would interact with it.

**`cargo package --workspace`** — irrelevant: every new `ocx_*` crate is
`publish = false` and never packaged to crates.io, per the ADR's own
rust-analyzer-model rejection. **Ignore.**

**Public/private dependencies (RFC 1977 → RFC 3516, `-Z public-dependency`)**
— still unstable, "in limbo" on compiler-side lint complexity per the [2026
Project Goals page](https://rust-lang.github.io/rust-project-goals/2026/pub-priv.html).
**Ignore now, flag as the one future item worth revisiting**: it maps almost
exactly onto the "ecosystem tier" boundary this ADR hand-enforces via `task
rust:deps:direction` — e.g. does `ocx_store` leak through `ocx_index`'s
public API. Strict upgrade over the hand-written check the day it stabilizes;
not before.

---

## Sources

| Source | Type | Date | Relevance |
|---|---|---|---|
| [Cargo Book — Workspaces](https://github.com/rust-lang/cargo/blob/master/doc/book/src/reference/workspaces.md) | Docs | current (master) | Q1, Q3, Q4 |
| [Cargo Book — Specifying Dependencies](https://github.com/rust-lang/cargo/blob/master/doc/book/src/reference/specifying-dependencies.md) | Docs | current | Q1 — `default-features` 1.99 gate |
| [Cargo Book — Overriding Dependencies](https://github.com/rust-lang/cargo/blob/master/doc/book/src/reference/overriding-dependencies.md) | Docs | current | Q4 — `[patch]` non-propagation |
| [Cargo Book — Resolver](https://github.com/rust-lang/cargo/blob/master/doc/book/src/reference/resolver.md) | Docs | current | Q2, Q3, Q4 |
| [Cargo Book — Build Performance guide](https://github.com/rust-lang/cargo/blob/master/doc/book/src/guide/build-performance.md) | Docs | current | Q2 — `resolver.feature-unification` |
| [rustc book — Cargo Specifics](https://doc.rust-lang.org/rustc/check-cfg/cargo-specifics.html) | Docs | current | Q2 — features auto-declare cfgs |
| [cargo-metadata man page](https://github.com/rust-lang/cargo/blob/master/doc/man/cargo-metadata.md) | Docs | current | Q6 — JSON schema |
| [rust-lang/cargo#14946](https://github.com/rust-lang/cargo/issues/14946) | Issue | 2024-12-17, open | Q4 — cross-repo transitive path dep |
| [rust-lang/cargo#9269](https://github.com/rust-lang/cargo/issues/9269) | Issue | open | Q4 — `-Z patch-in-config` |
| [rust-lang/cargo#15933](https://github.com/rust-lang/cargo/issues/15933) | Issue | closed dup | Q2 — workspace `unexpected_cfgs` gap |
| [rust-lang/cargo#14639](https://github.com/rust-lang/cargo/pull/14639) | PR | stabilized 1.84 | Q3 — MSRV-aware resolver |
| [rust-lang/rust#134286](https://github.com/rust-lang/rust/pull/134286) / [#136595](https://github.com/rust-lang/rust/pull/136595) | PR | 2025 | Q3 — `unreachable_pub` ratchet precedent |
| [nextest running.md](https://github.com/nextest-rs/nextest/blob/main/site/src/docs/running.md) / [index.md](https://github.com/nextest-rs/nextest/blob/main/site/src/index.md) | Docs | current | Q5 — `-p`, doctests unsupported |
| [Rust Blog 1.74.0](https://blog.rust-lang.org/2023/11/16/Rust-1.74.0/) | Blog | 2023-11-16 | Q10 — `[workspace.lints]` date |
| [Rust Blog 1.97.1](https://blog.rust-lang.org/2026/07/16/Rust-1.97.1/) | Blog | 2026-07-16 | Metadata — current stable |
| [Stabilize cargo script — Project Goals](https://rust-lang.github.io/rust-project-goals/2025h2/cargo-script.html) | Goals | 2025–2026 | Q10 |
| [Stabilize public/private deps — Project Goals 2026](https://rust-lang.github.io/rust-project-goals/2026/pub-priv.html) | Goals | 2026 | Q10 |
| [How we brought our Rust CI from 20 min to <5](https://blog.waleson.com/2026/08/how-we-brought-our-rust-ci-from-20.html) | Case study | 2026-08 | Q9 |
| [Feldera — Cutting compile times](https://www.feldera.com/blog/cutting-down-rust-compile-times-from-30-to-2-minutes-with-one-thousand-crates) | Case study | 2025 | Q9 |
| [sccache + CARGO_INCREMENTAL conflict](https://medium.com/@alistairisrael/using-both-cargo-incremental-and-sccache-gives-me-sccache-incremental-compilation-is-8cc40442c742) | Blog | — | Q9 |
| Local: `cargo --version`, `git submodule status`, timed `cargo metadata` runs | First-party | 2026-09-16 | Q6, Q9 — toolchain pin, submodule precondition |
